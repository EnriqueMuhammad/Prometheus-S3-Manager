//! One attempt of an upload, download, copy or move, resumable where S3 allows it.
//!
//! - Uploads stream from disk (multipart above [`PART_SIZE`], four parts in flight); under a
//!   bandwidth limit parts go one at a time, read into memory and released in small chunks. The
//!   upload id is checkpointed; Resume asks the server which parts it already has (ListParts) and
//!   sends only the rest, provided the local file still has the same size and modification time.
//! - Downloads stream into `<target>.<job>.s3m-partial`. Resume continues with an HTTP Range
//!   request guarded by `If-Match` on the ETag, so a changed object restarts from zero. The
//!   finished file is published without overwriting: an existing local file is never replaced.
//! - Copies and moves run server-side within an account; between accounts the object is streamed
//!   from one client to the other.
//!
//! Partial work survives a pause; [`TransferExecutor::discard`] removes it on cancel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream};
use s3m_core::transfer::{Checkpoint, Progress};
use s3m_core::{
    AccountService, BoxFuture, TransferDirection, TransferError, TransferExecutor, TransferJob,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::client::ClientProvider;
use crate::error::StorageError;
use crate::limit::RateLimiter;
use crate::throttle;

/// Multipart part size; files up to this size are sent with a single PUT.
pub const PART_SIZE: u64 = 8 * 1024 * 1024;
const PARTS_IN_FLIGHT: usize = 4;

/// Runs transfers against S3.
pub struct S3TransferExecutor {
    accounts: Arc<AccountService>,
    clients: Arc<ClientProvider>,
    limiter: Arc<RateLimiter>,
}

impl S3TransferExecutor {
    /// Executor using the given accounts and clients, without a bandwidth limit.
    pub fn new(accounts: Arc<AccountService>, clients: Arc<ClientProvider>) -> Self {
        Self {
            accounts,
            clients,
            limiter: Arc::new(RateLimiter::default()),
        }
    }

    /// The shared bandwidth limiter (Tools > Options sets its rate).
    pub fn limiter(&self) -> &Arc<RateLimiter> {
        &self.limiter
    }
}

/// Checkpoint of a multipart upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadResume {
    upload_id: String,
    size: u64,
    mtime: u64,
}

/// Checkpoint of a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadResume {
    etag: String,
}

/// Partial file of a download job.
fn partial_path(target: &Path, job: &TransferJob) -> PathBuf {
    let mut temp = target.as_os_str().to_owned();
    temp.push(format!(".{}.s3m-partial", job.id.simple()));
    PathBuf::from(temp)
}

impl TransferExecutor for S3TransferExecutor {
    fn execute(
        &self,
        job: TransferJob,
        progress: Progress,
        checkpoint: Checkpoint,
    ) -> BoxFuture<'static, Result<(), TransferError>> {
        let account = self.accounts.get(job.account_id);
        let source_account = job
            .source
            .as_ref()
            .and_then(|s| s.account_id)
            .filter(|a| *a != job.account_id)
            .map(|a| self.accounts.get(a));
        let clients = self.clients.clone();
        let limiter = self.limiter.clone();
        Box::pin(async move {
            let account = account.ok_or_else(|| {
                TransferError::permanent("The account for this transfer no longer exists.")
            })?;
            let client = clients.get(&account).map_err(storage)?;
            let io = Io {
                client: &client,
                job: &job,
                progress: &progress,
                checkpoint: &checkpoint,
                limiter: &limiter,
            };
            match job.direction {
                TransferDirection::Upload => upload(&io).await,
                TransferDirection::Download => download(&io).await,
                TransferDirection::Copy | TransferDirection::Move => match source_account {
                    Some(source) => {
                        let source = source.ok_or_else(|| {
                            TransferError::permanent("The source account no longer exists.")
                        })?;
                        let source_client = clients.get(&source).map_err(storage)?;
                        copy_between_accounts(&io, &source_client).await
                    }
                    None => copy_or_move(&io).await,
                },
            }
        })
    }

    fn discard(&self, job: TransferJob) -> BoxFuture<'static, ()> {
        let account = self.accounts.get(job.account_id);
        let clients = self.clients.clone();
        Box::pin(async move {
            match job.direction {
                TransferDirection::Download => {
                    let _ = tokio::fs::remove_file(partial_path(&job.local_path, &job)).await;
                }
                _ => {
                    let Some(resume) = job
                        .resume
                        .as_deref()
                        .and_then(|r| serde_json::from_str::<UploadResume>(r).ok())
                    else {
                        return;
                    };
                    let Some(client) = account.and_then(|a| clients.get(&a).ok()) else {
                        return;
                    };
                    abort(&client, &job.bucket, &job.key, &resume.upload_id).await;
                }
            }
        })
    }
}

