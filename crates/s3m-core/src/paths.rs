//! Maps S3 object keys to local paths without letting a hostile key escape the chosen folder or
//! hit a Windows device name. Keys that cannot be mapped faithfully are rejected rather than
//! silently renamed. Windows rules are applied on every platform so behaviour is consistent.

use std::path::{Component, Path, PathBuf};

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

const INVALID: &[char] = &['<', '>', ':', '"', '|', '?', '*', '\\'];

/// A key that cannot be stored locally.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Cannot save \"{key}\" locally: {reason}.")]
pub struct UnsafePath {
    /// The offending key or path.
    pub key: String,
    /// Why it was rejected.
    pub reason: String,
}

/// Returns `None` when the segment is a safe Windows file name, otherwise the reason.
pub fn check_segment(segment: &str) -> Option<String> {
    if segment.is_empty() {
        return Some("empty path segment".into());
    }
    if segment == "." || segment == ".." {
        return Some("relative path segment".into());
    }
    if segment.chars().count() > 255 {
        return Some("path segment longer than 255 characters".into());
    }
    if segment
        .chars()
        .any(|c| (c as u32) < 32 || INVALID.contains(&c))
    {
        return Some("character not allowed in Windows file names".into());
    }
    if segment.ends_with('.') || segment.ends_with(' ') {
        return Some("Windows names cannot end with a dot or space".into());
    }
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches(' ');
    let upper = stem.to_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return Some(format!("\"{stem}\" is a reserved Windows device name"));
    }
    None
}

/// Maps `key` (minus `strip_prefix`) under `base`.
pub fn map(base: &Path, key: &str, strip_prefix: &str) -> Result<PathBuf, UnsafePath> {
    let fail = |reason: &str| UnsafePath {
        key: key.to_owned(),
        reason: reason.to_owned(),
    };
    let relative = key.strip_prefix(strip_prefix).unwrap_or(key);
    if relative.is_empty() || relative.ends_with('/') {
        return Err(fail("key does not name a file"));
    }
    let segments: Vec<&str> = relative.split('/').collect();
    for segment in &segments {
        if let Some(reason) = check_segment(segment) {
            return Err(fail(&reason));
        }
    }
    let root = std::path::absolute(base).map_err(|_| fail("download folder is not valid"))?;
    let mut full = root.clone();
    for segment in segments {
        full.push(segment);
    }
    let escapes = full.strip_prefix(&root).map_or(true, |rest| {
        rest.components()
            .any(|c| !matches!(c, Component::Normal(_)))
    });
    if escapes {
        return Err(fail("path escapes the download folder"));
    }
    Ok(full)
}

/// Object key for a local file uploaded from `source_root` into `prefix`.
pub fn key_for_upload(prefix: &str, source_root: &Path, file: &Path) -> Result<String, UnsafePath> {
    let fail = || UnsafePath {
        key: file.display().to_string(),
        reason: "file is outside the selected folder".into(),
    };
    let relative = file.strip_prefix(source_root).map_err(|_| fail())?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return Err(fail()),
        }
    }
    if parts.is_empty() {
        return Err(fail());
    }
    Ok(format!("{prefix}{}", parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments() {
        assert_eq!(check_segment("report.pdf"), None);
        for bad in [
            "", ".", "..", "a:b", "a\\b", "x.", "x ", "CON", "con.txt", "LPT1.log", "COM¹",
        ] {
            assert!(check_segment(bad).is_some(), "{bad:?}");
        }
        assert!(check_segment("console.txt").is_none());
    }

    #[test]
    fn maps_inside_base() {
        let base = std::env::temp_dir().join("s3m-map");
        let p = map(&base, "a/b/c.txt", "a/").expect("map");
        assert!(p.ends_with(Path::new("b").join("c.txt")));
        assert!(p.starts_with(std::path::absolute(&base).expect("abs")));
    }

    #[test]
    fn rejects_hostile_keys() {
        let base = std::env::temp_dir();
        for key in [
            "../evil.txt",
            "a/../../x",
            "dir/",
            "",
            "a//b",
            "C:/x",
            "a/NUL",
        ] {
            assert!(map(&base, key, "").is_err(), "{key:?}");
        }
    }

    #[test]
    fn upload_keys() {
        let root = Path::new("data");
        let file = root.join("photos").join("a.jpg");
        assert_eq!(
            key_for_upload("in/", root, &file).expect("key"),
            "in/photos/a.jpg"
        );
        assert!(key_for_upload("", root, Path::new("other/x")).is_err());
    }
}
