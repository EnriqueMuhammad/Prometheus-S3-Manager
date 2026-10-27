//! Bounded-concurrency transfer queue with exponential backoff (full jitter), per-job
//! cancellation and durable records.
//!
//! Jobs that were queued or running when the app last closed come back as Failed
//! ("interrupted") and need an explicit Retry: the queue never silently resumes against local
//! paths.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::{BoxFuture, format};

/// Message for jobs cut off by closing the app.
pub const INTERRUPTED: &str = "Interrupted when the app closed. Resume to continue.";

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TransferDirection {
    /// Local file → S3.
    #[default]
    Upload,
    /// S3 → local file.
    Download,
    /// Server-side copy (S3 → S3, same account).
    Copy,
    /// Server-side copy, then delete the source.
    Move,
}

impl TransferDirection {
    /// Stable integer for storage.
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Upload => 0,
            Self::Download => 1,
            Self::Copy => 2,
            Self::Move => 3,
        }
    }

    /// Inverse of [`Self::as_i64`].
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Download,
            2 => Self::Copy,
            3 => Self::Move,
            _ => Self::Upload,
        }
    }

    /// "Upload", "Download", "Copy" or "Move".
    pub fn label(self) -> &'static str {
        match self {
            Self::Upload => "Upload",
            Self::Download => "Download",
            Self::Copy => "Copy",
            Self::Move => "Move",
        }
    }

    /// Server-side copy or move (no local file).
    pub fn is_server_side(self) -> bool {
        matches!(self, Self::Copy | Self::Move)
    }
}

/// Lifecycle of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferState {
    /// Waiting for a slot.
    Queued,
    /// Transferring.
    Running,
    /// Waiting before another attempt.
    Retrying,
    /// Done.
    Completed,
    /// Gave up (see the error).
    Failed,
    /// Canceled by the user.
    Canceled,
    /// Paused by the user or interrupted by closing the app; Resume continues it.
    Paused,
}

impl TransferState {
    /// Stable integer for storage; never renumber, `transfers.db` keeps these values.
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Queued => 0,
            Self::Running => 1,
            Self::Retrying => 2,
            Self::Completed => 3,
            Self::Failed => 4,
            Self::Canceled => 5,
            Self::Paused => 6,
        }
    }

    /// Inverse of [`Self::as_i64`].
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => Self::Queued,
            1 => Self::Running,
            2 => Self::Retrying,
            3 => Self::Completed,
            5 => Self::Canceled,
            6 => Self::Paused,
            _ => Self::Failed,
        }
    }

    /// Queued, running or retrying.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Retrying)
    }

    /// Active or paused: not finished yet.
    pub fn is_pending(self) -> bool {
        self.is_active() || self == Self::Paused
    }
}

/// An object in a bucket (source of a copy or move).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectRef {
    /// Bucket.
    pub bucket: String,
    /// Key.
    pub key: String,
    /// Account of the source when it differs from the job's account (cross-account copy).
    pub account_id: Option<Uuid>,
}

/// What to transfer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferRequest {
    /// Account to use.
    pub account_id: Uuid,
    /// Direction.
    pub direction: TransferDirection,
    /// Bucket.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// Local file.
    pub local_path: PathBuf,
    /// Expected size (shown before the transfer starts).
    pub size_hint: u64,
    /// Source object of a copy or move (`bucket`/`key` are the destination).
    pub source: Option<ObjectRef>,
    /// Object version to download or copy (`None` = latest).
    pub version_id: Option<String>,
}

