//! Local reader-writer locks guarding repository access.
//!
//! Mirrors `python/src/memoro/locking.py`: `shared()` and `exclusive()` are
//! scope-based (context managers in Python, closures here), the lock directory
//! is created on demand, and acquisition failures surface as a
//! [`MemoroError::Repository`] with the exact Python message.

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fd_lock::RwLock;

use crate::errors::MemoroError;

pub const LOCK_TIMEOUT_SECONDS: u64 = 60;
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Reader-writer locks over `<lock_dir>/repository.lock`.
#[derive(Debug, Clone)]
pub struct RepositoryLocks {
    pub lock_dir: PathBuf,
    pub timeout: Duration,
}

impl RepositoryLocks {
    pub fn new(lock_dir: PathBuf) -> Self {
        Self {
            lock_dir,
            timeout: Duration::from_secs(LOCK_TIMEOUT_SECONDS),
        }
    }

    pub fn repository_path(&self) -> PathBuf {
        self.lock_dir.join("repository.lock")
    }

    /// Run `body` while holding the shared (read) lock.
    pub fn shared<R>(&self, body: impl FnOnce() -> R) -> Result<R, MemoroError> {
        self.acquire(false, body)
    }

    /// Run `body` while holding the exclusive (write) lock.
    pub fn exclusive<R>(&self, body: impl FnOnce() -> R) -> Result<R, MemoroError> {
        self.acquire(true, body)
    }

    fn failure(&self) -> MemoroError {
        MemoroError::Repository(format!(
            "Memoro could not acquire the local lock {} within {} seconds. Wait for the other \
             local Agent operation to finish, then retry.",
            self.repository_path().display(),
            self.timeout.as_secs()
        ))
    }

    fn acquire<R>(&self, exclusive: bool, body: impl FnOnce() -> R) -> Result<R, MemoroError> {
        let failure = || self.failure();
        std::fs::create_dir_all(&self.lock_dir).map_err(|_| failure())?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(self.repository_path())
            .map_err(|_| failure())?;
        let mut lock = RwLock::new(file);
        let deadline = Instant::now() + self.timeout;
        // The read and write guards are distinct types, so the retry loop is
        // spelled out once per mode (Python funnels both through flock flags).
        if exclusive {
            loop {
                match lock.try_write() {
                    Ok(guard) => {
                        let result = body();
                        drop(guard);
                        return Ok(result);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(failure());
                        }
                        std::thread::sleep(RETRY_INTERVAL);
                    }
                    Err(_) => return Err(failure()),
                }
            }
        } else {
            loop {
                match lock.try_read() {
                    Ok(guard) => {
                        let result = body();
                        drop(guard);
                        return Ok(result);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(failure());
                        }
                        std::thread::sleep(RETRY_INTERVAL);
                    }
                    Err(_) => return Err(failure()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn exclusive_lock_creates_the_lock_directory() {
        let directory = tempfile::tempdir().unwrap();
        let locks = RepositoryLocks::new(directory.path().join("locks").join("nested"));

        let answer = locks.exclusive(|| 41 + 1).unwrap();

        assert_eq!(answer, 42);
        assert!(locks.repository_path().exists());
    }

    #[test]
    fn contending_lock_times_out_with_a_repository_error() {
        let directory = tempfile::tempdir().unwrap();
        let acquired = Arc::new(AtomicBool::new(false));
        let holder_acquired = Arc::clone(&acquired);
        let (send_release, release) = mpsc::channel::<()>();
        let holder = RepositoryLocks::new(directory.path().to_path_buf());
        let contender = RepositoryLocks {
            lock_dir: directory.path().to_path_buf(),
            timeout: Duration::ZERO,
        };
        let handle = std::thread::spawn(move || {
            holder
                .exclusive(|| {
                    holder_acquired.store(true, Ordering::SeqCst);
                    let _ = release.recv_timeout(Duration::from_secs(10));
                })
                .unwrap();
        });

        while !acquired.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let error = contender.exclusive(|| ()).unwrap_err();

        assert_eq!(
            error,
            MemoroError::Repository(format!(
                "Memoro could not acquire the local lock {} within 0 seconds. Wait for the other \
                 local Agent operation to finish, then retry.",
                contender.repository_path().display()
            ))
        );
        send_release.send(()).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn shared_locks_do_not_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let acquired = Arc::new(AtomicBool::new(false));
        let holder_acquired = Arc::clone(&acquired);
        let (send_release, release) = mpsc::channel::<()>();
        let first = RepositoryLocks::new(directory.path().to_path_buf());
        let second = RepositoryLocks::new(directory.path().to_path_buf());
        let handle = std::thread::spawn(move || {
            first
                .shared(|| {
                    holder_acquired.store(true, Ordering::SeqCst);
                    let _ = release.recv_timeout(Duration::from_secs(10));
                })
                .unwrap();
        });

        while !acquired.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let answer = second.shared(|| 7 * 6).unwrap();
        send_release.send(()).unwrap();
        handle.join().unwrap();

        assert_eq!(answer, 42);
    }
}
