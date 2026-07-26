use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use zip::ZipArchive;

use crate::inventory::hash::sha256_copy;
use crate::manifest::{validate_restore_path, Manifest, MANIFEST_NAME};
use crate::progress::{CancelToken, ProgressFn, ProgressUpdate};

pub fn open_archive(zip_path: &Path) -> Result<ZipArchive<File>> {
    let file =
        File::open(zip_path).with_context(|| format!("cannot open {}", zip_path.display()))?;
    ZipArchive::new(file).with_context(|| {
        format!(
            "{} is not a readable zip archive (corrupt or incomplete?)",
            zip_path.display()
        )
    })
}

/// Read and parse manifest.json from a backup zip.
pub fn read_manifest(archive: &mut ZipArchive<File>) -> Result<Manifest> {
    let mut entry = archive.by_name(MANIFEST_NAME).context(
        "no manifest.json in this zip — this is not a kobo-backup archive \
         (or the backup was never verified/completed)",
    )?;
    let mut json = String::new();
    entry
        .read_to_string(&mut json)
        .context("cannot read manifest.json from zip")?;
    Manifest::from_json(&json)
}

/// Full pre-restore validation of a backup zip. Nothing may touch the device
/// until this passes:
/// - central directory parses
/// - manifest present, schema supported, all paths restore-safe
/// - archive entries and manifest file list match exactly (both directions)
/// - every entry streams fully with matching SHA-256 and size (also exercises
///   the zip CRC path)
pub fn validate(zip_path: &Path, progress: ProgressFn, cancel: &CancelToken) -> Result<Manifest> {
    let mut archive = open_archive(zip_path)?;
    let manifest = read_manifest(&mut archive)?;

    for entry in &manifest.files {
        validate_restore_path(&entry.path)?;
    }
    for dir in &manifest.dirs {
        validate_restore_path(dir)?;
    }

    // Exact two-way match between archive contents and manifest.
    let manifest_names: HashSet<String> = manifest
        .files
        .iter()
        .map(|f| Manifest::zip_entry_name(&f.path))
        .collect();
    let mut archive_names: HashSet<String> = HashSet::new();
    for name in archive.file_names() {
        if name == MANIFEST_NAME || name.ends_with('/') {
            continue;
        }
        archive_names.insert(name.to_string());
    }
    if let Some(missing) = manifest_names.difference(&archive_names).next() {
        bail!("backup is incomplete: manifest lists {missing} but the zip does not contain it");
    }
    if let Some(orphan) = archive_names.difference(&manifest_names).next() {
        bail!("backup is inconsistent: zip contains {orphan} which the manifest does not list");
    }

    // Full hash sweep.
    let files_total = manifest.totals.file_count;
    let bytes_total = manifest.totals.total_bytes;
    let mut bytes_done = 0u64;
    let mut verified = 0u64;
    for entry in &manifest.files {
        cancel.check()?;
        progress(ProgressUpdate {
            phase: "Verifying backup integrity".into(),
            current_path: entry.path.clone(),
            files_done: verified,
            files_total,
            bytes_done,
            bytes_total,
            detail: Some(format!("{verified} / {files_total} hashes match")),
        });
        let zip_entry = archive
            .by_name(&Manifest::zip_entry_name(&entry.path))
            .with_context(|| format!("cannot open zip entry for {}", entry.path))?;
        let mut local = 0u64;
        let (hash, size) = sha256_copy(zip_entry, std::io::sink(), cancel, |d| local += d)?;
        bytes_done += local;
        if hash != entry.sha256 {
            bail!(
                "hash mismatch inside backup for {} — the zip is corrupt. \
                 Do NOT restore from this file.",
                entry.path
            );
        }
        if size != entry.size {
            bail!(
                "size mismatch inside backup for {} — the zip is corrupt.",
                entry.path
            );
        }
        verified += 1;
    }
    progress(ProgressUpdate {
        phase: "Verifying backup integrity".into(),
        current_path: String::new(),
        files_done: verified,
        files_total,
        bytes_done,
        bytes_total,
        detail: Some(format!("{verified} / {files_total} hashes match")),
    });

    Ok(manifest)
}

/// Extract `KoboReader.sqlite` (and sidecars, if any) from a backup zip into
/// a temp dir, e.g. to run insights or an integrity check on the backup's DB.
pub fn extract_db_to_temp(
    archive: &mut ZipArchive<File>,
    manifest: &Manifest,
) -> Result<Option<(tempfile::TempDir, PathBuf)>> {
    let db_rel = ".kobo/KoboReader.sqlite";
    if manifest.find(db_rel).is_none() {
        return Ok(None);
    }
    let tmp = tempfile::tempdir().context("cannot create temp dir")?;
    let mut db_path = PathBuf::new();
    for rel in [
        db_rel.to_string(),
        format!("{db_rel}-wal"),
        format!("{db_rel}-shm"),
    ] {
        if manifest.find(&rel).is_none() {
            continue;
        }
        let name = rel.rsplit('/').next().unwrap().to_string();
        let dest = tmp.path().join(&name);
        let mut entry = archive
            .by_name(&Manifest::zip_entry_name(&rel))
            .with_context(|| format!("cannot open zip entry {rel}"))?;
        let mut out = File::create(&dest)?;
        std::io::copy(&mut entry, &mut out).with_context(|| format!("cannot extract {rel}"))?;
        if rel == db_rel {
            db_path = dest;
        }
    }
    Ok(Some((tmp, db_path)))
}

/// Stream one entry out of the zip to `dest`, returning its SHA-256.
pub fn extract_entry_hashed(
    archive: &mut ZipArchive<File>,
    rel_path: &str,
    dest: &Path,
    cancel: &CancelToken,
    mut on_bytes: impl FnMut(u64),
) -> Result<String> {
    let entry = archive
        .by_name(&Manifest::zip_entry_name(rel_path))
        .with_context(|| format!("cannot open zip entry for {rel_path}"))?;
    let out = File::create(dest).with_context(|| format!("cannot create {}", dest.display()))?;
    let mut writer = std::io::BufWriter::new(out);
    let (hash, _) = sha256_copy(entry, &mut writer, cancel, &mut on_bytes)?;
    let out = writer.into_inner().context("cannot flush extracted file")?;
    out.sync_all()
        .with_context(|| format!("cannot sync {}", dest.display()))?;
    Ok(hash)
}

/// Zip archives found in a directory, newest first — for the restore picker.
pub fn list_backup_zips(dir: &Path) -> Vec<PathBuf> {
    let mut zips: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
            {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                zips.push((mtime, path));
            }
        }
    }
    zips.sort_by(|a, b| b.0.cmp(&a.0));
    zips.into_iter().map(|(_, p)| p).collect()
}