/// What one attempt works with.
struct Io<'a> {
    client: &'a Client,
    job: &'a TransferJob,
    progress: &'a Progress,
    checkpoint: &'a Checkpoint,
    limiter: &'a Arc<RateLimiter>,
}

fn storage(e: StorageError) -> TransferError {
    TransferError {
        transient: e.is_transient(),
        message: e.display(),
    }
}

fn sdk<E>(
    e: SdkError<E, aws_smithy_runtime_api::client::orchestrator::HttpResponse>,
) -> TransferError
where
    E: aws_sdk_s3::error::ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    storage(StorageError::from_sdk(e))
}

/// HTTP status of a failed request, if the server answered.
fn status<E>(
    e: &SdkError<E, aws_smithy_runtime_api::client::orchestrator::HttpResponse>,
) -> Option<u16> {
    e.raw_response().map(|r| r.status().as_u16())
}

fn local(e: &std::io::Error, path: &Path) -> TransferError {
    use std::io::ErrorKind as K;
    let message = format!("{}: {e}", path.display());
    match e.kind() {
        K::NotFound | K::PermissionDenied | K::AlreadyExists | K::InvalidInput | K::StorageFull => {
            TransferError::permanent(message)
        }
        _ => TransferError::transient(message),
    }
}

async fn abort(client: &Client, bucket: &str, key: &str, upload_id: &str) {
    if let Err(e) = client
        .abort_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await
    {
        tracing::warn!(
            "could not abort multipart upload: {}",
            StorageError::from_sdk(e)
        );
    }
}

async fn delete_source(
    client: &Client,
    io: &Io<'_>,
    bucket: &str,
    key: &str,
) -> Result<(), TransferError> {
    client
        .delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| {
            let e = StorageError::from_sdk(e);
            // Retrying would copy again; the copy is complete, only the delete failed.
            TransferError::permanent(format!(
                "Copied to \"{}\" but could not delete the original: {}",
                io.job.key,
                e.display()
            ))
        })?;
    Ok(())
}

async fn copy_or_move(io: &Io<'_>) -> Result<(), TransferError> {
    let job = io.job;
    let source = job
        .source
        .as_ref()
        .ok_or_else(|| TransferError::permanent("The copy has no source object."))?;
    if source.bucket == job.bucket && source.key == job.key && job.version_id.is_none() {
        return Err(TransferError::permanent(
            "Source and destination are the same object.",
        ));
    }
    let restore = source.bucket == job.bucket && source.key == job.key;
    let copied = crate::ops::copy_object(
        io.client,
        &source.bucket,
        &source.key,
        job.version_id.as_deref(),
        &job.bucket,
        &job.key,
        io.progress,
    )
    .await;
    match copied {
        Ok(()) => {}
        // Restoring a version onto its own key: some servers (SeaweedFS with the "null" version)
        // ignore the version in the copy source and refuse a copy onto itself. Stream the old
        // version instead; the result is the same new current version.
        Err(e) if restore && e.kind == crate::error::StorageErrorKind::InvalidRequest => {
            tracing::info!(
                "server-side restore refused ({}); streaming the version instead",
                e.message
            );
            return copy_between_accounts(io, io.client).await;
        }
        Err(e) => return Err(storage(e)),
    }
    if job.direction == TransferDirection::Move {
        delete_source(io.client, io, &source.bucket, &source.key).await?;
    }
    Ok(())
}

