//! What an observed session says, read from the tail of the transcript its harness writes
//! (Claude Code and Codex; OpenCode and Hermes keep theirs in stores this does not read).
//! Rows go through the same parser and privacy rule as a registered agent's narrative:
//! without content capture only tool names and reasoning token counts show, and the
//! operator's latest prompt is withheld.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::{Duration as Days, Local};
use serde_json::Value;

use crate::snapshot::{push_line, Line};
use crate::transcript::{self, Row};

/// Bytes read from the end of a transcript; a turn with large tool output can fill this.
const TAIL_BYTES: u64 = 512 * 1024;
/// Narrative rows kept per session, newest last.
const ROWS: usize = 80;
/// Longest prompt shown as a session's mission.
const PROMPT_CHARS: usize = 240;
/// How far back a session with no prompt in its tail is searched for one.
const PROMPT_SEARCH_BYTES: u64 = 8 * 1024 * 1024;
/// Fewer words than this is a reply ("da", "ok. do it"), not a request.
const MISSION_WORDS: usize = 4;
/// A transcript not found is looked for again after this long.
const RETRY_MISSING: Duration = Duration::from_secs(60);
/// Codex files transcripts under the day they started; a session resumed later than this
/// is not found.
const CODEX_DAYS: i64 = 8;

/// Transcripts found so far, re-parsed only when their file changes.
#[derive(Default)]
pub struct Tails {
    found: HashMap<String, Located>,
}

enum Located {
    Missing(Instant),
    At(Tail),
}

struct Tail {
    path: PathBuf,
    stamp: Option<(u64, SystemTime, bool)>,
    rows: Vec<Value>,
    prompt: Option<String>,
    searched: bool,
}

/// The narrative rows and the latest operator prompt of one session.
pub struct Said<'a> {
    pub rows: &'a [Value],
    pub prompt: Option<&'a str>,
    /// When the harness last wrote the transcript: mid-turn, Axon has no usage yet.
    pub written_ms: i64,
}

impl Tails {
    pub fn read(&mut self, harness: &str, session_id: &str, content: bool) -> Option<Said<'_>> {
        // Session ids come from Axon's database; anything but an id never reaches a path.
        let plain = !session_id.is_empty()
            && session_id.len() <= 80
            && session_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !plain {
            return None;
        }
        let stale = match self.found.get(session_id) {
            None => true,
            Some(Located::Missing(at)) => at.elapsed() >= RETRY_MISSING,
            Some(Located::At(_)) => false,
        };
        if stale {
            let located = match locate(harness, session_id) {
                Some(path) => Located::At(Tail {
                    path,
                    stamp: None,
                    rows: Vec::new(),
                    prompt: None,
                    searched: false,
                }),
                None => Located::Missing(Instant::now()),
            };
            self.found.insert(session_id.to_owned(), located);
        }
        let Some(Located::At(tail)) = self.found.get_mut(session_id) else {
            return None;
        };
        let meta = std::fs::metadata(&tail.path).ok()?;
        // The content flag is part of the stamp: turning capture off must drop text now.
        let stamp = (meta.len(), meta.modified().ok()?, content);
        if tail.stamp != Some(stamp) {
            let text = read_tail(&tail.path, meta.len(), TAIL_BYTES).ok()?;
            let (rows, prompt) = parse(harness, &text, content);
            tail.rows = rows;
            // The mission is the last real request. A long turn pushes it out of the tail,
            // so the last one seen holds; a tail with only a reply is searched deeper, once.
            if !content {
                tail.prompt = None;
            } else if prompt.as_deref().is_some_and(substantive) {
                tail.prompt = prompt;
            } else if !tail.prompt.as_deref().is_some_and(substantive) {
                let mut request = None;
                if !tail.searched {
                    tail.searched = true;
                    let deeper = read_tail(&tail.path, meta.len(), PROMPT_SEARCH_BYTES).ok()?;
                    request = latest_prompt(harness, &deeper)
                        .filter(|p| substantive(p))
                        .map(|p| clip(&p));
                }
                tail.prompt = request.or(prompt).or(tail.prompt.take());
            }
            tail.stamp = Some(stamp);
        }
        let written_ms = stamp
            .1
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        Some(Said {
            rows: &tail.rows,
            prompt: tail.prompt.as_deref(),
            written_ms,
        })
    }
}

