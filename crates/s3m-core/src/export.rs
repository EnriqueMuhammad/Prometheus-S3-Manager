//! Account export/import format `s3-manager-accounts` (version 2).
//!
//! Settings are clear JSON; secret keys are included only when a password is given, encrypted
//! with PBKDF2-SHA256 → AES-256-GCM. The account list is bound as GCM additional data, so an
//! edited endpoint makes decryption fail instead of sending a real secret elsewhere. The
//! additional data is the compact JSON of `{format, version, exportedAt, accounts}` with the
//! format's string escaping; [`canonical_aad`] reproduces it byte for byte, so the format must
//! not change once files exist.
//!
//! Version 1 files are read too (settings only; their scrypt-encrypted secrets are not).

use std::num::NonZeroU32;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::Utc;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::account::{Account, AccountDraft, AccountType};
use crate::endpoint;

/// Value of the `format` field.
pub const FORMAT_NAME: &str = "s3-manager-accounts";
/// Minimum export password length.
pub const MIN_PASSWORD_LENGTH: usize = 8;
/// Largest accepted import file.
pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
const PBKDF2_ITERATIONS: u32 = 600_000;
const TAG_LEN: usize = 16;

/// User-facing import/export failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ExportError(pub String);

fn err(message: impl Into<String>) -> ExportError {
    ExportError(message.into())
}

/// Account as it appears in an export file (no local ids, no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedAccount {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// `"aws"` or `"custom"` (same values as version 1 files).
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Host[:port].
    #[serde(default)]
    pub endpoint_host: String,
    /// SSL/TLS.
    #[serde(default)]
    pub use_tls: bool,
    /// Region.
    #[serde(default)]
    pub region: String,
    /// Access key id.
    #[serde(default)]
    pub access_key_id: String,
    /// Path-style addressing.
    #[serde(default)]
    pub force_path_style: bool,
    /// Custom CA bundle.
    #[serde(default)]
    pub custom_ca_pem: String,
    /// Registered buckets.
    #[serde(default)]
    pub manual_buckets: Vec<String>,
    /// Notes.
    #[serde(default)]
    pub notes: String,
    /// Folder in the account list. Not part of the authenticated data, so files stay readable
    /// by readers that do not know the field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
}

/// Encrypted secret blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedSecrets {
    /// `pbkdf2-sha256`.
    pub kdf: String,
    /// PBKDF2 iterations.
    pub iterations: u32,
    /// `aes-256-gcm`.
    pub cipher: String,
    /// Base64 salt.
    pub salt: String,
    /// Base64 nonce.
    pub iv: String,
    /// Base64 GCM tag.
    pub tag: String,
    /// Base64 ciphertext of a JSON array of secrets (one per account, `null` when missing).
    pub data: String,
}

/// Version 2 export file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountExportFile {
    /// Always [`FORMAT_NAME`].
    pub format: String,
    /// Always 2.
    pub version: u32,
    /// ISO-8601 timestamp, kept verbatim (it is part of the additional data).
    pub exported_at: String,
    /// Accounts.
    pub accounts: Vec<ExportedAccount>,
    /// Encrypted secrets, when exported with a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<EncryptedSecrets>,
}

/// What an import file contains, ready to be saved as new accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPreview {
    /// Drafts (with secrets when the file had them and the password was right).
    pub accounts: Vec<AccountDraft>,
    /// The file carried encrypted secrets that were decrypted.
    pub has_encrypted_secrets: bool,
    /// A v1 file carried secrets this version cannot decrypt.
    pub secrets_unsupported: bool,
}