/// Streams an object from another account's client into this one.
async fn copy_between_accounts(io: &Io<'_>, source_client: &Client) -> Result<(), TransferError> {
    let job = io.job;
    let source = job
        .source
        .as_ref()
        .ok_or_else(|| TransferError::permanent("The copy has no source object."))?;
    // An unfinished upload from an earlier attempt cannot be continued from a stream: start over.
    if let Some(old) = job
        .resume
        .as_deref()
        .and_then(|r| serde_json::from_str::<UploadResume>(r).ok())
    {
        abort(io.client, &job.bucket, &job.key, &old.upload_id).await;
        (io.checkpoint)(None);
    }
    let response = source_client
        .get_object()
        .bucket(&source.bucket)
        .key(&source.key)
        .set_version_id(job.version_id.clone())
        .send()
        .await
        .map_err(sdk)?;
    let total = response
        .content_length()
        .and_then(|l| u64::try_from(l).ok())
        .unwrap_or(0);
    let content_type = response.content_type().map(str::to_owned);
    let metadata = response.metadata().cloned();
    (io.progress)(0, total);
    let mut body = response.body;

    if total <= PART_SIZE {
        let mut data = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
        while let Some(chunk) = body
            .try_next()
            .await
            .map_err(|e| TransferError::transient(format!("Copy interrupted: {e}")))?
        {
            io.limiter.acquire(chunk.len() as u64).await;
            data.extend_from_slice(&chunk);
            (io.progress)(data.len() as u64, total);
        }
        io.client
            .put_object()
            .bucket(&job.bucket)
            .key(&job.key)
            .set_content_type(content_type)
            .set_metadata(metadata)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(sdk)?;
    } else {
        let created = io
            .client
            .create_multipart_upload()
            .bucket(&job.bucket)
            .key(&job.key)
            .set_content_type(content_type)
            .set_metadata(metadata)
            .send()
            .await
            .map_err(sdk)?;
        let upload_id = created
            .upload_id()
            .ok_or_else(|| TransferError::transient("The server did not return an upload id."))?
            .to_owned();
        let resume = UploadResume {
            upload_id: upload_id.clone(),
            size: total,
            mtime: 0,
        };
        (io.checkpoint)(serde_json::to_string(&resume).ok());
        let mut parts = Vec::new();
        let mut buffer: Vec<u8> = Vec::with_capacity(usize::try_from(PART_SIZE).unwrap_or(0));
        let mut done = 0u64;
        let mut number = 1i32;
        loop {
            let chunk = body
                .try_next()
                .await
                .map_err(|e| TransferError::transient(format!("Copy interrupted: {e}")))?;
            let finished = chunk.is_none();
            if let Some(chunk) = chunk {
                io.limiter.acquire(chunk.len() as u64).await;
                buffer.extend_from_slice(&chunk);
            }
            while buffer.len() as u64 >= PART_SIZE || (finished && !buffer.is_empty()) {
                let take = buffer
                    .len()
                    .min(usize::try_from(PART_SIZE).unwrap_or(usize::MAX));
                let part: Vec<u8> = buffer.drain(..take).collect();
                let size = part.len() as u64;
                let r = io
                    .client
                    .upload_part()
                    .bucket(&job.bucket)
                    .key(&job.key)
                    .upload_id(&upload_id)
                    .part_number(number)
                    .body(ByteStream::from(part))
                    .send()
                    .await
                    .map_err(sdk)?;
                parts.push(
                    CompletedPart::builder()
                        .part_number(number)
                        .set_e_tag(r.e_tag().map(str::to_owned))
                        .build(),
                );
                number += 1;
                done += size;
                (io.progress)(done, total);
            }
            if finished {
                break;
            }
        }
        io.client
            .complete_multipart_upload()
            .bucket(&job.bucket)
            .key(&job.key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send()
            .await
            .map_err(sdk)?;
        (io.checkpoint)(None);
    }
    if job.direction == TransferDirection::Move {
        delete_source(source_client, io, &source.bucket, &source.key).await?;
    }
    Ok(())
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Parts the server already has for an upload: part number → (etag, size).
async fn existing_parts(
    client: &Client,
    job: &TransferJob,
    upload_id: &str,
) -> Option<HashMap<i32, (String, u64)>> {
    let mut out = HashMap::new();
    let mut marker: Option<String> = None;
    loop {
        let r = client
            .list_parts()
            .bucket(&job.bucket)
            .key(&job.key)
            .upload_id(upload_id)
            .set_part_number_marker(marker)
            .send()
            .await
            .ok()?;
        for p in r.parts() {
            if let (Some(n), Some(etag)) = (p.part_number(), p.e_tag()) {
                let size = p.size().and_then(|s| u64::try_from(s).ok()).unwrap_or(0);
                out.insert(n, (etag.to_owned(), size));
            }
        }
        match (r.is_truncated(), r.next_part_number_marker()) {
            (Some(true), Some(m)) => marker = Some(m.to_owned()),
            _ => break,
        }
    }
    Some(out)
}

/// Body for `size` bytes of the job's file from `offset`. Under a bandwidth limit the bytes are
/// read into memory and released in small chunks (see [`throttle`]); otherwise streamed from disk.
async fn part_body(
    io: &Io<'_>,
    streamed: &Arc<AtomicU64>,
    offset: u64,
    size: u64,
) -> Result<ByteStream, TransferError> {
    let path = &io.job.local_path;
    if io.limiter.limit() == 0 {
        return ByteStream::read_from()
            .path(path)
            .offset(offset)
            .length(Length::Exact(size))
            .build()
            .await
            .map_err(|e| TransferError::permanent(format!("{}: {e}", path.display())));
    }
    let size = usize::try_from(size).map_err(|_| TransferError::permanent("Part too large."))?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| local(&e, path))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| local(&e, path))?;
    let mut data = vec![0; size];
    file.read_exact(&mut data)
        .await
        .map_err(|e| local(&e, path))?;
    Ok(throttle::body(
        Bytes::from(data),
        io.limiter.clone(),
        streamed.clone(),
    ))
}

