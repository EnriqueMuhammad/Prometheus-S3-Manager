//! Phase 3 administration: object versions, bucket versioning, default encryption, bucket
//! policy, CORS, lifecycle rules and ACLs.

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::types::{
    AbortIncompleteMultipartUpload, AccessControlPolicy, BucketLifecycleConfiguration,
    BucketVersioningStatus, CorsConfiguration, ExpirationStatus, Grant, Grantee,
    LifecycleExpiration, LifecycleRuleFilter, NoncurrentVersionExpiration, Owner, Permission,
    ServerSideEncryption, ServerSideEncryptionByDefault, ServerSideEncryptionConfiguration,
    ServerSideEncryptionRule, Transition, TransitionStorageClass, Type, VersioningConfiguration,
};
use s3m_core::admin::{Acl, AclGrant, CorsRule, Encryption, LifecycleRule};
use s3m_core::{Account, ObjectVersion};

use crate::error::StorageError;
use crate::service::StorageService;
use crate::to_chrono;

/// Whether an error means "nothing configured" (S3 answers 404 with a specific code).
fn not_configured<E, R>(e: &SdkError<E, R>, codes: &[&str]) -> bool
where
    E: ProvideErrorMetadata,
{
    e.as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .is_some_and(|c| codes.contains(&c))
}

fn build_error(e: impl std::fmt::Display) -> StorageError {
    StorageError::invalid(e.to_string())
}

/// Target of an ACL operation.
#[derive(Debug, Clone, Copy)]
pub enum AclTarget<'a> {
    /// The bucket itself.
    Bucket,
    /// One object.
    Object(&'a str),
}

