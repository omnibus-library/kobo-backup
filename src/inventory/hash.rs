use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::progress::CancelToken;

pub const CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB

/// Streaming SHA-256 of a file, checking the cancel token between chunks
/// and reporting byte progress via `on_bytes(delta)`.
pub fn sha256_file(
    path: &Path,
    cancel: &CancelToken,
    mut on_bytes: impl FnMut(u64),
) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("cannot open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        cancel.check()?;
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read error while hashing {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        on_bytes(n as u64);
    }
    Ok(hex(&hasher.finalize()))
}

/// SHA-256 of an in-flight stream: read from `reader`, write to `writer`,
/// hashing every byte that passes through. Returns (hash, bytes copied).
pub fn sha256_copy(
    mut reader: impl Read,
    mut writer: impl std::io::Write,
    cancel: &CancelToken,
    mut on_bytes: impl FnMut(u64),
) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let n = reader.read(&mut buf).context("read error in stream copy")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer
            .write_all(&buf[..n])
            .context("write error in stream copy")?;
        total += n as u64;
        on_bytes(n as u64);
    }
    Ok((hex(&hasher.finalize()), total))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_known_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.txt");
        std::fs::write(&p, b"abc").unwrap();
        let h = sha256_file(&p, &CancelToken::new(), |_| {}).unwrap();
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cancel_aborts_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.bin");
        std::fs::write(&p, vec![0u8; 4 * CHUNK_SIZE]).unwrap();
        let token = CancelToken::new();
        token.cancel();
        assert!(sha256_file(&p, &token, |_| {}).is_err());
    }

    #[test]
    fn sha256_copy_matches_file_hash() {
        let data = b"hello kobo".to_vec();
        let mut out = Vec::new();
        let (h, n) = sha256_copy(&data[..], &mut out, &CancelToken::new(), |_| {}).unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(out, data);
        let mut hasher = Sha256::new();
        hasher.update(&data);
        assert_eq!(h, hex(&hasher.finalize()));
    }
}
