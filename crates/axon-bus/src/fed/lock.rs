//! One federation service per data dir: an OS advisory lock on `fed/service.lock`, dropped
//! by the kernel if the holder dies.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::Path;

pub const LOCK_FILE: &str = "service.lock";

/// Held for as long as the service runs.
#[derive(Debug)]
pub struct ServiceLock(#[allow(dead_code)] File); // held, never read

pub enum Acquire {
    Held(ServiceLock),
    /// Another process owns the lock; what it wrote there, for the log.
    HeldBy(String),
}

pub fn acquire(fed_dir: &Path) -> std::io::Result<Acquire> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fed_dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            file.rewind()?;
            write!(file, "pid {}", std::process::id())?;
            Ok(Acquire::Held(ServiceLock(file)))
        }
        Err(TryLockError::WouldBlock) => {
            let mut holder = String::new();
            file.read_to_string(&mut holder)?;
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
        assert!(matches!(acquire(dir.path()).unwrap(), Acquire::Held(_)));
    }
}
