//! One render function per wizard screen. Screens are pure: they read the
//! App, they never mutate it.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Flow, RESTORE_PHRASE, SERIAL_OVERRIDE_PHRASE};
use crate::util;

use super::widgets::{self, ACCENT, DANGER, DIM, OK, WARN};

fn kv(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<26}"), Style::default().fg(DIM)),
        Span::styled(value, Style::default().fg(Color::White).bold()),
    ])
}

fn boxed(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, scroll: u16) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
        .title(format!(" {title} "));
    f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
}

// --------------------------------------------------------------------- home

pub fn home(f: &mut Frame, app: &App, selected: usize) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "KOBO BACKUP", None);

    let body = widgets::padded(chrome.body, 4);
    let rows = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(body);

    let intro = Paragraph::new(vec![
        Line::raw(""),
        Line::from("Point-in-time backup & restore for your Kobo e-reader.".bold()),
        Line::raw(""),
        Line::from(Span::styled(
            "Every step shows exactly what will happen and asks before doing it. \
             Backups are verified by re-reading every byte; restores are verified \
             the same way.",
            Style::default().fg(Color::Gray),
        )),
    ])
    .wrap(Wrap { trim: true });
    f.render_widget(intro, rows[0]);

    let items = [
        "Back up my Kobo",
        "Restore my Kobo",
        "Configure wireless sync",
        "Quit",
    ];
    for (i, item) in items.iter().enumerate() {
        let style = if i == selected {
            Style::default().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::default().fg(Color::White)
        };
        let prefix = if i == selected { " ▸ " } else { "   " };
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                format!("{prefix}{item}  "),
                style,
            )])),
            rows[2 + i],
        );
    }

    let mut notes = vec![Line::from(vec![
        Span::styled("Backups folder: ", Style::default().fg(DIM)),
        Span::styled(
            app.out_dir.display().to_string(),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(
            format!("  ({}) ", app.out_dir_source.describe()),
            Style::default().fg(DIM),
        ),
        Span::styled("— press c to change", Style::default().fg(DIM)),
    ])];
    if !app.stale_partials.is_empty() {
        notes.push(Line::from(Span::styled(
            format!(
                "⚠ {} stale .partial file(s) from an interrupted backup found — press d to delete \
                 them (they were never verified and cannot be restored from)",
                app.stale_partials.len()
            ),
            Style::default().fg(WARN),
        )));
    }
    f.render_widget(Paragraph::new(notes).wrap(Wrap { trim: true }), rows[6]);

    widgets::safety_line(
        f,
        chrome.safety,
        "Nothing happens without your explicit confirmation.",
        OK,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[
            ("↑↓", "choose"),
            ("Enter", "select"),
            ("c", "backups folder"),
            ("q", "quit"),
        ],
    );
}

// -------------------------------------------------- choose backup directory

pub fn choose_backup_dir(
    f: &mut Frame,
    app: &App,
    selected: usize,
    custom: &Option<String>,
    error: &Option<String>,
) {
    let chrome = widgets::chrome(f);
    let first_run = app.out_dir_source.needs_prompt();
    widgets::title_bar(
        f,
        chrome.title,
        if first_run {
            "WHERE SHOULD BACKUPS BE KEPT?"
        } else {
            "CHANGE BACKUPS FOLDER"
        },
        None,
    );
    let body = widgets::padded(chrome.body, 4);

    let choices = App::backup_dir_choices();
    let custom_index = choices.len();

    let mut lines: Vec<Line> = vec![Line::raw("")];
    if first_run {
        lines.push(Line::from(
            "Backups are large — often more than a gigabyte each — so pick somewhere \
             deliberate rather than letting them land wherever you happened to run this.",
        ));
    }
    lines.push(Line::from(
        "The restore wizard looks for backups in this same folder, so keeping them in \
         one place is what makes them findable later."
            .fg(DIM),
    ));
    lines.push(Line::raw(""));

    for (i, (label, path)) in choices.iter().enumerate() {
        let is_selected = i == selected && custom.is_none();
        let style = if is_selected {
            Style::default().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("{} {label} ", if is_selected { " ▸" } else { "  " }),
            style,
        )));
        lines.push(Line::from(Span::styled(
            format!("     {}", path.display()),
            Style::default().fg(DIM),
        )));
    }

    let custom_selected = selected == custom_index || custom.is_some();
    let style = if custom_selected {
        Style::default().fg(Color::Black).bg(ACCENT).bold()
    } else {
        Style::default().fg(Color::White)
    };
    lines.push(Line::from(Span::styled(
        format!(
            "{} Somewhere else… ",
            if custom_selected { " ▸" } else { "  " }
        ),
        style,
    )));

    if let Some(input) = custom {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "Type a path (Enter to use it, Esc to go back):".bold(),
        ));
        lines.push(Line::from(Span::styled(
            format!("  {input}▏"),
            Style::default().fg(ACCENT),
        )));
        lines.push(Line::from(
            "  ~ is expanded; the folder is created if needed.".fg(DIM),
        ));
    }

    if let Some(err) = error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(DANGER),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(
        format!(
            "Your choice is saved to {} and can be changed any time with c from the \
             main menu. A single run can always override it with --out <dir> or the \
             {} environment variable.",
            app.config_path.display(),
            crate::config::ENV_VAR
        )
        .fg(DIM),
    ));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    widgets::safety_line(
        f,
        chrome.safety,
        "Nothing is written anywhere until you choose.",
        OK,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("↑↓", "choose"), ("Enter", "select"), ("Esc", "back")],
    );
}

