//! UI state types shared by the panels and dialogs.

use chrono::{DateTime, Local};
use std::path::PathBuf;

use s3m_core::export::ImportPreview;
use s3m_core::plan::{Clipboard, ListedObject};
use s3m_core::{
    AccountDraft, BucketInfo, BucketListing, ConnectionTestResult, HeaderEdit, ObjectDetails,
    ObjectPage, TransferRequest,
};
use s3m_s3::{PreviewData, StorageError};
use uuid::Uuid;

/// Results delivered from background tasks to the UI thread.
pub enum Msg {
    /// Bucket listing finished.
    Buckets {
        /// Listing generation; stale results are dropped.
        version: u64,
        /// Result.
        result: Result<BucketListing, StorageError>,
    },
    /// Object listing page finished.
    Objects {
        /// Listing generation.
        version: u64,
        /// Appending a "next page".
        more: bool,
        /// Result.
        result: Result<ObjectPage, StorageError>,
    },
    /// HEAD for the Properties tab finished.
    Details {
        /// Key the details belong to.
        key: String,
        /// Result.
        result: Result<ObjectDetails, StorageError>,
    },
    /// A one-shot operation (new folder, rename, delete) finished.
    Done {
        /// Title used when reporting an error.
        title: String,
        /// Log line on success, error otherwise.
        result: Result<String, StorageError>,
        /// What to reload afterwards.
        reload: Reload,
    },
    /// Selected folders were expanded into their objects.
    Expanded {
        /// What the expansion is for.
        purpose: Expand,
        /// Objects found (selected files included as they are).
        result: Result<Vec<ListedObject>, StorageError>,
    },
    /// Bucket facts for the Properties tab.
    BucketInfo {
        /// Result.
        result: Result<BucketInfo, StorageError>,
    },
    /// A presigned URL for the Web URL dialog.
    Presigned {
        /// Dialog nonce.
        nonce: u64,
        /// Result.
        result: Result<String, StorageError>,
    },
    /// Versions of the focused object.
    Versions {
        /// Key.
        key: String,
        /// Result.
        result: Result<Vec<s3m_core::ObjectVersion>, StorageError>,
    },
    /// ACL of the focused object.
    Acl {
        /// Key.
        key: String,
        /// Result.
        result: Result<s3m_core::admin::Acl, StorageError>,
    },
    /// Everything for the Bucket settings window.
    BucketSettings {
        /// Bucket.
        bucket: String,
        /// Loaded settings.
        data: Box<crate::admin_ui::SettingsData>,
    },
    /// Result of an access key request.
    Keys(crate::access_keys_ui::KeysMsg),
    /// A bucket setting was saved (or failed).
    SettingsSaved {
        /// Bucket.
        bucket: String,
        /// Message.
        result: Result<String, String>,
        /// Reload the window afterwards.
        reload: bool,
    },
    /// Result of a folder sync comparison.
    SyncPlan {
        /// Dialog nonce.
        nonce: u64,
        /// Plan or error.
        result: Result<s3m_core::sync::SyncPlan, String>,
    },
    /// First bytes of an object for the Preview tab.
    Preview {
        /// Key previewed.
        key: String,
        /// Result.
        result: Result<PreviewData, StorageError>,
    },
    /// Update check finished.
    UpdateChecked {
        /// Started from Help > Check for updates.
        manual: bool,
        /// Latest release or error.
        result: Result<s3m_core::update::Release, String>,
    },
    /// Installer download progress.
    UpdateProgress {
        /// Bytes downloaded.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// Installer downloaded and verified (path, signature note) or failed.
    UpdateDownloaded {
        /// Result.
        result: Result<(PathBuf, String), String>,
    },
    /// "Test connection" finished.
    Test {
        /// Where the result goes.
        target: TestTarget,
        /// Result.
        result: ConnectionTestResult,
    },
}

/// What to reload after an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// The current folder.
    Objects,
    /// The bucket list.
    Buckets,
}

