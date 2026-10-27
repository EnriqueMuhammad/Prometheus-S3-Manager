//! Wire-level checks against a minimal fake S3 endpoint that records requests.
//! These run without Docker: they verify what the client sends, not server behaviour.

#![allow(clippy::expect_used, missing_docs)]

use std::sync::{Arc, Mutex};

use s3m_core::account::{MemoryRepository, MemorySecrets};
use s3m_core::{AccountDraft, AccountService, AccountType, HeaderEdit};
use s3m_s3::StorageService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Starts a server answering HEAD with an 8-byte object and PUT (CopyObject) with success.
/// Returns its port and the recorded request heads (lower-cased).
async fn fake_s3() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    // One request per read cycle: wait for the end of the header block.
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("end") + 4;
                    let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    let body_len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < end + body_len {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&buf[end..end + body_len]).into_owned();
                    buf.drain(..end + body_len);
                    seen.lock()
                        .expect("lock")
                        .push(format!("{head}{}", body.to_lowercase()));
                    let config_write = head.starts_with("delete ")
                        || (head.starts_with("put ") && !head.contains("x-amz-copy-source"));
                    let response = if config_write {
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned()
                    } else if head.starts_with("head ") {
                        "HTTP/1.1 200 OK\r\nContent-Length: 8\r\nContent-Type: text/plain\r\nETag: \"abc\"\r\n\r\n".to_owned()
                    } else {
                        let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CopyObjectResult><ETag>\"abc\"</ETag><LastModified>2026-10-17T00:00:00.000Z</LastModified></CopyObjectResult>";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, log)
}

#[tokio::test(flavor = "multi_thread")]
async fn header_edit_sends_replace_with_standard_headers() {
    let (port, log) = fake_s3().await;
    let secrets = Arc::new(MemorySecrets::default());
    let accounts = AccountService::new(Box::new(MemoryRepository::default()), secrets.clone());
    let account = accounts
        .save(&AccountDraft {
            name: "fake".into(),
            kind: AccountType::S3Compatible,
            endpoint_host: format!("127.0.0.1:{port}"),
            use_tls: false,
            region: "us-east-1".into(),
            access_key_id: "AKID".into(),
            secret_access_key: Some("secret".into()),
            force_path_style: true,
            ..AccountDraft::default()
        })
        .expect("account");
    let storage = StorageService::with_secrets(secrets);
    let headers = HeaderEdit {
        content_type: "text/markdown".into(),
        cache_control: "max-age=60".into(),
        content_disposition: "attachment".into(),
        metadata: vec![("Owner".into(), "finance".into())],
        ..HeaderEdit::default()
    };
    storage
        .update_headers(&account, "bkt", "docs/read me.md", &headers)
        .await
        .expect("update headers");

    let log = log.lock().expect("lock");
    let put = log
        .iter()
        .find(|h| h.starts_with("put "))
        .expect("CopyObject request");
    assert!(put.starts_with("put /bkt/docs/read%20me.md?"), "{put}");
    for expected in [
        "x-amz-copy-source: bkt/docs/read%20me.md",
        "x-amz-metadata-directive: replace",
        "content-type: text/markdown",
        "cache-control: max-age=60",
        "content-disposition: attachment",
        "x-amz-meta-owner: finance",
    ] {
        assert!(put.contains(expected), "missing {expected:?} in:\n{put}");
    }
}

async fn fake_account() -> (StorageService, s3m_core::Account, Arc<Mutex<Vec<String>>>) {
    let (port, log) = fake_s3().await;
    let secrets = Arc::new(MemorySecrets::default());
    let accounts = AccountService::new(Box::new(MemoryRepository::default()), secrets.clone());
    let account = accounts
        .save(&AccountDraft {
            name: "fake".into(),
            kind: AccountType::AmazonS3,
            endpoint_host: format!("127.0.0.1:{port}"),
            use_tls: false,
            region: "us-east-1".into(),
            access_key_id: "AKID".into(),
            secret_access_key: Some("secret".into()),
            force_path_style: true,
            ..AccountDraft::default()
        })
        .expect("account");
    (StorageService::with_secrets(secrets), account, log)
}

