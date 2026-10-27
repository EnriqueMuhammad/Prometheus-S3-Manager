//! Live tests against the MinIO of `dev/docker-compose.yml`. Skipped unless `S3M_IT=1`.
//!
//! ```text
//! docker compose -f dev/docker-compose.yml up -d
//! S3M_IT=1 cargo test -p s3m-s3 --test live
//! ```

#![allow(clippy::expect_used, missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use s3m_core::account::{MemoryRepository, MemorySecrets};
use s3m_core::transfer::MemoryTransferStore;
use s3m_core::{
    Account, AccountDraft, AccountService, AccountType, TransferDirection, TransferOptions,
    TransferQueue, TransferRequest, TransferState,
};
use s3m_s3::{S3TransferExecutor, StorageErrorKind, StorageService};
use uuid::Uuid;

const ENDPOINT: &str = "127.0.0.1:59000";
const ACCESS: &str = "s3m-test";
const SECRET: &str = "s3m-test-secret";

fn enabled() -> bool {
    std::env::var("S3M_IT").is_ok_and(|v| v == "1")
}

fn draft(endpoint: &str, secret: &str) -> AccountDraft {
    AccountDraft {
        name: "Local MinIO".into(),
        kind: AccountType::S3Compatible,
        endpoint_host: endpoint.into(),
        use_tls: false,
        region: "us-east-1".into(),
        access_key_id: ACCESS.into(),
        secret_access_key: Some(secret.into()),
        force_path_style: true,
        ..AccountDraft::default()
    }
}

struct Env {
    accounts: Arc<AccountService>,
    storage: Arc<StorageService>,
    account: Account,
}

fn env() -> Env {
    let secrets = Arc::new(MemorySecrets::default());
    let accounts = Arc::new(AccountService::new(
        Box::new(MemoryRepository::default()),
        secrets.clone(),
    ));
    let account = accounts.save(&draft(ENDPOINT, SECRET)).expect("account");
    let storage = Arc::new(StorageService::with_secrets(secrets));
    Env {
        accounts,
        storage,
        account,
    }
}