/// Interval for progress reports while a request body streams.
fn progress_tick() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick
}

/// Reports `value` (capped at `len`) when it moves past what was shown.
fn report(io: &Io<'_>, shown: &mut u64, value: u64, len: u64) {
    let value = value.min(len);
    if value > *shown {
        *shown = value;
        (io.progress)(value, len);
    }
}

async fn upload(io: &Io<'_>) -> Result<(), TransferError> {
    let job = io.job;
    let path = &job.local_path;
    let meta = tokio::fs::metadata(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TransferError::permanent(format!("Local file not found: {}", path.display()))
        } else {
            local(&e, path)
        }
    })?;
    let len = meta.len();
    let mtime = mtime_secs(&meta);
    (io.progress)(0, len);

    // Bytes of throttled bodies on the wire, for progress within a part.
    let streamed = Arc::new(AtomicU64::new(0));
    let mut shown = 0;
    let mut tick = progress_tick();

    if len <= PART_SIZE {
        let body = part_body(io, &streamed, 0, len).await?;
        let send = io
            .client
            .put_object()
            .bucket(&job.bucket)
            .key(&job.key)
            .body(body)
            .send();
        tokio::pin!(send);
        loop {
            tokio::select! {
                sent = &mut send => {
                    sent.map_err(sdk)?;
                    break;
                }
                _ = tick.tick() => report(io, &mut shown, streamed.load(Ordering::Relaxed), len),
            }
        }
        (io.progress)(len, len);
        return Ok(());
    }

    // Continue an earlier multipart upload when the file is unchanged.
    let mut upload_id = None;
    let mut have: HashMap<i32, (String, u64)> = HashMap::new();
    if let Some(old) = job
        .resume
        .as_deref()
        .and_then(|r| serde_json::from_str::<UploadResume>(r).ok())
    {
        if old.size == len && old.mtime == mtime {
            if let Some(parts) = existing_parts(io.client, job, &old.upload_id).await {
                have = parts;
                upload_id = Some(old.upload_id);
            }
        } else {
            abort(io.client, &job.bucket, &job.key, &old.upload_id).await;
        }
    }
    let upload_id = match upload_id {
        Some(id) => id,
        None => {
            let created = io
                .client
                .create_multipart_upload()
                .bucket(&job.bucket)
                .key(&job.key)
                .send()
                .await
                .map_err(sdk)?;
            let id = created
                .upload_id()
                .ok_or_else(|| TransferError::transient("The server did not return an upload id."))?
                .to_owned();
            let resume = UploadResume {
                upload_id: id.clone(),
                size: len,
                mtime,
            };
            (io.checkpoint)(serde_json::to_string(&resume).ok());
            id
        }
    };

    let part_count = len.div_ceil(PART_SIZE);
    let expected = |index: u64| PART_SIZE.min(len - index * PART_SIZE);
    let mut parts: Vec<CompletedPart> = Vec::new();
    let mut todo = Vec::new();
    let mut done = 0u64;
    for index in 0..part_count {
        let number = i32::try_from(index + 1)
            .map_err(|_| TransferError::permanent("File has too many parts."))?;
        match have.get(&number) {
            Some((etag, size)) if *size == expected(index) => {
                done += size;
                parts.push(
                    CompletedPart::builder()
                        .part_number(number)
                        .e_tag(etag)
                        .build(),
                );
            }
            _ => todo.push((index, number)),
        }
    }
    (io.progress)(done, len);
    shown = done;

    // Under a bandwidth limit the link is the bottleneck anyway; one part at a time finishes
    // parts early, so a pause or restart keeps more of the work.
    let in_flight = if io.limiter.limit() > 0 {
        1
    } else {
        PARTS_IN_FLIGHT
    };
    let mut uploads = stream::iter(todo)
        .map(|(index, number)| {
            let upload_id = upload_id.clone();
            let streamed = &streamed;
            async move {
                let size = expected(index);
                let body = part_body(io, streamed, index * PART_SIZE, size).await?;
                let r = io
                    .client
                    .upload_part()
                    .bucket(&job.bucket)
                    .key(&job.key)
                    .upload_id(upload_id)
                    .part_number(number)
                    .body(body)
                    .send()
                    .await
                    .map_err(sdk)?;
                let part = CompletedPart::builder()
                    .part_number(number)
                    .set_e_tag(r.e_tag().map(str::to_owned))
                    .build();
                Ok::<_, TransferError>((part, size))
            }
        })
        .buffer_unordered(in_flight);
    loop {
        tokio::select! {
            next = uploads.try_next() => match next? {
                Some((part, size)) => {
                    done += size;
                    parts.push(part);
                    report(io, &mut shown, done, len);
                }
                None => break,
            },
            _ = tick.tick() => {
                report(io, &mut shown, done + streamed.load(Ordering::Relaxed), len);
            }
        }
    }
    drop(uploads);
    parts.sort_by_key(CompletedPart::part_number);

    io.client
        .complete_multipart_upload()
        .bucket(&job.bucket)
        .key(&job.key)
        .upload_id(&upload_id)
        .multipart_upload(
            CompletedMultipartUpload::builder()
                .set_parts(Some(parts))
                .build(),
        )
        .send()
        .await
        .map_err(sdk)?;
    (io.checkpoint)(None);
    Ok(())
}

