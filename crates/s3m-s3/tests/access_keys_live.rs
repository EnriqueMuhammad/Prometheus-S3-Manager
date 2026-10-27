//! Live tests of access key management against the MinIO of `dev/docker-compose.yml`.
//! Skipped unless `S3M_IT=1` and `S3M_MINIO=1`.
//!
//! ```text
//! docker compose -f dev/docker-compose.yml --profile minio up -d minio
//! S3M_IT=1 S3M_MINIO=1 cargo test -p s3m-s3 --test access_keys_live
//! ```

#![allow(clippy::expect_used, missing_docs)]

use std::sync::Arc;

use s3m_core::account::{MemoryRepository, MemorySecrets};
use s3m_core::minio::{self, AccessKeyUpdate, NewAccessKey};
use s3m_core::{Account, AccountDraft, AccountService, AccountType};
use s3m_s3::{StorageErrorKind, StorageService};
use uuid::Uuid;

const ENDPOINT: &str = "127.0.0.1:59100";
const ROOT: &str = "s3m-test";
const ROOT_SECRET: &str = "s3m-test-secret";

fn enabled() -> bool {
    let on = |name| std::env::var(name).is_ok_and(|v| v == "1");
    on("S3M_IT") && on("S3M_MINIO")
}

/// A saved account for `access`/`secret` and a storage service using it.
fn account(access: &str, secret: &str) -> (StorageService, Account) {
    let secrets = Arc::new(MemorySecrets::default());
    let accounts = AccountService::new(Box::new(MemoryRepository::default()), secrets.clone());
    let account = accounts
        .save(&AccountDraft {
            name: format!("MinIO {access}"),
            kind: AccountType::S3Compatible,
            endpoint_host: ENDPOINT.into(),
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

#[tokio::test]
async fn access_keys_can_be_created_restricted_changed_and_deleted() {
    if !enabled() {
        eprintln!("skipped: set S3M_IT=1 and S3M_MINIO=1");
        return;
    }
    let (root, root_account) = account(ROOT, ROOT_SECRET);
    let (user, policy) = root
        .account_policy(&root_account)
        .await
        .expect("account info");
    assert_eq!(user, ROOT);
    eprintln!("root policy: {} bytes", policy.len());

    let tag = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let allowed = format!("allowed-{tag}");
    let denied = format!("denied-{tag}");
    for b in [&allowed, &denied] {
        root.create_bucket(&root_account, b, "us-east-1")
            .await
            .expect("bucket");
    }

    // Create a key restricted to the "allowed-*" buckets.
    let new = NewAccessKey {
        access_key: minio::random_access_key(),
        secret_key: minio::random_secret_key(),
        policy: Some(
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:*"],
                "Resource":["arn:aws:s3:::allowed-*","arn:aws:s3:::allowed-*/*"]}]}"#
                .into(),
        ),
        name: "build server".into(),
        description: "live test".into(),
        expiration: Some(chrono::Utc::now() + chrono::Duration::days(30)),
    };
    let (ak, sk) = root
        .create_access_key(&root_account, &new)
        .await
        .expect("create");
    assert_eq!(
        (ak.as_str(), sk.as_str()),
        (new.access_key.as_str(), new.secret_key.as_str())
    );

    let keys = root.list_access_keys(&root_account).await.expect("list");
    let listed = keys.iter().find(|k| k.access_key == ak).expect("listed");
    assert_eq!(listed.name, "build server");
    assert!(listed.enabled());
    assert!(listed.expiration.is_some());

    let info = root
        .access_key_info(&root_account, &ak)
        .await
        .expect("info");
    assert!(info.key.restricted);
    assert!(info.policy.contains("allowed-*"), "{}", info.policy);
    assert_eq!(info.key.description, "live test");

    // The key works, but only inside its policy.
    let (as_key, key_account) = account(&ak, &sk);
    as_key
        .list_objects(&key_account, &allowed, "", None)
        .await
        .expect("allowed bucket");
    let denied_err = as_key
        .list_objects(&key_account, &denied, "", None)
        .await
        .expect_err("denied bucket");
    assert_eq!(
        denied_err.kind,
        StorageErrorKind::AccessDenied,
        "{denied_err}"
    );

    // Drop the restriction and disable the key.
    root.update_access_key(
        &root_account,
        &ak,
        &AccessKeyUpdate {
            policy: Some(None),
            enabled: Some(false),
            name: Some("renamed".into()),
            ..AccessKeyUpdate::default()
        },
    )
    .await
    .expect("update");
    let info = root
        .access_key_info(&root_account, &ak)
        .await
        .expect("info");
    assert!(!info.key.restricted, "policy still set: {}", info.policy);
    assert!(!info.key.enabled());
    assert_eq!(info.key.name, "renamed");

    root.delete_access_key(&root_account, &ak)
        .await
        .expect("delete");
    let keys = root.list_access_keys(&root_account).await.expect("list");
    assert!(keys.iter().all(|k| k.access_key != ak));
    assert!(root.access_key_info(&root_account, &ak).await.is_err());

    for b in [&allowed, &denied] {
        root.delete_bucket(&root_account, b, true)
            .await
            .expect("cleanup");
    }
}

#[tokio::test]
async fn wrong_keys_and_servers_give_clear_errors() {
    if !enabled() {
        return;
    }
    let (svc, bad) = account(ROOT, "not-the-secret");
    let e = svc.list_access_keys(&bad).await.expect_err("bad secret");
    assert!(
        matches!(
            e.kind,
            StorageErrorKind::AccessDenied | StorageErrorKind::InvalidCredentials
        ),
        "{e:?}"
    );
}