/// Why selected folders are being expanded.
#[derive(Debug, Clone)]
pub enum Expand {
    /// Download into a local folder.
    Download {
        /// Local destination.
        dest: PathBuf,
        /// Folder shown when the download started (stripped from keys).
        strip: String,
    },
    /// Delete everything (asks first).
    Delete,
    /// Paste the clipboard into a folder.
    Paste {
        /// What was copied or cut.
        clipboard: Clipboard,
        /// Destination bucket.
        bucket: String,
        /// Destination folder.
        prefix: String,
    },
    /// Rename a folder (moves every object).
    RenameFolder {
        /// Old folder key (ends with `/`).
        old: String,
        /// New folder key (ends with `/`).
        new: String,
    },
}

/// Empty source folders to remove once a folder move has finished.
pub struct FolderCleanup {
    /// Account of the folders.
    pub account_id: uuid::Uuid,
    /// Bucket of the folders.
    pub bucket: String,
    /// Folder prefixes, deepest first.
    pub folders: Vec<String>,
    /// Move jobs still running.
    pub remaining: std::collections::HashSet<uuid::Uuid>,
    /// A move did not complete: keep the folders.
    pub failed: bool,
}

/// Who asked for a connection test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestTarget {
    /// Toolbar / Accounts menu: show a message box.
    Toolbar,
    /// Account editor with this nonce (ignored if the editor was reopened).
    Editor(u64),
}

/// Columns of the file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// Name.
    Name,
    /// Size.
    Size,
    /// Type.
    Type,
    /// Modified.
    Modified,
    /// Storage class.
    Class,
}

/// Tabs of the bottom panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    /// Transfer queue.
    Tasks,
    /// Object properties.
    Properties,
    /// HTTP headers and user metadata.
    Headers,
    /// Text or image preview.
    Preview,
    /// Object versions.
    Versions,
    /// Object ACL.
    Permissions,
    /// Activity log.
    EventLog,
}

/// What the Preview tab shows.
pub enum PreviewState {
    /// Loading.
    Loading,
    /// Text (possibly truncated).
    Text {
        /// Decoded text.
        text: String,
        /// Only the start of the object was loaded.
        truncated: bool,
    },
    /// Image bytes, shown through egui's image loaders.
    Image {
        /// `bytes://` URI used as the texture key.
        uri: String,
        /// Encoded image.
        bytes: std::sync::Arc<[u8]>,
    },
    /// Nothing to show (binary, too large, folder); the reason.
    Unavailable(String),
}

/// InfoBar-style message above the file list.
#[derive(Debug, Clone)]
pub struct Banner {
    /// Title.
    pub title: String,
    /// Detail.
    pub detail: String,
    /// Offer "Remove from list" (registered bucket that failed).
    pub offers_remove_bucket: bool,
    /// Offer "Edit account".
    pub offers_edit_account: bool,
}

/// One line of the event log.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// When.
    pub time: DateTime<Local>,
    /// What.
    pub message: String,
    /// Shown in red.
    pub is_error: bool,
}

/// What a text prompt does on OK.
#[derive(Debug, Clone)]
pub enum PromptAction {
    /// Create a folder in the current prefix.
    NewFolder,
    /// Rename the object with this key.
    Rename {
        /// Source key.
        key: String,
    },
    /// Rename the folder with this key (ends with `/`).
    RenameFolder {
        /// Folder key.
        key: String,
    },
    /// Register a bucket on the current account.
    AddBucket,
}

/// What a confirmation does on OK.
pub enum ConfirmAction {
    /// Delete these objects.
    Delete {
        /// Keys.
        keys: Vec<String>,
    },
    /// Remove a registered bucket from the account.
    RemoveBucket {
        /// Bucket name.
        name: String,
    },
    /// Enqueue these transfers.
    Enqueue {
        /// Requests.
        requests: Vec<TransferRequest>,
    },
    /// Delete these objects (expanded from folders).
    DeleteMany {
        /// Keys.
        keys: Vec<String>,
    },
    /// Delete an account.
    DeleteAccount {
        /// Account id.
        id: Uuid,
    },
    /// Save imported accounts.
    Import {
        /// Parsed file.
        preview: ImportPreview,
        /// Note appended to the result.
        note: String,
    },
    /// Export: include secrets (asks for a password) or settings only.
    ExportSecrets,
    /// Delete one object version.
    DeleteVersion {
        /// Key.
        key: String,
        /// Version id.
        version: String,
    },
}

