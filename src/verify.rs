//! Verification for both directions: freshly written backups (against the
//! source device) and freshly restored devices (against the manifest).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::archive;
use crate::device::Device;
use crate::insights;
use crate::inventory::{self, hash::sha256_file};
use crate::manifest::{Manifest, Verification};
use crate::progress::{CancelToken, ProgressFn, ProgressUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub duration: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub checks: Vec<CheckResult>,
}

impl VerifyReport {
    pub fn passed(&self) -> bool {
        !self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }

    pub fn warnings(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count()
    }

    fn push(&mut self, name: &str, status: CheckStatus, detail: String, started: Instant) {
        self.checks.push(CheckResult {
            name: name.to_string(),
            status,
            detail,
            duration: started.elapsed(),
        });
    }
}

/// Verify a freshly written `.partial` backup zip end to end. On success,
/// append the manifest (with a truthful verification block) and promote the
/// zip to its final name. Returns the report and the final path if promoted.
pub fn verify_backup(
    partial_path: &Path,
    final_path: &Path,
    manifest: &mut Manifest,
    device: &Device,
    progress: ProgressFn,
    cancel: &CancelToken,
) -> Result<(VerifyReport, Option<PathBuf>)> {
    let mut report = VerifyReport::default();

    // Check 1: archive opens (central directory parses).
    let started = Instant::now();
    let mut archive = match archive::read::open_archive(partial_path) {
        Ok(a) => {
            report.push(
                "Zip central directory",
                CheckStatus::Pass,
                "archive structure parses cleanly".into(),
                started,
            );
            a
        }
        Err(err) => {
            report.push(
                "Zip central directory",
                CheckStatus::Fail,
                format!("{err:#}"),
                started,
            );
            return Ok((report, None));
        }
    };

    // Check 2: entry count.
    let started = Instant::now();
    let file_entries = archive.file_names().filter(|n| !n.ends_with('/')).count() as u64;
    let expected = manifest.totals.file_count;
    if file_entries == expected {
        report.push(
            "Entry count",
            CheckStatus::Pass,
            format!("{file_entries} files in zip = {expected} in manifest"),
            started,
        );
    } else {
        report.push(
            "Entry count",
            CheckStatus::Fail,
            format!("{file_entries} files in zip, expected {expected}"),
            started,
        );
        return Ok((report, None));
    }

    // Check 3: full hash sweep of every entry vs manifest.
    let started = Instant::now();
    let files_total = manifest.totals.file_count;
    let bytes_total = manifest.totals.total_bytes;
    let mut bytes_done = 0u64;
    let mut ok_count = 0u64;
    let mut sweep_failed: Option<String> = None;
    for entry in &manifest.files {
        cancel.check()?;
        progress(ProgressUpdate {
            phase: "Re-reading zip & checking hashes".into(),
            current_path: entry.path.clone(),
            files_done: ok_count,
            files_total,
            bytes_done,
            bytes_total,
            detail: Some(format!("{ok_count} / {files_total} hashes match")),
        });
        let zip_entry = archive.by_name(&Manifest::zip_entry_name(&entry.path));
        let result = zip_entry.map_err(anyhow::Error::from).and_then(|e| {
            let mut local = 0u64;
            let out = inventory::hash::sha256_copy(e, std::io::sink(), cancel, |d| local += d)?;
            bytes_done += local;
            Ok(out)
        });
        match result {
            Ok((hash, size)) if hash == entry.sha256 && size == entry.size => ok_count += 1,
            Ok(_) => {
                sweep_failed = Some(format!("hash/size mismatch for {}", entry.path));
                break;
            }
            Err(err) => {
                if err.is::<crate::progress::Cancelled>() {
                    return Err(err);
                }
                sweep_failed = Some(format!("cannot read {}: {err:#}", entry.path));
                break;
            }
        }
    }
    match sweep_failed {
        None => report.push(
            "Content hash sweep",
            CheckStatus::Pass,
            format!("all {ok_count} file hashes match the manifest"),
            started,
        ),
        Some(why) => {
            report.push("Content hash sweep", CheckStatus::Fail, why, started);
            return Ok((report, None));
        }
    }

    // Check 4: KoboReader.sqlite integrity inside the backup.
    let started = Instant::now();
    let mut sqlite_result = "not present".to_string();
    match archive::read::extract_db_to_temp(&mut archive, manifest) {
        Ok(Some((_tmp, db_path))) => match insights::integrity_check(&db_path, false) {
            Ok(result) if result == "ok" => {
                sqlite_result = "ok".into();
                report.push(
                    "KoboReader.sqlite integrity",
                    CheckStatus::Pass,
                    "PRAGMA integrity_check = ok (on the copy inside the backup)".into(),
                    started,
                );
            }
            Ok(result) => {
                report.push(
                    "KoboReader.sqlite integrity",
                    CheckStatus::Fail,
                    format!("integrity_check reported: {result}"),
                    started,
                );
                return Ok((report, None));
            }
            Err(err) => {
                report.push(
                    "KoboReader.sqlite integrity",
                    CheckStatus::Fail,
                    format!("cannot check: {err:#}"),
                    started,
                );
                return Ok((report, None));
            }
        },
        Ok(None) => {
            report.push(
                "KoboReader.sqlite integrity",
                CheckStatus::Warn,
                "no KoboReader.sqlite in this backup (unusual for a Kobo volume)".into(),
                started,
            );
        }
        Err(err) => {
            report.push(
                "KoboReader.sqlite integrity",
                CheckStatus::Fail,
                format!("cannot extract DB from backup: {err:#}"),
                started,
            );
            return Ok((report, None));
        }
    }

    // Check 5: source drift — re-hash the device's databases now and compare
    // with the manifest; catches anything writing to the device mid-backup.
    let started = Instant::now();
    let db_entries: Vec<_> = manifest
        .files
        .iter()
        .filter(|f| f.category == inventory::Category::Database)
        .cloned()
        .collect();
    let mut drift: Option<String> = None;
    for entry in &db_entries {
        cancel.check()?;
        let device_path = device.mount.join(&entry.path);
        match sha256_file(&device_path, cancel, |_| {}) {
            Ok(hash) if hash == entry.sha256 => {}
            Ok(_) => {
                drift = Some(format!(
                    "{} changed on the device during backup",
                    entry.path
                ));
                break;
            }
            Err(err) => {
                if err.is::<crate::progress::Cancelled>() {
                    return Err(err);
                }
                drift = Some(format!("cannot re-read {}: {err:#}", entry.path));
                break;
            }
        }
    }
    match drift {
        None => report.push(
            "Source drift check",
            CheckStatus::Pass,
            format!(
                "{} database file(s) on the device still match the backup exactly",
                db_entries.len()
            ),
            started,
        ),
        Some(why) => {
            report.push("Source drift check", CheckStatus::Fail, why, started);
            return Ok((report, None));
        }
    }

    // All checks green: record verification truthfully, append manifest, promote.
    let started = Instant::now();
    manifest.verification = Some(Verification {
        verified_after_write: true,
        sqlite_integrity: sqlite_result,
    });
    drop(archive);
    archive::write::append_manifest(partial_path, manifest)
        .context("verified OK, but failed to write manifest into the zip")?;

    // Confirm the appended manifest reads back.
    let mut reopened = archive::read::open_archive(partial_path)?;
    archive::read::read_manifest(&mut reopened)
        .context("verified OK, but the appended manifest does not read back")?;
    drop(reopened);

    std::fs::rename(partial_path, final_path).with_context(|| {
        format!(
            "verified OK, but cannot rename {} to {}",
            partial_path.display(),
            final_path.display()
        )
    })?;
    report.push(
        "Finalize archive",
        CheckStatus::Pass,
        format!("manifest embedded, promoted to {}", final_path.display()),
        started,
    );

    Ok((report, Some(final_path.to_path_buf())))
}

