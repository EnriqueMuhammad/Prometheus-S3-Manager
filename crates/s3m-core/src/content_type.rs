//! `Content-Type` of uploaded files, guessed from the file extension.
//!
//! Without it S3 stores every object as `binary/octet-stream` (or `application/octet-stream`),
//! so browsers download images, PDFs and web pages from presigned URLs or static websites
//! instead of showing them.

use std::path::Path;

/// Content type for a file name or key, or `None` when the extension is unknown (the server
/// then applies its default).
pub fn guess(name: impl AsRef<Path>) -> Option<&'static str> {
    let ext = name.as_ref().extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        // Text and web
        "txt" | "text" | "log" | "conf" | "cfg" | "ini" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "md" | "markdown" => "text/markdown",
        "htm" | "html" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" | "map" => "application/json",
        "jsonld" => "application/ld+json",
        "webmanifest" => "application/manifest+json",
        "xml" => "application/xml",
        "xhtml" => "application/xhtml+xml",
        "rss" => "application/rss+xml",
        "atom" => "application/atom+xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "ics" => "text/calendar",
        "vtt" => "text/vtt",
        "wasm" => "application/wasm",
        // Images
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "bmp" => "image/bmp",
        "ico" => "image/vnd.microsoft.icon",
        "svg" | "svgz" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        // Audio and video
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "weba" => "audio/webm",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "mpeg" | "mpg" => "video/mpeg",
        "ts" => "video/mp2t",
        "m3u8" => "application/vnd.apple.mpegurl",
        "mpd" => "application/dash+xml",
        // Fonts
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        // Documents
        "pdf" => "application/pdf",
        "rtf" => "application/rtf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "epub" => "application/epub+zip",
        // Archives and binaries
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "zst" => "application/zstd",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "jar" => "application/java-archive",
        "apk" => "application/vnd.android.package-archive",
        "msi" => "application/x-msi",
        "exe" | "dll" | "bin" | "iso" | "img" | "dmg" => "application/octet-stream",
        "sql" => "application/sql",
        "parquet" => "application/vnd.apache.parquet",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_by_extension() {
        assert_eq!(guess("photo.JPG"), Some("image/jpeg"));
        assert_eq!(guess("site/index.html"), Some("text/html"));
        assert_eq!(guess(r"C:\data\report.pdf"), Some("application/pdf"));
        assert_eq!(guess("archive.tar.gz"), Some("application/gzip"));
        assert_eq!(guess("notes.txt"), Some("text/plain"));
        assert_eq!(guess("disk.bin"), Some("application/octet-stream"));
    }

    #[test]
    fn unknown_or_missing_extension_is_none() {
        assert_eq!(guess("README"), None);
        assert_eq!(guess("data.s3mweird"), None);
        assert_eq!(guess(".gitignore"), None);
        assert_eq!(guess("folder/"), None);
    }
}