// ------------------------------------------------------------------- detect

pub fn detect(
    f: &mut Frame,
    app: &App,
    selected: usize,
    manual: &Option<String>,
    error: &Option<String>,
) {
    let chrome = widgets::chrome(f);
    let (title, step) = match app.flow {
        Flow::Backup => ("BACKUP — FIND YOUR KOBO", Some((1, 4))),
        Flow::Restore => ("RESTORE — FIND YOUR KOBO", Some((3, 7))),
        Flow::ConfigureSync => ("CONFIGURE SYNC — FIND YOUR KOBO", Some((1, 3))),
    };
    widgets::title_bar(f, chrome.title, title, step);
    let body = widgets::padded(chrome.body, 4);

    let mut lines: Vec<Line> = Vec::new();
    if app.devices.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "No Kobo detected. Connect your Kobo via USB and tap “Connect” on its screen, \
             then press r to rescan."
                .fg(WARN),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "Volumes are recognized as Kobos when they contain a readable .kobo/version file."
                .fg(DIM),
        ));
    } else {
        lines.push(Line::from(format!(
            "Found {} candidate volume(s). Confirm the identity below is YOUR device:",
            app.devices.len()
        )));
        lines.push(Line::raw(""));
        for (i, device) in app.devices.iter().enumerate() {
            let marker = if i == selected { " ▸ " } else { "   " };
            let style = if i == selected {
                Style::default().fg(Color::Black).bg(ACCENT).bold()
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{marker}{}  —  {}  (serial {}, firmware {}) ",
                    device.mount.display(),
                    device.identity.model_name,
                    device.identity.serial,
                    device.identity.firmware
                ),
                style,
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "      {} used of {}",
                    util::bytes(device.volume_used()),
                    util::bytes(device.volume_total)
                ),
                Style::default().fg(DIM),
            )));
        }
    }

    if let Some(input) = manual {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "Manual mount path (Enter to probe, Esc to cancel):".bold(),
        ));
        lines.push(Line::from(Span::styled(
            format!("  {input}▏"),
            Style::default().fg(ACCENT),
        )));
    }
    if let Some(err) = error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(DANGER),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    widgets::safety_line(
        f,
        chrome.safety,
        "Detection only reads one small file (.kobo/version).",
        OK,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[
            ("↑↓", "choose"),
            ("Enter", "select"),
            ("r", "rescan"),
            ("m", "manual path"),
            ("Esc", "back"),
        ],
    );
}

// -------------------------------------------------------------- device info

pub fn device_info(f: &mut Frame, app: &App, proceed: bool) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "BACKUP — CONFIRM DEVICE", Some((2, 4)));
    let body = widgets::padded(chrome.body, 4);
    let Some(device) = &app.device else { return };

    let cols =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(body);

    let lines = vec![
        kv("Model", device.identity.model_name.clone()),
        kv("Serial", device.identity.serial.clone()),
        kv("Firmware", device.identity.firmware.clone()),
        kv("Mounted at", device.mount.display().to_string()),
        kv("Volume label", device.label.clone()),
        kv(
            "Storage",
            format!(
                "{} used of {} ({} free)",
                util::bytes(device.volume_used()),
                util::bytes(device.volume_total),
                util::bytes(device.volume_free)
            ),
        ),
        Line::raw(""),
        kv("Backups folder", app.out_dir.display().to_string()),
    ];
    boxed(f, cols[0], "Device identity", lines, 0);

    let what = vec![
        Line::from("What happens next".fg(ACCENT).bold()),
        Line::raw(""),
        Line::from("1. Every file on the volume is enumerated and"),
        Line::from("   hashed (SHA-256). This is 100% read-only."),
        Line::from("2. You review a full summary — including your"),
        Line::from("   annotation and reading data — before anything"),
        Line::from("   is written."),
        Line::from("3. Only then is the zip written, then re-read"),
        Line::from("   byte-by-byte to verify it."),
        Line::raw(""),
        Line::from("You can abort at any point.".fg(OK)),
    ];
    boxed(f, cols[1], "Plan", what, 0);

    widgets::safety_line(
        f,
        chrome.safety,
        "Your Kobo will NOT be written to at any point during a backup.",
        OK,
    );
    widgets::confirm_bar(
        f,
        chrome.actions,
        "Back",
        "Start read-only scan",
        proceed,
        false,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("←→", "choose"), ("Enter", "confirm"), ("Esc", "back")],
    );
}

// ----------------------------------------------------------------- progress

