pub mod app;
pub mod archive;
pub mod config;
pub mod device;
pub mod eject;
pub mod event;
pub mod insights;
pub mod inventory;
pub mod manifest;
pub mod progress;
pub mod restore;
pub mod sync_endpoint;
pub mod ui;
pub mod util;
pub mod verify;
pub mod worker;

/// Command-line arguments (the app is otherwise fully interactive).
#[derive(Debug, Default, Clone)]
pub struct CliArgs {
    /// Manual device mount path (--device PATH).
    pub device: Option<std::path::PathBuf>,
    /// Output directory for backup zips (--out DIR). Default: ~/KoboBackups.
    pub out_dir: Option<std::path::PathBuf>,
}
