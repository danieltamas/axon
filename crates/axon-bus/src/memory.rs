//! Resident memory of the agents' harness processes (BUS-PLAN §7.1), and the harness
//! sessions open on this machine whether or not they registered on the bus. Hooks record
//! the pid; `serve` samples here on a slow timer, never once per snapshot.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;

use rusqlite::Connection;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

/// Executable names of the harnesses whose sessions are listed.
const HARNESSES: [(&str, &str); 4] = [
    ("claude", "claude"),
    ("codex", "codex"),
    ("opencode", "opencode"),
    ("hermes", "hermes"),
];

/// One open harness session: a process running a harness CLI in a working directory.
#[derive(Clone)]
pub struct Session {
    pub pid: i64,
    pub harness: &'static str,
    pub cwd: PathBuf,
    pub rss: u64,
    /// Process start, in unix milliseconds.
    pub started_ms: i64,
}

pub struct Sampler {
    system: System,
    rss: HashMap<i64, u64>,
    /// When the last sample read the process table, in unix milliseconds.
    sampled_at: i64,
    /// Whether this sample saw this very process; a sandbox can hide the process table.
    sees_processes: bool,
    sessions: Vec<Session>,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            rss: HashMap::new(),
            sampled_at: 0,
            sees_processes: false,
            sessions: Vec::new(),
        }
    }

    /// Re-read every process once: the registered agents' memory and the open harness
    /// sessions.
    pub fn sample(&mut self, conn: &Connection) -> anyhow::Result<()> {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pid FROM agents WHERE pid IS NOT NULL AND status IN ('active','idle')",
        )?;
        let pids: Vec<i64> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cwd(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        let rss = pids
            .into_iter()
            .filter_map(|pid| {
                let process = self
                    .system
                    .process(Pid::from_u32(u32::try_from(pid).ok()?))?;
                Some((pid, whole_mib(process.memory())))
            })
            .collect();
        self.sampled_at = crate::store::now_ms();
        self.sees_processes = self
            .system
            .process(Pid::from_u32(std::process::id()))
            .is_some();
        self.sessions = self.open_sessions();
        self.rss = rss;
        Ok(())
    }

    /// Whether the harness process `pid`, last heard from at `last_seen_ms`, still runs. A
    /// process started after that is another one that reused the pid. None when this sample
    /// could not see processes at all, or misses a pid heard from after it was taken: a
    /// session that registered or resumed since may run a process the sample never saw.
    pub fn running(&self, pid: i64, last_seen_ms: i64) -> Option<bool> {
        if !self.sees_processes {
            return None;
        }
        match self.system.process(Pid::from_u32(u32::try_from(pid).ok()?)) {
            Some(process) => Some(start_ms(process.start_time()) <= last_seen_ms),
            None if last_seen_ms >= self.sampled_at => None,
            None => Some(false),
        }
    }

    fn open_sessions(&self) -> Vec<Session> {
        let harness_of = |pid: Pid| -> Option<&'static str> {
            let process = self.system.process(pid)?;
            let name = process.exe()?.file_name()?.to_str()?;
            HARNESSES
                .iter()
                .find(|(exe, _)| *exe == name)
                .map(|(_, harness)| *harness)
        };
        let mut sessions: Vec<Session> = self
            .system
            .processes()
            .iter()
            .filter_map(|(&pid, process)| {
                let harness = harness_of(pid)?;
                // A harness's own helpers (Claude's `bg-*` hosts, a child it spawned) are
                // not sessions; neither is a desktop app bundle.
                if process.parent().and_then(harness_of) == Some(harness)
                    || process.cmd().iter().skip(1).any(|arg| helper_arg(arg))
                    || process.exe()?.to_string_lossy().contains(".app/")
                {
                    return None;
                }
                let mut cwd = process.cwd()?.to_path_buf();
                if harness == "codex" {
                    cwd = codex_cwd(process.cmd(), cwd);
                }
                // The filesystem root is no one's project.
                cwd.parent()?;
                Some(Session {
                    pid: i64::from(pid.as_u32()),
                    harness,
                    cwd,
                    rss: whole_mib(process.memory()),
                    started_ms: start_ms(process.start_time()),
                })
            })
            .collect();
        sessions.sort_by_key(|s| s.pid);
        sessions
    }

    pub fn rss(&self) -> &HashMap<i64, u64> {
        &self.rss
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Ask one of the sampled sessions to exit (SIGTERM), as closing its terminal would.
    /// Only the process this sample found running a harness session, started at
    /// `started_ms`, qualifies, and it is read again just before the signal: a pid the
    /// system reused since is a different start time and is refused. True once sent.
    pub fn terminate(&mut self, pid: i64, started_ms: i64) -> bool {
        let Ok(raw) = u32::try_from(pid) else {
            return false;
        };
        if !self
            .sessions
            .iter()
            .any(|s| s.pid == pid && s.started_ms == started_ms)
        {
            return false;
        }
        let pid = Pid::from_u32(raw);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        self.system
            .process(pid)
            .filter(|process| start_ms(process.start_time()) == started_ms)
            .and_then(|process| process.kill_with(Signal::Term))
            .unwrap_or(false)
    }
}

