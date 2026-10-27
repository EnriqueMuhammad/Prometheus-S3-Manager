//! S3 operations used by the explorer. All calls are per account.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use aws_sdk_s3::Client;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use s3m_core::browse::name_under_prefix;
use s3m_core::{
    Account, BucketEntry, BucketListing, ConnectionTestResult, ObjectDetails, ObjectEntry,
    ObjectPage, SecretStore,
};

use crate::client::{self, ClientProvider};
use crate::error::{StorageError, StorageErrorKind};
use crate::{encode_key, to_chrono};

/// Objects per listing page (S3 maximum).
pub const PAGE_SIZE: i32 = 1000;

/// Explorer operations.
pub struct StorageService {
    clients: Arc<ClientProvider>,
}

impl StorageService {
    /// Service over a client provider.
    pub fn new(clients: Arc<ClientProvider>) -> Self {
        Self { clients }
    }

    /// Service with its own provider.
    pub fn with_secrets(secrets: Arc<dyn SecretStore>) -> Self {
        Self::new(Arc::new(ClientProvider::new(secrets)))
    }

    /// The client provider (shared with the transfer executor).
    pub fn clients(&self) -> &Arc<ClientProvider> {
        &self.clients
    }

    /// Tests a saved account with its stored secret.
    pub async fn test_connection(&self, account: &Account) -> ConnectionTestResult {
        let started = Instant::now();
        match self.clients.get(account) {
            Ok(client) => test_with(&client, account, started).await,
            Err(e) => failed(e, started),
        }
    }

    /// Tests settings that may not be saved yet, with the secret typed in the editor.
    pub async fn test_unsaved(&self, account: &Account, secret: &str) -> ConnectionTestResult {
        let started = Instant::now();
        match client::build(account, secret, &self.clients.proxy()) {
            Ok(client) => test_with(&client, account, started).await,
            Err(e) => failed(e, started),
        }
    }

    /// Lists buckets; falls back to the registered buckets when ListBuckets is denied.
    pub async fn list_buckets(&self, account: &Account) -> Result<BucketListing, StorageError> {
        list_buckets_with(&self.clients.get(account)?, account).await
    }

    /// One page of a prefix listing with `/` as delimiter.
    pub async fn list_objects(
        &self,
        account: &Account,
        bucket: &str,
        prefix: &str,
        token: Option<String>,
    ) -> Result<ObjectPage, StorageError> {
        let response = self
            .clients
            .get(account)?
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .delimiter("/")
            .max_keys(PAGE_SIZE)
            .set_continuation_token(token)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;

        let mut entries = Vec::new();
        for p in response.common_prefixes() {
            if let Some(key) = p.prefix() {
                entries.push(ObjectEntry {
                    key: key.to_owned(),
                    name: name_under_prefix(key, prefix),
                    is_folder: true,
                    size: 0,
                    last_modified: None,
                    storage_class: None,
                    etag: None,
                });
            }
        }
        for o in response.contents() {
            let Some(key) = o.key() else { continue };
            if key == prefix {
                continue; // the folder marker of the current prefix
            }
            entries.push(ObjectEntry {
                key: key.to_owned(),
                name: name_under_prefix(key, prefix),
                is_folder: key.ends_with('/'),
                size: o.size().and_then(|s| u64::try_from(s).ok()).unwrap_or(0),
                last_modified: o.last_modified().and_then(to_chrono),
                storage_class: o.storage_class().map(|c| c.as_str().to_owned()),
                etag: o.e_tag().map(str::to_owned),
            });
        }
        dedupe_current(&self.clients.get(account)?, bucket, &mut entries).await;
        let next_token = if response.is_truncated() == Some(true) {
            response.next_continuation_token().map(str::to_owned)
        } else {
            None
        };
        Ok(ObjectPage {
            entries,
            next_token,
        })
    }

