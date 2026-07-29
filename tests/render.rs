//! Render every screen the wizards pass through on a TestBackend terminal.
//! Catches layout panics and blank screens that state-machine tests miss.

mod common;

use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use kobo_backup::app::{App, Screen, RESTORE_PHRASE};
use kobo_backup::event::Event;
use kobo_backup::ui;
use kobo_backup::CliArgs;

use common::stage_fake_device;

struct Harness {
    app: App,
    rx: Receiver<Event>,
    terminal: Terminal<TestBackend>,
    seen: Vec<&'static str>,
}

impl Harness {
    fn new(mount: &std::path::Path, out_dir: &std::path::Path) -> Self {
        let (tx, rx) = unbounded();
        let args = CliArgs {
            device: Some(mount.to_path_buf()),
            out_dir: Some(out_dir.to_path_buf()),
        };
        Harness {
            app: App::new(&args, tx),
            rx,
            terminal: Terminal::new(TestBackend::new(120, 40)).unwrap(),
            seen: Vec::new(),
        }
    }

    /// Draw the current screen; record its name; panic on any render error.
    fn draw(&mut self) {
        let name = screen_name(&self.app.screen);
        if !self.seen.contains(&name) {
            self.seen.push(name);
        }
        self.terminal.draw(|f| ui::draw(f, &self.app)).unwrap();
        // The frame must never be entirely blank.
        let buffer = self.terminal.backend().buffer();
        assert!(
            buffer.content().iter().any(|cell| cell.symbol() != " "),
            "screen {name} rendered completely blank"
        );
    }

    fn key(&mut self, code: KeyCode) {
        self.app
            .handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        self.draw();
    }

    fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    fn pump_until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(&self.app) {
            assert!(Instant::now() < deadline, "timeout waiting for {what}");
            let event = self
                .rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no events while waiting for {what}"));
            self.app.handle(event);
            self.draw();
        }
    }
}

fn screen_name(screen: &Screen) -> &'static str {
    match screen {
        Screen::Home { .. } => "Home",
        Screen::ChooseBackupDir { .. } => "ChooseBackupDir",
        Screen::Detect { .. } => "Detect",
        Screen::DeviceInfo { .. } => "DeviceInfo",
        Screen::Progress { .. } => "Progress",
        Screen::ScanSummary { .. } => "ScanSummary",
        Screen::BackupReport { .. } => "BackupReport",
        Screen::PickZip { .. } => "PickZip",
        Screen::ZipSummary { .. } => "ZipSummary",
        Screen::SerialGate { .. } => "SerialGate",
        Screen::DiffPreview { .. } => "DiffPreview",
        Screen::DeleteConsent { .. } => "DeleteConsent",
        Screen::TypedConfirm { .. } => "TypedConfirm",
        Screen::RestoreReport { .. } => "RestoreReport",
        Screen::SyncEndpointEntry { .. } => "SyncEndpointEntry",
        Screen::SyncEndpointConfirm { .. } => "SyncEndpointConfirm",
        Screen::SyncEndpointReport => "SyncEndpointReport",
        Screen::Error { .. } => "Error",
        Screen::ConfirmQuit => "ConfirmQuit",
    }
}

#[test]
fn every_screen_renders_through_both_flows() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut h = Harness::new(&mount, out.path());
    h.draw(); // Home

    // Backup flow.
    h.key(KeyCode::Enter); // Detect
    h.key(KeyCode::Enter); // DeviceInfo
    h.key(KeyCode::Tab);
    h.key(KeyCode::Enter); // Progress (scan)
    h.pump_until("scan summary", |a| {
        matches!(a.screen, Screen::ScanSummary { .. })
    });
    h.key(KeyCode::Down); // exercise scroll render
    h.key(KeyCode::Tab);
    h.key(KeyCode::Enter); // Progress (write) → Progress (verify)
    h.pump_until("backup report", |a| {
        matches!(a.screen, Screen::BackupReport { .. })
    });
    h.key(KeyCode::Enter); // Home

    // Mutate device so the restore plan has all sections populated.
    std::fs::write(mount.join("stray.txt"), b"extra").unwrap();
    common::add_annotation(&mount);

    // Restore flow.
    h.key(KeyCode::Down);
    h.key(KeyCode::Enter); // PickZip
    h.key(KeyCode::Enter); // Progress (validate)
    h.pump_until("zip summary", |a| {
        matches!(a.screen, Screen::ZipSummary { .. })
    });
    h.key(KeyCode::Tab);
    h.key(KeyCode::Enter); // Detect
    h.key(KeyCode::Enter); // diff job
    h.pump_until("diff preview", |a| {
        matches!(a.screen, Screen::DiffPreview { .. })
    });
    // Render every diff section tab.
    h.key(KeyCode::Tab);
    h.key(KeyCode::Tab);
    h.key(KeyCode::Tab);
    h.key(KeyCode::Tab);
    h.key(KeyCode::Right);
    h.key(KeyCode::Enter); // DeleteConsent
    h.key(KeyCode::Enter); // TypedConfirm
    h.type_str(RESTORE_PHRASE);
    h.key(KeyCode::Enter); // micro-backup → apply → verify
    h.pump_until("restore report", |a| {
        matches!(a.screen, Screen::RestoreReport { .. })
    });
    h.key(KeyCode::Enter); // Home

    // Configure-sync flow.
    std::env::set_var("KOBO_BACKUP_CONF_EDIT_DIR", out.path().join("conf-edits"));
    h.key(KeyCode::Down);
    h.key(KeyCode::Down);
    h.key(KeyCode::Enter); // Detect
    h.key(KeyCode::Enter); // SyncEndpointEntry
    h.type_str("https://omni.example.com/kobo/tok");
    h.key(KeyCode::Enter); // SyncEndpointConfirm
    h.key(KeyCode::Tab); // select Apply
    h.key(KeyCode::Enter); // SyncEndpointReport
    assert!(
        matches!(h.app.screen, Screen::SyncEndpointReport),
        "apply must land on the report (screen: {:?})",
        h.app.screen
    );
    h.key(KeyCode::Enter); // Home

    // Overlay screens.
    h.app.screen = Screen::ChooseBackupDir {
        selected: 0,
        custom: None,
        error: None,
    };
    h.draw();
    h.app.screen = Screen::ChooseBackupDir {
        selected: App::backup_dir_choices().len(),
        custom: Some("/tmp/some/path".into()),
        error: Some("cannot use that directory".into()),
    };
    h.draw();
    h.app.screen = Screen::ConfirmQuit;
    h.draw();
    h.app.screen = Screen::Error {
        title: "Operation failed".into(),
        message: "example error".into(),
    };
    h.draw();
    h.app.screen = Screen::SerialGate {
        typed: "DIFF".into(),
    };
    h.draw();
    h.app.screen = Screen::Progress {
        title: "Scanning".into(),
        abort_overlay: true,
        abort_selected: true,
    };
    h.draw();

    let expected = [
        "Home",
        "ChooseBackupDir",
        "Detect",
        "DeviceInfo",
        "Progress",
        "ScanSummary",
        "BackupReport",
        "PickZip",
        "ZipSummary",
        "DiffPreview",
        "DeleteConsent",
        "TypedConfirm",
        "RestoreReport",
        "SyncEndpointEntry",
        "SyncEndpointConfirm",
        "SyncEndpointReport",
        "ConfirmQuit",
        "Error",
        "SerialGate",
    ];
    for name in expected {
        assert!(h.seen.contains(&name), "screen {name} was never rendered");
    }
}
