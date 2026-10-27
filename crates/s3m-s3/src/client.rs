//! The only place accounts become S3 clients.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedAsyncSleep, SharedHttpClient,
};
use aws_smithy_async::rt::sleep::TokioSleep;
use aws_smithy_http_client::tls::{self, TlsContext, TrustStore, rustls_provider::CryptoMode};
use s3m_core::{Account, AccountType, SecretStore, ca, endpoint};
use uuid::Uuid;

use crate::error::{StorageError, StorageErrorKind};
use crate::guard::PrivateOnlyResolver;
use crate::proxy::{self, ProxySettings};

/// Builds and caches one client per account; a client is rebuilt when any
/// connection-relevant setting changes.
pub struct ClientProvider {
    secrets: Arc<dyn SecretStore>,
    cache: Mutex<HashMap<Uuid, (u64, Client)>>,
    proxy: Mutex<ProxySettings>,
}

impl ClientProvider {
    /// Provider reading secrets from `secrets`.
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            secrets,
            cache: Mutex::default(),
            proxy: Mutex::default(),
        }
    }

    /// Current proxy settings.
    pub fn proxy(&self) -> ProxySettings {
        self.proxy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Changes the proxy; clients are rebuilt on next use.
    pub fn set_proxy(&self, settings: ProxySettings) {
        *self.proxy.lock().unwrap_or_else(PoisonError::into_inner) = settings;
        self.invalidate();
    }

    /// Cached client for a saved account.
    pub fn get(&self, account: &Account) -> Result<Client, StorageError> {
        let proxy = self.proxy();
        let key = cache_key(account, &proxy);
        if let Some((k, client)) = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&account.id)
            && *k == key
        {
            return Ok(client.clone());
        }
        let secret = self.secret(account)?;
        let client = build(account, &secret, &proxy)?;
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.id, (key, client.clone()));
        Ok(client)
    }

    /// The account's stored secret key.
    pub(crate) fn secret(&self, account: &Account) -> Result<String, StorageError> {
        self.secrets
            .get(account.id)
            .map_err(|e| StorageError::new(StorageErrorKind::InvalidCredentials, e.to_string()))?
            .ok_or_else(|| {
                StorageError::new(
                    StorageErrorKind::InvalidCredentials,
                    format!(
                        "Account \"{}\" has no stored Secret Access Key. Edit the account and enter it.",
                        account.name
                    ),
                )
            })
    }

    /// Drops every cached client.
    pub fn invalidate(&self) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn cache_key(a: &Account, proxy: &ProxySettings) -> u64 {
    let mut h = DefaultHasher::new();
    proxy.hash(&mut h);
    (
        &a.endpoint_host,
        a.use_tls,
        &a.region,
        a.force_path_style,
        &a.access_key_id,
        a.kind == AccountType::AmazonS3,
        &a.custom_ca_pem,
        a.updated_at,
    )
        .hash(&mut h);
    h.finish()
}

/// Builds an uncached client (also used by "Test connection" before the account is saved).
pub fn build(
    account: &Account,
    secret: &str,
    proxy: &ProxySettings,
) -> Result<Client, StorageError> {
    endpoint::validate(&account.endpoint_host, account.use_tls).map_err(StorageError::invalid)?;
    let http = http_client(account, proxy)?;
    let mut config = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .credentials_provider(Credentials::new(
            account.access_key_id.clone(),
            secret.to_owned(),
            None,
            None,
            "prometheus-s3manager",
        ))
        .region(Region::new(account.region.clone()))
        .force_path_style(account.force_path_style)
        .http_client(http)
        .sleep_impl(SharedAsyncSleep::new(TokioSleep::new()))
        .retry_config(RetryConfig::standard().with_max_attempts(4))
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(300))
                .build(),
        );
    let aws_default = account.kind == AccountType::AmazonS3
        && account.use_tls
        && account
            .endpoint_host
            .eq_ignore_ascii_case("s3.amazonaws.com");
    if !aws_default {
        config = config.endpoint_url(account.endpoint_url());
    }
    if account.kind == AccountType::S3Compatible {
        // Newer SDK checksum defaults break many MinIO/Ceph/gateway versions.
        config = config
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired);
    }
    Ok(Client::from_conf(config.build()))
}

/// TLS: system roots + the account's custom CA. HTTP: every connection goes through the
/// private-network DNS guard.
fn http_client(account: &Account, proxy: &ProxySettings) -> Result<SharedHttpClient, StorageError> {
    let mut trust = TrustStore::default();
    if !account.custom_ca_pem.trim().is_empty() {
        let certs = ca::parse(&account.custom_ca_pem, chrono::Utc::now()).map_err(|e| {
            StorageError::with_hint(
                StorageErrorKind::Tls,
                e,
                "Edit the account and replace the custom CA.",
            )
        })?;
        for cert in certs {
            trust = trust.with_pem_certificate(ca::to_pem(&cert.der).into_bytes());
        }
    }
    let context = TlsContext::builder()
        .with_trust_store(trust)
        .build()
        .map_err(|e| StorageError::new(StorageErrorKind::Tls, format!("TLS setup failed: {e}")))?;
    if !account.use_tls {
        // Plain HTTP goes to private addresses only, never through a proxy.
        return Ok(aws_smithy_http_client::Builder::new()
            .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
            .tls_context(context)
            .build_with_resolver(PrivateOnlyResolver));
    }
    let proxy_config = proxy::proxy_config(proxy)?;
    if proxy_config.is_disabled() {
        return Ok(aws_smithy_http_client::Builder::new()
            .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
            .tls_context(context)
            .build_https());
    }
    Ok(
        aws_smithy_http_client::Builder::new().build_with_connector_fn(
            move |settings, components| {
                let mut builder = aws_smithy_http_client::Connector::builder();
                builder.set_connector_settings(settings.cloned());
                if let Some(components) = components {
                    builder.set_sleep_impl(components.sleep_impl());
                }
                builder
                    .proxy_config(proxy_config.clone())
                    .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
                    .tls_context(context.clone())
                    .build()
            },
        ),
    )
}
