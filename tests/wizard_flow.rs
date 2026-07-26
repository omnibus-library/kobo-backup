//! Drive the real App state machine through both wizards with synthetic key
//! events — no terminal involved. Asserts the safety gates actually gate.

mod common;

use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use kobo_backup::app::{App, Flow, Screen, RESTORE_PHRASE};
use kobo_backup::event::Event;
use kobo_backup::CliArgs;

use common::stage_fake_device;

fn key(app: &mut App, code: KeyCode) {
    app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

/// Process worker events until `done` says the app reached the target state.
fn pump_until(app: &mut App, rx: &Receiver<Event>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(app) {
        if Instant::now() > deadline {
            panic!(
                "timed out pumping events while waiting for: {what} (screen: {:?})",
                app.screen
            );
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(event) => app.handle(event),
            Err(_) => panic!(
                "no worker events while waiting for: {what} (screen: {:?})",
                app.screen
            ),
        }
    }
}

fn test_app(mount: &std::path::Path, out_dir: &std::path::Path) -> (App, Receiver<Event>) {
    let (tx, rx) = unbounded();
    let args = CliArgs {
        device: Some(mount.to_path_buf()),
        out_dir: Some(out_dir.to_path_buf()),
    };
    (App::new(&args, tx), rx)
}

#[test]
fn full_wizard_backup_then_restore() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let (mut app, rx) = test_app(&mount, out.path());

    // ---------- BACKUP ----------
    assert!(matches!(app.screen, Screen::Home { selected: 0 }));
    key(&mut app, KeyCode::Enter); // Back up my Kobo
    assert!(matches!(app.screen, Screen::Detect { .. }));
    assert!(!app.devices.is_empty(), "manual --device must be listed");

    key(&mut app, KeyCode::Enter); // select device
    assert!(matches!(app.screen, Screen::DeviceInfo { proceed: false }));

    // Enter-mash safety: Enter on the default (Abort) goes BACK, not forward.
    key(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.screen, Screen::Detect { .. }),
        "default action must be the safe one"
    );
    key(&mut app, KeyCode::Enter); // re-select device
    key(&mut app, KeyCode::Tab); // switch to Proceed
    key(&mut app, KeyCode::Enter); // start scan
    assert!(app.worker_running());
    pump_until(&mut app, &rx, "scan summary", |a| {
        matches!(a.screen, Screen::ScanSummary { .. })
    });

    let insights = app.device_insights.as_ref().expect("insights must load");
    assert_eq!(insights.annotations_total, Some(3));
    assert!(app.inventory.as_ref().unwrap().file_count() > 0);

    key(&mut app, KeyCode::Tab); // Proceed
    key(&mut app, KeyCode::Enter); // write backup
    pump_until(&mut app, &rx, "backup report", |a| {
        matches!(a.screen, Screen::BackupReport { .. })
    });
    assert!(app.backup_report.as_ref().unwrap().passed());
    let zip = app.final_zip.clone().expect("promoted zip path");
    assert!(zip.exists(), "verified zip must exist at final path");

    key(&mut app, KeyCode::Enter); // home
    assert!(matches!(app.screen, Screen::Home { .. }));

    // ---------- mutate device (simulate later use) ----------
    std::fs::write(mount.join("stray.txt"), b"added after backup").unwrap();
    common::add_annotation(&mount);

    // ---------- RESTORE ----------
    key(&mut app, KeyCode::Down); // Restore my Kobo
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::PickZip { .. }));
    assert_eq!(app.zip_candidates.len(), 1, "backup zip must be listed");

    key(&mut app, KeyCode::Enter); // validate it
    pump_until(&mut app, &rx, "zip summary", |a| {
        matches!(a.screen, Screen::ZipSummary { .. })
    });
    assert_eq!(
        app.backup_insights.as_ref().unwrap().annotations_total,
        Some(3),
        "insights must come from the BACKUP db (3), not the device (4)"
    );

    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter); // to device detection
    assert!(matches!(app.screen, Screen::Detect { .. }));
    assert_eq!(app.flow, Flow::Restore);

    key(&mut app, KeyCode::Enter); // pick device; serial matches → diff
    pump_until(&mut app, &rx, "diff preview", |a| {
        matches!(a.screen, Screen::DiffPreview { .. })
    });
    let plan = app.plan.as_ref().unwrap();
    assert!(plan.delete.iter().any(|f| f.path == "stray.txt"));
    assert!(plan
        .overwrite
        .iter()
        .any(|f| f.path == ".kobo/KoboReader.sqlite"));

    key(&mut app, KeyCode::Right); // Proceed
    key(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            app.screen,
            Screen::DeleteConsent {
                delete_selected: true
            }
        ),
        "deletes exist → consent screen with point-in-time default"
    );
    key(&mut app, KeyCode::Enter); // accept deletion pass
    assert!(matches!(app.screen, Screen::TypedConfirm { .. }));

    // Wrong phrase must not start anything.
    type_str(&mut app, "YES");
    key(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.screen, Screen::TypedConfirm { .. }) && !app.worker_running(),
        "wrong phrase must be inert"
    );
    for _ in 0..3 {
        key(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, RESTORE_PHRASE);
    key(&mut app, KeyCode::Enter); // micro-backup → apply → verify chain
    pump_until(&mut app, &rx, "restore report", |a| {
        matches!(a.screen, Screen::RestoreReport { .. })
    });

    let report = app.restore_report.as_ref().unwrap();
    assert!(
        report.passed(),
        "restore verify failed: {:?}",
        report.checks
    );
    assert!(app.micro_backup_dir.as_ref().unwrap().exists());
    assert!(!mount.join("stray.txt").exists(), "stray file deleted");
}

