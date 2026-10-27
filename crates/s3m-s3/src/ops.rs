//! Phase 2 operations: recursive listing, bucket create/delete/info, server-side copy,
//! presigned URLs, previews and header editing.

use std::collections::HashMap;
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{
    BucketLocationConstraint, CompletedMultipartUpload, CompletedPart, CreateBucketConfiguration,
    MetadataDirective, StorageClass,
};
use s3m_core::plan::ListedObject;
use s3m_core::transfer::Progress;
use s3m_core::{Account, AccountType, BucketInfo, HeaderEdit, bucket};

use crate::encode_key;
use crate::error::{StorageError, StorageErrorKind};
use crate::service::StorageService;

/// Largest object a single CopyObject may copy.
pub const MAX_SINGLE_COPY: u64 = 5 * 1024 * 1024 * 1024;
/// Part size for multipart copies of larger objects.
const COPY_PART_SIZE: u64 = 512 * 1024 * 1024;
/// Longest presigned URL lifetime allowed by Signature V4.
pub const MAX_PRESIGN: Duration = Duration::from_secs(7 * 24 * 3600);

/// First bytes of an object for the Preview tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewData {
    /// Bytes read (at most the requested limit).
    pub bytes: Vec<u8>,
    /// Full object size.
    pub total: u64,
    /// Content-Type reported by the server.
    pub content_type: Option<String>,
}

