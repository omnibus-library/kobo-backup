//! Library insights: read-only facts extracted from `KoboReader.sqlite`.
//!
//! The device database is NEVER opened directly — callers hand this module a
//! path to a *copy* (in a temp dir, or extracted from a backup zip), and it is
//! opened with SQLITE_OPEN_READ_ONLY on top of that. Every query degrades
//! gracefully: schema differences across firmware versions produce `None`
//! fields, never errors.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

#[derive(Debug, Clone, Default)]
pub struct LibraryInsights {
    pub total_books: Option<i64>,
    pub sideloaded_books: Option<i64>,
    pub store_books: Option<i64>,
    pub annotations_total: Option<i64>,
    pub highlights: Option<i64>,
    pub notes: Option<i64>,
    pub dogears: Option<i64>,
    pub books_with_annotations: Option<i64>,
    pub newest_annotation: Option<String>,
    pub books_in_progress: Option<i64>,
    pub books_finished: Option<i64>,
    /// (title, percent read) of the most recently read in-progress book.
    pub current_book: Option<(String, i64)>,
    pub shelves: Option<i64>,
    pub db_size_bytes: u64,
}

/// Copy `KoboReader.sqlite` (and WAL/SHM sidecars if present) from `dir`
/// into a temp dir, returning the temp handle and the copied DB path.
/// Keeping the temp dir alive is the caller's job (drop = cleanup).
pub fn copy_db_to_temp(db_dir: &Path) -> Result<(tempfile::TempDir, PathBuf)> {
    let src = db_dir.join("KoboReader.sqlite");
    if !src.is_file() {
        anyhow::bail!("KoboReader.sqlite not found in {}", db_dir.display());
    }
    let tmp = tempfile::tempdir().context("cannot create temp dir for DB copy")?;
    let dst = tmp.path().join("KoboReader.sqlite");
    std::fs::copy(&src, &dst).with_context(|| format!("cannot copy {} to temp", src.display()))?;
    for sidecar in ["-wal", "-shm"] {
        let side_src = db_dir.join(format!("KoboReader.sqlite{sidecar}"));
        if side_src.is_file() {
            std::fs::copy(
                &side_src,
                tmp.path().join(format!("KoboReader.sqlite{sidecar}")),
            )
            .with_context(|| format!("cannot copy sidecar {}", side_src.display()))?;
        }
    }
    Ok((tmp, dst))
}

/// Gather insights from a KoboReader.sqlite file (must be a safe copy —
/// the connection may write, e.g. to recover a WAL journal on the copy).
pub fn gather(db_path: &Path) -> Result<LibraryInsights> {
    let db_size_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let conn = Connection::open(db_path)
        .with_context(|| format!("cannot open DB copy {}", db_path.display()))?;

    let mut insights = LibraryInsights {
        db_size_bytes,
        ..Default::default()
    };

    let has_content = table_exists(&conn, "content");
    let has_bookmark = table_exists(&conn, "Bookmark");

    if has_content {
        insights.total_books = count(
            &conn,
            "SELECT COUNT(*) FROM content WHERE ContentType = 6 AND IsDownloaded IN ('true', 1)",
        )
        .or_else(|| count(&conn, "SELECT COUNT(*) FROM content WHERE ContentType = 6"));
        insights.sideloaded_books = count(
            &conn,
            "SELECT COUNT(*) FROM content WHERE ContentType = 6 AND ContentID LIKE 'file:///%'",
        );
        insights.store_books = match (insights.total_books, insights.sideloaded_books) {
            (Some(t), Some(s)) => Some((t - s).max(0)),
            _ => None,
        };
        insights.books_in_progress = count(
            &conn,
            "SELECT COUNT(*) FROM content WHERE ContentType = 6 AND ReadStatus = 1",
        );
        insights.books_finished = count(
            &conn,
            "SELECT COUNT(*) FROM content WHERE ContentType = 6 AND ReadStatus = 2",
        );
        insights.current_book = conn
            .query_row(
                "SELECT Title, CAST(___PercentRead AS INTEGER) FROM content \
                 WHERE ContentType = 6 AND ReadStatus = 1 AND DateLastRead IS NOT NULL \
                 ORDER BY DateLastRead DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .ok();
    }

    if has_bookmark {
        insights.annotations_total = count(&conn, "SELECT COUNT(*) FROM Bookmark");
        insights.highlights = count(
            &conn,
            "SELECT COUNT(*) FROM Bookmark WHERE Type = 'highlight'",
        );
        insights.notes = count(&conn, "SELECT COUNT(*) FROM Bookmark WHERE Type = 'note'");
        insights.dogears = count(&conn, "SELECT COUNT(*) FROM Bookmark WHERE Type = 'dogear'");
        insights.books_with_annotations =
            count(&conn, "SELECT COUNT(DISTINCT VolumeID) FROM Bookmark");
        insights.newest_annotation = conn
            .query_row(
                "SELECT MAX(DateCreated) FROM Bookmark WHERE DateCreated IS NOT NULL",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .map(|d| d.chars().take(10).collect());
    }

    if table_exists(&conn, "Shelf") {
        insights.shelves = count(
            &conn,
            "SELECT COUNT(*) FROM Shelf WHERE _IsDeleted IN ('false', 0)",
        )
        .or_else(|| count(&conn, "SELECT COUNT(*) FROM Shelf"));
    }

    Ok(insights)
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

fn count(conn: &Connection, sql: &str) -> Option<i64> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0)).ok()
}

