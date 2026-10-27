//! Application state, background task plumbing and the actions behind menus and buttons.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use chrono::Local;
use s3m_core::{
    APP_NAME, Account, AccountDraft, AccountService, BucketEntry, ObjectEntry, TransferDirection,
    TransferJob, TransferOptions, TransferQueue, TransferRequest, TransferState, TransferStore,
    browse, export, format, uploads,
};
use s3m_s3::{S3TransferExecutor, StorageError, StorageErrorKind, StorageService};
use s3m_storage::{Settings, SettingsStore};
use uuid::Uuid;

use crate::model::{
    AccountsWindow, Banner, BottomTab, ConfirmAction, EditorState, LogEntry, Modal, Msg,
    OptionsState, PasswordAction, PromptAction, Reload, SortColumn, TestTarget,
};
use crate::theme;

/// Error type returned while creating the app.
pub type InitError = Box<dyn std::error::Error + Send + Sync>;

/// Everything the app needs from the binary.
pub struct AppConfig {
    /// Account service (JSON file + keychain in normal runs).
    pub accounts: Arc<AccountService>,
    /// Durable transfer records.
    pub transfer_store: Arc<dyn TransferStore>,
    /// Preferences file.
    pub settings_store: SettingsStore,
    /// Secret store with the optional master password (the account service uses it too).
    pub vault: Arc<s3m_core::vault::ProtectedSecrets>,
    /// Data folder (shown in About, opened from Tools).
    pub data_dir: PathBuf,
    /// Problem detected at startup, shown once.
    pub startup_warning: Option<String>,
    /// GitHub repository (`owner/name`) whose releases are checked for updates.
    pub update_repo: Option<String>,
}

/// User intents collected while drawing a frame and applied afterwards.
pub(crate) enum Action {
    CheckUpdates,
    SelectAccount(Uuid),
    ManageAccounts,
    EditAccount(Option<Uuid>),
    TestConnection,
    Refresh,
    SelectBucket(String),
    AddBucketPrompt,
    RemoveBucket(String),
    CopyText(String),
    Navigate(String),
    Up,
    Open(String),
    ClickRow {
        view_index: usize,
        ctrl: bool,
        shift: bool,
    },
    ContextRow(usize),
    SelectAll,
    UploadFiles,
    UploadFolder,
    UploadDropped(Vec<PathBuf>),
    Download,
    NewFolderPrompt,
    RenamePrompt,
    DeleteSelected,
    LoadMore,
    SortBy(SortColumn),
    CancelJob(Uuid),
    RetryJob(Uuid),
    PauseJob(Uuid),
    PauseAll,
    ResumeAll,
    ClearFinished,
    DismissBanner,
    Options,
    About,
    ImportAccounts,
    ExportAccounts,
    /// Switch between the light and dark theme.
    ToggleTheme,
    /// Open the access keys of the current account.
    AccessKeys,
    OpenFolder(PathBuf),
    Exit,
    Copy,
    Cut,
    Paste,
    CopyPath,
    WebUrl,
    CreateBucketPrompt,
    DeleteBucket(String),
    BucketProperties(String),
    EditHeaders,
    BucketSettings(String),
    DownloadVersion(String, String),
    RestoreVersion(String, String),
    DeleteVersion(String, String, bool),
    SaveObjectAcl(String),
    Sync,
    MasterPassword,
    Lock,
    Unlock,
}

/// The Prometheus S3Manager application.
pub struct App {
    pub(crate) rt: tokio::runtime::Runtime,
    pub(crate) accounts: Arc<AccountService>,
    pub(crate) storage: Arc<StorageService>,
    pub(crate) queue: TransferQueue,
    /// Shared bandwidth limit of all transfers.
    pub(crate) limiter: Arc<s3m_s3::RateLimiter>,
    pub(crate) settings_store: SettingsStore,
    pub(crate) settings: Settings,
    pub(crate) data_dir: PathBuf,
    pub(crate) tx: Sender<Msg>,
    pub(crate) rx: Receiver<Msg>,
    pub(crate) jobs_rx: Receiver<TransferJob>,
    pub(crate) ctx: egui::Context,

    pub(crate) account_list: Vec<Account>,
    pub(crate) accounts_revision: u64,
    pub(crate) current_account: Option<Uuid>,
    /// Account (and its settings stamp) the bucket list was loaded for.
    pub(crate) loaded_for: Option<(Uuid, chrono::DateTime<chrono::Utc>)>,

    pub(crate) buckets: Vec<BucketEntry>,
    pub(crate) restricted: bool,
    pub(crate) current_bucket: Option<String>,
    pub(crate) buckets_loading: bool,

    pub(crate) prefix: String,
    pub(crate) objects: Vec<ObjectEntry>,
    pub(crate) next_token: Option<String>,
    pub(crate) objects_loaded: bool,
    pub(crate) objects_loading: bool,
    pub(crate) list_version: u64,

    pub(crate) filter: String,
    pub(crate) sort: (SortColumn, bool),
    pub(crate) view: Vec<usize>,
    pub(crate) view_key: (usize, String, SortColumn, bool, u64),
    pub(crate) objects_generation: u64,
    pub(crate) selection: HashSet<String>,
    pub(crate) anchor: Option<usize>,
    pub(crate) focused: Option<String>,
    pub(crate) properties: Vec<(String, String)>,

    pub(crate) banner: Option<Banner>,
    pub(crate) log: VecDeque<LogEntry>,
    pub(crate) bottom_tab: BottomTab,