impl StorageService {
    /// Every object under `prefix` (no delimiter), including folder markers.
    pub async fn list_all(
        &self,
        account: &Account,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<ListedObject>, StorageError> {
        let client = self.clients().get(account)?;
        let mut out = Vec::new();
        let mut token = None;
        loop {
            let r = client
                .list_objects_v2()
                .bucket(bucket)
                .prefix(prefix)
                .max_keys(1000)
                .set_continuation_token(token)
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            out.extend(r.contents().iter().filter_map(|o| {
                Some(ListedObject {
                    key: o.key()?.to_owned(),
                    size: o.size().and_then(|s| u64::try_from(s).ok()).unwrap_or(0),
                })
            }));
            token = match (r.is_truncated(), r.next_continuation_token()) {
                (Some(true), Some(t)) => Some(t.to_owned()),
                _ => break,
            };
        }
        // Keys are unique in S3; some servers repeat versioned keys (see dedupe_current).
        let mut seen = std::collections::HashSet::new();
        out.reverse();
        out.retain(|o| seen.insert(o.key.clone()));
        out.reverse();
        Ok(out)
    }

    /// Every object under `prefix` with its last modification time (for folder sync).
    pub async fn list_all_with_times(
        &self,
        account: &Account,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<(ListedObject, Option<chrono::DateTime<chrono::Utc>>)>, StorageError> {
        let client = self.clients().get(account)?;
        let mut out = Vec::new();
        let mut token = None;
        loop {
            let r = client
                .list_objects_v2()
                .bucket(bucket)
                .prefix(prefix)
                .max_keys(1000)
                .set_continuation_token(token)
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            out.extend(r.contents().iter().filter_map(|o| {
                Some((
                    ListedObject {
                        key: o.key()?.to_owned(),
                        size: o.size().and_then(|s| u64::try_from(s).ok()).unwrap_or(0),
                    },
                    o.last_modified().and_then(crate::to_chrono),
                ))
            }));
            token = match (r.is_truncated(), r.next_continuation_token()) {
                (Some(true), Some(t)) => Some(t.to_owned()),
                _ => break,
            };
        }
        let mut newest: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, (o, t)) in out.iter().enumerate() {
            match newest.get(&o.key) {
                Some(&j) if out[j].1 > *t => {}
                _ => {
                    newest.insert(o.key.clone(), i);
                }
            }
        }
        let mut index = 0;
        out.retain(|(o, _)| {
            let i = index;
            index += 1;
            newest.get(&o.key) == Some(&i)
        });
        Ok(out)
    }

    /// Creates a bucket. For Amazon S3 outside us-east-1 the region becomes the location
    /// constraint; S3-compatible servers get no constraint (many reject unknown ones).
    pub async fn create_bucket(
        &self,
        account: &Account,
        name: &str,
        region: &str,
    ) -> Result<(), StorageError> {
        let name = bucket::normalize(name);
        if !bucket::is_valid(&name) {
            return Err(StorageError::invalid(format!(
                "\"{name}\" is not a valid bucket name: 3–63 lowercase letters, digits, '.' or '-', starting and ending with a letter or digit."
            )));
        }
        let mut request = self.clients().get(account)?.create_bucket().bucket(&name);
        let region = region.trim();
        if account.kind == AccountType::AmazonS3 && !region.is_empty() && region != "us-east-1" {
            request = request.create_bucket_configuration(
                CreateBucketConfiguration::builder()
                    .location_constraint(BucketLocationConstraint::from(region))
                    .build(),
            );
        }
        request.send().await.map_err(|e| {
            let mapped = StorageError::from_sdk(e);
            if mapped.message.contains("BucketAlreadyExists")
                || mapped.message.contains("BucketAlreadyOwnedByYou")
            {
                StorageError::invalid(format!("A bucket named \"{name}\" already exists."))
            } else {
                mapped
            }
        })?;
        Ok(())
    }

    /// Deletes a bucket; with `empty_first`, deletes every object in it before.
    pub async fn delete_bucket(
        &self,
        account: &Account,
        name: &str,
        empty_first: bool,
    ) -> Result<usize, StorageError> {
        let mut deleted = 0;
        if empty_first {
            let keys: Vec<String> = self
                .list_all(account, name, "")
                .await?
                .into_iter()
                .map(|o| o.key)
                .collect();
            if !keys.is_empty() {
                deleted = self.delete_objects(account, name, &keys).await?;
            }
        }
        self.clients()
            .get(account)?
            .delete_bucket()
            .bucket(name)
            .send()
            .await
            .map_err(|e| {
                let mapped = StorageError::from_sdk(e);
                if mapped.message.contains("BucketNotEmpty")
                    || mapped.display().contains("not empty")
                {
                    StorageError::invalid(format!("The bucket \"{name}\" is not empty."))
                } else {
                    mapped
                }
            })?;
        Ok(deleted)
    }

    /// Deletes files, then the folder prefixes deepest first (one level per request, so servers
    /// with real directories see each folder empty before it is removed). Returns the number of
    /// files deleted.
    pub async fn delete_tree(
        &self,
        account: &Account,
        bucket: &str,
        files: &[String],
        folders: &[String],
    ) -> Result<usize, StorageError> {
        let deleted = if files.is_empty() {
            0
        } else {
            self.delete_objects(account, bucket, files).await?
        };
        let mut depth = usize::MAX;
        let mut level: Vec<String> = Vec::new();
        for folder in folders {
            let d = folder.matches('/').count();
            if d != depth && !level.is_empty() {
                self.delete_objects(account, bucket, &level).await?;
                level.clear();
            }
            depth = d;
            level.push(folder.clone());
        }
        if !level.is_empty() {
            self.delete_objects(account, bucket, &level).await?;
        }
        Ok(deleted)
    }

    /// Region and versioning status of a bucket. Failures of either call are reported as unknown.
    pub async fn bucket_info(
        &self,
        account: &Account,
        name: &str,
    ) -> Result<BucketInfo, StorageError> {
        let client = self.clients().get(account)?;
        let region = match client.get_bucket_location().bucket(name).send().await {
            Ok(r) => Some(r.location_constraint().map_or_else(
                || "us-east-1".to_owned(),
                |c| {
                    if c.as_str().is_empty() {
                        "us-east-1".to_owned()
                    } else {
                        c.as_str().to_owned()
                    }
                },
            )),
            Err(e) => {
                let e = StorageError::from_sdk(e);
                if e.kind == StorageErrorKind::NotFound {
                    return Err(e);
                }
                None
            }
        };
        let versioning = client
            .get_bucket_versioning()
            .bucket(name)
            .send()
            .await
            .ok()
            .map(|r| {
                r.status()
                    .map_or_else(|| "Never enabled".to_owned(), |s| s.as_str().to_owned())
            });
        Ok(BucketInfo {
            name: name.to_owned(),
            region,
            versioning,
        })
    }

    /// Presigned GET URL valid for `expires` (at most 7 days).
    pub async fn presign_get(
        &self,
        account: &Account,
        bucket: &str,
        key: &str,
        expires: Duration,
    ) -> Result<String, StorageError> {
        if expires.is_zero() || expires > MAX_PRESIGN {
            return Err(StorageError::invalid(
                "A Web URL can be valid for 1 second up to 7 days.",
            ));
        }
        let config = PresigningConfig::expires_in(expires)
            .map_err(|e| StorageError::invalid(e.to_string()))?;
        let request = self
            .clients()
            .get(account)?
            .get_object()
            .bucket(bucket)
            .key(key)
            .presigned(config)
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(request.uri().to_owned())
    }

    /// Reads at most `limit` bytes from the start of an object.
    pub async fn preview(
        &self,
        account: &Account,
        bucket: &str,
        key: &str,
        limit: u64,
    ) -> Result<PreviewData, StorageError> {
        let r = self
            .clients()
            .get(account)?
            .get_object()
            .bucket(bucket)
            .key(key)
            .range(format!("bytes=0-{}", limit.max(1) - 1))
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        let total = r
            .content_range()
            .and_then(|c| c.rsplit('/').next())
            .and_then(|t| t.parse().ok())
            .or_else(|| r.content_length().and_then(|l| u64::try_from(l).ok()))
            .unwrap_or(0);
        let content_type = r.content_type().map(str::to_owned);
        let mut bytes = r
            .body
            .collect()
            .await
            .map_err(|e| {
                StorageError::new(
                    StorageErrorKind::Network,
                    format!("Preview interrupted: {e}"),
                )
            })?
            .to_vec();
        bytes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(PreviewData {
            bytes,
            total,
            content_type,
        })
    }

    /// Replaces the HTTP headers and user metadata of an object (CopyObject onto itself).
    pub async fn update_headers(
        &self,
        account: &Account,
        bucket: &str,
        key: &str,
        headers: &HeaderEdit,
    ) -> Result<(), StorageError> {
        headers.validate().map_err(StorageError::invalid)?;
        let details = self.object_details(account, bucket, key).await?;
        if details.size > MAX_SINGLE_COPY {
            return Err(StorageError::invalid(
                "Editing headers of objects larger than 5 GB is not supported.",
            ));
        }
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
        let metadata: HashMap<String, String> = headers
            .metadata
            .iter()
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.clone()))
            .collect();
        let mut request = self
            .clients()
            .get(account)?
            .copy_object()
            .bucket(bucket)
            .key(key)
            .copy_source(format!("{bucket}/{}", encode_key(key)))
            .metadata_directive(MetadataDirective::Replace)
            .set_content_type(opt(&headers.content_type))
            .set_cache_control(opt(&headers.cache_control))
            .set_content_disposition(opt(&headers.content_disposition))
            .set_content_encoding(opt(&headers.content_encoding))
            .set_content_language(opt(&headers.content_language))
            .set_metadata(Some(metadata));
        if let Some(class) = details.storage_class.filter(|c| c != "STANDARD") {
            request = request.storage_class(StorageClass::from(class.as_str()));
        }
        request.send().await.map_err(StorageError::from_sdk)?;
        Ok(())
    }
}

