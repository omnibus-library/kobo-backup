//! Configure the device's wireless sync endpoint: read and rewrite the
//! `api_endpoint` key in the `[OneStoreServices]` section of
//! `.kobo/Kobo/Kobo eReader.conf`. This is the mechanism self-hosted sync
//! servers (Omnibus, Calibre-Web) document as their setup step — there is no
//! on-device UI for it. One of exactly two code paths allowed to write to the
//! Kobo (the other is `restore::apply`), and it follows the same discipline:
//! safety copy first, write to `.kbtmp`, verify by re-reading, then rename.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::device::Device;

/// The conf file, relative to the mount root.
pub const CONF_RELATIVE: &str = ".kobo/Kobo/Kobo eReader.conf";

const SECTION_HEADER: &str = "[OneStoreServices]";
const KEY: &str = "api_endpoint";

/// What an applied edit did — everything the report screen shows.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The value that was in the conf before the edit, if any.
    pub old: Option<String>,
    pub new: String,
    /// Verbatim copy of the pre-edit conf; `None` only when the device had no
    /// conf file at all (nothing existed to copy).
    pub safety_copy: Option<PathBuf>,
    pub conf_path: PathBuf,
}

/// Validate a pasted endpoint URL. Returns the normalized form (trimmed,
/// trailing slashes stripped) or a message suitable for inline display.
pub fn validate_url(input: &str) -> Result<String, String> {
    let url = input.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err("paste the endpoint URL from your sync server".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URL must not contain spaces".into());
    }
    let rest = if let Some(r) = url.strip_prefix("https://") {
        r
    } else if let Some(r) = url.strip_prefix("http://") {
        r
    } else {
        return Err("the URL must start with https:// or http://".into());
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(
            "the URL has no host (e.g. https://your-server.example.com/kobo/<token>)".into(),
        );
    }
    Ok(url.to_string())
}

