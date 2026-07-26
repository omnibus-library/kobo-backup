pub mod apply;
pub mod diff;

pub use apply::{apply, micro_backup, ApplyOptions, ApplyReport};
pub use diff::{diff, DevicePlan};