/// Run `PRAGMA integrity_check` against a DB path.
/// `in_place_read_only` opens with immutable semantics so no sidecar files
/// are ever created next to the DB (needed when checking on the device itself).
pub fn integrity_check(db_path: &Path, in_place_read_only: bool) -> Result<String> {
    let conn = if in_place_read_only {
        let uri = format!(
            "file:{}?immutable=1",
            db_path.to_string_lossy().replace('?', "%3f")
        );
        Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?
    } else {
        Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?
    };
    let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn create_test_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE content (
                ContentID TEXT PRIMARY KEY,
                ContentType INTEGER,
                Title TEXT,
                ReadStatus INTEGER,
                ___PercentRead INTEGER,
                DateLastRead TEXT,
                IsDownloaded TEXT
            );
            CREATE TABLE Bookmark (
                BookmarkID TEXT PRIMARY KEY,
                VolumeID TEXT,
                Type TEXT,
                Text TEXT,
                Annotation TEXT,
                DateCreated TEXT
            );
            CREATE TABLE Shelf (
                Name TEXT,
                _IsDeleted TEXT
            );
            INSERT INTO content VALUES
              ('file:///mnt/onboard/a.epub', 6, 'Book A', 1, 42, '2026-07-01T10:00:00Z', 'true'),
              ('file:///mnt/onboard/b.epub', 6, 'Book B', 2, 100, '2026-06-01T10:00:00Z', 'true'),
              ('store-uuid-1', 6, 'Store Book', 0, 0, NULL, 'true'),
              ('file:///mnt/onboard/a.epub#chap1', 899, 'Chapter', 0, 0, NULL, 'true');
            INSERT INTO Bookmark VALUES
              ('bm1', 'file:///mnt/onboard/a.epub', 'highlight', 'highlighted text', NULL, '2026-07-10T08:00:00.000'),
              ('bm2', 'file:///mnt/onboard/a.epub', 'note', 'text', 'my note', '2026-07-19T09:00:00.000'),
              ('bm3', 'file:///mnt/onboard/b.epub', 'dogear', NULL, NULL, '2026-05-01T09:00:00.000');
            INSERT INTO Shelf VALUES ('Favorites', 'false'), ('Old', 'true');
            "#,
        )
        .unwrap();
    }

    #[test]
    fn gathers_insights_from_test_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("KoboReader.sqlite");
        create_test_db(&db);
        let insights = gather(&db).unwrap();
        assert_eq!(insights.total_books, Some(3));
        assert_eq!(insights.sideloaded_books, Some(2));
        assert_eq!(insights.store_books, Some(1));
        assert_eq!(insights.annotations_total, Some(3));
        assert_eq!(insights.highlights, Some(1));
        assert_eq!(insights.notes, Some(1));
        assert_eq!(insights.dogears, Some(1));
        assert_eq!(insights.books_with_annotations, Some(2));
        assert_eq!(insights.newest_annotation.as_deref(), Some("2026-07-19"));
        assert_eq!(insights.books_in_progress, Some(1));
        assert_eq!(insights.books_finished, Some(1));
        assert_eq!(insights.current_book, Some(("Book A".to_string(), 42)));
        assert_eq!(insights.shelves, Some(1));
    }

    #[test]
    fn missing_tables_degrade_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("KoboReader.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE unrelated (x INTEGER);")
            .unwrap();
        drop(conn);
        let insights = gather(&db).unwrap();
        assert_eq!(insights.total_books, None);
        assert_eq!(insights.annotations_total, None);
    }

    #[test]
    fn integrity_check_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("KoboReader.sqlite");
        create_test_db(&db);
        assert_eq!(integrity_check(&db, false).unwrap(), "ok");
        assert_eq!(integrity_check(&db, true).unwrap(), "ok");
    }

    #[test]
    fn copy_db_to_temp_copies_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        create_test_db(&dir.join("KoboReader.sqlite"));
        std::fs::write(dir.join("KoboReader.sqlite-wal"), b"fake wal").unwrap();
        let (_guard, copied) = copy_db_to_temp(dir).unwrap();
        assert!(copied.is_file());
        assert!(copied.with_file_name("KoboReader.sqlite-wal").is_file());
    }
}