/// Serialises accounts; with a password, their secrets are encrypted into the file.
pub fn export(
    accounts: &[Account],
    secret_for: impl Fn(&Account) -> Option<String>,
    password: Option<&str>,
) -> Result<String, ExportError> {
    if accounts.is_empty() {
        return Err(err("There are no accounts to export."));
    }
    let mut file = AccountExportFile {
        format: FORMAT_NAME.into(),
        version: 2,
        exported_at: round_trip_now(),
        accounts: accounts.iter().map(to_exported).collect(),
        secrets: None,
    };
    if let Some(password) = password {
        if password.chars().count() < MIN_PASSWORD_LENGTH {
            return Err(err(format!(
                "Export password must be at least {MIN_PASSWORD_LENGTH} characters."
            )));
        }
        let rng = SystemRandom::new();
        let mut salt = [0u8; 16];
        let mut iv = [0u8; NONCE_LEN];
        rng.fill(&mut salt)
            .map_err(|_| err("No secure random source."))?;
        rng.fill(&mut iv)
            .map_err(|_| err("No secure random source."))?;
        let secrets: Vec<Option<String>> = accounts.iter().map(&secret_for).collect();
        let mut buffer = serde_json::to_vec(&secrets).map_err(|e| err(e.to_string()))?;
        let key = derive_key(password, &salt, PBKDF2_ITERATIONS)?;
        let tag = key
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(iv),
                Aad::from(canonical_aad(&file)),
                &mut buffer,
            )
            .map_err(|_| err("Encryption failed."))?;
        file.secrets = Some(EncryptedSecrets {
            kdf: "pbkdf2-sha256".into(),
            iterations: PBKDF2_ITERATIONS,
            cipher: "aes-256-gcm".into(),
            salt: B64.encode(salt),
            iv: B64.encode(iv),
            tag: B64.encode(tag.as_ref()),
            data: B64.encode(&buffer),
        });
    }
    serde_json::to_string_pretty(&file).map_err(|e| err(e.to_string()))
}

/// Whether the text is a v2 export with encrypted secrets (asks for a password first).
pub fn has_encrypted_secrets(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .is_ok_and(|v| v.get("secrets").is_some_and(|s| !s.is_null()) && v["version"] == 2)
}

/// Parses an export file. `password` is required when the file holds encrypted secrets.
pub fn read(text: &str, password: Option<&str>) -> Result<ImportPreview, ExportError> {
    if text.len() > MAX_FILE_BYTES {
        return Err(err("Import file is too large."));
    }
    let root: Value =
        serde_json::from_str(text).map_err(|_| err("Import file is not valid JSON."))?;
    if root.get("format").and_then(Value::as_str) != Some(FORMAT_NAME) {
        return Err(err("Not a Prometheus S3Manager account export."));
    }
    match root.get("version").and_then(Value::as_u64) {
        Some(2) => read_v2(text, password),
        Some(1) => read_v1(&root),
        _ => Err(err("Unsupported export version.")),
    }
}

fn read_v2(text: &str, password: Option<&str>) -> Result<ImportPreview, ExportError> {
    let file: AccountExportFile =
        serde_json::from_str(text).map_err(|e| err(format!("Export file is malformed: {e}")))?;
    if file.accounts.is_empty() || file.accounts.len() > 1000 {
        return Err(err("Export must contain between 1 and 1000 accounts."));
    }
    let mut secrets: Vec<Option<String>> = vec![None; file.accounts.len()];
    if let Some(enc) = &file.secrets {
        let password = password.filter(|p| !p.is_empty()).ok_or_else(|| {
            err("This export contains encrypted secret keys. Enter its password.")
        })?;
        if enc.kdf != "pbkdf2-sha256"
            || enc.cipher != "aes-256-gcm"
            || !(100_000..=10_000_000).contains(&enc.iterations)
        {
            return Err(err("Unsupported secret encryption in export file."));
        }
        let wrong = || err("Wrong password, or the export file was modified.");
        let salt = B64.decode(&enc.salt).map_err(|_| wrong())?;
        let iv: [u8; NONCE_LEN] = B64
            .decode(&enc.iv)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(wrong)?;
        let mut buffer = B64.decode(&enc.data).map_err(|_| wrong())?;
        let tag = B64.decode(&enc.tag).map_err(|_| wrong())?;
        if tag.len() != TAG_LEN {
            return Err(wrong());
        }
        buffer.extend_from_slice(&tag);
        let key = derive_key(password, &salt, enc.iterations)?;
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(iv),
                Aad::from(canonical_aad(&file)),
                &mut buffer,
            )
            .map_err(|_| wrong())?;
        let list: Vec<Option<String>> = serde_json::from_slice(plain).map_err(|_| wrong())?;
        if list.len() != file.accounts.len() {
            return Err(wrong());
        }
        secrets = list;
    }
    let accounts = file
        .accounts
        .iter()
        .zip(secrets)
        .map(|(a, secret)| AccountDraft {
            id: None,
            name: a.name.clone(),
            kind: if a.kind == "aws" {
                AccountType::AmazonS3
            } else {
                AccountType::S3Compatible
            },
            endpoint_host: a.endpoint_host.clone(),
            use_tls: a.use_tls,
            region: a.region.clone(),
            access_key_id: a.access_key_id.clone(),
            secret_access_key: secret,
            force_path_style: a.force_path_style,
            custom_ca_pem: a.custom_ca_pem.clone(),
            manual_buckets: a.manual_buckets.clone(),
            notes: a.notes.clone(),
            folder: a.folder.clone(),
        })
        .collect();
    Ok(ImportPreview {
        accounts,
        has_encrypted_secrets: file.secrets.is_some(),
        secrets_unsupported: false,
    })
}

