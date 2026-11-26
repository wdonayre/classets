//! Walks the sync root and reports every syncable file with its hash.
//!
//! Skips `rules::DEFAULT_SKIP` and symlinks. A file is only re-hashed when its size or
//! mtime differ from what the index already knows.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::rules::{DEFAULT_SKIP, is_skipped};

/// What the index knows about a path (used to skip hashing).
#[derive(Debug, Clone)]
pub struct Known {
    pub size: u64,
    pub mtime_ns: i64,
    pub sha256: String,
}

/// One file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    /// Relative, `/`-separated, NFC.
    pub path: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub sha256: String,
}

/// Result of a scan: files found plus per-file errors (unreadable files are skipped).
#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<LocalFile>,
    pub errors: Vec<String>,
}

/// Relative path of `full` under `root`, `/`-separated and NFC; `None` outside the root.
pub fn rel_path(root: &Path, full: &Path) -> Option<String> {
    let rel = full.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().nfc().collect::<String>()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Absolute path for a relative `/`-separated path.
pub fn abs_path(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
    {
        p.push(seg);
    }
    p
}

/// NFC-normalises a server path and trims slashes.
pub fn normalize(path: &str) -> String {
    path.trim_matches('/').nfc().collect()
}

/// Modification time in nanoseconds since the Unix epoch.
pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Streams a file through SHA-256; returns lowercase hex.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Size, mtime and hash of one file, reusing `known` when size and mtime match.
pub fn stat_file(path: &Path, known: Option<&Known>) -> std::io::Result<(u64, i64, String)> {
    let meta = std::fs::metadata(path)?;
    let (size, mtime) = (meta.len(), mtime_ns(&meta));
    if let Some(k) = known.filter(|k| k.size == size && k.mtime_ns == mtime) {
        return Ok((size, mtime, k.sha256.clone()));
    }
    Ok((size, mtime, hash_file(path)?))
}

/// True when a relative path should never sync.
pub fn skipped(rel: &str, is_dir: bool) -> bool {
    if is_dir {
        // Directory patterns only match parent segments, so test a child path.
        is_skipped(&format!("{rel}/."), DEFAULT_SKIP)
    } else {
        is_skipped(rel, DEFAULT_SKIP)
    }
}

/// Walks `root` and returns every syncable regular file (blocking).
pub fn scan(root: &Path, known: &HashMap<String, Known>) -> ScanResult {
    let mut out = ScanResult::default();
    if !root.exists() {
        return out;
    }
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            match rel_path(root, e.path()) {
                Some(rel) => !e.file_type().is_symlink() && !skipped(&rel, e.file_type().is_dir()),
                None => false,
            }
        });
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                out.errors.push(format!("scan: {e}"));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(rel) = rel_path(root, entry.path()) else {
            continue;
        };
        match stat_file(entry.path(), known.get(&rel)) {
            Ok((size, mtime_ns, sha256)) => out.files.push(LocalFile {
                path: rel,
                size,
                mtime_ns,
                sha256,
            }),
            Err(e) => out.errors.push(format!("scan {rel}: {e}")),
        }
    }
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_skips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("Team/node_modules/x")).unwrap();
        std::fs::write(root.join("Team/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("Team/draft.tmp"), b"x").unwrap();
        std::fs::write(root.join("Team/node_modules/x/i.js"), b"x").unwrap();
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("Team/a.txt"), root.join("Team/link.txt")).unwrap();

        let r = scan(root, &HashMap::new());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let paths: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["Team/a.txt"]);
        assert_eq!(r.files[0].sha256, crate::crypto::sha256_hex(b"hello"));
    }

    #[test]
    fn reuses_known_hash_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let first = scan(dir.path(), &HashMap::new()).files.remove(0);
        let mut known = HashMap::new();
        known.insert(
            "a.txt".to_string(),
            Known {
                size: first.size,
                mtime_ns: first.mtime_ns,
                sha256: "cached".into(),
            },
        );
        assert_eq!(scan(dir.path(), &known).files[0].sha256, "cached");
        known.get_mut("a.txt").unwrap().mtime_ns -= 1;
        assert_eq!(scan(dir.path(), &known).files[0].sha256, first.sha256);
    }

    #[test]
    fn paths_round_trip() {
        let root = Path::new("/r");
        assert_eq!(
            rel_path(root, &abs_path(root, "a/b c/d.txt")).as_deref(),
            Some("a/b c/d.txt")
        );
        assert_eq!(normalize("/Cafe\u{301}/x/"), "Caf\u{e9}/x");
    }
}
