mod common;

use kobo_backup::archive::{read, write};
use kobo_backup::device::probe;
use kobo_backup::insights;
use kobo_backup::inventory::scan;
use kobo_backup::progress::{silent, CancelToken};
use kobo_backup::restore::{apply, diff, micro_backup, ApplyOptions};
use kobo_backup::verify::{verify_backup, verify_restore};

use common::{add_annotation, stage_fake_device, TEST_SERIAL};

#[test]
fn full_backup_restore_roundtrip() {
    let (_guard, mount) = stage_fake_device();
    let device = probe(&mount).expect("fake device must probe as a Kobo");
    assert_eq!(device.identity.serial, TEST_SERIAL);
    assert_eq!(device.identity.model_name, "Kobo Libra 2");

    let cancel = CancelToken::new();
    let quiet = silent();

    // --- Backup: scan ---
    let inventory = scan(&mount, &quiet, &cancel).unwrap();
    assert!(inventory.find(".kobo/KoboReader.sqlite").is_some());
    assert!(inventory.find("books/Test Novel.epub").is_some());
    assert!(
        inventory.find(".DS_Store").is_none(),
        "junk must be excluded"
    );
    assert!(inventory.excluded_count >= 3);
    assert!(inventory.dirs.contains(&"empty-dir".to_string()));

    // --- Insights from a temp copy of the DB ---
    let (_db_guard, db_copy) = insights::copy_db_to_temp(&mount.join(".kobo")).unwrap();
    let stats = insights::gather(&db_copy).unwrap();
    assert_eq!(stats.annotations_total, Some(3));
    assert_eq!(stats.books_with_annotations, Some(2));
    assert_eq!(stats.total_books, Some(2));
    assert_eq!(stats.current_book.as_ref().unwrap().0, "Test Novel");

    // --- Backup: write zip ---
    let out = tempfile::tempdir().unwrap();
    let final_zip = out.path().join("backup.zip");
    let outcome = write::write_backup(&device, &inventory, &final_zip, &quiet, &cancel).unwrap();
    assert!(outcome.partial_path.exists(), "zip starts life as .partial");
    assert!(
        !final_zip.exists(),
        "final name must not exist before verify"
    );

    // --- Backup: verify + promote ---
    let mut manifest = outcome.manifest;
    let (report, promoted) = verify_backup(
        &outcome.partial_path,
        &final_zip,
        &mut manifest,
        &device,
        &quiet,
        &cancel,
    )
    .unwrap();
    assert!(report.passed(), "verify failed: {:?}", report.checks);
    assert_eq!(promoted.as_deref(), Some(final_zip.as_path()));
    assert!(final_zip.exists());
    assert!(
        !outcome.partial_path.exists(),
        ".partial must be promoted away"
    );
    let verification = manifest.verification.as_ref().unwrap();
    assert!(verification.verified_after_write);
    assert_eq!(verification.sqlite_integrity, "ok");

    // --- Restore preflight: validate the zip like a fresh session would ---
    let manifest_from_zip = read::validate(&final_zip, &quiet, &cancel).unwrap();
    assert_eq!(manifest_from_zip.device.serial, TEST_SERIAL);
    assert_eq!(manifest_from_zip.totals.file_count, inventory.file_count());

    // Insights read back out of the backup itself.
    let mut archive = read::open_archive(&final_zip).unwrap();
    let (_tmp2, backup_db) = read::extract_db_to_temp(&mut archive, &manifest_from_zip)
        .unwrap()
        .expect("backup contains the DB");
    let backup_stats = insights::gather(&backup_db).unwrap();
    assert_eq!(backup_stats.annotations_total, Some(3));
    drop(archive);

    // --- Mutate the device like weeks of use ---
    std::fs::remove_file(mount.join("books/Test Novel.epub")).unwrap();
    add_annotation(&mount);
    std::fs::write(mount.join("stray-download.txt"), b"not in backup").unwrap();
    std::fs::create_dir_all(mount.join("new-dir/nested")).unwrap();
    std::fs::write(mount.join("new-dir/nested/file.bin"), b"extra").unwrap();

    // --- Diff ---
    let device_inv = scan(&mount, &quiet, &cancel).unwrap();
    let plan = diff(&manifest_from_zip, &device_inv);
    assert!(plan.add.iter().any(|f| f.path == "books/Test Novel.epub"));
    assert!(plan
        .overwrite
        .iter()
        .any(|f| f.path == ".kobo/KoboReader.sqlite"));
    assert!(plan.delete.iter().any(|f| f.path == "stray-download.txt"));
    assert!(plan
        .delete
        .iter()
        .any(|f| f.path == "new-dir/nested/file.bin"));
    assert!(plan.dirs_to_delete.contains(&"new-dir".to_string()));

    // --- Micro-backup of current DBs before writing ---
    let safety_root = out.path().join("pre-restore");
    let safety_dir = micro_backup(&device, &safety_root).unwrap();
    assert!(safety_dir.join("KoboReader.sqlite").is_file());
    assert!(safety_dir.join("version").is_file());

    // --- Apply ---
    let apply_report = apply(
        &final_zip,
        &device,
        &plan,
        ApplyOptions {
            delete_extras: true,
        },
        &quiet,
        &cancel,
    )
    .unwrap();
    assert!(apply_report.files_copied >= 2);
    assert!(apply_report.files_deleted >= 2);

    // --- Verify restore ---
    let restore_report =
        verify_restore(&device, &manifest_from_zip, true, &quiet, &cancel).unwrap();
    assert!(
        restore_report.passed(),
        "restore verify failed: {:?}",
        restore_report.checks
    );

    // --- The strongest guarantee: tree is byte-identical to the original ---
    let final_inv = scan(&mount, &quiet, &cancel).unwrap();
    let original: Vec<(String, String)> = inventory
        .files
        .iter()
        .map(|f| (f.path.clone(), f.sha256.clone()))
        .collect();
    let restored: Vec<(String, String)> = final_inv
        .files
        .iter()
        .map(|f| (f.path.clone(), f.sha256.clone()))
        .collect();
    assert_eq!(original, restored, "device tree must be byte-identical");

    // Annotation state rolled back to backup point.
    let (_g, db_after) = insights::copy_db_to_temp(&mount.join(".kobo")).unwrap();
    let stats_after = insights::gather(&db_after).unwrap();
    assert_eq!(
        stats_after.annotations_total,
        Some(3),
        "post-backup annotation must be gone"
    );

    // Empty dirs restored; extra dirs removed.
    assert!(mount.join("empty-dir").is_dir());
    assert!(!mount.join("new-dir").exists());

    // mtimes restored within FAT32 2-second granularity.
    let entry = manifest_from_zip.find("books/Test Novel.epub").unwrap();
    let want = entry.mtime.unwrap();
    let got = kobo_backup::util::file_mtime(&mount.join("books/Test Novel.epub")).unwrap();
    let drift = (want.as_second() - got.as_second()).abs();
    assert!(drift <= 2, "mtime drift {drift}s exceeds FAT32 granularity");
}