impl StorageService {
    /// Versions and delete markers of one key, newest first.
    pub async fn list_versions(
        &self,
        account: &Account,
        bucket: &str,
        key: &str,
    ) -> Result<Vec<ObjectVersion>, StorageError> {
        let client = self.clients().get(account)?;
        let mut out = Vec::new();
        let mut key_marker: Option<String> = None;
        let mut version_marker: Option<String> = None;
        loop {
            let r = client
                .list_object_versions()
                .bucket(bucket)
                .prefix(key)
                .set_key_marker(key_marker)
                .set_version_id_marker(version_marker)
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            out.extend(
                r.versions()
                    .iter()
                    .filter(|v| v.key() == Some(key))
                    .map(|v| ObjectVersion {
                        version_id: v.version_id().unwrap_or("null").to_owned(),
                        last_modified: v.last_modified().and_then(to_chrono),
                        size: v.size().and_then(|s| u64::try_from(s).ok()).unwrap_or(0),
                        is_latest: v.is_latest().unwrap_or(false),
                        delete_marker: false,
                        etag: v.e_tag().map(str::to_owned),
                    }),
            );
            out.extend(
                r.delete_markers()
                    .iter()
                    .filter(|d| d.key() == Some(key))
                    .map(|d| ObjectVersion {
                        version_id: d.version_id().unwrap_or("null").to_owned(),
                        last_modified: d.last_modified().and_then(to_chrono),
                        size: 0,
                        is_latest: d.is_latest().unwrap_or(false),
                        delete_marker: true,
                        etag: None,
                    }),
            );
            if r.is_truncated() == Some(true) {
                key_marker = r.next_key_marker().map(str::to_owned);
                version_marker = r.next_version_id_marker().map(str::to_owned);
                if key_marker.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        out.sort_by(|a, b| {
            b.last_modified
                .cmp(&a.last_modified)
                .then(b.is_latest.cmp(&a.is_latest))
        });
        Ok(out)
    }

    /// Permanently deletes one version (or removes a delete marker, which restores the object).
    pub async fn delete_version(
        &self,
        account: &Account,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> Result<(), StorageError> {
        self.clients()
            .get(account)?
            .delete_object()
            .bucket(bucket)
            .key(key)
            .version_id(version_id)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(())
    }

    /// Enables or suspends versioning.
    pub async fn set_versioning(
        &self,
        account: &Account,
        bucket: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let status = if enabled {
            BucketVersioningStatus::Enabled
        } else {
            BucketVersioningStatus::Suspended
        };
        self.clients()
            .get(account)?
            .put_bucket_versioning()
            .bucket(bucket)
            .versioning_configuration(VersioningConfiguration::builder().status(status).build())
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(())
    }

    /// Default encryption of a bucket.
    pub async fn get_encryption(
        &self,
        account: &Account,
        bucket: &str,
    ) -> Result<Encryption, StorageError> {
        match self
            .clients()
            .get(account)?
            .get_bucket_encryption()
            .bucket(bucket)
            .send()
            .await
        {
            Ok(r) => {
                let rule = r
                    .server_side_encryption_configuration()
                    .and_then(|c| c.rules().first())
                    .and_then(|r| r.apply_server_side_encryption_by_default());
                Ok(match rule {
                    Some(d) if *d.sse_algorithm() == ServerSideEncryption::AwsKms => {
                        Encryption::Kms(d.kms_master_key_id().unwrap_or_default().to_owned())
                    }
                    Some(_) => Encryption::Aes256,
                    None => Encryption::None,
                })
            }
            Err(e) if not_configured(&e, &["ServerSideEncryptionConfigurationNotFoundError"]) => {
                Ok(Encryption::None)
            }
            Err(e) => Err(StorageError::from_sdk(e)),
        }
    }

    /// Sets (or removes) default encryption.
    pub async fn set_encryption(
        &self,
        account: &Account,
        bucket: &str,
        encryption: &Encryption,
    ) -> Result<(), StorageError> {
        let client = self.clients().get(account)?;
        let (algorithm, key) = match encryption {
            Encryption::None => {
                client
                    .delete_bucket_encryption()
                    .bucket(bucket)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
                return Ok(());
            }
            Encryption::Aes256 => (ServerSideEncryption::Aes256, None),
            Encryption::Kms(id) => (
                ServerSideEncryption::AwsKms,
                (!id.trim().is_empty()).then(|| id.trim().to_owned()),
            ),
        };
        let default = ServerSideEncryptionByDefault::builder()
            .sse_algorithm(algorithm)
            .set_kms_master_key_id(key)
            .build()
            .map_err(build_error)?;
        let config = ServerSideEncryptionConfiguration::builder()
            .rules(
                ServerSideEncryptionRule::builder()
                    .apply_server_side_encryption_by_default(default)
                    .build(),
            )
            .build()
            .map_err(build_error)?;
        client
            .put_bucket_encryption()
            .bucket(bucket)
            .server_side_encryption_configuration(config)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(())
    }

    /// Bucket policy JSON, `None` when there is none.
    pub async fn get_policy(
        &self,
        account: &Account,
        bucket: &str,
    ) -> Result<Option<String>, StorageError> {
        match self
            .clients()
            .get(account)?
            .get_bucket_policy()
            .bucket(bucket)
            .send()
            .await
        {
            Ok(r) => Ok(r.policy().map(str::to_owned)),
            Err(e) if not_configured(&e, &["NoSuchBucketPolicy"]) => Ok(None),
            Err(e) => Err(StorageError::from_sdk(e)),
        }
    }

    /// Replaces (`Some`) or deletes (`None`) the bucket policy.
    pub async fn set_policy(
        &self,
        account: &Account,
        bucket: &str,
        policy: Option<&str>,
    ) -> Result<(), StorageError> {
        let client = self.clients().get(account)?;
        match policy {
            Some(text) => {
                let text = s3m_core::admin::check_policy(text).map_err(StorageError::invalid)?;
                client
                    .put_bucket_policy()
                    .bucket(bucket)
                    .policy(text)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
            }
            None => {
                client
                    .delete_bucket_policy()
                    .bucket(bucket)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
            }
        }
        Ok(())
    }

    /// CORS rules (empty when none).
    pub async fn get_cors(
        &self,
        account: &Account,
        bucket: &str,
    ) -> Result<Vec<CorsRule>, StorageError> {
        match self
            .clients()
            .get(account)?
            .get_bucket_cors()
            .bucket(bucket)
            .send()
            .await
        {
            Ok(r) => Ok(r
                .cors_rules()
                .iter()
                .map(|c| CorsRule {
                    allowed_origins: c.allowed_origins().to_vec(),
                    allowed_methods: c.allowed_methods().to_vec(),
                    allowed_headers: c.allowed_headers().to_vec(),
                    expose_headers: c.expose_headers().to_vec(),
                    max_age_seconds: c.max_age_seconds(),
                })
                .collect()),
            Err(e) if not_configured(&e, &["NoSuchCORSConfiguration"]) => Ok(Vec::new()),
            Err(e) => Err(StorageError::from_sdk(e)),
        }
    }

    /// Replaces CORS rules; an empty list deletes the configuration.
    pub async fn set_cors(
        &self,
        account: &Account,
        bucket: &str,
        rules: &[CorsRule],
    ) -> Result<(), StorageError> {
        let client = self.clients().get(account)?;
        if rules.is_empty() {
            client
                .delete_bucket_cors()
                .bucket(bucket)
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            return Ok(());
        }
        let rules = rules
            .iter()
            .map(|r| {
                aws_sdk_s3::types::CorsRule::builder()
                    .set_allowed_origins(Some(r.allowed_origins.clone()))
                    .set_allowed_methods(Some(
                        r.allowed_methods
                            .iter()
                            .map(|m| m.to_ascii_uppercase())
                            .collect(),
                    ))
                    .set_allowed_headers(
                        (!r.allowed_headers.is_empty()).then(|| r.allowed_headers.clone()),
                    )
                    .set_expose_headers(
                        (!r.expose_headers.is_empty()).then(|| r.expose_headers.clone()),
                    )
                    .set_max_age_seconds(r.max_age_seconds)
                    .build()
                    .map_err(build_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let config = CorsConfiguration::builder()
            .set_cors_rules(Some(rules))
            .build()
            .map_err(build_error)?;
        client
            .put_bucket_cors()
            .bucket(bucket)
            .cors_configuration(config)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(())
    }

    /// Lifecycle rules (empty when none).
    pub async fn get_lifecycle(
        &self,
        account: &Account,
        bucket: &str,
    ) -> Result<Vec<LifecycleRule>, StorageError> {
        match self
            .clients()
            .get(account)?
            .get_bucket_lifecycle_configuration()
            .bucket(bucket)
            .send()
            .await
        {
            Ok(r) => Ok(r
                .rules()
                .iter()
                .map(|l| {
                    #[allow(deprecated)]
                    let prefix = l
                        .filter()
                        .and_then(LifecycleRuleFilter::prefix)
                        .or(l.prefix())
                        .unwrap_or_default()
                        .to_owned();
                    let transition = l.transitions().first();
                    LifecycleRule {
                        id: l.id().unwrap_or_default().to_owned(),
                        enabled: *l.status() == ExpirationStatus::Enabled,
                        prefix,
                        expiration_days: l.expiration().and_then(LifecycleExpiration::days),
                        noncurrent_expiration_days: l
                            .noncurrent_version_expiration()
                            .and_then(NoncurrentVersionExpiration::noncurrent_days),
                        abort_incomplete_upload_days: l
                            .abort_incomplete_multipart_upload()
                            .and_then(AbortIncompleteMultipartUpload::days_after_initiation),
                        transition_days: transition.and_then(Transition::days),
                        transition_storage_class: transition
                            .and_then(Transition::storage_class)
                            .map(|c| c.as_str().to_owned()),
                    }
                })
                .collect()),
            Err(e) if not_configured(&e, &["NoSuchLifecycleConfiguration"]) => Ok(Vec::new()),
            Err(e) => Err(StorageError::from_sdk(e)),
        }
    }

    /// Replaces lifecycle rules; an empty list deletes the configuration.
    pub async fn set_lifecycle(
        &self,
        account: &Account,
        bucket: &str,
        rules: &[LifecycleRule],
    ) -> Result<(), StorageError> {
        let client = self.clients().get(account)?;
        if rules.is_empty() {
            client
                .delete_bucket_lifecycle()
                .bucket(bucket)
                .send()
                .await
                .map_err(StorageError::from_sdk)?;
            return Ok(());
        }
        let rules = rules
            .iter()
            .map(|r| {
                let mut b = aws_sdk_s3::types::LifecycleRule::builder()
                    .id(&r.id)
                    .status(if r.enabled {
                        ExpirationStatus::Enabled
                    } else {
                        ExpirationStatus::Disabled
                    })
                    .filter(LifecycleRuleFilter::builder().prefix(&r.prefix).build());
                if let Some(days) = r.expiration_days {
                    b = b.expiration(LifecycleExpiration::builder().days(days).build());
                }
                if let Some(days) = r.noncurrent_expiration_days {
                    b = b.noncurrent_version_expiration(
                        NoncurrentVersionExpiration::builder()
                            .noncurrent_days(days)
                            .build(),
                    );
                }
                if let Some(days) = r.abort_incomplete_upload_days {
                    b = b.abort_incomplete_multipart_upload(
                        AbortIncompleteMultipartUpload::builder()
                            .days_after_initiation(days)
                            .build(),
                    );
                }
                if let (Some(days), Some(class)) = (r.transition_days, &r.transition_storage_class)
                {
                    b = b.transitions(
                        Transition::builder()
                            .days(days)
                            .storage_class(TransitionStorageClass::from(class.as_str()))
                            .build(),
                    );
                }
                b.build().map_err(build_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let config = BucketLifecycleConfiguration::builder()
            .set_rules(Some(rules))
            .build()
            .map_err(build_error)?;
        client
            .put_bucket_lifecycle_configuration()
            .bucket(bucket)
            .lifecycle_configuration(config)
            .send()
            .await
            .map_err(StorageError::from_sdk)?;
        Ok(())
    }

    /// ACL of a bucket or object.
    pub async fn get_acl(
        &self,
        account: &Account,
        bucket: &str,
        target: AclTarget<'_>,
    ) -> Result<Acl, StorageError> {
        let client = self.clients().get(account)?;
        let (owner, grants) = match target {
            AclTarget::Bucket => {
                let r = client
                    .get_bucket_acl()
                    .bucket(bucket)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
                (r.owner().cloned(), r.grants().to_vec())
            }
            AclTarget::Object(key) => {
                let r = client
                    .get_object_acl()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
                (r.owner().cloned(), r.grants().to_vec())
            }
        };
        Ok(Acl {
            owner_id: owner
                .as_ref()
                .and_then(Owner::id)
                .unwrap_or_default()
                .to_owned(),
            owner_name: owner
                .as_ref()
                .and_then(Owner::display_name)
                .map(str::to_owned),
            grants: grants.iter().filter_map(grant_from_sdk).collect(),
        })
    }

    /// Replaces the ACL of a bucket or object.
    pub async fn set_acl(
        &self,
        account: &Account,
        bucket: &str,
        target: AclTarget<'_>,
        acl: &Acl,
    ) -> Result<(), StorageError> {
        let client = self.clients().get(account)?;
        let grants = acl
            .grants
            .iter()
            .map(grant_to_sdk)
            .collect::<Result<Vec<_>, _>>()?;
        let policy = AccessControlPolicy::builder()
            .owner(
                Owner::builder()
                    .id(&acl.owner_id)
                    .set_display_name(acl.owner_name.clone())
                    .build(),
            )
            .set_grants(Some(grants))
            .build();
        match target {
            AclTarget::Bucket => {
                client
                    .put_bucket_acl()
                    .bucket(bucket)
                    .access_control_policy(policy)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
            }
            AclTarget::Object(key) => {
                client
                    .put_object_acl()
                    .bucket(bucket)
                    .key(key)
                    .access_control_policy(policy)
                    .send()
                    .await
                    .map_err(StorageError::from_sdk)?;
            }
        }
        Ok(())
    }
}

fn grant_from_sdk(g: &Grant) -> Option<AclGrant> {
    let grantee = g.grantee()?;
    let kind = grantee.r#type().as_str().to_owned();
    let id = grantee
        .id()
        .or(grantee.uri())
        .or(grantee.email_address())
        .unwrap_or_default()
        .to_owned();
    Some(AclGrant {
        kind,
        grantee: id,
        display_name: grantee.display_name().map(str::to_owned),
        permission: g.permission()?.as_str().to_owned(),
    })
}

fn grant_to_sdk(g: &AclGrant) -> Result<Grant, StorageError> {
    let mut b = Grantee::builder().r#type(Type::from(g.kind.as_str()));
    b = match g.kind.as_str() {
        "Group" => b.uri(&g.grantee),
        "AmazonCustomerByEmail" => b.email_address(&g.grantee),
        _ => b.id(&g.grantee).set_display_name(g.display_name.clone()),
    };
    Ok(Grant::builder()
        .grantee(b.build().map_err(build_error)?)
        .permission(Permission::from(g.permission.as_str()))
        .build())
}
