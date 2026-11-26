//! Local SQLite index at `{data_dir}/index.db`.
//!
//! `files` holds one row per known path (relative to the sync root, `/`-separated, NFC):
//! the last seen local hash/size/mtime, the server version the device last saw, and the
//! hash of the content at that version (`synced_sha256`). A file has unsynced local edits
//! when its current hash differs from `synced_sha256`.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

/// One row of the `files` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    /// Hash of the local file when last scanned.
    pub sha256: String,
    pub size: u64,
    pub mtime_ns: i64,
    /// Server version the device last saw at this path (`None` = never on the server).
    pub version_id: Option<i64>,
    /// Hash of that server version (`None` = never synced).
    pub synced_sha256: Option<String>,
}

impl FileRow {
    /// True when the local content differs from what was last synced.
    pub fn has_local_edits(&self) -> bool {
        self.synced_sha256.as_deref() != Some(self.sha256.as_str())
    }
}

/// The SQLite index.
pub struct Index {
    conn: Connection,
}

impl Index {
    /// Opens (or creates) the index at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// An in-memory index (tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS files (
                 path TEXT PRIMARY KEY,
                 sha256 TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL,
                 version_id INTEGER,
                 synced_sha256 TEXT
             );
             CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    /// The row for `path`.
    pub fn get(&self, path: &str) -> rusqlite::Result<Option<FileRow>> {
        self.conn
            .query_row(
                "SELECT path, sha256, size, mtime_ns, version_id, synced_sha256 FROM files WHERE path = ?1",
                [path],
                row_to_file,
            )
            .optional()
    }

    /// Inserts or replaces a row.
    pub fn upsert(&self, row: &FileRow) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO files (path, sha256, size, mtime_ns, version_id, synced_sha256)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET sha256 = excluded.sha256, size = excluded.size,
                 mtime_ns = excluded.mtime_ns, version_id = excluded.version_id,
                 synced_sha256 = excluded.synced_sha256",
            params![
                row.path,
                row.sha256,
                row.size as i64,
                row.mtime_ns,
                row.version_id,
                row.synced_sha256
            ],
        )?;
        Ok(())
    }

    /// Deletes the row for `path`.
    pub fn remove(&self, path: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", [path])?;
        Ok(())
    }

    /// Every row, ordered by path.
    pub fn all(&self) -> rusqlite::Result<Vec<FileRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, sha256, size, mtime_ns, version_id, synced_sha256 FROM files ORDER BY path")?;
        let rows = stmt.query_map([], row_to_file)?;
        rows.collect()
    }

    /// Sum of file sizes in the index.
    pub fn total_bytes(&self) -> rusqlite::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM files", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// Reads a state value (e.g. the change cursor).
    pub fn get_state(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM state WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
    }

    /// Writes a state value.
    pub fn set_state(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// Deletes every file row and state value (after a revoke).
    pub fn clear(&self) -> rusqlite::Result<()> {
        self.conn
            .execute_batch("DELETE FROM files; DELETE FROM state;")
    }
}

fn row_to_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        path: r.get(0)?,
        sha256: r.get(1)?,
        size: r.get::<_, i64>(2)?.max(0) as u64,
        mtime_ns: r.get(3)?,
        version_id: r.get(4)?,
        synced_sha256: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str) -> FileRow {
        FileRow {
            path: path.into(),
            sha256: "aa".into(),
            size: 3,
            mtime_ns: 10,
            version_id: Some(1),
            synced_sha256: Some("aa".into()),
        }
    }

    #[test]
    fn crud_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let idx = Index::open(&dir.path().join("index.db")).unwrap();
        idx.upsert(&row("b/x.txt")).unwrap();
        idx.upsert(&row("a.txt")).unwrap();
        let mut r = row("a.txt");
        r.sha256 = "bb".into();
        idx.upsert(&r).unwrap();
        let got = idx.get("a.txt").unwrap().unwrap();
        assert!(got.has_local_edits());
        assert!(!idx.get("b/x.txt").unwrap().unwrap().has_local_edits());
        assert_eq!(idx.all().unwrap().len(), 2);
        assert_eq!(idx.total_bytes().unwrap(), 6);
        idx.remove("a.txt").unwrap();
        assert_eq!(idx.get("a.txt").unwrap(), None);

        assert_eq!(idx.get_state("cursor").unwrap(), None);
        idx.set_state("cursor", "41").unwrap();
        idx.set_state("cursor", "42").unwrap();
        assert_eq!(idx.get_state("cursor").unwrap().as_deref(), Some("42"));
        idx.clear().unwrap();
        assert!(idx.all().unwrap().is_empty());
    }
}
