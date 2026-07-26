use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::Sender;

use crate::event::Event;
use crate::insights::LibraryInsights;
use crate::inventory::Inventory;
use crate::manifest::Manifest;
use crate::progress::{CancelToken, Cancelled, ProgressUpdate};
use crate::restore::{ApplyReport, DevicePlan};
use crate::verify::VerifyReport;

/// Result payload of a completed background job.
/// Variants differ widely in size, but values only ever travel boxed
/// (see `WorkerMsg::Done`), so the size difference is irrelevant.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum JobOutput {
    /// Backup B3: device scanned & hashed; insights from a temp DB copy.
    Scanned {
        inventory: Inventory,
        insights: Option<LibraryInsights>,
    },
    /// Backup B5: zip written to a .partial path (not yet promoted).
    ZipWritten {
        partial_path: PathBuf,
        manifest: Manifest,
    },
    /// Backup B6: verification finished; on success the zip was promoted.
    BackupVerified {
        report: VerifyReport,
        final_path: Option<PathBuf>,
    },
    /// Restore R2: zip validated; manifest + insights extracted from backup.
    ZipValidated {
        zip_path: PathBuf,
        manifest: Manifest,
        insights: Option<LibraryInsights>,
    },
    /// Restore R6: device scanned and diffed against the manifest.
    Diffed {
        plan: DevicePlan,
        device_insights: Option<LibraryInsights>,
    },
    /// Restore R10: safety copy of current device databases.
    MicroBackedUp { dir: PathBuf },
    /// Restore R11: files applied to the device.
    Applied { report: ApplyReport },
    /// Restore R12: device re-scanned and verified against manifest.
    RestoreVerified { report: VerifyReport },
}

#[derive(Debug)]
pub enum WorkerMsg {
    Progress(ProgressUpdate),
    Done(Box<JobOutput>),
    Failed(String),
    Cancelled,
}

pub struct WorkerHandle {
    pub cancel: CancelToken,
}

/// Throttling progress reporter passed to engine functions.
pub struct Reporter {
    tx: Sender<Event>,
    last_emit: std::sync::Mutex<Instant>,
}

const EMIT_INTERVAL: Duration = Duration::from_millis(50);

impl Reporter {
    pub fn report(&self, update: ProgressUpdate) {
        let mut last = self.last_emit.lock().unwrap();
        if last.elapsed() >= EMIT_INTERVAL {
            *last = Instant::now();
            let _ = self.tx.send(Event::Worker(WorkerMsg::Progress(update)));
        }
    }

    /// Unthrottled: use for phase transitions so they are never dropped.
    pub fn report_now(&self, update: ProgressUpdate) {
        *self.last_emit.lock().unwrap() = Instant::now();
        let _ = self.tx.send(Event::Worker(WorkerMsg::Progress(update)));
    }
}

/// Spawn a background job. Its result (or failure/cancellation) arrives on
/// the event channel as a `WorkerMsg`. Exactly one worker should be alive at
/// a time — the App enforces that.
pub fn spawn<F>(tx: Sender<Event>, cancel: CancelToken, job: F) -> WorkerHandle
where
    F: FnOnce(&Reporter, &CancelToken) -> Result<JobOutput> + Send + 'static,
{
    let token = cancel.clone();
    let reporter = Reporter {
        tx: tx.clone(),
        last_emit: std::sync::Mutex::new(Instant::now() - EMIT_INTERVAL),
    };
    std::thread::spawn(move || {
        let msg = match job(&reporter, &token) {
            Ok(output) => WorkerMsg::Done(Box::new(output)),
            Err(err) if err.is::<Cancelled>() => WorkerMsg::Cancelled,
            Err(err) => WorkerMsg::Failed(format!("{err:#}")),
        };
        let _ = tx.send(Event::Worker(msg));
    });
    WorkerHandle { cancel }
}
