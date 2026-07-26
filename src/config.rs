//! Persistent configuration: where backups live.
//!
//! The file is a tiny TOML subset (`key = "value"`, `#` comments) parsed by
//! hand so the dependency tree stays lean. Unknown keys and malformed lines
//! are ignored rather than fatal — a corrupt config must never stop you from
//! reaching your backups.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const ENV_VAR: &str = "KOBO_BACKUP_DIR";
const FILE_NAME: &str = "config.toml";

/// Where the effective backup directory came from — surfaced in the UI so the
/// location is never a mystery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `--out <dir>` on the command line (this run only, never persisted).
    CommandLine,
    /// The `KOBO_BACKUP_DIR` environment variable.
    Environment,
    /// Read from the config file at this path.
    ConfigFile(PathBuf),
    /// Nothing configured yet — the wizard should ask before writing anything.
    Unset,
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::CommandLine => "set by --out for this run".into(),
            Source::Environment => format!("set by ${ENV_VAR}"),
            Source::ConfigFile(path) => format!("from {}", path.display()),
            Source::Unset => "not configured yet".into(),
        }
    }

    /// True when the user has never chosen a location, so we must ask.
    pub fn needs_prompt(&self) -> bool {
        matches!(self, Source::Unset)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub backup_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Resolution {
    /// The directory to actually use.
    pub dir: PathBuf,
    pub source: Source,
}

/// `$XDG_CONFIG_HOME/kobo-backup`, else `~/.config/kobo-backup`.
pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("kobo-backup");
        }
    }
    home().join(".config").join("kobo-backup")
}

