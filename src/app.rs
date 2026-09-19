use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::archive;
use crate::config;
use crate::device::{self, Device};
use crate::eject::{self, EjectOutcome};
use crate::event::{Event, Events};
use crate::insights::{self, LibraryInsights};
use crate::inventory::{self, Inventory};
use crate::manifest::Manifest;
use crate::progress::{CancelToken, ProgressUpdate};
use crate::restore::{self, ApplyOptions, ApplyReport, DevicePlan};
use crate::sync_endpoint;
use crate::verify::{self, VerifyReport};
use crate::worker::{self, JobOutput, WorkerHandle, WorkerMsg};
use crate::CliArgs;

pub const SERIAL_OVERRIDE_PHRASE: &str = "DIFFERENT DEVICE";
pub const RESTORE_PHRASE: &str = "RESTORE";

/// How the app ejects a device; boxed so tests can swap in a recorder.
pub type Ejector = Box<dyn Fn(&std::path::Path) -> EjectOutcome>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Backup,
    Restore,
    /// Point the device's wireless sync at a self-hosted server by rewriting
    /// `api_endpoint` in `Kobo eReader.conf`.
    ConfigureSync,
}

/// A row on the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeItem {
    Backup,
    Restore,
    ConfigureSync,
    Eject,
    Quit,
}

impl HomeItem {
    pub fn label(self) -> &'static str {
        match self {
            HomeItem::Backup => "Back up my Kobo",
            HomeItem::Restore => "Restore my Kobo",
            HomeItem::ConfigureSync => "Configure wireless sync",
            HomeItem::Eject => "Eject my Kobo",
            HomeItem::Quit => "Quit",
        }
    }
}

/// How often Home looks for a device appearing or vanishing.
const HOME_RESCAN_INTERVAL: Duration = Duration::from_secs(1);

/// Which screen is on. Payload-light: shared data lives on `App`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Home {
        selected: usize,
    },
    /// First run (or `c` from Home) — choose where backups are kept.
    ChooseBackupDir {
        selected: usize,
        custom: Option<String>,
        error: Option<String>,
    },
    /// B1 / R4 — pick the Kobo.
    Detect {
        devices_shown: usize, // cache marker so we can tell rescans apart
        selected: usize,
        manual: Option<String>,
        error: Option<String>,
    },
    /// B2 — everything we know about the device before touching anything.
    DeviceInfo {
        proceed: bool,
    },
    /// Long-running worker phases (B3/B5/B6, R2/R6/R10/R11/R12).
    Progress {
        title: String,
        abort_overlay: bool,
        abort_selected: bool,
    },
    /// B4 — scan summary + library insights.
    ScanSummary {
        proceed: bool,
        scroll: u16,
    },
    /// B7 — final backup report.
    BackupReport {
        scroll: u16,
    },
    /// R1 — choose the backup zip.
    PickZip {
        selected: usize,
        manual: Option<String>,
        error: Option<String>,
    },
    /// R3 — what's inside this backup (manifest + insights).
    ZipSummary {
        proceed: bool,
        scroll: u16,
    },
    /// R5 — serial mismatch gate.
    SerialGate {
        typed: String,
    },
    /// R7 — full preview of what restore will do.
    DiffPreview {
        proceed: bool,
        section: usize,
        scroll: u16,
    },
    /// R8 — separate consent for the deletion pass.
    DeleteConsent {
        delete_selected: bool,
    },
    /// R9 — type RESTORE.
    TypedConfirm {
        typed: String,
    },
    /// R13 — final restore report.
    RestoreReport {
        scroll: u16,
    },
    /// S2 — paste the new sync endpoint URL.
    SyncEndpointEntry {
        input: String,
        error: Option<String>,
    },
    /// S3 — before/after preview of the conf edit; Cancel is the default.
    SyncEndpointConfirm {
        apply: bool,
    },
    /// S4 — what changed and where the pre-edit conf was saved.
    SyncEndpointReport,
    Error {
        title: String,
        message: String,
    },
    ConfirmQuit,
}

pub struct App {
    pub screen: Screen,
    pub flow: Flow,
    pub should_quit: bool,
    /// Effective backup directory (see `config::resolve` for precedence).
    pub out_dir: PathBuf,
    /// Where `out_dir` came from — shown in the UI so it is never a mystery.
    pub out_dir_source: config::Source,
    /// Flow the user picked before being asked to choose a backup location.
    pending_flow: Option<Flow>,
    /// Path the chosen backup location is persisted to.
    pub config_path: PathBuf,
    pub manual_device: Option<PathBuf>,
    ejector: Ejector,
    /// Outcome of the most recent eject. Cleared on `go_home` and on remount.
    pub last_eject: Option<EjectOutcome>,

    // Detection results (kept out of Screen so rescans are cheap).
    pub devices: Vec<Device>,
    pub stale_partials: Vec<PathBuf>,
    last_device_scan: Option<Instant>,

    // Worker state.
    worker: Option<WorkerHandle>,
    tx: Sender<Event>,
    pub progress: ProgressUpdate,
    pub progress_started: Option<Instant>,

    // Backup flow data.
    pub device: Option<Device>,
    pub inventory: Option<Inventory>,
    pub device_insights: Option<LibraryInsights>,
    pub final_zip: Option<PathBuf>,
    pub partial_path: Option<PathBuf>,
    pub manifest: Option<Manifest>,
    pub backup_report: Option<VerifyReport>,

    // Restore flow data.
    pub zip_candidates: Vec<PathBuf>,
    pub zip_path: Option<PathBuf>,
    pub backup_insights: Option<LibraryInsights>,
    pub plan: Option<DevicePlan>,
    pub delete_extras: bool,
    pub micro_backup_dir: Option<PathBuf>,
    pub apply_report: Option<ApplyReport>,
    pub restore_report: Option<VerifyReport>,

