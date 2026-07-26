//! Engine-level progress reporting and cancellation, free of any UI types.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative cancellation flag shared between the UI and a worker.
/// Engine code checks it at file boundaries and chunk boundaries; it is
/// never checked mid-write of a chunk, so files are never left truncated
/// by a cancel (interrupted files are temp files that get cleaned up).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

/// Error returned by engine operations when the cancel token trips.
#[derive(Debug, thiserror::Error)]
#[error("operation cancelled")]
pub struct Cancelled;

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A progress snapshot emitted by long-running engine operations.
#[derive(Debug, Clone, Default)]
pub struct ProgressUpdate {
    /// Short label of what is happening right now ("Hashing", "Writing zip"...).
    pub phase: String,
    /// The exact path currently being processed (device-relative when possible).
    pub current_path: String,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Optional extra fact line ("1,412 / 1,892 hashes match").
    pub detail: Option<String>,
}

/// Callback used by engine functions to report progress.
pub type ProgressFn<'a> = &'a (dyn Fn(ProgressUpdate) + Send + Sync);

/// A no-op progress sink, handy in tests.
pub fn silent() -> impl Fn(ProgressUpdate) + Send + Sync {
    |_| {}
}