async fn download(io: &Io<'_>) -> Result<(), TransferError> {
    let job = io.job;
    let target = &job.local_path;
    if tokio::fs::symlink_metadata(target).await.is_ok() {
        return Err(TransferError::permanent(format!(
            "\"{}\" already exists. Existing files are never overwritten.",
            target.display()
        )));
    }
    if let Some(dir) = target.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| local(&e, dir))?;
    }
    let temp = partial_path(target, job);

    // Continue a partial file when the object is unchanged (same ETag).
    let mut offset = 0u64;
    let mut etag: Option<String> = None;
    if let Some(old) = job
        .resume
        .as_deref()
        .and_then(|r| serde_json::from_str::<DownloadResume>(r).ok())
        && let Ok(meta) = tokio::fs::metadata(&temp).await
    {
        offset = meta.len();
        etag = Some(old.etag);
    }

    let response = loop {
        let mut request = io
            .client
            .get_object()
            .bucket(&job.bucket)
            .key(&job.key)
            .set_version_id(job.version_id.clone());
        if offset > 0 {
            request = request
                .range(format!("bytes={offset}-"))
                .set_if_match(etag.clone());
        }
        match request.send().await {
            Ok(r) => break Some(r),
            Err(e) if offset > 0 && status(&e) == Some(412) => {
                // The object changed since the partial download: start over.
                let _ = tokio::fs::remove_file(&temp).await;
                offset = 0;
                etag = None;
            }
            Err(e) if offset > 0 && status(&e) == Some(416) => break None,
            Err(e) => return Err(sdk(e)),
        }
    };

    if let Some(response) = response {
        let total = offset
            + response
                .content_length()
                .and_then(|l| u64::try_from(l).ok())
                .unwrap_or(0);
        if offset == 0 {
            let new_etag = response.e_tag().unwrap_or_default().to_owned();
            (io.checkpoint)(serde_json::to_string(&DownloadResume { etag: new_etag }).ok());
        }
        (io.progress)(offset, total);
        let mut file = if offset == 0 {
            tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp)
                .await
        } else {
            tokio::fs::OpenOptions::new().append(true).open(&temp).await
        }
        .map_err(|e| local(&e, &temp))?;
        let mut body = response.body;
        let mut done = offset;
        while let Some(chunk) = body
            .try_next()
            .await
            .map_err(|e| TransferError::transient(format!("Download interrupted: {e}")))?
        {
            io.limiter.acquire(chunk.len() as u64).await;
            file.write_all(&chunk).await.map_err(|e| local(&e, &temp))?;
            done += chunk.len() as u64;
            (io.progress)(done, total);
        }
        file.flush().await.map_err(|e| local(&e, &temp))?;
        file.sync_all().await.map_err(|e| local(&e, &temp))?;
        drop(file);
        if total > 0 && done != total {
            return Err(TransferError::transient(format!(
                "Download ended early ({done} of {total} bytes)."
            )));
        }
    }
    publish_no_clobber(&temp, target)?;
    (io.checkpoint)(None);
    Ok(())
}