/// The current `api_endpoint` value in `conf_text`, if the section and key
/// exist. Section and key matching is exact — QSettings writes both verbatim.
pub fn read_endpoint(conf_text: &str) -> Option<String> {
    let mut in_section = false;
    for line in conf_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == SECTION_HEADER;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(KEY) {
            if let Some(value) = value.trim_start().strip_prefix('=') {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Return `conf_text` with `api_endpoint` set to `url`: replace the key in
/// place when present, insert it under an existing `[OneStoreServices]`
/// header, or append the section when the file has neither. Every other line
/// is preserved. CRLF files stay CRLF (line endings are normalized per file,
/// not mixed).
pub fn set_endpoint(conf_text: &str, url: &str) -> String {
    let crlf = conf_text.contains("\r\n");
    let text = if crlf {
        conf_text.replace("\r\n", "\n")
    } else {
        conf_text.to_string()
    };

    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let new_line = format!("{KEY}={url}");

    let section_at = lines.iter().position(|line| line.trim() == SECTION_HEADER);

    match section_at {
        Some(header) => {
            // The section runs until the next `[header]` or end of file.
            let section_end = lines[header + 1..]
                .iter()
                .position(|line| line.trim().starts_with('['))
                .map(|offset| header + 1 + offset)
                .unwrap_or(lines.len());
            let key_at = lines[header + 1..section_end].iter().position(|line| {
                line.trim()
                    .strip_prefix(KEY)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            });
            match key_at {
                Some(offset) => lines[header + 1 + offset] = new_line,
                None => lines.insert(header + 1, new_line),
            }
        }
        None => {
            if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(SECTION_HEADER.to_string());
            lines.push(new_line);
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    if crlf {
        out = out.replace('\n', "\r\n");
    }
    out
}

/// Apply `url` to the device's conf file. Safety-copies the current conf to
/// `<safety_root>/<serial>-<timestamp>/` first, writes the new content to
/// `<conf>.kbtmp` (fsynced), renames it over the target, then re-reads the
/// file and verifies the value landed. Never touches anything else on the
/// device.
pub fn apply(device: &Device, url: &str, safety_root: &Path) -> Result<Outcome> {
    if !device.is_present() {
        bail!("the device disappeared — reconnect it and try again; nothing was written");
    }
    let conf_path = device.mount.join(CONF_RELATIVE);

    let old_text = match std::fs::read_to_string(&conf_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| format!("cannot read {}", conf_path.display()))
        }
    };

    let safety_copy = match &old_text {
        Some(_) => {
            let dir = safety_root.join(format!(
                "{}-{}",
                device.identity.serial,
                crate::util::filename_timestamp()
            ));
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            let copy = dir.join("Kobo eReader.conf");
            std::fs::copy(&conf_path, &copy)
                .with_context(|| format!("cannot safety-copy {}", conf_path.display()))?;
            Some(copy)
        }
        None => None,
    };

    let new_text = set_endpoint(old_text.as_deref().unwrap_or(""), url);
    if let Some(parent) = conf_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    let tmp = {
        let mut os = conf_path.as_os_str().to_owned();
        os.push(".kbtmp");
        PathBuf::from(os)
    };
    let write = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(new_text.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.context(format!("cannot write {}", tmp.display())));
    }
    std::fs::rename(&tmp, &conf_path)
        .with_context(|| format!("cannot move new conf into place: {}", conf_path.display()))?;

    // Trust nothing: the value must read back from the file on the device.
    let landed = std::fs::read_to_string(&conf_path)
        .with_context(|| format!("cannot re-read {}", conf_path.display()))?;
    if read_endpoint(&landed).as_deref() != Some(url) {
        bail!(
            "the new endpoint did not read back from {} — the pre-edit conf is saved at {}",
            conf_path.display(),
            safety_copy
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(no prior conf existed)".into()),
        );
    }

    Ok(Outcome {
        old: old_text.as_deref().and_then(read_endpoint),
        new: url.to_string(),
        safety_copy,
        conf_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = "[ApplicationPreferences]\nLastOpened=book\n\n\
                        [OneStoreServices]\napi_endpoint=https://storeapi.kobo.com\n\
                        products_path=https://storeapi.kobo.com/products\n\n\
                        [FeatureSettings]\nExportHighlights=true\n";

    #[test]
    fn validate_url_accepts_and_normalizes_a_pasted_endpoint() {
        assert_eq!(
            validate_url("  https://omni.example.com/kobo/tok123/ "),
            Ok("https://omni.example.com/kobo/tok123".to_string())
        );
        assert_eq!(
            validate_url("http://192.168.1.10:3000/kobo/tok"),
            Ok("http://192.168.1.10:3000/kobo/tok".to_string())
        );
    }

    #[test]
    fn validate_url_rejects_empty_schemeless_hostless_and_spaced_input() {
        assert!(validate_url("").is_err());
        assert!(validate_url("   ").is_err());
        assert!(validate_url("omni.example.com/kobo/tok").is_err());
        assert!(validate_url("ftp://omni.example.com").is_err());
        assert!(validate_url("https:///kobo/tok").is_err());
        assert!(validate_url("https://omni.example .com").is_err());
    }

    #[test]
    fn read_endpoint_finds_the_key_only_inside_the_section() {
        assert_eq!(
            read_endpoint(CONF),
            Some("https://storeapi.kobo.com".to_string())
        );
        // Same key outside the section must not match.
        let decoy = "[OtherSection]\napi_endpoint=https://wrong.example.com\n";
        assert_eq!(read_endpoint(decoy), None);
        assert_eq!(read_endpoint(""), None);
    }

    #[test]
    fn set_endpoint_replaces_in_place_and_preserves_every_other_line() {
        let out = set_endpoint(CONF, "https://omni.example.com/kobo/tok");
        assert_eq!(
            read_endpoint(&out),
            Some("https://omni.example.com/kobo/tok".to_string())
        );
        assert!(out.contains("LastOpened=book"));
        assert!(out.contains("products_path=https://storeapi.kobo.com/products"));
        assert!(out.contains("ExportHighlights=true"));
        assert!(!out.contains("api_endpoint=https://storeapi.kobo.com\n"));
    }

    #[test]
    fn set_endpoint_inserts_into_an_existing_section_without_the_key() {
        let conf = "[OneStoreServices]\nproducts_path=https://storeapi.kobo.com/products\n";
        let out = set_endpoint(conf, "https://omni.example.com/kobo/tok");
        assert_eq!(
            read_endpoint(&out),
            Some("https://omni.example.com/kobo/tok".to_string())
        );
        assert!(out.contains("products_path="));
    }

    #[test]
    fn set_endpoint_appends_the_section_when_absent() {
        let conf = "[FeatureSettings]\nExportHighlights=true\n";
        let out = set_endpoint(conf, "https://omni.example.com/kobo/tok");
        assert_eq!(
            read_endpoint(&out),
            Some("https://omni.example.com/kobo/tok".to_string())
        );
        assert!(out.contains("ExportHighlights=true"));
        // Works from a missing/empty file too.
        let from_empty = set_endpoint("", "https://omni.example.com/kobo/tok");
        assert_eq!(
            read_endpoint(&from_empty),
            Some("https://omni.example.com/kobo/tok".to_string())
        );
    }

    #[test]
    fn set_endpoint_keeps_crlf_files_crlf() {
        let conf = "[OneStoreServices]\r\napi_endpoint=https://storeapi.kobo.com\r\n";
        let out = set_endpoint(conf, "https://omni.example.com/kobo/tok");
        assert!(out.contains("api_endpoint=https://omni.example.com/kobo/tok\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn apply_edits_the_conf_and_leaves_a_safety_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        std::fs::create_dir_all(mount.join(".kobo/Kobo")).unwrap();
        std::fs::write(
            mount.join(".kobo/version"),
            "N000TESTSERIAL,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
        )
        .unwrap();
        std::fs::write(mount.join(CONF_RELATIVE), CONF).unwrap();
        let device = crate::device::probe(&mount).unwrap();
        let safety_root = tmp.path().join("conf-edits");

        let outcome = apply(&device, "https://omni.example.com/kobo/tok", &safety_root).unwrap();

        assert_eq!(outcome.old, Some("https://storeapi.kobo.com".to_string()));
        assert_eq!(outcome.new, "https://omni.example.com/kobo/tok");
        let copy = outcome.safety_copy.expect("safety copy for existing conf");
        assert_eq!(std::fs::read_to_string(copy).unwrap(), CONF);
        let landed = std::fs::read_to_string(mount.join(CONF_RELATIVE)).unwrap();
        assert_eq!(
            read_endpoint(&landed),
            Some("https://omni.example.com/kobo/tok".to_string())
        );
        assert!(landed.contains("ExportHighlights=true"));
    }

    #[test]
    fn apply_creates_the_conf_when_the_device_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        std::fs::create_dir_all(mount.join(".kobo")).unwrap();
        std::fs::write(
            mount.join(".kobo/version"),
            "N000TESTSERIAL,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
        )
        .unwrap();
        let device = crate::device::probe(&mount).unwrap();
        let safety_root = tmp.path().join("conf-edits");

        let outcome = apply(&device, "https://omni.example.com/kobo/tok", &safety_root).unwrap();

        assert_eq!(outcome.old, None);
        assert!(outcome.safety_copy.is_none());
        let landed = std::fs::read_to_string(mount.join(CONF_RELATIVE)).unwrap();
        assert_eq!(
            read_endpoint(&landed),
            Some("https://omni.example.com/kobo/tok".to_string())
        );
    }

    #[test]
    fn apply_refuses_a_vanished_device() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        std::fs::create_dir_all(mount.join(".kobo")).unwrap();
        std::fs::write(
            mount.join(".kobo/version"),
            "N000TESTSERIAL,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
        )
        .unwrap();
        let device = crate::device::probe(&mount).unwrap();
        std::fs::remove_dir_all(&mount).unwrap();

        let err = apply(
            &device,
            "https://omni.example.com/kobo/tok",
            &tmp.path().join("conf-edits"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("nothing was written"));
    }
}