fn locate(harness: &str, session_id: &str) -> Option<PathBuf> {
    match harness {
        "claude" => {
            let name = format!("{session_id}.jsonl");
            let projects =
                crate::install::env_dir("CLAUDE_CONFIG_DIR", &[".claude"]).join("projects");
            std::fs::read_dir(projects)
                .ok()?
                .filter_map(Result::ok)
                .map(|dir| dir.path().join(&name))
                .find(|path| path.is_file())
        }
        "codex" => {
            let sessions = crate::install::env_dir("CODEX_HOME", &[".codex"]).join("sessions");
            let suffix = format!("-{session_id}.jsonl");
            let today = Local::now().date_naive();
            (0..CODEX_DAYS).find_map(|back| {
                let day = sessions.join((today - Days::days(back)).format("%Y/%m/%d").to_string());
                std::fs::read_dir(day)
                    .ok()?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .find(|path| path.to_string_lossy().ends_with(&suffix))
            })
        }
        _ => None,
    }
}

fn read_tail(path: &Path, len: u64, bytes: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let start = len.saturating_sub(bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // Mid-file, the first line is cut; the last may still be being written.
    let from = if start > 0 {
        text.find('\n').map_or(text.len(), |i| i + 1)
    } else {
        0
    };
    let to = text.rfind('\n').map_or(from, |i| i + 1).max(from);
    Ok(text[from..to].to_owned())
}

fn parse(harness: &str, text: &str, content: bool) -> (Vec<Value>, Option<String>) {
    let kept = |text: Option<String>| text.filter(|_| content).map(|t| transcript::redact(&t));
    let mut rows = Vec::new();
    for record in records(text) {
        let ts = transcript::timestamp(harness, &record).unwrap_or(0);
        for row in transcript::rows(harness, &record, None) {
            let (kind, text, recorded, tokens, tool) = match row {
                Row::Assistant(text) => ("assistant", kept(Some(text)), None, None, None),
                Row::Progress(text) => ("progress", kept(Some(text)), None, None, None),
                Row::Reasoning { text, tokens } => (
                    "reasoning",
                    kept(text.clone()),
                    Some(text.is_some()),
                    tokens,
                    None,
                ),
                Row::Tool {
                    name,
                    detail,
                    failed,
                } => ("tool", None, None, None, Some((name, kept(detail), failed))),
            };
            push_line(
                &mut rows,
                Line {
                    kind,
                    source: harness,
                    text,
                    recorded,
                    tokens,
                    tool,
                    ts,
                },
            );
        }
    }
    let skip = rows.len().saturating_sub(ROWS);
    rows.drain(..skip);
    let prompt = latest_prompt(harness, text)
        .filter(|_| content)
        .map(|p| clip(&p));
    (rows, prompt)
}

fn records(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
}

/// The operator's last real request: "yes" or "do it" answers the agent instead.
fn latest_prompt(harness: &str, text: &str) -> Option<String> {
    let prompts: Vec<String> = records(text)
        .filter_map(|r| operator_prompt(harness, &r))
        .collect();
    let request = prompts.iter().rev().find(|p| substantive(p));
    request.or(prompts.last()).cloned()
}

fn substantive(prompt: &str) -> bool {
    prompt.split_whitespace().count() >= MISSION_WORDS
}

fn clip(prompt: &str) -> String {
    transcript::redact(prompt)
        .chars()
        .take(PROMPT_CHARS)
        .collect()
}

/// A prompt the operator typed. Harness-injected turns (command output, reminders,
/// environment blocks) open with markup and are not the operator's words.
fn operator_prompt(harness: &str, record: &Value) -> Option<String> {
    let content = match harness {
        // Claude marks injected turns (peer messages, subagent reports) as meta or as from
        // an origin other than the human.
        "claude"
            if record["type"] == "user"
                && record["isMeta"] != true
                && record["isCompactSummary"] != true
                && record["origin"]["kind"]
                    .as_str()
                    .map_or(true, |k| k == "human") =>
        {
            &record["message"]["content"]
        }
        "codex"
            if record["payload"]["type"] == "message" && record["payload"]["role"] == "user" =>
        {
            &record["payload"]["content"]
        }
        _ => return None,
    };
    // A prompt with an image arrives as blocks; tool results are blocks without text.
    let text = match content {
        Value::String(text) => text.as_str(),
        Value::Array(blocks) => blocks.iter().find_map(|b| b["text"].as_str())?,
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<') && !text.starts_with("Caveat:"))
        .then(|| text.to_owned())
}
