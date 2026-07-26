use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use humansize::{format_size, DECIMAL};

pub fn bytes(n: u64) -> String {
    format_size(n, DECIMAL)
}

pub fn eta(bytes_done: u64, bytes_total: u64, elapsed: Duration) -> Option<Duration> {
    if bytes_done == 0 || bytes_total == 0 || bytes_done >= bytes_total {
        return None;
    }
    let rate = bytes_done as f64 / elapsed.as_secs_f64().max(0.001);
    if rate <= 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        (bytes_total - bytes_done) as f64 / rate,
    ))
}

pub fn throughput(bytes_done: u64, elapsed: Duration) -> String {
    let rate = bytes_done as f64 / elapsed.as_secs_f64().max(0.001);
    format!("{}/s", format_size(rate as u64, DECIMAL))
}

pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {:02}m {:02}s", s / 3600, (s % 3600) / 60, s % 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Total and free bytes of the filesystem containing `path`.
pub fn disk_space(path: &Path) -> Result<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath =
        CString::new(path.as_os_str().as_bytes()).context("path contains interior NUL byte")?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("statvfs failed for {}", path.display()));
    }
    let frsize = if stat.f_frsize > 0 {
        stat.f_frsize as u64
    } else {
        stat.f_bsize as u64
    };
    let total = frsize * stat.f_blocks as u64;
    let free = frsize * stat.f_bavail as u64;
    Ok((total, free))
}

/// Current time as a jiff Timestamp.
pub fn now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Filesystem-safe local timestamp for filenames: 20260726-140311
pub fn filename_timestamp() -> String {
    let zoned = jiff::Zoned::now();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        zoned.year(),
        zoned.month(),
        zoned.day(),
        zoned.hour(),
        zoned.minute(),
        zoned.second()
    )
}

/// mtime of a file as a jiff Timestamp (None if unavailable).
pub fn file_mtime(path: &Path) -> Option<jiff::Timestamp> {
    let meta = std::fs::metadata(path).ok()?;
    let systime = meta.modified().ok()?;
    jiff::Timestamp::try_from(systime).ok()
}

/// Convert a jiff Timestamp to SystemTime.
pub fn timestamp_to_systemtime(ts: jiff::Timestamp) -> std::time::SystemTime {
    use std::time::UNIX_EPOCH;
    let secs = ts.as_second();
    let nanos = ts.subsec_nanosecond();
    if secs >= 0 {
        UNIX_EPOCH + Duration::new(secs as u64, nanos as u32)
    } else {
        UNIX_EPOCH - Duration::from_secs((-secs) as u64) + Duration::from_nanos(nanos as u64)
    }
}
