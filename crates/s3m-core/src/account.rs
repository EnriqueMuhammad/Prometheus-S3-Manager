//! Storage accounts: model, validation, CRUD, ordering and the active-account pointer.
//!
//! Accounts never carry the secret access key: secrets live only in a [`SecretStore`].
//! `accounts.json` uses camelCase and enum names as strings; unknown fields are ignored, so files
//! with extra fields still load.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{bucket, ca, endpoint};

/// Kind of storage behind an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AccountType {
    /// Amazon Web Services S3.
    #[default]
    AmazonS3,
    /// Any S3-compatible service (MinIO, Ceph, Wasabi, R2, company gateways).
    S3Compatible,
}

impl AccountType {
    /// Label shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::AmazonS3 => "Amazon S3 Storage",
            Self::S3Compatible => "S3 Compatible Storage",
        }
    }
}

/// A storage account as persisted in `accounts.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Stable identifier (also the key of the stored secret).
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// Kind of storage.
    #[serde(rename = "type")]
    pub kind: AccountType,
    /// Host with optional port, e.g. `store.example.net` or `10.0.0.5:9000`.
    pub endpoint_host: String,
    /// "Use secure transfer (SSL/TLS)". `false` is the explicit opt-in for plain HTTP.
    pub use_tls: bool,
    /// Signing region.
    pub region: String,
    /// Access key id (public half of the credentials).
    pub access_key_id: String,
    /// Path-style addressing (`host/bucket/key`), needed by most S3-compatible servers.
    pub force_path_style: bool,
    /// Extra trusted CA certificates (PEM). Public data, not a secret.
    #[serde(default)]
    pub custom_ca_pem: String,
    /// Buckets registered by hand for keys that may not call ListBuckets.
    #[serde(default)]
    pub manual_buckets: Vec<String>,
    /// Free-form notes.
    #[serde(default)]
    pub notes: String,
    /// Folder in the account list (`""` = top level, `/` separates levels).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Creation time.
    #[serde(default = "epoch")]
    pub created_at: DateTime<Utc>,
    /// Last change.
    #[serde(default = "epoch")]
    pub updated_at: DateTime<Utc>,
}

fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

impl Account {
    /// `https://host` or `http://host`.
    pub fn endpoint_url(&self) -> String {
        format!(
            "{}://{}",
            if self.use_tls { "https" } else { "http" },
            self.endpoint_host
        )
    }
}

/// What the account editor submits. `secret_access_key: None` keeps the stored key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDraft {
    /// `None` creates a new account.
    pub id: Option<Uuid>,
    /// Display name.
    pub name: String,
    /// Kind of storage.
    pub kind: AccountType,
    /// Endpoint as typed (scheme allowed).
    pub endpoint_host: String,
    /// Use SSL/TLS.
    pub use_tls: bool,
    /// Signing region.
    pub region: String,
    /// Access key id.
    pub access_key_id: String,
    /// New secret, or `None` to keep the stored one.
    pub secret_access_key: Option<String>,
    /// Path-style addressing.
    pub force_path_style: bool,
    /// Custom CA bundle.
    pub custom_ca_pem: String,
    /// Registered buckets.
    pub manual_buckets: Vec<String>,
    /// Notes.
    pub notes: String,
    /// Folder in the account list.
    pub folder: String,
}

impl AccountDraft {
    /// Draft pre-filled from an existing account (secret left empty).
    pub fn from_account(a: &Account) -> Self {
        Self {
            id: Some(a.id),
            name: a.name.clone(),
            kind: a.kind,
            endpoint_host: a.endpoint_host.clone(),
            use_tls: a.use_tls,
            region: a.region.clone(),
            access_key_id: a.access_key_id.clone(),
            secret_access_key: None,
            force_path_style: a.force_path_style,
            custom_ca_pem: a.custom_ca_pem.clone(),
            manual_buckets: a.manual_buckets.clone(),
            notes: a.notes.clone(),
            folder: a.folder.clone(),
        }
    }
}

/// `accounts.json` contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsDocument {
    /// Format version.
    #[serde(default = "one")]
    pub version: u32,
    /// Account selected in the toolbar.
    #[serde(default)]
    pub active_account_id: Option<Uuid>,
    /// Accounts in display order.
    #[serde(default)]
    pub accounts: Vec<Account>,
}