/// What a password prompt does on OK.
pub enum PasswordAction {
    /// Export with secrets encrypted by this password.
    Export,
    /// Decrypt an import file.
    Import {
        /// File contents.
        text: String,
    },
}

/// The dialog on top of everything else.
pub enum Modal {
    /// Single-line text input.
    Prompt {
        /// Title.
        title: String,
        /// Label above the input.
        label: String,
        /// Current text.
        text: String,
        /// Action on OK.
        action: PromptAction,
        /// Focus the input on the first frame.
        focus: bool,
    },
    /// Yes/No question.
    Confirm {
        /// Title.
        title: String,
        /// Body.
        body: String,
        /// OK button label.
        ok: String,
        /// Secondary button (e.g. "Settings only"); `None` for a plain OK/Cancel.
        alternative: Option<String>,
        /// Action on OK (or on the alternative).
        action: ConfirmAction,
        /// Destructive: OK is shown in red.
        danger: bool,
    },
    /// Information or error.
    Message {
        /// Title.
        title: String,
        /// Body.
        body: String,
        /// Error styling.
        error: bool,
    },
    /// Create a bucket.
    CreateBucket {
        /// Name.
        name: String,
        /// Region (Amazon S3 only).
        region: String,
        /// Validation error.
        error: String,
    },
    /// Delete a bucket.
    DeleteBucket {
        /// Bucket name.
        name: String,
        /// Also delete every object in it.
        contents: bool,
        /// Name typed to confirm deleting contents.
        typed: String,
    },
    /// Generate a presigned Web URL.
    WebUrl {
        /// Bucket.
        bucket: String,
        /// Key.
        key: String,
        /// Lifetime in minutes.
        minutes: u32,
        /// Generated URL.
        url: String,
        /// Error.
        error: String,
        /// Distinguishes responses for earlier requests.
        nonce: u64,
        /// Request in flight.
        busy: bool,
    },
    /// Edit HTTP headers and user metadata.
    Headers {
        /// Bucket.
        bucket: String,
        /// Key.
        key: String,
        /// Values being edited.
        edit: HeaderEdit,
        /// Validation error.
        error: String,
    },
    /// Password input (optionally with confirmation).
    Password {
        /// Title.
        title: String,
        /// Explanation.
        body: String,
        /// Ask twice.
        confirm: bool,
        /// First input.
        first: String,
        /// Second input.
        second: String,
        /// Validation message.
        error: String,
        /// Action on OK.
        action: PasswordAction,
    },
}

/// Account editor state.
pub struct EditorState {
    /// Form contents.
    pub draft: AccountDraft,
    /// Secret typed in the password box.
    pub secret: String,
    /// Registered buckets, one per line.
    pub buckets_text: String,
    /// Validation error.
    pub error: String,
    /// Last test result.
    pub test_result: Option<ConnectionTestResult>,
    /// Test in flight.
    pub testing: bool,
    /// Distinguishes test results of earlier editor sessions.
    pub nonce: u64,
    /// The account already has a stored secret.
    pub has_secret: bool,
    /// Show advanced settings.
    pub advanced: bool,
}

/// Row of the Storage Accounts window.
#[derive(Debug, Clone)]
pub struct AccountRow {
    /// Account id.
    pub id: Uuid,
    /// Name.
    pub name: String,
    /// Type label.
    pub type_label: &'static str,
    /// `https://host`.
    pub endpoint: String,
    /// Has a stored secret.
    pub has_secret: bool,
    /// Folder.
    pub folder: String,
}

/// Storage Accounts window state.
#[derive(Default)]
pub struct AccountsWindow {
    /// Rows (refreshed when accounts change).
    pub rows: Vec<AccountRow>,
    /// Revision the rows were built from.
    pub revision: u64,
    /// Selected row.
    pub selected: Option<Uuid>,
    /// Result of the last import/export.
    pub notice: String,
}

/// Tools → Options dialog state.
pub struct OptionsState {
    /// Settings being edited.
    pub settings: s3m_storage::Settings,
    /// New proxy password (empty = keep the stored one).
    pub proxy_password: String,
    /// Validation error.
    pub error: String,
}
