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
                    "kobo-backup {}\n\n\
                     Transparency-first backup & restore for Kobo e-readers.\n\n\
                     USAGE: kobo-backup [--device <mount-path>] [--out <backup-dir>]\n\n\
                     Runs an interactive terminal wizard. No file is written without\n\
                     an explicit confirmation step, and every write is verified.",
                    env!("CARGO_PKG_VERSION")
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
