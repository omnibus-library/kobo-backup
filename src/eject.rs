//! Ejecting the connected Kobo — `run_eject` is the only part that touches the system.

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

/// The program plus arguments that eject `mount` on `os`; `None` if unknown.
pub fn eject_command(mount: &Path, os: &str) -> Option<(String, Vec<String>)> {
    let mount = mount.display().to_string();
    match os {
        "macos" => Some(("diskutil".to_string(), vec!["eject".to_string(), mount])),
        "linux" => Some(("umount".to_string(), vec![mount])),
        _ => None,
    }
}

/// Classify a finished eject command, with the command's own words on failure.
fn outcome_from(
    mount: &Path,
    program: &str,
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> EjectOutcome {
    if status.success() {
        return EjectOutcome::success(mount);
    }
    let mut detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        detail = String::from_utf8_lossy(stdout).trim().to_string();
    }
    if detail.is_empty() {
        detail = format!("{program} exited with {status}");
    }
    EjectOutcome::failure(mount, detail)
}

/// Run the platform eject command and report the result.
pub fn run_eject(mount: &Path) -> EjectOutcome {
    let Some((program, args)) = eject_command(mount, std::env::consts::OS) else {
        return EjectOutcome::failure(mount, UNSUPPORTED);
    };
    match std::process::Command::new(&program).args(&args).output() {
        Ok(out) => outcome_from(mount, &program, out.status, &out.stdout, &out.stderr),
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

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(unix)]
    #[test]
    fn outcome_from_reports_success_on_a_zero_exit() {
        let outcome = outcome_from(
            Path::new("/Volumes/KOBOeReader"),
            "diskutil",
            exit_status(0),
            b"",
            b"",
        );
        assert!(outcome.ok);
        assert_eq!(outcome.detail, "");
    }

    #[cfg(unix)]
    #[test]
    fn outcome_from_uses_trimmed_stderr_on_failure() {
        let outcome = outcome_from(
            Path::new("/mnt/kobo"),
            "umount",
            exit_status(1),
            b"",
            b"  umount: /mnt/kobo: target is busy.\n",
        );
        assert!(!outcome.ok);
        assert_eq!(outcome.detail, "umount: /mnt/kobo: target is busy.");
    }

    #[cfg(unix)]
    #[test]
    fn outcome_from_falls_back_to_stdout_when_stderr_is_empty() {
        let outcome = outcome_from(
            Path::new("/Volumes/KOBOeReader"),
            "diskutil",
            exit_status(1),
            b"Disk /Volumes/KOBOeReader could not be unmounted.\n",
            b"",
        );
        assert!(!outcome.ok);
        assert_eq!(
            outcome.detail,
            "Disk /Volumes/KOBOeReader could not be unmounted."
        );
    }

    #[cfg(unix)]
    #[test]
    fn outcome_from_mentions_the_program_and_status_when_both_streams_are_empty() {
        let outcome = outcome_from(Path::new("/mnt/kobo"), "umount", exit_status(1), b"", b"");
        assert!(!outcome.ok);
        assert!(outcome.detail.contains("umount"));
        assert!(outcome.detail.contains("exited with"));
    }
}
