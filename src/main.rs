use anyhow::Result;
use kobo_backup::{app, CliArgs};

fn parse_args() -> Result<CliArgs> {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--device" => {
                args.device = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("--device requires a path"))?
                        .into(),
                );
            }
            "--out" => {
                args.out_dir = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("--out requires a directory"))?
                        .into(),
                );
            }
            "--help" | "-h" => {
                println!(
                    r#"kobo-backup {version}

Transparency-first backup & restore for Kobo e-readers.

USAGE: kobo-backup [--device <mount-path>] [--out <backup-dir>]

OPTIONS:
  --device <path>  Use this mount instead of auto-detection
  --out <dir>      Backup folder for this run only
                   (the saved setting is not changed)

BACKUP FOLDER (highest precedence first):
  1. --out <dir>
  2. ${env_var} environment variable
  3. backup_dir in {config}
  4. asked on first run, then remembered

Runs an interactive terminal wizard. No file is written without
an explicit confirmation step, and every write is verified."#,
                    version = env!("CARGO_PKG_VERSION"),
                    env_var = kobo_backup::config::ENV_VAR,
                    config = kobo_backup::config::config_file().display(),
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other} (try --help)"),
        }
    }
    Ok(args)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    app::run(args)
}
