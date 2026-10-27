//! Planning of folder operations: paste (copy/move), folder rename and folder download.
//! Pure functions over keys, so the rules are testable without a server.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::browse::parent_prefix;
use crate::paths;
use crate::transfer::{ObjectRef, TransferDirection, TransferRequest};

/// An object found by a recursive listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedObject {
    /// Full key (folder markers end with `/`).
    pub key: String,
    /// Size in bytes.
    pub size: u64,
}

/// What was copied or cut in the file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    /// Account the objects belong to.
    pub account_id: Uuid,
    /// Bucket of the objects.
    pub bucket: String,
    /// Selected keys (folders end with `/`).
    pub keys: Vec<String>,
    /// Move instead of copy.
    pub cut: bool,
}

/// Checks that `key` (a file or folder) may be pasted into `dest_bucket/dest_prefix`.
pub fn check_paste(
    source_bucket: &str,
    key: &str,
    dest_bucket: &str,
    dest_prefix: &str,
) -> Result<(), String> {
    if source_bucket != dest_bucket {
        return Ok(());
    }
    if key.ends_with('/') && dest_prefix.starts_with(key) {
        return Err(format!("Cannot paste the folder \"{key}\" into itself."));
    }
    if parent_prefix(key) == dest_prefix {
        return Err("Source and destination are the same folder.".into());
    }
    Ok(())
}

/// Destination key when an object under `source_parent` is pasted into `dest_prefix`:
/// `a/b/c.txt` copied from `a/` into `x/` becomes `x/b/c.txt`.
pub fn rebase(key: &str, source_parent: &str, dest_prefix: &str) -> String {
    format!(
        "{dest_prefix}{}",
        key.strip_prefix(source_parent).unwrap_or(key)
    )
}

/// Copy or move requests for pasting `objects` (already expanded from the selected `roots`).
pub fn paste_requests(
    clipboard: &Clipboard,
    objects: &[ListedObject],
    dest_account: Uuid,
    dest_bucket: &str,
    dest_prefix: &str,
) -> Vec<TransferRequest> {
    let direction = if clipboard.cut {
        TransferDirection::Move
    } else {
        TransferDirection::Copy
    };
    objects
        .iter()
        .map(|o| {
            // Each object keeps its path relative to the parent of the selected root.
            let root = clipboard
                .keys
                .iter()
                .find(|k| o.key == **k || (k.ends_with('/') && o.key.starts_with(k.as_str())))
                .map_or(o.key.as_str(), String::as_str);
            TransferRequest {
                account_id: dest_account,
                direction,
                bucket: dest_bucket.to_owned(),
                key: rebase(&o.key, &parent_prefix(root), dest_prefix),
                local_path: PathBuf::new(),
                size_hint: o.size,
                version_id: None,
                source: Some(ObjectRef {
                    bucket: clipboard.bucket.clone(),
                    key: o.key.clone(),
                    account_id: (clipboard.account_id != dest_account)
                        .then_some(clipboard.account_id),
                }),
            }
        })
        .collect()
}

/// Folder prefixes inside the selected `roots` that contain `keys`, deepest first
/// (`a/b/c.txt` under root `a/` gives `a/b/`, `a/`). Deleting them after the files removes
/// leftover folder markers and, on servers with real directories, the emptied folders.
pub fn folder_prefixes(keys: &[String], roots: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        let mut prefix = if key.ends_with('/') {
            key.clone()
        } else {
            parent_prefix(key)
        };
        while !prefix.is_empty() && roots.iter().any(|r| prefix.starts_with(r.as_str())) {
            if !out.contains(&prefix) {
                out.push(prefix.clone());
            }
            prefix = parent_prefix(&prefix);
        }
    }
    out.sort_by(|a, b| {
        b.matches('/')
            .count()
            .cmp(&a.matches('/').count())
            .then_with(|| a.cmp(b))
    });
    out
}

/// Validates a new folder name for "rename folder".
pub fn check_folder_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err("The new name must not be empty or contain \"/\".".into());
    }
    Ok(())
}

/// Move requests that rename the folder `old_prefix` (ending with `/`) to `new_prefix`.
pub fn rename_folder_requests(
    account_id: Uuid,
    bucket: &str,
    old_prefix: &str,
    new_prefix: &str,
    objects: &[ListedObject],
) -> Vec<TransferRequest> {
    objects
        .iter()
        .map(|o| TransferRequest {
            account_id,
            direction: TransferDirection::Move,
            bucket: bucket.to_owned(),
            key: rebase(&o.key, old_prefix, new_prefix),
            local_path: PathBuf::new(),
            size_hint: o.size,
            version_id: None,
            source: Some(ObjectRef {
                bucket: bucket.to_owned(),
                key: o.key.clone(),
                account_id: None,
            }),
        })
        .collect()
}