#[test]
fn declined_deletion_keeps_extras_and_warns() {
    let (_guard, mount) = stage_fake_device();
    let device = probe(&mount).unwrap();
    let cancel = CancelToken::new();
    let quiet = silent();

    let inventory = scan(&mount, &quiet, &cancel).unwrap();
    let out = tempfile::tempdir().unwrap();
    let final_zip = out.path().join("backup.zip");
    let outcome = write::write_backup(&device, &inventory, &final_zip, &quiet, &cancel).unwrap();
    let mut manifest = outcome.manifest;
    let (report, _) = verify_backup(
        &outcome.partial_path,
        &final_zip,
        &mut manifest,
        &device,
        &quiet,
        &cancel,
    )
    .unwrap();
    assert!(report.passed());

    std::fs::write(mount.join("keep-me.txt"), b"user says keep").unwrap();

    let device_inv = scan(&mount, &quiet, &cancel).unwrap();
    let plan = diff(&manifest, &device_inv);
    apply(
        &final_zip,
        &device,
        &plan,
        ApplyOptions {
            delete_extras: false,
        },
        &quiet,
        &cancel,
    )
    .unwrap();

    assert!(
        mount.join("keep-me.txt").exists(),
        "declined deletion must keep file"
    );
    let report = verify_restore(&device, &manifest, false, &quiet, &cancel).unwrap();
    assert!(
        report.passed(),
        "kept extras must be a warning, not a failure"
    );
    assert!(report.warnings() >= 1);
}

#[test]
fn cancelled_backup_never_produces_final_zip() {
    let (_guard, mount) = stage_fake_device();
    let device = probe(&mount).unwrap();
    let quiet = silent();

    let inventory = scan(&mount, &quiet, &CancelToken::new()).unwrap();
    let out = tempfile::tempdir().unwrap();
    let final_zip = out.path().join("backup.zip");

    let cancelled = CancelToken::new();
    cancelled.cancel();
    let err = write::write_backup(&device, &inventory, &final_zip, &quiet, &cancelled)
        .expect_err("cancelled write must error");
    assert!(err.is::<kobo_backup::progress::Cancelled>());
    assert!(!final_zip.exists(), "no final zip after cancellation");
}

#[test]
fn corrupt_zip_is_refused_before_restore() {
    let (_guard, mount) = stage_fake_device();
    let device = probe(&mount).unwrap();
    let cancel = CancelToken::new();
    let quiet = silent();

    let inventory = scan(&mount, &quiet, &cancel).unwrap();
    let out = tempfile::tempdir().unwrap();
    let final_zip = out.path().join("backup.zip");
    let outcome = write::write_backup(&device, &inventory, &final_zip, &quiet, &cancel).unwrap();
    let mut manifest = outcome.manifest;
    verify_backup(
        &outcome.partial_path,
        &final_zip,
        &mut manifest,
        &device,
        &quiet,
        &cancel,
    )
    .unwrap();

    // Flip one byte in the middle of the archive.
    let mut bytes = std::fs::read(&final_zip).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&final_zip, &bytes).unwrap();

    assert!(
        read::validate(&final_zip, &quiet, &cancel).is_err(),
        "corrupted backup must be refused"
    );
}

#[test]
fn drift_during_backup_aborts() {
    let (_guard, mount) = stage_fake_device();
    let device = probe(&mount).unwrap();
    let cancel = CancelToken::new();
    let quiet = silent();

    let inventory = scan(&mount, &quiet, &cancel).unwrap();
    // Change a file AFTER the scan, BEFORE the write.
    add_annotation(&mount);

    let out = tempfile::tempdir().unwrap();
    let final_zip = out.path().join("backup.zip");
    let err = write::write_backup(&device, &inventory, &final_zip, &quiet, &cancel)
        .expect_err("write must detect the changed file");
    assert!(
        err.to_string().contains("changed on the device"),
        "got: {err:#}"
    );
}