fn one() -> u32 {
    1
}

impl Default for AccountsDocument {
    fn default() -> Self {
        Self {
            version: 1,
            active_account_id: None,
            accounts: Vec::new(),
        }
    }
}

/// Errors from the account service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    /// Invalid input; the message is user-facing.
    #[error("{0}")]
    Validation(String),
    /// Reading or writing local storage failed.
    #[error("{0}")]
    Storage(String),
}

/// Failure of the secret store. Never contains the secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("credential store: {0}")]
pub struct SecretError(pub String);

/// Persists account settings (never secrets).
pub trait AccountRepository: Send + Sync {
    /// Loads the document (empty when there is none yet).
    fn load(&self) -> Result<AccountsDocument, AccountError>;
    /// Replaces the stored document.
    fn save(&self, document: &AccountsDocument) -> Result<(), AccountError>;
}

/// The only place plaintext secret access keys are handled. Implementations must encrypt at rest
/// and must never fall back to plaintext.
pub trait SecretStore: Send + Sync {
    /// Reads the secret of an account.
    fn get(&self, account_id: Uuid) -> Result<Option<String>, SecretError>;
    /// Stores the secret of an account.
    fn set(&self, account_id: Uuid, secret: &str) -> Result<(), SecretError>;
    /// Removes the secret of an account (no error when absent).
    fn delete(&self, account_id: Uuid) -> Result<(), SecretError>;
    /// Whether a secret is stored.
    fn has(&self, account_id: Uuid) -> bool {
        matches!(self.get(account_id), Ok(Some(_)))
    }
}

/// In-memory secret store for tests and demo runs.
#[derive(Debug, Default)]
pub struct MemorySecrets(Mutex<std::collections::HashMap<Uuid, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, id: Uuid) -> Result<Option<String>, SecretError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned())
    }
    fn set(&self, id: Uuid, secret: &str) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, secret.to_owned());
        Ok(())
    }
    fn delete(&self, id: Uuid) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        Ok(())
    }
}

/// In-memory repository for tests and demo runs.
#[derive(Debug, Default)]
pub struct MemoryRepository(Mutex<AccountsDocument>);

impl AccountRepository for MemoryRepository {
    fn load(&self) -> Result<AccountsDocument, AccountError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }
    fn save(&self, document: &AccountsDocument) -> Result<(), AccountError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = document.clone();
        Ok(())
    }
}

/// Account CRUD, ordering and the active-account pointer. Unlimited accounts.
pub struct AccountService {
    repository: Box<dyn AccountRepository>,
    secrets: Arc<dyn SecretStore>,
    document: Mutex<Option<AccountsDocument>>,
    revision: AtomicU64,
}