/// Server-side copy of one object; multipart (UploadPartCopy) above 5 GB.
pub(crate) async fn copy_object(
    client: &Client,
    source_bucket: &str,
    source_key: &str,
    source_version: Option<&str>,
    bucket: &str,
    key: &str,
    progress: &Progress,
) -> Result<(), StorageError> {
    let head = client
        .head_object()
        .bucket(source_bucket)
        .key(source_key)
        .set_version_id(source_version.map(str::to_owned))
        .send()
        .await
        .map_err(StorageError::from_sdk)?;
    let size = head
        .content_length()
        .and_then(|l| u64::try_from(l).ok())
        .unwrap_or(0);
    progress(0, size);
    let mut source = format!("{source_bucket}/{}", encode_key(source_key));
    if let Some(version) = source_version {
        source.push_str(&format!("?versionId={}", encode_key(version)));
    }
    if size <= MAX_SINGLE_COPY {
        client
            .copy_object()
            .bucket(bucket)
            .key(key)
            .copy_source(&source)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        progress(size, size);
        return Ok(());
    }

    let created = client
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .set_content_type(head.content_type().map(str::to_owned))
        .set_metadata(head.metadata().cloned())
        .send()
        .await
        .map_err(StorageError::from_sdk)?;
    let upload_id = created
        .upload_id()
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Server,
                "The server did not return an upload id.",
            )
        })?
        .to_owned();
    let result: Result<Vec<CompletedPart>, StorageError> = async {
        let mut parts = Vec::new();
        let mut offset = 0u64;
        let mut number = 1i32;
        while offset < size {
            let end = (offset + COPY_PART_SIZE).min(size) - 1;
            let r = client
                .upload_part_copy()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .part_number(number)
                .copy_source(&source)
                .copy_source_range(format!("bytes={offset}-{end}"))
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            parts.push(
                CompletedPart::builder()
                    .part_number(number)
                    .set_e_tag(
                        r.copy_part_result()
                            .and_then(|c| c.e_tag())
                            .map(str::to_owned),
                    )
                    .build(),
            );
            offset = end + 1;
            number += 1;
            progress(offset, size);
        }
        Ok(parts)
    }
    .await;
    match result {
        Ok(parts) => {
            client
                .complete_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(parts))
                        .build(),
                )
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            Ok(())
        }
        Err(e) => {
            let _ = client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .send()
                .await;
            Err(e)
        }
    }
}
