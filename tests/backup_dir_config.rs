//! First-run backup-location chooser and the --out / env / config precedence,
//! driven through the real App state machine.
//!
//! Every test injects a temp config path via `App::with_config`, so none of
//! them can read or write the real ~/.config/kobo-backup/config.toml.

mod common;

use std::path::{Path, PathBuf};

use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use kobo_backup::app::{App, Screen};
use kobo_backup::config::{self, Config, Source};
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

/// An App with nothing configured yet — the true first-run state.
fn unconfigured_app(mount: &Path, config_path: &Path) -> App {
    let (tx, _rx) = unbounded();
    let args = CliArgs {
        device: Some(mount.to_path_buf()),
        out_dir: None,
    };
    let resolution = config::resolve_with(None, None, Config::default(), config_path.to_path_buf());
    assert_eq!(resolution.source, Source::Unset);
    App::with_config(&args, tx, resolution, config_path.to_path_buf())
}

#[test]
fn first_backup_asks_where_to_put_backups_before_touching_anything() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config/config.toml");
    let mut app = unconfigured_app(&mount, &config_path);

    // Choosing "Back up my Kobo" must divert to the chooser, not to detection.
    assert!(matches!(app.screen, Screen::Home { selected: 0 }));
    key(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.screen, Screen::ChooseBackupDir { .. }),
        "an unconfigured first run must ask before doing anything, got {:?}",
        app.screen
    );
    assert!(
        !config_path.exists(),
        "nothing written until the user picks"
    );

    // Pick "Somewhere else…" and type a path.
    let chosen = tmp.path().join("MyBackups");
    let custom_index = App::backup_dir_choices().len();
    for _ in 0..custom_index {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter); // enter text-entry mode
    type_str(&mut app, chosen.to_str().unwrap());
    key(&mut app, KeyCode::Enter);

    // Choice persisted, directory created, and the pending flow resumed.
    assert!(
        config_path.is_file(),
        "choice must be saved to the config file"
    );
    assert!(chosen.is_dir(), "chosen directory must be created");
    assert_eq!(app.out_dir, chosen);
    assert_eq!(app.out_dir_source, Source::ConfigFile(config_path.clone()));
    assert!(
        matches!(app.screen, Screen::Detect { .. }),
        "after choosing, the backup flow should continue, got {:?}",
        app.screen
    );

    // And the saved file round-trips.
    assert_eq!(config::load_from(&config_path).backup_dir, Some(chosen));
}

#[test]
fn preset_choice_is_saved_and_restore_flow_resumes() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let mut app = unconfigured_app(&mount, &config_path);

    key(&mut app, KeyCode::Down); // "Restore my Kobo"
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::ChooseBackupDir { .. }));

    key(&mut app, KeyCode::Enter); // accept the first preset
    let expected = App::backup_dir_choices()[0].1.clone();
    assert_eq!(app.out_dir, expected);
    assert!(
        matches!(app.screen, Screen::PickZip { .. }),
        "restore flow should resume after choosing, got {:?}",
        app.screen
    );
    assert_eq!(config::load_from(&config_path).backup_dir, Some(expected));
}

#[test]
fn configured_app_never_prompts() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let configured = tmp.path().join("Configured");
    std::fs::create_dir_all(&configured).unwrap();

    let (tx, _rx) = unbounded();
    let args = CliArgs {
        device: Some(mount.clone()),
        out_dir: None,
    };
    let resolution = config::resolve_with(
        None,
        None,
        Config {
            backup_dir: Some(configured.clone()),
        },
        config_path.clone(),
    );
    let mut app = App::with_config(&args, tx, resolution, config_path);

    assert!(!app.out_dir_source.needs_prompt());
    key(&mut app, KeyCode::Enter); // Back up
    assert!(
        matches!(app.screen, Screen::Detect { .. }),
        "an already-configured run must go straight to the wizard"
    );
    assert_eq!(app.out_dir, configured);
}

#[test]
fn out_flag_overrides_the_config_file_without_persisting() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let from_config = tmp.path().join("FromConfig");
    let from_flag = tmp.path().join("FromFlag");
    config::save_backup_dir_to(&config_path, &from_config).unwrap();

    let (tx, _rx) = unbounded();
    let args = CliArgs {
        device: Some(mount),
        out_dir: Some(from_flag.clone()),
    };
    let resolution = config::resolve_with(
        args.out_dir.clone(),
        None,
        config::load_from(&config_path),
        config_path.clone(),
    );
    let app = App::with_config(&args, tx, resolution, config_path.clone());

    assert_eq!(
        app.out_dir, from_flag,
        "--out must win over the config file"
    );
    assert_eq!(app.out_dir_source, Source::CommandLine);
    // The override is for this run only — the file on disk is untouched.
    assert_eq!(
        config::load_from(&config_path).backup_dir,
        Some(from_config),
        "--out must not rewrite the saved preference"
    );
}

#[test]
fn c_from_home_changes_location_and_returns_home() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let original = tmp.path().join("Original");
    let (tx, _rx) = unbounded();
    let args = CliArgs {
        device: Some(mount),
        out_dir: None,
    };
    let resolution = config::resolve_with(
        None,
        None,
        Config {
            backup_dir: Some(original),
        },
        config_path.clone(),
    );
    let mut app = App::with_config(&args, tx, resolution, config_path.clone());

    key(&mut app, KeyCode::Char('c'));
    assert!(matches!(app.screen, Screen::ChooseBackupDir { .. }));

    let chosen = tmp.path().join("Moved");
    let custom_index = App::backup_dir_choices().len();
    for _ in 0..custom_index {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter);
    type_str(&mut app, chosen.to_str().unwrap());
    key(&mut app, KeyCode::Enter);

    assert_eq!(app.out_dir, chosen);
    assert!(
        matches!(app.screen, Screen::Home { .. }),
        "changing the folder outside a flow returns to the menu"
    );
    assert_eq!(config::load_from(&config_path).backup_dir, Some(chosen));
}

#[test]
fn unusable_path_is_rejected_with_an_error_not_a_crash() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let mut app = unconfigured_app(&mount, &config_path);

    key(&mut app, KeyCode::Enter); // Back up -> chooser
    let custom_index = App::backup_dir_choices().len();
    for _ in 0..custom_index {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter);

    // A file, not a directory — create_dir_all must fail.
    let blocker = tmp.path().join("a-file");
    std::fs::write(&blocker, b"not a directory").unwrap();
    type_str(&mut app, &format!("{}/nested", blocker.display()));
    key(&mut app, KeyCode::Enter);

    match &app.screen {
        Screen::ChooseBackupDir { error, .. } => {
            assert!(error.is_some(), "a bad path must surface an error");
        }
        other => panic!("expected to stay on the chooser, got {other:?}"),
    }
    assert!(!config_path.exists(), "a rejected path must not be saved");
}

#[test]
fn escaping_the_first_run_chooser_writes_nothing() {
    let (_guard, mount) = stage_fake_device();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let mut app = unconfigured_app(&mount, &config_path);

    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.screen, Screen::ChooseBackupDir { .. }));
    key(&mut app, KeyCode::Esc);

    assert!(matches!(app.screen, Screen::Home { .. }));
    assert!(!config_path.exists());
    assert!(app.out_dir_source.needs_prompt(), "still unconfigured");
}

#[test]
fn choices_include_cwd_and_home() {
    let choices: Vec<PathBuf> = App::backup_dir_choices()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    assert!(choices.contains(&std::env::current_dir().unwrap()));
    assert!(choices.contains(&config::fallback_dir()));
}
