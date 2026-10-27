//! Folder synchronisation planning: compares a local folder with a bucket prefix by size and
//! modification time and lists what to transfer or delete. The plan is shown before anything
//! runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::paths;
use crate::plan::ListedObject;
use crate::transfer::{TransferDirection, TransferRequest};

/// Which side is the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// Local folder → bucket (upload new and changed files).
    Upload,
    /// Bucket → local folder (download new files).
    Download,
}

/// A local file found under the sync root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    /// Path relative to the root, with `/` separators.
    pub relative: String,
    /// Full path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Modification time.
    pub modified: Option<DateTime<Utc>>,
}

/// A remote object found under the sync prefix (with its last modification time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    /// Key relative to the prefix.
    pub relative: String,
    /// Full key.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification time.
    pub modified: Option<DateTime<Utc>>,
}

/// What the comparison found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// Transfers to run.
    pub transfers: Vec<TransferRequest>,
    /// Remote keys to delete (upload direction with "delete extra").
    pub deletes: Vec<String>,
    /// Files left alone, with the reason.
    pub skipped: Vec<(String, String)>,
    /// Files already in sync.
    pub unchanged: usize,
    /// Bytes to transfer.
    pub bytes: u64,
}

/// Modification times closer than this count as equal (file systems round differently).
const TIME_TOLERANCE_SECS: i64 = 2;

fn newer(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).num_seconds() > TIME_TOLERANCE_SECS,
        _ => false,
    }
}

/// Lists local files under `root` (symbolic links and system files skipped).
pub fn scan_local(root: &Path) -> Vec<LocalFile> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !e.path_is_symlink())
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let relative = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let Ok(meta) = entry.metadata() else { continue };
        out.push(LocalFile {
            relative,
            path: entry.path().to_path_buf(),
            size: meta.len(),
            modified: meta.modified().ok().map(DateTime::<Utc>::from),
        });
    }
    out
}

/// Remote files relative to `prefix` (folder markers dropped).
pub fn remote_files(
    prefix: &str,
    objects: &[(ListedObject, Option<DateTime<Utc>>)],
) -> Vec<RemoteFile> {
    objects
        .iter()
        .filter(|(o, _)| !o.key.ends_with('/'))
        .filter_map(|(o, modified)| {
            Some(RemoteFile {
                relative: o.key.strip_prefix(prefix)?.to_owned(),
                key: o.key.clone(),
                size: o.size,
                modified: *modified,
            })
        })
        .collect()
}

