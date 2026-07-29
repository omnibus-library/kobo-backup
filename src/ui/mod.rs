pub mod screens;
pub mod widgets;

use ratatui::Frame;

use crate::app::{App, Screen};

/// Top-level render dispatcher. Pure: reads App, never mutates.
pub fn draw(f: &mut Frame, app: &App) {
    match &app.screen {
        Screen::Home { selected } => screens::home(f, app, *selected),
        Screen::ChooseBackupDir {
            selected,
            custom,
            error,
        } => screens::choose_backup_dir(f, app, *selected, custom, error),
        Screen::Detect {
            selected,
            manual,
            error,
            ..
        } => screens::detect(f, app, *selected, manual, error),
        Screen::DeviceInfo { proceed } => screens::device_info(f, app, *proceed),
        Screen::Progress {
            title,
            abort_overlay,
            abort_selected,
        } => screens::progress(f, app, title, *abort_overlay, *abort_selected),
        Screen::ScanSummary { proceed, scroll } => screens::scan_summary(f, app, *proceed, *scroll),
        Screen::BackupReport { scroll } => screens::backup_report(f, app, *scroll),
        Screen::PickZip {
            selected,
            manual,
            error,
        } => screens::pick_zip(f, app, *selected, manual, error),
        Screen::ZipSummary { proceed, scroll } => screens::zip_summary(f, app, *proceed, *scroll),
        Screen::SerialGate { typed } => screens::serial_gate(f, app, typed),
        Screen::DiffPreview {
            proceed,
            section,
            scroll,
        } => screens::diff_preview(f, app, *proceed, *section, *scroll),
        Screen::DeleteConsent { delete_selected } => {
            screens::delete_consent(f, app, *delete_selected)
        }
        Screen::TypedConfirm { typed } => screens::typed_confirm(f, app, typed),
        Screen::RestoreReport { scroll } => screens::restore_report(f, app, *scroll),
        Screen::SyncEndpointEntry { input, error } => {
            screens::sync_endpoint_entry(f, app, input, error)
        }
        Screen::SyncEndpointConfirm { apply } => screens::sync_endpoint_confirm(f, app, *apply),
        Screen::SyncEndpointReport => screens::sync_endpoint_report(f, app),
        Screen::Error { title, message } => screens::error(f, app, title, message),
        Screen::ConfirmQuit => screens::confirm_quit(f, app),
    }
}
