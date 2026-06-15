//! Lines-edited and skills extraction from a collapsed turn's tool-use blocks (DESIGN.md §8).
//!
//! "lines edited by agent (≠ committed diff)": counted once per `message.id`, only when the
//! edit's `tool_result` did not error. `Write` over an existing file is a rewrite, not
//! all-new — without prior content we record its line count and label it accordingly upstream.

use std::collections::HashSet;

use serde_json::Value;

/// Result of scanning one turn's unioned `content[]`.
#[derive(Debug, Default, PartialEq)]
pub struct LocResult {
    pub added: u32,
    pub removed: u32,
    /// True if at least one edit in this turn errored (its lines are excluded).
    pub failed: bool,
    pub skills: Vec<String>,
}

/// Extract LOC + skills from a turn's content blocks. `failed_ids` is the set of
/// `tool_use` ids whose `tool_result` reported an error (absent ⇒ treated as success).
pub fn extract(content: &[Value], failed_ids: &HashSet<String>) -> LocResult {
    let mut out = LocResult::default();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
        let input = block.get("input").unwrap_or(&Value::Null);

        if name == "Skill" {
            if let Some(s) = input.get("skill").and_then(Value::as_str) {
                out.skills.push(s.to_string());
            }
            continue;
        }
        if !is_edit_tool(name) {
            continue;
        }
        if failed_ids.contains(id) {
            out.failed = true; // errored edit: flag it, don't count its lines
            continue;
        }
        let (a, r) = edit_loc(name, input);
        out.added += a;
        out.removed += r;
    }
    out
}

fn is_edit_tool(name: &str) -> bool {
    // apply_patch (Codex) is deferred to M2.
    matches!(name, "Edit" | "MultiEdit" | "Write")
}

fn edit_loc(name: &str, input: &Value) -> (u32, u32) {
    match name {
        "Edit" => {
            let old = input
                .get("old_string")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let new = input
                .get("new_string")
                .and_then(Value::as_str)
                .unwrap_or_default();
            line_diff(old, new)
        }
        "MultiEdit" => {
            let mut added = 0;
            let mut removed = 0;
            if let Some(edits) = input.get("edits").and_then(Value::as_array) {
                for e in edits {
                    let old = e
                        .get("old_string")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let new = e
                        .get("new_string")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let (a, r) = line_diff(old, new);
                    added += a;
                    removed += r;
                }
            }
            (added, removed)
        }
        "Write" => {
            let content = input
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (count_lines(content), 0)
        }
        _ => (0, 0),
    }
}

/// Multiset line difference: added = lines in `new` not matched in `old`, and vice versa.
/// `"a"` → `"a\nb\nc"` yields `(2, 0)` (the shared `"a"` is not counted).
fn line_diff(old: &str, new: &str) -> (u32, u32) {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for l in old.lines() {
        *counts.entry(l).or_insert(0) -= 1;
    }
    for l in new.lines() {
        *counts.entry(l).or_insert(0) += 1;
    }
    let mut added = 0u32;
    let mut removed = 0u32;
    for c in counts.values() {
        if *c > 0 {
            added += *c as u32;
        } else if *c < 0 {
            removed += (-*c) as u32;
        }
    }
    (added, removed)
}

fn count_lines(s: &str) -> u32 {
    s.lines().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit_block(id: &str, old: &str, new: &str) -> Value {
        json!({"type":"tool_use","id":id,"name":"Edit","input":{"old_string":old,"new_string":new}})
    }

    #[test]
    fn edit_multiset_diff() {
        let content = vec![edit_block("toolu_A", "a", "a\nb\nc")];
        let r = extract(&content, &HashSet::new());
        assert_eq!(r.added, 2);
        assert_eq!(r.removed, 0);
        assert!(!r.failed);
    }

    #[test]
    fn failed_edit_not_counted() {
        let content = vec![edit_block("toolu_A", "a", "a\nb\nc")];
        let failed: HashSet<String> = ["toolu_A".to_string()].into_iter().collect();
        let r = extract(&content, &failed);
        assert_eq!(r.added, 0);
        assert!(r.failed);
    }

    #[test]
    fn collects_skill_names() {
        let content =
            vec![json!({"type":"tool_use","id":"t","name":"Skill","input":{"skill":"verify"}})];
        let r = extract(&content, &HashSet::new());
        assert_eq!(r.skills, vec!["verify".to_string()]);
    }
}
