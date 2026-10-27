//! Connect-time guard for plain-HTTP accounts: the host is resolved here and the connection is
//! refused unless every address is private. Checking all addresses stops a mixed public/private
//! DNS answer from being dialled on its public half.

use std::future::Future;
use std::net::IpAddr;

use aws_smithy_runtime_api::client::dns::{DnsFuture, ResolveDns, ResolveDnsError};
use s3m_core::network;

/// Refusal to send credentials over plain HTTP outside the private network.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Plain HTTP endpoint \"{host}\" {reason}; refusing to send credentials over unencrypted HTTP outside the private network. Use SSL/TLS, or check that the host resolves to an internal address (VPN/DNS)."
)]
pub struct PrivateNetworkViolation {
    /// Host that was resolved.
    pub host: String,
    /// What was wrong with the answer.
    pub reason: String,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Resolves `host` with `lookup` and accepts the answer only if every address is private.
pub async fn resolve_private<F, Fut>(host: &str, lookup: F) -> Result<Vec<IpAddr>, BoxError>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = std::io::Result<Vec<IpAddr>>>,
{
    let violation = |reason: String| -> BoxError {
        Box::new(PrivateNetworkViolation {
            host: host.to_owned(),
            reason,
        })
    };
    if let Some(ip) = network::parse_literal(host) {
        return if network::is_private(ip) {
            Ok(vec![ip])
        } else {
            Err(violation(format!("is the public address {ip}")))
        };
    }
    let addresses = lookup(host.to_owned()).await?;
    if addresses.is_empty() {
        return Err(violation("resolved to no address".into()));
    }
    if let Some(public) = addresses.iter().find(|a| !network::is_private(**a)) {
        return Err(violation(format!("resolved to public address {public}")));
    }
    Ok(addresses)
}

async fn system_lookup(host: String) -> std::io::Result<Vec<IpAddr>> {
    let mut ips: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), 0))
        .await?
        .map(|a| a.ip())
        .collect();
    ips.dedup();
    Ok(ips)
}

/// DNS resolver for plain-HTTP accounts.
#[derive(Debug, Clone, Copy, Default)]
pub struct PrivateOnlyResolver;

impl ResolveDns for PrivateOnlyResolver {
    fn resolve_dns<'a>(&'a self, name: &'a str) -> DnsFuture<'a> {
        DnsFuture::new(async move {
            resolve_private(name, system_lookup)
                .await
                .map_err(ResolveDnsError::new)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|s| s.parse().expect("ip")).collect()
    }

    async fn run(host: &str, answer: &[&str]) -> Result<Vec<IpAddr>, String> {
        let answer = ips(answer);
        resolve_private(host, |_| async move { Ok(answer) })
            .await
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn accepts_private_answers() {
        assert_eq!(
            run("minio.internal", &["10.0.0.5", "fd00::5"])
                .await
                .expect("ok"),
            ips(&["10.0.0.5", "fd00::5"])
        );
        assert_eq!(
            run("10.1.1.1", &[]).await.expect("literal"),
            ips(&["10.1.1.1"])
        );
    }

    #[tokio::test]
    async fn rejects_public_or_mixed_answers() {
        let e = run("minio.internal", &["10.0.0.5", "8.8.8.8"])
            .await
            .unwrap_err();
        assert!(e.contains("resolved to public address 8.8.8.8"), "{e}");
        assert!(run("x", &[]).await.unwrap_err().contains("no address"));
        assert!(
            run("1.1.1.1", &[])
                .await
                .unwrap_err()
                .contains("public address 1.1.1.1")
        );
    }

    #[tokio::test]
    async fn violation_survives_the_resolver_error_wrapper() {
        let err = ResolveDnsError::new(run_err().await);
        let source = std::error::Error::source(&err).expect("source");
        assert!(source.downcast_ref::<PrivateNetworkViolation>().is_some());
    }

    async fn run_err() -> BoxError {
        resolve_private("8.8.4.4", |_| async { Ok(Vec::new()) })
            .await
            .unwrap_err()
    }
}