pub fn progress(f: &mut Frame, app: &App, title: &str, abort_overlay: bool, abort_selected: bool) {
    let chrome = widgets::chrome(f);
    let flow_title = match app.flow {
        Flow::Backup => format!("BACKUP — {}", title.to_uppercase()),
        Flow::Restore => format!("RESTORE — {}", title.to_uppercase()),
        // Unreachable today (the endpoint flow runs no worker jobs), but the
        // render must not panic if that ever changes.
        Flow::ConfigureSync => format!("CONFIGURE SYNC — {}", title.to_uppercase()),
    };
    widgets::title_bar(f, chrome.title, &flow_title, None);
    let body = widgets::padded(chrome.body, 6);

    let elapsed = app
        .progress_started
        .map(|s| s.elapsed())
        .unwrap_or_default();
    widgets::progress_pane(f, body, &app.progress, elapsed);

    let phase = &app.progress.phase;
    let (text, color) = if phase.contains("Restoring") || phase.contains("Deleting") {
        (
            "WRITING TO DEVICE — do not unplug. Each file is verified before it replaces anything.",
            DANGER,
        )
    } else if phase.contains("Safety-copying") {
        (
            "Copying current device databases aside before any change.",
            WARN,
        )
    } else if app.flow == Flow::Backup {
        (
            "Your Kobo is only being read. Nothing on it is being modified.",
            OK,
        )
    } else {
        (
            "Reading only — nothing has been written to the device yet.",
            OK,
        )
    };
    widgets::safety_line(f, chrome.safety, text, color);
    widgets::footer(f, chrome.footer, &[("Esc", "abort…")]);

    if abort_overlay {
        let buttons = Line::from(vec![
            Span::styled(
                "  Keep running  ",
                if !abort_selected {
                    Style::default().fg(Color::Black).bg(OK).bold()
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
            Span::raw("    "),
            Span::styled(
                "  Abort  ",
                if abort_selected {
                    Style::default().fg(Color::Black).bg(DANGER).bold()
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
        ]);
        let body_text = if app.flow == Flow::Restore && phase.contains("Restoring") {
            "Abort the restore? Files copied so far are complete and verified; the device \
             will hold a mix of old and new files until you re-run the restore."
        } else {
            "Abort this operation? Nothing incomplete will be left behind."
        };
        widgets::modal(f, "Abort?", body_text, buttons, true);
    }
}

// ------------------------------------------------------------- scan summary

pub fn scan_summary(f: &mut Frame, app: &App, proceed: bool, scroll: u16) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "BACKUP — REVIEW WHAT WAS FOUND",
        Some((3, 4)),
    );
    let body = widgets::padded(chrome.body, 2);
    let Some(inventory) = &app.inventory else {
        return;
    };

    let cols =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).split(body);

    let mut left: Vec<Line> = vec![
        kv("Files to back up", format!("{}", inventory.file_count())),
        kv("Directories", format!("{}", inventory.dirs.len())),
        kv("Total size", util::bytes(inventory.total_bytes)),
        kv(
            "Host junk excluded",
            format!(
                "{} files ({}) — .DS_Store, ._*, Spotlight…",
                inventory.excluded_count,
                util::bytes(inventory.excluded_bytes)
            ),
        ),
        Line::raw(""),
        Line::from("By category".fg(ACCENT).bold()),
    ];
    for (category, count, bytes) in inventory.category_totals() {
        left.push(kv(
            &format!("  {}", category.label()),
            format!("{count} files, {}", util::bytes(bytes)),
        ));
    }
    left.push(Line::raw(""));
    left.push(Line::from("Largest files".fg(ACCENT).bold()));
    for entry in inventory.largest(12) {
        left.push(Line::from(vec![
            Span::styled(
                format!("{:>10}  ", util::bytes(entry.size)),
                Style::default().fg(Color::White),
            ),
            Span::styled(entry.path.clone(), Style::default().fg(Color::Gray)),
        ]));
    }
    boxed(f, cols[0], "Volume contents (↑↓ to scroll)", left, scroll);

    let right: Vec<Line> = match &app.device_insights {
        Some(insights) => {
            widgets::insights_lines(insights, "Your library — what this backup protects")
        }
        None => vec![
            Line::raw(""),
            Line::from("Could not read library facts from KoboReader.sqlite.".fg(WARN)),
            Line::from("The raw database file is still backed up byte-for-byte.".fg(DIM)),
        ],
    };
    boxed(f, cols[1], "Library insights", right, 0);

    widgets::safety_line(
        f,
        chrome.safety,
        "Still read-only. Confirming below writes the zip to your computer — the Kobo itself is never modified.",
        OK,
    );
    widgets::confirm_bar(
        f,
        chrome.actions,
        "Abort",
        "Write backup zip",
        proceed,
        false,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[
            ("←→", "choose"),
            ("Enter", "confirm"),
            ("↑↓", "scroll"),
            ("Esc", "abort"),
        ],
    );
}

// ------------------------------------------------------------ backup report

pub fn backup_report(f: &mut Frame, app: &App, scroll: u16) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "BACKUP — VERIFICATION REPORT",
        Some((4, 4)),
    );
    let body = widgets::padded(chrome.body, 4);
    let Some(report) = &app.backup_report else {
        return;
    };

    let mut lines = widgets::report_lines(report);
    lines.push(Line::raw(""));
    if report.passed() {
        if let Some(path) = &app.final_zip {
            lines.push(Line::from("✓ Backup verified and saved to:".fg(OK).bold()));
            lines.push(Line::from(format!("  {}", path.display()).bold()));
            if let Ok(meta) = std::fs::metadata(path) {
                lines.push(Line::from(
                    format!("  {} on disk", util::bytes(meta.len())).fg(DIM),
                ));
            }
        }
        if let Some(manifest) = &app.manifest {
            lines.push(Line::raw(""));
            lines.push(Line::from(
                format!(
                    "This zip can restore {} ({}) to exactly this point in time.",
                    manifest.device.model_name, manifest.device.serial
                )
                .fg(Color::Gray),
            ));
        }
    } else {
        lines.push(Line::from(
            "✗ Verification FAILED — this backup is NOT trustworthy."
                .fg(DANGER)
                .bold(),
        ));
        lines.push(Line::from(
            "The unverified .partial file was kept for inspection. Your Kobo was not modified. \
             Re-run the backup; if it fails again, check the disk you are backing up to."
                .fg(Color::Gray),
        ));
    }
    boxed(f, body, "Checks", lines, scroll);

    let (text, color) = if report.passed() {
        ("Backup complete. Your Kobo was never written to.", OK)
    } else {
        (
            "Backup failed verification — do not rely on this file.",
            DANGER,
        )
    };
    widgets::safety_line(f, chrome.safety, text, color);
    widgets::footer(
        f,
        chrome.footer,
        &[("Enter", "home"), ("↑↓", "scroll"), ("e", "eject device")],
    );
}

