//! Optional master password that encrypts the stored access keys.
//!
//! Without a master password, secrets go to the keychain as they are (the keychain itself is
//! encrypted per Windows user). With one, each secret is additionally sealed with AES-256-GCM
//! under a key derived from the password (PBKDF2-SHA256), so another program running as the same
//! user cannot read them from the keychain without the password.

use std::num::NonZeroU32;
use std::sync::{Arc, PoisonError, RwLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::account::{SecretError, SecretStore};

const PREFIX: &str = "enc:v1:";
const CHECK: &[u8] = b"prometheus-s3manager-master-check";
/// PBKDF2 iterations for new master passwords.
pub const ITERATIONS: u32 = 600_000;
/// Minimum master password length.
pub const MIN_LENGTH: usize = 8;

/// Stored next to the settings: how to derive the key and how to check a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterConfig {
    /// Base64 salt.
    pub salt: String,
    /// PBKDF2 iterations.
    pub iterations: u32,
    /// A known value sealed with the key.
    pub verifier: String,
}

/// Derived key, kept in memory while unlocked.
pub struct MasterKey(LessSafeKey);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

fn derive(password: &str, salt: &[u8], iterations: u32) -> Result<MasterKey, SecretError> {
    let mut key = [0u8; 32];
    let normalized: String = password.nfkc().collect();
    let iterations =
        NonZeroU32::new(iterations).ok_or_else(|| SecretError("invalid iteration count".into()))?;
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        normalized.as_bytes(),
        &mut key,
    );
    let unbound =
        UnboundKey::new(&AES_256_GCM, &key).map_err(|_| SecretError("invalid key".into()))?;
    Ok(MasterKey(LessSafeKey::new(unbound)))
}

impl MasterKey {
    fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<String, SecretError> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| SecretError("no secure random source".into()))?;
        let mut data = plain.to_vec();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut data,
            )
            .map_err(|_| SecretError("encryption failed".into()))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&data);
        Ok(B64.encode(out))
    }

    fn open(&self, sealed: &str, aad: &[u8]) -> Result<Vec<u8>, SecretError> {
        let bad = || {
            SecretError("the stored secret cannot be decrypted with this master password".into())
        };
        let raw = B64.decode(sealed).map_err(|_| bad())?;
        if raw.len() < NONCE_LEN + 16 {
            return Err(bad());
        }
        let (nonce, data) = raw.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| bad())?;
        let mut data = data.to_vec();
        let plain = self
            .0
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut data,
            )
            .map_err(|_| bad())?;
        Ok(plain.to_vec())
    }
}

/// Creates a configuration and key for a new master password.
pub fn create(password: &str) -> Result<(MasterConfig, MasterKey), SecretError> {
    if password.chars().count() < MIN_LENGTH {
        return Err(SecretError(format!(
            "The master password must have at least {MIN_LENGTH} characters."
        )));
    }
    let mut salt = [0u8; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| SecretError("no secure random source".into()))?;
    let key = derive(password, &salt, ITERATIONS)?;
    let verifier = key.seal(CHECK, b"check")?;
    Ok((
        MasterConfig {
            salt: B64.encode(salt),
            iterations: ITERATIONS,
            verifier,
        },
        key,
    ))
}

/// Checks a password against the configuration and returns the key.
pub fn unlock(config: &MasterConfig, password: &str) -> Result<MasterKey, SecretError> {
    let salt = B64
        .decode(&config.salt)
        .map_err(|_| SecretError("damaged master password settings".into()))?;
    let key = derive(password, &salt, config.iterations)?;
    match key.open(&config.verifier, b"check") {
        Ok(v) if v == CHECK => Ok(key),
        _ => Err(SecretError("Wrong master password.".into())),
    }
}

/// [`SecretStore`] wrapper that seals secrets while a master key is set.
pub struct ProtectedSecrets {
    inner: Arc<dyn SecretStore>,
    key: RwLock<Option<Arc<MasterKey>>>,
    required: RwLock<bool>,
}

impl ProtectedSecrets {
    /// Wraps a store. `required` is true when a master password is configured (secrets are
    /// unreadable until [`Self::set_key`]).
    pub fn new(inner: Arc<dyn SecretStore>, required: bool) -> Self {
        Self {
            inner,
            key: RwLock::new(None),
            required: RwLock::new(required),
        }
    }

    /// Whether a master password is configured but not entered yet.
    pub fn is_locked(&self) -> bool {
        *self.required.read().unwrap_or_else(PoisonError::into_inner)
            && self
                .key
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
    }