    // Configure-sync flow data.
    /// The `api_endpoint` currently in the device's conf, read on entry.
    pub sync_current: Option<String>,
    /// The validated URL waiting for confirmation.
    pub sync_candidate: Option<String>,
    pub sync_outcome: Option<sync_endpoint::Outcome>,
}

impl App {
    pub fn new(args: &CliArgs, tx: Sender<Event>) -> Self {
        let resolution = config::resolve(args.out_dir.clone());
        Self::build(args, tx, resolution, config::config_file())
    }

    /// Construct with an explicit resolution and config path. Tests use this
    /// so they can exercise the first-run chooser without reading or writing
    /// the real `~/.config/kobo-backup/config.toml`.
    pub fn with_config(
        args: &CliArgs,
        tx: Sender<Event>,
        resolution: config::Resolution,
        config_path: PathBuf,
    ) -> Self {
        Self::build(args, tx, resolution, config_path)
    }

    fn build(
        args: &CliArgs,
        tx: Sender<Event>,
        resolution: config::Resolution,
        config_path: PathBuf,
    ) -> Self {
        let stale_partials = find_stale_partials(&resolution.dir);
        let mut app = App {
            config_path,
            screen: Screen::Home { selected: 0 },
            flow: Flow::Backup,
            should_quit: false,
            out_dir: resolution.dir,
            out_dir_source: resolution.source,
            pending_flow: None,
            manual_device: args.device.clone(),
            ejector: Box::new(eject::run_eject),
            last_eject: None,
            devices: Vec::new(),
            stale_partials,
            last_device_scan: None,
            worker: None,
            tx,
            progress: ProgressUpdate::default(),
            progress_started: None,
            device: None,
            inventory: None,
            device_insights: None,
            final_zip: None,
            partial_path: None,
            manifest: None,
            backup_report: None,
            zip_candidates: Vec::new(),
            zip_path: None,
            backup_insights: None,
            plan: None,
            delete_extras: true,
            micro_backup_dir: None,
            apply_report: None,
            restore_report: None,
            sync_current: None,
            sync_candidate: None,
            sync_outcome: None,
        };
        app.refresh_devices();
        app
    }

    pub fn worker_running(&self) -> bool {
        self.worker.is_some()
    }

    /// Replace the eject implementation (tests inject a recorder).
    pub fn set_ejector(&mut self, ejector: Ejector) {
        self.ejector = ejector;
    }

    /// Rescan Home, or reconcile a stale eject line on a report screen.
    fn on_tick(&mut self) {
        match self.screen {
            Screen::Home { .. } => {
                if self.rescan_due() {
                    self.refresh_devices();
                }
            }
            Screen::BackupReport { .. }
            | Screen::RestoreReport { .. }
            | Screen::SyncEndpointReport
                if self.last_eject.is_some() && self.rescan_due() =>
            {
                self.reconcile_report_eject();
            }
            _ => {}
        }
    }

    fn rescan_due(&self) -> bool {
        match self.last_device_scan {
            Some(at) => at.elapsed() >= HOME_RESCAN_INTERVAL,
            None => true,
        }
    }

    /// Clears `last_eject` once presence matches it: remounted after success, gone after failure.
    fn reconcile_eject_outcome(&mut self, present: bool) {
        if let Some(outcome) = &self.last_eject {
            if outcome.ok == present {
                self.last_eject = None;
            }
        }
    }

    fn reconcile_report_eject(&mut self) {
        self.last_device_scan = Some(Instant::now());
        let present = self
            .last_eject
            .as_ref()
            .map(|outcome| outcome.mount.join(".kobo").is_dir());
        if let Some(present) = present {
            self.reconcile_eject_outcome(present);
        }
    }

    /// Refresh `devices` for Home. A `--device` is probed directly and never
    /// falls back to scanning, so a dead named device is never swapped for a different Kobo.
    fn refresh_devices(&mut self) {
        let items_before = self.home_items().len();
        self.devices = match &self.manual_device {
            Some(m) => device::probe(m).ok().into_iter().collect(),
            None => device::scan(),
        };
        self.last_device_scan = Some(Instant::now());
        let present = self
            .last_eject
            .as_ref()
            .map(|outcome| self.devices.iter().any(|d| d.mount == outcome.mount));
        if let Some(present) = present {
            self.reconcile_eject_outcome(present);
        }
        if self.home_items().len() < items_before {
            self.clamp_home_selection(items_before);
        }
    }

    /// Keeps the cursor off Quit after a shrink, unless it was parked there already.
    fn clamp_home_selection(&mut self, items_before: usize) {
        if let Screen::Home { selected } = self.screen {
            let len = self.home_items().len();
            if selected == items_before.saturating_sub(1) {
                self.screen = Screen::Home {
                    selected: len.saturating_sub(1),
                };
            } else if selected + 1 >= len {
                self.screen = Screen::Home {
                    selected: len.saturating_sub(2),
                };
            }
        }
    }

    /// The main menu, in order. Eject only appears while a Kobo is mounted.
    pub fn home_items(&self) -> Vec<HomeItem> {
        let mut items = vec![HomeItem::Backup, HomeItem::Restore, HomeItem::ConfigureSync];
        if !self.devices.is_empty() {
            items.push(HomeItem::Eject);
        }
        items.push(HomeItem::Quit);
        items
    }

