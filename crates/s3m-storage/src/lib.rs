//! Local persistence for Prometheus S3Manager.
//!
//! - `accounts.json`: account settings, replaced atomically, never secrets
//! - OS keychain (Windows Credential Manager): secret access keys
//! - `transfers.db` (SQLite): durable transfer records
//! - `settings.json`: preferences
//! - `logs/app-YYYYMMDD.log`: daily log with secrets redacted

mod accounts;
mod logging;
mod paths;
mod secrets;
mod settings;
mod transfers;

pub use accounts::JsonAccountRepository;
pub use logging::{LogWriter, redact};
pub use paths::AppPaths;
pub use secrets::{KEYRING_SERVICE, KeyringSecrets, PROXY_SECRET_ID};
pub use settings::{Settings, SettingsStore};
pub use transfers::SqliteTransferStore;

/// Writes `contents` to `path` via a temporary file and a rename, so readers never see a
/// half-written file.
pub(crate) fn atomic_write(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    std::fs::write(&temp, contents)?;
    std::fs::rename(&temp, path)
}
