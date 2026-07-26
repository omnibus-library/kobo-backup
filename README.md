# kobo-backup

A transparency-first terminal app (Rust + [Ratatui](https://ratatui.rs)) that backs up a Kobo
e-reader to a single zip file and can restore the device to exactly that point in time —
books, annotations, highlights, reading progress, collections, settings, and databases.

Built around one principle: **you should never have to trust it blindly.** Every step shows
exactly what it is about to do, requires an explicit confirmation (the default action is
always the safe one), and verifies its own work by re-reading every byte it wrote.

## Usage

```
cargo run --release
```

Optional flags:

| Flag | Meaning |
|---|---|
| `--device <path>` | Use this mount path instead of auto-detection (e.g. `/Volumes/KOBOeReader`) |
| `--out <dir>` | Where backup zips are stored (default `~/KoboBackups`) |

Connect the Kobo over USB, tap **Connect** on its screen, and run the app.

## What a backup contains

The **entire volume**, mirrored: the hidden `.kobo/` directory (`KoboReader.sqlite` with its
WAL/SHM sidecars, `BookReader.sqlite`, `Kobo eReader.conf`, `version`, `device.salt.conf`,
Kobo-store `kepub/` books, certificates, dictionaries, stylus `markups/`), all sideloaded
books, `Digital Editions/` (Adobe DRM), fonts, screensavers — everything.

The only exclusions are host-OS artifacts that are not Kobo state (`.Spotlight-V100/`,
`.fseventsd/`, `.Trashes/`, `.TemporaryItems/`, `.DS_Store`, `._*` AppleDouble files, and this
tool's own `*.kbtmp` leftovers). The exclusion rules and skipped counts are recorded in the
manifest so nothing is silently hidden.

Inside the zip: `manifest.json` (device serial/model/firmware, timestamps, and a SHA-256 +
size + mtime for every file) plus the content under `files/`. The manifest is only written
after verification passes, so **a zip containing a manifest is by construction a verified zip**.

## Safety model

Backup:

1. Detection reads one file (`.kobo/version`). You confirm the identity on screen.
2. The scan is 100% read-only: every file enumerated and SHA-256-hashed, with live progress.
3. You review a full summary — including library facts read from a *copy* of the database
   (books, annotations per book, reading progress) — before anything is written.
4. The zip is written to `<name>.zip.partial`, re-hashing every file as it streams; if a file
   changed since the scan, the backup aborts.
5. Verification re-opens the zip fresh, re-reads **every** entry, compares all hashes, runs
   `PRAGMA integrity_check` on the backup's copy of `KoboReader.sqlite`, and re-hashes the
   device databases to detect mid-run drift. Only then is `.partial` promoted to the final name.
6. The Kobo is never written to at any point during a backup.

Restore:

1. The chosen zip gets a full integrity sweep (every hash re-checked, paths validated against
   zip-slip and FAT32 rules) before anything else happens.
2. The backup's serial is compared with the connected device. A mismatch shows a red gate that
   requires typing `DIFFERENT DEVICE` (DRM files are device-bound; store purchases will likely
   not survive a cross-device transplant).
3. You see the exact plan — files to overwrite / add / **delete**, with the device's current
   library facts side-by-side against the backup's — before consenting.
4. The deletion pass (what makes it a true point-in-time restore) gets its own consent screen.
5. Typing `RESTORE` is the final gate. Then:
   - current device databases are safety-copied to `~/.kobo-backup/pre-restore/<serial>-<ts>/`;
   - each file streams to `<target>.kbtmp`, is hash-verified, fsynced, and only then renamed
     over the target — a yanked cable never leaves truncated files;
   - `KoboReader.sqlite` is written last of all files;
   - deletions run last, only after every write verified;
   - the whole device is re-read and compared hash-by-hash against the manifest, and the
     restored database gets an in-place read-only `integrity_check`.
6. Aborting mid-restore leaves a mix of old and complete new files (never partial ones);
   re-running the restore is idempotent and finishes the job.

## Manual verification protocol (real hardware)

1. Plug in the Kobo, run a backup, and let verification finish (all checks green).
2. Open the zip by hand: check `manifest.json` matches your device serial; spot-check a book.
3. On the Kobo: add a highlight and a note to any book, and read a few pages further.
4. Run a restore from the step-1 backup (same device, accept the deletion pass).
5. After the green report: eject, unplug, and confirm on the device that the new highlight is
   gone, reading position is back where it was, and library/collections look exactly as before.
6. Re-plug and run another backup — the diff of the two zips' manifests should be empty except
   for timestamps.

## Development

```
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

The test suite stages a fake Kobo volume (real SQLite schema, junk files, DRM dirs) and drives:
engine round-trips (backup → mutate → restore → byte-identical assertion), corruption refusal,
mid-backup drift detection, cancellation cleanliness, the full TUI state machine via synthetic
key events (including the Enter-mash and wrong-phrase gates), and a render pass over every
screen on a headless terminal.

### Layout

- `src/inventory/` — read-only volume scan, exclusions, SHA-256
- `src/insights.rs` — library facts from a copy of `KoboReader.sqlite` (never opens the device DB)
- `src/archive/` — streaming zip write/read, manifest-last discipline
- `src/verify.rs` — both verification directions
- `src/restore/` — diff plan + `apply.rs`, the **only** code that writes to the device
- `src/app.rs` — wizard state machine; `src/ui/` — Ratatui screens & widgets
