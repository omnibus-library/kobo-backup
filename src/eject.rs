//! Ejecting the connected Kobo — and saying what happened.
//!
//! The command itself is built by a pure function so it can be unit-tested
//! without running anything; `run_eject` is the only part that touches the
//! system. Nothing here is fire-and-forget: every attempt produces an
//! `EjectOutcome` that a screen renders verbatim.

use std::path::{Path, PathBuf};

/// What one eject attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EjectOutcome {
    pub mount: PathBuf,
    pub ok: bool,
    /// The command's own words when it refused. Empty on success.
    pub detail: String,
}

/// Shown under a failed eject: the usual reason a volume will not unmount.
pub const BUSY_HINT: &str = "The volume is probably still in use — close Finder windows and any \
                             terminal sitting inside it, then try again.";

/// Detail used when this platform has no eject command we know.
pub const UNSUPPORTED: &str =
    "ejecting is not supported on this platform; eject from your file manager";

impl EjectOutcome {
    pub fn success(mount: &Path) -> Self {
        EjectOutcome {
            mount: mount.to_path_buf(),
            ok: true,
            detail: String::new(),
        }
    }

    pub fn failure(mount: &Path, detail: impl Into<String>) -> Self {
        EjectOutcome {
            mount: mount.to_path_buf(),
            ok: false,
            detail: detail.into(),
        }
    }

    /// The single line every screen shows after an eject.
    pub fn headline(&self) -> String {
        if self.ok {
            format!("✓ Ejected {} — safe to unplug", self.mount.display())
        } else {
            format!(
                "✗ Could not eject {}: {}",
                self.mount.display(),
                self.detail
            )
        }
    }
}

/// Program plus arguments that eject `mount` on `os` (the values of
/// `std::env::consts::OS`). `None` means this platform has no command we know.
pub fn eject_command(mount: &Path, os: &str) -> Option<(String, Vec<String>)> {
    let mount = mount.display().to_string();
    match os {
        "macos" => Some(("diskutil".to_string(), vec!["eject".to_string(), mount])),
        // We only ever know the mount path, so unmount by path and report
        // whatever the kernel says if it refuses.
        "linux" => Some(("umount".to_string(), vec![mount])),
        _ => None,
    }
}

/// Run the platform eject command and report the result. Never panics, never
/// swallows the reason for a failure.
pub fn run_eject(mount: &Path) -> EjectOutcome {
    let Some((program, args)) = eject_command(mount, std::env::consts::OS) else {
        return EjectOutcome::failure(mount, UNSUPPORTED);
    };
    match std::process::Command::new(&program).args(&args).output() {
        Ok(out) if out.status.success() => EjectOutcome::success(mount),
        Ok(out) => {
            let mut detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if detail.is_empty() {
                detail = String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
            if detail.is_empty() {
                detail = format!("{program} exited with {}", out.status);
            }
            EjectOutcome::failure(mount, detail)
        }
        Err(err) => EjectOutcome::failure(mount, format!("could not run {program}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn macos_ejects_with_diskutil() {
        let (program, args) = eject_command(Path::new("/Volumes/KOBOeReader"), "macos").unwrap();
        assert_eq!(program, "diskutil");
        assert_eq!(
            args,
            vec!["eject".to_string(), "/Volumes/KOBOeReader".to_string()]
        );
    }

    #[test]
    fn linux_unmounts_the_mount_path() {
        let (program, args) =
            eject_command(Path::new("/media/sloan/KOBOeReader"), "linux").unwrap();
        assert_eq!(program, "umount");
        assert_eq!(args, vec!["/media/sloan/KOBOeReader".to_string()]);
    }

    #[test]
    fn other_platforms_have_no_command() {
        assert!(eject_command(Path::new("D:\\"), "windows").is_none());
    }

    #[test]
    fn success_headline_says_it_is_safe_to_unplug() {
        let outcome = EjectOutcome::success(Path::new("/Volumes/KOBOeReader"));
        assert!(outcome.ok);
        assert_eq!(
            outcome.headline(),
            "✓ Ejected /Volumes/KOBOeReader — safe to unplug"
        );
    }

    #[test]
    fn failure_headline_quotes_the_command_output() {
        let outcome = EjectOutcome::failure(
            Path::new("/Volumes/KOBOeReader"),
            "Unmount failed: dissenter PID 501 (Finder)",
        );
        assert!(!outcome.ok);
        assert_eq!(
            outcome.headline(),
            "✗ Could not eject /Volumes/KOBOeReader: Unmount failed: dissenter PID 501 (Finder)"
        );
        assert_eq!(outcome.mount, PathBuf::from("/Volumes/KOBOeReader"));
    }

    #[test]
    fn unsupported_platform_is_reported_as_a_failure_with_advice() {
        // run_eject cannot be exercised for a foreign OS, but the message it
        // hands back is the constant the failure path uses.
        let outcome = EjectOutcome::failure(Path::new("/mnt/kobo"), UNSUPPORTED);
        assert!(outcome.headline().contains("not supported on this platform"));
        assert!(outcome.headline().contains("file manager"));
    }
}
