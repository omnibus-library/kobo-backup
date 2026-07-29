//! Drive the configure-sync wizard through the real App state machine with
//! synthetic key events. Asserts the conf edit lands, the cancel paths leave
//! the device untouched, and validation blocks bad URLs.

mod common;

use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use kobo_backup::app::{App, Screen};
use kobo_backup::event::Event;
use kobo_backup::sync_endpoint;
use kobo_backup::CliArgs;

use common::stage_fake_device;

const URL: &str = "https://omni.example.com/kobo/tok123";

fn key(app: &mut App, code: KeyCode) {
    app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

/// App parked on the endpoint-entry screen for the staged fake device, with
/// the safety-copy root redirected into the temp dir.
fn app_at_entry(mount: &std::path::Path, tmp: &std::path::Path) -> App {
    std::env::set_var("KOBO_BACKUP_CONF_EDIT_DIR", tmp.join("conf-edits"));
    let (tx, _rx) = unbounded();
    let args = CliArgs {
        device: Some(mount.to_path_buf()),
        out_dir: Some(tmp.join("backups")),
    };
    let mut app = App::new(&args, tx);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter); // Configure wireless sync
    assert!(matches!(app.screen, Screen::Detect { .. }));
    key(&mut app, KeyCode::Enter); // select device
    assert!(matches!(app.screen, Screen::SyncEndpointEntry { .. }));
    app
}

fn conf_on_device(mount: &std::path::Path) -> String {
    std::fs::read_to_string(mount.join(sync_endpoint::CONF_RELATIVE)).unwrap()
}

#[test]
fn configure_sync_edits_the_conf_and_keeps_a_safety_copy() {
    let (guard, mount) = stage_fake_device();
    let before = conf_on_device(&mount);
    let mut app = app_at_entry(&mount, guard.path());

    // The fake device's conf has no api_endpoint yet.
    assert_eq!(app.sync_current, None);

    type_str(&mut app, URL);
    key(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.screen, Screen::SyncEndpointConfirm { apply: false }),
        "Cancel must be the default"
    );

    // Enter-mash safety: Enter on the default goes BACK, not forward.
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::SyncEndpointEntry { .. }));
    assert_eq!(
        conf_on_device(&mount),
        before,
        "cancel must leave the device untouched"
    );

    // The typed URL survives the round trip; confirm for real this time.
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::SyncEndpointReport));

    let outcome = app.sync_outcome.as_ref().expect("outcome after apply");
    assert_eq!(outcome.old, None);
    assert_eq!(outcome.new, URL);
    let copy = outcome.safety_copy.as_ref().expect("safety copy");
    assert_eq!(
        std::fs::read_to_string(copy).unwrap(),
        before,
        "safety copy must be the verbatim pre-edit conf"
    );

    let after = conf_on_device(&mount);
    assert_eq!(sync_endpoint::read_endpoint(&after), Some(URL.to_string()));
    assert!(
        after.contains("ExportHighlights=true"),
        "unrelated settings must survive"
    );

    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::Home { .. }));
}

#[test]
fn configure_sync_rejects_an_invalid_url_inline() {
    let (guard, mount) = stage_fake_device();
    let before = conf_on_device(&mount);
    let mut app = app_at_entry(&mount, guard.path());

    type_str(&mut app, "omni.example.com/kobo/tok");
    key(&mut app, KeyCode::Enter);
    match &app.screen {
        Screen::SyncEndpointEntry { error, .. } => {
            assert!(error.as_deref().unwrap_or("").contains("https://"));
        }
        other => panic!("expected entry screen with error, got {other:?}"),
    }
    assert_eq!(conf_on_device(&mount), before);
}

#[test]
fn configure_sync_esc_backs_out_without_writing() {
    let (guard, mount) = stage_fake_device();
    let before = conf_on_device(&mount);
    let mut app = app_at_entry(&mount, guard.path());

    type_str(&mut app, URL);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc); // confirm → entry
    assert!(matches!(app.screen, Screen::SyncEndpointEntry { .. }));
    key(&mut app, KeyCode::Esc); // entry → detect
    assert!(matches!(app.screen, Screen::Detect { .. }));
    assert_eq!(conf_on_device(&mount), before);
}