/// One upload or download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferJob {
    /// Identifier.
    pub id: Uuid,
    /// Account.
    pub account_id: Uuid,
    /// Direction.
    pub direction: TransferDirection,
    /// Bucket.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// Local file.
    pub local_path: PathBuf,
    /// When it was queued.
    pub created_at: DateTime<Utc>,
    /// Current state.
    pub state: TransferState,
    /// Bytes transferred in the current attempt.
    pub bytes_done: u64,
    /// Total bytes (0 when unknown).
    pub bytes_total: u64,
    /// Attempts started.
    pub attempts: u32,
    /// Last error.
    pub error: Option<String>,
    /// When it reached a final state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Source object of a copy or move.
    pub source: Option<ObjectRef>,
    /// Object version to download or copy (`None` = latest).
    pub version_id: Option<String>,
    /// Executor checkpoint (multipart upload id, partial download) used to resume.
    pub resume: Option<String>,
    /// Current throughput in bytes per second (not persisted).
    pub rate: u64,
}

impl TransferJob {
    /// Queued, running or retrying.
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// Active or paused.
    pub fn is_pending(&self) -> bool {
        self.state.is_pending()
    }

    /// Progress in percent.
    pub fn percent(&self) -> f32 {
        if self.bytes_total > 0 {
            #[allow(clippy::cast_precision_loss)]
            let p = self.bytes_done as f64 * 100.0 / self.bytes_total as f64;
            #[allow(clippy::cast_possible_truncation)]
            return p.min(100.0) as f32;
        }
        if self.state == TransferState::Completed {
            100.0
        } else {
            0.0
        }
    }

    /// File name (last key segment).
    pub fn name(&self) -> &str {
        crate::browse::leaf_name(&self.key)
    }

    /// Estimated time left, when the rate is known.
    pub fn eta(&self) -> Option<Duration> {
        (self.state == TransferState::Running
            && self.rate > 0
            && self.bytes_total > self.bytes_done)
            .then(|| Duration::from_secs((self.bytes_total - self.bytes_done).div_ceil(self.rate)))
    }

    /// Status text for the Tasks tab.
    pub fn summary(&self) -> String {
        let error = self.error.as_deref().unwrap_or("");
        match self.state {
            TransferState::Running => {
                let mut text = format!(
                    "{:.0}% · {} of {}",
                    self.percent(),
                    format::size(self.bytes_done),
                    format::size(self.bytes_total)
                );
                if self.rate > 0 {
                    text.push_str(&format!(" · {}/s", format::size(self.rate)));
                }
                if let Some(eta) = self.eta() {
                    text.push_str(&format!(" · {} left", format::duration(eta)));
                }
                text
            }
            TransferState::Retrying => {
                format!("Retrying (attempt {}) · {error}", self.attempts + 1)
            }
            TransferState::Completed => format!("Done · {}", format::size(self.bytes_total)),
            TransferState::Failed => format!("Failed · {error}"),
            TransferState::Canceled if !error.is_empty() => format!("Canceled · {error}"),
            TransferState::Canceled => "Canceled".into(),
            TransferState::Queued => "Queued".into(),
            TransferState::Paused => {
                let mut text = format!(
                    "Paused · {:.0}% · {} of {}",
                    self.percent(),
                    format::size(self.bytes_done),
                    format::size(self.bytes_total)
                );
                if !error.is_empty() {
                    text.push_str(&format!(" · {error}"));
                }
                text
            }
        }
    }
}

/// Failure of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TransferError {
    /// User-facing message.
    pub message: String,
    /// Worth retrying (network, timeout, throttling, server errors).
    pub transient: bool,
}

impl TransferError {
    /// Permanent failure.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }

    /// Retryable failure.
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }
}

/// Progress callback: `(bytes_done, bytes_total)`; total 0 means unknown.
pub type Progress = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// Saves (or clears) the executor's resume checkpoint of the running job.
pub type Checkpoint = Arc<dyn Fn(Option<String>) + Send + Sync>;

/// Performs one attempt of a transfer. The future is dropped on pause or cancellation; partial
/// work stays behind (for Resume) until [`TransferExecutor::discard`] is called.
pub trait TransferExecutor: Send + Sync {
    /// Runs one attempt, continuing from `job.resume` when present.
    fn execute(
        &self,
        job: TransferJob,
        progress: Progress,
        checkpoint: Checkpoint,
    ) -> BoxFuture<'static, Result<(), TransferError>>;

