//! User-facing storage errors with actionable hints. Never contain secrets.

use std::error::Error as StdError;

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;

use crate::guard::PrivateNetworkViolation;

/// Category of a storage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    /// 403 / policy.
    AccessDenied,
    /// Wrong or missing keys.
    InvalidCredentials,
    /// Bucket or object missing.
    NotFound,
    /// Certificate not trusted.
    Tls,
    /// Plain HTTP to a public address refused.
    PrivateNetwork,
    /// Connection problems.
    Network,
    /// Timed out.
    Timeout,
    /// Throttled by the server.
    Throttled,
    /// 5xx.
    Server,
    /// Rejected request or invalid input.
    InvalidRequest,
    /// Canceled by the user.
    Canceled,
    /// Anything else.
    Unknown,
}

/// A failed storage operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.display())]
pub struct StorageError {
    /// Category.
    pub kind: StorageErrorKind,
    /// What happened.
    pub message: String,
    /// What to do about it.
    pub hint: Option<String>,
}

const TLS_HINT: &str = "Install the company root CA in Windows (Trusted Root Certification Authorities) or paste it in the account Advanced settings > Custom CA. Certificate checks are never disabled.";

impl StorageError {
    /// Error without a hint.
    pub fn new(kind: StorageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    /// Error with a hint.
    pub fn with_hint(
        kind: StorageErrorKind,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: Some(hint.into()),
        }
    }

    /// Invalid input.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StorageErrorKind::InvalidRequest, message)
    }

    /// Worth retrying.
    pub fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            StorageErrorKind::Network
                | StorageErrorKind::Timeout
                | StorageErrorKind::Throttled
                | StorageErrorKind::Server
        )
    }

    /// Message followed by the hint.
    pub fn display(&self) -> String {
        match &self.hint {
            Some(hint) => format!("{} {hint}", self.message),
            None => self.message.clone(),
        }
    }

    /// Maps an SDK failure.
    pub fn from_sdk<E>(err: SdkError<E, HttpResponse>) -> Self
    where
        E: ProvideErrorMetadata + StdError + Send + Sync + 'static,
    {
        use StorageErrorKind as K;
        match &err {
            SdkError::ServiceError(ctx) => {
                let code = ctx.err().code().unwrap_or("").to_owned();
                let message = ctx.err().message().unwrap_or("").to_owned();
                let status = ctx.raw().status().as_u16();
                from_service(&code, &message, status)
            }
            SdkError::TimeoutError(_) => Self::with_hint(
                K::Timeout,
                "The request timed out.",
                "The server may be unreachable or overloaded.",
            ),
            SdkError::DispatchFailure(failure) => {
                let chain = chain(&err);
                if let Some(v) = find_violation(&err) {
                    return Self::new(K::PrivateNetwork, v.to_string());
                }
                if chain.contains("Plain HTTP endpoint") {
                    return Self::new(K::PrivateNetwork, root_cause(&err));
                }
                if chain.to_ascii_lowercase().contains("certificate") {
                    return Self::with_hint(
                        K::Tls,
                        "TLS certificate verification failed.",
                        TLS_HINT,
                    );
                }
                if failure.is_timeout() {
                    return Self::with_hint(
                        K::Timeout,
                        "The request timed out.",
                        "The server may be unreachable or overloaded.",
                    );
                }
                Self::with_hint(
                    K::Network,
                    format!("Could not reach the endpoint: {}", root_cause(&err)),
                    "Check host, port, VPN and firewall.",
                )
            }
            SdkError::ResponseError(_) => Self::with_hint(
                K::Network,
                format!("The server sent an invalid response: {}", root_cause(&err)),
                "Check that the endpoint is an S3 API (not a web console) and that path-style matches the server.",
            ),
            SdkError::ConstructionFailure(_) => Self::new(
                K::InvalidRequest,
                format!("Invalid request: {}", root_cause(&err)),
            ),
            _ => Self::new(K::Unknown, root_cause(&err)),
        }
    }
}

fn from_service(code: &str, message: &str, status: u16) -> StorageError {
    use StorageErrorKind as K;
    match code {
        "AccessDenied" | "AllAccessDisabled" => StorageError::with_hint(
            K::AccessDenied,
            "Access denied.",
            "Check the bucket policy or IAM permissions. If this key cannot list buckets, register bucket names in the account settings.",
        ),
        "InvalidAccessKeyId" => StorageError::with_hint(
            K::InvalidCredentials,
            "The Access Key ID is not recognized by this endpoint.",
            "Check the key and the endpoint.",
        ),
        "SignatureDoesNotMatch" => StorageError::with_hint(
            K::InvalidCredentials,
            "The Secret Access Key does not match.",
            "Re-enter the secret. For S3-compatible storage also check region and path-style.",
        ),
        "NoSuchBucket" => {
            StorageError::new(K::NotFound, "The bucket does not exist on this endpoint.")
        }
        "NoSuchKey" | "NotFound" => StorageError::new(K::NotFound, "The object was not found."),
        "SlowDown" | "Throttling" | "RequestLimitExceeded" => {
            StorageError::new(K::Throttled, "The server is throttling requests.")
        }
        "RequestTimeout" => StorageError::new(K::Timeout, "The request timed out."),
        _ => match status {
            403 => StorageError::new(
                K::AccessDenied,
                if code.is_empty() {
                    "Access denied.".to_owned()
                } else {
                    format!("Access denied ({code}).")
                },
            ),
            404 => StorageError::new(K::NotFound, "Not found."),
            429 | 503 => StorageError::new(K::Throttled, "The server is busy."),
            500.. => StorageError::new(K::Server, format!("Server error ({status} {code}).")),
            _ => StorageError::new(
                K::InvalidRequest,
                format!(
                    "The server rejected the request: {}",
                    if message.is_empty() { code } else { message }
                ),
            ),
        },
    }
}

fn find_violation<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a PrivateNetworkViolation> {
    let mut current = Some(err);
    while let Some(e) = current {
        if let Some(v) = e.downcast_ref::<PrivateNetworkViolation>() {
            return Some(v);
        }
        current = e.source();
    }
    None
}

/// All messages in the source chain, joined.
fn chain(err: &(dyn StdError + 'static)) -> String {
    let mut parts = Vec::new();
    let mut current = Some(err);
    while let Some(e) = current {
        parts.push(e.to_string());
        current = e.source();
    }
    parts.join(": ")
}

/// Deepest message in the source chain.
fn root_cause(err: &(dyn StdError + 'static)) -> String {
    let mut current = err;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_codes() {
        assert_eq!(
            from_service("AccessDenied", "", 403).kind,
            StorageErrorKind::AccessDenied
        );
        assert_eq!(
            from_service("SignatureDoesNotMatch", "", 403).kind,
            StorageErrorKind::InvalidCredentials
        );
        assert_eq!(from_service("", "", 404).kind, StorageErrorKind::NotFound);
        assert_eq!(from_service("", "", 503).kind, StorageErrorKind::Throttled);
        let e = from_service("InternalError", "", 500);
        assert_eq!(e.kind, StorageErrorKind::Server);
        assert!(e.is_transient());
        assert!(
            from_service("Weird", "bad thing", 400)
                .message
                .contains("bad thing")
        );
    }
}