// ----------------------------------------------------------------- pick zip

pub fn pick_zip(
    f: &mut Frame,
    app: &App,
    selected: usize,
    manual: &Option<String>,
    error: &Option<String>,
) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "RESTORE — CHOOSE A BACKUP", Some((1, 7)));
    let body = widgets::padded(chrome.body, 4);

    let mut lines: Vec<Line> = Vec::new();
    if app.zip_candidates.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            format!("No .zip backups found in {}", app.out_dir.display()).fg(WARN),
        ));
        lines.push(Line::from(
            "Press m to type a path to a backup file.".fg(DIM),
        ));
    } else {
        lines.push(Line::from(format!(
            "Backups in {} (newest first):",
            app.out_dir.display()
        )));
        lines.push(Line::raw(""));
        for (i, path) in app.zip_candidates.iter().enumerate() {
            let marker = if i == selected { " ▸ " } else { "   " };
            let style = if i == selected {
                Style::default().fg(Color::Black).bg(ACCENT).bold()
            } else {
                Style::default().fg(Color::White)
            };
            let size = std::fs::metadata(path)
                .map(|m| util::bytes(m.len()))
                .unwrap_or_default();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("{marker}{name}  ({size}) "),
                style,
            )));
        }
    }
    if let Some(input) = manual {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "Path to backup zip (Enter to load, Esc to cancel):".bold(),
        ));
        lines.push(Line::from(Span::styled(
            format!("  {input}▏"),
            Style::default().fg(ACCENT),
        )));
    }
    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(DANGER),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    widgets::safety_line(
        f,
        chrome.safety,
        "The chosen file will be fully integrity-checked before anything else happens.",
        OK,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[
            ("↑↓", "choose"),
            ("Enter", "validate"),
            ("m", "manual path"),
            ("Esc", "back"),
        ],
    );
}

// -------------------------------------------------------------- zip summary

pub fn zip_summary(f: &mut Frame, app: &App, proceed: bool, scroll: u16) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "RESTORE — WHAT THIS BACKUP CONTAINS",
        Some((2, 7)),
    );
    let body = widgets::padded(chrome.body, 2);
    let Some(manifest) = &app.manifest else {
        return;
    };

    let cols =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).split(body);

    let created_local: String = manifest
        .created_at
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M %Z")
        .to_string();
    let mut left = vec![
        kv(
            "Backup file",
            app.zip_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        ),
        kv("Created", created_local),
        kv(
            "Tool version",
            format!("{} {}", manifest.tool.name, manifest.tool.version),
        ),
        Line::raw(""),
        Line::from("Backed-up device".fg(ACCENT).bold()),
        kv("  Model", manifest.device.model_name.clone()),
        kv("  Serial", manifest.device.serial.clone()),
        kv("  Firmware", manifest.device.firmware.clone()),
        Line::raw(""),
        kv("Files", format!("{}", manifest.totals.file_count)),
        kv("Total size", util::bytes(manifest.totals.total_bytes)),
        Line::raw(""),
    ];
    match &manifest.verification {
        Some(v) if v.verified_after_write => {
            left.push(Line::from(
                "✓ This backup was fully verified when it was created.".fg(OK),
            ));
            left.push(Line::from(
                format!("  (sqlite integrity at creation: {})", v.sqlite_integrity).fg(DIM),
            ));
        }
        _ => left.push(Line::from(
            "⚠ No verification record — treat with care (it just re-passed a full hash sweep)."
                .fg(WARN),
        )),
    }
    left.push(Line::from(
        "✓ Full hash sweep of this file passed just now.".fg(OK),
    ));
    boxed(f, cols[0], "Backup details", left, scroll);

    let right: Vec<Line> = match &app.backup_insights {
        Some(insights) => {
            widgets::insights_lines(insights, "Library state you would be restoring TO")
        }
        None => vec![Line::from(
            "Could not read library facts from the backup's database.".fg(WARN),
        )],
    };
    boxed(f, cols[1], "Library inside this backup", right, 0);

    widgets::safety_line(f, chrome.safety, "Nothing has touched any device yet.", OK);
    widgets::confirm_bar(
        f,
        chrome.actions,
        "Back",
        "Choose device to restore to",
        proceed,
        false,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("←→", "choose"), ("Enter", "confirm"), ("Esc", "back")],
    );
}

