//! S3 access for Prometheus S3Manager.
//!
//! Accounts become `aws-sdk-s3` clients only in [`ClientProvider`]: credentials come exclusively
//! from the secret store, TLS trusts the system roots plus the account's custom CA, and
//! plain-HTTP accounts dial private addresses only (see [`guard`]).

mod access_keys;
mod admin;
mod client;
mod error;
mod executor;
pub mod guard;
mod limit;
mod ops;
mod proxy;
mod service;
mod throttle;

pub use admin::AclTarget;
pub use client::ClientProvider;
pub use error::{StorageError, StorageErrorKind};
pub use executor::{PART_SIZE, S3TransferExecutor};
pub use limit::RateLimiter;
pub use ops::{MAX_PRESIGN, MAX_SINGLE_COPY, PreviewData};
pub use proxy::{ProxySettings, system_proxy, validate_proxy_url};
pub use service::StorageService;

/// Percent-encodes a key for `x-amz-copy-source` (keeps `/` and unreserved characters).
pub(crate) fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Converts an SDK timestamp.
pub(crate) fn to_chrono(t: &aws_smithy_types::DateTime) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_timestamp(t.secs(), t.subsec_nanos())
}

#[cfg(test)]
mod tests {
    #[test]
    fn copy_source_encoding() {
        assert_eq!(super::encode_key("a b/c+d/é.txt"), "a%20b/c%2Bd/%C3%A9.txt");
    }
}