fn start_ms(start_secs: u64) -> i64 {
    i64::try_from(start_secs).unwrap_or(0) * 1000
}

/// Where a Codex process works: `codex exec -C <dir>` keeps the OS cwd and records the
/// session under `<dir>`, so the flag (last one wins, relative to the OS cwd) is the
/// directory its transcript names. Only the flags before a bare `--` are read.
pub fn codex_cwd(argv: &[impl AsRef<OsStr>], os_cwd: PathBuf) -> PathBuf {
    let mut dir: Option<&str> = None;
    let mut args = argv.iter().skip(1).filter_map(|a| a.as_ref().to_str());
    while let Some(arg) = args.next() {
        dir = match arg {
            "--" => break,
            "-C" | "--cd" => args.next().or(dir),
            _ => arg
                .strip_prefix("--cd=")
                .or_else(|| arg.strip_prefix("-C").filter(|_| !arg.starts_with("--")))
                .map(|d| d.strip_prefix('=').unwrap_or(d))
                .or(dir),
        };
    }
    dir.filter(|d| !d.is_empty())
        .map_or(os_cwd.clone(), |d| os_cwd.join(d))
}

fn helper_arg(arg: &OsStr) -> bool {
    arg.to_str().is_some_and(|a| a.starts_with("bg-"))
}

/// Whole MiB, so a few pages of churn do not republish the snapshot.
fn whole_mib(bytes: u64) -> u64 {
    bytes >> 20 << 20
}

#[cfg(test)]
mod codex_cwd_tests {
    use super::*;

    fn cwd_of(argv: &[&str]) -> PathBuf {
        codex_cwd(argv, PathBuf::from("/repo"))
    }

    #[test]
    fn codex_cwd_follows_the_cd_flag_in_every_spelling() {
        for flag in [
            &["codex", "exec", "-C", "/wt"][..],
            &["codex", "exec", "--cd", "/wt"],
            &["codex", "exec", "--cd=/wt"],
            &["codex", "exec", "-C/wt"],
            &["codex", "exec", "-C=/wt", "do it"],
        ] {
            assert_eq!(cwd_of(flag), PathBuf::from("/wt"), "{flag:?}");
        }
    }

    #[test]
    fn codex_cwd_resolves_a_relative_dir_and_ignores_the_prompt_after_dashes() {
        assert_eq!(
            cwd_of(&["codex", "-C", "../wt"]),
            PathBuf::from("/repo/../wt")
        );
        assert_eq!(
            cwd_of(&["codex", "exec", "--", "-C", "/x"]),
            PathBuf::from("/repo")
        );
        assert_eq!(
            cwd_of(&["codex", "exec", "--model", "o3"]),
            PathBuf::from("/repo")
        );
        assert_eq!(cwd_of(&["codex", "-C"]), PathBuf::from("/repo"));
    }
}

#[cfg(test)]
mod acceptance_review2 {
    use super::*;

    fn session(pid: i64, started_ms: i64) -> Session {
        Session {
            pid,
            started_ms,
            harness: "claude",
            cwd: std::env::temp_dir(),
            rss: 0,
        }
    }

    #[test]
    fn r1_terminate_refuses_generation_different_from_sample() {
        let mut sampler = Sampler::new();
        let pid = i64::from(std::process::id());
        sampler.sessions.push(session(pid, 1_000));
        assert!(!sampler.terminate(pid, -1));
    }

    #[test]
    fn r1_terminate_refuses_generation_different_from_live_process() {
        let mut sampler = Sampler::new();
        let pid = i64::from(std::process::id());
        // The cached identity passes, but no real process can have this start time.
        // This exercises only refusal; no matching generation is ever supplied.
        sampler.sessions.push(session(pid, -1));
        assert!(!sampler.terminate(pid, -1));
    }
}