// -------------------------------------------------------------- serial gate

pub fn serial_gate(f: &mut Frame, app: &App, typed: &str) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "RESTORE — DEVICE MISMATCH", Some((4, 7)));
    let body = widgets::padded(chrome.body, 6);

    let (Some(device), Some(manifest)) = (&app.device, &app.manifest) else {
        return;
    };
    let lines = vec![
        Line::raw(""),
        Line::from(
            "⚠ THIS BACKUP WAS MADE FROM A DIFFERENT KOBO"
                .fg(DANGER)
                .bold(),
        ),
        Line::raw(""),
        kv(
            "Backup came from",
            format!(
                "{} — serial {}",
                manifest.device.model_name, manifest.device.serial
            ),
        ),
        kv(
            "Connected device",
            format!(
                "{} — serial {}",
                device.identity.model_name, device.identity.serial
            ),
        ),
        Line::raw(""),
        Line::from(
            "Restoring will transplant device-bound files (device.salt.conf, DRM keys, \
             Kobo-store books). Kobo-store purchases and Adobe DRM books from the backup \
             will most likely NOT open on this device, and the device may need to be set \
             up again with your Kobo account.",
        ),
        Line::raw(""),
        Line::from(
            "This is appropriate mainly for warranty replacements or data forensics.".fg(DIM),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::raw("Type "),
            Span::styled(SERIAL_OVERRIDE_PHRASE, Style::default().fg(DANGER).bold()),
            Span::raw(" and press Enter to continue anyway, or Esc to abort:"),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            format!("  {typed}▏"),
            Style::default().fg(DANGER).bold(),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DANGER)),
        ),
        body,
    );

    widgets::safety_line(
        f,
        chrome.safety,
        "Nothing has been written. This gate exists because this action cannot be fully undone.",
        DANGER,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("Esc", "abort"), ("Enter", "submit phrase")],
    );
}

// ------------------------------------------------------------- diff preview

pub fn diff_preview(f: &mut Frame, app: &App, proceed: bool, section: usize, scroll: u16) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "RESTORE — EXACTLY WHAT WILL CHANGE",
        Some((5, 7)),
    );
    let body = widgets::padded(chrome.body, 2);
    let Some(plan) = &app.plan else { return };

    let rows = Layout::vertical([Constraint::Length(8), Constraint::Min(6)]).split(body);

    // Insights side-by-side: device now vs backup.
    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[0]);
    let now_lines: Vec<Line> = match &app.device_insights {
        Some(i) => summary_insights(i),
        None => vec![Line::from("no library facts available".fg(DIM))],
    };
    let backup_lines: Vec<Line> = match &app.backup_insights {
        Some(i) => summary_insights(i),
        None => vec![Line::from("no library facts available".fg(DIM))],
    };
    boxed(f, cols[0], "Device NOW (will be replaced)", now_lines, 0);
    boxed(f, cols[1], "Backup (device becomes this)", backup_lines, 0);

    // File-level plan with tabbed sections.
    let sections = [
        format!("Overwrite ({})", plan.overwrite.len()),
        format!("Add ({})", plan.add.len()),
        format!("DELETE ({})", plan.delete.len()),
        format!("Notes ({})", plan.case_collisions.len()),
    ];
    let mut tab_spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, label) in sections.iter().enumerate() {
        let style = if i == section {
            Style::default()
                .fg(Color::Black)
                .bg(if i == 2 { DANGER } else { ACCENT })
                .bold()
        } else {
            Style::default().fg(Color::Gray)
        };
        tab_spans.push(Span::styled(format!(" {label} "), style));
        tab_spans.push(Span::raw("  "));
    }

    let mut list: Vec<Line> = vec![Line::from(tab_spans), Line::raw("")];
    match section {
        0 => {
            list.push(Line::from(
                format!(
                    "These {} file(s) exist on the device but their content differs from the backup:",
                    plan.overwrite.len()
                )
                .fg(DIM),
            ));
            for entry in &plan.overwrite {
                list.push(Line::from(format!(
                    "  ~ {}  ({})",
                    entry.path,
                    util::bytes(entry.size)
                )));
            }
        }
        1 => {
            list.push(Line::from(
                format!(
                    "These {} file(s) are in the backup but missing from the device:",
                    plan.add.len()
                )
                .fg(DIM),
            ));
            for entry in &plan.add {
                list.push(Line::from(
                    format!("  + {}  ({})", entry.path, util::bytes(entry.size)).fg(OK),
                ));
            }
        }
        2 => {
            list.push(Line::from(
                format!(
                    "These {} file(s) are on the device but NOT in the backup — they did not \
                     exist at backup time. You will choose on the next screen whether to delete them:",
                    plan.delete.len()
                )
                .fg(DIM),
            ));
            for entry in &plan.delete {
                list.push(Line::from(
                    format!("  ✗ {}  ({})", entry.path, util::bytes(entry.size)).fg(DANGER),
                ));
            }
        }
        _ => {
            if plan.case_collisions.is_empty() {
                list.push(Line::from("No case-sensitivity notes.".fg(DIM)));
            } else {
                list.push(Line::from(
                    "Same file, different capitalization (FAT32 is case-insensitive; the backup's \
                     spelling wins):"
                        .fg(DIM),
                ));
                for (backup_path, device_path) in &plan.case_collisions {
                    list.push(Line::from(
                        format!("  {device_path} → {backup_path}").fg(WARN),
                    ));
                }
            }
            list.push(Line::raw(""));
            list.push(Line::from(
                format!(
                    "{} file(s) are already identical and will not be touched.",
                    plan.unchanged.len()
                )
                .fg(OK),
            ));
        }
    }
    boxed(
        f,
        rows[1],
        &format!(
            "Restore plan — {} to write, {} to delete (Tab switches section)",
            util::bytes(plan.bytes_to_write),
            util::bytes(plan.bytes_to_delete)
        ),
        list,
        scroll,
    );

    widgets::safety_line(
        f,
        chrome.safety,
        "Still nothing written. Two more explicit steps before any change is made.",
        WARN,
    );
    widgets::confirm_bar(f, chrome.actions, "Abort", "Continue", proceed, false);
    widgets::footer(
        f,
        chrome.footer,
        &[
            ("Tab", "section"),
            ("↑↓", "scroll"),
            ("←→", "choose"),
            ("Enter", "confirm"),
            ("Esc", "abort"),
        ],
    );
}