/// Compares both sides.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    account_id: Uuid,
    bucket: &str,
    prefix: &str,
    root: &Path,
    direction: SyncDirection,
    delete_extra: bool,
    local: &[LocalFile],
    remote: &[RemoteFile],
) -> SyncPlan {
    let local_map: BTreeMap<&str, &LocalFile> =
        local.iter().map(|f| (f.relative.as_str(), f)).collect();
    let remote_map: BTreeMap<&str, &RemoteFile> =
        remote.iter().map(|f| (f.relative.as_str(), f)).collect();
    let mut plan = SyncPlan::default();
    match direction {
        SyncDirection::Upload => {
            for (rel, l) in &local_map {
                let upload = match remote_map.get(rel) {
                    None => true,
                    Some(r) => r.size != l.size || newer(l.modified, r.modified),
                };
                if upload {
                    plan.bytes += l.size;
                    plan.transfers.push(TransferRequest {
                        account_id,
                        direction: TransferDirection::Upload,
                        bucket: bucket.to_owned(),
                        key: format!("{prefix}{rel}"),
                        local_path: l.path.clone(),
                        size_hint: l.size,
                        source: None,
                        version_id: None,
                    });
                } else {
                    plan.unchanged += 1;
                }
            }
            if delete_extra {
                plan.deletes = remote_map
                    .iter()
                    .filter(|(rel, _)| !local_map.contains_key(*rel))
                    .map(|(_, r)| r.key.clone())
                    .collect();
            }
        }
        SyncDirection::Download => {
            for (rel, r) in &remote_map {
                match local_map.get(rel) {
                    None => match paths::map(root, &r.key, prefix) {
                        Ok(local_path) => {
                            plan.bytes += r.size;
                            plan.transfers.push(TransferRequest {
                                account_id,
                                direction: TransferDirection::Download,
                                bucket: bucket.to_owned(),
                                key: r.key.clone(),
                                local_path,
                                size_hint: r.size,
                                source: None,
                                version_id: None,
                            });
                        }
                        Err(e) => plan.skipped.push(((*rel).to_owned(), e.reason)),
                    },
                    Some(l) if l.size != r.size || newer(r.modified, l.modified) => {
                        plan.skipped.push((
                            (*rel).to_owned(),
                            "differs from the local file; local files are never overwritten".into(),
                        ))
                    }
                    Some(_) => plan.unchanged += 1,
                }
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> Option<DateTime<Utc>> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).single()
    }

    fn local(rel: &str, size: u64, m: i64) -> LocalFile {
        LocalFile {
            relative: rel.into(),
            path: PathBuf::from("root").join(rel),
            size,
            modified: t(m),
        }
    }

    fn remote(rel: &str, size: u64, m: i64) -> RemoteFile {
        RemoteFile {
            relative: rel.into(),
            key: format!("p/{rel}"),
            size,
            modified: t(m),
        }
    }

    #[test]
    fn upload_new_and_changed_only() {
        let l = [
            local("a.txt", 1, 0),
            local("b.txt", 2, 100),
            local("c.txt", 3, 0),
            local("d/e.txt", 4, 1),
        ];
        let r = [
            remote("a.txt", 1, 0),
            remote("b.txt", 2, 0),
            remote("c.txt", 9, 0),
            remote("old.txt", 5, 0),
        ];
        let p = plan(
            Uuid::nil(),
            "b",
            "p/",
            Path::new("root"),
            SyncDirection::Upload,
            true,
            &l,
            &r,
        );
        let keys: Vec<_> = p.transfers.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["p/b.txt", "p/c.txt", "p/d/e.txt"]);
        assert_eq!(p.unchanged, 1);
        assert_eq!(p.deletes, ["p/old.txt"]);
        assert_eq!(p.bytes, 9);
        let p = plan(
            Uuid::nil(),
            "b",
            "p/",
            Path::new("root"),
            SyncDirection::Upload,
            false,
            &l,
            &r,
        );
        assert!(p.deletes.is_empty());
    }

    #[test]
    fn download_new_files_and_reports_conflicts() {
        let l = [local("a.txt", 1, 0), local("b.txt", 2, 0)];
        let r = [
            remote("a.txt", 1, 1),
            remote("b.txt", 7, 0),
            remote("new/x.txt", 3, 0),
            remote("bad/CON", 1, 0),
        ];
        let base = std::env::temp_dir().join("s3m-sync");
        let p = plan(
            Uuid::nil(),
            "b",
            "p/",
            &base,
            SyncDirection::Download,
            true,
            &l,
            &r,
        );
        assert_eq!(p.transfers.len(), 1);
        assert!(
            p.transfers[0]
                .local_path
                .ends_with(Path::new("new").join("x.txt"))
        );
        assert_eq!(p.unchanged, 1);
        assert_eq!(p.skipped.len(), 2);
        assert!(p.deletes.is_empty(), "download never deletes");
    }

    #[test]
    fn scans_a_folder() {
        let dir = std::env::temp_dir().join(format!("s3m-scan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("sub").join("f.txt"), b"abc").expect("write");
        let files = scan_local(&dir);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative, "sub/f.txt");
        assert_eq!(files[0].size, 3);
        let remote = remote_files(
            "p/",
            &[
                (
                    ListedObject {
                        key: "p/sub/".into(),
                        size: 0,
                    },
                    None,
                ),
                (
                    ListedObject {
                        key: "p/sub/f.txt".into(),
                        size: 3,
                    },
                    None,
                ),
            ],
        );
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].relative, "sub/f.txt");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
