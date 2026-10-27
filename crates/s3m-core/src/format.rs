//! Display formatting shared by the UI and the transfer queue.

/// Human-readable byte size: `512 B`, `1.5 KB`, `12.0 MB`, `1.25 GB`.
pub fn size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    match bytes {
        0..KB => format!("{bytes} B"),
        KB..MB => format!("{:.1} KB", b / KB as f64),
        MB..GB => format!("{:.1} MB", b / MB as f64),
        _ => format!("{:.2} GB", b / GB as f64),
    }
}

/// Integer with thousands separators: `1234567` → `1,234,567`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Compact duration: `42s`, `3m 05s`, `1h 02m`.
pub fn duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(12 * 1024 * 1024), "12.0 MB");
        assert_eq!(size(5 * 1024 * 1024 * 1024 / 4), "1.25 GB");
    }

    #[test]
    fn durations() {
        use std::time::Duration;
        assert_eq!(duration(Duration::from_secs(42)), "42s");
        assert_eq!(duration(Duration::from_secs(185)), "3m 05s");
        assert_eq!(duration(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn counts() {
        assert_eq!(count(999), "999");
        assert_eq!(count(1_234_567), "1,234,567");
    }
}
