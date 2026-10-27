//! `accounts.json` repository.

use std::path::{Path, PathBuf};

use s3m_core::{AccountError, AccountRepository, AccountsDocument};

/// `accounts.json` with replace-on-write. Contains no secrets by construction.
#[derive(Debug, Clone)]
pub struct JsonAccountRepository {
    path: PathBuf,
}

impl JsonAccountRepository {
    /// Repository at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

fn read_document(path: &Path) -> Result<AccountsDocument, AccountError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| AccountError::Storage(format!("{} is unreadable: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountsDocument::default()),
        Err(e) => Err(AccountError::Storage(format!(
            "Cannot read {}: {e}",
            path.display()
        ))),
    }
}

impl AccountRepository for JsonAccountRepository {
    fn load(&self) -> Result<AccountsDocument, AccountError> {
        read_document(&self.path)
    }

    fn save(&self, document: &AccountsDocument) -> Result<(), AccountError> {
        let json = serde_json::to_vec_pretty(document)
            .map_err(|e| AccountError::Storage(e.to_string()))?;
        crate::atomic_write(&self.path, &json)
            .map_err(|e| AccountError::Storage(format!("Cannot save {}: {e}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use s3m_core::{AccountDraft, AccountService, AccountType};
    use std::sync::Arc;

    #[test]
    fn round_trip_without_secrets() {
        let dir = std::env::temp_dir().join(format!("s3m-acc-{}", uuid::Uuid::new_v4()));
        let path = dir.join("accounts.json");
        let secrets = Arc::new(s3m_core::account::MemorySecrets::default());
        let svc = AccountService::new(Box::new(JsonAccountRepository::new(&path)), secrets);
        let saved = svc
            .save(&AccountDraft {
                name: "Local MinIO".into(),
                kind: AccountType::S3Compatible,
                endpoint_host: "http://127.0.0.1:9000".into(),
                region: "us-east-1".into(),
                access_key_id: "minio".into(),
                secret_access_key: Some("minio-secret".into()),
                force_path_style: true,
                ..AccountDraft::default()
            })
            .expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"endpointHost\": \"127.0.0.1:9000\""));
        assert!(text.contains("\"type\": \"S3Compatible\""));
        assert!(!text.contains("minio-secret"));
        let doc = JsonAccountRepository::new(&path).load().expect("load");
        assert_eq!(doc.accounts[0], saved);
        assert_eq!(doc.active_account_id, Some(saved.id));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn missing_file_is_empty_and_garbage_is_an_error() {
        let dir = std::env::temp_dir().join(format!("s3m-acc-{}", uuid::Uuid::new_v4()));
        let repo = JsonAccountRepository::new(dir.join("accounts.json"));
        assert!(repo.load().expect("empty").accounts.is_empty());
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("accounts.json"), "{oops").expect("write");
        assert!(repo.load().is_err());
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
