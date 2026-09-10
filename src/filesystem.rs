//! Atomic filesystem primitives.
//!
//! Mirrors `python/src/memoro/filesystem.py`: write data through a temporary
//! file in the target directory, fsync it, then rename it over the target.

use std::fs::{self, Metadata, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn metadata_mode(metadata: &Metadata) -> u32 {
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn metadata_mode(_: &Metadata) -> u32 {
    0o600
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

/// Atomically replace `target` with `data`.
///
/// The temporary file is created in the target's directory (so the final
/// rename stays on one filesystem), written and fsynced, inherits the mode of
/// an existing target (or `0o600` for fresh files, like `tempfile.mkstemp`),
/// and is renamed into place. Any failure removes the temporary file.
pub fn atomic_replace(target: &Path, data: &[u8]) -> io::Result<()> {
    let existing_mode = fs::metadata(target)
        .ok()
        .map(|metadata| metadata_mode(&metadata));
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());

    let mut collision = None;
    for _ in 0..100 {
        let counter = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let temporary = target.with_file_name(format!(
            ".{file_name}.{}-{nanos}-{counter}.tmp",
            std::process::id()
        ));
        let mut handle = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(handle) => handle,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                collision = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let outcome = (|| {
            use std::io::Write;
            handle.write_all(data)?;
            handle.sync_all()?;
            drop(handle);
            set_mode(&temporary, existing_mode.unwrap_or(0o600))?;
            fs::rename(&temporary, target)
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return outcome;
    }
    Err(collision.unwrap_or_else(|| io::Error::other("could not create a temporary file")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn atomic_replace_round_trips_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("nested").join("config.json");

        atomic_replace(&target, b"{\"a\": 1}\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"{\"a\": 1}\n");

        atomic_replace(&target, &[0xff, 0x00, 0xfe]).unwrap();
        assert_eq!(fs::read(&target).unwrap(), &[0xff, 0x00, 0xfe]);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_preserves_the_existing_mode() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("data.bin");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

        atomic_replace(&target, b"new").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(mode_of(&target), 0o640);

        let fresh = directory.path().join("fresh.bin");
        atomic_replace(&fresh, b"new").unwrap();
        assert_eq!(fs::read(&fresh).unwrap(), b"new");
        assert_eq!(mode_of(&fresh), 0o600);
    }

    #[test]
    fn atomic_replace_leaves_no_temporary_files_behind() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.json");
        atomic_replace(&target, b"payload").unwrap();
        let leftovers: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.') || name.ends_with(".tmp"))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());
    }
}
