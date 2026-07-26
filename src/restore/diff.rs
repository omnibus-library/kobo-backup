use std::collections::{HashMap, HashSet};

use crate::inventory::{FileEntry, Inventory};
use crate::manifest::Manifest;

/// What a restore will do to the device, computed before anything is touched.
#[derive(Debug, Clone, Default)]
pub struct DevicePlan {
    /// In backup and on device, but content differs → will be overwritten.
    pub overwrite: Vec<FileEntry>,
    /// In backup, not on device → will be added.
    pub add: Vec<FileEntry>,
    /// Identical on both sides → untouched.
    pub unchanged: Vec<FileEntry>,
    /// On device, not in backup → deleted IF the deletion pass is enabled.
    /// (This is what makes the restore a true point-in-time rollback.)
    pub delete: Vec<FileEntry>,
    /// Directories in the backup missing on the device.
    pub dirs_to_create: Vec<String>,
    /// Directories on the device that are not in the backup (removed after
    /// the deletion pass empties them).
    pub dirs_to_delete: Vec<String>,
    /// Paths that matched only case-insensitively (FAT32 is case-insensitive;
    /// these are treated as overwrites but flagged for the user).
    pub case_collisions: Vec<(String, String)>,
    pub bytes_to_write: u64,
    pub bytes_to_delete: u64,
}

impl DevicePlan {
    pub fn files_to_copy(&self) -> u64 {
        (self.overwrite.len() + self.add.len()) as u64
    }

    pub fn is_noop(&self) -> bool {
        self.overwrite.is_empty()
            && self.add.is_empty()
            && self.delete.is_empty()
            && self.dirs_to_create.is_empty()
            && self.dirs_to_delete.is_empty()
    }
}

/// Compare the backup manifest against a fresh device inventory.
pub fn diff(manifest: &Manifest, device: &Inventory) -> DevicePlan {
    let mut plan = DevicePlan::default();

    // FAT32 is case-insensitive: index the device by case-folded path.
    let device_by_folded: HashMap<String, &FileEntry> = device
        .files
        .iter()
        .map(|f| (f.path.to_lowercase(), f))
        .collect();
    let manifest_folded: HashSet<String> = manifest
        .files
        .iter()
        .map(|f| f.path.to_lowercase())
        .collect();

    for entry in &manifest.files {
        match device_by_folded.get(&entry.path.to_lowercase()) {
            Some(on_device) => {
                if on_device.path != entry.path {
                    plan.case_collisions
                        .push((entry.path.clone(), on_device.path.clone()));
                }
                if on_device.sha256 == entry.sha256 && on_device.size == entry.size {
                    plan.unchanged.push(entry.clone());
                } else {
                    plan.bytes_to_write += entry.size;
                    plan.overwrite.push(entry.clone());
                }
            }
            None => {
                plan.bytes_to_write += entry.size;
                plan.add.push(entry.clone());
            }
        }
    }

    for on_device in &device.files {
        if !manifest_folded.contains(&on_device.path.to_lowercase()) {
            plan.bytes_to_delete += on_device.size;
            plan.delete.push(on_device.clone());
        }
    }

    let device_dirs_folded: HashSet<String> =
        device.dirs.iter().map(|d| d.to_lowercase()).collect();
    let manifest_dirs_folded: HashSet<String> =
        manifest.dirs.iter().map(|d| d.to_lowercase()).collect();
    plan.dirs_to_create = manifest
        .dirs
        .iter()
        .filter(|d| !device_dirs_folded.contains(&d.to_lowercase()))
        .cloned()
        .collect();
    plan.dirs_to_delete = device
        .dirs
        .iter()
        .filter(|d| !manifest_dirs_folded.contains(&d.to_lowercase()))
        .cloned()
        .collect();
    // Delete deepest-first so rmdir of emptied parents works.
    plan.dirs_to_delete
        .sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::Category;

    fn entry(path: &str, sha: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            size,
            mtime: None,
            sha256: sha.into(),
            category: Category::Other,
        }
    }

    fn manifest_with(files: Vec<FileEntry>, dirs: Vec<&str>) -> Manifest {
        let json = serde_json::json!({
            "schema_version": 1,
            "tool": {"name": "kobo-backup", "version": "0.1.0"},
            "created_at": "2026-07-26T00:00:00Z",
            "host": {"os": "macos", "mount_point": "/Volumes/K"},
            "device": {
                "serial": "N000TEST", "model_id": "x", "model_name": "Test",
                "firmware": "1.0", "version_file_raw": "raw", "volume_label": "K",
                "volume_total_bytes": 0, "volume_used_bytes": 0
            },
            "exclusions": {"rules": [], "excluded_count": 0, "excluded_bytes": 0},
            "totals": {"file_count": files.len(), "dir_count": dirs.len(), "total_bytes": 0},
            "files": files,
            "dirs": dirs,
        });
        Manifest::from_json(&json.to_string()).unwrap()
    }

    #[test]
    fn diff_classifies_all_actions() {
        let manifest = manifest_with(
            vec![
                entry("same.txt", "aaa", 1),
                entry("changed.txt", "bbb", 2),
                entry("new.txt", "ccc", 3),
            ],
            vec!["keep", "create-me"],
        );
        let device = Inventory {
            files: vec![
                entry("same.txt", "aaa", 1),
                entry("changed.txt", "OLD", 2),
                entry("extra.txt", "ddd", 4),
            ],
            dirs: vec!["keep".into(), "delete-me".into()],
            ..Default::default()
        };
        let plan = diff(&manifest, &device);
        assert_eq!(plan.unchanged.len(), 1);
        assert_eq!(plan.overwrite.len(), 1);
        assert_eq!(plan.overwrite[0].path, "changed.txt");
        assert_eq!(plan.add.len(), 1);
        assert_eq!(plan.add[0].path, "new.txt");
        assert_eq!(plan.delete.len(), 1);
        assert_eq!(plan.delete[0].path, "extra.txt");
        assert_eq!(plan.dirs_to_create, vec!["create-me"]);
        assert_eq!(plan.dirs_to_delete, vec!["delete-me"]);
        assert_eq!(plan.bytes_to_write, 2 + 3);
        assert_eq!(plan.bytes_to_delete, 4);
        assert!(plan.case_collisions.is_empty());
    }

    #[test]
    fn diff_handles_case_insensitive_match() {
        let manifest = manifest_with(vec![entry("Book.epub", "aaa", 1)], vec![]);
        let device = Inventory {
            files: vec![entry("book.EPUB", "aaa", 1)],
            ..Default::default()
        };
        let plan = diff(&manifest, &device);
        assert_eq!(plan.unchanged.len(), 1);
        assert!(plan.delete.is_empty(), "case variant must not be deleted");
        assert_eq!(plan.case_collisions.len(), 1);
    }

    #[test]
    fn noop_when_identical() {
        let manifest = manifest_with(vec![entry("a.txt", "aaa", 1)], vec!["d"]);
        let device = Inventory {
            files: vec![entry("a.txt", "aaa", 1)],
            dirs: vec!["d".into()],
            ..Default::default()
        };
        assert!(diff(&manifest, &device).is_noop());
    }

    #[test]
    fn deepest_dirs_deleted_first() {
        let manifest = manifest_with(vec![], vec![]);
        let device = Inventory {
            dirs: vec!["a".into(), "a/b/c".into(), "a/b".into()],
            ..Default::default()
        };
        let plan = diff(&manifest, &device);
        assert_eq!(plan.dirs_to_delete, vec!["a/b/c", "a/b", "a"]);
    }
}