pub fn config_file() -> PathBuf {
    config_dir().join(FILE_NAME)
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// The fallback shown when nothing is configured.
pub fn fallback_dir() -> PathBuf {
    home().join("KoboBackups")
}

/// Expand a leading `~` so hand-edited config files behave as expected.
pub fn expand_user_path(value: &str) -> PathBuf {
    if value == "~" {
        return home();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(value)
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Extract a value, honouring quoting, escapes and trailing `#` comments.
/// Returns `None` for a malformed value (e.g. an unterminated quote).
fn parse_value(raw: &str) -> Option<String> {
    let Some(rest) = raw.strip_prefix('"') else {
        // Unquoted: everything up to a trailing comment.
        return Some(raw.split('#').next().unwrap_or("").trim().to_string());
    };
    let mut escaped = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next() {
            // Closing quote: anything after it (e.g. a comment) is ignored.
            Some('"') => return Some(unescape(&escaped).trim().to_string()),
            Some('\\') => {
                escaped.push('\\');
                escaped.push(chars.next()?);
            }
            Some(other) => escaped.push(other),
            // Unterminated quote — treat the line as malformed.
            None => return None,
        }
    }
}

/// Parse the config text. Never fails: bad lines are skipped.
pub fn parse(content: &str) -> Config {
    let mut config = Config::default();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let Some(value) = parse_value(raw.trim()) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if key == "backup_dir" {
            config.backup_dir = Some(expand_user_path(&value));
        }
    }
    config
}

pub fn render(config: &Config) -> String {
    let mut out = String::from(
        "# kobo-backup configuration\n\
         #\n\
         # backup_dir: where backup .zip files are written, and where the\n\
         # restore wizard looks for them. Override for a single run with\n\
         # `--out <dir>` or the KOBO_BACKUP_DIR environment variable.\n",
    );
    if let Some(dir) = &config.backup_dir {
        out.push_str(&format!(
            "backup_dir = \"{}\"\n",
            escape(&dir.display().to_string())
        ));
    }
    out
}

/// Read the config file, returning an empty config when absent or unreadable.
pub fn load() -> Config {
    load_from(&config_file())
}

pub fn load_from(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Persist the chosen backup directory, creating the config dir if needed.
pub fn save_backup_dir_to(path: &Path, dir: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create config directory {}", parent.display()))?;
    }
    let config = Config {
        backup_dir: Some(dir.to_path_buf()),
    };
    std::fs::write(path, render(&config))
        .with_context(|| format!("cannot write config file {}", path.display()))?;
    Ok(())
}

/// Resolve the effective backup directory.
///
/// Precedence: `--out` > `$KOBO_BACKUP_DIR` > config file > unset (prompt).
pub fn resolve(cli_out: Option<PathBuf>) -> Resolution {
    let env = std::env::var(ENV_VAR).ok().filter(|v| !v.trim().is_empty());
    resolve_with(cli_out, env, load(), config_file())
}

/// Pure form of [`resolve`], so precedence is testable without touching the
/// process environment (which would race across parallel tests).
pub fn resolve_with(
    cli_out: Option<PathBuf>,
    env: Option<String>,
    config: Config,
    config_path: PathBuf,
) -> Resolution {
    if let Some(dir) = cli_out {
        return Resolution {
            dir,
            source: Source::CommandLine,
        };
    }
    if let Some(value) = env {
        return Resolution {
            dir: expand_user_path(value.trim()),
            source: Source::Environment,
        };
    }
    if let Some(dir) = config.backup_dir {
        return Resolution {
            dir,
            source: Source::ConfigFile(config_path),
        };
    }
    Resolution {
        dir: fallback_dir(),
        source: Source::Unset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_config() {
        let config = parse("backup_dir = \"/Volumes/Archive/Kobo\"\n");
        assert_eq!(
            config.backup_dir,
            Some(PathBuf::from("/Volumes/Archive/Kobo"))
        );
    }

    #[test]
    fn ignores_comments_blanks_and_unknown_keys() {
        let config = parse(
            "# a comment\n\n  \nfuture_option = \"x\"\nbackup_dir = \"/tmp/b\"\ngarbage line\n",
        );
        assert_eq!(config.backup_dir, Some(PathBuf::from("/tmp/b")));
    }

    #[test]
    fn malformed_config_is_not_fatal() {
        assert_eq!(parse("]]not toml[[").backup_dir, None);
        assert_eq!(parse("backup_dir =").backup_dir, None);
        assert_eq!(parse("backup_dir = \"\"").backup_dir, None);
        assert_eq!(parse("").backup_dir, None);
    }

    #[test]
    fn strips_trailing_comments() {
        assert_eq!(
            parse("backup_dir = \"/tmp/b\"  # where they go\n").backup_dir,
            Some(PathBuf::from("/tmp/b"))
        );
        assert_eq!(
            parse("backup_dir = /tmp/plain # unquoted too\n").backup_dir,
            Some(PathBuf::from("/tmp/plain"))
        );
    }

    #[test]
    fn hash_inside_quotes_is_part_of_the_path() {
        assert_eq!(
            parse("backup_dir = \"/tmp/my#folder\"\n").backup_dir,
            Some(PathBuf::from("/tmp/my#folder"))
        );
    }

    #[test]
    fn unterminated_quote_is_skipped() {
        assert_eq!(parse("backup_dir = \"/tmp/oops\n").backup_dir, None);
    }

    #[test]
    fn accepts_unquoted_values() {
        assert_eq!(
            parse("backup_dir = /tmp/plain\n").backup_dir,
            Some(PathBuf::from("/tmp/plain"))
        );
    }

    #[test]
    fn expands_tilde() {
        let config = parse("backup_dir = \"~/Backups/Kobo\"\n");
        let expected = home().join("Backups/Kobo");
        assert_eq!(config.backup_dir, Some(expected));
    }

    #[test]
    fn round_trips_paths_with_quotes_and_backslashes() {
        let weird = PathBuf::from(r#"/tmp/od"d\path"#);
        let text = render(&Config {
            backup_dir: Some(weird.clone()),
        });
        assert_eq!(parse(&text).backup_dir, Some(weird));
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/config.toml");
        let dir = PathBuf::from("/Volumes/Archive/Kobo");
        save_backup_dir_to(&path, &dir).unwrap();
        assert!(path.is_file());
        assert_eq!(load_from(&path).backup_dir, Some(dir));
    }

    #[test]
    fn missing_file_loads_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load_from(&tmp.path().join("nope.toml")), Config::default());
    }

    #[test]
    fn precedence_cli_beats_everything() {
        let resolution = resolve_with(
            Some(PathBuf::from("/cli")),
            Some("/env".into()),
            Config {
                backup_dir: Some(PathBuf::from("/config")),
            },
            PathBuf::from("/cfg.toml"),
        );
        assert_eq!(resolution.dir, PathBuf::from("/cli"));
        assert_eq!(resolution.source, Source::CommandLine);
    }

    #[test]
    fn precedence_env_beats_config() {
        let resolution = resolve_with(
            None,
            Some("/env".into()),
            Config {
                backup_dir: Some(PathBuf::from("/config")),
            },
            PathBuf::from("/cfg.toml"),
        );
        assert_eq!(resolution.dir, PathBuf::from("/env"));
        assert_eq!(resolution.source, Source::Environment);
    }

    #[test]
    fn precedence_config_beats_default() {
        let resolution = resolve_with(
            None,
            None,
            Config {
                backup_dir: Some(PathBuf::from("/config")),
            },
            PathBuf::from("/cfg.toml"),
        );
        assert_eq!(resolution.dir, PathBuf::from("/config"));
        assert_eq!(
            resolution.source,
            Source::ConfigFile(PathBuf::from("/cfg.toml"))
        );
        assert!(!resolution.source.needs_prompt());
    }

    #[test]
    fn nothing_configured_requests_a_prompt() {
        let resolution = resolve_with(None, None, Config::default(), PathBuf::from("/cfg.toml"));
        assert_eq!(resolution.source, Source::Unset);
        assert!(resolution.source.needs_prompt());
        assert_eq!(resolution.dir, fallback_dir());
    }

    #[test]
    fn env_var_tilde_is_expanded() {
        let resolution = resolve_with(
            None,
            Some("~/EnvBackups".into()),
            Config::default(),
            PathBuf::from("/cfg.toml"),
        );
        assert_eq!(resolution.dir, home().join("EnvBackups"));
    }
}
