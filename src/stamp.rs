//! What a scan compares to decide whether a log changed since it was last read.

use axon::ingest::{Source, SourceKind};
use axon::store::Stamp;

/// The identity of a source's file and its sidecars: a subagent's meta file, and a SQLite
/// source's write-ahead log, where new rows sit until a checkpoint touches the main file.
/// None when the source is missing.
pub fn stamp(source: &Source) -> Option<Stamp> {
    let mut stamp = file_identity(&source.path)?;
    let mut sidecar = |path: std::path::PathBuf| {
        if let Some(identity) = file_identity(&path) {
            stamp.push('|');
            stamp.push_str(&identity);
        }
    };
    // A subagent's agent type and description come from its meta file.
    if source.kind == SourceKind::ClaudeSubagent {
        sidecar(source.path.with_extension("meta.json"));
    }
    if matches!(source.kind, SourceKind::OpenCode | SourceKind::Ccflare) {
        let mut wal = source.path.clone().into_os_string();
        wal.push("-wal");
        sidecar(wal.into());
    }
    Some(stamp)
}

/// Size, mtime and a change marker that a rewrite cannot keep: on Unix the inode and ctime,
/// which no write can set back; elsewhere a hash of the file's first and last 4 KiB.
fn file_identity(path: &std::path::Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!(
        "{}:{mtime}:{}",
        meta.len(),
        change_marker(path, &meta)?
    ))
}

#[cfg(unix)]
fn change_marker(_: &std::path::Path, meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!(
        "{}:{}.{}",
        meta.ino(),
        meta.ctime(),
        meta.ctime_nsec()
    ))
}

#[cfg(not(unix))]
fn change_marker(path: &std::path::Path, meta: &std::fs::Metadata) -> Option<String> {
    use std::hash::{Hash, Hasher};
    use std::io::{Read, Seek, SeekFrom};
    const SAMPLE: u64 = 4096;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = Vec::new();
    (&mut file).take(SAMPLE).read_to_end(&mut head).ok()?;
    let mut tail = Vec::new();
    if meta.len() > SAMPLE {
        file.seek(SeekFrom::End(-(SAMPLE as i64))).ok()?;
        file.read_to_end(&mut tail).ok()?;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (head, tail).hash(&mut hasher);
    Some(format!("{:x}", hasher.finish()))
}
