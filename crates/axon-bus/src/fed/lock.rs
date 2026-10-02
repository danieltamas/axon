//! One federation service per data dir: an OS lock on `fed/service.lock`, dropped by the
//! kernel if the holder dies. The holder is named in `fed/service.pid`, a separate file,
//! because Windows locks are mandatory and the locked file cannot be read by anyone else.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;

pub const LOCK_FILE: &str = "service.lock";
const PID_FILE: &str = "service.pid";

/// Held for as long as the service runs.
#[derive(Debug)]
pub struct ServiceLock(#[allow(dead_code)] File); // held, never read

pub enum Acquire {
    Held(ServiceLock),
    /// Another process owns the lock; what it wrote there, for the log.
    HeldBy(String),
}

pub fn acquire(fed_dir: &Path) -> std::io::Result<Acquire> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    // Like every other file in `fed/`; it holds only a pid, but a lock nobody else can open
    // cannot be used to confuse the owner.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(fed_dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => {
            let mut pid = options.truncate(true).open(fed_dir.join(PID_FILE))?;
            write!(pid, "pid {}", std::process::id())?;
            Ok(Acquire::Held(ServiceLock(file)))
        }
        Err(TryLockError::WouldBlock) => {
            let holder = std::fs::read_to_string(fed_dir.join(PID_FILE)).unwrap_or_default();
            Ok(Acquire::HeldBy(holder))
        }
        Err(TryLockError::Error(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_is_refused_naming_the_holder_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let Acquire::Held(first) = acquire(dir.path()).unwrap() else {
            panic!("an empty dir must be lockable");
        };
        match acquire(dir.path()).unwrap() {
            Acquire::HeldBy(holder) => assert_eq!(holder, format!("pid {}", std::process::id())),
            Acquire::Held(_) => panic!("two services on one data dir"),
        }
        drop(first);
        // A test thread forking `git` holds a copy of the descriptor until it execs, so the
        // release can lag by a few milliseconds.
        let released = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            matches!(acquire(dir.path()).unwrap(), Acquire::Held(_))
        });
        assert!(released, "the lock is free once its holder drops");
    }
}