/// Version 1 export: endpoint is a full URL; secrets are scrypt-encrypted and not supported.
fn read_v1(root: &Value) -> Result<ImportPreview, ExportError> {
    let list = root
        .get("accounts")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= 1000)
        .ok_or_else(|| err("Export must contain between 1 and 1000 accounts."))?;
    let s = |node: &Value, key: &str| node.get(key).and_then(Value::as_str).map(str::to_owned);
    let accounts = list
        .iter()
        .filter(|n| n.is_object())
        .map(|node| {
            let fields = endpoint::normalize(&s(node, "endpoint").unwrap_or_default(), true);
            AccountDraft {
                id: None,
                name: s(node, "name").unwrap_or_else(|| "Imported account".into()),
                kind: if s(node, "provider").as_deref() == Some("aws") {
                    AccountType::AmazonS3
                } else {
                    AccountType::S3Compatible
                },
                endpoint_host: fields.host,
                use_tls: fields.use_tls,
                region: s(node, "region").unwrap_or_else(|| "us-east-1".into()),
                access_key_id: s(node, "accessKeyId").unwrap_or_default(),
                secret_access_key: None,
                force_path_style: node
                    .get("forcePathStyle")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                custom_ca_pem: s(node, "caBundlePem").unwrap_or_default(),
                manual_buckets: node
                    .get("manualBuckets")
                    .and_then(Value::as_array)
                    .map(|b| {
                        b.iter()
                            .filter_map(Value::as_str)
                            .filter(|b| !b.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                notes: s(node, "notes").unwrap_or_default(),
                folder: String::new(),
            }
        })
        .collect();
    Ok(ImportPreview {
        accounts,
        has_encrypted_secrets: false,
        secrets_unsupported: root.get("secrets").is_some_and(|v| !v.is_null()),
    })
}

fn to_exported(a: &Account) -> ExportedAccount {
    ExportedAccount {
        name: a.name.clone(),
        kind: match a.kind {
            AccountType::AmazonS3 => "aws".into(),
            AccountType::S3Compatible => "custom".into(),
        },
        endpoint_host: a.endpoint_host.clone(),
        use_tls: a.use_tls,
        region: a.region.clone(),
        access_key_id: a.access_key_id.clone(),
        force_path_style: a.force_path_style,
        custom_ca_pem: a.custom_ca_pem.clone(),
        manual_buckets: a.manual_buckets.clone(),
        notes: a.notes.clone(),
        folder: a.folder.clone(),
    }
}

/// Current UTC time in round-trip form: seven fractional digits and an explicit offset.
fn round_trip_now() -> String {
    let now = Utc::now();
    format!(
        "{}.{:07}+00:00",
        now.format("%Y-%m-%dT%H:%M:%S"),
        now.timestamp_subsec_nanos() / 100
    )
}

fn derive_key(password: &str, salt: &[u8], iterations: u32) -> Result<LessSafeKey, ExportError> {
    let normalized: String = password.nfkc().collect();
    let mut key = [0u8; 32];
    let iterations = NonZeroU32::new(iterations).ok_or_else(|| err("Invalid iteration count."))?;
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        normalized.as_bytes(),
        &mut key,
    );
    let unbound = UnboundKey::new(&AES_256_GCM, &key).map_err(|_| err("Invalid key."))?;
    Ok(LessSafeKey::new(unbound))
}

/// Compact JSON of everything except the secrets blob, byte for byte as the format defines it.
pub fn canonical_aad(file: &AccountExportFile) -> Vec<u8> {
    let mut out = String::with_capacity(256);
    out.push_str("{\"format\":");
    canonical_string(&mut out, &file.format);
    out.push_str(",\"version\":");
    out.push_str(&file.version.to_string());
    out.push_str(",\"exportedAt\":");
    canonical_string(&mut out, &file.exported_at);
    out.push_str(",\"accounts\":[");
    for (i, a) in file.accounts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        canonical_string(&mut out, &a.name);
        out.push_str(",\"type\":");
        canonical_string(&mut out, &a.kind);
        out.push_str(",\"endpointHost\":");
        canonical_string(&mut out, &a.endpoint_host);
        out.push_str(",\"useTls\":");
        out.push_str(if a.use_tls { "true" } else { "false" });
        out.push_str(",\"region\":");
        canonical_string(&mut out, &a.region);
        out.push_str(",\"accessKeyId\":");
        canonical_string(&mut out, &a.access_key_id);
        out.push_str(",\"forcePathStyle\":");
        out.push_str(if a.force_path_style { "true" } else { "false" });
        out.push_str(",\"customCaPem\":");
        canonical_string(&mut out, &a.custom_ca_pem);
        out.push_str(",\"manualBuckets\":[");
        for (j, b) in a.manual_buckets.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            canonical_string(&mut out, b);
        }
        out.push_str("],\"notes\":");
        canonical_string(&mut out, &a.notes);
        out.push('}');
    }
    out.push_str("]}");
    out.into_bytes()
}