    /// HEAD of an object.
    pub async fn object_details(
        &self,
        account: &Account,
        bucket: &str,
        key: &str,
    ) -> Result<ObjectDetails, StorageError> {
        let r = self
            .clients
            .get(account)?
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(ObjectDetails {
            key: key.to_owned(),
            size: r
                .content_length()
                .and_then(|s| u64::try_from(s).ok())
                .unwrap_or(0),
            last_modified: r.last_modified().and_then(to_chrono),
            content_type: r.content_type().map(str::to_owned),
            etag: r.e_tag().map(str::to_owned),
            storage_class: r.storage_class().map(|c| c.as_str().to_owned()),
            version_id: r.version_id().map(str::to_owned),
            cache_control: r.cache_control().map(str::to_owned),
            content_disposition: r.content_disposition().map(str::to_owned),
            content_encoding: r.content_encoding().map(str::to_owned),
            content_language: r.content_language().map(str::to_owned),
            metadata: r
                .metadata()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_else(BTreeMap::new),
        })
    }

    /// Creates a zero-byte `prefix + name + "/"` folder marker.
    pub async fn create_folder(
        &self,
        account: &Account,
        bucket: &str,
        prefix: &str,
        name: &str,
    ) -> Result<(), StorageError> {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(StorageError::invalid(
                "Folder name must not be empty or contain \"/\".",
            ));
        }
        self.clients
            .get(account)?
            .put_object()
            .bucket(bucket)
            .key(format!("{prefix}{name}/"))
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(())
    }

    /// Deletes objects in batches of 1000; returns how many were deleted.
    pub async fn delete_objects(
        &self,
        account: &Account,
        bucket: &str,
        keys: &[String],
    ) -> Result<usize, StorageError> {
        let client = self.clients.get(account)?;
        let mut deleted = 0;
        let mut failures = Vec::new();
        for chunk in keys.chunks(1000) {
            let objects = chunk
                .iter()
                .map(|k| ObjectIdentifier::builder().key(k).build())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| StorageError::invalid(e.to_string()))?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .quiet(false)
                .build()
                .map_err(|e| StorageError::invalid(e.to_string()))?;
            let response = client
                .delete_objects()
                .bucket(bucket)
                .delete(delete)
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            deleted += response.deleted().len();
            failures.extend(response.errors().iter().map(|e| {
                format!(
                    "{}: {}",
                    e.key().unwrap_or("?"),
                    e.code().unwrap_or("error")
                )
            }));
        }
        if !failures.is_empty() {
            return Err(StorageError::new(
                StorageErrorKind::AccessDenied,
                format!(
                    "{} object(s) were not deleted: {}",
                    failures.len(),
                    failures
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ));
        }
        Ok(deleted)
    }

    /// Copy then delete. Not atomic: if the delete fails both objects remain and the error says so.
    pub async fn rename_object(
        &self,
        account: &Account,
        bucket: &str,
        source: &str,
        destination: &str,
    ) -> Result<(), StorageError> {
        if source == destination {
            return Ok(());
        }
        if source.ends_with('/') {
            return Err(StorageError::invalid(
                "Renaming folders is not supported yet.",
            ));
        }
        if destination.is_empty() || destination.len() > 1024 {
            return Err(StorageError::invalid(
                "The new key must be 1–1024 characters.",
            ));
        }
        let client = self.clients.get(account)?;
        client
            .copy_object()
            .bucket(bucket)
            .key(destination)
            .copy_source(format!("{bucket}/{}", encode_key(source)))
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        client
            .delete_object()
            .bucket(bucket)
            .key(source)
            .send()
            .await
            .map_err(|e| {
                let e = StorageError::from_sdk(e);
                StorageError::with_hint(
                    e.kind,
                    format!(
                        "Copied to \"{destination}\" but could not delete the original: {}",
                        e.message
                    ),
                    "Both objects now exist.",
                )
            })?;
        Ok(())
    }
}