impl AccountService {
    /// Creates the service; the document is loaded lazily on first use.
    pub fn new(repository: Box<dyn AccountRepository>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            repository,
            secrets,
            document: Mutex::new(None),
            revision: AtomicU64::new(0),
        }
    }

    /// Increments on every change; the UI compares it to know when to reload.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// The secret store (for clients that build S3 credentials).
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    fn doc(&self) -> Result<MutexGuard<'_, Option<AccountsDocument>>, AccountError> {
        let mut guard = self.document.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(self.repository.load()?);
        }
        Ok(guard)
    }

    fn with_doc<T>(&self, f: impl FnOnce(&AccountsDocument) -> T) -> Result<T, AccountError> {
        let guard = self.doc()?;
        Ok(f(guard.as_ref().unwrap_or(&AccountsDocument::default())))
    }

    /// Applies a change, persists it, and bumps the revision.
    fn update<T>(
        &self,
        f: impl FnOnce(&mut AccountsDocument) -> Result<T, AccountError>,
    ) -> Result<T, AccountError> {
        let mut guard = self.doc()?;
        let mut next = guard.clone().unwrap_or_default();
        let out = f(&mut next)?;
        self.repository.save(&next)?;
        *guard = Some(next);
        self.revision.fetch_add(1, Ordering::AcqRel);
        Ok(out)
    }

    /// All accounts in display order.
    pub fn list(&self) -> Result<Vec<Account>, AccountError> {
        self.with_doc(|d| d.accounts.clone())
    }

    /// One account by id.
    pub fn get(&self, id: Uuid) -> Option<Account> {
        self.with_doc(|d| d.accounts.iter().find(|a| a.id == id).cloned())
            .ok()
            .flatten()
    }

    /// Whether a secret is stored for the account.
    pub fn has_secret(&self, id: Uuid) -> bool {
        self.secrets.has(id)
    }

    /// Plaintext secret for export or a connection test. Callers must not log or persist it.
    pub fn secret(&self, id: Uuid) -> Result<Option<String>, SecretError> {
        self.secrets.get(id)
    }

    /// The active account, falling back to the first one.
    pub fn active_account_id(&self) -> Option<Uuid> {
        self.with_doc(|d| match d.active_account_id {
            Some(id) if d.accounts.iter().any(|a| a.id == id) => Some(id),
            _ => d.accounts.first().map(|a| a.id),
        })
        .ok()
        .flatten()
    }

    /// Selects the active account.
    pub fn set_active(&self, id: Uuid) -> Result<(), AccountError> {
        self.update(|d| {
            if !d.accounts.iter().any(|a| a.id == id) {
                return Err(unknown());
            }
            d.active_account_id = Some(id);
            Ok(())
        })
    }

    /// Creates (`id: None`) or updates an account. A `None` secret on update keeps the stored one.
    pub fn save(&self, draft: &AccountDraft) -> Result<Account, AccountError> {
        let normalized = validate(draft)?;
        let secret = draft.secret_access_key.as_deref().filter(|s| !s.is_empty());
        self.update(|d| {
            let now = Utc::now();
            let existing = match draft.id {
                Some(id) => Some(d.accounts.iter().find(|a| a.id == id).ok_or_else(unknown)?),
                None => None,
            };
            let saved = Account {
                id: existing.map_or_else(Uuid::new_v4, |e| e.id),
                created_at: existing.map_or(now, |e| e.created_at),
                updated_at: now,
                ..normalized
            };
            if let Some(secret) = secret {
                self.secrets
                    .set(saved.id, secret)
                    .map_err(|e| AccountError::Storage(e.to_string()))?;
            }
            match d.accounts.iter_mut().find(|a| a.id == saved.id) {
                Some(slot) => *slot = saved.clone(),
                None => d.accounts.push(saved.clone()),
            }
            d.active_account_id = d.active_account_id.or(Some(saved.id));
            Ok(saved)
        })
    }

    /// Deletes an account and its stored secret.
    pub fn remove(&self, id: Uuid) -> Result<(), AccountError> {
        self.update(|d| {
            let index = d
                .accounts
                .iter()
                .position(|a| a.id == id)
                .ok_or_else(unknown)?;
            d.accounts.remove(index);
            if d.active_account_id == Some(id) {
                d.active_account_id = d.accounts.first().map(|a| a.id);
            }
            Ok(())
        })?;
        self.secrets
            .delete(id)
            .map_err(|e| AccountError::Storage(e.to_string()))
    }

    /// Moves one position up (`-1`) or down (`+1`). Moves past either end are ignored.
    pub fn move_by(&self, id: Uuid, direction: i32) -> Result<(), AccountError> {
        self.update(|d| {
            let index = d
                .accounts
                .iter()
                .position(|a| a.id == id)
                .ok_or_else(unknown)?;
            let target = i64::try_from(index).unwrap_or(0) + i64::from(direction.signum());
            if let Ok(target) = usize::try_from(target)
                && target < d.accounts.len()
            {
                d.accounts.swap(index, target);
            }
            Ok(())
        })
    }

    /// Registers a bucket by name for an account.
    pub fn add_manual_bucket(&self, id: Uuid, name: &str) -> Result<(), AccountError> {
        let name = bucket::normalize(name);
        if !bucket::is_valid(&name) {
            return Err(AccountError::Validation(format!(
                "\"{name}\" is not a valid bucket name."
            )));
        }
        self.update(|d| {
            let account = d
                .accounts
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(unknown)?;
            if !account.manual_buckets.contains(&name) {
                account.manual_buckets.push(name);
                account.updated_at = Utc::now();
            }
            Ok(())
        })
    }

    /// Removes a hand-registered bucket from the account (the bucket on the server is untouched).
    pub fn remove_manual_bucket(&self, id: Uuid, name: &str) -> Result<(), AccountError> {
        let name = bucket::normalize(name);
        self.update(|d| {
            let account = d
                .accounts
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(unknown)?;
            if let Some(pos) = account.manual_buckets.iter().position(|b| *b == name) {
                account.manual_buckets.remove(pos);
                account.updated_at = Utc::now();
            }
            Ok(())
        })
    }
}

