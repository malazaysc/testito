use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

/// Absolute path to the screenshots directory under the platform data dir.
/// Always returned alongside the parent dir; we don't auto-create until ingest.
pub fn screenshots_dir() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "testito", "testito") {
        dirs.data_dir().join("screenshots")
    } else {
        PathBuf::from("./testito-screenshots")
    }
}

/// Outcome of a screenshot ingestion. The caller writes one row per
/// attachment, and the on-disk file is content-addressed so duplicates
/// across runs/notes share storage.
pub struct Ingested {
    pub content_hash: String,
    pub extension: String,
    pub mime_type: String,
    pub original_filename: String,
    pub bytes_written: u64,
}

impl Ingested {
    /// Filename used both on disk and in the served URL (`<hash>.<ext>`).
    pub fn filename(&self) -> String {
        if self.extension.is_empty() {
            self.content_hash.clone()
        } else {
            format!("{}.{}", self.content_hash, self.extension)
        }
    }
}

/// Read the source file, hash it, and copy into the screenshots dir using
/// `<sha256>.<ext>` as the filename. Idempotent: if the file already exists
/// (same content from another finding), we skip the write.
pub fn ingest_screenshot(src: &Path) -> Result<Ingested> {
    let bytes =
        std::fs::read(src).with_context(|| format!("reading screenshot {}", src.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let content_hash = hex::encode(h.finalize());

    let extension = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| is_safe_extension(e))
        .unwrap_or_default();

    let mime_type = mime_for(&extension).to_string();
    let original_filename = src
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| content_hash.clone());

    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let dest_filename = if extension.is_empty() {
        content_hash.clone()
    } else {
        format!("{}.{}", content_hash, extension)
    };
    let dest = dir.join(&dest_filename);

    if !dest.exists() {
        std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    }

    Ok(Ingested {
        content_hash,
        extension,
        mime_type,
        original_filename,
        bytes_written: bytes.len() as u64,
    })
}

/// Open a stored screenshot for serving by HTTP. Validates the filename
/// against the on-disk format so a malicious request can't traverse out of
/// the screenshots dir.
pub fn open_for_serving(filename: &str) -> Result<(PathBuf, &'static str)> {
    if !is_valid_filename(filename) {
        return Err(anyhow!("invalid screenshot filename"));
    }
    let dir = screenshots_dir();
    let path = dir.join(filename);
    // canonicalize defensively — even though is_valid_filename rejects '/'
    // and '..', a future change shouldn't accidentally let a path escape.
    let canonical_path = std::fs::canonicalize(&path)
        .with_context(|| format!("canonicalizing {}", path.display()))?;
    let canonical_dir =
        std::fs::canonicalize(&dir).with_context(|| format!("canonicalizing {}", dir.display()))?;
    if !canonical_path.starts_with(&canonical_dir) {
        return Err(anyhow!("screenshot path escaped storage dir"));
    }
    let ext = filename.rsplit('.').next().unwrap_or("");
    Ok((canonical_path, mime_for(ext)))
}

fn is_valid_filename(name: &str) -> bool {
    // Format: 64 hex chars, optional `.<ext>` where ext is 1–8 alphanumeric.
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_hexdigit() && i < 64 {
        i += 1;
    }
    if i != 64 {
        return false;
    }
    if i == bytes.len() {
        return true;
    }
    if bytes[i] != b'.' {
        return false;
    }
    let ext = &name[i + 1..];
    !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn is_safe_extension(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tiff"
    )
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_validation_accepts_hash_with_ext() {
        let h = "a".repeat(64);
        assert!(is_valid_filename(&format!("{h}.png")));
        assert!(is_valid_filename(&format!("{h}.jpg")));
        assert!(is_valid_filename(&h)); // no extension is allowed
    }

    #[test]
    fn filename_validation_rejects_traversal() {
        assert!(!is_valid_filename("../etc/passwd"));
        assert!(!is_valid_filename("./foo"));
        assert!(!is_valid_filename("foo/bar"));
        assert!(!is_valid_filename("foo\\bar"));
        assert!(!is_valid_filename(""));
        assert!(!is_valid_filename("z".repeat(64).as_str())); // not hex
        assert!(!is_valid_filename("abc"));
    }

    #[test]
    fn filename_validation_rejects_long_or_weird_extensions() {
        let h = "a".repeat(64);
        assert!(!is_valid_filename(&format!("{h}.")));
        assert!(!is_valid_filename(&format!("{h}.toolongextension")));
        assert!(!is_valid_filename(&format!("{h}.../etc/p")));
    }

    #[test]
    fn ingest_dedupes_and_returns_hash() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.png");
        std::fs::write(&src, b"fake png bytes").unwrap();

        // We override the storage dir by setting a custom env... actually we
        // can't override because screenshots_dir() reads platform dirs. Instead
        // we can ingest twice and assert the same hash + idempotence.
        let a = ingest_screenshot(&src).unwrap();
        let b = ingest_screenshot(&src).unwrap();
        assert_eq!(a.content_hash, b.content_hash);
        assert_eq!(a.extension, "png");
        assert_eq!(a.mime_type, "image/png");
        assert!(a.content_hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.content_hash.len(), 64);
    }
}