    pub(crate) modal: Option<Modal>,
    pub(crate) accounts_window: Option<AccountsWindow>,
    pub(crate) editor: Option<EditorState>,
    pub(crate) options: Option<OptionsState>,
    pub(crate) show_about: bool,
    pub(crate) focus_filter: bool,
    pub(crate) editor_nonce: u64,
    pub(crate) upload_refresh_at: Option<Instant>,
    pub(crate) toolbar_testing: bool,
    /// Folder expansions in flight.
    pub(crate) busy_ops: usize,
    /// Copied or cut items.
    pub(crate) clipboard: Option<s3m_core::plan::Clipboard>,
    /// HEAD of the focused file (Headers tab, header editor).
    pub(crate) details: Option<s3m_core::ObjectDetails>,
    /// Preview of the focused file.
    pub(crate) preview: Option<(String, crate::model::PreviewState)>,
    /// Counter for Web URL dialogs and preview texture keys.
    pub(crate) web_nonce: u64,
    /// Bucket to select once the bucket list reloads (just created).
    pub(crate) pending_bucket: Option<String>,
    /// Folder moves whose emptied source folders are removed when they finish.
    pub(crate) cleanups: Vec<crate::model::FolderCleanup>,
    /// Selection and focus to restore after a refresh of the same folder.
    pub(crate) restore: Option<(HashSet<String>, Option<String>)>,
    /// Versions of the focused file.
    pub(crate) versions: Option<(String, crate::admin_ui::Fetch<Vec<s3m_core::ObjectVersion>>)>,
    /// ACL of the focused file.
    pub(crate) acl: Option<(String, crate::admin_ui::Fetch<crate::admin_ui::AclEdit>)>,
    /// Bucket settings window.
    pub(crate) bucket_settings: Option<crate::admin_ui::BucketSettings>,
    /// Access keys window.
    pub(crate) access_keys: Option<crate::access_keys_ui::KeysWindow>,
    /// Create/edit access key form.
    pub(crate) key_editor: Option<crate::access_keys_ui::KeyEditor>,
    /// Credentials of a key that was just created.
    pub(crate) created_key: Option<crate::access_keys_ui::CreatedKey>,
    /// Sync folder dialog.
    pub(crate) sync: Option<crate::sync_ui::SyncDialog>,
    /// Master password store.
    pub(crate) vault: Arc<s3m_core::vault::ProtectedSecrets>,
    /// Unlock prompt (shown while the master password is not entered).
    pub(crate) unlock: Option<crate::vault_ui::UnlockState>,
    /// Tools > Master password dialog.
    pub(crate) master_dialog: Option<crate::vault_ui::MasterDialog>,
    /// GitHub repository checked for updates (None = this build does not update).
    pub(crate) update_repo: Option<String>,
    /// An update check is running.
    pub(crate) update_checking: bool,
    /// Update dialog.
    pub(crate) update: Option<crate::update_ui::UpdateDialog>,
    /// Keeps the window on its monitor.
    pub(crate) window_fit: crate::window::WindowFit,
}

