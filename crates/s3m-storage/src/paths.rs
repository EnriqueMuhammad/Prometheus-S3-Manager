//! Where the app keeps its data.

use std::path::{Path, PathBuf};

/// Data folder layout: `%APPDATA%\Prometheus-S3Manager` unless overridden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Root folder.
    pub root: PathBuf,
}

/// Environment variable that overrides the data folder (tests, portable runs).
pub const DATA_DIR_ENV: &str = "S3MANAGER_DATA_DIR";

impl AppPaths {
    /// Uses `root` as the data folder.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `override_root`, else `$S3MANAGER_DATA_DIR`, else the roaming app-data folder.
    pub fn resolve(override_root: Option<&Path>) -> Self {
        if let Some(root) = override_root {
            return Self::new(root);
        }
        if let Some(root) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
            return Self::new(root);
        }
        let base = directories::BaseDirs::new()
            .map_or_else(std::env::temp_dir, |d| d.config_dir().to_path_buf());
        Self::new(base.join("Prometheus-S3Manager"))
    }

    /// `accounts.json`.
    pub fn accounts(&self) -> PathBuf {
        self.root.join("accounts.json")
    }

    /// `settings.json`.
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// `transfers.db`.
    pub fn transfers(&self) -> PathBuf {
        self.root.join("transfers.db")
    }

    /// `logs` folder.
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
}