/// `" Office / Production "` → `"Office/Production"`.
pub fn normalize_folder(folder: &str) -> String {
    folder
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn unknown() -> AccountError {
    AccountError::Validation("Unknown account.".into())
}

fn invalid(message: impl Into<String>) -> AccountError {
    AccountError::Validation(message.into())
}

fn printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// Validates a draft and returns the normalised account (id and timestamps filled by the caller).
pub fn validate(draft: &AccountDraft) -> Result<Account, AccountError> {
    let name = draft.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(invalid("Display name is required (max 120 characters)."));
    }

    let fields = endpoint::normalize(&draft.endpoint_host, draft.use_tls);
    endpoint::validate(&fields.host, fields.use_tls).map_err(invalid)?;

    let region = draft.region.trim();
    if region.is_empty()
        || region.len() > 80
        || !region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid("Region contains invalid characters."));
    }

    let access_key = draft.access_key_id.trim();
    if access_key.is_empty() || access_key.len() > 200 || !printable_ascii(access_key) {
        return Err(invalid("Access Key ID must be printable ASCII."));
    }
    if let Some(secret) = draft.secret_access_key.as_deref().filter(|s| !s.is_empty())
        && (secret.len() > 400 || !printable_ascii(secret))
    {
        return Err(invalid("Secret Access Key must be printable ASCII."));
    }

    let pem = ca::normalize(&draft.custom_ca_pem).map_err(invalid)?;

    let mut buckets: Vec<String> = Vec::new();
    for b in draft.manual_buckets.iter().map(|b| bucket::normalize(b)) {
        if !b.is_empty() && !buckets.contains(&b) {
            buckets.push(b);
        }
    }
    if buckets.len() > 200 {
        return Err(invalid("At most 200 registered buckets per account."));
    }
    if let Some(bad) = buckets.iter().find(|b| !bucket::is_valid(b)) {
        return Err(invalid(format!("\"{bad}\" is not a valid bucket name.")));
    }
    if draft.notes.chars().count() > 2000 {
        return Err(invalid("Notes are limited to 2000 characters."));
    }
    let folder = normalize_folder(&draft.folder);
    if folder.chars().count() > 120 || folder.chars().any(char::is_control) {
        return Err(invalid(
            "Folder names are limited to 120 printable characters.",
        ));
    }

    Ok(Account {
        id: draft.id.unwrap_or(Uuid::nil()),
        name: name.to_owned(),
        kind: draft.kind,
        endpoint_host: fields.host,
        use_tls: fields.use_tls,
        region: region.to_owned(),
        access_key_id: access_key.to_owned(),
        force_path_style: draft.force_path_style,
        custom_ca_pem: pem,
        manual_buckets: buckets,
        notes: draft.notes.clone(),
        folder,
        created_at: epoch(),
        updated_at: epoch(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn draft(name: &str) -> AccountDraft {
        AccountDraft {
            name: name.into(),
            kind: AccountType::S3Compatible,
            endpoint_host: "https://store.example.net/".into(),
            use_tls: false,
            region: "us-east-1".into(),
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: Some("secret-1".into()),
            force_path_style: true,
            manual_buckets: vec![" PO-Archive ".into(), "po-archive".into()],
            ..AccountDraft::default()
        }
    }

    fn service() -> (AccountService, Arc<MemorySecrets>) {
        let secrets = Arc::new(MemorySecrets::default());
        (
            AccountService::new(Box::new(MemoryRepository::default()), secrets.clone()),
            secrets,
        )
    }

    #[test]
    fn save_normalizes_and_stores_secret_separately() {
        let (svc, secrets) = service();
        let a = svc.save(&draft("Company S3")).expect("save");
        assert_eq!(a.endpoint_host, "store.example.net");
        assert!(a.use_tls, "typed https:// wins over the checkbox");
        assert_eq!(a.manual_buckets, vec!["po-archive".to_owned()]);
        assert_eq!(secrets.get(a.id).expect("get").as_deref(), Some("secret-1"));
        assert_eq!(svc.active_account_id(), Some(a.id));
        let json = serde_json::to_string(&svc.list().expect("list")).expect("json");
        assert!(!json.contains("secret-1"));
    }

    #[test]
    fn update_keeps_secret_when_blank() {
        let (svc, secrets) = service();
        let a = svc.save(&draft("A")).expect("save");
        let mut d = AccountDraft::from_account(&a);
        d.name = "Renamed".into();
        let b = svc.save(&d).expect("update");
        assert_eq!(b.id, a.id);
        assert_eq!(b.created_at, a.created_at);
        assert_eq!(secrets.get(a.id).expect("get").as_deref(), Some("secret-1"));
    }

    #[test]
    fn move_remove_and_active_fallback() {
        let (svc, secrets) = service();
        let a = svc.save(&draft("A")).expect("a");
        let b = svc.save(&draft("B")).expect("b");
        svc.move_by(b.id, -1).expect("move");
        svc.move_by(b.id, -1).expect("move past start is ignored");
        let names: Vec<_> = svc
            .list()
            .expect("list")
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["B", "A"]);
        svc.remove(a.id).expect("remove");
        assert_eq!(svc.active_account_id(), Some(b.id));
        assert_eq!(secrets.get(a.id).expect("get"), None);
        let rev = svc.revision();
        svc.set_active(b.id).expect("active");
        assert!(svc.revision() > rev);
    }

    #[test]
    fn manual_buckets() {
        let (svc, _) = service();
        let a = svc.save(&draft("A")).expect("a");
        assert!(svc.add_manual_bucket(a.id, "Bad_Name").is_err());
        svc.add_manual_bucket(a.id, " Logs-2026 ").expect("add");
        svc.remove_manual_bucket(a.id, "po-archive")
            .expect("remove");
        assert_eq!(
            svc.get(a.id).expect("get").manual_buckets,
            vec!["logs-2026".to_owned()]
        );
    }

    #[test]
    fn validation_messages() {
        let mut d = draft("A");
        d.name = " ".into();
        assert!(
            validate(&d)
                .unwrap_err()
                .to_string()
                .contains("Display name")
        );
        let mut d = draft("A");
        d.region = "us east".into();
        assert!(validate(&d).unwrap_err().to_string().contains("Region"));
        let mut d = draft("A");
        d.secret_access_key = Some("é".into());
        assert!(validate(&d).unwrap_err().to_string().contains("Secret"));
        let mut d = draft("A");
        d.endpoint_host = "http://8.8.8.8".into();
        assert!(
            validate(&d)
                .unwrap_err()
                .to_string()
                .contains("private-network")
        );
    }

    #[test]
    fn reads_accounts_json_with_extra_fields() {
        let json = r#"{
          "version": 1,
          "activeAccountId": "6f1c1f9e-6a43-4c55-9d0a-0e0f3b2f7a11",
          "accounts": [{
            "id": "6f1c1f9e-6a43-4c55-9d0a-0e0f3b2f7a11",
            "name": "Digital PO S3",
            "type": "S3Compatible",
            "endpointHost": "store.example.net",
            "useTls": true,
            "region": "us-east-1",
            "accessKeyId": "AKID",
            "forcePathStyle": true,
            "customCaPem": "",
            "manualBuckets": ["po-archive"],
            "notes": "",
            "createdAt": "2026-10-16T11:07:00.1234567+07:00",
            "updatedAt": "2026-10-16T11:07:00.1234567+07:00",
            "endpointUri": "https://store.example.net/",
            "typeLabel": "S3 Compatible Storage"
          }]
        }"#;
        let doc: AccountsDocument = serde_json::from_str(json).expect("parse");
        assert_eq!(doc.accounts[0].kind, AccountType::S3Compatible);
        assert_eq!(doc.accounts[0].manual_buckets, ["po-archive"]);
    }
}