async fn settle(queue: &TransferQueue) {
    for _ in 0..600 {
        if queue.active_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let states: Vec<_> = queue
        .jobs()
        .iter()
        .map(|j| {
            format!(
                "{} {:?} {}/{} attempts={} {:?}",
                j.key, j.state, j.bytes_done, j.bytes_total, j.attempts, j.error
            )
        })
        .collect();
    panic!("transfers did not finish: {states:#?}");
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("s3m-live-{tag}-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn browse_transfer_rename_delete() {
    if !enabled() {
        return;
    }
    let env = env();
    let client = env.storage.clients().get(&env.account).expect("client");
    let bucket = format!("s3m-it-{}", &Uuid::new_v4().simple().to_string()[..12]);
    client
        .create_bucket()
        .bucket(&bucket)
        .send()
        .await
        .expect("create bucket");

    let test = env.storage.test_connection(&env.account).await;
    assert!(test.ok, "{}", test.message);
    let listing = env
        .storage
        .list_buckets(&env.account)
        .await
        .expect("buckets");
    assert!(!listing.restricted);
    assert!(listing.buckets.iter().any(|b| b.name == bucket));

    env.storage
        .create_folder(&env.account, &bucket, "", "docs")
        .await
        .expect("folder");
    let root = env
        .storage
        .list_objects(&env.account, &bucket, "", None)
        .await
        .expect("root");
    assert!(root.entries.iter().any(|e| e.is_folder && e.key == "docs/"));

    // Uploads: one single PUT, one multipart (3 parts).
    let src = temp_dir("src");
    let small: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
    let large: Vec<u8> = (0..(20 * 1024 * 1024u32))
        .map(|i| (i.wrapping_mul(31) % 253) as u8)
        .collect();
    std::fs::write(src.join("small.txt"), &small).expect("write");
    std::fs::write(src.join("large.bin"), &large).expect("write");

    let executor = Arc::new(S3TransferExecutor::new(
        env.accounts.clone(),
        env.storage.clients().clone(),
    ));
    let queue = TransferQueue::new(
        executor,
        Arc::new(MemoryTransferStore::default()),
        TransferOptions::default(),
        tokio::runtime::Handle::current(),
        Arc::new(|_| {}),
    );
    let uploads =
        s3m_core::uploads::collect(env.account.id, &bucket, "docs/", std::slice::from_ref(&src));
    assert_eq!(uploads.len(), 2);
    let folder_name = src
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    queue.enqueue(uploads);
    settle(&queue).await;
    for job in queue.jobs() {
        assert_eq!(
            job.state,
            TransferState::Completed,
            "{}: {:?}",
            job.key,
            job.error
        );
    }

    let prefix = format!("docs/{folder_name}/");
    let page = env
        .storage
        .list_objects(&env.account, &bucket, &prefix, None)
        .await
        .expect("docs");
    let mut sizes: Vec<_> = page
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.size))
        .collect();
    sizes.sort();
    assert_eq!(
        sizes,
        vec![
            ("large.bin".into(), large.len() as u64),
            ("small.txt".into(), small.len() as u64)
        ]
    );
    let details = env
        .storage
        .object_details(&env.account, &bucket, &format!("{prefix}large.bin"))
        .await
        .expect("head");
    assert_eq!(details.size, large.len() as u64);

    // Downloads, then a second download that must not overwrite.
    let dst = temp_dir("dst");
    let downloads: Vec<TransferRequest> = page
        .entries
        .iter()
        .map(|e| TransferRequest {
            account_id: env.account.id,
            direction: TransferDirection::Download,
            bucket: bucket.clone(),
            key: e.key.clone(),
            local_path: s3m_core::paths::map(&dst, &e.key, &prefix).expect("map"),
            size_hint: e.size,
            source: None,
            version_id: None,
        })
        .collect();
    queue.clear_finished();
    queue.enqueue(downloads.clone());
    settle(&queue).await;
    for job in queue.jobs() {
        assert_eq!(
            job.state,
            TransferState::Completed,
            "{}: {:?}",
            job.key,
            job.error
        );
    }
    assert_eq!(std::fs::read(dst.join("small.txt")).expect("small"), small);
    assert_eq!(std::fs::read(dst.join("large.bin")).expect("large"), large);
    queue.clear_finished();
    queue.enqueue(downloads);
    settle(&queue).await;
    for job in queue.jobs() {
        assert_eq!(job.state, TransferState::Failed);
        assert!(
            job.error
                .as_deref()
                .unwrap_or("")
                .contains("never overwritten")
        );
        assert_eq!(job.attempts, 1, "a local conflict is not retried");
    }
    let leftovers: Vec<_> = std::fs::read_dir(&dst)
        .expect("dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".s3m-partial"))
        .collect();
    assert!(leftovers.is_empty(), "partial files left behind");

    // Rename (copy + delete) with a key that needs URL encoding.
    let renamed = format!("{prefix}renamed + spaced é.txt");
    env.storage
        .rename_object(
            &env.account,
            &bucket,
            &format!("{prefix}small.txt"),
            &renamed,
        )
        .await
        .expect("rename");
    let page = env
        .storage
        .list_objects(&env.account, &bucket, &prefix, None)
        .await
        .expect("after rename");
    let names: Vec<_> = page.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"renamed + spaced é.txt"), "{names:?}");
    assert!(!names.contains(&"small.txt"));

    // Delete everything, then the bucket.
    let keys: Vec<String> = page
        .entries
        .iter()
        .map(|e| e.key.clone())
        .chain(["docs/".to_owned()])
        .collect();
    let deleted = env
        .storage
        .delete_objects(&env.account, &bucket, &keys)
        .await
        .expect("delete");
    assert_eq!(deleted, keys.len());
    let root = env
        .storage
        .list_objects(&env.account, &bucket, "", None)
        .await
        .expect("empty");
    // Some servers (SeaweedFS) keep emptied directories as folder entries; no files may remain.
    assert!(
        root.entries.iter().all(|e| e.is_folder),
        "{:?}",
        root.entries
    );
    let _ = client.delete_bucket().bucket(&bucket).send().await;
    queue.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dst);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_and_missing_bucket() {
    if !enabled() {
        return;
    }
    let env = env();
    let mut bad = env.account.clone();
    bad.id = Uuid::new_v4();
    let result = env.storage.test_unsaved(&bad, "not-the-secret").await;
    assert!(!result.ok);
    assert!(
        result.message.contains("Secret Access Key does not match"),
        "{}",
        result.message
    );

    let err = env
        .storage
        .list_objects(&env.account, "s3m-does-not-exist-0000", "", None)
        .await
        .unwrap_err();
    assert_eq!(err.kind, StorageErrorKind::NotFound, "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_http_guard() {
    if !enabled() {
        return;
    }
    let env = env();
    // localhost resolves to loopback: allowed.
    let mut local = env.account.clone();
    local.endpoint_host = "localhost:59000".into();
    let ok = env.storage.test_unsaved(&local, SECRET).await;
    assert!(ok.ok, "{}", ok.message);

    // A public host over plain HTTP is refused before any credentials are sent.
    let resolves = tokio::net::lookup_host(("dns.google", 80)).await.is_ok();
    let mut public = env.account.clone();
    public.endpoint_host = "dns.google".into();
    let refused = env.storage.test_unsaved(&public, SECRET).await;
    assert!(!refused.ok);
    if resolves {
        assert!(
            refused.message.contains("refusing to send credentials"),
            "{}",
            refused.message
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn buckets_copy_move_presign_preview_headers() {
    if !enabled() {
        return;
    }
    let env = env();
    let bucket = format!("s3m-p2-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let other = format!("{bucket}-b");
    assert!(
        env.storage
            .create_bucket(&env.account, "Bad_Name", "")
            .await
            .is_err()
    );
    env.storage
        .create_bucket(&env.account, &bucket, "us-east-1")
        .await
        .expect("create");
    env.storage
        .create_bucket(&env.account, &other, "")
        .await
        .expect("create other");
    let info = env
        .storage
        .bucket_info(&env.account, &bucket)
        .await
        .expect("info");
    assert_eq!(info.name, bucket);

    let client = env.storage.clients().get(&env.account).expect("client");
    let put = |key: &'static str, body: &'static [u8]| {
        let client = client.clone();
        let bucket = bucket.clone();
        async move {
            client
                .put_object()
                .bucket(&bucket)
                .key(key)
                .content_type("text/plain")
                .body(aws_sdk_s3::primitives::ByteStream::from_static(body))
                .send()
                .await
                .expect("put");
        }
    };
    put("src/2026/a.txt", b"hello world").await;
    put("src/2026/sub/b.txt", b"bee").await;
    put("src/readme.md", b"# readme").await;

    let all = env
        .storage
        .list_all(&env.account, &bucket, "src/2026/")
        .await
        .expect("list all");
    let mut keys: Vec<_> = all.iter().map(|o| o.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["src/2026/a.txt", "src/2026/sub/b.txt"]);

    let executor = Arc::new(S3TransferExecutor::new(
        env.accounts.clone(),
        env.storage.clients().clone(),
    ));
    let queue = TransferQueue::new(
        executor,
        Arc::new(MemoryTransferStore::default()),
        TransferOptions::default(),
        tokio::runtime::Handle::current(),
        Arc::new(|_| {}),
    );

    // Copy folder "src/2026/" into the other bucket's "dst/".
    let clip = s3m_core::plan::Clipboard {
        account_id: env.account.id,
        bucket: bucket.clone(),
        keys: vec!["src/2026/".into()],
        cut: false,
    };
    queue.enqueue(s3m_core::plan::paste_requests(
        &clip,
        &all,
        env.account.id,
        &other,
        "dst/",
    ));
    settle(&queue).await;
    for job in queue.jobs() {
        assert_eq!(
            job.state,
            TransferState::Completed,
            "{}: {:?}",
            job.key,
            job.error
        );
    }
    let copied = env
        .storage
        .list_all(&env.account, &other, "dst/")
        .await
        .expect("copied");
    let mut keys: Vec<_> = copied.iter().map(|o| o.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["dst/2026/a.txt", "dst/2026/sub/b.txt"]);

    // Rename folder "src/2026/" to "src/archive/" (moves).
    queue.clear_finished();
    queue.enqueue(s3m_core::plan::rename_folder_requests(
        env.account.id,
        &bucket,
        "src/2026/",
        "src/archive/",
        &all,
    ));
    settle(&queue).await;
    for job in queue.jobs() {
        assert_eq!(
            job.state,
            TransferState::Completed,
            "{}: {:?}",
            job.key,
            job.error
        );
    }
    let moved = env
        .storage
        .list_all(&env.account, &bucket, "src/")
        .await
        .expect("moved");
    let mut keys: Vec<_> = moved.iter().map(|o| o.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "src/archive/a.txt",
            "src/archive/sub/b.txt",
            "src/readme.md"
        ]
    );

    // Deleting a folder tree leaves no folder behind, even on servers with real directories.
    put("tree/a/b/c.txt", b"c").await;
    put("tree/a/d.txt", b"d").await;
    let files = vec!["tree/a/b/c.txt".to_owned(), "tree/a/d.txt".to_owned()];
    let folders = s3m_core::plan::folder_prefixes(&files, &["tree/".to_owned()]);
    let n = env
        .storage
        .delete_tree(&env.account, &bucket, &files, &folders)
        .await
        .expect("delete tree");
    assert_eq!(n, 2);
    let root = env
        .storage
        .list_objects(&env.account, &bucket, "", None)
        .await
        .expect("root");
    assert!(
        !root.entries.iter().any(|e| e.key.starts_with("tree/")),
        "{:?}",
        root.entries
    );

    // Presigned URL and preview.
    let url = env
        .storage
        .presign_get(
            &env.account,
            &bucket,
            "src/archive/a.txt",
            Duration::from_secs(600),
        )
        .await
        .expect("presign");
    assert!(url.starts_with("http://127.0.0.1:59000/"), "{url}");
    assert!(url.contains("X-Amz-Signature="), "{url}");
    if let Ok(out) = std::process::Command::new("curl")
        .args(["-s", &url])
        .output()
    {
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello world");
    }
    assert!(
        env.storage
            .presign_get(
                &env.account,
                &bucket,
                "x",
                Duration::from_secs(8 * 24 * 3600)
            )
            .await
            .is_err()
    );
    let preview = env
        .storage
        .preview(&env.account, &bucket, "src/archive/a.txt", 5)
        .await
        .expect("preview");
    assert_eq!(preview.bytes, b"hello");
    assert_eq!(preview.total, 11);

    // Headers.
    let mut headers = s3m_core::HeaderEdit {
        content_type: "text/markdown".into(),
        cache_control: "max-age=60".into(),
        metadata: vec![("owner".into(), "finance".into())],
        ..s3m_core::HeaderEdit::default()
    };
    env.storage
        .update_headers(&env.account, &bucket, "src/readme.md", &headers)
        .await
        .expect("headers");
    let d = env
        .storage
        .object_details(&env.account, &bucket, "src/readme.md")
        .await
        .expect("head");
    // SeaweedFS replaces user metadata but ignores standard headers on CopyObject REPLACE;
    // tests/wire.rs checks that they are sent.
    assert_eq!(d.metadata.get("owner").map(String::as_str), Some("finance"));
    assert_eq!(d.size, 8, "content survives a header edit");
    headers.metadata.push(("Owner".into(), "dup".into()));
    assert!(
        env.storage
            .update_headers(&env.account, &bucket, "src/readme.md", &headers)
            .await
            .is_err()
    );

    // Delete buckets with their contents.
    let n = env
        .storage
        .delete_bucket(&env.account, &bucket, true)
        .await
        .expect("delete bucket");
    assert!(n >= 3);
    env.storage
        .delete_bucket(&env.account, &other, true)
        .await
        .expect("delete other");
    let listing = env
        .storage
        .list_buckets(&env.account)
        .await
        .expect("buckets");
    assert!(
        !listing
            .buckets
            .iter()
            .any(|b| b.name == bucket || b.name == other)
    );
    queue.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_resume_upload_and_download() {
    if !enabled() {
        return;
    }
    let env = env();
    let bucket = format!("s3m-p3-{}", &Uuid::new_v4().simple().to_string()[..12]);
    env.storage
        .create_bucket(&env.account, &bucket, "")
        .await
        .expect("create");

    let executor = Arc::new(S3TransferExecutor::new(
        env.accounts.clone(),
        env.storage.clients().clone(),
    ));
    let limiter = executor.limiter().clone();
    let store = Arc::new(MemoryTransferStore::default());
    let queue = TransferQueue::new(
        executor,
        store.clone(),
        TransferOptions::default(),
        tokio::runtime::Handle::current(),
        Arc::new(|_| {}),
    );

    // Upload 24 MiB (3 parts) at 6 MiB/s, pause after one part, resume.
    let src = temp_dir("p3src");
    let data: Vec<u8> = (0..(24 * 1024 * 1024u32))
        .map(|i| (i.wrapping_mul(7) % 251) as u8)
        .collect();
    let file = src.join("big.bin");
    std::fs::write(&file, &data).expect("write");
    limiter.set_limit(6 * 1024 * 1024);
    let job = queue
        .enqueue(vec![TransferRequest {
            account_id: env.account.id,
            direction: TransferDirection::Upload,
            bucket: bucket.clone(),
            key: "big.bin".into(),
            local_path: file.clone(),
            size_hint: data.len() as u64,
            source: None,
            version_id: None,
        }])
        .remove(0);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    queue.pause(job.id);
    settle(&queue).await;
    let paused = queue
        .jobs()
        .into_iter()
        .find(|j| j.id == job.id)
        .expect("job");
    assert_eq!(paused.state, TransferState::Paused, "{:?}", paused.error);
    assert!(
        paused
            .resume
            .as_deref()
            .is_some_and(|r| r.contains("uploadId")),
        "{:?}",
        paused.resume
    );
    limiter.set_limit(0);
    queue.retry(job.id);
    settle(&queue).await;
    let done = queue
        .jobs()
        .into_iter()
        .find(|j| j.id == job.id)
        .expect("job");
    assert_eq!(done.state, TransferState::Completed, "{:?}", done.error);
    assert_eq!(done.resume, None);
    let d = env
        .storage
        .object_details(&env.account, &bucket, "big.bin")
        .await
        .expect("head");
    assert_eq!(d.size, data.len() as u64);

    // Download it at 6 MiB/s, pause, resume: the partial file is continued, not restarted.
    let dst = temp_dir("p3dst");
    let target = dst.join("big.bin");
    limiter.set_limit(6 * 1024 * 1024);
    let job = queue
        .enqueue(vec![TransferRequest {
            account_id: env.account.id,
            direction: TransferDirection::Download,
            bucket: bucket.clone(),
            key: "big.bin".into(),
            local_path: target.clone(),
            size_hint: data.len() as u64,
            source: None,
            version_id: None,
        }])
        .remove(0);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    queue.pause(job.id);
    settle(&queue).await;
    let partial = std::fs::read_dir(&dst)
        .expect("dir")
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().ends_with(".s3m-partial"))
        .expect("partial file kept while paused");
    let partial_len = partial.metadata().expect("meta").len();
    assert!(
        partial_len > 0 && partial_len < data.len() as u64,
        "{partial_len}"
    );
    assert!(!target.exists());
    limiter.set_limit(0);
    queue.retry(job.id);
    settle(&queue).await;
    let done = queue
        .jobs()
        .into_iter()
        .find(|j| j.id == job.id)
        .expect("job");
    assert_eq!(done.state, TransferState::Completed, "{:?}", done.error);
    assert_eq!(std::fs::read(&target).expect("read"), data);
    assert!(!partial.path().exists());

    // Cancel a paused download: its partial file is removed.
    let target2 = dst.join("again.bin");
    limiter.set_limit(4 * 1024 * 1024);
    let job = queue
        .enqueue(vec![TransferRequest {
            account_id: env.account.id,
            direction: TransferDirection::Download,
            bucket: bucket.clone(),
            key: "big.bin".into(),
            local_path: target2.clone(),
            size_hint: data.len() as u64,
            source: None,
            version_id: None,
        }])
        .remove(0);
    tokio::time::sleep(Duration::from_millis(2000)).await;
    queue.pause(job.id);
    settle(&queue).await;
    queue.cancel(job.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let leftovers = std::fs::read_dir(&dst)
        .expect("dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".s3m-partial"))
        .count();
    assert_eq!(leftovers, 0);
    limiter.set_limit(0);

    // A throttled upload (10 MiB in two parts at 4 MiB/s) holds the limit on the wire and
    // arrives intact.
    let small = &data[..10 * 1024 * 1024];
    let file = src.join("throttled.bin");
    std::fs::write(&file, small).expect("write");
    limiter.set_limit(4 * 1024 * 1024);
    let started = std::time::Instant::now();
    let job = queue
        .enqueue(vec![TransferRequest {
            account_id: env.account.id,
            direction: TransferDirection::Upload,
            bucket: bucket.clone(),
            key: "throttled.bin".into(),
            local_path: file.clone(),
            size_hint: small.len() as u64,
            source: None,
            version_id: None,
        }])
        .remove(0);
    settle(&queue).await;
    let elapsed = started.elapsed();
    limiter.set_limit(0);
    let done = queue
        .jobs()
        .into_iter()
        .find(|j| j.id == job.id)
        .expect("job");
    assert_eq!(done.state, TransferState::Completed, "{:?}", done.error);
    assert!(elapsed >= Duration::from_millis(2000), "{elapsed:?}");
    let back = dst.join("throttled.bin");
    queue.enqueue(vec![TransferRequest {
        account_id: env.account.id,
        direction: TransferDirection::Download,
        bucket: bucket.clone(),
        key: "throttled.bin".into(),
        local_path: back.clone(),
        size_hint: small.len() as u64,
        source: None,
        version_id: None,
    }]);
    settle(&queue).await;
    assert_eq!(std::fs::read(&back).expect("read"), small);

    env.storage
        .delete_bucket(&env.account, &bucket, true)
        .await
        .expect("delete bucket");
    queue.shutdown(Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dst);
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_and_move_between_accounts() {
    if !enabled() {
        return;
    }
    let env = env();
    let mut second = draft(ENDPOINT, SECRET);
    second.name = "Second account".into();
    let other = env.accounts.save(&second).expect("second account");
    let src_bucket = format!("s3m-xa-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let dst_bucket = format!("{src_bucket}-b");
    env.storage
        .create_bucket(&env.account, &src_bucket, "")
        .await
        .expect("create src");
    env.storage
        .create_bucket(&other, &dst_bucket, "")
        .await
        .expect("create dst");
    let client = env.storage.clients().get(&env.account).expect("client");
    let big: Vec<u8> = (0..(10 * 1024 * 1024u32))
        .map(|i| (i % 249) as u8)
        .collect();
    client
        .put_object()
        .bucket(&src_bucket)
        .key("f/big.bin")
        .content_type("application/x-test")
        .body(aws_sdk_s3::primitives::ByteStream::from(big.clone()))
        .send()
        .await
        .expect("put big");
    client
        .put_object()
        .bucket(&src_bucket)
        .key("f/small.txt")
        .body(aws_sdk_s3::primitives::ByteStream::from_static(b"small"))
        .send()
        .await
        .expect("put small");

    let executor = Arc::new(S3TransferExecutor::new(
        env.accounts.clone(),
        env.storage.clients().clone(),
    ));
    let queue = TransferQueue::new(
        executor,
        Arc::new(MemoryTransferStore::default()),
        TransferOptions::default(),
        tokio::runtime::Handle::current(),
        Arc::new(|_| {}),
    );
    let all = env
        .storage
        .list_all(&env.account, &src_bucket, "f/")
        .await
        .expect("list");
    let clip = s3m_core::plan::Clipboard {
        account_id: env.account.id,
        bucket: src_bucket.clone(),
        keys: vec!["f/".into()],
        cut: true,
    };
    let requests = s3m_core::plan::paste_requests(&clip, &all, other.id, &dst_bucket, "");
    assert!(
        requests
            .iter()
            .all(|r| r.source.as_ref().and_then(|s| s.account_id) == Some(env.account.id))
    );
    queue.enqueue(requests);
    settle(&queue).await;
    for job in queue.jobs() {
        assert_eq!(
            job.state,
            TransferState::Completed,
            "{}: {:?}",
            job.key,
            job.error
        );
    }
    let copied = env
        .storage
        .object_details(&other, &dst_bucket, "f/big.bin")
        .await
        .expect("head big");
    assert_eq!(copied.size, big.len() as u64);
    assert_eq!(copied.content_type.as_deref(), Some("application/x-test"));
    let small = env
        .storage
        .preview(&other, &dst_bucket, "f/small.txt", 100)
        .await
        .expect("small");
    assert_eq!(small.bytes, b"small");
    let left = env
        .storage
        .list_all(&env.account, &src_bucket, "f/")
        .await
        .expect("source after move");
    assert!(
        left.iter().all(|o| o.key.ends_with('/')),
        "moved files must leave the source: {left:?}"
    );

    env.storage
        .delete_bucket(&env.account, &src_bucket, true)
        .await
        .expect("cleanup src");
    env.storage
        .delete_bucket(&other, &dst_bucket, true)
        .await
        .expect("cleanup dst");
    queue.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn versions_and_bucket_settings() {
    if !enabled() {
        return;
    }
    let env = env();
    let bucket = format!("s3m-adm-{}", &Uuid::new_v4().simple().to_string()[..12]);
    env.storage
        .create_bucket(&env.account, &bucket, "")
        .await
        .expect("create");
    let client = env.storage.clients().get(&env.account).expect("client");

    // Versioning: two writes give two versions; deleting the latest version restores the old one.
    env.storage
        .set_versioning(&env.account, &bucket, true)
        .await
        .expect("enable versioning");
    for body in [&b"first"[..], &b"second!"[..]] {
        client
            .put_object()
            .bucket(&bucket)
            .key("doc.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from(body.to_vec()))
            .send()
            .await
            .expect("put");
    }
    let versions = env
        .storage
        .list_versions(&env.account, &bucket, "doc.txt")
        .await
        .expect("versions");
    eprintln!("versions: {versions:?}");
    assert_eq!(versions.len(), 2, "{versions:?}");
    assert!(versions[0].is_latest && versions[0].size == 7);
    env.storage
        .delete_version(&env.account, &bucket, "doc.txt", &versions[0].version_id)
        .await
        .expect("delete latest version");
    let d = env
        .storage
        .object_details(&env.account, &bucket, "doc.txt")
        .await
        .expect("head");
    assert_eq!(d.size, 5, "the older version is current again");

    // Optional features: report what this server supports, check round trips where it does.
    let rules = s3m_core::admin::parse_cors(s3m_core::admin::CORS_TEMPLATE).expect("cors");
    match env.storage.set_cors(&env.account, &bucket, &rules).await {
        Ok(()) => {
            let back = env
                .storage
                .get_cors(&env.account, &bucket)
                .await
                .expect("get cors");
            eprintln!("CORS supported: {back:?}");
            assert_eq!(back[0].allowed_origins, rules[0].allowed_origins);
            env.storage
                .set_cors(&env.account, &bucket, &[])
                .await
                .expect("delete cors");
        }
        Err(e) => eprintln!("CORS not supported: {e}"),
    }
    let policy = s3m_core::admin::policy_template(&bucket);
    match env
        .storage
        .set_policy(&env.account, &bucket, Some(&policy))
        .await
    {
        Ok(()) => {
            let back = env
                .storage
                .get_policy(&env.account, &bucket)
                .await
                .expect("get policy");
            eprintln!("policy supported: {}", back.is_some());
            env.storage
                .set_policy(&env.account, &bucket, None)
                .await
                .expect("delete policy");
        }
        Err(e) => eprintln!("policy not supported: {e}"),
    }
    let lifecycle =
        s3m_core::admin::parse_lifecycle(s3m_core::admin::LIFECYCLE_TEMPLATE).expect("lc");
    match env
        .storage
        .set_lifecycle(&env.account, &bucket, &lifecycle)
        .await
    {
        Ok(()) => eprintln!(
            "lifecycle supported: {:?}",
            env.storage.get_lifecycle(&env.account, &bucket).await
        ),
        Err(e) => eprintln!("lifecycle not supported: {e}"),
    }
    match env
        .storage
        .set_encryption(&env.account, &bucket, &s3m_core::admin::Encryption::Aes256)
        .await
    {
        Ok(()) => eprintln!(
            "encryption supported: {:?}",
            env.storage.get_encryption(&env.account, &bucket).await
        ),
        Err(e) => eprintln!("encryption not supported: {e}"),
    }
    match env
        .storage
        .get_acl(&env.account, &bucket, s3m_s3::AclTarget::Object("doc.txt"))
        .await
    {
        Ok(acl) => {
            eprintln!("ACL: {acl:?}");
            let (m, others) = acl.matrix();
            let mut public = m;
            public.everyone[0] = true;
            let updated = s3m_core::admin::Acl::from_matrix(
                &acl.owner_id,
                acl.owner_name.clone(),
                &public,
                &others,
            );
            match env
                .storage
                .set_acl(
                    &env.account,
                    &bucket,
                    s3m_s3::AclTarget::Object("doc.txt"),
                    &updated,
                )
                .await
            {
                Ok(()) => eprintln!(
                    "ACL set: {:?}",
                    env.storage
                        .get_acl(&env.account, &bucket, s3m_s3::AclTarget::Object("doc.txt"))
                        .await
                ),
                Err(e) => eprintln!("ACL put not supported: {e}"),
            }
        }
        Err(e) => eprintln!("ACL not supported: {e}"),
    }

    // Versioned buckets keep old versions: remove them explicitly.
    for v in env
        .storage
        .list_versions(&env.account, &bucket, "doc.txt")
        .await
        .unwrap_or_default()
    {
        let _ = env
            .storage
            .delete_version(&env.account, &bucket, "doc.txt", &v.version_id)
            .await;
    }
    let _ = env.storage.delete_bucket(&env.account, &bucket, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn versioned_listing_shows_each_key_once() {
    if !enabled() {
        return;
    }
    let env = env();
    let bucket = format!("s3m-vl-{}", &Uuid::new_v4().simple().to_string()[..12]);
    env.storage
        .create_bucket(&env.account, &bucket, "")
        .await
        .expect("create");
    let client = env.storage.clients().get(&env.account).expect("client");
    // The first write happens before versioning is enabled (a "null" version), like an
    // existing bucket that gets versioning later.
    for (i, body) in [&b"one"[..], &b"two!"[..]].into_iter().enumerate() {
        if i == 1 {
            env.storage
                .set_versioning(&env.account, &bucket, true)
                .await
                .expect("versioning");
        }
        client
            .put_object()
            .bucket(&bucket)
            .key("same.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from(body.to_vec()))
            .send()
            .await
            .expect("put");
    }
    let raw = client
        .list_objects_v2()
        .bucket(&bucket)
        .delimiter("/")
        .send()
        .await
        .expect("raw list");
    let raw_count = raw
        .contents()
        .iter()
        .filter(|o| o.key() == Some("same.txt"))
        .count();
    eprintln!("server returned same.txt {raw_count} time(s)");
    let page = env
        .storage
        .list_objects(&env.account, &bucket, "", None)
        .await
        .expect("list");
    let entries: Vec<_> = page
        .entries
        .iter()
        .filter(|e| e.key == "same.txt")
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].size, 4, "the newest version is shown");

    // Restore the pre-versioning ("null") version through the queue, as the Versions tab does.
    let executor = Arc::new(S3TransferExecutor::new(
        env.accounts.clone(),
        env.storage.clients().clone(),
    ));
    let queue = TransferQueue::new(
        executor,
        Arc::new(MemoryTransferStore::default()),
        TransferOptions::default(),
        tokio::runtime::Handle::current(),
        Arc::new(|_| {}),
    );
    queue.enqueue(vec![TransferRequest {
        account_id: env.account.id,
        direction: TransferDirection::Copy,
        bucket: bucket.clone(),
        key: "same.txt".into(),
        local_path: PathBuf::new(),
        size_hint: 0,
        source: Some(s3m_core::ObjectRef {
            bucket: bucket.clone(),
            key: "same.txt".into(),
            account_id: None,
        }),
        version_id: Some("null".into()),
    }]);
    settle(&queue).await;
    let job = queue.jobs().remove(0);
    assert_eq!(job.state, TransferState::Completed, "{:?}", job.error);
    let restored = env
        .storage
        .preview(&env.account, &bucket, "same.txt", 100)
        .await
        .expect("read");
    assert_eq!(restored.bytes, b"one", "the old version is current again");
    queue.shutdown(Duration::from_secs(1)).await;
    for v in env
        .storage
        .list_versions(&env.account, &bucket, "same.txt")
        .await
        .unwrap_or_default()
    {
        let _ = env
            .storage
            .delete_version(&env.account, &bucket, "same.txt", &v.version_id)
            .await;
    }
    let _ = env.storage.delete_bucket(&env.account, &bucket, true).await;
}