/// Moves `temp` to `target` without ever replacing an existing file.
fn publish_no_clobber(temp: &Path, target: &Path) -> Result<(), TransferError> {
    let exists = || {
        TransferError::permanent(format!(
            "\"{}\" already exists. Existing files are never overwritten.",
            target.display()
        ))
    };
    match std::fs::hard_link(temp, target) {
        Ok(()) => {
            let _ = std::fs::remove_file(temp);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists()),
        // File systems without hard links (FAT/exFAT, some shares): check, then rename.
        Err(_) => {
            if target.exists() {
                return Err(exists());
            }
            std::fs::rename(temp, target).map_err(|e| local(&e, target))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("s3m-pub-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let temp = dir.join("a.txt.x.s3m-partial");
        let target = dir.join("a.txt");
        std::fs::write(&temp, b"new").expect("write");
        publish_no_clobber(&temp, &target).expect("publish");
        assert_eq!(std::fs::read(&target).expect("read"), b"new");
        assert!(!temp.exists());

        std::fs::write(&temp, b"newer").expect("write");
        let err = publish_no_clobber(&temp, &target).unwrap_err();
        assert!(err.message.contains("never overwritten"));
        assert!(!err.transient);
        assert_eq!(std::fs::read(&target).expect("read"), b"new");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn checkpoints_round_trip() {
        let u = UploadResume {
            upload_id: "abc".into(),
            size: 10,
            mtime: 20,
        };
        let text = serde_json::to_string(&u).expect("json");
        assert_eq!(text, r#"{"uploadId":"abc","size":10,"mtime":20}"#);
        assert_eq!(
            serde_json::from_str::<UploadResume>(&text).expect("parse"),
            u
        );
    }
}
