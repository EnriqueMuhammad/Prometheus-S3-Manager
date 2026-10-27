//! Update checks against GitHub Releases: release parsing, version comparison and checksums.
//! Downloading and installing live in `s3m-update`.

use chrono::{DateTime, Utc};
use semver::Version;
use serde::Deserialize;

/// Name suffix of the installer asset the updater installs.
pub const MSI_SUFFIX: &str = "-x64.msi";
/// Checksum file published with every release.
pub const CHECKSUMS: &str = "SHA256SUMS.txt";

/// A downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name.
    pub name: String,
    /// Download URL.
    pub url: String,
    /// Size in bytes.
    pub size: u64,
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Version from the tag.
    pub version: Version,
    /// Tag, e.g. `v0.3.0`.
    pub tag: String,
    /// Release title (the tag when empty).
    pub name: String,
    /// Release notes (Markdown as written on GitHub).
    pub notes: String,
    /// Release page in the browser.
    pub page: String,
    /// Publication time.
    pub published: Option<DateTime<Utc>>,
    /// The MSI installer.
    pub installer: Option<Asset>,
    /// `SHA256SUMS.txt`.
    pub checksums: Option<Asset>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Version from a tag such as `v1.2.3` or `1.2.3`.
pub fn parse_version(tag: &str) -> Option<Version> {
    let t = tag.trim();
    Version::parse(t.strip_prefix(['v', 'V']).unwrap_or(t)).ok()
}

/// Parses GitHub's "latest release" response. Drafts, pre-releases and tags that are not a
/// version are rejected with a user-facing message.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let r: GhRelease = serde_json::from_str(json)
        .map_err(|e| format!("GitHub returned an unexpected answer: {e}"))?;
    if r.draft || r.prerelease {
        return Err(format!("Release {} is not a final release.", r.tag_name));
    }
    let version = parse_version(&r.tag_name)
        .ok_or_else(|| format!("Release tag \"{}\" is not a version number.", r.tag_name))?;
    let pick = |pred: &dyn Fn(&str) -> bool| {
        r.assets.iter().find(|a| pred(&a.name)).map(|a| Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    };
    let installer = pick(&|n| n.to_ascii_lowercase().ends_with(MSI_SUFFIX));
    let checksums = pick(&|n| n.eq_ignore_ascii_case(CHECKSUMS));
    Ok(Release {
        version,
        name: r
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| r.tag_name.clone()),
        tag: r.tag_name,
        notes: r.body.unwrap_or_default(),
        page: r.html_url,
        published: r.published_at,
        installer,
        checksums,
    })
}

/// Whether `release` is newer than the running version `current`.
pub fn is_newer(release: &Release, current: &str) -> bool {
    parse_version(current).is_some_and(|c| release.version > c)
}

/// Hex SHA-256 listed for `name` in a `SHA256SUMS.txt` (`<hex>  name` or `<hex> *name`).
pub fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Lowercase hex of a digest.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether an automatic check is due: never checked, or the last check is 20 hours old.
pub fn check_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last.is_none_or(|l| now - l >= chrono::Duration::hours(20) || l > now)
}

/// Whether `repo` looks like `owner/name`.
pub fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split_once('/'), Some((o, n)) if ok(o) && ok(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATEST: &str = r#"{
        "tag_name": "v0.3.0",
        "name": "",
        "body": "Faster uploads",
        "html_url": "https://github.com/acme/s3m/releases/tag/v0.3.0",
        "draft": false,
        "prerelease": false,
        "published_at": "2026-11-01T08:00:00Z",
        "assets": [
            {"name": "Prometheus-S3Manager-0.3.0-x64.exe", "browser_download_url": "https://x/exe", "size": 10},
            {"name": "Prometheus-S3Manager-0.3.0-x64.msi", "browser_download_url": "https://x/msi", "size": 20},
            {"name": "SHA256SUMS.txt", "browser_download_url": "https://x/sums", "size": 3}
        ]
    }"#;

    #[test]
    fn parses_latest_release() {
        let r = parse_release(LATEST).unwrap();
        assert_eq!(r.version, Version::new(0, 3, 0));
        assert_eq!(r.name, "v0.3.0");
        assert_eq!(r.notes, "Faster uploads");
        assert_eq!(r.installer.as_ref().unwrap().url, "https://x/msi");
        assert_eq!(r.installer.as_ref().unwrap().size, 20);
        assert_eq!(r.checksums.as_ref().unwrap().name, "SHA256SUMS.txt");
        assert!(is_newer(&r, "0.2.0"));
        assert!(!is_newer(&r, "0.3.0"));
        assert!(!is_newer(&r, "0.4.0-beta.1"));
        assert!(is_newer(&r, "0.3.0-rc.1"));
    }

    #[test]
    fn rejects_drafts_prereleases_and_odd_tags() {
        let pre = LATEST.replace("\"prerelease\": false", "\"prerelease\": true");
        assert!(parse_release(&pre).is_err());
        let odd = LATEST.replace("v0.3.0\",\n        \"name", "nightly\",\n        \"name");
        assert!(parse_release(&odd).unwrap_err().contains("nightly"));
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn checksums() {
        let h = "A".repeat(64);
        let sums = format!(
            "{h}  Prometheus-S3Manager-0.3.0-x64.msi\n{}  *other.exe\n",
            "b".repeat(64)
        );
        assert_eq!(
            checksum_for(&sums, "Prometheus-S3Manager-0.3.0-x64.msi"),
            Some("a".repeat(64))
        );
        assert_eq!(checksum_for(&sums, "other.exe"), Some("b".repeat(64)));
        assert_eq!(checksum_for(&sums, "missing.msi"), None);
        assert_eq!(checksum_for("short  x.msi", "x.msi"), None);
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }

    #[test]
    fn due_and_repo() {
        let now = Utc::now();
        assert!(check_due(None, now));
        assert!(!check_due(Some(now - chrono::Duration::hours(2)), now));
        assert!(check_due(Some(now - chrono::Duration::hours(21)), now));
        assert!(check_due(Some(now + chrono::Duration::hours(5)), now));
        assert!(valid_repo("acme/prometheus-s3manager"));
        assert!(!valid_repo("acme"));
        assert!(!valid_repo("acme/x/y"));
        assert!(!valid_repo("/x"));
    }
}
