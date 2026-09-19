use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::progress::{CancelToken, ProgressFn, ProgressUpdate};

use super::hash::sha256_file;

/// Directory names (at any depth) that are host-OS artifacts, never Kobo state.
pub const EXCLUDED_DIRS: &[&str] = &[
    ".Spotlight-V100",
    ".fseventsd",
    ".Trashes",
    ".TemporaryItems",
];

/// File-name rules for host junk / our own leftovers.
pub fn is_excluded_file_name(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._") || name.ends_with(".kbtmp")
}

pub fn exclusion_rules_description() -> Vec<String> {
    EXCLUDED_DIRS
        .iter()
        .map(|d| format!("{d}/"))
        .chain([
            ".DS_Store".into(),
            "._* (AppleDouble)".into(),
            "*.kbtmp".into(),
        ])
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Database,
    Book,
    Config,
    Drm,
    ImageCache,
    Other,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Database => "Databases",
            Category::Book => "Books",
            Category::Config => "Configuration",
            Category::Drm => "DRM / Adobe",
            Category::ImageCache => "Cover-image cache",
            Category::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Forward-slash path relative to the volume root, e.g. `.kobo/KoboReader.sqlite`.
    pub path: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<jiff::Timestamp>,
    pub sha256: String,
    pub category: Category,
}

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub files: Vec<FileEntry>,
    /// All directories (relative, forward-slash), including empty ones.
    pub dirs: Vec<String>,
    pub total_bytes: u64,
    pub excluded_count: u64,
    pub excluded_bytes: u64,
}

impl Inventory {
    pub fn file_count(&self) -> u64 {
        self.files.len() as u64
    }

    pub fn category_totals(&self) -> Vec<(Category, u64, u64)> {
        let cats = [
            Category::Database,
            Category::Book,
            Category::Config,
            Category::Drm,
            Category::ImageCache,
            Category::Other,
        ];
        cats.iter()
            .map(|&c| {
                let (count, bytes) = self
                    .files
                    .iter()
                    .filter(|f| f.category == c)
                    .fold((0u64, 0u64), |(n, b), f| (n + 1, b + f.size));
                (c, count, bytes)
            })
            .filter(|(_, count, _)| *count > 0)
            .collect()
    }

    pub fn find(&self, rel_path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.path == rel_path)
    }

    /// Largest files, for the "what is actually in here" summary screen.
    pub fn largest(&self, n: usize) -> Vec<&FileEntry> {
        let mut sorted: Vec<&FileEntry> = self.files.iter().collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(b.size));
        sorted.truncate(n);
        sorted
    }
}

/// Categorize a device-relative forward-slash path.
pub fn categorize(rel_path: &str) -> Category {
    let lower = rel_path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);

    if lower.starts_with(".kobo/") {
        if name.contains(".sqlite") {
            return Category::Database;
        }
        if lower.starts_with(".kobo/kepub/") {
            return Category::Book;
        }
        if name == "device.salt.conf" || lower.starts_with(".kobo/adobe") {
            return Category::Drm;
        }
        if name.ends_with(".conf") || name == "version" || lower.starts_with(".kobo/certificates/")
        {
            return Category::Config;
        }
        return Category::Other;
    }
    if lower.starts_with(".kobo-images/") {
        return Category::ImageCache;
    }
    if lower.starts_with("digital editions/") || lower.starts_with(".adobe-digital-editions/") {
        return Category::Drm;
    }
    let book_exts = [
        ".epub", ".kepub", ".pdf", ".mobi", ".azw", ".azw3", ".fb2", ".cbz", ".cbr", ".txt",
        ".rtf", ".html", ".htm",
    ];
    if book_exts.iter().any(|ext| name.ends_with(ext)) {
        return Category::Book;
    }
    Category::Other
}

