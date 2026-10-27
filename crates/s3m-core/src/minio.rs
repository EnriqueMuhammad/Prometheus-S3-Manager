//! MinIO admin API: access keys (service accounts) of the signed-in user.
//!
//! Requests and responses that carry credentials are encrypted with the caller's secret key
//! (Argon2id or PBKDF2 key derivation, then an AES-256-GCM or ChaCha20-Poly1305 stream in
//! 16 KiB fragments), which is what MinIO servers expect on these endpoints.

use chrono::{DateTime, Utc};
use ring::aead::{AES_256_GCM, Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// Admin API prefix.
pub const ADMIN_PREFIX: &str = "/minio/admin/v3";

const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 8;
const HEADER_LEN: usize = SALT_LEN + 1 + NONCE_LEN;
const TAG_LEN: usize = 16;
/// Plaintext bytes per encrypted fragment.
const FRAGMENT: usize = 1 << 14;

const ARGON2ID_AES_GCM: u8 = 0x00;
const ARGON2ID_CHACHA20: u8 = 0x01;
const PBKDF2_AES_GCM: u8 = 0x02;

/// Key derivation and cipher of one encrypted payload.
fn key_for(password: &str, salt: &[u8], id: u8) -> Result<LessSafeKey, String> {
    let mut key = [0u8; 32];
    match id {
        ARGON2ID_AES_GCM | ARGON2ID_CHACHA20 => {
            let params =
                argon2::Params::new(64 * 1024, 1, 4, Some(32)).map_err(|e| e.to_string())?;
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
                .hash_password_into(password.as_bytes(), salt, &mut key)
                .map_err(|e| e.to_string())?;
        }
        PBKDF2_AES_GCM => ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::new(8192).ok_or("bad iteration count")?,
            salt,
            password.as_bytes(),
            &mut key,
        ),
        other => {
            return Err(format!(
                "Unknown encryption scheme {other} in the server response."
            ));
        }
    }
    let algorithm = if id == ARGON2ID_CHACHA20 {
        &CHACHA20_POLY1305
    } else {
        &AES_256_GCM
    };
    UnboundKey::new(algorithm, &key)
        .map(LessSafeKey::new)
        .map_err(|_| "Invalid key.".to_owned())
}

/// Nonce of fragment `seq`: the random prefix followed by the little-endian sequence number.
fn nonce(prefix: &[u8], seq: u32) -> Result<Nonce, String> {
    let mut n = [0u8; 12];
    n[..NONCE_LEN].copy_from_slice(prefix);
    n[NONCE_LEN..].copy_from_slice(&seq.to_le_bytes());
    Ok(Nonce::assume_unique_for_key(n))
}

/// Additional data of every fragment: a final-fragment flag and the tag of an empty message.
fn stream_header(key: &LessSafeKey, prefix: &[u8]) -> Result<[u8; 1 + TAG_LEN], String> {
    let mut empty = Vec::new();
    key.seal_in_place_append_tag(nonce(prefix, 0)?, Aad::empty(), &mut empty)
        .map_err(|_| "Encryption failed.".to_owned())?;
    let mut ad = [0u8; 1 + TAG_LEN];
    ad[1..].copy_from_slice(&empty);
    Ok(ad)
}

/// Encrypts `data` with `password` (the secret key of the signing credentials).
pub fn encrypt_data(password: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let rng = SystemRandom::new();
    let mut header = [0u8; HEADER_LEN];
    rng.fill(&mut header)
        .map_err(|_| "No random numbers available.".to_owned())?;
    header[SALT_LEN] = ARGON2ID_AES_GCM;
    let (salt, rest) = header.split_at(SALT_LEN);
    let prefix = &rest[1..];
    let key = key_for(password, salt, ARGON2ID_AES_GCM)?;
    let mut ad = stream_header(&key, prefix)?;
    let mut out = header.to_vec();
    let mut chunks: Vec<&[u8]> = data.chunks(FRAGMENT).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.into_iter().enumerate() {
        ad[0] = if i == last { 0x80 } else { 0x00 };
        let seq = u32::try_from(i + 1).map_err(|_| "The payload is too large.".to_owned())?;
        let mut buf = chunk.to_vec();
        key.seal_in_place_append_tag(nonce(prefix, seq)?, Aad::from(ad), &mut buf)
            .map_err(|_| "Encryption failed.".to_owned())?;
        out.extend_from_slice(&buf);
    }
    Ok(out)
}