    /// Whether a master password is configured.
    pub fn is_required(&self) -> bool {
        *self.required.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets (unlock) or clears (lock) the key.
    pub fn set_key(&self, key: Option<MasterKey>) {
        *self.key.write().unwrap_or_else(PoisonError::into_inner) = key.map(Arc::new);
    }

    /// Re-stores the given secrets under a new key (`None` = without master password) and
    /// switches to it. `ids` are all account ids (plus any other secret ids in use).
    pub fn rekey(&self, ids: &[Uuid], new_key: Option<MasterKey>) -> Result<(), SecretError> {
        let mut plain = Vec::new();
        for id in ids {
            if let Some(secret) = self.get(*id)? {
                plain.push((*id, secret));
            }
        }
        let required = new_key.is_some();
        *self.key.write().unwrap_or_else(PoisonError::into_inner) = new_key.map(Arc::new);
        *self
            .required
            .write()
            .unwrap_or_else(PoisonError::into_inner) = required;
        for (id, secret) in plain {
            self.set(id, &secret)?;
        }
        Ok(())
    }
}

impl SecretStore for ProtectedSecrets {
    fn get(&self, id: Uuid) -> Result<Option<String>, SecretError> {
        let Some(stored) = self.inner.get(id)? else {
            return Ok(None);
        };
        let Some(sealed) = stored.strip_prefix(PREFIX) else {
            return Ok(Some(stored));
        };
        let key = self
            .key
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let key = key.ok_or_else(|| {
            SecretError("Secrets are locked. Enter the master password (Tools > Unlock).".into())
        })?;
        let plain = key.open(sealed, id.as_bytes())?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|_| SecretError("damaged secret".into()))
    }

    fn set(&self, id: Uuid, secret: &str) -> Result<(), SecretError> {
        if self.is_locked() {
            return Err(SecretError(
                "Secrets are locked. Enter the master password (Tools > Unlock).".into(),
            ));
        }
        let key = self
            .key
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match key {
            Some(key) => {
                let sealed = key.seal(secret.as_bytes(), id.as_bytes())?;
                self.inner.set(id, &format!("{PREFIX}{sealed}"))
            }
            None => self.inner.set(id, secret),
        }
    }

    fn delete(&self, id: Uuid) -> Result<(), SecretError> {
        self.inner.delete(id)
    }

    fn has(&self, id: Uuid) -> bool {
        self.inner.has(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::MemorySecrets;

    fn quick(password: &str) -> (MasterConfig, MasterKey) {
        // Fewer iterations keep the test fast; the format is the same.
        let salt = [7u8; 16];
        let key = derive(password, &salt, 1000).expect("derive");
        let verifier = key.seal(CHECK, b"check").expect("seal");
        (
            MasterConfig {
                salt: B64.encode(salt),
                iterations: 1000,
                verifier,
            },
            key,
        )
    }

    #[test]
    fn unlock_checks_the_password() {
        let (config, _) = quick("correct horse");
        assert!(unlock(&config, "correct horse").is_ok());
        assert!(
            unlock(&config, "wrong horse")
                .unwrap_err()
                .0
                .contains("Wrong")
        );
        assert!(create("short").is_err());
    }

    #[test]
    fn seals_rekeys_and_locks() {
        let inner = Arc::new(MemorySecrets::default());
        let store = ProtectedSecrets::new(inner.clone(), false);
        let a = Uuid::new_v4();
        store.set(a, "plain-secret").expect("set plain");
        assert_eq!(inner.get(a).expect("raw").as_deref(), Some("plain-secret"));

        let (config, key) = quick("master-pass");
        store.rekey(&[a], Some(key)).expect("rekey");
        let raw = inner.get(a).expect("raw").expect("some");
        assert!(raw.starts_with(PREFIX) && !raw.contains("plain-secret"));
        assert_eq!(store.get(a).expect("get").as_deref(), Some("plain-secret"));

        store.set_key(None);
        assert!(store.is_locked());
        assert!(store.get(a).unwrap_err().0.contains("locked"));
        assert!(store.set(a, "x").is_err());
        store.set_key(Some(unlock(&config, "master-pass").expect("unlock")));
        assert_eq!(store.get(a).expect("get").as_deref(), Some("plain-secret"));

        // A sealed secret cannot be moved to another account id.
        let b = Uuid::new_v4();
        inner.set(b, &raw).expect("copy raw");
        assert!(store.get(b).is_err());

        store.rekey(&[a], None).expect("remove master");
        assert!(!store.is_required());
        assert_eq!(inner.get(a).expect("raw").as_deref(), Some("plain-secret"));
    }
}