/// Some servers (SeaweedFS with versioning enabled on a bucket that already had objects) list a
/// key twice: the old "null" version and the current one. S3 lists each key once, so when a key
/// repeats, keep the entry whose ETag matches the current object (HEAD), else the last one.
async fn dedupe_current(client: &Client, bucket: &str, entries: &mut Vec<ObjectEntry>) {
    let mut seen = std::collections::HashMap::new();
    let mut duplicates = Vec::new();
    for e in entries.iter() {
        if *seen
            .entry(e.key.clone())
            .and_modify(|n| *n += 1)
            .or_insert(1)
            == 2
        {
            duplicates.push(e.key.clone());
        }
    }
    for key in duplicates {
        let current = client
            .head_object()
            .bucket(bucket)
            .key(&key)
            .send()
            .await
            .ok()
            .and_then(|h| h.e_tag().map(str::to_owned));
        let positions: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.key == key)
            .map(|(i, _)| i)
            .collect();
        // Several entries may carry the current ETag (a restored version): take the newest.
        let keep = positions
            .iter()
            .copied()
            .filter(|&i| current.is_some() && entries[i].etag == current)
            .max_by_key(|&i| entries[i].last_modified)
            .or_else(|| {
                positions
                    .iter()
                    .copied()
                    .max_by_key(|&i| entries[i].last_modified)
            });
        let mut index = 0;
        entries.retain(|e| {
            let i = index;
            index += 1;
            e.key != key || Some(i) == keep
        });
    }
}

async fn list_buckets_with(
    client: &Client,
    account: &Account,
) -> Result<BucketListing, StorageError> {
    let manual = account.manual_buckets.iter().map(|b| BucketEntry {
        name: b.clone(),
        creation_date: None,
        manual: true,
    });
    match client.list_buckets().send().await {
        Ok(response) => {
            let mut buckets: Vec<BucketEntry> = response
                .buckets()
                .iter()
                .filter_map(|b| {
                    Some(BucketEntry {
                        name: b.name()?.to_owned(),
                        creation_date: b.creation_date().and_then(to_chrono),
                        manual: false,
                    })
                })
                .collect();
            let listed: Vec<String> = buckets.iter().map(|b| b.name.clone()).collect();
            buckets.extend(manual.filter(|m| !listed.contains(&m.name)));
            Ok(BucketListing {
                buckets,
                restricted: false,
            })
        }
        Err(e) => {
            let e = StorageError::from_sdk(e);
            if e.kind == StorageErrorKind::AccessDenied {
                Ok(BucketListing {
                    buckets: manual.collect(),
                    restricted: true,
                })
            } else {
                Err(e)
            }
        }
    }
}

async fn test_with(client: &Client, account: &Account, started: Instant) -> ConnectionTestResult {
    let listing = match list_buckets_with(client, account).await {
        Ok(l) => l,
        Err(e) => return failed(e, started),
    };
    if !listing.restricted {
        return ConnectionTestResult {
            ok: true,
            message: format!("Connected. {} bucket(s) visible.", listing.buckets.len()),
            latency: started.elapsed(),
            buckets_visible: listing.buckets.len(),
            restricted: false,
        };
    }
    // ListBuckets denied: a 403 alone does not prove the credentials work.
    let Some(first) = account.manual_buckets.first() else {
        return ConnectionTestResult {
            ok: false,
            message: "Listing buckets is not allowed for this key. Register a bucket name in Advanced settings to test access.".into(),
            latency: started.elapsed(),
            buckets_visible: 0,
            restricted: true,
        };
    };
    match client
        .list_objects_v2()
        .bucket(first)
        .max_keys(1)
        .send()
        .await
    {
        Ok(_) => ConnectionTestResult {
            ok: true,
            message: format!("Connected with restricted access. Bucket \"{first}\" is reachable."),
            latency: started.elapsed(),
            buckets_visible: account.manual_buckets.len(),
            restricted: true,
        },
        Err(e) => failed(StorageError::from_sdk(e), started),
    }
}

fn failed(e: StorageError, started: Instant) -> ConnectionTestResult {
    ConnectionTestResult {
        ok: false,
        message: e.display(),
        latency: started.elapsed(),
        buckets_visible: 0,
        restricted: false,
    }
}
