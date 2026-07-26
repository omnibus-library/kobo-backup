use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

pub const TEST_SERIAL: &str = "N000TESTSERIAL";

/// Build a miniature but structurally faithful Kobo volume in a temp dir.
/// Returns the guard and the mount path.
pub fn stage_fake_device() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let mount = tmp.path().join("KOBOeReader");
    let kobo = mount.join(".kobo");

    std::fs::create_dir_all(kobo.join("Kobo")).unwrap();
    std::fs::create_dir_all(kobo.join("kepub")).unwrap();
    std::fs::create_dir_all(mount.join("books")).unwrap();
    std::fs::create_dir_all(mount.join("Digital Editions")).unwrap();
    std::fs::create_dir_all(mount.join("empty-dir")).unwrap();

    std::fs::write(
        kobo.join("version"),
        format!(
            "{TEST_SERIAL},3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382"
        ),
    )
    .unwrap();
    create_kobo_db(&kobo.join("KoboReader.sqlite"));
    std::fs::write(
        kobo.join("Kobo").join("Kobo eReader.conf"),
        "[FeatureSettings]\nExportHighlights=true\n",
    )
    .unwrap();
    std::fs::write(kobo.join("device.salt.conf"), "fake-salt-device-bound").unwrap();
    std::fs::write(kobo.join("kepub").join("store-book-uuid"), vec![7u8; 4096]).unwrap();
    std::fs::write(
        mount.join("books").join("Test Novel.epub"),
        b"PK-fake-epub-content".repeat(100),
    )
    .unwrap();
    std::fs::write(
        mount.join("Digital Editions").join("drm-book.epub"),
        b"adobe-drm-bytes".repeat(50),
    )
    .unwrap();

    // Host junk that must be excluded from backups.
    std::fs::write(mount.join(".DS_Store"), b"finder junk").unwrap();
    // AppleDouble sidecar next to the database — name-matches ".sqlite" but is
    // host pollution, so neither backups nor the pre-restore safety copy may
    // pick it up. Seen on a real device.
    std::fs::write(kobo.join("._KoboReader.sqlite"), b"appledouble junk").unwrap();
    std::fs::write(
        mount.join("books").join("._Test Novel.epub"),
        b"appledouble",
    )
    .unwrap();
    std::fs::create_dir_all(mount.join(".Spotlight-V100/Store-V2")).unwrap();
    std::fs::write(mount.join(".Spotlight-V100/Store-V2/index"), b"spotlight").unwrap();

    (tmp, mount)
}

/// A small KoboReader.sqlite with the real tables our code touches.
pub fn create_kobo_db(path: &Path) {
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
        CREATE TABLE Shelf (Name TEXT, _IsDeleted TEXT);
        INSERT INTO content VALUES
          ('file:///mnt/onboard/books/Test Novel.epub', 6, 'Test Novel', 1, 37,
           '2026-07-20T21:14:00Z', 'true'),
          ('store-uuid-1', 6, 'Store Book', 2, 100, '2026-06-11T08:00:00Z', 'true');
        INSERT INTO Bookmark VALUES
          ('bm1', 'file:///mnt/onboard/books/Test Novel.epub', 'highlight',
           'a highlighted passage', NULL, '2026-07-19T09:00:00.000'),
          ('bm2', 'file:///mnt/onboard/books/Test Novel.epub', 'note',
           'passage', 'my margin note', '2026-07-19T09:05:00.000'),
          ('bm3', 'store-uuid-1', 'dogear', NULL, NULL, '2026-05-02T12:00:00.000');
        INSERT INTO Shelf VALUES ('Favorites', 'false');
        "#,
    )
    .unwrap();
}

/// Add one more annotation to the DB — mutates its hash the way real device
/// use would between a backup and a later restore.
pub fn add_annotation(mount: &Path) {
    let conn = Connection::open(mount.join(".kobo/KoboReader.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO Bookmark VALUES ('bm-new', 'store-uuid-1', 'highlight',
         'post-backup highlight', NULL, '2026-07-25T10:00:00.000')",
        [],
    )
    .unwrap();
}
