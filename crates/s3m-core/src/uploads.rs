//! Turns picked or dropped files and folders into upload requests.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::paths;
use crate::transfer::{TransferDirection, TransferRequest};

/// Files found under the given paths, as upload requests into `bucket/prefix`.
///
/// Folders are walked recursively; symbolic links, junctions and system files are skipped, and
/// unreadable entries are ignored (like Explorer's copy). A dropped folder keeps its own name:
/// `C:\data\photos` uploads to `prefix + "photos/..."`.
pub fn collect(
    account_id: Uuid,
    bucket: &str,
    prefix: &str,
    paths: &[PathBuf],
) -> Vec<TransferRequest> {
    let mut requests = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if meta.is_file() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.is_empty() {
                requests.push(request(
                    account_id,
                    bucket,
                    format!("{prefix}{name}"),
                    path,
                    meta.len(),
                ));
            }
        } else if meta.is_dir() {
            let root = path.parent().unwrap_or(Path::new(""));
            for entry in walkdir::WalkDir::new(path)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|e| !skip(e))
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                if let Ok(key) = paths::key_for_upload(prefix, root, entry.path()) {
                    requests.push(request(account_id, bucket, key, entry.path(), meta.len()));
                }
            }
        }
    }
    requests
}

fn request(account_id: Uuid, bucket: &str, key: String, path: &Path, size: u64) -> TransferRequest {
    TransferRequest {
        account_id,
        direction: TransferDirection::Upload,
        bucket: bucket.to_owned(),
        key,
        local_path: path.to_path_buf(),
        size_hint: size,
        version_id: None,
        source: None,
    }
}

fn skip(entry: &walkdir::DirEntry) -> bool {
    if entry.path_is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const SYSTEM: u32 = 0x4;
        const REPARSE_POINT: u32 = 0x400;
        if let Ok(meta) = entry.metadata()
            && meta.file_attributes() & (SYSTEM | REPARSE_POINT) != 0
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_files_and_folders() {
        let dir = std::env::temp_dir().join(format!("s3m-up-{}", Uuid::new_v4()));
        let photos = dir.join("photos");
        std::fs::create_dir_all(photos.join("2026")).expect("mkdir");
        std::fs::write(photos.join("a.jpg"), b"aa").expect("write");
        std::fs::write(photos.join("2026").join("b.jpg"), b"bbb").expect("write");
        std::fs::write(dir.join("single.txt"), b"s").expect("write");

        let id = Uuid::new_v4();
        let reqs = collect(id, "bkt", "in/", &[photos.clone(), dir.join("single.txt")]);
        let mut keys: Vec<_> = reqs.iter().map(|r| (r.key.clone(), r.size_hint)).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                ("in/photos/2026/b.jpg".to_owned(), 3),
                ("in/photos/a.jpg".to_owned(), 2),
                ("in/single.txt".to_owned(), 1)
            ]
        );
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
