//! The normalized [`Event`] (DESIGN.md §7), the [`Harness`] enum, and the canonical
//! model-id map (DESIGN.md Appendix B).

use serde::{Deserialize, Serialize};

/// Which AI coding-agent harness produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
    /// A ccflare-family proxy (better-ccflare / ccflare) — its `requests` table, not a harness log.
    Ccflare,
}

impl Harness {
    /// Stable string form (used as the SQLite column value and the id hash prefix).
    pub fn as_str(&self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
            Harness::Ccflare => "ccflare",
        }
    }

    /// Inverse of [`Harness::as_str`]; used when reading rows back from SQLite.
    pub fn from_tag(s: &str) -> Option<Harness> {
        match s {
            "claude-code" => Some(Harness::ClaudeCode),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::OpenCode),
            "ccflare" => Some(Harness::Ccflare),
            _ => None,
        }
    }
}

/// One assistant turn, collapsed by `message.id` and normalized across harnesses.
///
/// The `id` is an idempotent content key (never a positional index), so a boot-scan, a
/// live-tail delta, and a full re-scan all converge to the same row (DESIGN.md §15.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// `blake3(harness, session_id | agent_id, message_id)`, hex.
    pub id: String,
    /// Epoch milliseconds, parsed from the source's ISO-8601 timestamp.
    pub ts: i64,
    pub harness: Harness,
    /// Git toplevel or cwd of the session (M1: cwd).
    pub project: String,
    /// `"main"` for the main thread; the sub-agent's `agentType` otherwise.
    pub agent: String,
    pub is_subagent: bool,
    /// Canonical model id (Appendix B); unknown ids pass through and are flagged `unpriced`.
    pub model: String,
    pub session_id: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    /// Kept separate from `cache_write_1h` — billed at different rates (DESIGN.md §8).
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub duration_ms: Option<u64>,
    /// "lines edited by agent (≠ committed diff)" — DESIGN.md §8.
    pub loc_added: u32,
    pub loc_removed: u32,
    /// True if any edit in this turn errored (excluded from the headline LOC).
    pub loc_failed: bool,
    pub skills: Vec<String>,
    /// Computed at ingest from `pricing.toml`; 0.0 for local models.
    pub cost_eur: f64,
    /// Model id missing from the pricing map → cost is a floor, surface loudly.
    pub unpriced: bool,
}

/// Map a raw model id to its canonical form (DESIGN.md Appendix B).
///
/// - Claude `…-<major>-<minor>[-<date>]` → `…-<major>.<minor>` (`claude-opus-4-8` →
///   `claude-opus-4.8`; the dated snapshot `claude-haiku-4-5-20251001` → `claude-haiku-4.5`).
/// - `gpt-5.5*` → `gpt-5.5`.
/// - Anything else passes through unchanged (and will be flagged `unpriced` if not in the map).
pub fn canonicalize_model(raw: &str) -> String {
    if raw.starts_with("gpt-5.5") {
        return "gpt-5.5".to_string();
    }
    if raw.starts_with("claude-") {
        // claude-<family>-<major>-<minor>[-<date>] -> claude-<family>-<major>.<minor>
        // (drop any trailing dated-snapshot segment, e.g. `-20251001`).
        let parts: Vec<&str> = raw.split('-').collect();
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if parts.len() >= 4 && numeric(parts[2]) && numeric(parts[3]) {
            return format!("claude-{}-{}.{}", parts[1], parts[2], parts[3]);
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_claude_ids() {
        assert_eq!(canonicalize_model("claude-opus-4-8"), "claude-opus-4.8");
        assert_eq!(canonicalize_model("claude-sonnet-4-6"), "claude-sonnet-4.6");
        assert_eq!(canonicalize_model("claude-haiku-4-5"), "claude-haiku-4.5");
        // Dated snapshots collapse to the same canonical id (DESIGN.md Appendix B intent).
        assert_eq!(
            canonicalize_model("claude-haiku-4-5-20251001"),
            "claude-haiku-4.5"
        );
        // Only a major-minor pair → unchanged (no numeric pair at [2],[3]).
        assert_eq!(canonicalize_model("claude-fable-5"), "claude-fable-5");
    }

    #[test]
    fn canonicalizes_others_and_passthrough() {
        assert_eq!(canonicalize_model("gpt-5.5-codex"), "gpt-5.5");
        assert_eq!(
            canonicalize_model("some-unknown-model"),
            "some-unknown-model"
        );
    }

    #[test]
    fn harness_str_roundtrip() {
        for h in [
            Harness::ClaudeCode,
            Harness::Codex,
            Harness::OpenCode,
            Harness::Ccflare,
        ] {
            assert_eq!(Harness::from_tag(h.as_str()), Some(h));
        }
    }
}
