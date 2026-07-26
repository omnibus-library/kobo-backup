use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::identity::{parse_version_file, DeviceIdentity};
use crate::util;

/// A mounted Kobo volume.
#[derive(Debug, Clone)]
pub struct Device {
    pub mount: PathBuf,
    pub label: String,
    pub identity: DeviceIdentity,
    pub volume_total: u64,
    pub volume_free: u64,
}

impl Device {
    pub fn volume_used(&self) -> u64 {
        self.volume_total.saturating_sub(self.volume_free)
    }

    /// Cheap liveness probe used to detect surprise unplug mid-operation.
    pub fn is_present(&self) -> bool {
        self.mount.join(".kobo").is_dir()
    }
}

/// Roots under which removable volumes appear on this platform.
fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Volumes")];
    if cfg!(target_os = "linux") {
        if let Ok(user) = std::env::var("USER") {
            roots.push(PathBuf::from(format!("/media/{user}")));
            roots.push(PathBuf::from(format!("/run/media/{user}")));
        }
        roots.push(PathBuf::from("/media"));
    }
    roots
}

/// Scan mount roots for volumes that look like a Kobo (contain `.kobo/version`).
pub fn scan() -> Vec<Device> {
    let mut found = Vec::new();
    for root in candidate_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let mount = entry.path();
            if !mount.is_dir() {
                continue;
            }
            if let Ok(device) = probe(&mount) {
                found.push(device);
            }
        }
    }
    found
}

/// Validate that `mount` is a Kobo volume and read its identity.
pub fn probe(mount: &Path) -> Result<Device> {
    let version_path = mount.join(".kobo").join("version");
    let meta = std::fs::metadata(&version_path)
        .with_context(|| format!("no Kobo detected: {} not found", version_path.display()))?;
    if !meta.is_file() || meta.len() > 4096 {
        bail!(
            "{} exists but does not look like a Kobo version file",
            version_path.display()
        );
    }
    let content = std::fs::read_to_string(&version_path)
        .with_context(|| format!("cannot read {}", version_path.display()))?;
    let identity = parse_version_file(&content)?;
    let (volume_total, volume_free) = util::disk_space(mount).unwrap_or((0, 0));
    let label = mount
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| mount.display().to_string());
    Ok(Device {
        mount: mount.to_path_buf(),
        label,
        identity,
        volume_total,
        volume_free,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_device_dir(tmp: &Path) -> PathBuf {
        let mount = tmp.join("KOBOeReader");
        fs::create_dir_all(mount.join(".kobo")).unwrap();
        fs::write(
            mount.join(".kobo/version"),
            "N000TESTSERIAL,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
        )
        .unwrap();
        mount
    }

    #[test]
    fn probe_accepts_fake_device() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = fake_device_dir(tmp.path());
        let device = probe(&mount).unwrap();
        assert_eq!(device.identity.serial, "N000TESTSERIAL");
        assert_eq!(device.label, "KOBOeReader");
        assert!(device.is_present());
    }

    #[test]
    fn probe_rejects_plain_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(probe(tmp.path()).is_err());
    }
}
