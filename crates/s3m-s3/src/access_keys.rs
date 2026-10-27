//! Access keys (service accounts) through the MinIO admin API.
//!
//! Admin requests reuse the account's S3 client, so they get the same TLS trust, private-network
//! guard, proxy and SigV4 signing: a ListBuckets call is rewritten to the admin endpoint before
//! it is signed, and the raw response is captured instead of being parsed as S3.

use std::sync::{Arc, Mutex, PoisonError};

use aws_sdk_s3::config::retry::RetryConfig;
use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::interceptors::Intercept;
use aws_smithy_runtime_api::client::interceptors::context::AfterDeserializationInterceptorContextRef;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::config_bag::ConfigBag;
use s3m_core::Account;
use s3m_core::minio::{
    self, ADMIN_PREFIX, AccessKey, AccessKeyInfo, AccessKeyUpdate, NewAccessKey,
};

use crate::error::{StorageError, StorageErrorKind};
use crate::service::StorageService;

/// Status and body of an admin response.
type Captured = Arc<Mutex<Option<(u16, Vec<u8>)>>>;

/// Keeps the raw response of the rewritten call.
#[derive(Debug, Clone, Default)]
struct Capture(Captured);

impl Intercept for Capture {
    fn name(&self) -> &'static str {
        "AdminResponseCapture"
    }

    fn read_after_deserialization(
        &self,
        context: &AfterDeserializationInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let response = context.response();
        let body = response.body().bytes().unwrap_or_default().to_vec();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((response.status().as_u16(), body));
        Ok(())
    }
}

/// `scheme://authority` of a URI.
fn origin(uri: &str) -> &str {
    let start = uri.find("://").map_or(0, |i| i + 3);
    uri[start..].find('/').map_or(uri, |i| &uri[..start + i])
}

/// Turns an admin error response into a storage error.
fn admin_error(status: u16, body: &[u8]) -> StorageError {
    let message = minio::error_message(body)
        .unwrap_or_else(|| format!("The server answered with HTTP status {status}."));
    match status {
        404 | 400 if minio::error_message(body).is_none() => StorageError::with_hint(
            StorageErrorKind::InvalidRequest,
            "This server does not offer the MinIO admin API.",
            "Access keys can only be managed on MinIO servers.",
        ),
        401 | 403 => StorageError::with_hint(
            StorageErrorKind::AccessDenied,
            message,
            "The account's user needs the admin:CreateServiceAccount, admin:ListServiceAccounts, admin:UpdateServiceAccount or admin:RemoveServiceAccount permission.",
        ),
        404 => StorageError::new(StorageErrorKind::NotFound, message),
        500.. => StorageError::new(StorageErrorKind::Server, message),
        _ => StorageError::new(StorageErrorKind::InvalidRequest, message),
    }
}