/// Convert an absolute path under `root` to a device-relative forward-slash string.
fn rel_string(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// Walk the volume and build a complete inventory with SHA-256 hashes.
///
/// Two passes: a fast enumeration pass (so progress bars have real totals),
/// then the hashing pass. Read-only throughout.
pub fn scan(root: &Path, progress: ProgressFn, cancel: &CancelToken) -> Result<Inventory> {
    // Pass 1: enumerate.
    let mut pending: Vec<(std::path::PathBuf, String, u64)> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut excluded_count = 0u64;
    let mut excluded_bytes = 0u64;
    let mut total_bytes = 0u64;

    let walker = WalkDir::new(root).follow_links(false).into_iter();
    let mut iter = walker;
    loop {
        cancel.check()?;
        let Some(entry) = iter.next() else { break };
        let entry = entry.context("error walking device volume")?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();

        if entry.file_type().is_dir() {
            if EXCLUDED_DIRS.contains(&name.as_str()) {
                // Count what we're skipping so the exclusion is transparent.
                for skipped in WalkDir::new(path).into_iter().flatten() {
                    if skipped.file_type().is_file() {
                        excluded_count += 1;
                        excluded_bytes += skipped.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
                iter.skip_current_dir();
                continue;
            }
            dirs.push(rel_string(root, path)?);
            continue;
        }
        if !entry.file_type().is_file() {
            continue; // symlinks etc. cannot exist on FAT32; ignore defensively
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if is_excluded_file_name(&name) {
            excluded_count += 1;
            excluded_bytes += size;
            continue;
        }
        let rel = rel_string(root, path)?;
        total_bytes += size;
        pending.push((path.to_path_buf(), rel, size));

        if pending.len().is_multiple_of(100) {
            progress(ProgressUpdate {
                phase: "Enumerating files".into(),
                current_path: rel_string(root, path).unwrap_or_default(),
                files_done: pending.len() as u64,
                ..Default::default()
            });
        }
    }

    // Deterministic order: stable manifests, stable diffs.
    pending.sort_by(|a, b| a.1.cmp(&b.1));
    dirs.sort();

    // Pass 2: hash.
    let files_total = pending.len() as u64;
    let mut files = Vec::with_capacity(pending.len());
    let mut bytes_done = 0u64;
    for (i, (abs, rel, size)) in pending.into_iter().enumerate() {
        cancel.check()?;
        progress(ProgressUpdate {
            phase: "Hashing (read-only)".into(),
            current_path: rel.clone(),
            files_done: i as u64,
            files_total,
            bytes_done,
            bytes_total: total_bytes,
            detail: None,
        });
        let mut file_bytes = 0u64;
        let sha256 = sha256_file(&abs, cancel, |delta| {
            file_bytes += delta;
        })?;
        bytes_done += file_bytes;
        files.push(FileEntry {
            category: categorize(&rel),
            mtime: crate::util::file_mtime(&abs),
            path: rel,
            size,
            sha256,
        });
    }
    progress(ProgressUpdate {
        phase: "Hashing (read-only)".into(),
        current_path: String::new(),
        files_done: files_total,
        files_total,
        bytes_done,
        bytes_total: total_bytes,
        detail: None,
    });

    Ok(Inventory {
        files,
        dirs,
        total_bytes,
        excluded_count,
        excluded_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::CancelToken;
    use std::fs;

    #[test]
    fn categorize_paths() {
        assert_eq!(categorize(".kobo/KoboReader.sqlite"), Category::Database);
        assert_eq!(
            categorize(".kobo/KoboReader.sqlite-wal"),
            Category::Database
        );
        assert_eq!(categorize(".kobo/BookReader.sqlite"), Category::Database);
        assert_eq!(categorize(".kobo/kepub/abc123"), Category::Book);
        assert_eq!(categorize(".kobo/Kobo/Kobo eReader.conf"), Category::Config);
        assert_eq!(categorize(".kobo/version"), Category::Config);
        assert_eq!(categorize(".kobo/device.salt.conf"), Category::Drm);
        assert_eq!(categorize("Digital Editions/book.epub"), Category::Drm);
        assert_eq!(
            categorize(".kobo-images/123/cover.jpg"),
            Category::ImageCache
        );
        assert_eq!(categorize("books/My Novel.epub"), Category::Book);
        assert_eq!(categorize("random.bin"), Category::Other);
        assert_eq!(categorize(".kobo/markups/page1.svg"), Category::Other);
    }

    #[test]
    fn scan_excludes_junk_and_counts_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".kobo")).unwrap();
        fs::write(
            root.join(".kobo/version"),
            "N000TEST,1.0,4.1.1,1.0,1.0,x-310",
        )
        .unwrap();
        fs::write(root.join("book.epub"), b"epub bytes").unwrap();
        fs::write(root.join(".DS_Store"), b"junk").unwrap();
        fs::write(root.join("._book.epub"), b"appledouble").unwrap();
        fs::create_dir_all(root.join(".Spotlight-V100/store")).unwrap();
        fs::write(root.join(".Spotlight-V100/store/db"), b"spotlight junk").unwrap();
        fs::create_dir_all(root.join("empty-dir")).unwrap();

        let inv = scan(root, &crate::progress::silent(), &CancelToken::new()).unwrap();
        let paths: Vec<&str> = inv.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec![".kobo/version", "book.epub"]);
        assert_eq!(inv.excluded_count, 3);
        assert!(inv.excluded_bytes > 0);
        assert!(inv.dirs.contains(&"empty-dir".to_string()));
        assert!(inv.dirs.contains(&".kobo".to_string()));
        assert!(!inv.dirs.iter().any(|d| d.contains("Spotlight")));
    }

    #[test]
    fn scan_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("b.txt"), b"b").unwrap();
        fs::write(root.join("a.txt"), b"a").unwrap();
        let inv1 = scan(root, &crate::progress::silent(), &CancelToken::new()).unwrap();
        let inv2 = scan(root, &crate::progress::silent(), &CancelToken::new()).unwrap();
        let p1: Vec<_> = inv1.files.iter().map(|f| &f.path).collect();
        let p2: Vec<_> = inv2.files.iter().map(|f| &f.path).collect();
        assert_eq!(p1, p2);
        assert_eq!(p1, vec!["a.txt", "b.txt"]);
    }
}