fn summary_insights(insights: &crate::insights::LibraryInsights) -> Vec<Line<'static>> {
    let opt = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "–".into());
    vec![
        kv("Annotations", opt(insights.annotations_total)),
        kv(
            "  newest",
            insights
                .newest_annotation
                .clone()
                .unwrap_or_else(|| "–".into()),
        ),
        kv("Books", opt(insights.total_books)),
        kv("In progress", opt(insights.books_in_progress)),
        kv(
            "Reading now",
            insights
                .current_book
                .as_ref()
                .map(|(t, p)| format!("{t} ({p}%)"))
                .unwrap_or_else(|| "–".into()),
        ),
    ]
}

// ----------------------------------------------------------- delete consent

pub fn delete_consent(f: &mut Frame, app: &App, delete_selected: bool) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "RESTORE — FILES NOT IN THE BACKUP",
        Some((6, 7)),
    );
    let body = widgets::padded(chrome.body, 6);
    let Some(plan) = &app.plan else { return };

    let option = |label: String, selected: bool, danger: bool| -> Line<'static> {
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(if danger { DANGER } else { ACCENT })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        Line::from(Span::styled(
            format!("{} {label} ", if selected { " ▸" } else { "  " }),
            style,
        ))
    };

    let lines = vec![
        Line::raw(""),
        Line::from(format!(
            "{} file(s) ({}) exist on the device now but were NOT part of the backup.",
            plan.delete.len(),
            util::bytes(plan.bytes_to_delete)
        )),
        Line::from("They appeared after the backup was taken (new books, new annotations DB state, etc.).".fg(DIM)),
        Line::raw(""),
        Line::from("A true point-in-time restore removes them. Keeping them leaves the device in a mixed state".fg(DIM)),
        Line::from("(the restored database may not know about kept books, or reference deleted state).".fg(DIM)),
        Line::raw(""),
        option(
            format!("Delete these {} file(s) — exact point-in-time restore (recommended)", plan.delete.len()),
            delete_selected,
            true,
        ),
        Line::raw(""),
        option(
            "Keep them — restore everything else around them".to_string(),
            !delete_selected,
            false,
        ),
        Line::raw(""),
        Line::from("Deletions always run LAST, only after every restored file has been verified.".fg(OK)),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    widgets::safety_line(
        f,
        chrome.safety,
        "Nothing deleted yet — this only sets the plan. One typed confirmation remains.",
        WARN,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("↑↓", "choose"), ("Enter", "confirm"), ("Esc", "back")],
    );
}

// ------------------------------------------------------------ typed confirm