fn last(log: &Arc<Mutex<Vec<String>>>) -> String {
    log.lock()
        .expect("lock")
        .last()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn bucket_settings_requests() {
    let (storage, account, log) = fake_account().await;

    storage
        .set_versioning(&account, "bkt", true)
        .await
        .expect("versioning");
    let r = last(&log);
    assert!(r.starts_with("put /bkt/?versioning"), "{r}");
    assert!(r.contains("<status>enabled</status>"), "{r}");

    let lifecycle = s3m_core::admin::parse_lifecycle(
        r#"[{"Id":"old","Enabled":true,"Prefix":"logs/","ExpirationDays":30,"NoncurrentExpirationDays":7,"AbortIncompleteUploadDays":2,"TransitionDays":10,"TransitionStorageClass":"GLACIER"}]"#,
    )
    .expect("rules");
    storage
        .set_lifecycle(&account, "bkt", &lifecycle)
        .await
        .expect("lifecycle");
    let r = last(&log);
    assert!(r.starts_with("put /bkt/?lifecycle"), "{r}");
    for part in [
        "<id>old</id>",
        "<filter><prefix>logs/</prefix></filter>",
        "<status>enabled</status>",
        "<expiration><days>30</days></expiration>",
        "<noncurrentdays>7</noncurrentdays>",
        "<daysafterinitiation>2</daysafterinitiation>",
        "<storageclass>glacier</storageclass>",
    ] {
        assert!(r.contains(part), "missing {part} in {r}");
    }

    let cors = s3m_core::admin::parse_cors(s3m_core::admin::CORS_TEMPLATE).expect("cors");
    storage
        .set_cors(&account, "bkt", &cors)
        .await
        .expect("cors");
    let r = last(&log);
    assert!(r.starts_with("put /bkt/?cors"), "{r}");
    assert!(
        r.contains("<allowedorigin>https://example.com</allowedorigin>"),
        "{r}"
    );
    assert!(r.contains("<allowedmethod>head</allowedmethod>"), "{r}");
    storage
        .set_cors(&account, "bkt", &[])
        .await
        .expect("delete cors");
    assert!(last(&log).starts_with("delete /bkt/?cors"));

    storage
        .set_policy(
            &account,
            "bkt",
            Some(&s3m_core::admin::policy_template("bkt")),
        )
        .await
        .expect("policy");
    let r = last(&log);
    assert!(r.starts_with("put /bkt/?policy"), "{r}");
    assert!(r.contains("arn:aws:s3:::bkt/public/*"), "{r}");

    storage
        .set_encryption(
            &account,
            "bkt",
            &s3m_core::admin::Encryption::Kms("key-1".into()),
        )
        .await
        .expect("encryption");
    let r = last(&log);
    assert!(
        r.contains("<ssealgorithm>aws:kms</ssealgorithm>")
            && r.contains("<kmsmasterkeyid>key-1</kmsmasterkeyid>"),
        "{r}"
    );

    let acl = s3m_core::admin::Acl::from_matrix(
        "owner-id",
        None,
        &s3m_core::admin::AclMatrix {
            owner: [true; 4],
            everyone: [true, false, false, false],
            ..Default::default()
        },
        &[],
    );
    storage
        .set_acl(&account, "bkt", s3m_s3::AclTarget::Object("a b.txt"), &acl)
        .await
        .expect("acl");
    let r = last(&log);
    assert!(r.starts_with("put /bkt/a%20b.txt?acl"), "{r}");
    assert!(r.contains("<permission>full_control</permission>"), "{r}");
    assert!(
        r.contains("<uri>http://acs.amazonaws.com/groups/global/allusers</uri>"),
        "{r}"
    );

    storage
        .delete_version(&account, "bkt", "doc.txt", "v-1")
        .await
        .expect("delete version");
    let r = last(&log);
    assert!(
        r.starts_with("delete /bkt/doc.txt?") && r.contains("versionid=v-1 "),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_accounts_go_through_the_manual_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(Mutex::new(String::new()));
    let record = seen.clone();
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            *record.lock().expect("lock") = String::from_utf8_lossy(&buf[..n]).to_lowercase();
            let _ = socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });

    let secrets = Arc::new(MemorySecrets::default());
    let accounts = AccountService::new(Box::new(MemoryRepository::default()), secrets.clone());
    let account = accounts
        .save(&AccountDraft {
            name: "behind proxy".into(),
            kind: AccountType::S3Compatible,
            endpoint_host: "s3.example.test".into(),
            use_tls: true,
            region: "us-east-1".into(),
            access_key_id: "AKID".into(),
            secret_access_key: Some("secret".into()),
            force_path_style: true,
            ..AccountDraft::default()
        })
        .expect("account");
    let storage = StorageService::with_secrets(secrets);
    storage.clients().set_proxy(s3m_s3::ProxySettings::Manual {
        url: format!("http://127.0.0.1:{port}"),
        user: "me".into(),
        password: "pw".into(),
        no_proxy: String::new(),
    });
    let result = storage.test_connection(&account).await;
    assert!(!result.ok);
    let request = seen.lock().expect("lock").clone();
    assert!(
        request.starts_with("connect s3.example.test:443 "),
        "{request}"
    );
    // base64("me:pw")
    assert!(
        request.contains("proxy-authorization: basic bwu6chc="),
        "{request}"
    );
}
