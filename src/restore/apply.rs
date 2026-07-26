use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive::read::{extract_entry_hashed, open_archive};
use crate::device::Device;
use crate::inventory::scan::is_excluded_file_name;
use crate::inventory::{Category, FileEntry};
use crate::progress::{CancelToken, ProgressFn, ProgressUpdate};

use super::diff::DevicePlan;

#[derive(Debug, Clone, Copy)]
pub struct ApplyOptions {
    /// Delete device files that are not part of the backup (true point-in-time).
    pub delete_extras: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub files_deleted: u64,
    pub dirs_created: u64,
    pub dirs_removed: u64,
    pub unchanged_skipped: u64,
    pub mtimes_set: u64,
}

/// Copy the device's current databases aside before any write touches them.
/// Returns the directory containing the safety copies.
pub fn micro_backup(device: &Device, dest_root: &Path) -> Result<PathBuf> {
    let dir = dest_root.join(format!(
        "{}-{}",
        device.identity.serial,
        crate::util::filename_timestamp()
    ));
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let kobo_dir = device.mount.join(".kobo");
    let mut copied = 0;
    if let Ok(entries) = std::fs::read_dir(&kobo_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Host junk (`._KoboReader.sqlite` and friends) name-matches the
            // patterns below but is not device state — apply the same
            // exclusions the scanner uses.
            if is_excluded_file_name(&name) || !entry.path().is_file() {
                continue;
            }
            if name.contains(".sqlite") || name == "version" {
                std::fs::copy(entry.path(), dir.join(&name))
                    .with_context(|| format!("cannot safety-copy {name}"))?;
                copied += 1;
            }
        }
    }
    if copied == 0 {
        bail!(
            "found nothing to safety-copy in {} — refusing to continue",
            kobo_dir.display()
        );
    }
    Ok(dir)
}

/// Copy order: regular files first, then databases, with KoboReader.sqlite
/// family last of all — minimizing the window where the catalog DB disagrees
/// with the books on the volume.
fn copy_order_key(entry: &FileEntry) -> (u8, String) {
    let tier = if entry.path.starts_with(".kobo/KoboReader.sqlite") {
        2
    } else if entry.category == Category::Database {
        1
    } else {
        0
    };
    (tier, entry.path.clone())
}