/// Decrypts a payload produced by [`encrypt_data`] or by the server.
pub fn decrypt_data(password: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < HEADER_LEN + TAG_LEN {
        return Err("The server response is too short to be encrypted data.".into());
    }
    let (salt, rest) = data.split_at(SALT_LEN);
    let id = rest[0];
    let prefix = &rest[1..=NONCE_LEN];
    let body = &rest[1 + NONCE_LEN..];
    let key = key_for(password, salt, id)?;
    let mut ad = stream_header(&key, prefix)?;
    let chunks: Vec<&[u8]> = body.chunks(FRAGMENT + TAG_LEN).collect();
    let last = chunks.len() - 1;
    let mut out = Vec::with_capacity(body.len());
    for (i, chunk) in chunks.into_iter().enumerate() {
        ad[0] = if i == last { 0x80 } else { 0x00 };
        let seq = u32::try_from(i + 1).map_err(|_| "The response is too large.".to_owned())?;
        let mut buf = chunk.to_vec();
        let plain = key
            .open_in_place(nonce(prefix, seq)?, Aad::from(ad), &mut buf)
            .map_err(|_| {
                "The server response could not be decrypted (wrong secret key?).".to_owned()
            })?;
        out.extend_from_slice(plain);
    }
    Ok(out)
}

/// Random characters from `alphabet`.
fn random_string(len: usize, alphabet: &[u8]) -> String {
    let rng = SystemRandom::new();
    let mut out = String::with_capacity(len);
    let mut byte = [0u8; 1];
    // Rejection sampling keeps every character equally likely.
    let limit = 256 - 256 % alphabet.len();
    while out.len() < len {
        if rng.fill(&mut byte).is_err() {
            out.push(char::from(alphabet[fastrand::usize(..alphabet.len())]));
            continue;
        }
        if usize::from(byte[0]) < limit {
            out.push(char::from(alphabet[usize::from(byte[0]) % alphabet.len()]));
        }
    }
    out
}

/// A new random access key (20 characters, like the server generates).
pub fn random_access_key() -> String {
    random_string(20, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
}

/// A new random secret key (40 characters).
pub fn random_secret_key() -> String {
    random_string(
        40,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
    )
}

/// Checks an access key and secret key typed by the user.
pub fn check_credentials(access_key: &str, secret_key: &str) -> Result<(), String> {
    let n = access_key.chars().count();
    if !(3..=20).contains(&n) {
        return Err("The access key must be 3 to 20 characters long.".into());
    }
    if access_key.contains(['=', ',']) || access_key.chars().any(char::is_whitespace) {
        return Err("The access key cannot contain spaces, '=' or ','.".into());
    }
    let n = secret_key.chars().count();
    if !(8..=40).contains(&n) {
        return Err("The secret key must be 8 to 40 characters long.".into());
    }
    Ok(())
}

/// Checks an access policy (JSON object with a `Statement`). Returns it re-indented.
pub fn check_policy(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("The policy is not valid JSON: {e}"))?;
    let statement = value
        .get("Statement")
        .ok_or("The policy needs a \"Statement\" element.")?;
    if !(statement.is_array() || statement.is_object()) {
        return Err("\"Statement\" must be an object or an array.".into());
    }
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// Pretty JSON of a policy as the server returns it (object or JSON string).
pub fn pretty_policy(value: &serde_json::Value) -> String {
    let parsed = match value {
        serde_json::Value::String(s) if s.trim().is_empty() => return String::new(),
        serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| value.clone()),
        serde_json::Value::Null => return String::new(),
        other => other.clone(),
    };
    serde_json::to_string_pretty(&parsed).unwrap_or_default()
}

/// Expiry dates up to the Unix epoch mean "never expires".
fn real_expiry(t: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    t.filter(|t| t.timestamp() > 0)
}

/// An access key in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessKey {
    /// Access key id.
    pub access_key: String,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// `on` or `off`.
    pub status: String,
    /// User the key belongs to.
    pub parent_user: String,
    /// The key has its own policy (restricted beyond the user's policy).
    pub restricted: bool,
    /// Expiry (`None` = never).
    pub expiration: Option<DateTime<Utc>>,
}