#[test]
fn serial_mismatch_requires_typed_override() {
    let (_guard, mount) = stage_fake_device();
    let out = tempfile::tempdir().unwrap();
    let (mut app, rx) = test_app(&mount, out.path());

    // Make a backup first.
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    pump_until(&mut app, &rx, "scan summary", |a| {
        matches!(a.screen, Screen::ScanSummary { .. })
    });
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    pump_until(&mut app, &rx, "backup report", |a| {
        matches!(a.screen, Screen::BackupReport { .. })
    });
    key(&mut app, KeyCode::Enter);

    // Change the device serial (simulates a different Kobo).
    std::fs::write(
        mount.join(".kobo/version"),
        "N999OTHERDEVICE,3.0.35+,4.41.23145,3.0.35+,3.0.35,00000000-0000-0000-0000-000000000376",
    )
    .unwrap();

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter); // restore
    key(&mut app, KeyCode::Enter); // pick zip
    pump_until(&mut app, &rx, "zip summary", |a| {
        matches!(a.screen, Screen::ZipSummary { .. })
    });
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter); // pick the (now different-serial) device

    assert!(
        matches!(app.screen, Screen::SerialGate { .. }),
        "mismatched serial must hit the gate, got {:?}",
        app.screen
    );

    // Enter without the phrase is inert.
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::SerialGate { .. }) && !app.worker_running());

    type_str(&mut app, "different device"); // typed input is upcased by the app
    key(&mut app, KeyCode::Enter);
    pump_until(&mut app, &rx, "diff preview after override", |a| {
        matches!(a.screen, Screen::DiffPreview { .. })
    });
}

#[test]
fn abort_during_progress_is_clean() {
    let (_guard, mount) = stage_fake_device();
    // Enough data that the scan is still running while the abort keys land.
    let bulk = mount.join("bulk");
    std::fs::create_dir_all(&bulk).unwrap();
    for i in 0..128 {
        std::fs::write(
            bulk.join(format!("book-{i:03}.epub")),
            vec![i as u8; 512 * 1024],
        )
        .unwrap();
    }
    let out = tempfile::tempdir().unwrap();
    let (mut app, rx) = test_app(&mount, out.path());

    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter); // scan starts
    assert!(app.worker_running());

    key(&mut app, KeyCode::Esc); // abort overlay
    assert!(matches!(
        app.screen,
        Screen::Progress {
            abort_overlay: true,
            ..
        }
    ));
    key(&mut app, KeyCode::Tab); // select Abort
    key(&mut app, KeyCode::Enter); // confirm
    pump_until(&mut app, &rx, "abort acknowledged", |a| {
        matches!(a.screen, Screen::Error { .. })
    });
    if let Screen::Error { title, .. } = &app.screen {
        assert_eq!(title, "Aborted");
    }
}
