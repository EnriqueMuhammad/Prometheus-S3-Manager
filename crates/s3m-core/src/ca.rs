//! Validation of the per-account custom CA bundle (PEM). Certificates only, never keys.

use base64::Engine as _;
use chrono::{DateTime, Utc};

/// Maximum accepted size of the PEM text.
pub const MAX_LENGTH: usize = 65_536;
/// Maximum number of certificates in one bundle.
pub const MAX_CERTIFICATES: usize = 20;

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// A parsed certificate from the bundle.
#[derive(Debug, Clone)]
pub struct CaCertificate {
    /// DER bytes.
    pub der: Vec<u8>,
    /// Subject distinguished name, for messages.
    pub subject: String,
    /// Expiry.
    pub not_after: DateTime<Utc>,
}

/// Parses and validates a bundle; empty input yields an empty list. Errors are user-facing.
pub fn parse(pem: &str, now: DateTime<Utc>) -> Result<Vec<CaCertificate>, String> {
    let text = pem.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.len() > MAX_LENGTH {
        return Err("Custom CA bundle is too large.".into());
    }

    let mut blocks = Vec::new();
    let mut outside = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        outside.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(END) else {
            outside.push_str(after);
            rest = "";
            break;
        };
        blocks.push(&after[BEGIN.len()..end]);
        rest = &after[end + END.len()..];
    }
    outside.push_str(rest);

    if blocks.is_empty() {
        return Err("Custom CA must contain at least one PEM \"BEGIN CERTIFICATE\" block.".into());
    }
    if blocks.len() > MAX_CERTIFICATES {
        return Err(format!(
            "Custom CA bundle may contain at most {MAX_CERTIFICATES} certificates."
        ));
    }
    if !outside.trim().is_empty() {
        return Err("Custom CA contains data outside PEM certificate blocks (private keys are never accepted).".into());
    }

    blocks
        .iter()
        .enumerate()
        .map(|(i, body)| {
            let n = i + 1;
            let invalid =
                || format!("Custom CA certificate #{n} is not a valid X.509 certificate.");
            let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            let der = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| invalid())?;
            let (_, cert) = x509_parser::parse_x509_certificate(&der).map_err(|_| invalid())?;
            let not_after = DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)
                .ok_or_else(invalid)?;
            let subject = cert.subject().to_string();
            if not_after < now {
                return Err(format!(
                    "Custom CA certificate #{n} ({subject}) expired on {}.",
                    not_after.format("%Y-%m-%d")
                ));
            }
            Ok(CaCertificate {
                der: der.clone(),
                subject,
                not_after,
            })
        })
        .collect()
}

/// Normalised PEM text for storage (`""` when none).
pub fn normalize(pem: &str) -> Result<String, String> {
    let certs = parse(pem, Utc::now())?;
    Ok(certs
        .iter()
        .map(|c| to_pem(&c.der))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Encodes DER as a PEM certificate block with 64-character lines.
pub fn to_pem(der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::from(BEGIN);
    out.push('\n');
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str(END);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_fine() {
        assert!(parse("  ", Utc::now()).expect("empty").is_empty());
        assert_eq!(normalize("").expect("empty"), "");
    }

    #[test]
    fn rejects_garbage() {
        assert!(
            parse("hello", Utc::now())
                .unwrap_err()
                .contains("BEGIN CERTIFICATE")
        );
        let key = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";
        assert!(parse(key, Utc::now()).unwrap_err().contains("outside PEM"));
        let bad = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";
        assert!(parse(bad, Utc::now()).unwrap_err().contains("#1"));
    }
}