/// Writes a JSON string literal with the format's escaping: only printable ASCII outside the
/// HTML-sensitive set stays literal; everything else becomes `\uXXXX` (upper-case hex, UTF-16
/// units), except the short escapes `\b \t \n \f \r \\`.
fn canonical_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' | '&' | '\'' | '+' | '<' | '>' | '`' => push_unit(out, c as u16),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    push_unit(out, *unit);
                }
            }
        }
    }
    out.push('"');
}

fn push_unit(out: &mut String, unit: u16) {
    out.push_str(&format!("\\u{unit:04X}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn account(name: &str) -> Account {
        Account {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: AccountType::S3Compatible,
            endpoint_host: "store.example.net".into(),
            use_tls: true,
            region: "us-east-1".into(),
            access_key_id: "AKID".into(),
            force_path_style: true,
            custom_ca_pem: String::new(),
            manual_buckets: vec!["po-archive".into()],
            notes: "Kantor <pusat> & \"cabang\" + café 🚀".into(),
            folder: "Kantor/Produksi".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn canonical_escaping() {
        let mut s = String::new();
        canonical_string(&mut s, "a\"b<c>&'+`\\\n é🚀\u{1}");
        // `~` stands for a backslash to keep the expectation readable.
        let expected =
            r#""a~u0022b~u003Cc~u003E~u0026~u0027~u002B~u0060~~~n ~u00E9~uD83D~uDE80~u0001""#;
        assert_eq!(s, expected.replace('~', "\\"));
    }

    #[test]
    fn round_trip_with_password() {
        let accounts = [account("A"), account("B")];
        let text = export(
            &accounts,
            |a| (a.name == "A").then(|| "s3cr3t".into()),
            Some("password1"),
        )
        .expect("export");
        assert!(!text.contains("s3cr3t"));
        assert!(has_encrypted_secrets(&text));
        let preview = read(&text, Some("password1")).expect("read");
        assert_eq!(preview.accounts.len(), 2);
        assert_eq!(
            preview.accounts[0].secret_access_key.as_deref(),
            Some("s3cr3t")
        );
        assert_eq!(preview.accounts[1].secret_access_key, None);
        assert_eq!(preview.accounts[0].notes, accounts[0].notes);
        assert_eq!(preview.accounts[0].folder, "Kantor/Produksi");

        assert!(
            read(&text, Some("wrong-pass"))
                .unwrap_err()
                .0
                .contains("Wrong password")
        );
        assert!(
            read(&text, None)
                .unwrap_err()
                .0
                .contains("Enter its password")
        );
        let tampered = text.replace("store.example.net", "evil.example.com");
        assert!(
            read(&tampered, Some("password1"))
                .unwrap_err()
                .0
                .contains("modified")
        );
    }

    #[test]
    fn export_without_password_and_limits() {
        let text = export(&[account("A")], |_| Some("x".into()), None).expect("export");
        assert!(!has_encrypted_secrets(&text));
        assert_eq!(
            read(&text, None).expect("read").accounts[0].secret_access_key,
            None
        );
        assert!(export(&[], |_| None, None).is_err());
        assert!(export(&[account("A")], |_| None, Some("short")).is_err());
        assert!(read("{}", None).unwrap_err().0.contains("Not a Prometheus"));
        assert!(read("nope", None).unwrap_err().0.contains("not valid JSON"));
    }

    #[test]
    fn reads_v1_export() {
        let text = r#"{"format":"s3-manager-accounts","version":1,"accounts":[{"name":"Old","provider":"custom","endpoint":"http://10.0.0.5:9000","region":"id-1","accessKeyId":"K","forcePathStyle":true,"manualBuckets":["b1"]}],"secrets":{"kdf":"scrypt"}}"#;
        let p = read(text, None).expect("v1");
        assert!(p.secrets_unsupported);
        assert_eq!(p.accounts[0].endpoint_host, "10.0.0.5:9000");
        assert!(!p.accounts[0].use_tls);
        assert_eq!(p.accounts[0].manual_buckets, ["b1"]);
    }

    /// A version 2 file with escaped text and an encrypted secret (password `password123`).
    #[test]
    fn reads_v2_export() {
        let text = include_str!("../tests/fixtures/export-v2.json");
        let p = read(text, Some("password123")).expect("decrypt v2 export");
        assert_eq!(p.accounts.len(), 2);
        assert_eq!(
            p.accounts[0].secret_access_key.as_deref(),
            Some("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY")
        );
        assert_eq!(p.accounts[1].secret_access_key, None);
        assert!(p.accounts[0].notes.contains('é'));
    }
}

#[cfg(test)]
mod interop {
    /// Writes a password-protected export to `$S3M_WRITE_EXPORT` so other readers of the format
    /// can be checked against it (`password123`).
    #[test]
    fn write_export_for_interop() {
        let Ok(path) = std::env::var("S3M_WRITE_EXPORT") else {
            return;
        };
        let text = include_str!("../tests/fixtures/export-v2.json");
        let preview = super::read(text, Some("password123")).expect("read");
        let accounts: Vec<_> = preview
            .accounts
            .iter()
            .map(|d| crate::account::Account {
                id: uuid::Uuid::new_v4(),
                folder: "Kantor/Produksi".into(),
                ..crate::account::validate(d).expect("valid")
            })
            .collect();
        let secrets = preview.accounts.clone();
        let out = super::export(
            &accounts,
            |a| {
                secrets
                    .iter()
                    .find(|d| d.name == a.name)
                    .and_then(|d| d.secret_access_key.clone())
            },
            Some("password123"),
        )
        .expect("export");
        std::fs::write(path, out).expect("write");
    }
}
