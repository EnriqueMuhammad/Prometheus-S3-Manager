//! Browsing models and object-key helpers.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// A bucket in the left panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketEntry {
    /// Bucket name.
    pub name: String,
    /// Creation date reported by ListBuckets.
    pub creation_date: Option<DateTime<Utc>>,
    /// Registered by hand on the account (not from ListBuckets).
    pub manual: bool,
}

/// Result of listing buckets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketListing {
    /// Buckets: listed ones first, then registered ones not already listed.
    pub buckets: Vec<BucketEntry>,
    /// `true` when ListBuckets was denied and only registered buckets are shown.
    pub restricted: bool,
}

/// A folder (common prefix) or object in the file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectEntry {
    /// Full key (folders end with `/`).
    pub key: String,
    /// Name relative to the listing prefix.
    pub name: String,
    /// Folder (common prefix or zero-byte `/` marker).
    pub is_folder: bool,
    /// Size in bytes (0 for folders).
    pub size: u64,
    /// Last modification time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Storage class reported by the listing.
    pub storage_class: Option<String>,
    /// ETag reported by the listing.
    pub etag: Option<String>,
}

impl ObjectEntry {
    /// "Folder", the upper-case extension, or "File".
    pub fn type_label(&self) -> String {
        if self.is_folder {
            return "Folder".into();
        }
        match self.name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_uppercase(),
            _ => "File".into(),
        }
    }
}

/// One page of a prefix listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPage {
    /// Folders first, then objects, in server order.
    pub entries: Vec<ObjectEntry>,
    /// Token for the next page, when the listing was truncated.
    pub next_token: Option<String>,
}

/// HEAD result shown in the Properties tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectDetails {
    /// Key.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Content-Type.
    pub content_type: Option<String>,
    /// ETag.
    pub etag: Option<String>,
    /// Storage class.
    pub storage_class: Option<String>,
    /// Version id when versioning is enabled.
    pub version_id: Option<String>,
    /// Cache-Control.
    pub cache_control: Option<String>,
    /// Content-Disposition.
    pub content_disposition: Option<String>,
    /// Content-Encoding.
    pub content_encoding: Option<String>,
    /// Content-Language.
    pub content_language: Option<String>,
    /// User metadata (`x-amz-meta-*`).
    pub metadata: BTreeMap<String, String>,
}

/// Editable HTTP headers and user metadata of an object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderEdit {
    /// Content-Type.
    pub content_type: String,
    /// Cache-Control.
    pub cache_control: String,
    /// Content-Disposition.
    pub content_disposition: String,
    /// Content-Encoding.
    pub content_encoding: String,
    /// Content-Language.
    pub content_language: String,
    /// User metadata (keys without the `x-amz-meta-` prefix).
    pub metadata: Vec<(String, String)>,
}

impl HeaderEdit {
    /// Editable copy of an object's current headers.
    pub fn from_details(d: &ObjectDetails) -> Self {
        Self {
            content_type: d.content_type.clone().unwrap_or_default(),
            cache_control: d.cache_control.clone().unwrap_or_default(),
            content_disposition: d.content_disposition.clone().unwrap_or_default(),
            content_encoding: d.content_encoding.clone().unwrap_or_default(),
            content_language: d.content_language.clone().unwrap_or_default(),
            metadata: d
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// Validates metadata keys and values (printable ASCII, no duplicates, 2 KB total).
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut total = 0;
        for (k, v) in &self.metadata {
            let key = k.trim().to_ascii_lowercase();
            if key.is_empty() {
                return Err("Metadata names must not be empty.".into());
            }
            if !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return Err(format!(
                    "Metadata name \"{k}\" may only contain letters, digits, '-', '_' and '.'."
                ));
            }
            if !v.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
                return Err(format!(
                    "Metadata value of \"{k}\" must be printable ASCII."
                ));
            }
            if !seen.insert(key.clone()) {
                return Err(format!("Metadata name \"{key}\" is used twice."));
            }
            total += key.len() + v.len();
        }
        if total > 2048 {
            return Err("User metadata is limited to 2 KB.".into());
        }
        Ok(())
    }
}

