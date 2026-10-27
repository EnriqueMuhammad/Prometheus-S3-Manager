//! Daily rolling log file. Every line passes through [`redact`] first.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use regex::Regex;

static ACCESS_KEY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").ok());
static SECRET_QUERY: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(X-Amz-Signature|X-Amz-Credential|X-Amz-Security-Token|Signature|secret[A-Za-z]*|password)=([^&\s"]+)"#,
    )
    .ok()
});

/// Removes AWS access key ids and signature/secret query values from a log line.
pub fn redact(text: &str) -> String {
    let step = ACCESS_KEY.as_ref().map_or_else(
        || text.to_owned(),
        |r| r.replace_all(text, "[redacted-key]").into_owned(),
    );
    SECRET_QUERY.as_ref().map_or(step.clone(), |r| {
        r.replace_all(&step, "$1=[redacted]").into_owned()
    })
}

/// `io::Write` sink for `tracing-subscriber` that appends redacted lines to
/// `logs/app-YYYYMMDD.log`. Logging never takes the app down: I/O errors are swallowed.
#[derive(Clone)]
pub struct LogWriter {
    inner: Arc<Mutex<Pending>>,
}

struct Pending {
    dir: PathBuf,
    buffer: Vec<u8>,
}

impl LogWriter {
    /// Writer appending to files in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Pending {
                dir: dir.into(),
                buffer: Vec::new(),
            })),
        }
    }
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut pending = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        pending.buffer.extend_from_slice(buf);
        if let Some(end) = pending.buffer.iter().rposition(|&b| b == b'\n') {
            let lines: Vec<u8> = pending.buffer.drain(..=end).collect();
            let text = redact(&String::from_utf8_lossy(&lines));
            let _ = std::fs::create_dir_all(&pending.dir);
            let file = pending
                .dir
                .join(format!("app-{}.log", chrono::Local::now().format("%Y%m%d")));
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
            {
                let _ = f.write_all(text.as_bytes());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys_and_signatures() {
        let line = "GET https://h/b/k?X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F2026&X-Amz-Signature=abc123 key AKIAIOSFODNN7EXAMPLE password=hunter2";
        let out = redact(line);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains("abc123"));
        assert!(!out.contains("hunter2"));
        assert!(out.contains("X-Amz-Signature=[redacted]"));
    }

    #[test]
    fn writes_complete_lines() {
        let dir = std::env::temp_dir().join(format!("s3m-log-{}", uuid::Uuid::new_v4()));
        let mut w = LogWriter::new(&dir);
        w.write_all(b"first AKIAIOSFODNN7EXAMPLE\nsecond")
            .expect("write");
        let file = std::fs::read_dir(&dir)
            .expect("dir")
            .next()
            .expect("file")
            .expect("entry");
        let text = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(text, "first [redacted-key]\n");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
