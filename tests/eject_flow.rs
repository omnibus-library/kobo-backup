//! The eject action, driven through the real App state machine. A stub
//! ejector stands in for diskutil/umount and records what it was asked to do.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use kobo_backup::app::{App, Ejector, Screen};
use kobo_backup::eject::EjectOutcome;
use kobo_backup::event::Event;
use kobo_backup::{ui, CliArgs};

use common::stage_fake_device;

fn key(app: &mut App, code: KeyCode) {
    app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn app_for(mount: &Path, out_dir: &Path) -> App {
    let (tx, _rx) = unbounded();
    let args = CliArgs {
        device: Some(mount.to_path_buf()),
        out_dir: Some(out_dir.to_path_buf()),
    };
    App::new(&args, tx)
}

/// Records every mount it is asked to eject. With `unmount`, it also removes
/// `.kobo` so a later rescan sees the volume as gone, the way a real eject
/// would.
fn recording_ejector(calls: Arc<Mutex<Vec<PathBuf>>>, ok: bool, unmount: bool) -> Ejector {
    Box::new(move |mount: &Path| {
        calls.lock().unwrap().push(mount.to_path_buf());
        if ok {
            if unmount {
                let _ = std::fs::remove_dir_all(mount.join(".kobo"));
            }
            EjectOutcome::success(mount)
        } else {
            EjectOutcome::failure(mount, "Unmount failed: dissenter PID 501 (Finder)")
        }
    })
}

/// The whole screen as text, one string per terminal row.
fn rendered(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(200, 40)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn sync_report_ejects_with_e_and_reports_it() {
    let (guard, mount) = stage_fake_device();
    std::env::set_var("KOBO_BACKUP_CONF_EDIT_DIR", guard.path().join("conf-edits"));
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.set_ejector(recording_ejector(calls.clone(), true, false));

    // Home → Configure wireless sync → device → URL → confirm → apply.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter); // select the device
    type_str(&mut app, "https://omni.example.com/kobo/tok123");
    key(&mut app, KeyCode::Enter); // preview
    key(&mut app, KeyCode::Tab); // select Apply
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::SyncEndpointReport));

    key(&mut app, KeyCode::Char('e'));
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[mount.clone()],
        "e must eject the device the flow just edited"
    );
    assert!(app.last_eject.as_ref().expect("outcome recorded").ok);
    assert!(
        matches!(app.screen, Screen::SyncEndpointReport),
        "ejecting must not navigate away from the report"
    );

    let screen = rendered(&app);
    assert!(
        screen.contains("✓ Ejected"),
        "the report must show the eject outcome:\n{screen}"
    );
}

#[test]
fn a_failed_eject_is_reported_not_swallowed() {
    let (guard, mount) = stage_fake_device();
    std::env::set_var("KOBO_BACKUP_CONF_EDIT_DIR", guard.path().join("conf-edits"));
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.set_ejector(recording_ejector(calls.clone(), false, false));

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    type_str(&mut app, "https://omni.example.com/kobo/tok123");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('e'));

    let outcome = app.last_eject.as_ref().expect("outcome recorded");
    assert!(!outcome.ok);
    assert!(outcome.detail.contains("dissenter"));
    assert_eq!(calls.lock().unwrap().len(), 1);

    // Leaving the screen must not leave a stale message behind.
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::Home { .. }));
    assert!(app.last_eject.is_none(), "last_eject must clear on go_home");
}
