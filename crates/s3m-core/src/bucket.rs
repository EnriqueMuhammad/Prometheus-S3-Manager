//! Bucket naming rules.

/// AWS bucket naming rules (S3-compatible gateways follow them too): 3–63 characters of
/// lowercase letters, digits, `.` and `-`, starting and ending with a letter or digit, no
/// `..`, `.-` or `-.`, and not shaped like an IPv4 address.
pub fn is_valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    if !(3..=63).contains(&bytes.len()) {
        return false;
    }
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let inner = |b: u8| edge(b) || b == b'.' || b == b'-';
    if !edge(bytes[0]) || !edge(bytes[bytes.len() - 1]) || !bytes.iter().all(|&b| inner(b)) {
        return false;
    }
    if name.contains("..") || name.contains(".-") || name.contains("-.") {
        return false;
    }
    !looks_like_ipv4(name)
}

fn looks_like_ipv4(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Trimmed, lowercased form used when storing a registered bucket.
pub fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        for name in ["po-archive", "my.bucket.01", "abc", "a1b"] {
            assert!(is_valid(name), "{name}");
        }
    }

    #[test]
    fn rejects_bad_names() {
        for name in [
            "ab",
            "Uppercase",
            "-start",
            "end-",
            "a..b",
            "a.-b",
            "a-.b",
            "192.168.1.1",
            "under_score",
            &"x".repeat(64),
        ] {
            assert!(!is_valid(name), "{name}");
        }
    }
}