pub fn typed_confirm(f: &mut Frame, app: &App, typed: &str) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "RESTORE — FINAL CONFIRMATION",
        Some((6, 7)),
    );
    let body = widgets::padded(chrome.body, 6);
    let (Some(plan), Some(device), Some(manifest)) = (&app.plan, &app.device, &app.manifest) else {
        return;
    };

    let deletes = if app.delete_extras {
        format!("{} file(s) will be deleted", plan.delete.len())
    } else {
        format!("{} extra file(s) will be KEPT", plan.delete.len())
    };
    let lines = vec![
        Line::raw(""),
        Line::from("About to modify the device:".bold()),
        Line::raw(""),
        kv(
            "Device",
            format!(
                "{} (serial {}) at {}",
                device.identity.model_name,
                device.identity.serial,
                device.mount.display()
            ),
        ),
        kv(
            "Restoring to",
            format!(
                "backup from {}",
                manifest
                    .created_at
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%Y-%m-%d %H:%M")
            ),
        ),
        kv(
            "Will write",
            format!(
                "{} file(s), {}",
                plan.files_to_copy(),
                util::bytes(plan.bytes_to_write)
            ),
        ),
        kv("Will delete", deletes),
        kv(
            "Untouched",
            format!("{} identical file(s)", plan.unchanged.len()),
        ),
        Line::raw(""),
        Line::from("Order of operations, for your safety:".fg(ACCENT)),
        Line::from("  1. Current device databases are safety-copied to your computer.".fg(DIM)),
        Line::from("  2. Every file is streamed next to its target, hash-verified,".fg(DIM)),
        Line::from("     and only then moved into place. Databases go last.".fg(DIM)),
        Line::from("  3. Deletions (if chosen) run only after all writes verified.".fg(DIM)),
        Line::from("  4. The whole device is re-read and compared to the backup.".fg(DIM)),
        Line::raw(""),
        Line::from(vec![
            Span::raw("Type "),
            Span::styled(RESTORE_PHRASE, Style::default().fg(DANGER).bold()),
            Span::raw(" and press Enter to begin, or Esc to abort:"),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            format!("  {typed}▏"),
            Style::default().fg(DANGER).bold(),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DANGER)),
        ),
        body,
    );

    widgets::safety_line(
        f,
        chrome.safety,
        "This is the last gate. After this, the device WILL be modified.",
        DANGER,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("Esc", "abort"), ("Enter", "submit phrase")],
    );
}

// ----------------------------------------------------------- restore report

pub fn restore_report(f: &mut Frame, app: &App, scroll: u16) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "RESTORE — VERIFICATION REPORT",
        Some((7, 7)),
    );
    let body = widgets::padded(chrome.body, 4);
    let Some(report) = &app.restore_report else {
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    if let Some(apply) = &app.apply_report {
        lines.push(Line::from("What was done".fg(ACCENT).bold()));
        lines.push(kv(
            "Files restored",
            format!(
                "{} ({})",
                apply.files_copied,
                util::bytes(apply.bytes_copied)
            ),
        ));
        lines.push(kv("Files deleted", format!("{}", apply.files_deleted)));
        lines.push(kv(
            "Identical, untouched",
            format!("{}", apply.unchanged_skipped),
        ));
        lines.push(kv(
            "Directories created / removed",
            format!("{} / {}", apply.dirs_created, apply.dirs_removed),
        ));
        lines.push(kv("Timestamps restored", format!("{}", apply.mtimes_set)));
        if let Some(dir) = &app.micro_backup_dir {
            lines.push(kv("Pre-restore DB safety copy", dir.display().to_string()));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(
        "Verification (full device re-read)".fg(ACCENT).bold(),
    ));
    lines.extend(widgets::report_lines(report));
    lines.push(Line::raw(""));
    if report.passed() {
        lines.push(Line::from(
            "✓ The device now matches the backup. Eject it before unplugging (press e)."
                .fg(OK)
                .bold(),
        ));
    } else {
        lines.push(Line::from(
            "✗ Verification found problems — review the checks above. The pre-restore \
             safety copy of your databases is untouched on this computer."
                .fg(DANGER)
                .bold(),
        ));
    }
    boxed(f, body, "Restore report", lines, scroll);

    let (text, color) = if report.passed() {
        ("Restore complete and verified byte-for-byte.", OK)
    } else {
        (
            "Restore verification failed — device may be in a mixed state.",
            DANGER,
        )
    };
    widgets::safety_line(f, chrome.safety, text, color);
    widgets::footer(
        f,
        chrome.footer,
        &[("Enter", "home"), ("↑↓", "scroll"), ("e", "eject device")],
    );
}

// ---------------------------------------------------------- configure sync

pub fn sync_endpoint_entry(f: &mut Frame, app: &App, input: &str, error: &Option<String>) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(
        f,
        chrome.title,
        "CONFIGURE SYNC — ENDPOINT URL",
        Some((2, 3)),
    );
    let body = widgets::padded(chrome.body, 4);
    let Some(device) = &app.device else { return };

    let mut lines = vec![
        Line::raw(""),
        kv("Device", device.identity.model_name.clone()),
        kv("Serial", device.identity.serial.clone()),
        kv(
            "Current sync endpoint",
            app.sync_current
                .clone()
                .unwrap_or_else(|| "not set (Kobo's own store)".to_string()),
        ),
        Line::raw(""),
        Line::from(
            "Point this Kobo's wireless sync at a self-hosted server (Omnibus, \
             Calibre-Web). Paste the endpoint URL your server shows — for Omnibus \
             it looks like https://your-server.example.com/kobo/<token>.",
        ),
        Line::raw(""),
        Line::from("New endpoint URL (Enter to continue, Esc to go back):".bold()),
        Line::from(Span::styled(
            format!("  {input}▏"),
            Style::default().fg(ACCENT),
        )),
    ];
    if let Some(err) = error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(DANGER),
        )));
    }
    if input.trim_start().starts_with("http://") {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "http:// is unencrypted — fine on a trusted home network, risky beyond it.".fg(WARN),
        ));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    widgets::safety_line(
        f,
        chrome.safety,
        "Nothing has been written. The change is previewed before anything happens.",
        OK,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("Enter", "preview change"), ("Esc", "back")],
    );
}