    // ---------------------------------------------------------------- events

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Worker(msg) => self.handle_worker(msg),
            Event::Tick => self.on_tick(),
            Event::Resize => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl-C always asks (or quits instantly if nothing is running).
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.worker_running() {
                self.screen = Screen::ConfirmQuit;
            } else {
                self.should_quit = true;
            }
            return;
        }

        match self.screen.clone() {
            Screen::Home { selected } => self.key_home(key, selected),
            Screen::ChooseBackupDir {
                selected,
                custom,
                error,
            } => self.key_choose_backup_dir(key, selected, custom, error),
            Screen::Detect {
                selected, manual, ..
            } => self.key_detect(key, selected, manual),
            Screen::DeviceInfo { proceed } => self.key_device_info(key, proceed),
            Screen::Progress {
                title,
                abort_overlay,
                abort_selected,
            } => self.key_progress(key, title, abort_overlay, abort_selected),
            Screen::ScanSummary { proceed, scroll } => self.key_scan_summary(key, proceed, scroll),
            Screen::BackupReport { scroll } => self.key_backup_report(key, scroll),
            Screen::PickZip {
                selected, manual, ..
            } => self.key_pick_zip(key, selected, manual),
            Screen::ZipSummary { proceed, scroll } => self.key_zip_summary(key, proceed, scroll),
            Screen::SerialGate { typed } => self.key_serial_gate(key, typed),
            Screen::DiffPreview {
                proceed,
                section,
                scroll,
            } => self.key_diff_preview(key, proceed, section, scroll),
            Screen::DeleteConsent { delete_selected } => {
                self.key_delete_consent(key, delete_selected)
            }
            Screen::TypedConfirm { typed } => self.key_typed_confirm(key, typed),
            Screen::RestoreReport { scroll } => self.key_restore_report(key, scroll),
            Screen::SyncEndpointEntry { input, .. } => self.key_sync_entry(key, input),
            Screen::SyncEndpointConfirm { apply } => self.key_sync_confirm(key, apply),
            Screen::SyncEndpointReport => self.key_sync_report(key),
            Screen::Error { .. } => self.key_error(key),
            Screen::ConfirmQuit => self.key_confirm_quit(key),
        }
    }

    // ------------------------------------------------------------- per screen

    fn key_home(&mut self, key: KeyEvent, selected: usize) {
        let items = self.home_items();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.screen = Screen::Home {
                    selected: selected.saturating_sub(1),
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.screen = Screen::Home {
                    selected: (selected + 1).min(items.len() - 1),
                }
            }
            KeyCode::Char('d') if !self.stale_partials.is_empty() => {
                for p in &self.stale_partials {
                    let _ = std::fs::remove_file(p);
                }
                self.stale_partials.clear();
            }
            KeyCode::Char('c') => self.enter_choose_backup_dir(None),
            // A stale index (device unplugged since the last rescan) does nothing rather than guess.
            KeyCode::Enter => {
                let Some(item) = items.get(selected).copied() else {
                    return;
                };
                match item {
                    HomeItem::Backup => self.start_flow(Flow::Backup),
                    HomeItem::Restore => self.start_flow(Flow::Restore),
                    HomeItem::ConfigureSync => self.start_flow(Flow::ConfigureSync),
                    HomeItem::Eject => {
                        let items_before = items.len();
                        self.eject_device();
                        self.clamp_home_selection(items_before);
                    }
                    HomeItem::Quit => self.should_quit = true,
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            _ => {}
        }
    }

    /// Begin a wizard — but if no backup location has ever been chosen, ask
    /// first. Nothing should ever be written to a directory the user did not
    /// pick.
    fn start_flow(&mut self, flow: Flow) {
        self.reset_flow_data();
        self.flow = flow;
        // The endpoint flow writes nothing to the backups folder, so it never
        // needs the first-run location prompt.
        if flow != Flow::ConfigureSync && self.out_dir_source.needs_prompt() {
            self.enter_choose_backup_dir(Some(flow));
            return;
        }
        match flow {
            Flow::Backup | Flow::ConfigureSync => self.enter_detect(),
            Flow::Restore => self.enter_pick_zip(),
        }
    }

    fn enter_choose_backup_dir(&mut self, pending: Option<Flow>) {
        self.pending_flow = pending;
        self.screen = Screen::ChooseBackupDir {
            selected: 0,
            custom: None,
            error: None,
        };
    }

    /// The presets offered on the chooser: (label, path).
    pub fn backup_dir_choices() -> Vec<(String, PathBuf)> {
        let mut choices = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            choices.push(("Current directory".to_string(), cwd));
        }
        choices.push(("Home folder".to_string(), config::fallback_dir()));
        choices
    }

    fn commit_backup_dir(&mut self, dir: PathBuf) {
        // Prove it is usable before recording it, so a bad choice fails here
        // rather than after a long scan.
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.screen = Screen::ChooseBackupDir {
                selected: 0,
                custom: Some(dir.display().to_string()),
                error: Some(format!("cannot use that directory: {err}")),
            };
            return;
        }
        match config::save_backup_dir_to(&self.config_path, &dir) {
            Ok(()) => {
                self.out_dir = dir;
                self.out_dir_source = config::Source::ConfigFile(self.config_path.clone());
                self.stale_partials = find_stale_partials(&self.out_dir);
                match self.pending_flow.take() {
                    Some(Flow::Backup | Flow::ConfigureSync) => self.enter_detect(),
                    Some(Flow::Restore) => self.enter_pick_zip(),
                    None => self.go_home(),
                }
            }
            Err(err) => self.fail("Could not save your choice", format!("{err:#}")),
        }
    }

    fn key_choose_backup_dir(
        &mut self,
        key: KeyEvent,
        selected: usize,
        custom: Option<String>,
        error: Option<String>,
    ) {
        let choices = Self::backup_dir_choices();
        let custom_index = choices.len();

        if let Some(mut input) = custom {
            match key.code {
                KeyCode::Esc => {
                    self.screen = Screen::ChooseBackupDir {
                        selected: custom_index,
                        custom: None,
                        error: None,
                    }
                }
                KeyCode::Enter => {
                    let trimmed = input.trim();
                    if trimmed.is_empty() {
                        self.screen = Screen::ChooseBackupDir {
                            selected: custom_index,
                            custom: Some(input),
                            error: Some("enter a path, or press Esc to go back".into()),
                        };
                    } else {
                        let expanded = config::expand_user_path(trimmed);
                        self.commit_backup_dir(expanded);
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.screen = Screen::ChooseBackupDir {
                        selected: custom_index,
                        custom: Some(input),
                        error: None,
                    };
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.screen = Screen::ChooseBackupDir {
                        selected: custom_index,
                        custom: Some(input),
                        error: None,
                    };
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.pending_flow = None;
                self.go_home();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.screen = Screen::ChooseBackupDir {
                    selected: selected.saturating_sub(1),
                    custom: None,
                    error,
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.screen = Screen::ChooseBackupDir {
                    selected: (selected + 1).min(custom_index),
                    custom: None,
                    error,
                }
            }
            KeyCode::Enter => {
                if selected == custom_index {
                    self.screen = Screen::ChooseBackupDir {
                        selected,
                        custom: Some(String::new()),
                        error: None,
                    };
                } else if let Some((_, dir)) = choices.get(selected).cloned() {
                    self.commit_backup_dir(dir);
                }
            }
            _ => {}
        }
    }

    fn enter_detect(&mut self) {
        self.devices = device::scan();
        if let Some(manual) = &self.manual_device {
            if let Ok(d) = device::probe(manual) {
                if !self.devices.iter().any(|x| x.mount == d.mount) {
                    self.devices.insert(0, d);
                }
            }
        }
        self.screen = Screen::Detect {
            devices_shown: self.devices.len(),
            selected: 0,
            manual: None,
            error: None,
        };
    }

    fn key_detect(&mut self, key: KeyEvent, selected: usize, manual: Option<String>) {
        // Manual path entry mode.
        if let Some(mut input) = manual {
            match key.code {
                KeyCode::Esc => {
                    self.screen = Screen::Detect {
                        devices_shown: self.devices.len(),
                        selected,
                        manual: None,
                        error: None,
                    }
                }
                KeyCode::Enter => match device::probe(std::path::Path::new(input.trim())) {
                    Ok(d) => {
                        self.devices.insert(0, d);
                        self.select_device(0);
                    }
                    Err(err) => {
                        self.screen = Screen::Detect {
                            devices_shown: self.devices.len(),
                            selected,
                            manual: Some(input),
                            error: Some(format!("{err:#}")),
                        }
                    }
                },
                KeyCode::Backspace => {
                    input.pop();
                    self.screen = Screen::Detect {
                        devices_shown: self.devices.len(),
                        selected,
                        manual: Some(input),
                        error: None,
                    };
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.screen = Screen::Detect {
                        devices_shown: self.devices.len(),
                        selected,
                        manual: Some(input),
                        error: None,
                    };
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            KeyCode::Char('r') => self.enter_detect(),
            KeyCode::Char('m') => {
                self.screen = Screen::Detect {
                    devices_shown: self.devices.len(),
                    selected,
                    manual: Some(String::new()),
                    error: None,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.screen = Screen::Detect {
                    devices_shown: self.devices.len(),
                    selected: selected.saturating_sub(1),
                    manual: None,
                    error: None,
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.devices.len().saturating_sub(1);
                self.screen = Screen::Detect {
                    devices_shown: self.devices.len(),
                    selected: (selected + 1).min(max),
                    manual: None,
                    error: None,
                }
            }
            KeyCode::Enter if !self.devices.is_empty() => self.select_device(selected),
            _ => {}
        }
    }

    fn select_device(&mut self, index: usize) {
        let Some(device) = self.devices.get(index).cloned() else {
            return;
        };
        self.device = Some(device);
        match self.flow {
            Flow::Backup => self.screen = Screen::DeviceInfo { proceed: false },
            Flow::Restore => self.check_serial(),
            Flow::ConfigureSync => self.enter_sync_entry(),
        }
    }

    // ------------------------------------------------- configure-sync screens

    fn enter_sync_entry(&mut self) {
        let Some(device) = &self.device else {
            return;
        };
        let conf = std::fs::read_to_string(device.mount.join(sync_endpoint::CONF_RELATIVE))
            .unwrap_or_default();
        self.sync_current = sync_endpoint::read_endpoint(&conf);
        self.screen = Screen::SyncEndpointEntry {
            input: self.sync_candidate.clone().unwrap_or_default(),
            error: None,
        };
    }

    fn key_sync_entry(&mut self, key: KeyEvent, mut input: String) {
        match key.code {
            // No `q`-to-quit here — the URL being typed can contain one.
            KeyCode::Esc => self.enter_detect(),
            KeyCode::Enter => match sync_endpoint::validate_url(&input) {
                Ok(url) => {
                    self.sync_candidate = Some(url);
                    self.screen = Screen::SyncEndpointConfirm { apply: false };
                }
                Err(msg) => {
                    self.screen = Screen::SyncEndpointEntry {
                        input,
                        error: Some(msg),
                    }
                }
            },
            KeyCode::Backspace => {
                input.pop();
                self.screen = Screen::SyncEndpointEntry { input, error: None };
            }
            KeyCode::Char(c) => {
                input.push(c);
                self.screen = Screen::SyncEndpointEntry { input, error: None };
            }
            _ => {}
        }
    }

    fn key_sync_confirm(&mut self, key: KeyEvent, apply: bool) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.back_to_sync_entry(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.screen = Screen::SyncEndpointConfirm { apply: !apply }
            }
            KeyCode::Enter if apply => self.apply_sync_endpoint(),
            KeyCode::Enter => self.back_to_sync_entry(),
            _ => {}
        }
    }

    fn back_to_sync_entry(&mut self) {
        self.screen = Screen::SyncEndpointEntry {
            input: self.sync_candidate.clone().unwrap_or_default(),
            error: None,
        };
    }

    /// The conf write is a few hundred bytes — no worker, applied inline.
    fn apply_sync_endpoint(&mut self) {
        let (Some(device), Some(url)) = (self.device.clone(), self.sync_candidate.clone()) else {
            return;
        };
        match sync_endpoint::apply(&device, &url, &conf_edit_root()) {
            Ok(outcome) => {
                self.sync_outcome = Some(outcome);
                self.screen = Screen::SyncEndpointReport;
            }
            Err(err) => self.fail("Could not update the sync endpoint", format!("{err:#}")),
        }
    }

    fn key_sync_report(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('e') => self.eject_device(),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            _ => {}
        }
    }

    fn key_device_info(&mut self, key: KeyEvent, proceed: bool) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.enter_detect(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.screen = Screen::DeviceInfo { proceed: !proceed }
            }
            KeyCode::Enter if proceed => self.start_scan_job(),
            KeyCode::Enter => self.enter_detect(),
            _ => {}
        }
    }

    fn key_progress(
        &mut self,
        key: KeyEvent,
        title: String,
        abort_overlay: bool,
        abort_selected: bool,
    ) {
        if abort_overlay {
            match key.code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    self.screen = Screen::Progress {
                        title,
                        abort_overlay: true,
                        abort_selected: !abort_selected,
                    }
                }
                KeyCode::Enter => {
                    if abort_selected {
                        if let Some(worker) = &self.worker {
                            worker.cancel.cancel();
                        }
                        self.screen = Screen::Progress {
                            title,
                            abort_overlay: false,
                            abort_selected: false,
                        };
                    } else {
                        self.screen = Screen::Progress {
                            title,
                            abort_overlay: false,
                            abort_selected: false,
                        };
                    }
                }
                KeyCode::Esc => {
                    self.screen = Screen::Progress {
                        title,
                        abort_overlay: false,
                        abort_selected: false,
                    }
                }
                _ => {}
            }
        } else if key.code == KeyCode::Esc {
            self.screen = Screen::Progress {
                title,
                abort_overlay: true,
                abort_selected: false,
            };
        }
    }

    fn key_scan_summary(&mut self, key: KeyEvent, proceed: bool, scroll: u16) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.screen = Screen::ScanSummary {
                    proceed: !proceed,
                    scroll,
                }
            }
            KeyCode::Up => {
                self.screen = Screen::ScanSummary {
                    proceed,
                    scroll: scroll.saturating_sub(1),
                }
            }
            KeyCode::Down => {
                self.screen = Screen::ScanSummary {
                    proceed,
                    scroll: scroll + 1,
                }
            }
            KeyCode::Enter if proceed => self.start_write_job(),
            KeyCode::Enter => self.go_home(),
            _ => {}
        }
    }

    fn key_backup_report(&mut self, key: KeyEvent, scroll: u16) {
        match key.code {
            KeyCode::Up => {
                self.screen = Screen::BackupReport {
                    scroll: scroll.saturating_sub(1),
                }
            }
            KeyCode::Down => self.screen = Screen::BackupReport { scroll: scroll + 1 },
            KeyCode::Char('e') => self.eject_device(),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            _ => {}
        }
    }

    fn enter_pick_zip(&mut self) {
        self.zip_candidates = archive::read::list_backup_zips(&self.out_dir);
        self.screen = Screen::PickZip {
            selected: 0,
            manual: None,
            error: None,
        };
    }

    fn key_pick_zip(&mut self, key: KeyEvent, selected: usize, manual: Option<String>) {
        if let Some(mut input) = manual {
            match key.code {
                KeyCode::Esc => {
                    self.screen = Screen::PickZip {
                        selected,
                        manual: None,
                        error: None,
                    }
                }
                KeyCode::Enter => {
                    let path = PathBuf::from(input.trim());
                    if path.is_file() {
                        self.start_validate_job(path);
                    } else {
                        self.screen = Screen::PickZip {
                            selected,
                            manual: Some(input),
                            error: Some("file does not exist".into()),
                        };
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.screen = Screen::PickZip {
                        selected,
                        manual: Some(input),
                        error: None,
                    };
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.screen = Screen::PickZip {
                        selected,
                        manual: Some(input),
                        error: None,
                    };
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            KeyCode::Char('m') => {
                self.screen = Screen::PickZip {
                    selected,
                    manual: Some(String::new()),
                    error: None,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.screen = Screen::PickZip {
                    selected: selected.saturating_sub(1),
                    manual: None,
                    error: None,
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.zip_candidates.len().saturating_sub(1);
                self.screen = Screen::PickZip {
                    selected: (selected + 1).min(max),
                    manual: None,
                    error: None,
                }
            }
            KeyCode::Enter => {
                if let Some(path) = self.zip_candidates.get(selected).cloned() {
                    self.start_validate_job(path);
                }
            }
            _ => {}
        }
    }

    fn key_zip_summary(&mut self, key: KeyEvent, proceed: bool, scroll: u16) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.enter_pick_zip(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.screen = Screen::ZipSummary {
                    proceed: !proceed,
                    scroll,
                }
            }
            KeyCode::Up => {
                self.screen = Screen::ZipSummary {
                    proceed,
                    scroll: scroll.saturating_sub(1),
                }
            }
            KeyCode::Down => {
                self.screen = Screen::ZipSummary {
                    proceed,
                    scroll: scroll + 1,
                }
            }
            KeyCode::Enter if proceed => self.enter_detect(),
            KeyCode::Enter => self.enter_pick_zip(),
            _ => {}
        }
    }

    fn check_serial(&mut self) {
        let (Some(device), Some(manifest)) = (&self.device, &self.manifest) else {
            return;
        };
        if device.identity.serial == manifest.device.serial {
            self.start_diff_job();
        } else {
            self.screen = Screen::SerialGate {
                typed: String::new(),
            };
        }
    }

    fn key_serial_gate(&mut self, key: KeyEvent, mut typed: String) {
        match key.code {
            KeyCode::Esc => self.go_home(),
            KeyCode::Enter if typed == SERIAL_OVERRIDE_PHRASE => self.start_diff_job(),
            KeyCode::Backspace => {
                typed.pop();
                self.screen = Screen::SerialGate { typed };
            }
            KeyCode::Char(c) => {
                typed.push(c.to_ascii_uppercase());
                self.screen = Screen::SerialGate { typed };
            }
            _ => {}
        }
    }

    fn key_diff_preview(&mut self, key: KeyEvent, proceed: bool, section: usize, scroll: u16) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            KeyCode::Tab => {
                self.screen = Screen::DiffPreview {
                    proceed,
                    section: (section + 1) % 4,
                    scroll: 0,
                }
            }
            KeyCode::Left | KeyCode::Right => {
                self.screen = Screen::DiffPreview {
                    proceed: !proceed,
                    section,
                    scroll,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.screen = Screen::DiffPreview {
                    proceed,
                    section,
                    scroll: scroll.saturating_sub(1),
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.screen = Screen::DiffPreview {
                    proceed,
                    section,
                    scroll: scroll + 1,
                }
            }
            KeyCode::Enter if proceed => {
                let has_deletes = self
                    .plan
                    .as_ref()
                    .is_some_and(|p| !p.delete.is_empty() || !p.dirs_to_delete.is_empty());
                if has_deletes {
                    self.screen = Screen::DeleteConsent {
                        delete_selected: true,
                    };
                } else {
                    self.delete_extras = false;
                    self.enter_typed_confirm();
                }
            }
            KeyCode::Enter => self.go_home(),
            _ => {}
        }
    }

    fn key_delete_consent(&mut self, key: KeyEvent, delete_selected: bool) {
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::DiffPreview {
                    proceed: false,
                    section: 0,
                    scroll: 0,
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.screen = Screen::DeleteConsent {
                    delete_selected: !delete_selected,
                }
            }
            KeyCode::Enter => {
                self.delete_extras = delete_selected;
                self.enter_typed_confirm();
            }
            _ => {}
        }
    }

    fn enter_typed_confirm(&mut self) {
        // Free-space guard: deletions run last, so all writes must fit first.
        if let (Some(device), Some(plan)) = (&self.device, &self.plan) {
            if let Ok((_, free)) = crate::util::disk_space(&device.mount) {
                if free < plan.bytes_to_write {
                    self.fail(
                        "Not enough space on the device",
                        format!(
                            "The restore needs to write {} but the Kobo only has {} free. \
                             Deletions run last (for safety), so the writes must fit first. \
                             Free some space on the device and try again.",
                            crate::util::bytes(plan.bytes_to_write),
                            crate::util::bytes(free),
                        ),
                    );
                    return;
                }
            }
        }
        self.screen = Screen::TypedConfirm {
            typed: String::new(),
        };
    }

    fn key_typed_confirm(&mut self, key: KeyEvent, mut typed: String) {
        match key.code {
            KeyCode::Esc => self.go_home(),
            KeyCode::Enter if typed == RESTORE_PHRASE => self.start_micro_backup_job(),
            KeyCode::Backspace => {
                typed.pop();
                self.screen = Screen::TypedConfirm { typed };
            }
            KeyCode::Char(c) => {
                typed.push(c.to_ascii_uppercase());
                self.screen = Screen::TypedConfirm { typed };
            }
            _ => {}
        }
    }

    fn key_restore_report(&mut self, key: KeyEvent, scroll: u16) {
        match key.code {
            KeyCode::Up => {
                self.screen = Screen::RestoreReport {
                    scroll: scroll.saturating_sub(1),
                }
            }
            KeyCode::Down => self.screen = Screen::RestoreReport { scroll: scroll + 1 },
            KeyCode::Char('e') => self.eject_device(),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => self.go_home(),
            _ => {}
        }
    }

    fn key_error(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
            self.go_home();
        }
    }

    fn key_confirm_quit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                if let Some(worker) = &self.worker {
                    worker.cancel.cancel();
                }
                self.should_quit = true;
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                // Return to a sensible screen: progress if running, else home.
                if self.worker_running() {
                    self.screen = Screen::Progress {
                        title: self.progress.phase.clone(),
                        abort_overlay: false,
                        abort_selected: false,
                    };
                } else {
                    self.go_home();
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- worker msgs

    fn handle_worker(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Progress(update) => {
                self.progress = update;
            }
            WorkerMsg::Cancelled => {
                self.worker = None;
                self.cleanup_after_abort();
                self.screen = Screen::Error {
                    title: "Aborted".into(),
                    message: self.abort_explanation(),
                };
            }
            WorkerMsg::Failed(message) => {
                self.worker = None;
                self.cleanup_after_abort();
                self.screen = Screen::Error {
                    title: "Operation failed".into(),
                    message,
                };
            }
            WorkerMsg::Done(output) => {
                self.worker = None;
                self.on_job_done(*output);
            }
        }
    }

    fn on_job_done(&mut self, output: JobOutput) {
        match output {
            JobOutput::Scanned {
                inventory,
                insights,
            } => {
                self.inventory = Some(inventory);
                self.device_insights = insights;
                self.screen = Screen::ScanSummary {
                    proceed: false,
                    scroll: 0,
                };
            }
            JobOutput::ZipWritten {
                partial_path,
                manifest,
            } => {
                self.partial_path = Some(partial_path);
                self.manifest = Some(manifest);
                self.start_backup_verify_job();
            }
            JobOutput::BackupVerified { report, final_path } => {
                if let Some(path) = &final_path {
                    self.final_zip = Some(path.clone());
                    self.partial_path = None;
                }
                let passed = report.passed();
                self.backup_report = Some(report);
                self.screen = Screen::BackupReport { scroll: 0 };
                if !passed {
                    // keep .partial for forensics; report screen explains.
                }
            }
            JobOutput::ZipValidated {
                zip_path,
                manifest,
                insights,
            } => {
                self.zip_path = Some(zip_path);
                self.manifest = Some(manifest);
                self.backup_insights = insights;
                self.screen = Screen::ZipSummary {
                    proceed: false,
                    scroll: 0,
                };
            }
            JobOutput::Diffed {
                plan,
                device_insights,
            } => {
                self.plan = Some(plan);
                self.device_insights = device_insights;
                self.screen = Screen::DiffPreview {
                    proceed: false,
                    section: 0,
                    scroll: 0,
                };
            }
            JobOutput::MicroBackedUp { dir } => {
                self.micro_backup_dir = Some(dir);
                self.start_apply_job();
            }
            JobOutput::Applied { report } => {
                self.apply_report = Some(report);
                self.start_restore_verify_job();
            }
            JobOutput::RestoreVerified { report } => {
                self.restore_report = Some(report);
                self.screen = Screen::RestoreReport { scroll: 0 };
            }
        }
    }

    // ------------------------------------------------------------------ jobs

    fn spawn_job<F>(&mut self, title: &str, job: F)
    where
        F: FnOnce(&worker::Reporter, &CancelToken) -> Result<JobOutput> + Send + 'static,
    {
        self.progress = ProgressUpdate {
            phase: title.to_string(),
            ..Default::default()
        };
        self.progress_started = Some(Instant::now());
        self.worker = Some(worker::spawn(self.tx.clone(), CancelToken::new(), job));
        self.screen = Screen::Progress {
            title: title.to_string(),
            abort_overlay: false,
            abort_selected: false,
        };
    }

    fn start_scan_job(&mut self) {
        let Some(device) = self.device.clone() else {
            return;
        };
        self.spawn_job("Scanning device (read-only)", move |reporter, cancel| {
            let inventory = inventory::scan(&device.mount, &|u| reporter.report(u), cancel)?;
            reporter.report_now(ProgressUpdate {
                phase: "Reading library facts from a copy of the database".into(),
                files_done: inventory.file_count(),
                files_total: inventory.file_count(),
                bytes_done: inventory.total_bytes,
                bytes_total: inventory.total_bytes,
                ..Default::default()
            });
            let insights = insights::copy_db_to_temp(&device.mount.join(".kobo"))
                .ok()
                .and_then(|(_guard, db)| insights::gather(&db).ok());
            Ok(JobOutput::Scanned {
                inventory,
                insights,
            })
        });
    }

    fn start_write_job(&mut self) {
        let (Some(device), Some(inventory)) = (self.device.clone(), self.inventory.clone()) else {
            return;
        };

        // Host free-space guard before any write.
        if let Err(message) = check_host_space(&self.out_dir, inventory.total_bytes) {
            self.fail("Not enough space for the backup", message);
            return;
        }

        let final_zip = self.out_dir.join(format!(
            "kobo-backup-{}-{}.zip",
            device.identity.serial,
            crate::util::filename_timestamp()
        ));
        self.final_zip = Some(final_zip.clone());
        self.spawn_job("Writing backup zip", move |reporter, cancel| {
            let outcome = archive::write::write_backup(
                &device,
                &inventory,
                &final_zip,
                &|u| reporter.report(u),
                cancel,
            )?;
            Ok(JobOutput::ZipWritten {
                partial_path: outcome.partial_path,
                manifest: outcome.manifest,
            })
        });
    }

    fn start_backup_verify_job(&mut self) {
        let (Some(device), Some(partial), Some(final_zip), Some(mut manifest)) = (
            self.device.clone(),
            self.partial_path.clone(),
            self.final_zip.clone(),
            self.manifest.clone(),
        ) else {
            return;
        };
        self.spawn_job(
            "Verifying backup (full re-read)",
            move |reporter, cancel| {
                let (report, promoted) = verify::verify_backup(
                    &partial,
                    &final_zip,
                    &mut manifest,
                    &device,
                    &|u| reporter.report(u),
                    cancel,
                )?;
                Ok(JobOutput::BackupVerified {
                    report,
                    final_path: promoted,
                })
            },
        );
    }

    fn start_validate_job(&mut self, zip_path: PathBuf) {
        self.spawn_job("Validating backup file", move |reporter, cancel| {
            let manifest = archive::read::validate(&zip_path, &|u| reporter.report(u), cancel)?;
            reporter.report_now(ProgressUpdate {
                phase: "Reading library facts from the backup's database".into(),
                ..Default::default()
            });
            let insights = (|| -> Result<LibraryInsights> {
                let mut archive = archive::read::open_archive(&zip_path)?;
                let extracted = archive::read::extract_db_to_temp(&mut archive, &manifest)?;
                let (_guard, db) = extracted.context("backup contains no KoboReader.sqlite")?;
                insights::gather(&db)
            })()
            .ok();
            Ok(JobOutput::ZipValidated {
                zip_path,
                manifest,
                insights,
            })
        });
    }

    fn start_diff_job(&mut self) {
        let (Some(device), Some(manifest)) = (self.device.clone(), self.manifest.clone()) else {
            return;
        };
        self.spawn_job(
            "Scanning device & computing restore plan",
            move |reporter, cancel| {
                let device_inv = inventory::scan(&device.mount, &|u| reporter.report(u), cancel)?;
                let plan = restore::diff(&manifest, &device_inv);
                let device_insights = insights::copy_db_to_temp(&device.mount.join(".kobo"))
                    .ok()
                    .and_then(|(_guard, db)| insights::gather(&db).ok());
                Ok(JobOutput::Diffed {
                    plan,
                    device_insights,
                })
            },
        );
    }

    fn start_micro_backup_job(&mut self) {
        let Some(device) = self.device.clone() else {
            return;
        };
        let safety_root = safety_backup_root();
        self.spawn_job(
            "Safety-copying current device databases",
            move |_reporter, _cancel| {
                let dir = restore::micro_backup(&device, &safety_root)?;
                Ok(JobOutput::MicroBackedUp { dir })
            },
        );
    }

    fn start_apply_job(&mut self) {
        let (Some(device), Some(plan), Some(zip_path)) = (
            self.device.clone(),
            self.plan.clone(),
            self.zip_path.clone(),
        ) else {
            return;
        };
        let options = ApplyOptions {
            delete_extras: self.delete_extras,
        };
        self.spawn_job("Restoring files to device", move |reporter, cancel| {
            let report = restore::apply(
                &zip_path,
                &device,
                &plan,
                options,
                &|u| reporter.report(u),
                cancel,
            )?;
            Ok(JobOutput::Applied { report })
        });
    }

    fn start_restore_verify_job(&mut self) {
        let (Some(device), Some(manifest)) = (self.device.clone(), self.manifest.clone()) else {
            return;
        };
        let deletions = self.delete_extras;
        self.spawn_job(
            "Verifying device against backup (full re-read)",
            move |reporter, cancel| {
                let report = verify::verify_restore(
                    &device,
                    &manifest,
                    deletions,
                    &|u| reporter.report(u),
                    cancel,
                )?;
                Ok(JobOutput::RestoreVerified { report })
            },
        );
    }

    // ----------------------------------------------------------------- helpers

    fn go_home(&mut self) {
        self.stale_partials = find_stale_partials(&self.out_dir);
        self.last_eject = None;
        self.refresh_devices();
        self.screen = Screen::Home { selected: 0 };
    }

    fn fail(&mut self, title: &str, message: String) {
        self.screen = Screen::Error {
            title: title.to_string(),
            message,
        };
    }

    fn reset_flow_data(&mut self) {
        self.device = None;
        self.inventory = None;
        self.device_insights = None;
        self.final_zip = None;
        self.partial_path = None;
        self.manifest = None;
        self.backup_report = None;
        self.zip_path = None;
        self.backup_insights = None;
        self.plan = None;
        self.delete_extras = true;
        self.micro_backup_dir = None;
        self.apply_report = None;
        self.restore_report = None;
        self.sync_current = None;
        self.sync_candidate = None;
        self.sync_outcome = None;
        self.last_eject = None;
        self.progress = ProgressUpdate::default();
        self.progress_started = None;
    }

    fn cleanup_after_abort(&mut self) {
        // A dead .partial file is safe to remove — it was never promoted.
        if let Some(partial) = self.partial_path.take() {
            let _ = std::fs::remove_file(&partial);
        }
        if let Some(final_zip) = &self.final_zip {
            let partial = archive::write::partial_path_for(final_zip);
            let _ = std::fs::remove_file(partial);
        }
    }

    fn abort_explanation(&self) -> String {
        match self.flow {
            Flow::Backup => "Backup aborted. Your Kobo was only ever read — nothing on it \
                             was touched. The partial zip file has been deleted."
                .to_string(),
            Flow::Restore => {
                if self.apply_report.is_some() || matches!(self.screen, Screen::Progress { .. }) {
                    format!(
                        "Restore aborted. Files already restored are complete (each one was \
                         hash-verified before being moved into place) — none are truncated. \
                         Deletions had not started. The device may hold a mix of old and new \
                         files; run the restore again to finish, or find the pre-restore \
                         database safety copy in {}.",
                        self.micro_backup_dir
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| safety_backup_root().display().to_string())
                    )
                } else {
                    "Aborted before anything was written to the device.".to_string()
                }
            }
            // The endpoint flow runs no worker jobs; an abort can only happen
            // before the (inline, atomic) conf write.
            Flow::ConfigureSync => "Aborted. The sync endpoint was not changed.".to_string(),
        }
    }

    fn eject_target(&self) -> Option<PathBuf> {
        match self.screen {
            Screen::Home { .. } => self.devices.first().map(|d| d.mount.clone()),
            _ => self.device.as_ref().map(|d| d.mount.clone()),
        }
    }

    fn eject_device(&mut self) {
        let Some(mount) = self.eject_target() else {
            return;
        };
        let outcome = (self.ejector)(&mount);
        if outcome.ok {
            if self.device.as_ref().is_some_and(|d| d.mount == mount) {
                self.device = None;
            }
            self.devices.retain(|d| d.mount != mount);
        }
        self.last_eject = Some(outcome);
    }
}

