use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::device::Device;
use crate::inventory::{hash::sha256_copy, Inventory};
use crate::manifest::{Manifest, FILES_PREFIX, MANIFEST_NAME};
use crate::progress::{CancelToken, ProgressFn, ProgressUpdate};

/// Extensions that are already compressed — store them raw for speed.
const STORED_EXTS: &[&str] = &[
    ".epub", ".kepub", ".zip", ".cbz", ".cbr", ".jpg", ".jpeg", ".png", ".gif", ".webp", ".mp3",
    ".m4a", ".m4b", ".ogg",
];

fn compression_for(path: &str) -> CompressionMethod {
    let lower = path.to_ascii_lowercase();
    if STORED_EXTS.iter().any(|ext| lower.ends_with(ext)) {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    }
}

fn zip_datetime(ts: Option<jiff::Timestamp>) -> zip::DateTime {
    let Some(ts) = ts else {
        return zip::DateTime::default();
    };
    let zoned = ts.to_zoned(jiff::tz::TimeZone::UTC);
    zip::DateTime::from_date_and_time(
        (zoned.year().clamp(1980, 2107)) as u16,
        zoned.month() as u8,
        zoned.day() as u8,
        zoned.hour() as u8,
        zoned.minute() as u8,
        zoned.second() as u8,
    )
    .unwrap_or_default()
}

#[derive(Debug)]
pub struct WriteOutcome {
    pub partial_path: PathBuf,
    pub manifest: Manifest,
}

/// Path of the in-progress zip for a given final destination.
pub fn partial_path_for(final_path: &Path) -> PathBuf {
    let mut os = final_path.as_os_str().to_owned();
    os.push(".partial");
    PathBuf::from(os)
}

/// Stream the device volume into `<final_path>.partial`.
///
/// The manifest is NOT written here: it is appended by backup verification
/// after every entry has been re-read and checked, so a zip containing a
/// manifest is by construction a verified zip. The device is only read.
///
/// Every file is re-hashed while being streamed into the zip; if a hash no
/// longer matches the inventory from the scan phase, something modified the
/// device between scan and write and the backup aborts.
pub fn write_backup(
    device: &Device,
    inventory: &Inventory,
    final_path: &Path,
    progress: ProgressFn,
    cancel: &CancelToken,
) -> Result<WriteOutcome> {
    let partial = partial_path_for(final_path);
    if let Some(parent) = partial.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create backup directory {}", parent.display()))?;
    }
    let file =
        File::create(&partial).with_context(|| format!("cannot create {}", partial.display()))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));

    let dir_options = SimpleFileOptions::default();
    for dir in &inventory.dirs {
        cancel.check()?;
        zip.add_directory(format!("{FILES_PREFIX}{dir}"), dir_options)
            .with_context(|| format!("cannot add directory entry {dir}"))?;
    }

    let files_total = inventory.file_count();
    let bytes_total = inventory.total_bytes;
    let mut bytes_done = 0u64;

    for (i, entry) in inventory.files.iter().enumerate() {
        cancel.check()?;
        progress(ProgressUpdate {
            phase: "Writing zip".into(),
            current_path: entry.path.clone(),
            files_done: i as u64,
            files_total,
            bytes_done,
            bytes_total,
            detail: None,
        });

        let options = SimpleFileOptions::default()
            .compression_method(compression_for(&entry.path))
            .last_modified_time(zip_datetime(entry.mtime))
            .large_file(entry.size >= 0xFFFF_FF00);
        zip.start_file(Manifest::zip_entry_name(&entry.path), options)
            .with_context(|| format!("cannot start zip entry for {}", entry.path))?;

        let src_path = device.mount.join(&entry.path);
        let src = File::open(&src_path)
            .with_context(|| format!("cannot open {} for backup", src_path.display()))?;
        let mut local = 0u64;
        let (hash, copied) = sha256_copy(src, &mut zip, cancel, |delta| {
            local += delta;
        })?;
        bytes_done += local;

        if hash != entry.sha256 {
            bail!(
                "{} changed on the device between scan and write \
                 (hash mismatch) — aborting backup. Re-run the backup; \
                 avoid touching the device while it runs.",
                entry.path
            );
        }
        if copied != entry.size {
            bail!(
                "{} changed size on the device between scan and write \
                 ({} scanned, {} copied) — aborting backup.",
                entry.path,
                entry.size,
                copied
            );
        }
    }

    let inner = zip.finish().context("cannot finalize zip")?;
    let file = inner.into_inner().context("cannot flush zip buffer")?;
    file.sync_all().context("cannot sync zip to disk")?;

    progress(ProgressUpdate {
        phase: "Writing zip".into(),
        current_path: String::new(),
        files_done: files_total,
        files_total,
        bytes_done,
        bytes_total,
        detail: None,
    });

    Ok(WriteOutcome {
        partial_path: partial,
        manifest: Manifest::new(device, inventory),
    })
}

/// Append the manifest (with its verification block filled in) to the zip.
/// Called by verify only after all content checks pass.
pub fn append_manifest(partial_path: &Path, manifest: &Manifest) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(partial_path)
        .with_context(|| format!("cannot reopen {}", partial_path.display()))?;
    let mut zip = ZipWriter::new_append(file).context("cannot append to zip")?;
    zip.start_file(
        MANIFEST_NAME,
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )?;
    zip.write_all(manifest.to_json()?.as_bytes())?;
    let file = zip.finish().context("cannot finalize manifest append")?;
    file.sync_all()
        .context("cannot sync zip after manifest append")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_choice() {
        assert_eq!(compression_for("books/x.epub"), CompressionMethod::Stored);
        assert_eq!(
            compression_for(".kobo/KoboReader.sqlite"),
            CompressionMethod::Deflated
        );
        assert_eq!(compression_for("cover.JPG"), CompressionMethod::Stored);
    }

    #[test]
    fn partial_path_appends_suffix() {
        assert_eq!(
            partial_path_for(Path::new("/tmp/b.zip")),
            Path::new("/tmp/b.zip.partial")
        );
    }
}