/// Post-restore verification: re-scan the device and compare with the manifest.
pub fn verify_restore(
    device: &Device,
    manifest: &Manifest,
    deletions_enabled: bool,
    progress: ProgressFn,
    cancel: &CancelToken,
) -> Result<VerifyReport> {
    let mut report = VerifyReport::default();

    let started = Instant::now();
    let device_inv = inventory::scan(&device.mount, progress, cancel)?;
    report.push(
        "Device re-scan",
        CheckStatus::Pass,
        format!(
            "{} files re-read and hashed from the device",
            device_inv.file_count()
        ),
        started,
    );

    // Every manifest file must be present and byte-identical.
    let started = Instant::now();
    let mut missing = 0u64;
    let mut mismatched = 0u64;
    let mut first_bad = String::new();
    for entry in &manifest.files {
        match device_inv.find(&entry.path) {
            None => {
                missing += 1;
                if first_bad.is_empty() {
                    first_bad = format!("missing: {}", entry.path);
                }
            }
            Some(on_device) if on_device.sha256 != entry.sha256 || on_device.size != entry.size => {
                mismatched += 1;
                if first_bad.is_empty() {
                    first_bad = format!("differs: {}", entry.path);
                }
            }
            Some(_) => {}
        }
    }
    if missing == 0 && mismatched == 0 {
        report.push(
            "All files match backup",
            CheckStatus::Pass,
            format!(
                "{} / {} files on the device are byte-identical to the backup",
                manifest.totals.file_count, manifest.totals.file_count
            ),
            started,
        );
    } else {
        report.push(
            "All files match backup",
            CheckStatus::Fail,
            format!("{missing} missing, {mismatched} mismatched (first: {first_bad})"),
            started,
        );
    }

    // Extra files not in the manifest.
    let started = Instant::now();
    let extras: Vec<&str> = device_inv
        .files
        .iter()
        .filter(|f| manifest.find(&f.path).is_none())
        .map(|f| f.path.as_str())
        .collect();
    if extras.is_empty() {
        report.push(
            "No unexpected files",
            CheckStatus::Pass,
            "device contains exactly the backed-up file set".into(),
            started,
        );
    } else if deletions_enabled {
        report.push(
            "No unexpected files",
            CheckStatus::Fail,
            format!(
                "{} file(s) remain that are not in the backup (first: {})",
                extras.len(),
                extras[0]
            ),
            started,
        );
    } else {
        report.push(
            "Unexpected files (deletion pass was declined)",
            CheckStatus::Warn,
            format!(
                "{} file(s) on the device are not part of the backup — kept as you chose",
                extras.len()
            ),
            started,
        );
    }

    // In-place integrity check of the restored database (immutable open —
    // guaranteed not to create sidecar files on the device).
    let started = Instant::now();
    let db_path = device.mount.join(".kobo/KoboReader.sqlite");
    if db_path.is_file() {
        match insights::integrity_check(&db_path, true) {
            Ok(result) if result == "ok" => report.push(
                "Restored KoboReader.sqlite integrity",
                CheckStatus::Pass,
                "PRAGMA integrity_check = ok (read-only, in place)".into(),
                started,
            ),
            Ok(result) => report.push(
                "Restored KoboReader.sqlite integrity",
                CheckStatus::Fail,
                format!("integrity_check reported: {result}"),
                started,
            ),
            Err(err) => report.push(
                "Restored KoboReader.sqlite integrity",
                CheckStatus::Warn,
                format!("could not check in place: {err:#}"),
                started,
            ),
        }
    }

    Ok(report)
}
