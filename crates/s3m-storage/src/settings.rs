//! User preferences (`settings.json`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Preferences edited in Tools → Options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Transfers running at once (1–16).
    pub transfer_concurrency: usize,
    /// Attempts per transfer (1–10).
    pub transfer_max_attempts: u32,
    /// Dark theme instead of the light, classic one.
    pub dark_mode: bool,
    /// Ask before uploading more than this many files at once (0 = never ask).
    pub confirm_upload_over: usize,
    /// Bandwidth limit for all transfers together, in KB/s (0 = unlimited).
    pub bandwidth_limit_kbps: u64,
    /// `none`, `system` or `manual`.
    pub proxy_mode: String,
    /// Manual proxy URL.
    pub proxy_url: String,
    /// Manual proxy user (the password is in the keychain).
    pub proxy_user: String,
    /// Hosts that bypass the manual proxy.
    pub proxy_no_proxy: String,
    /// Master password check data (None = no master password).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_password: Option<s3m_core::vault::MasterConfig>,
    /// Look for a new version when the app starts (at most every 20 hours).
    pub check_updates: bool,
    /// Last successful update check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update_check: Option<chrono::DateTime<chrono::Utc>>,
    /// Version the user chose to skip (empty = none).
    pub skipped_version: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            transfer_concurrency: 3,
            transfer_max_attempts: 4,
            dark_mode: false,
            confirm_upload_over: 50,
            bandwidth_limit_kbps: 0,
            proxy_mode: "none".into(),
            proxy_url: String::new(),
            proxy_user: String::new(),
            proxy_no_proxy: String::new(),
            master_password: None,
            check_updates: true,
            last_update_check: None,
            skipped_version: String::new(),
        }
    }
}

impl Settings {
    /// Clamps values into their valid ranges.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.transfer_concurrency = self.transfer_concurrency.clamp(1, 16);
        self.transfer_max_attempts = self.transfer_max_attempts.clamp(1, 10);
        self
    }
}

/// Reads and writes `settings.json`.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Store at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Loads settings; a missing or unreadable file yields defaults.
    pub fn load(&self) -> Settings {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .unwrap_or_default()
            .sanitized()
    }

    /// Saves settings.
    pub fn save(&self, settings: &Settings) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
        crate::atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_clamp() {
        let path = std::env::temp_dir().join(format!("s3m-set-{}.json", uuid::Uuid::new_v4()));
        let store = SettingsStore::new(&path);
        assert_eq!(store.load(), Settings::default());
        store
            .save(&Settings {
                transfer_concurrency: 99,
                dark_mode: true,
                ..Settings::default()
            })
            .expect("save");
        let s = store.load();
        assert_eq!(s.transfer_concurrency, 16);
        assert!(s.dark_mode);
        std::fs::remove_file(path).expect("cleanup");
    }
}