    /// Throws away partial work of a canceled or removed job (abort multipart, delete temp file).
    fn discard(&self, _job: TransferJob) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// Durable job records so the queue survives restarts.
pub trait TransferStore: Send + Sync {
    /// All records, oldest first.
    fn load_all(&self) -> Vec<TransferJob>;
    /// Inserts or updates a record.
    fn upsert(&self, job: &TransferJob);
    /// Deletes records.
    fn delete(&self, ids: &[Uuid]);
}

/// In-memory store for tests and demo runs.
#[derive(Debug, Default)]
pub struct MemoryTransferStore(Mutex<Vec<TransferJob>>);

impl TransferStore for MemoryTransferStore {
    fn load_all(&self) -> Vec<TransferJob> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn upsert(&self, job: &TransferJob) {
        let mut jobs = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match jobs.iter_mut().find(|j| j.id == job.id) {
            Some(slot) => *slot = job.clone(),
            None => jobs.push(job.clone()),
        }
    }
    fn delete(&self, ids: &[Uuid]) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|j| !ids.contains(&j.id));
    }
}

/// Queue tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferOptions {
    /// Jobs running at once (1–16).
    pub concurrency: usize,
    /// Attempts per job, including the first (1–10).
    pub max_attempts: u32,
    /// First retry delay cap.
    pub base_retry_delay: Duration,
    /// Largest retry delay cap.
    pub max_retry_delay: Duration,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            concurrency: 3,
            max_attempts: 4,
            base_retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(30),
        }
    }
}

/// Called after every state change (not on progress) with the job's new snapshot.
pub type JobListener = Arc<dyn Fn(&TransferJob) + Send + Sync>;

struct State {
    jobs: Vec<TransferJob>,
    running: HashMap<Uuid, CancellationToken>,
    pausing: std::collections::HashSet<Uuid>,
    options: TransferOptions,
    slots: Arc<Semaphore>,
}

struct Inner {
    executor: Arc<dyn TransferExecutor>,
    store: Arc<dyn TransferStore>,
    state: Mutex<State>,
    shutdown: CancellationToken,
    tracker: TaskTracker,
    runtime: tokio::runtime::Handle,
    listener: JobListener,
}

/// The transfer queue. Cheap to clone.
#[derive(Clone)]
pub struct TransferQueue {
    inner: Arc<Inner>,
}

