//! Secret access keys in the OS keychain (Windows Credential Manager, macOS Keychain, Secret
//! Service). There is no plaintext fallback: if the keychain fails, the call fails.

use s3m_core::{SecretError, SecretStore};
use uuid::Uuid;

/// Service name of the keychain entries.
pub const KEYRING_SERVICE: &str = "Prometheus S3Manager";

/// Secret-store id under which the proxy password is kept.
pub const PROXY_SECRET_ID: Uuid = Uuid::from_u128(0x5072_6f78_7950_6173_7377_6f72_6400_0001);

/// [`SecretStore`] backed by the platform keychain.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringSecrets;

fn entry(id: Uuid) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(KEYRING_SERVICE, &format!("secret-access-key:{id}"))
        .map_err(|e| SecretError(e.to_string()))
}

impl SecretStore for KeyringSecrets {
    fn get(&self, id: Uuid) -> Result<Option<String>, SecretError> {
        match entry(id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError(e.to_string())),
        }
    }

    fn set(&self, id: Uuid, secret: &str) -> Result<(), SecretError> {
        entry(id)?
            .set_password(secret)
            .map_err(|e| SecretError(e.to_string()))
    }

    fn delete(&self, id: Uuid) -> Result<(), SecretError> {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError(e.to_string())),
        }
    }
}