pub fn sync_endpoint_confirm(f: &mut Frame, app: &App, apply: bool) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "CONFIGURE SYNC — CONFIRM", Some((3, 3)));
    let body = widgets::padded(chrome.body, 4);
    let (Some(device), Some(candidate)) = (&app.device, &app.sync_candidate) else {
        return;
    };

    let rows = Layout::vertical([Constraint::Min(6), Constraint::Length(2)]).split(body);
    let conf = device.mount.join(crate::sync_endpoint::CONF_RELATIVE);
    let lines = vec![
        Line::raw(""),
        Line::from("Exactly one line changes, in one file:".bold()),
        Line::raw(""),
        kv("File", conf.display().to_string()),
        kv("Section", "[OneStoreServices]".to_string()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  - api_endpoint=", Style::default().fg(DIM)),
            Span::styled(
                app.sync_current
                    .clone()
                    .unwrap_or_else(|| "(not set)".to_string()),
                Style::default().fg(DANGER),
            ),
        ]),
        Line::from(vec![
            Span::styled("  + api_endpoint=", Style::default().fg(DIM)),
            Span::styled(candidate.clone(), Style::default().fg(OK).bold()),
        ]),
        Line::raw(""),
        Line::from(
            "A verbatim copy of the current file is saved on this computer first, \
             so the change can always be undone. Every other line is preserved."
                .fg(DIM),
        ),
        Line::from("After ejecting, tap Sync on the device to connect to the new server.".fg(DIM)),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[0]);
    widgets::confirm_bar(f, rows[1], "Cancel", "Apply the change", apply, false);

    widgets::safety_line(
        f,
        chrome.safety,
        "Only Kobo eReader.conf is touched — books, annotations, and databases are not.",
        WARN,
    );
    widgets::footer(
        f,
        chrome.footer,
        &[("Tab", "switch"), ("Enter", "confirm"), ("Esc", "back")],
    );
}

pub fn sync_endpoint_report(f: &mut Frame, app: &App) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "CONFIGURE SYNC — DONE", None);
    let body = widgets::padded(chrome.body, 4);
    let Some(outcome) = &app.sync_outcome else {
        return;
    };

    let mut lines = vec![
        Line::raw(""),
        Line::from(
            "✓ The sync endpoint was updated and verified."
                .fg(OK)
                .bold(),
        ),
        Line::raw(""),
        kv("File", outcome.conf_path.display().to_string()),
        kv(
            "Was",
            outcome
                .old
                .clone()
                .unwrap_or_else(|| "not set (Kobo's own store)".to_string()),
        ),
        kv("Now", outcome.new.clone()),
    ];
    if let Some(copy) = &outcome.safety_copy {
        lines.push(kv("Pre-edit copy saved to", copy.display().to_string()));
        lines.push(Line::from(
            "To undo, copy that file back over the conf on the device.".fg(DIM),
        ));
    }
    lines.extend([
        Line::raw(""),
        Line::from("Next steps".fg(ACCENT).bold()),
        Line::from("  1. Eject the Kobo safely, then unplug it."),
        Line::from("  2. On the device, tap Sync — it now talks to your server."),
        Line::from(
            "  3. If the server is brand new to this device, back up first: the first \
             sync against a new server can affect on-device annotations.",
        ),
    ]);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    widgets::safety_line(
        f,
        chrome.safety,
        "The new value was read back from the device after writing.",
        OK,
    );
    widgets::footer(f, chrome.footer, &[("Enter", "home")]);
}

// -------------------------------------------------------------------- error

pub fn error(f: &mut Frame, _app: &App, title: &str, message: &str) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, title, None);
    let body = widgets::padded(chrome.body, 6);
    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(Color::White),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(WARN))
                .title(" Details "),
        ),
        body,
    );
    widgets::footer(f, chrome.footer, &[("Enter", "home")]);
}

// ------------------------------------------------------------- confirm quit

pub fn confirm_quit(f: &mut Frame, app: &App) {
    let chrome = widgets::chrome(f);
    widgets::title_bar(f, chrome.title, "QUIT?", None);
    let body_text = if app.worker_running() {
        "An operation is still running. Quit anyway? It will be cancelled at the next \
         safe file boundary."
    } else {
        "Quit kobo-backup?"
    };
    let buttons = Line::from(vec![
        Span::styled("  y / Enter: quit  ", Style::default().fg(DANGER)),
        Span::raw("    "),
        Span::styled("  n / Esc: stay  ", Style::default().fg(OK)),
    ]);
    widgets::modal(f, "Confirm quit", body_text, buttons, app.worker_running());
    widgets::footer(f, chrome.footer, &[]);
}