impl TransferQueue {
    /// Loads stored jobs (active ones become Paused – interrupted) and prepares the queue.
    pub fn new(
        executor: Arc<dyn TransferExecutor>,
        store: Arc<dyn TransferStore>,
        options: TransferOptions,
        runtime: tokio::runtime::Handle,
        listener: JobListener,
    ) -> Self {
        let mut jobs = store.load_all();
        jobs.sort_by_key(|j| j.created_at);
        for job in &mut jobs {
            if job.is_active() {
                job.state = TransferState::Paused;
                job.error = Some(INTERRUPTED.into());
                store.upsert(job);
            }
        }
        Self {
            inner: Arc::new(Inner {
                executor,
                store,
                state: Mutex::new(State {
                    jobs,
                    running: HashMap::new(),
                    pausing: std::collections::HashSet::new(),
                    options,
                    slots: Arc::new(Semaphore::new(options.concurrency)),
                }),
                shutdown: CancellationToken::new(),
                tracker: TaskTracker::new(),
                runtime,
                listener,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Snapshot of all jobs, oldest first.
    pub fn jobs(&self) -> Vec<TransferJob> {
        self.state().jobs.clone()
    }

    /// Runs `f` over the jobs without cloning them (keep it short: progress waits on it).
    pub fn with_jobs<T>(&self, f: impl FnOnce(&[TransferJob]) -> T) -> T {
        f(&self.state().jobs)
    }

    /// Number of active jobs.
    pub fn active_count(&self) -> usize {
        self.state().jobs.iter().filter(|j| j.is_active()).count()
    }

    /// Whether an account has active or paused jobs (it cannot be deleted then).
    pub fn has_active_jobs(&self, account_id: Uuid) -> bool {
        self.state()
            .jobs
            .iter()
            .any(|j| j.account_id == account_id && j.is_pending())
    }

    /// Current options.
    pub fn options(&self) -> TransferOptions {
        self.state().options
    }

    /// Changes options; concurrency applies to jobs that start afterwards.
    pub fn update_options(&self, options: TransferOptions) -> Result<(), String> {
        if !(1..=16).contains(&options.concurrency) {
            return Err("Concurrency must be 1–16.".into());
        }
        if !(1..=10).contains(&options.max_attempts) {
            return Err("Max attempts must be 1–10.".into());
        }
        let mut state = self.state();
        state.options = options;
        state.slots = Arc::new(Semaphore::new(options.concurrency));
        Ok(())
    }

    /// Adds jobs and starts them.
    pub fn enqueue(&self, requests: Vec<TransferRequest>) -> Vec<TransferJob> {
        let now = Utc::now();
        let added: Vec<TransferJob> = requests
            .into_iter()
            .map(|r| TransferJob {
                id: Uuid::new_v4(),
                account_id: r.account_id,
                direction: r.direction,
                bucket: r.bucket,
                key: r.key,
                local_path: r.local_path,
                created_at: now,
                state: TransferState::Queued,
                bytes_done: 0,
                bytes_total: r.size_hint,
                attempts: 0,
                error: None,
                finished_at: None,
                source: r.source,
                version_id: r.version_id,
                resume: None,
                rate: 0,
            })
            .collect();
        {
            let mut state = self.state();
            for job in &added {
                self.inner.store.upsert(job);
                state.jobs.push(job.clone());
            }
        }
        for job in &added {
            (self.inner.listener)(job);
            self.start(job.id);
        }
        added
    }

    /// Cancels an active or paused job and discards its partial work.
    pub fn cancel(&self, id: Uuid) {
        let (token, paused) = {
            let state = self.state();
            let Some(job) = state.jobs.iter().find(|j| j.id == id) else {
                return;
            };
            if !job.is_pending() {
                return;
            }
            (
                state.running.get(&id).cloned(),
                job.state == TransferState::Paused,
            )
        };
        if paused {
            if let Some(job) = self.change(id, |job| {
                job.state = TransferState::Canceled;
                job.error = None;
                job.finished_at = Some(Utc::now());
            }) {
                self.discard(job);
            }
            return;
        }
        if let Some(token) = token {
            token.cancel();
        }
    }

    /// Pauses an active job; partial work is kept so Resume continues it.
    pub fn pause(&self, id: Uuid) {
        let token = {
            let mut state = self.state();
            if !state.jobs.iter().any(|j| j.id == id && j.is_active()) {
                return;
            }
            state.pausing.insert(id);
            state.running.get(&id).cloned()
        };
        if let Some(token) = token {
            token.cancel();
        }
    }

    /// Pauses every active job.
    pub fn pause_all(&self) {
        let ids: Vec<Uuid> = self
            .state()
            .jobs
            .iter()
            .filter(|j| j.is_active())
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.pause(id);
        }
    }

    /// Resumes every paused job.
    pub fn resume_all(&self) {
        let ids: Vec<Uuid> = self
            .state()
            .jobs
            .iter()
            .filter(|j| j.state == TransferState::Paused)
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.retry(id);
        }
    }

    fn discard(&self, job: TransferJob) {
        if job.resume.is_none() {
            return;
        }
        let id = job.id;
        let future = self.inner.executor.discard(job);
        let queue = self.clone();
        self.inner.tracker.spawn_on(
            async move {
                future.await;
                let mut state = queue.state();
                if let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) {
                    job.resume = None;
                    queue.inner.store.upsert(job);
                }
            },
            &self.inner.runtime,
        );
    }

    /// Restarts a failed or canceled job, or resumes a paused one (from its checkpoint).
    pub fn retry(&self, id: Uuid) {
        let snapshot = {
            let mut state = self.state();
            let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) else {
                return;
            };
            if !matches!(
                job.state,
                TransferState::Failed | TransferState::Canceled | TransferState::Paused
            ) {
                return;
            }
            job.state = TransferState::Queued;
            job.error = None;
            job.attempts = 0;
            job.bytes_done = 0;
            job.finished_at = None;
            self.inner.store.upsert(job);
            job.clone()
        };
        (self.inner.listener)(&snapshot);
        self.start(id);
    }

