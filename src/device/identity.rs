use anyhow::{bail, Result};

/// Identity of a Kobo device, parsed from `.kobo/version`.
///
/// The version file is a single comma-separated line, e.g.:
/// `N428700012345,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000384`
/// Field 0 is the device serial; the third field is the firmware version;
/// the last field is the model id whose trailing digits identify the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub serial: String,
    pub firmware: String,
    pub model_id: String,
    pub model_name: String,
    /// The raw version file content, preserved verbatim for the manifest.
    pub raw: String,
}

pub fn parse_version_file(content: &str) -> Result<DeviceIdentity> {
    let line = content.trim();
    if line.is_empty() {
        bail!(".kobo/version file is empty");
    }
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 2 {
        bail!(
            ".kobo/version does not look like a Kobo version file (expected comma-separated fields, got {line:?})"
        );
    }
    let serial = fields[0].trim().to_string();
    if serial.is_empty() {
        bail!(".kobo/version has an empty serial field");
    }

    let looks_like_version = |s: &str| -> bool {
        s.split('.').count() >= 2 && s.chars().next().is_some_and(|c| c.is_ascii_digit())
    };

    // Firmware is conventionally field 2, but scan for the last plausible
    // dotted-numeric field that is not a "3.0.35+" hardware revision.
    let firmware = fields
        .get(2)
        .filter(|s| looks_like_version(s) && !s.ends_with('+'))
        .map(|s| s.to_string())
        .or_else(|| {
            fields
                .iter()
                .skip(1)
                .filter(|s| looks_like_version(s) && !s.ends_with('+'))
                .max_by_key(|s| s.len())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let model_id = fields
        .last()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let model_name = model_name_for_id(&model_id, &serial);

    Ok(DeviceIdentity {
        serial,
        firmware,
        model_id,
        model_name,
        raw: line.to_string(),
    })
}

/// Best-effort mapping of the model-id suffix to a friendly name.
/// Unknown ids fall back to the raw id — never an error (display only).
fn model_name_for_id(model_id: &str, _serial: &str) -> String {
    let suffix = model_id.rsplit('-').next().unwrap_or("");
    let code = suffix.trim_start_matches('0');
    let name = match code {
        "310" => "Kobo Touch",
        "320" => "Kobo Touch C",
        "330" => "Kobo Glo",
        "340" => "Kobo Mini",
        "350" => "Kobo Aura HD",
        "360" => "Kobo Aura",
        "370" => "Kobo Aura H2O",
        "371" => "Kobo Glo HD",
        "372" => "Kobo Touch 2.0",
        "373" => "Kobo Aura ONE",
        "374" => "Kobo Aura H2O Edition 2",
        "375" => "Kobo Aura Edition 2",
        "376" => "Kobo Clara HD",
        "377" => "Kobo Forma",
        "378" => "Kobo Libra H2O",
        "379" => "Kobo Nia",
        "380" => "Kobo Elipsa",
        "381" => "Kobo Sage",
        "382" => "Kobo Libra 2",
        "383" => "Kobo Clara 2E",
        "384" => "Kobo Elipsa 2E",
        "386" => "Kobo Clara BW",
        "387" => "Kobo Clara Colour",
        "388" => "Kobo Libra Colour",
        _ => "",
    };
    if name.is_empty() {
        if model_id.is_empty() {
            "Unknown Kobo model".to_string()
        } else {
            format!("Kobo (model id {model_id})")
        }
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_libra2_style_version() {
        let raw =
            "N428700012345,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382";
        let id = parse_version_file(raw).unwrap();
        assert_eq!(id.serial, "N428700012345");
        assert_eq!(id.firmware, "4.41.23145");
        assert_eq!(id.model_name, "Kobo Libra 2");
        assert_eq!(id.raw, raw);
    }

    #[test]
    fn parses_older_touch_version() {
        let raw = "N905B12345678,2.6.1,2.6.1,2.6.1,2.6.1,00000000-0000-0000-0000-000000000310";
        let id = parse_version_file(raw).unwrap();
        assert_eq!(id.serial, "N905B12345678");
        assert_eq!(id.firmware, "2.6.1");
        assert_eq!(id.model_name, "Kobo Touch");
    }

    #[test]
    fn unknown_model_falls_back_to_raw_id() {
        let raw =
            "N999900012345,3.0.35+,5.1.100,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000999";
        let id = parse_version_file(raw).unwrap();
        assert!(id.model_name.contains("999"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_version_file("").is_err());
        assert!(parse_version_file("not a version file").is_err());
    }

    #[test]
    fn trailing_newline_ok() {
        let raw = "N123400012345,3.0.35+,4.38.21908,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000376\n";
        let id = parse_version_file(raw).unwrap();
        assert_eq!(id.model_name, "Kobo Clara HD");
        assert_eq!(id.firmware, "4.38.21908");
    }
}