impl AccessKey {
    /// Whether the key is enabled.
    pub fn enabled(&self) -> bool {
        !self.status.eq_ignore_ascii_case("off")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InfoJson {
    #[serde(default)]
    parent_user: String,
    #[serde(default)]
    account_status: String,
    #[serde(default)]
    implied_policy: bool,
    #[serde(default)]
    access_key: String,
    #[serde(default)]
    policy: serde_json::Value,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    expiration: Option<DateTime<Utc>>,
}

impl InfoJson {
    fn into_key(self, access_key: String) -> AccessKey {
        AccessKey {
            access_key: if self.access_key.is_empty() {
                access_key
            } else {
                self.access_key
            },
            name: self.name,
            description: self.description,
            status: self.account_status,
            parent_user: self.parent_user,
            restricted: !self.implied_policy,
            expiration: real_expiry(self.expiration),
        }
    }
}

/// Parses `list-service-accounts` (decrypted). Old servers list bare access key ids.
pub fn parse_list(json: &[u8]) -> Result<Vec<AccessKey>, String> {
    #[derive(Deserialize)]
    struct List {
        // `null` when the user has no keys.
        #[serde(default)]
        accounts: Option<Vec<serde_json::Value>>,
    }
    let list: List = serde_json::from_slice(json)
        .map_err(|e| format!("Unexpected access key list from the server: {e}"))?;
    let accounts = list.accounts.unwrap_or_default();
    let mut keys = Vec::with_capacity(accounts.len());
    for value in accounts {
        match value {
            serde_json::Value::String(id) => keys.push(AccessKey {
                access_key: id,
                name: String::new(),
                description: String::new(),
                status: String::new(),
                parent_user: String::new(),
                restricted: false,
                expiration: None,
            }),
            other => {
                let info: InfoJson = serde_json::from_value(other)
                    .map_err(|e| format!("Unexpected access key from the server: {e}"))?;
                keys.push(info.into_key(String::new()));
            }
        }
    }
    keys.sort_by(|a, b| a.access_key.cmp(&b.access_key));
    Ok(keys)
}

/// Details of one access key, including its own policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessKeyInfo {
    /// The key.
    pub key: AccessKey,
    /// Its own policy, pretty-printed (empty = inherits the user's policy).
    pub policy: String,
}

/// Parses `info-service-account` (decrypted).
pub fn parse_info(access_key: &str, json: &[u8]) -> Result<AccessKeyInfo, String> {
    let info: InfoJson = serde_json::from_slice(json)
        .map_err(|e| format!("Unexpected access key details from the server: {e}"))?;
    let policy = if info.implied_policy {
        String::new()
    } else {
        pretty_policy(&info.policy)
    };
    Ok(AccessKeyInfo {
        key: info.into_key(access_key.to_owned()),
        policy,
    })
}

/// Parses `accountinfo`: the signed-in user's name and policy (pretty-printed).
pub fn parse_account_info(json: &[u8]) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    struct Info {
        #[serde(rename = "AccountName", default)]
        account_name: String,
        #[serde(rename = "Policy", default)]
        policy: serde_json::Value,
    }
    let info: Info = serde_json::from_slice(json)
        .map_err(|e| format!("Unexpected account information from the server: {e}"))?;
    Ok((info.account_name, pretty_policy(&info.policy)))
}

/// A key to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAccessKey {
    /// Access key id.
    pub access_key: String,
    /// Secret key.
    pub secret_key: String,
    /// Policy restricting the key (`None` = the user's policy).
    pub policy: Option<String>,
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Expiry (`None` = never).
    pub expiration: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AddJson<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    policy: Option<&'a serde_json::value::RawValue>,
    access_key: &'a str,
    secret_key: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    name: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expiration: Option<DateTime<Utc>>,
}

fn raw(policy: &str) -> Result<Box<serde_json::value::RawValue>, String> {
    let compact: serde_json::Value =
        serde_json::from_str(policy).map_err(|e| format!("The policy is not valid JSON: {e}"))?;
    serde_json::value::to_raw_value(&compact).map_err(|e| e.to_string())
}

impl NewAccessKey {
    /// Body of `add-service-account` (before encryption).
    pub fn request_json(&self) -> Result<Vec<u8>, String> {
        let policy = self.policy.as_deref().map(raw).transpose()?;
        serde_json::to_vec(&AddJson {
            policy: policy.as_deref(),
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            name: self.name.trim(),
            description: self.description.trim(),
            expiration: self.expiration,
        })
        .map_err(|e| e.to_string())
    }
}

/// Changes to an existing key; `None` fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessKeyUpdate {
    /// New policy: `Some(None)` removes the key's own policy (back to the user's policy).
    pub policy: Option<Option<String>>,
    /// Enable or disable.
    pub enabled: Option<bool>,
    /// New name.
    pub name: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// New expiry.
    pub expiration: Option<DateTime<Utc>>,
    /// New secret key.
    pub secret_key: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateJson<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    new_policy: Option<&'a serde_json::value::RawValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_secret_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_expiration: Option<DateTime<Utc>>,
}

impl AccessKeyUpdate {
    /// Body of `update-service-account` (before encryption).
    pub fn request_json(&self) -> Result<Vec<u8>, String> {
        // An empty policy document tells the server to drop the key's own policy.
        let policy = match &self.policy {
            None => None,
            Some(None) => Some(raw("{}")?),
            Some(Some(p)) => Some(raw(p)?),
        };
        serde_json::to_vec(&UpdateJson {
            new_policy: policy.as_deref(),
            new_secret_key: self.secret_key.as_deref(),
            new_status: self.enabled.map(|on| if on { "on" } else { "off" }),
            new_name: self.name.as_deref().map(str::trim),
            new_description: self.description.as_deref().map(str::trim),
            new_expiration: self.expiration,
        })
        .map_err(|e| e.to_string())
    }
}

