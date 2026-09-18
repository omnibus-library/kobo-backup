//! The eject action, driven through the real App state machine. A stub
//! ejector stands in for diskutil/umount and records what it was asked to do.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use kobo_backup::app::{App, Ejector, HomeItem, Screen};
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

/// The whole screen as text, one string per terminal row, on a terminal of
/// the given size.
fn rendered_at(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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

/// The whole screen as text, one string per terminal row, on a roomy
/// terminal.
fn rendered(app: &App) -> String {
    rendered_at(app, 200, 40)
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
        std::slice::from_ref(&mount),
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

#[test]
fn home_outcome_is_visible_on_an_80x24_terminal() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.set_ejector(recording_ejector(calls.clone(), false, false));

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);

    let screen = rendered_at(&app, 80, 24);
    assert!(
        screen.contains("✗ Could not eject"),
        "the outcome headline must survive a small terminal:\n{screen}"
    );
    assert!(
        screen.contains("still in use"),
        "the busy hint must survive a small terminal:\n{screen}"
    );
    assert!(
        screen.contains("Backups folder:"),
        "the notes area must not be squeezed out entirely:\n{screen}"
    );
}

#[test]
fn a_remounted_kobo_clears_the_safe_to_unplug_line() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.set_ejector(recording_ejector(calls.clone(), true, true));

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);

    let screen = rendered(&app);
    assert!(
        screen.contains("safe to unplug"),
        "the eject outcome must render before the remount:\n{screen}"
    );

    // Re-plug: recreate the marker file the way stage_fake_device does.
    std::fs::create_dir_all(mount.join(".kobo")).unwrap();
    std::fs::write(
        mount.join(".kobo").join("version"),
        format!(
            "{},3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000382",
            common::TEST_SERIAL
        ),
    )
    .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    app.handle(Event::Tick);

    let screen = rendered(&app);
    assert!(
        screen.contains("Connected Kobo:"),
        "the remounted device must show again:\n{screen}"
    );
    assert!(
        !screen.contains("safe to unplug"),
        "a remounted device must not still claim to be safe to unplug:\n{screen}"
    );
}

#[test]
fn home_offers_eject_only_while_a_kobo_is_mounted() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());

    let items = app.home_items();
    assert_eq!(
        items.iter().position(|i| *i == HomeItem::Eject),
        Some(3),
        "eject must sit after Configure wireless sync so existing indices do not move"
    );
    assert_eq!(items.last(), Some(&HomeItem::Quit), "Quit stays last");
    assert!(app.devices.iter().any(|d| d.mount == mount));

    app.devices.clear();
    let items = app.home_items();
    assert!(!items.contains(&HomeItem::Eject));
    assert_eq!(items.len(), 4);
}

#[test]
fn home_eject_calls_the_ejector_and_the_device_disappears() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.set_ejector(recording_ejector(calls.clone(), true, true));

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    assert!(matches!(app.screen, Screen::Home { selected: 3 }));
    key(&mut app, KeyCode::Enter);

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        std::slice::from_ref(&mount)
    );
    assert!(app.last_eject.as_ref().expect("outcome recorded").ok);
    assert!(
        !app.home_items().contains(&HomeItem::Eject),
        "a device that is gone must not still offer eject"
    );
    assert!(matches!(app.screen, Screen::Home { .. }));

    let items = app.home_items();
    if let Screen::Home { selected } = app.screen {
        assert!(
            selected < items.len(),
            "selection must stay within the shrunk menu"
        );
    }
    key(&mut app, KeyCode::Enter);
    assert!(
        !app.should_quit,
        "a stray Enter right after ejecting must not quit the app"
    );
}

#[test]
fn home_eject_failure_keeps_the_device_listed() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.set_ejector(recording_ejector(calls.clone(), false, false));

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);

    let outcome = app.last_eject.as_ref().expect("outcome recorded");
    assert!(!outcome.ok);
    assert_eq!(outcome.mount, mount);
    assert!(
        app.home_items().contains(&HomeItem::Eject),
        "a device that refused to eject is still there"
    );
}

#[test]
fn home_rescans_on_a_tick_but_only_once_a_second() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    assert!(
        app.devices.iter().any(|d| d.mount == mount),
        "construction scans for a device"
    );

    app.devices.clear();
    app.handle(Event::Tick);
    assert!(
        app.devices.is_empty(),
        "a tick inside the throttle window must not rescan"
    );

    std::thread::sleep(std::time::Duration::from_millis(1100));
    app.handle(Event::Tick);
    assert!(
        app.devices.iter().any(|d| d.mount == mount),
        "after the throttle window a tick picks the device back up"
    );
}

#[test]
fn home_shows_the_connected_device_and_hides_the_line_when_it_is_gone() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());

    let screen = rendered(&app);
    assert!(
        screen.contains("Eject my Kobo"),
        "the eject item must be visible:\n{screen}"
    );
    assert!(
        screen.contains(common::TEST_SERIAL),
        "the device line must name the serial:\n{screen}"
    );
    assert!(
        screen.contains("KOBOeReader"),
        "the device line must name the mount:\n{screen}"
    );

    app.devices.clear();
    let screen = rendered(&app);
    assert!(
        screen.contains("No Kobo connected"),
        "an empty menu must say so:\n{screen}"
    );
    assert!(!screen.contains("Eject my Kobo"));
}

#[test]
fn home_renders_both_eject_outcomes() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());
    let calls = Arc::new(Mutex::new(Vec::new()));

    app.set_ejector(recording_ejector(calls.clone(), false, false));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    let screen = rendered(&app);
    assert!(
        screen.contains("✗ Could not eject"),
        "a failed eject must be visible:\n{screen}"
    );
    assert!(screen.contains("dissenter"), "with the reason:\n{screen}");
    assert!(screen.contains("still in use"), "and the hint:\n{screen}");

    app.set_ejector(recording_ejector(calls.clone(), true, true));
    key(&mut app, KeyCode::Enter);
    let screen = rendered(&app);
    assert!(
        screen.contains("✓ Ejected"),
        "a successful eject must be visible:\n{screen}"
    );
    assert!(screen.contains("safe to unplug"));
    assert!(screen.contains("No Kobo connected"));
}

#[test]
fn sync_report_advertises_the_eject_key() {
    let (guard, mount) = stage_fake_device();
    std::env::set_var("KOBO_BACKUP_CONF_EDIT_DIR", guard.path().join("conf-edits"));
    let out = tempfile::tempdir().unwrap();
    let mut app = app_for(&mount, out.path());

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    type_str(&mut app, "https://omni.example.com/kobo/tok123");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::SyncEndpointReport));

    let screen = rendered(&app);
    assert!(
        screen.contains("press e"),
        "the done screen must say how to eject:\n{screen}"
    );
    assert!(
        screen.contains("eject device"),
        "and advertise it in the footer:\n{screen}"
    );
}