    /// Removes finished jobs (not paused ones); returns how many.
    pub fn clear_finished(&self) -> usize {
        let removed: Vec<TransferJob> = {
            let mut state = self.state();
            let removed = state
                .jobs
                .iter()
                .filter(|j| !j.is_pending())
                .cloned()
                .collect::<Vec<_>>();
            state.jobs.retain(TransferJob::is_pending);
            removed
        };
        let ids: Vec<Uuid> = removed.iter().map(|j| j.id).collect();
        self.inner.store.delete(&ids);
        for job in removed {
            if job.resume.is_some() {
                let future = self.inner.executor.discard(job);
                self.inner.tracker.spawn_on(future, &self.inner.runtime);
            }
        }
        ids.len()
    }

    fn start(&self, id: Uuid) {
        let token = self.inner.shutdown.child_token();
        self.state().running.insert(id, token.clone());
        let queue = self.clone();
        self.inner.tracker.spawn_on(
            async move { queue.run(id, token).await },
            &self.inner.runtime,
        );
    }

    /// Applies a change to a job, persists it and notifies the listener.
    fn change(&self, id: Uuid, f: impl FnOnce(&mut TransferJob)) -> Option<TransferJob> {
        let snapshot = {
            let mut state = self.state();
            let job = state.jobs.iter_mut().find(|j| j.id == id)?;
            f(job);
            self.inner.store.upsert(job);
            job.clone()
        };
        (self.inner.listener)(&snapshot);
        Some(snapshot)
    }

    fn finish(&self, id: Uuid, state: TransferState, error: Option<String>) {
        let still_active = self
            .state()
            .jobs
            .iter()
            .any(|j| j.id == id && j.is_active());
        if !still_active {
            return;
        }
        self.change(id, |job| {
            job.state = state;
            job.error = error;
            job.finished_at = Some(Utc::now());
            job.rate = 0;
            if state == TransferState::Completed {
                job.resume = None;
                if job.bytes_total > 0 {
                    job.bytes_done = job.bytes_total;
                }
            }
        });
    }

    async fn run(&self, id: Uuid, token: CancellationToken) {
        let slots = self.state().slots.clone();
        let outcome = tokio::select! {
            biased;
            () = token.cancelled() => None,
            outcome = async {
                let _permit = slots.acquire_owned().await;
                self.attempts(id, &token).await
            } => Some(outcome),
        };
        match outcome {
            Some(Ok(())) => self.finish(id, TransferState::Completed, None),
            Some(Err(message)) => self.finish(id, TransferState::Failed, Some(message)),
            None => {
                let pausing = self.state().pausing.remove(&id);
                if self.inner.shutdown.is_cancelled() {
                    self.finish(id, TransferState::Paused, Some(INTERRUPTED.to_owned()));
                } else if pausing {
                    self.finish(id, TransferState::Paused, None);
                } else {
                    self.finish(id, TransferState::Canceled, None);
                    let job = self.state().jobs.iter().find(|j| j.id == id).cloned();
                    if let Some(job) = job {
                        self.discard(job);
                    }
                }
            }
        }
        let mut state = self.state();
        state.running.remove(&id);
        state.pausing.remove(&id);
    }