/// Result of planning a folder download.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DownloadPlan {
    /// Download requests for files.
    pub requests: Vec<TransferRequest>,
    /// Local folders to create for (possibly empty) folder markers.
    pub folders: Vec<PathBuf>,
    /// Keys that cannot be stored locally, with the reason.
    pub rejected: Vec<String>,
}

/// Maps listed objects to local paths under `base`, keeping their path relative to `strip`
/// (the folder currently shown, so a downloaded folder keeps its own name).
pub fn download_plan(
    account_id: Uuid,
    bucket: &str,
    base: &Path,
    strip: &str,
    objects: &[ListedObject],
) -> DownloadPlan {
    let mut plan = DownloadPlan::default();
    for o in objects {
        if o.key.ends_with('/') {
            let trimmed = o.key.trim_end_matches('/');
            if trimmed.len() <= strip.trim_end_matches('/').len() {
                continue;
            }
            match paths::map(base, trimmed, strip) {
                Ok(dir) => plan.folders.push(dir),
                Err(e) => plan.rejected.push(e.to_string()),
            }
            continue;
        }
        match paths::map(base, &o.key, strip) {
            Ok(local_path) => plan.requests.push(TransferRequest {
                account_id,
                direction: TransferDirection::Download,
                bucket: bucket.to_owned(),
                key: o.key.clone(),
                local_path,
                size_hint: o.size,
                version_id: None,
                source: None,
            }),
            Err(e) => plan.rejected.push(e.to_string()),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(key: &str, size: u64) -> ListedObject {
        ListedObject {
            key: key.into(),
            size,
        }
    }

    #[test]
    fn paste_rules() {
        assert!(check_paste("b", "a/x.txt", "b", "c/").is_ok());
        assert!(check_paste("b", "a/x.txt", "other", "a/").is_ok());
        assert!(
            check_paste("b", "a/x.txt", "b", "a/")
                .unwrap_err()
                .contains("same folder")
        );
        assert!(
            check_paste("b", "a/", "b", "a/sub/")
                .unwrap_err()
                .contains("into itself")
        );
        assert!(
            check_paste("b", "a/", "b", "a/")
                .unwrap_err()
                .contains("into itself")
        );
        assert!(
            check_paste("b", "a/", "b", "")
                .unwrap_err()
                .contains("same folder")
        );
        assert!(check_paste("b", "a/", "b", "ab/").is_ok());
    }

    #[test]
    fn paste_keeps_relative_paths() {
        let clip = Clipboard {
            account_id: Uuid::nil(),
            bucket: "src".into(),
            keys: vec!["2026/10/".into(), "2026/readme.txt".into()],
            cut: true,
        };
        let objects = [
            obj("2026/10/a.pdf", 5),
            obj("2026/10/sub/b.pdf", 6),
            obj("2026/10/", 0),
            obj("2026/readme.txt", 1),
        ];
        let reqs = paste_requests(&clip, &objects, Uuid::nil(), "dst", "archive/");
        let keys: Vec<_> = reqs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "archive/10/a.pdf",
                "archive/10/sub/b.pdf",
                "archive/10/",
                "archive/readme.txt"
            ]
        );
        assert!(
            reqs.iter()
                .all(|r| r.direction == TransferDirection::Move && r.bucket == "dst")
        );
        assert_eq!(
            reqs[0].source.as_ref().map(|s| s.key.as_str()),
            Some("2026/10/a.pdf")
        );
    }

    #[test]
    fn prefixes_deepest_first() {
        let keys = [
            "a/b/c.txt".to_owned(),
            "a/d.txt".to_owned(),
            "x/y.txt".to_owned(),
        ];
        assert_eq!(folder_prefixes(&keys, &["a/".to_owned()]), ["a/b/", "a/"]);
        assert!(folder_prefixes(&keys, &[]).is_empty());
    }

    #[test]
    fn folder_rename() {
        assert!(check_folder_name("new").is_ok());
        assert!(check_folder_name("a/b").is_err());
        let reqs = rename_folder_requests(
            Uuid::nil(),
            "b",
            "docs/old/",
            "docs/new/",
            &[obj("docs/old/", 0), obj("docs/old/x/y.txt", 3)],
        );
        let keys: Vec<_> = reqs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["docs/new/", "docs/new/x/y.txt"]);
    }

    #[test]
    fn folder_download() {
        let base = std::env::temp_dir().join("s3m-plan");
        let plan = download_plan(
            Uuid::nil(),
            "b",
            &base,
            "2026/",
            &[
                obj("2026/10/", 0),
                obj("2026/10/a.pdf", 5),
                obj("2026/10/empty/", 0),
                obj("2026/10/CON", 1),
            ],
        );
        assert_eq!(plan.requests.len(), 1);
        assert!(
            plan.requests[0]
                .local_path
                .ends_with(Path::new("10").join("a.pdf"))
        );
        assert_eq!(plan.folders.len(), 2);
        assert!(plan.folders[1].ends_with(Path::new("10").join("empty")));
        assert_eq!(plan.rejected.len(), 1);
    }
}