impl App {
    /// Creates the app: theme, runtime, transfer queue, accounts.
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig) -> Result<Self, InitError> {
        let settings = config.settings_store.load();
        theme::install_fonts(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, settings.dark_mode);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("s3m-worker")
            .enable_all()
            .build()?;
        let storage = Arc::new(StorageService::with_secrets(
            config.accounts.secrets().clone(),
        ));
        let executor = Arc::new(S3TransferExecutor::new(
            config.accounts.clone(),
            storage.clients().clone(),
        ));
        let limiter = executor.limiter().clone();
        limiter.set_limit(settings.bandwidth_limit_kbps * 1024);
        let (jobs_tx, jobs_rx) = channel::<TransferJob>();
        let repaint = cc.egui_ctx.clone();
        let queue = TransferQueue::new(
            executor,
            config.transfer_store,
            TransferOptions {
                concurrency: settings.transfer_concurrency,
                max_attempts: settings.transfer_max_attempts,
                ..TransferOptions::default()
            },
            rt.handle().clone(),
            Arc::new(move |job: &TransferJob| {
                let _ = jobs_tx.send(job.clone());
                repaint.request_repaint();
            }),
        );
        let (tx, rx) = channel();
        let mut app = Self {
            rt,
            accounts: config.accounts,
            storage,
            queue,
            limiter,
            settings_store: config.settings_store,
            settings,
            data_dir: config.data_dir,
            tx,
            rx,
            jobs_rx,
            ctx: cc.egui_ctx.clone(),
            account_list: Vec::new(),
            accounts_revision: u64::MAX,
            current_account: None,
            loaded_for: None,
            buckets: Vec::new(),
            restricted: false,
            current_bucket: None,
            buckets_loading: false,
            prefix: String::new(),
            objects: Vec::new(),
            next_token: None,
            objects_loaded: false,
            objects_loading: false,
            list_version: 0,
            filter: String::new(),
            sort: (SortColumn::Name, true),
            view: Vec::new(),
            view_key: (usize::MAX, String::new(), SortColumn::Name, true, u64::MAX),
            objects_generation: 0,
            selection: HashSet::new(),
            anchor: None,
            focused: None,
            properties: Vec::new(),
            banner: None,
            log: VecDeque::new(),
            bottom_tab: BottomTab::Tasks,
            modal: None,
            accounts_window: None,
            editor: None,
            options: None,
            show_about: false,
            focus_filter: false,
            editor_nonce: 0,
            upload_refresh_at: None,
            toolbar_testing: false,
            busy_ops: 0,
            clipboard: None,
            details: None,
            preview: None,
            web_nonce: 0,
            pending_bucket: None,
            cleanups: Vec::new(),
            restore: None,
            versions: None,
            acl: None,
            bucket_settings: None,
            access_keys: None,
            key_editor: None,
            created_key: None,
            sync: None,
            unlock: config
                .vault
                .is_locked()
                .then(crate::vault_ui::UnlockState::default),
            vault: config.vault,
            master_dialog: None,
            update_repo: config.update_repo,
            update_checking: false,
            update: None,
            window_fit: crate::window::WindowFit::default(),
        };
        if let Some(warning) = config.startup_warning {
            app.log(warning.clone(), true);
            app.modal = Some(Modal::Message {
                title: APP_NAME.into(),
                body: warning,
                error: true,
            });
        }
        let proxy = app.proxy_settings(&app.settings);
        app.storage.clients().set_proxy(proxy);
        app.sync_accounts();
        if app.settings.check_updates
            && s3m_core::update::check_due(app.settings.last_update_check, chrono::Utc::now())
        {
            app.check_for_updates(false);
        }
        Ok(app)
    }

    // ------------------------------------------------------------------ plumbing

    /// Runs `fut` on the worker runtime and delivers its message to the UI thread.
    pub(crate) fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = Msg> + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let msg = fut.await;
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// Adds a line to the event log (newest first, 500 kept).
    pub(crate) fn log(&mut self, message: impl Into<String>, is_error: bool) {
        let message = message.into();
        if is_error {
            tracing::warn!("{message}");
        } else {
            tracing::info!("{message}");
        }
        self.log.push_front(LogEntry {
            time: Local::now(),
            message,
            is_error,
        });
        self.log.truncate(500);
    }

    pub(crate) fn show_error(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.modal = Some(Modal::Message {
            title: title.into(),
            body: body.into(),
            error: true,
        });
    }

    pub(crate) fn show_info(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.modal = Some(Modal::Message {
            title: title.into(),
            body: body.into(),
            error: false,
        });
    }

    /// Called at the start of every frame.
    pub(crate) fn pump(&mut self) {
        if self.accounts.revision() != self.accounts_revision {
            self.sync_accounts();
        }
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        while let Ok(job) = self.jobs_rx.try_recv() {
            self.on_job(&job);
        }
        if let Some(at) = self.upload_refresh_at
            && Instant::now() >= at
        {
            self.upload_refresh_at = None;
            if !self.objects_loading {
                self.reload_objects();
            }
        }
        if self.queue.active_count() > 0 {
            // Progress is polled, not pushed: 10 frames a second keep the bars moving smoothly.
            self.ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(at) = self.upload_refresh_at {
            self.ctx
                .request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
    }

    fn on_job(&mut self, job: &TransferJob) {
        if !job.is_active() {
            self.cleanup_after(job);
        }
        match job.state {
            TransferState::Completed => {
                if self.focused.as_deref() == Some(job.key.as_str())
                    && self.current_bucket.as_deref() == Some(job.bucket.as_str())
                {
                    // The file changed (upload, restore, copy onto it): reload its versions.
                    self.versions = None;
                }
                self.log(
                    format!("{} {}: {}", job.direction.label(), job.key, job.summary()),
                    false,
                );
                let here = self.current_bucket.as_deref() == Some(job.bucket.as_str())
                    && job.key.starts_with(&self.prefix);
                let source_here = job.direction == TransferDirection::Move
                    && job.source.as_ref().is_some_and(|s| {
                        self.current_bucket.as_deref() == Some(s.bucket.as_str())
                            && s.key.starts_with(&self.prefix)
                    });
                if (job.direction != TransferDirection::Download && here) || source_here {
                    // Coalesce refreshes while many uploads finish.
                    self.upload_refresh_at
                        .get_or_insert_with(|| Instant::now() + Duration::from_millis(1200));
                }
            }
            TransferState::Failed => {
                self.log(
                    format!("{} {}: {}", job.direction.label(), job.key, job.summary()),
                    true,
                );
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ accounts

    fn sync_accounts(&mut self) {
        self.accounts_revision = self.accounts.revision();
        match self.accounts.list() {
            Ok(list) => self.account_list = list,
            Err(e) => {
                self.account_list.clear();
                self.log(format!("Accounts could not be loaded: {e}"), true);
            }
        }
        let active = self.accounts.active_account_id();
        let previous = self.current_account;
        let still_there = previous.is_some_and(|id| self.account_list.iter().any(|a| a.id == id));
        let target = if still_there && previous == active {
            previous
        } else {
            active
        };
        let changed_settings = target.is_some_and(|id| {
            self.account(id).is_some_and(|a| {
                self.loaded_for
                    .as_ref()
                    .is_some_and(|(lid, stamp)| *lid == id && *stamp != a.updated_at)
            })
        });
        if target != previous || changed_settings || (target.is_some() && self.loaded_for.is_none())
        {
            self.current_account = target;
            self.load_buckets();
        }
    }

    pub(crate) fn account(&self, id: Uuid) -> Option<&Account> {
        self.account_list.iter().find(|a| a.id == id)
    }

    pub(crate) fn current(&self) -> Option<&Account> {
        self.current_account.and_then(|id| self.account(id))
    }

    fn select_account(&mut self, id: Uuid) {
        if Some(id) == self.current_account {
            return;
        }
        if let Err(e) = self.accounts.set_active(id) {
            self.show_error(APP_NAME, e.to_string());
        }
    }

    pub(crate) fn open_editor(&mut self, id: Option<Uuid>) {
        self.editor_nonce += 1;
        let (draft, has_secret) = match id.and_then(|id| self.account(id).cloned()) {
            Some(a) => (
                AccountDraft::from_account(&a),
                self.accounts.has_secret(a.id),
            ),
            None => (
                AccountDraft {
                    endpoint_host: "s3.amazonaws.com".into(),
                    use_tls: true,
                    region: "us-east-1".into(),
                    ..AccountDraft::default()
                },
                false,
            ),
        };
        self.editor = Some(EditorState {
            buckets_text: draft.manual_buckets.join("\n"),
            draft,
            secret: String::new(),
            error: String::new(),
            test_result: None,
            testing: false,
            nonce: self.editor_nonce,
            has_secret,
            advanced: false,
        });
    }

    fn test_connection(&mut self) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        self.toolbar_testing = true;
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Test {
                target: TestTarget::Toolbar,
                result: storage.test_connection(&account).await,
            }
        });
    }

    /// Starts a test from the account editor with the typed (possibly unsaved) settings.
    pub(crate) fn test_from_editor(&mut self) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.error.clear();
        editor.test_result = None;
        let mut draft = editor.draft.clone();
        draft.manual_buckets = editor.buckets_text.lines().map(str::to_owned).collect();
        let account = match s3m_core::account::validate(&draft) {
            Ok(a) => Account {
                id: draft.id.unwrap_or_else(Uuid::new_v4),
                ..a
            },
            Err(e) => {
                editor.error = e.to_string();
                return;
            }
        };
        let secret = if editor.secret.is_empty() {
            draft
                .id
                .and_then(|id| self.accounts.secret(id).ok().flatten())
        } else {
            Some(editor.secret.clone())
        };
        let Some(secret) = secret else {
            editor.error = "Enter the Secret Access Key to test the connection.".into();
            return;
        };
        editor.testing = true;
        let nonce = editor.nonce;
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Test {
                target: TestTarget::Editor(nonce),
                result: storage.test_unsaved(&account, &secret).await,
            }
        });
    }

    /// Saves the account editor; closes it on success.
    pub(crate) fn save_editor(&mut self) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.error.clear();
        let fields =
            s3m_core::endpoint::normalize(&editor.draft.endpoint_host, editor.draft.use_tls);
        editor.draft.endpoint_host = fields.host;
        editor.draft.use_tls = fields.use_tls;
        let mut draft = editor.draft.clone();
        draft.manual_buckets = editor
            .buckets_text
            .split(['\n', '\r', ','])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        draft.secret_access_key = (!editor.secret.is_empty()).then(|| editor.secret.clone());
        if draft.id.is_none() && draft.secret_access_key.is_none() {
            editor.error = "Secret Access Key is required.".into();
            return;
        }
        if s3m_core::presets::has_placeholder(&draft.endpoint_host) {
            editor.error =
                "Replace the placeholder (ACCOUNT_ID, REGION or HOST) in the API endpoint.".into();
            return;
        }
        match self.accounts.save(&draft) {
            Ok(saved) => {
                let created = draft.id.is_none();
                self.editor = None;
                self.storage.clients().invalidate();
                self.log(
                    format!(
                        "{} account \"{}\"",
                        if created { "Added" } else { "Saved" },
                        saved.name
                    ),
                    false,
                );
                if created && self.current_account.is_none() {
                    let _ = self.accounts.set_active(saved.id);
                }
                if let Some(w) = self.accounts_window.as_mut() {
                    w.selected = Some(saved.id);
                    w.revision = u64::MAX;
                }
            }
            Err(e) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.error = e.to_string();
                }
            }
        }
    }

    // ------------------------------------------------------------------ buckets

    pub(crate) fn load_buckets(&mut self) {
        self.restore = None;
        self.list_version += 1;
        let version = self.list_version;
        self.buckets.clear();
        self.restricted = false;
        self.clear_objects();
        self.banner = None;
        let Some(account) = self.current().cloned() else {
            self.loaded_for = None;
            self.current_bucket = None;
            self.buckets_loading = false;
            return;
        };
        if self.vault.is_locked() {
            self.loaded_for = None;
            self.buckets_loading = false;
            self.banner = Some(Banner {
                title: "Secrets are locked".into(),
                detail: "Enter the master password (Tools > Unlock) to connect.".into(),
                offers_remove_bucket: false,
                offers_edit_account: false,
            });
            return;
        }
        self.loaded_for = Some((account.id, account.updated_at));
        self.buckets_loading = true;
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Buckets {
                version,
                result: storage.list_buckets(&account).await,
            }
        });
    }

    fn clear_objects(&mut self) {
        self.objects.clear();
        self.objects_generation += 1;
        self.next_token = None;
        self.objects_loaded = false;
        self.objects_loading = false;
        self.selection.clear();
        self.anchor = None;
        self.focused = None;
        self.properties.clear();
    }

    fn select_bucket(&mut self, name: String) {
        self.restore = None;
        if self.current_bucket.as_deref() == Some(name.as_str()) && self.prefix.is_empty() {
            return;
        }
        self.current_bucket = Some(name);
        self.prefix.clear();
        self.filter.clear();
        self.load_objects(false);
    }

    // ------------------------------------------------------------------ objects

    pub(crate) fn load_objects(&mut self, more: bool) {
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        let token = if more {
            match self.next_token.clone() {
                Some(t) => Some(t),
                None => return,
            }
        } else {
            self.list_version += 1;
            self.clear_objects();
            self.banner = None;
            None
        };
        let version = self.list_version;
        self.objects_loading = true;
        let prefix = self.prefix.clone();
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Objects {
                version,
                more,
                result: storage
                    .list_objects(&account, &bucket, &prefix, token)
                    .await,
            }
        });
    }

    /// Reloads the current folder and restores the selection afterwards.
    pub(crate) fn reload_objects(&mut self) {
        self.restore = Some((self.selection.clone(), self.focused.clone()));
        self.load_objects(false);
    }

    fn navigate(&mut self, prefix: String) {
        self.restore = None;
        if self.current_bucket.is_none() {
            return;
        }
        self.prefix = prefix;
        self.filter.clear();
        self.load_objects(false);
    }

    /// Visible rows: indices into `objects`, filtered and sorted. Recomputed only when needed.
    pub(crate) fn refresh_view(&mut self) {
        let key = (
            self.objects.len(),
            self.filter.clone(),
            self.sort.0,
            self.sort.1,
            self.objects_generation,
        );
        if key == self.view_key {
            return;
        }
        self.view_key = key;
        let needle = self.filter.trim().to_lowercase();
        let mut view: Vec<usize> = (0..self.objects.len())
            .filter(|&i| needle.is_empty() || self.objects[i].name.to_lowercase().contains(&needle))
            .collect();
        let (column, ascending) = self.sort;
        let objects = &self.objects;
        view.sort_by(|&a, &b| {
            let (a, b) = (&objects[a], &objects[b]);
            // Folders always first, like Explorer.
            b.is_folder.cmp(&a.is_folder).then_with(|| {
                let ord = match column {
                    SortColumn::Name => natural_cmp(&a.name, &b.name),
                    SortColumn::Size => a.size.cmp(&b.size),
                    SortColumn::Type => a.type_label().cmp(&b.type_label()),
                    SortColumn::Modified => a.last_modified.cmp(&b.last_modified),
                    SortColumn::Class => a.storage_class.cmp(&b.storage_class),
                }
                .then_with(|| natural_cmp(&a.name, &b.name));
                if ascending { ord } else { ord.reverse() }
            })
        });
        self.view = view;
    }

    pub(crate) fn selected_entries(&self) -> Vec<&ObjectEntry> {
        // Asked several times a frame; skip the walk over a large listing when nothing is selected.
        if self.selection.is_empty() {
            return Vec::new();
        }
        self.objects
            .iter()
            .filter(|o| self.selection.contains(&o.key))
            .collect()
    }

    fn click_row(&mut self, view_index: usize, ctrl: bool, shift: bool) {
        let Some(&i) = self.view.get(view_index) else {
            return;
        };
        let key = self.objects[i].key.clone();
        if shift && let Some(anchor) = self.anchor {
            let (lo, hi) = (anchor.min(view_index), anchor.max(view_index));
            if !ctrl {
                self.selection.clear();
            }
            for v in lo..=hi {
                if let Some(&j) = self.view.get(v) {
                    self.selection.insert(self.objects[j].key.clone());
                }
            }
        } else if ctrl {
            if !self.selection.remove(&key) {
                self.selection.insert(key.clone());
            }
            self.anchor = Some(view_index);
        } else {
            self.selection.clear();
            self.selection.insert(key.clone());
            self.anchor = Some(view_index);
        }
        self.set_focused(Some(key));
    }

    fn set_focused(&mut self, key: Option<String>) {
        if self.focused == key {
            return;
        }
        self.focused = key.clone();
        self.properties.clear();
        self.details = None;
        self.versions = None;
        self.acl = None;
        let Some(key) = key else { return };
        self.properties.push(("Key".into(), key.clone()));
        let Some(entry) = self.objects.iter().find(|o| o.key == key) else {
            return;
        };
        if entry.is_folder {
            return;
        }
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Details {
                result: storage.object_details(&account, &bucket, &key).await,
                key,
            }
        });
    }

    // ------------------------------------------------------------------ messages

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Buckets { version, result } => {
                if version != self.list_version {
                    return;
                }
                self.buckets_loading = false;
                let name = self.current().map(|a| a.name.clone()).unwrap_or_default();
                match result {
                    Ok(listing) => {
                        self.log(
                            format!(
                                "{name}: {} bucket(s){}",
                                listing.buckets.len(),
                                if listing.restricted {
                                    " (ListBuckets denied; showing registered buckets)"
                                } else {
                                    ""
                                }
                            ),
                            false,
                        );
                        self.restricted = listing.restricted;
                        self.buckets = listing.buckets;
                        let keep = self
                            .pending_bucket
                            .take()
                            .or_else(|| self.current_bucket.take());
                        if let Some(keep) =
                            keep.filter(|k| self.buckets.iter().any(|b| &b.name == k))
                        {
                            self.current_bucket = Some(keep);
                            self.load_objects(false);
                        } else {
                            self.prefix.clear();
                        }
                    }
                    Err(e) => {
                        self.current_bucket = None;
                        self.log(e.display(), true);
                        self.banner = Some(self.banner_for(
                            &e,
                            &format!("Couldn't list buckets for \"{name}\""),
                            None,
                        ));
                    }
                }
            }
            Msg::Objects {
                version,
                more,
                result,
            } => {
                if version != self.list_version {
                    return;
                }
                self.objects_loading = false;
                match result {
                    Ok(page) => {
                        if !more {
                            self.objects.clear();
                        }
                        self.objects.extend(page.entries);
                        self.objects_generation += 1;
                        self.next_token = page.next_token;
                        self.objects_loaded = true;
                        if let Some((selection, focused)) = self.restore.take() {
                            self.selection = selection
                                .into_iter()
                                .filter(|k| self.objects.iter().any(|o| &o.key == k))
                                .collect();
                            if let Some(key) = focused.filter(|k| self.selection.contains(k)) {
                                self.set_focused(Some(key));
                            }
                        }
                    }
                    Err(e) => {
                        self.log(e.display(), true);
                        let bucket = self
                            .current_bucket
                            .as_ref()
                            .and_then(|b| self.buckets.iter().find(|x| &x.name == b))
                            .cloned();
                        self.banner =
                            Some(self.banner_for(&e, "Couldn't list this folder", bucket.as_ref()));
                    }
                }
            }
            Msg::Details { key, result } => {
                if self.focused.as_deref() != Some(key.as_str()) {
                    return;
                }
                match result {
                    Ok(d) => {
                        self.details = Some(d.clone());
                        let p = &mut self.properties;
                        p.push((
                            "Size".into(),
                            format!("{} ({} bytes)", format::size(d.size), format::count(d.size)),
                        ));
                        p.push((
                            "Last modified".into(),
                            d.last_modified
                                .map(|t| {
                                    t.with_timezone(&Local)
                                        .format("%A, %d %B %Y %H:%M:%S")
                                        .to_string()
                                })
                                .unwrap_or_default(),
                        ));
                        p.push(("Content type".into(), d.content_type.unwrap_or_default()));
                        p.push(("ETag".into(), d.etag.unwrap_or_default()));
                        p.push((
                            "Storage class".into(),
                            d.storage_class.unwrap_or_else(|| "STANDARD".into()),
                        ));
                        if let Some(v) = d.version_id {
                            p.push(("Version".into(), v));
                        }
                        for (k, v) in d.metadata {
                            p.push((format!("x-amz-meta-{k}"), v));
                        }
                    }
                    Err(e) => self.properties.push(("Error".into(), e.display())),
                }
            }
            Msg::Done {
                title,
                result,
                reload,
            } => {
                match result {
                    Ok(line) => self.log(line, false),
                    Err(e) => {
                        self.log(e.display(), true);
                        self.show_error(title, e.display());
                    }
                }
                match reload {
                    Reload::Objects => self.reload_objects(),
                    Reload::Buckets => self.load_buckets(),
                }
            }
            Msg::Expanded { purpose, result } => self.handle_expanded(purpose, result),
            Msg::BucketInfo { result } => {
                self.properties = match result {
                    Ok(info) => vec![
                        ("Bucket".into(), info.name),
                        (
                            "Region".into(),
                            info.region
                                .unwrap_or_else(|| "Unknown (not allowed for this key)".into()),
                        ),
                        (
                            "Versioning".into(),
                            info.versioning
                                .unwrap_or_else(|| "Unknown (not allowed for this key)".into()),
                        ),
                    ],
                    Err(e) => vec![("Error".into(), e.display())],
                };
            }
            Msg::Presigned { nonce, result } => {
                if let Some(Modal::WebUrl {
                    nonce: current,
                    url,
                    error,
                    busy,
                    ..
                }) = self.modal.as_mut()
                    && *current == nonce
                {
                    *busy = false;
                    match result {
                        Ok(u) => *url = u,
                        Err(e) => *error = e.display(),
                    }
                }
            }
            Msg::Preview { key, result } => self.handle_preview(key, result),
            Msg::Versions { key, result } => self.handle_versions(key, result),
            Msg::SyncPlan { nonce, result } => self.handle_sync_plan(nonce, result),
            Msg::UpdateChecked { manual, result } => self.handle_update_checked(manual, result),
            Msg::UpdateProgress { done, total } => self.handle_update_progress(done, total),
            Msg::UpdateDownloaded { result } => self.handle_update_downloaded(result),
            Msg::Acl { key, result } => self.handle_acl(key, result),
            Msg::BucketSettings { bucket, data } => self.handle_bucket_settings(&bucket, *data),
            Msg::Keys(msg) => self.handle_keys_msg(msg),
            Msg::SettingsSaved {
                bucket,
                result,
                reload,
            } => self.handle_settings_saved(&bucket, result, reload),
            Msg::Test { target, result } => {
                let latency = result.latency.as_millis();
                match target {
                    TestTarget::Toolbar => {
                        self.toolbar_testing = false;
                        let name = self.current().map(|a| a.name.clone()).unwrap_or_default();
                        self.log(
                            format!("Test connection {name}: {} ({latency} ms)", result.message),
                            !result.ok,
                        );
                        if result.ok {
                            self.show_info(
                                "Test connection",
                                format!("{}\nLatency: {latency} ms", result.message),
                            );
                        } else {
                            self.show_error("Test connection", result.message);
                        }
                    }
                    TestTarget::Editor(nonce) => {
                        if let Some(editor) = self.editor.as_mut().filter(|e| e.nonce == nonce) {
                            editor.testing = false;
                            editor.test_result = Some(result);
                        }
                    }
                }
            }
        }
    }

    fn banner_for(&self, e: &StorageError, fallback: &str, bucket: Option<&BucketEntry>) -> Banner {
        let account = self.current();
        let name = account.map(|a| a.name.as_str()).unwrap_or("");
        let host = account.map(|a| a.endpoint_host.as_str()).unwrap_or("");
        let mut banner = Banner {
            title: fallback.to_owned(),
            detail: e.display(),
            offers_remove_bucket: false,
            offers_edit_account: false,
        };
        match (e.kind, bucket) {
            (StorageErrorKind::AccessDenied, Some(b)) => {
                banner.title = format!("No access to bucket \"{}\"", b.name);
                banner.detail = format!(
                    "The key of \"{name}\" is not allowed to read this bucket, or the bucket doesn't exist on {host}. {}",
                    if b.manual {
                        "Check the spelling, or remove it from the list if it was added by mistake."
                    } else {
                        "Ask the storage admin to grant this key read access."
                    }
                );
                banner.offers_remove_bucket = b.manual;
            }
            (StorageErrorKind::NotFound, Some(b)) => {
                banner.title = format!("Bucket \"{}\" not found", b.name);
                banner.detail = format!("It doesn't exist on {host}. Check the spelling.");
                banner.offers_remove_bucket = b.manual;
            }
            (StorageErrorKind::InvalidCredentials, _) => {
                banner.title = "The access key or secret key is not accepted".into();
                banner.detail = format!("{} Use Edit account to re-enter the keys.", e.message);
                banner.offers_edit_account = true;
            }
            (StorageErrorKind::Tls, _) => {
                banner.title = "The server's certificate is not trusted".into();
                banner.detail = e.hint.clone().unwrap_or_else(|| e.message.clone());
                banner.offers_edit_account = true;
            }
            (StorageErrorKind::PrivateNetwork, _) => {
                banner.title = "Plain HTTP refused outside the private network".into();
                banner.offers_edit_account = true;
            }
            (StorageErrorKind::Network | StorageErrorKind::Timeout, _) => {
                banner.title = format!("Can't reach {host}");
                banner.detail =
                    format!("{} Check VPN, network and the endpoint address.", e.message);
                banner.offers_edit_account = true;
            }
            _ => {}
        }
        banner
    }

    // ------------------------------------------------------------------ transfers

    pub(crate) fn enqueue(&mut self, requests: Vec<TransferRequest>) {
        let Some(first) = requests.first() else {
            return;
        };
        let what = if first.direction == TransferDirection::Upload {
            "upload"
        } else {
            "download"
        };
        let count = requests.len();
        self.queue.enqueue(requests);
        self.bottom_tab = crate::model::BottomTab::Tasks;
        self.log(format!("Queued {count} {what}(s)"), false);
    }

    fn uploads_from(&mut self, paths: Vec<PathBuf>, always_confirm: bool) {
        let (Some(account), Some(bucket)) = (self.current_account, self.current_bucket.clone())
        else {
            return;
        };
        let requests = uploads::collect(account, &bucket, &self.prefix, &paths);
        if requests.is_empty() {
            return;
        }
        let threshold = self.settings.confirm_upload_over;
        if always_confirm || (threshold > 0 && requests.len() > threshold) {
            self.modal = Some(Modal::Confirm {
                title: "Upload".into(),
                body: format!(
                    "Upload {} file(s) ({}) to \"{bucket}/{}\"?\n\nExisting objects with the same keys are overwritten.",
                    requests.len(),
                    format::size(requests.iter().map(|r| r.size_hint).sum()),
                    self.prefix
                ),
                ok: "Upload".into(),
                alternative: None,
                action: ConfirmAction::Enqueue { requests },
                danger: false,
            });
        } else {
            self.enqueue(requests);
        }
    }

    // ------------------------------------------------------------------ prompts

    pub(crate) fn submit_prompt(&mut self, action: PromptAction, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        match action {
            PromptAction::AddBucket => {
                if let Some(id) = self.current_account
                    && let Err(e) = self.accounts.add_manual_bucket(id, &text)
                {
                    self.show_error("Add bucket", e.to_string());
                }
            }
            PromptAction::NewFolder => {
                let (Some(account), Some(bucket)) =
                    (self.current().cloned(), self.current_bucket.clone())
                else {
                    return;
                };
                let prefix = self.prefix.clone();
                let storage = self.storage.clone();
                self.spawn(async move {
                    let result = storage
                        .create_folder(&account, &bucket, &prefix, &text)
                        .await
                        .map(|()| format!("Created folder {bucket}/{prefix}{text}/"));
                    Msg::Done {
                        title: format!("Couldn't create folder \"{text}\""),
                        result,
                        reload: Reload::Objects,
                    }
                });
            }
            PromptAction::RenameFolder { key } => self.rename_folder(key, text),
            PromptAction::Rename { key } => {
                let old = browse::leaf_name(&key).to_owned();
                if text == old {
                    return;
                }
                if text.contains('/') {
                    self.show_error("Rename", "The new name must not contain \"/\".");
                    return;
                }
                let (Some(account), Some(bucket)) =
                    (self.current().cloned(), self.current_bucket.clone())
                else {
                    return;
                };
                let destination = format!("{}{text}", self.prefix);
                let storage = self.storage.clone();
                self.spawn(async move {
                    let result = storage
                        .rename_object(&account, &bucket, &key, &destination)
                        .await
                        .map(|()| format!("Renamed {key} -> {destination}"));
                    Msg::Done {
                        title: format!("Couldn't rename \"{old}\""),
                        result,
                        reload: Reload::Objects,
                    }
                });
            }
        }
    }

    pub(crate) fn submit_confirm(&mut self, action: ConfirmAction, alternative: bool) {
        match action {
            ConfirmAction::Delete { keys } => {
                let (Some(account), Some(bucket)) =
                    (self.current().cloned(), self.current_bucket.clone())
                else {
                    return;
                };
                let storage = self.storage.clone();
                self.spawn(async move {
                    let result = storage
                        .delete_objects(&account, &bucket, &keys)
                        .await
                        .map(|n| format!("Deleted {n} file(s) from {bucket}"));
                    Msg::Done {
                        title: "Couldn't delete the selected files".into(),
                        result,
                        reload: Reload::Objects,
                    }
                });
            }
            ConfirmAction::RemoveBucket { name } => {
                if let Some(account) = self.current().cloned() {
                    match self.accounts.remove_manual_bucket(account.id, &name) {
                        Ok(()) => self.log(
                            format!(
                                "Removed \"{name}\" from the bucket list of {}",
                                account.name
                            ),
                            false,
                        ),
                        Err(e) => self.show_error("Remove bucket", e.to_string()),
                    }
                }
            }
            ConfirmAction::Enqueue { requests } => self.enqueue(requests),
            ConfirmAction::DeleteMany { keys } => self.delete_keys(keys),
            ConfirmAction::DeleteVersion { key, version } => self.delete_version(key, version),
            ConfirmAction::DeleteAccount { id } => {
                let name = self.account(id).map(|a| a.name.clone()).unwrap_or_default();
                match self.accounts.remove(id) {
                    Ok(()) => {
                        self.storage.clients().invalidate();
                        self.log(format!("Deleted account \"{name}\""), false);
                    }
                    Err(e) => self.show_error("Delete account", e.to_string()),
                }
            }
            ConfirmAction::Import { preview, note } => self.save_import(&preview, &note),
            ConfirmAction::ExportSecrets => {
                if alternative {
                    self.export_to_file(None);
                } else {
                    self.modal = Some(Modal::Password {
                        title: "Export password".into(),
                        body: format!(
                            "Choose a password (at least {} characters). It is needed to import the keys.",
                            export::MIN_PASSWORD_LENGTH
                        ),
                        confirm: true,
                        first: String::new(),
                        second: String::new(),
                        error: String::new(),
                        action: PasswordAction::Export,
                    });
                }
            }
        }
    }

    pub(crate) fn submit_password(&mut self, action: PasswordAction, password: String) {
        match action {
            PasswordAction::Export => self.export_to_file(Some(password)),
            PasswordAction::Import { text } => self.preview_import(&text, Some(&password)),
        }
    }

    // ------------------------------------------------------------------ import / export

    fn export_accounts(&mut self) {
        if self.account_list.is_empty() {
            self.show_info("Export accounts", "There are no accounts to export.");
            return;
        }
        self.modal = Some(Modal::Confirm {
            title: "Export accounts".into(),
            body: "Include Secret Access Keys?\n\nInclude: keys are encrypted with a password you choose.\nSettings only: endpoints, buckets and Access Key IDs, no secrets.".into(),
            ok: "Include keys…".into(),
            alternative: Some("Settings only".into()),
            action: ConfirmAction::ExportSecrets,
            danger: false,
        });
    }

    fn export_to_file(&mut self, password: Option<String>) {
        let name = format!("s3-accounts-{}.json", Local::now().format("%Y-%m-%d"));
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export accounts")
            .set_file_name(name)
            .add_filter("Prometheus S3Manager accounts", &["json"])
            .save_file()
        else {
            return;
        };
        let accounts = self.account_list.clone();
        let service = self.accounts.clone();
        let result = export::export(
            &accounts,
            |a| service.secret(a.id).ok().flatten(),
            password.as_deref(),
        )
        .map_err(|e| e.to_string())
        .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                let note = format!(
                    "Exported {} account(s){} to {}.",
                    accounts.len(),
                    if password.is_some() {
                        " with encrypted secret keys"
                    } else {
                        " without secret keys"
                    },
                    path.display()
                );
                self.log(note.clone(), false);
                if let Some(w) = self.accounts_window.as_mut() {
                    w.notice = note;
                }
            }
            Err(e) => self.show_error("Export accounts", e),
        }
    }

    fn import_accounts(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import accounts")
            .add_filter("Prometheus S3Manager accounts", &["json"])
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() > export::MAX_FILE_BYTES as u64 => {
                self.show_error("Import accounts", "Import file is too large.");
                return;
            }
            _ => match std::fs::read_to_string(&path) {
                Ok(t) => t.trim_start_matches('\u{feff}').to_owned(),
                Err(e) => {
                    self.show_error("Import accounts", e.to_string());
                    return;
                }
            },
        };
        if export::has_encrypted_secrets(&text) {
            self.modal = Some(Modal::Password {
                title: "Import accounts".into(),
                body: "This file contains encrypted secret keys. Enter its password.".into(),
                confirm: false,
                first: String::new(),
                second: String::new(),
                error: String::new(),
                action: PasswordAction::Import { text },
            });
        } else {
            self.preview_import(&text, None);
        }
    }

    fn preview_import(&mut self, text: &str, password: Option<&str>) {
        match export::read(text, password) {
            Ok(preview) => {
                let names: Vec<String> = preview
                    .accounts
                    .iter()
                    .take(15)
                    .map(|a| format!("• {} ({})", a.name, a.endpoint_host))
                    .collect();
                let mut note = String::new();
                if !preview.has_encrypted_secrets {
                    note.push_str(" Secret keys were not included; edit each account to add them.");
                }
                if preview.secrets_unsupported {
                    note.push_str(" (Secrets in version 1 export files cannot be read here.)");
                }
                self.modal = Some(Modal::Confirm {
                    title: "Import accounts".into(),
                    body: format!(
                        "Add {} account(s)? Existing accounts are not changed.\n\n{}",
                        preview.accounts.len(),
                        names.join("\n")
                    ),
                    ok: "Import".into(),
                    alternative: None,
                    action: ConfirmAction::Import { preview, note },
                    danger: false,
                });
            }
            Err(e) => self.show_error("Import accounts", e.to_string()),
        }
    }

    fn save_import(&mut self, preview: &export::ImportPreview, note: &str) {
        let mut failed = Vec::new();
        let mut last = None;
        for draft in &preview.accounts {
            match self.accounts.save(draft) {
                Ok(saved) => last = Some(saved.id),
                Err(e) => failed.push(format!("{}: {e}", draft.name)),
            }
        }
        let mut message = format!(
            "Imported {} account(s).{note}",
            preview.accounts.len() - failed.len()
        );
        if !failed.is_empty() {
            message.push_str(&format!(" Skipped: {}", failed.join("; ")));
        }
        self.log(message.clone(), !failed.is_empty());
        match self.accounts_window.as_mut() {
            Some(w) => {
                w.notice = message;
                w.selected = last.or(w.selected);
            }
            None => self.show_info("Import accounts", message),
        }
    }

    // ------------------------------------------------------------------ settings

    /// Proxy settings from preferences plus the keychain password.
    pub(crate) fn proxy_settings(&self, settings: &Settings) -> s3m_s3::ProxySettings {
        match settings.proxy_mode.as_str() {
            "system" => s3m_s3::ProxySettings::System,
            "manual" => s3m_s3::ProxySettings::Manual {
                url: settings.proxy_url.clone(),
                user: settings.proxy_user.clone(),
                password: self
                    .accounts
                    .secrets()
                    .get(s3m_storage::PROXY_SECRET_ID)
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                no_proxy: settings.proxy_no_proxy.clone(),
            },
            _ => s3m_s3::ProxySettings::None,
        }
    }

    /// Saves settings to disk and keeps them in memory.
    pub(crate) fn persist_settings(&mut self, settings: Settings) -> Result<(), String> {
        self.settings_store
            .save(&settings)
            .map_err(|e| format!("Settings could not be saved: {e}"))?;
        self.settings = settings;
        Ok(())
    }

    pub(crate) fn save_options(&mut self, settings: Settings, proxy_password: String) {
        let mut settings = settings.sanitized();
        settings.master_password = self.settings.master_password.clone();
        if !proxy_password.is_empty()
            && let Err(e) = self
                .accounts
                .secrets()
                .set(s3m_storage::PROXY_SECRET_ID, &proxy_password)
        {
            self.show_error(
                "Options",
                format!("The proxy password could not be saved: {e}"),
            );
            return;
        }
        let proxy = self.proxy_settings(&settings);
        if proxy != self.storage.clients().proxy() {
            self.storage.clients().set_proxy(proxy);
            self.log("Proxy settings changed; connections are rebuilt.", false);
        }
        if let Err(e) = self.queue.update_options(TransferOptions {
            concurrency: settings.transfer_concurrency,
            max_attempts: settings.transfer_max_attempts,
            ..self.queue.options()
        }) {
            self.show_error("Options", e);
            return;
        }
        self.limiter.set_limit(settings.bandwidth_limit_kbps * 1024);
        if settings.dark_mode != self.settings.dark_mode {
            theme::apply(&self.ctx, settings.dark_mode);
        }
        if let Err(e) = self.settings_store.save(&settings) {
            self.show_error("Options", format!("Settings could not be saved: {e}"));
        }
        self.settings = settings;
    }

    // ------------------------------------------------------------------ actions

    pub(crate) fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for action in actions {
            self.apply_one(ctx, action);
        }
    }

    fn apply_one(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::SelectAccount(id) => self.select_account(id),
            Action::ManageAccounts => {
                self.accounts_window = Some(AccountsWindow {
                    revision: u64::MAX,
                    selected: self.current_account,
                    ..AccountsWindow::default()
                });
            }
            Action::EditAccount(id) => self.open_editor(id),
            Action::TestConnection => self.test_connection(),
            Action::Refresh => {
                if self.current_bucket.is_some() {
                    self.reload_objects();
                } else {
                    self.load_buckets();
                }
            }
            Action::SelectBucket(name) => self.select_bucket(name),
            Action::AddBucketPrompt => {
                if self.current_account.is_some() {
                    self.modal = Some(Modal::Prompt {
                        title: "Add bucket".into(),
                        label: "Bucket name. Use this when the key can open a bucket but cannot list all buckets:".into(),
                        text: String::new(),
                        action: PromptAction::AddBucket,
                        focus: true,
                    });
                }
            }
            Action::RemoveBucket(name) => {
                let Some(account) = self.current() else {
                    return;
                };
                let account_name = account.name.clone();
                let manual = self.buckets.iter().any(|b| b.name == name && b.manual);
                if manual {
                    self.modal = Some(Modal::Confirm {
                        title: "Remove bucket from list".into(),
                        body: format!(
                            "Remove \"{name}\" from the list for \"{account_name}\"?\n\nOnly the entry in this app is removed. The bucket and its files on the server are not touched."
                        ),
                        ok: "Remove".into(),
                        alternative: None,
                        action: ConfirmAction::RemoveBucket { name },
                        danger: false,
                    });
                } else {
                    self.show_info(
                        "Remove bucket",
                        format!("\"{name}\" comes from the server's bucket list, so it can't be removed here. Only buckets you added by hand can be removed."),
                    );
                }
            }
            Action::CopyText(text) => ctx.copy_text(text),
            Action::Navigate(prefix) => self.navigate(prefix),
            Action::Up => {
                if !self.prefix.is_empty() {
                    self.navigate(browse::parent_prefix(&self.prefix));
                }
            }
            Action::Open(key) => {
                if key.ends_with('/') {
                    self.navigate(key);
                }
            }
            Action::ClickRow {
                view_index,
                ctrl,
                shift,
            } => self.click_row(view_index, ctrl, shift),
            Action::ContextRow(view_index) => {
                if let Some(&i) = self.view.get(view_index)
                    && !self.selection.contains(&self.objects[i].key)
                {
                    self.click_row(view_index, false, false);
                }
            }
            Action::SelectAll => {
                self.selection = self
                    .view
                    .iter()
                    .map(|&i| self.objects[i].key.clone())
                    .collect();
            }
            Action::UploadFiles => {
                if self.current_bucket.is_some()
                    && let Some(files) = rfd::FileDialog::new()
                        .set_title("Upload files")
                        .pick_files()
                {
                    self.uploads_from(files, false);
                }
            }
            Action::UploadFolder => {
                if self.current_bucket.is_some()
                    && let Some(folders) = rfd::FileDialog::new()
                        .set_title("Upload folder")
                        .pick_folders()
                {
                    self.uploads_from(folders, false);
                }
            }
            Action::UploadDropped(paths) => {
                if self.current_bucket.is_some() {
                    self.uploads_from(paths, true);
                } else {
                    self.show_info(
                        "Upload",
                        "Select a bucket first, then drop files onto the file list.",
                    );
                }
            }
            Action::Download => self.download_selection(),
            Action::NewFolderPrompt => {
                if self.current_bucket.is_some() {
                    self.modal = Some(Modal::Prompt {
                        title: "New folder".into(),
                        label: "Folder name:".into(),
                        text: String::new(),
                        action: PromptAction::NewFolder,
                        focus: true,
                    });
                }
            }
            Action::RenamePrompt => self.rename_prompt(),
            Action::DeleteSelected => self.delete_selection(),
            Action::LoadMore => self.load_objects(true),
            Action::SortBy(column) => {
                self.sort = if self.sort.0 == column {
                    (column, !self.sort.1)
                } else {
                    (column, true)
                };
            }
            Action::CancelJob(id) => self.queue.cancel(id),
            Action::PauseJob(id) => self.queue.pause(id),
            Action::PauseAll => self.queue.pause_all(),
            Action::ResumeAll => self.queue.resume_all(),
            Action::RetryJob(id) => self.queue.retry(id),
            Action::ClearFinished => {
                let n = self.queue.clear_finished();
                if n > 0 {
                    self.log(format!("Cleared {n} finished task(s)"), false);
                }
            }
            Action::DismissBanner => self.banner = None,
            Action::Options => {
                self.options = Some(OptionsState {
                    settings: self.settings.clone(),
                    proxy_password: String::new(),
                    error: String::new(),
                });
            }
            Action::About => self.show_about = true,
            Action::AccessKeys => self.open_access_keys(),
            Action::ToggleTheme => {
                let mut settings = self.settings.clone();
                settings.dark_mode = !settings.dark_mode;
                theme::apply(ctx, settings.dark_mode);
                if let Err(e) = self.persist_settings(settings) {
                    self.show_error("Theme", e);
                }
            }
            Action::CheckUpdates => self.check_for_updates(true),
            Action::ImportAccounts => self.import_accounts(),
            Action::ExportAccounts => self.export_accounts(),
            Action::OpenFolder(path) => {
                let _ = std::fs::create_dir_all(&path);
                if let Err(e) = open_in_file_manager(&path) {
                    self.show_error(APP_NAME, format!("Cannot open {}: {e}", path.display()));
                }
            }
            Action::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Copy => self.copy_to_clipboard(false),
            Action::Cut => self.copy_to_clipboard(true),
            Action::Paste => self.paste(),
            Action::CopyPath => self.copy_paths(ctx),
            Action::WebUrl => self.open_web_url(),
            Action::CreateBucketPrompt => self.create_bucket_prompt(),
            Action::DeleteBucket(name) => {
                self.modal = Some(Modal::DeleteBucket {
                    name,
                    contents: false,
                    typed: String::new(),
                });
            }
            Action::BucketProperties(name) => self.bucket_properties(name),
            Action::EditHeaders => self.edit_headers(),
            Action::BucketSettings(name) => self.open_bucket_settings(name),
            Action::DownloadVersion(key, version) => self.download_version(key, version),
            Action::RestoreVersion(key, version) => self.restore_version(key, version),
            Action::DeleteVersion(key, version, marker) => {
                self.confirm_delete_version(key, version, marker)
            }
            Action::SaveObjectAcl(key) => self.save_object_acl(key),
            Action::Sync => self.open_sync(),
            Action::MasterPassword => self.open_master_dialog(),
            Action::Lock => self.lock(),
            Action::Unlock => self.unlock = Some(crate::vault_ui::UnlockState::default()),
        }
    }

    /// Stops transfers before the window closes.
    pub(crate) fn shutdown(&self) {
        self.queue.shutdown_blocking(Duration::from_secs(3));
    }
}

fn open_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
}

/// Case-insensitive comparison that orders embedded numbers by value (`file2` < `file10`).
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut nx = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    nx.push(c);
                    ai.next();
                }
                let mut ny = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    ny.push(c);
                    bi.next();
                }
                let tx = nx.trim_start_matches('0');
                let ty = ny.trim_start_matches('0');
                let ord = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("File", "file"), Ordering::Less);
        assert_eq!(natural_cmp("a", "B"), Ordering::Less);
        assert_eq!(natural_cmp("x01", "x1"), Ordering::Less);
    }
}