impl StorageService {
    /// One admin API call; returns the body of a successful response.
    async fn admin_call(
        &self,
        account: &Account,
        method: &'static str,
        path_and_query: String,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, StorageError> {
        let client = self.clients().get(account)?;
        let capture = Capture::default();
        let target = format!("{ADMIN_PREFIX}{path_and_query}");
        let result = client
            .list_buckets()
            .customize()
            // Creating a key is not idempotent.
            .config_override(aws_sdk_s3::Config::builder().retry_config(RetryConfig::disabled()))
            .mutate_request(move |req| {
                let uri = format!("{}{target}", origin(req.uri()));
                if req.set_uri(uri).is_err() || req.set_method(method).is_err() {
                    tracing::warn!("could not rewrite the admin request");
                }
                *req.body_mut() = SdkBody::from(body.clone());
            })
            .interceptor(capture.clone())
            .send()
            .await;
        let captured = capture
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match (captured, result) {
            (Some((status, body)), _) if (200..300).contains(&status) => Ok(body),
            (Some((status, body)), _) => Err(admin_error(status, &body)),
            (None, Err(e)) => Err(StorageError::from_sdk(e)),
            (None, Ok(_)) => Err(StorageError::new(
                StorageErrorKind::Unknown,
                "The server sent no response.",
            )),
        }
    }

    /// Decrypts a response encrypted with the account's secret key.
    fn decrypt(&self, account: &Account, body: &[u8]) -> Result<Vec<u8>, StorageError> {
        let secret = self.clients().secret(account)?;
        minio::decrypt_data(&secret, body)
            .map_err(|e| StorageError::new(StorageErrorKind::Unknown, e))
    }

    /// Encrypts a request body with the account's secret key.
    fn encrypt(&self, account: &Account, body: &[u8]) -> Result<Vec<u8>, StorageError> {
        let secret = self.clients().secret(account)?;
        minio::encrypt_data(&secret, body)
            .map_err(|e| StorageError::new(StorageErrorKind::Unknown, e))
    }

    /// The signed-in user's name and policy (pretty JSON; empty when the server sends none).
    pub async fn account_policy(
        &self,
        account: &Account,
    ) -> Result<(String, String), StorageError> {
        let body = self
            .admin_call(account, "GET", "/accountinfo".into(), Vec::new())
            .await?;
        minio::parse_account_info(&body)
            .map_err(|e| StorageError::new(StorageErrorKind::Unknown, e))
    }

    /// The access keys of the signed-in user.
    pub async fn list_access_keys(
        &self,
        account: &Account,
    ) -> Result<Vec<AccessKey>, StorageError> {
        let body = self
            .admin_call(account, "GET", "/list-service-accounts".into(), Vec::new())
            .await?;
        let json = self.decrypt(account, &body)?;
        minio::parse_list(&json).map_err(|e| StorageError::new(StorageErrorKind::Unknown, e))
    }

    /// Details and policy of one access key.
    pub async fn access_key_info(
        &self,
        account: &Account,
        access_key: &str,
    ) -> Result<AccessKeyInfo, StorageError> {
        let path = format!(
            "/info-service-account?accessKey={}",
            minio::query_value(access_key)
        );
        let body = self.admin_call(account, "GET", path, Vec::new()).await?;
        let json = self.decrypt(account, &body)?;
        minio::parse_info(access_key, &json)
            .map_err(|e| StorageError::new(StorageErrorKind::Unknown, e))
    }

    /// Creates an access key; returns the access and secret key the server stored.
    pub async fn create_access_key(
        &self,
        account: &Account,
        key: &NewAccessKey,
    ) -> Result<(String, String), StorageError> {
        let json = key.request_json().map_err(StorageError::invalid)?;
        let body = self.encrypt(account, &json)?;
        let response = self
            .admin_call(account, "PUT", "/add-service-account".into(), body)
            .await?;
        let json = self.decrypt(account, &response)?;
        minio::parse_created(&json).map_err(|e| StorageError::new(StorageErrorKind::Unknown, e))
    }

    /// Changes an access key.
    pub async fn update_access_key(
        &self,
        account: &Account,
        access_key: &str,
        update: &AccessKeyUpdate,
    ) -> Result<(), StorageError> {
        let json = update.request_json().map_err(StorageError::invalid)?;
        let body = self.encrypt(account, &json)?;
        let path = format!(
            "/update-service-account?accessKey={}",
            minio::query_value(access_key)
        );
        self.admin_call(account, "POST", path, body).await?;
        Ok(())
    }

    /// Deletes an access key.
    pub async fn delete_access_key(
        &self,
        account: &Account,
        access_key: &str,
    ) -> Result<(), StorageError> {
        let path = format!(
            "/delete-service-account?accessKey={}",
            minio::query_value(access_key)
        );
        self.admin_call(account, "DELETE", path, Vec::new()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use s3m_core::account::{MemoryRepository, MemorySecrets};
    use s3m_core::minio::{self, NewAccessKey};
    use s3m_core::{Account, AccountDraft, AccountService, AccountType};

    use super::origin;
    use crate::service::StorageService;

    fn account(access: &str, secret: &str) -> (StorageService, Account) {
        let secrets = Arc::new(MemorySecrets::default());
        let accounts = AccountService::new(Box::new(MemoryRepository::default()), secrets.clone());
        let account = accounts
            .save(&AccountDraft {
                name: access.into(),
                kind: AccountType::S3Compatible,
                endpoint_host: "127.0.0.1:59100".into(),
                use_tls: false,
                region: "us-east-1".into(),
                access_key_id: access.into(),
                secret_access_key: Some(secret.into()),
                force_path_style: true,
                ..AccountDraft::default()
            })
            .expect("account");
        (StorageService::with_secrets(secrets), account)
    }

    /// A regular user (no admin permissions) manages its own keys. Live: needs the MinIO of
    /// `dev/docker-compose.yml` and `S3M_IT=1 S3M_MINIO=1`.
    #[tokio::test]
    #[allow(clippy::expect_used)]
    async fn regular_users_manage_their_own_keys() {
        let on = |name| std::env::var(name).is_ok_and(|v| v == "1");
        if !(on("S3M_IT") && on("S3M_MINIO")) {
            return;
        }
        let (root, root_account) = account("s3m-test", "s3m-test-secret");
        let user = format!("user-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let secret = "user-secret-123";
        let body = root
            .encrypt(
                &root_account,
                br#"{"secretKey":"user-secret-123","status":"enabled"}"#,
            )
            .expect("encrypt");
        root.admin_call(
            &root_account,
            "PUT",
            format!("/add-user?accessKey={user}"),
            body,
        )
        .await
        .expect("add user");
        root.admin_call(
            &root_account,
            "PUT",
            format!(
                "/set-user-or-group-policy?policyName=readwrite&userOrGroup={user}&isGroup=false"
            ),
            Vec::new(),
        )
        .await
        .expect("attach policy");

        let (svc, me) = account(&user, secret);
        let (name, policy) = svc.account_policy(&me).await.expect("account info");
        assert_eq!(name, user);
        assert!(policy.contains("s3:*"), "{policy}");
        let new = NewAccessKey {
            access_key: minio::random_access_key(),
            secret_key: minio::random_secret_key(),
            policy: Some(policy),
            name: "mine".into(),
            ..NewAccessKey::default()
        };
        svc.create_access_key(&me, &new)
            .await
            .expect("create own key");
        let keys = svc.list_access_keys(&me).await.expect("list own keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].parent_user, user);
        svc.delete_access_key(&me, &new.access_key)
            .await
            .expect("delete own key");

        root.admin_call(
            &root_account,
            "DELETE",
            format!("/remove-user?accessKey={user}"),
            Vec::new(),
        )
        .await
        .expect("remove user");
    }

    #[test]
    fn origin_of_uris() {
        assert_eq!(
            origin("https://s3.example.com/?x-id=ListBuckets"),
            "https://s3.example.com"
        );
        assert_eq!(origin("http://10.0.0.5:9000/"), "http://10.0.0.5:9000");
        assert_eq!(origin("http://10.0.0.5:9000"), "http://10.0.0.5:9000");
    }
}