fn safety_backup_root() -> PathBuf {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".kobo-backup").join("pre-restore"))
        .unwrap_or_else(|_| PathBuf::from(".kobo-backup-pre-restore"))
}

/// Where pre-edit copies of `Kobo eReader.conf` are kept, mirroring
/// `safety_backup_root`. `KOBO_BACKUP_CONF_EDIT_DIR` overrides it (tests use
/// this to stay out of the real home directory).
fn conf_edit_root() -> PathBuf {
    if let Ok(dir) = std::env::var("KOBO_BACKUP_CONF_EDIT_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".kobo-backup").join("conf-edits"))
        .unwrap_or_else(|_| PathBuf::from(".kobo-backup-conf-edits"))
}

fn find_stale_partials(out_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.to_string_lossy().ends_with(".zip.partial") {
                found.push(path);
            }
        }
    }
    found
}

fn check_host_space(out_dir: &std::path::Path, needed: u64) -> Result<(), String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("cannot create backup directory {}: {e}", out_dir.display()))?;
    let (_, free) = crate::util::disk_space(out_dir)
        .map_err(|e| format!("cannot determine free space: {e:#}"))?;
    let padded = needed + needed / 20; // 1.05x
    if free < padded {
        return Err(format!(
            "the backup needs about {} (plus 5% headroom) but {} only has {} free",
            crate::util::bytes(needed),
            out_dir.display(),
            crate::util::bytes(free),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ run loop

pub fn run(args: CliArgs) -> Result<()> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use crossterm::ExecutableCommand;

    // Restore the terminal on panic — standard ratatui hygiene.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = std::io::stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));

    enable_raw_mode().context("cannot enable raw terminal mode")?;
    std::io::stdout()
        .execute(EnterAlternateScreen)
        .context("cannot enter alternate screen")?;
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;

    let events = Events::new();
    let mut app = App::new(&args, events.worker_sender());

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| crate::ui::draw(frame, &app))?;
            app.handle(events.next());
            if app.should_quit {
                return Ok(());
            }
        }
    })();

    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    result
}
