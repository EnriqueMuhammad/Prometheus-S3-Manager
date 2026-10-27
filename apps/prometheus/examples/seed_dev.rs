//! Development helper: registers the local SeaweedFS of `dev/docker-compose.yml` as an account
//! in a data folder and fills a sample bucket.
//!
//! ```text
//! docker compose -f dev/docker-compose.yml up -d
//! cargo run -p prometheus-s3manager --example seed_dev -- <data-dir>
//! cargo run -p prometheus-s3manager -- --data-dir <data-dir>
//! cargo run -p prometheus-s3manager --example seed_dev -- <data-dir> --clean
//! ```
//!
//! The secret goes to the OS keychain like any account; `--clean` (or deleting the account in
//! the app) removes the account and its secret again.

use std::sync::Arc;

use aws_sdk_s3::primitives::ByteStream;
use s3m_core::{AccountDraft, AccountService, AccountType};
use s3m_s3::StorageService;
use s3m_storage::{JsonAccountRepository, KeyringSecrets};

const DEV_ACCOUNT: &str = "Local SeaweedFS (dev)";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("usage: seed_dev <data-dir>")?;
    let paths = s3m_storage::AppPaths::new(dir);
    let secrets = Arc::new(KeyringSecrets);
    let accounts = AccountService::new(
        Box::new(JsonAccountRepository::new(paths.accounts())),
        secrets.clone(),
    );
    if std::env::args().any(|a| a == "--clean") {
        for a in accounts
            .list()?
            .into_iter()
            .filter(|a| a.name == DEV_ACCOUNT)
        {
            accounts.remove(a.id)?;
            println!("removed account {} and its keychain secret", a.id);
        }
        return Ok(());
    }
    let account = match accounts.list()?.into_iter().find(|a| a.name == DEV_ACCOUNT) {
        Some(a) => a,
        None => accounts.save(&AccountDraft {
            name: DEV_ACCOUNT.into(),
            kind: AccountType::S3Compatible,
            endpoint_host: "127.0.0.1:59000".into(),
            use_tls: false,
            region: "us-east-1".into(),
            access_key_id: "s3m-test".into(),
            secret_access_key: Some("s3m-test-secret".into()),
            force_path_style: true,
            manual_buckets: vec!["registered-only".into()],
            notes: "Created by examples/seed_dev.rs".into(),
            ..AccountDraft::default()
        })?,
    };
    let storage = StorageService::with_secrets(secrets);
    let client = storage.clients().get(&account)?;
    for bucket in ["po-archive", "backups"] {
        let _ = client.create_bucket().bucket(bucket).send().await;
    }
    let files: &[(&str, usize)] = &[
        ("2026/10/PO-2026-0001.pdf", 1_250_000),
        ("2026/10/PO-2026-0002.pdf", 980_000),
        ("2026/10/PO-2026-0010.pdf", 455_000),
        ("2026/10/summary.xlsx", 64_000),
        ("2026/09/PO-2026-0950.pdf", 870_000),
        ("invoices/INV-001.png", 210_000),
        ("readme.txt", 1_200),
        ("export.csv", 33_000),
        ("logo.svg", 4_000),
    ];
    for (key, size) in files {
        let body: Vec<u8> = match *key {
            // Real content so the Preview tab has something to show.
            "invoices/INV-001.png" => include_bytes!("../assets/icon-256.png").to_vec(),
            "readme.txt" => "Prometheus S3Manager sample bucket\n\nFolders 2026/09 and 2026/10 hold purchase orders.\n"
                .repeat(20)
                .into_bytes(),
            _ => (0..*size).map(|i| (i % 251) as u8).collect(),
        };
        client
            .put_object()
            .bucket("po-archive")
            .key(*key)
            .body(ByteStream::from(body))
            .send()
            .await?;
    }
    storage
        .create_folder(&account, "po-archive", "", "empty-folder")
        .await?;
    println!("seeded account {} with bucket po-archive", account.id);
    Ok(())
}