    /// Attempt loop; cancellation is handled by the caller dropping this future.
    async fn attempts(&self, id: Uuid, token: &CancellationToken) -> Result<(), String> {
        loop {
            let max_attempts = self.state().options.max_attempts;
            let Some(job) = self.change(id, |job| {
                job.state = TransferState::Running;
                job.attempts += 1;
                job.error = None;
                job.bytes_done = 0;
                job.rate = 0;
            }) else {
                return Err("The job was removed.".into());
            };
            let attempt = job.attempts;
            let queue = self.clone();
            // Throughput: smoothed over ~1 s windows. The first report of an attempt (work kept
            // from before a pause) and any step backwards only set the baseline.
            let window = Mutex::new((std::time::Instant::now(), None::<u64>));
            let progress: Progress = Arc::new(move |done, total| {
                let rate = {
                    let mut w = window.lock().unwrap_or_else(PoisonError::into_inner);
                    let elapsed = w.0.elapsed();
                    match w.1 {
                        Some(base) if done < base => {
                            *w = (std::time::Instant::now(), Some(done));
                            None
                        }
                        Some(base) if elapsed >= Duration::from_millis(1000) => {
                            #[allow(
                                clippy::cast_possible_truncation,
                                clippy::cast_sign_loss,
                                clippy::cast_precision_loss
                            )]
                            let r = ((done - base) as f64 / elapsed.as_secs_f64()) as u64;
                            *w = (std::time::Instant::now(), Some(done));
                            Some(r)
                        }
                        Some(_) => None,
                        None => {
                            *w = (std::time::Instant::now(), Some(done));
                            None
                        }
                    }
                };
                let mut state = queue.state();
                if let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) {
                    job.bytes_done = done;
                    if total > 0 {
                        job.bytes_total = total;
                    }
                    if let Some(r) = rate {
                        job.rate = if job.rate == 0 { r } else { (job.rate + r) / 2 };
                    }
                }
            });
            let queue = self.clone();
            let checkpoint: Checkpoint = Arc::new(move |resume| {
                let mut state = queue.state();
                if let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) {
                    job.resume = resume;
                    queue.inner.store.upsert(job);
                }
            });
            match self.inner.executor.execute(job, progress, checkpoint).await {
                Ok(()) => return Ok(()),
                Err(e) if e.transient && attempt < max_attempts && !token.is_cancelled() => {
                    self.change(id, |job| {
                        job.state = TransferState::Retrying;
                        job.error = Some(e.message.clone());
                    });
                    tokio::time::sleep(self.backoff(attempt)).await;
                }
                Err(e) => return Err(e.message),
            }
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let options = self.state().options;
        let factor = 2f64.powi(i32::try_from(attempt.saturating_sub(1)).unwrap_or(30));
        let cap = (options.base_retry_delay.as_secs_f64() * factor)
            .min(options.max_retry_delay.as_secs_f64());
        Duration::from_secs_f64(fastrand::f64() * cap)
    }

    /// Cancels running work and waits up to `timeout` for workers to stop. Must not be called
    /// from inside the runtime.
    pub fn shutdown_blocking(&self, timeout: Duration) {
        self.inner.shutdown.cancel();
        self.inner.tracker.close();
        let tracker = self.inner.tracker.clone();
        let _ = self
            .inner
            .runtime
            .block_on(async { tokio::time::timeout(timeout, tracker.wait()).await });
    }

    /// Async variant of [`Self::shutdown_blocking`] for use inside the runtime.
    pub async fn shutdown(&self, timeout: Duration) {
        self.inner.shutdown.cancel();
        self.inner.tracker.close();
        let _ = tokio::time::timeout(timeout, self.inner.tracker.wait()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Executor scripted per key: `fail-N` fails transiently N times, `perm` fails permanently,
    /// `slow` waits for an hour, anything else succeeds.
    struct Scripted {
        calls: Mutex<HashMap<String, usize>>,
    }

    impl Scripted {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::default(),
            })
        }
    }

    impl TransferExecutor for Scripted {
        fn execute(
            &self,
            job: TransferJob,
            progress: Progress,
            _checkpoint: Checkpoint,
        ) -> BoxFuture<'static, Result<(), TransferError>> {
            let n = {
                let mut calls = self.calls.lock().expect("lock");
                let n = calls.entry(job.key.clone()).or_default();
                *n += 1;
                *n
            };
            Box::pin(async move {
                let key = job.key.clone();
                if key == "slow" {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Ok(())
                } else if key == "perm" {
                    Err(TransferError::permanent("denied"))
                } else if let Some(times) = key.strip_prefix("fail-") {
                    let times: usize = times.parse().expect("n");
                    if n <= times {
                        Err(TransferError::transient("flaky"))
                    } else {
                        Ok(())
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    progress(10, 10);
                    Ok(())
                }
            })
        }
    }

    fn request(key: &str) -> TransferRequest {
        TransferRequest {
            account_id: Uuid::nil(),
            direction: TransferDirection::Upload,
            bucket: "b".into(),
            key: key.into(),
            local_path: PathBuf::from("x"),
            size_hint: 10,
            source: None,
            version_id: None,
        }
    }

    fn queue(exec: Arc<dyn TransferExecutor>, store: Arc<MemoryTransferStore>) -> TransferQueue {
        TransferQueue::new(
            exec,
            store,
            TransferOptions::default(),
            tokio::runtime::Handle::current(),
            Arc::new(|_| {}),
        )
    }

    async fn settle(q: &TransferQueue) {
        for _ in 0..10_000 {
            if q.active_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("queue did not settle");
    }

    #[tokio::test(start_paused = true)]
    async fn completes_and_retries_transient_failures() {
        let store = Arc::new(MemoryTransferStore::default());
        let q = queue(Scripted::new(), store.clone());
        q.enqueue(vec![
            request("ok"),
            request("fail-2"),
            request("fail-9"),
            request("perm"),
        ]);
        settle(&q).await;
        let jobs = q.jobs();
        let by = |k: &str| jobs.iter().find(|j| j.key == k).expect("job").clone();
        assert_eq!(by("ok").state, TransferState::Completed);
        assert_eq!(by("ok").percent(), 100.0);
        assert_eq!(by("fail-2").state, TransferState::Completed);
        assert_eq!(by("fail-2").attempts, 3);
        assert_eq!(by("fail-9").state, TransferState::Failed);
        assert_eq!(by("fail-9").attempts, 4);
        assert_eq!(by("perm").state, TransferState::Failed);
        assert_eq!(by("perm").attempts, 1);
        assert_eq!(store.load_all().len(), 4);
        assert_eq!(q.clear_finished(), 4);
        assert!(store.load_all().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_and_retry() {
        let q = queue(Scripted::new(), Arc::new(MemoryTransferStore::default()));
        let job = q.enqueue(vec![request("slow")]).remove(0);
        tokio::time::sleep(Duration::from_millis(10)).await;
        q.cancel(job.id);
        settle(&q).await;
        assert_eq!(q.jobs()[0].state, TransferState::Canceled);
        assert_eq!(q.jobs()[0].error, None);
        q.retry(job.id);
        assert!(q.jobs()[0].is_active());
        q.shutdown(Duration::from_secs(1)).await;
        let j = &q.jobs()[0];
        assert_eq!(j.state, TransferState::Paused);
        assert_eq!(j.error.as_deref(), Some(INTERRUPTED));
    }

    #[tokio::test(start_paused = true)]
    async fn respects_concurrency() {
        struct Gate {
            running: Arc<AtomicUsize>,
            peak: Arc<AtomicUsize>,
        }
        impl TransferExecutor for Gate {
            fn execute(
                &self,
                _: TransferJob,
                _: Progress,
                _: Checkpoint,
            ) -> BoxFuture<'static, Result<(), TransferError>> {
                let running = self.running.clone();
                let peak = self.peak.clone();
                Box::pin(async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        }
        let peak = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Gate {
            running: Arc::default(),
            peak: peak.clone(),
        });
        let q = queue(gate, Arc::new(MemoryTransferStore::default()));
        q.enqueue((0..10).map(|i| request(&format!("f{i}"))).collect());
        settle(&q).await;
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert!(
            q.update_options(TransferOptions {
                concurrency: 0,
                ..TransferOptions::default()
            })
            .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pause_resume_and_cancel_paused() {
        let q = queue(Scripted::new(), Arc::new(MemoryTransferStore::default()));
        let job = q.enqueue(vec![request("slow")]).remove(0);
        tokio::time::sleep(Duration::from_millis(10)).await;
        q.pause(job.id);
        settle(&q).await;
        assert_eq!(q.jobs()[0].state, TransferState::Paused);
        assert!(q.jobs()[0].is_pending());
        assert!(
            q.has_active_jobs(Uuid::nil()),
            "paused jobs block account deletion"
        );
        assert_eq!(q.clear_finished(), 0, "paused jobs are not cleared");
        q.resume_all();
        assert!(q.jobs()[0].is_active());
        q.pause_all();
        settle(&q).await;
        q.cancel(job.id);
        assert_eq!(q.jobs()[0].state, TransferState::Canceled);
    }

    #[test]
    fn summary_shows_speed_and_eta() {
        let job = TransferJob {
            id: Uuid::nil(),
            account_id: Uuid::nil(),
            direction: TransferDirection::Copy,
            bucket: "b".into(),
            key: "k".into(),
            local_path: PathBuf::new(),
            created_at: Utc::now(),
            state: TransferState::Running,
            bytes_done: 4 * 1024 * 1024,
            bytes_total: 10 * 1024 * 1024,
            attempts: 1,
            error: None,
            finished_at: None,
            source: None,
            version_id: None,
            resume: None,
            rate: 1024 * 1024,
        };
        assert_eq!(job.eta(), Some(Duration::from_secs(6)));
        assert_eq!(
            job.summary(),
            "40% · 4.0 MB of 10.0 MB · 1.0 MB/s · 6s left"
        );
        assert!(TransferDirection::Move.is_server_side());
        assert_eq!(
            TransferDirection::from_i64(TransferDirection::Move.as_i64()),
            TransferDirection::Move
        );
    }

    #[tokio::test]
    async fn restart_pauses_active_jobs() {
        let store = Arc::new(MemoryTransferStore::default());
        let mut job = TransferJob {
            id: Uuid::new_v4(),
            account_id: Uuid::nil(),
            direction: TransferDirection::Download,
            bucket: "b".into(),
            key: "k".into(),
            local_path: PathBuf::from("x"),
            created_at: Utc::now(),
            state: TransferState::Running,
            bytes_done: 5,
            bytes_total: 10,
            attempts: 1,
            error: None,
            finished_at: None,
            source: None,
            version_id: None,
            resume: None,
            rate: 0,
        };
        store.upsert(&job);
        job.id = Uuid::new_v4();
        job.state = TransferState::Completed;
        store.upsert(&job);
        let q = queue(Scripted::new(), store.clone());
        let jobs = q.jobs();
        assert_eq!(jobs[0].state, TransferState::Paused);
        assert_eq!(jobs[0].error.as_deref(), Some(INTERRUPTED));
        assert_eq!(jobs[1].state, TransferState::Completed);
        assert_eq!(store.load_all()[0].state, TransferState::Paused);
    }
}
