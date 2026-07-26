use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::device::Device;
use crate::inventory::{FileEntry, Inventory};

pub const SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_NAME: &str = "manifest.json";
/// All device content is stored in the zip under this prefix.
pub const FILES_PREFIX: &str = "files/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub tool: ToolInfo,
    pub created_at: jiff::Timestamp,
    pub host: HostInfo,
    pub device: DeviceInfo,
    pub exclusions: Exclusions,
    pub totals: Totals,
    pub files: Vec<FileEntry>,
    pub dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub os: String,
    pub mount_point: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub serial: String,
    pub model_id: String,
    pub model_name: String,
    pub firmware: String,
    pub version_file_raw: String,
    pub volume_label: String,
    pub volume_total_bytes: u64,
    pub volume_used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exclusions {
    pub rules: Vec<String>,
    pub excluded_count: u64,
    pub excluded_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Totals {
    pub file_count: u64,
    pub dir_count: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub verified_after_write: bool,
    pub sqlite_integrity: String,
}

impl Manifest {
    pub fn new(device: &Device, inventory: &Inventory) -> Self {
        Manifest {
            schema_version: SCHEMA_VERSION,
            tool: ToolInfo {
                name: env!("CARGO_PKG_NAME").to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            created_at: crate::util::now(),
            host: HostInfo {
                os: std::env::consts::OS.to_string(),
                mount_point: device.mount.display().to_string(),
            },
            device: DeviceInfo {
                serial: device.identity.serial.clone(),
                model_id: device.identity.model_id.clone(),
                model_name: device.identity.model_name.clone(),
                firmware: device.identity.firmware.clone(),
                version_file_raw: device.identity.raw.clone(),
                volume_label: device.label.clone(),
                volume_total_bytes: device.volume_total,
                volume_used_bytes: device.volume_used(),
            },
            exclusions: Exclusions {
                rules: crate::inventory::scan::exclusion_rules_description(),
                excluded_count: inventory.excluded_count,
                excluded_bytes: inventory.excluded_bytes,
            },
            totals: Totals {
                file_count: inventory.file_count(),
                dir_count: inventory.dirs.len() as u64,
                total_bytes: inventory.total_bytes,
            },
            files: inventory.files.clone(),
            dirs: inventory.dirs.clone(),
            verification: None,
        }
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("failed to serialize manifest")
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let manifest: Manifest =
            serde_json::from_str(json).context("manifest.json is not valid for this tool")?;
        if manifest.schema_version > SCHEMA_VERSION {
            bail!(
                "this backup was created by a newer version of kobo-backup \
                 (manifest schema {} > supported {}). Upgrade the tool to restore it.",
                manifest.schema_version,
                SCHEMA_VERSION
            );
        }
        Ok(manifest)
    }

    /// Zip entry name for a manifest file path.
    pub fn zip_entry_name(rel_path: &str) -> String {
        format!("{FILES_PREFIX}{rel_path}")
    }

    pub fn find(&self, rel_path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.path == rel_path)
    }
}

/// Reject manifest paths that could escape the device root or are illegal on FAT32.
/// This is the zip-slip defense — run on every path before restore.
pub fn validate_restore_path(rel_path: &str) -> Result<()> {
    if rel_path.is_empty() {
        bail!("empty path in manifest");
    }
    if rel_path.starts_with('/') || rel_path.contains('\\') {
        bail!("unsafe path in manifest (absolute or backslash): {rel_path:?}");
    }
    if rel_path.contains('\0') {
        bail!("unsafe path in manifest (NUL byte)");
    }
    for component in rel_path.split('/') {
        if component.is_empty() {
            bail!("unsafe path in manifest (empty component): {rel_path:?}");
        }
        if component == "." || component == ".." {
            bail!("unsafe path in manifest (dot component): {rel_path:?}");
        }
        // FAT32-illegal characters (excluding '/' which is our separator).
        if component
            .chars()
            .any(|c| matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 0x20)
        {
            bail!("path contains FAT32-illegal characters: {rel_path:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation() {
        assert!(validate_restore_path(".kobo/KoboReader.sqlite").is_ok());
        assert!(validate_restore_path("books/A Novel (2020).epub").is_ok());
        assert!(validate_restore_path("/etc/passwd").is_err());
        assert!(validate_restore_path("../escape").is_err());
        assert!(validate_restore_path("a/../b").is_err());
        assert!(validate_restore_path("a//b").is_err());
        assert!(validate_restore_path("bad\\path").is_err());
        assert!(validate_restore_path("que?tion.txt").is_err());
        assert!(validate_restore_path("").is_err());
    }

    #[test]
    fn manifest_round_trip() {
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            tool: ToolInfo {
                name: "kobo-backup".into(),
                version: "0.1.0".into(),
            },
            created_at: "2026-07-26T14:03:11Z".parse().unwrap(),
            host: HostInfo {
                os: "macos".into(),
                mount_point: "/Volumes/KOBOeReader".into(),
            },
            device: DeviceInfo {
                serial: "N000TEST".into(),
                model_id: "x-382".into(),
                model_name: "Kobo Libra 2".into(),
                firmware: "4.41.23145".into(),
                version_file_raw: "N000TEST,...".into(),
                volume_label: "KOBOeReader".into(),
                volume_total_bytes: 100,
                volume_used_bytes: 50,
            },
            exclusions: Exclusions {
                rules: vec![".DS_Store".into()],
                excluded_count: 1,
                excluded_bytes: 2,
            },
            totals: Totals {
                file_count: 0,
                dir_count: 0,
                total_bytes: 0,
            },
            files: vec![],
            dirs: vec![],
            verification: None,
        };
        let json = manifest.to_json().unwrap();
        let back = Manifest::from_json(&json).unwrap();
        assert_eq!(back.device.serial, "N000TEST");
        assert_eq!(back.created_at, manifest.created_at);
    }

    #[test]
    fn rejects_future_schema() {
        let json = r#"{"schema_version": 999}"#;
        assert!(Manifest::from_json(json).is_err());
    }
}