/// One version of an object (or a delete marker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectVersion {
    /// Version id (`null` for objects written before versioning was enabled).
    pub version_id: String,
    /// When this version was written.
    pub last_modified: Option<DateTime<Utc>>,
    /// Size in bytes (0 for delete markers).
    pub size: u64,
    /// The current version.
    pub is_latest: bool,
    /// A delete marker rather than content.
    pub delete_marker: bool,
    /// ETag.
    pub etag: Option<String>,
}

/// Bucket facts shown in Properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketInfo {
    /// Bucket name.
    pub name: String,
    /// Region reported by GetBucketLocation (empty for us-east-1 / unknown).
    pub region: Option<String>,
    /// Versioning status (`Enabled`, `Suspended`, or never enabled).
    pub versioning: Option<String>,
}

/// Outcome of "Test connection".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionTestResult {
    /// Whether the credentials work.
    pub ok: bool,
    /// User-facing summary.
    pub message: String,
    /// Round-trip time.
    pub latency: Duration,
    /// Buckets visible to the key.
    pub buckets_visible: usize,
    /// ListBuckets was denied.
    pub restricted: bool,
}

/// Display name of a key relative to the listing prefix (`a/b/c.txt` under `a/b/` → `c.txt`).
pub fn name_under_prefix(key: &str, prefix: &str) -> String {
    key.strip_prefix(prefix)
        .unwrap_or(key)
        .trim_end_matches('/')
        .to_owned()
}

/// Breadcrumb segments of a prefix: `a/b/` → `[("a", "a/"), ("b", "a/b/")]`.
pub fn breadcrumbs(prefix: &str) -> Vec<(String, String)> {
    let mut current = String::new();
    prefix
        .split('/')
        .filter(|p| !p.is_empty())
        .map(|part| {
            current.push_str(part);
            current.push('/');
            (part.to_owned(), current.clone())
        })
        .collect()
}

/// Parent of a prefix (`a/b/` → `a/`, `a/` → ``).
pub fn parent_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    trimmed
        .rfind('/')
        .map_or_else(String::new, |i| trimmed[..=i].to_owned())
}

/// Last segment of a key (`a/b/c.txt` → `c.txt`, `a/b/` → `b`).
pub fn leaf_name(key: &str) -> &str {
    key.trim_end_matches('/').rsplit('/').next().unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_helpers() {
        assert_eq!(name_under_prefix("a/b/c.txt", "a/b/"), "c.txt");
        assert_eq!(name_under_prefix("a/b/sub/", "a/b/"), "sub");
        assert_eq!(
            breadcrumbs("a/b/"),
            vec![("a".into(), "a/".into()), ("b".into(), "a/b/".into())]
        );
        assert_eq!(parent_prefix("a/b/"), "a/");
        assert_eq!(parent_prefix("a/"), "");
        assert_eq!(leaf_name("a/b/c.txt"), "c.txt");
        assert_eq!(leaf_name("a/b/"), "b");
    }

    #[test]
    fn header_validation() {
        let mut h = HeaderEdit {
            metadata: vec![("owner".into(), "finance".into())],
            ..HeaderEdit::default()
        };
        assert!(h.validate().is_ok());
        h.metadata.push(("Owner".into(), "x".into()));
        assert!(h.validate().unwrap_err().contains("twice"));
        h.metadata = vec![("bad key".into(), "x".into())];
        assert!(h.validate().is_err());
        h.metadata = vec![("k".into(), "é".into())];
        assert!(h.validate().is_err());
    }

    #[test]
    fn type_labels() {
        let mut e = ObjectEntry {
            key: "x/report.pdf".into(),
            name: "report.pdf".into(),
            is_folder: false,
            size: 1,
            last_modified: None,
            storage_class: None,
            etag: None,
        };
        assert_eq!(e.type_label(), "PDF");
        e.name = "Makefile".into();
        assert_eq!(e.type_label(), "File");
        e.name = ".env".into();
        assert_eq!(e.type_label(), "File");
        e.is_folder = true;
        assert_eq!(e.type_label(), "Folder");
    }
}