/// Parses `add-service-account` (decrypted): the created access and secret key.
pub fn parse_created(json: &[u8]) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Creds {
        #[serde(default)]
        access_key: String,
        #[serde(default)]
        secret_key: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        credentials: Creds,
    }
    let r: Resp = serde_json::from_slice(json)
        .map_err(|e| format!("Unexpected answer from the server: {e}"))?;
    Ok((r.credentials.access_key, r.credentials.secret_key))
}

/// Message of an admin API error body (`{"Code": …, "Message": …}`), if it is one.
pub fn error_message(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Err {
        #[serde(rename = "Code", default)]
        code: String,
        #[serde(rename = "Message", default)]
        message: String,
    }
    let e: Err = serde_json::from_slice(body).ok()?;
    match (e.message.is_empty(), e.code.is_empty()) {
        (false, _) => Some(e.message),
        (true, false) => Some(e.code),
        (true, true) => None,
    }
}

/// Percent-encodes a query parameter value.
pub fn query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_round_trips() {
        for len in [0, 1, 300, FRAGMENT, FRAGMENT + 1, 3 * FRAGMENT + 7] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let sealed = encrypt_data("secret-key", &data).expect("encrypt");
            assert_eq!(sealed[SALT_LEN], ARGON2ID_AES_GCM);
            assert_eq!(decrypt_data("secret-key", &sealed).expect("decrypt"), data);
            assert!(decrypt_data("other-key", &sealed).is_err());
        }
    }

    #[test]
    fn random_keys_are_valid() {
        let (a, s) = (random_access_key(), random_secret_key());
        assert_eq!(a.len(), 20);
        assert_eq!(s.len(), 40);
        assert!(check_credentials(&a, &s).is_ok());
        assert_ne!(random_access_key(), a);
        assert!(check_credentials("ab", &s).is_err());
        assert!(check_credentials("a=b", &s).is_err());
        assert!(check_credentials(&a, "short").is_err());
    }

    #[test]
    fn parses_server_answers() {
        let list = br#"{"accounts":[
            {"parentUser":"u","accountStatus":"on","impliedPolicy":false,"accessKey":"B","name":"n","expiration":"1970-01-01T00:00:00Z"},
            {"parentUser":"u","accountStatus":"off","impliedPolicy":true,"accessKey":"A","expiration":"2030-01-02T03:04:05Z"}]}"#;
        let keys = parse_list(list).expect("list");
        assert_eq!(keys[0].access_key, "A");
        assert!(!keys[0].enabled() && !keys[0].restricted);
        assert!(keys[0].expiration.is_some());
        assert!(keys[1].restricted && keys[1].expiration.is_none());
        assert_eq!(
            parse_list(br#"{"accounts":["X"]}"#).expect("old")[0].access_key,
            "X"
        );
        assert!(
            parse_list(br#"{"accounts":null}"#)
                .expect("none")
                .is_empty()
        );

        let info = parse_info(
            "A",
            br#"{"parentUser":"u","accountStatus":"on","impliedPolicy":false,"policy":"{\"Version\":\"2012-10-17\",\"Statement\":[]}"}"#,
        )
        .expect("info");
        assert_eq!(info.key.access_key, "A");
        assert!(info.policy.contains("\"Statement\": []"));

        let (user, policy) =
            parse_account_info(br#"{"AccountName":"me","Policy":{"Statement":[]}}"#).expect("ai");
        assert_eq!(user, "me");
        assert!(policy.contains("Statement"));
        assert_eq!(
            error_message(br#"{"Code":"XMinioAdminNoSuchServiceAccount","Message":"gone"}"#),
            Some("gone".into())
        );
    }

    #[test]
    fn builds_requests() {
        let add = NewAccessKey {
            access_key: "AK".into(),
            secret_key: "secret".into(),
            policy: Some("{ \"Statement\": [] }".into()),
            name: " build ".into(),
            ..NewAccessKey::default()
        };
        let json = String::from_utf8(add.request_json().expect("add")).expect("utf8");
        assert_eq!(
            json,
            r#"{"policy":{"Statement":[]},"accessKey":"AK","secretKey":"secret","name":"build"}"#
        );
        let update = AccessKeyUpdate {
            policy: Some(None),
            enabled: Some(false),
            ..AccessKeyUpdate::default()
        };
        let json = String::from_utf8(update.request_json().expect("update")).expect("utf8");
        assert_eq!(json, r#"{"newPolicy":{},"newStatus":"off"}"#);
        assert_eq!(query_value("a b/c"), "a%20b%2Fc");
    }
}