/// Apply the plan to the device. The ONLY function in the codebase that
/// writes to or deletes from the Kobo.
///
/// Discipline per file: stream to `<target>.kbtmp` while hashing → compare
/// hash against the manifest BEFORE rename → fsync → atomic-ish rename over
/// the target. Cancellation is honored between files, never mid-file.
/// Deletions run last, only after every copy succeeded.
pub fn apply(
    zip_path: &Path,
    device: &Device,
    plan: &DevicePlan,
    options: ApplyOptions,
    progress: ProgressFn,
    cancel: &CancelToken,
) -> Result<ApplyReport> {
    let mut archive = open_archive(zip_path)?;
    let mut report = ApplyReport {
        unchanged_skipped: plan.unchanged.len() as u64,
        ..Default::default()
    };

    // Phase 1: directories.
    for dir in &plan.dirs_to_create {
        cancel.check()?;
        let path = device.mount.join(dir);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("cannot create directory {}", path.display()))?;
        report.dirs_created += 1;
    }

    // Phase 2: copies (adds + overwrites), databases last.
    let mut to_copy: Vec<&FileEntry> = plan.add.iter().chain(plan.overwrite.iter()).collect();
    to_copy.sort_by_key(|e| copy_order_key(e));

    let files_total = to_copy.len() as u64;
    let bytes_total = plan.bytes_to_write;
    let mut bytes_done = 0u64;

    for (i, entry) in to_copy.iter().enumerate() {
        cancel.check()?;
        if !device.is_present() {
            bail!(
                "device disappeared mid-restore (after {i} of {files_total} files) — \
                 reconnect it and run the restore again; already-restored files \
                 are complete, none are truncated"
            );
        }
        progress(ProgressUpdate {
            phase: "Restoring files to device".into(),
            current_path: entry.path.clone(),
            files_done: i as u64,
            files_total,
            bytes_done,
            bytes_total,
            detail: None,
        });

        let target = device.mount.join(&entry.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create parent dir for {}", entry.path))?;
        }
        let tmp = {
            let mut os = target.as_os_str().to_owned();
            os.push(".kbtmp");
            PathBuf::from(os)
        };

        let mut local = 0u64;
        let result = extract_entry_hashed(&mut archive, &entry.path, &tmp, cancel, |d| {
            local += d;
        });
        let hash = match result {
            Ok(hash) => hash,
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(err.context(format!("while restoring {}", entry.path)));
            }
        };
        bytes_done += local;

        if hash != entry.sha256 {
            let _ = std::fs::remove_file(&tmp);
            bail!(
                "backup entry {} did not hash-verify during extraction — \
                 the zip may be corrupt; the existing file on the device \
                 was NOT touched",
                entry.path
            );
        }

        std::fs::rename(&tmp, &target).with_context(|| {
            format!("cannot move verified file into place: {}", target.display())
        })?;

        if let Some(mtime) = entry.mtime {
            if let Ok(file) = std::fs::File::options().write(true).open(&target) {
                if file
                    .set_modified(crate::util::timestamp_to_systemtime(mtime))
                    .is_ok()
                {
                    report.mtimes_set += 1;
                }
            }
        }
        report.files_copied += 1;
        report.bytes_copied += local;
    }

    // Phase 3: deletions — only after every copy verified and landed.
    if options.delete_extras {
        let delete_total = plan.delete.len() as u64;
        for (i, entry) in plan.delete.iter().enumerate() {
            cancel.check()?;
            progress(ProgressUpdate {
                phase: "Deleting files not in backup".into(),
                current_path: entry.path.clone(),
                files_done: i as u64,
                files_total: delete_total,
                bytes_done: bytes_total,
                bytes_total,
                detail: None,
            });
            let path = device.mount.join(&entry.path);
            match std::fs::remove_file(&path) {
                Ok(()) => report.files_deleted += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(
                        anyhow::Error::from(err).context(format!("cannot delete {}", entry.path))
                    );
                }
            }
        }
        for dir in &plan.dirs_to_delete {
            cancel.check()?;
            let path = device.mount.join(dir);
            // Only ever removes EMPTY directories; a non-empty one is left alone.
            if std::fs::remove_dir(&path).is_ok() {
                report.dirs_removed += 1;
            }
        }
    }

    progress(ProgressUpdate {
        phase: "Flushing writes to device".into(),
        current_path: String::new(),
        files_done: files_total,
        files_total,
        bytes_done,
        bytes_total,
        detail: None,
    });

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_order_puts_koboreader_last() {
        let mk = |path: &str, cat: Category| FileEntry {
            path: path.into(),
            size: 0,
            mtime: None,
            sha256: String::new(),
            category: cat,
        };
        let mut entries = [
            mk(".kobo/KoboReader.sqlite", Category::Database),
            mk("zz-last-book.epub", Category::Book),
            mk(".kobo/BookReader.sqlite", Category::Database),
            mk(".kobo/KoboReader.sqlite-wal", Category::Database),
            mk("aa-first.epub", Category::Book),
        ];
        entries.sort_by_key(copy_order_key);
        let order: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "aa-first.epub",
                "zz-last-book.epub",
                ".kobo/BookReader.sqlite",
                ".kobo/KoboReader.sqlite",
                ".kobo/KoboReader.sqlite-wal",
            ]
        );
    }

    #[test]
    fn micro_backup_copies_databases_but_not_host_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        let kobo = mount.join(".kobo");
        std::fs::create_dir_all(&kobo).unwrap();
        std::fs::write(
            kobo.join("version"),
            "N000TESTSERIAL,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
        )
        .unwrap();
        std::fs::write(kobo.join("KoboReader.sqlite"), b"db bytes").unwrap();
        std::fs::write(kobo.join("BookReader.sqlite"), b"encrypted bytes").unwrap();
        std::fs::write(kobo.join("fonts.sqlite"), b"fonts").unwrap();
        // macOS pollution that name-matches the ".sqlite" pattern.
        std::fs::write(kobo.join("._KoboReader.sqlite"), b"appledouble junk").unwrap();
        std::fs::write(kobo.join(".DS_Store"), b"finder junk").unwrap();
        // A directory whose name also contains ".sqlite".
        std::fs::create_dir_all(kobo.join("backup.sqlite.d")).unwrap();

        let device = crate::device::probe(&mount).unwrap();
        let dest = tmp.path().join("pre-restore");
        let dir = micro_backup(&device, &dest).unwrap();

        let mut copied: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        copied.sort();
        assert_eq!(
            copied,
            vec![
                "BookReader.sqlite",
                "KoboReader.sqlite",
                "fonts.sqlite",
                "version",
            ],
            "safety copy must contain the real databases only"
        );
    }

    #[test]
    fn micro_backup_refuses_when_nothing_to_save() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        let kobo = mount.join(".kobo");
        std::fs::create_dir_all(&kobo).unwrap();
        std::fs::write(
            kobo.join("version"),
            "N000TESTSERIAL,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
        )
        .unwrap();
        let device = crate::device::probe(&mount).unwrap();
        // Only `version` exists, which does get copied — so this must succeed.
        assert!(micro_backup(&device, &tmp.path().join("a")).is_ok());

        // With nothing at all in .kobo, it must refuse rather than proceed.
        std::fs::remove_file(kobo.join("version")).unwrap();
        let err = micro_backup(&device, &tmp.path().join("b")).unwrap_err();
        assert!(err.to_string().contains("refusing to continue"));
    }
}
