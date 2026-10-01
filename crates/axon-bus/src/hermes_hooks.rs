//! The bus hooks inside a Hermes `hooks:` block the owner already has (other tools'
//! hooks live there too). Edited as text so the owner's formatting and comments stay:
//! one `- command: '…'` line per event, under the event's key, added or taken out.

use crate::install::{command, is_bus_command, HERMES_EVENTS};
use crate::Harness;

pub(crate) const HERMES_MARKER: &str = "# axon-bus hooks; `axon-bus uninstall` removes them\n";

pub(crate) fn hermes_block(exe: &str) -> String {
    let mut block = format!("{HERMES_MARKER}hooks:\n");
    for event in HERMES_EVENTS {
        let cmd = command(exe, Harness::Hermes, event).replace('\'', "''");
        block.push_str(&format!("  {event}:\n    - command: '{cmd}'\n"));
    }
    block
}

/// `line` without a trailing ` # comment`, for matching keys; item lines are matched whole.
fn code(line: &str) -> &str {
    if line.trim_start().starts_with('#') {
        return "";
    }
    line.find(" #").map_or(line, |at| &line[..at]).trim_end()
}

/// Whether `line` holds YAML content rather than blank space or a comment.
fn content(line: &str) -> bool {
    !code(line).trim().is_empty()
}

/// The line range of the top-level `hooks:` block's body, if the config has one.
fn block(lines: &[&str]) -> Option<(usize, usize)> {
    let head = lines.iter().position(|l| code(l) == "hooks:")?;
    let end = (head + 1..lines.len())
        .find(|&i| content(lines[i]) && !lines[i].starts_with([' ', '\t']))
        .unwrap_or(lines.len());
    Some((head + 1, end))
}

/// Whether the config has a top-level `hooks` key in a form this editor does not merge
/// into (`hooks: {}`, a flow mapping, an anchor), where appending a block would duplicate it.
pub fn has_unmergeable_hooks(config: &str) -> bool {
    config
        .lines()
        .any(|l| l.starts_with("hooks:") && code(l) != "hooks:")
}

/// Whether the first content line after `at` holds items of the key on `at`, indented
/// deeper or as an indentless sequence at the key's own indent.
fn has_items(lines: &[&str], at: usize) -> bool {
    lines[at + 1..]
        .iter()
        .find(|l| content(l))
        .is_some_and(|next| {
            indent(next) > indent(lines[at])
                || (indent(next) == indent(lines[at]) && next.trim_start().starts_with("- "))
        })
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Whether `line` is a bus hook item, for any Hermes event.
fn is_bus_item(line: &str) -> bool {
    let Some(quoted) = line.trim().strip_prefix("- command: ") else {
        return false;
    };
    let command = quoted.trim_matches('\'').replace("''", "'");
    command
        .rsplit_once(' ')
        .is_some_and(|(_, event)| is_bus_command(&command, Harness::Hermes, event))
}

/// Whether `config` runs `command` for `event`: a live item under the event's key inside
/// the `hooks:` block, not a comment or a line elsewhere.
pub fn has_hook(config: &str, event: &str, command: &str) -> bool {
    let lines: Vec<&str> = config.lines().collect();
    let Some((start, end)) = block(&lines) else {
        return false;
    };
    let item = format!("- command: '{}'", command.replace('\'', "''"));
    let key = format!("{event}:");
    let is_key = |l: &str| code(l).trim() == key && !l.trim_start().starts_with('-');
    let Some(at) = (start..end).find(|&i| is_key(lines[i])) else {
        return false;
    };
    lines[at + 1..end]
        .iter()
        .filter(|l| content(l))
        .take_while(|l| indent(l) > indent(lines[at]) || l.trim_start().starts_with("- "))
        .any(|l| l.trim() == item)
}

/// `config` without the bus items, and without event keys left empty by removing them.
pub fn unwire(config: &str) -> String {
    let lines: Vec<&str> = config.split_inclusive('\n').collect();
    let Some((start, end)) = block(&lines) else {
        return config.to_owned();
    };
    let mut kept: Vec<&str> = lines[..start].to_vec();
    let body: Vec<&str> = lines[start..end]
        .iter()
        .copied()
        .filter(|l| !is_bus_item(l))
        .collect();
    for (i, line) in body.iter().enumerate() {
        let is_key = code(line).ends_with(':') && !line.trim().starts_with('-');
        if !is_key || has_items(&body, i) {
            kept.push(line);
        }
    }
    kept.extend_from_slice(&lines[end..]);
    kept.concat()
}

/// `config` with one bus item per `(event, command)` in its existing `hooks:` block,
/// replacing bus items already there. None when the config has no `hooks:` block.
pub fn wire(config: &str, hooks: &[(&str, String)]) -> Option<String> {
    let config = unwire(config);
    let mut lines: Vec<String> = config.split_inclusive('\n').map(str::to_owned).collect();
    if let Some(last) = lines.last_mut().filter(|l| !l.ends_with('\n')) {
        last.push('\n');
    }
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (start, mut end) = block(&borrowed)?;
    let body = &borrowed[start..end];
    // Comments may sit at any column; only content lines set the indents.
    let key_indent = body.iter().find(|l| content(l)).map_or(2, |l| indent(l));
    let item_indent = body
        .iter()
        .find(|l| content(l) && l.trim().starts_with("- "))
        .map_or(key_indent + 2, |l| indent(l));
    let (keys, items) = (" ".repeat(key_indent), " ".repeat(item_indent));
    for (event, command) in hooks {
        let item = format!("{items}- command: '{}'\n", command.replace('\'', "''"));
        let key = format!("{keys}{event}:");
        match (start..end).find(|&i| code(&lines[i]) == key) {
            Some(at) => {
                // After the key's last item: the next content line that is not one of its
                // items (shallower, or a sibling key at the same indent).
                let after = (at + 1..end)
                    .find(|&i| {
                        let line = &lines[i];
                        content(line)
                            && (indent(line) < key_indent
                                || (indent(line) == key_indent
                                    && !line.trim_start().starts_with("- ")))
                    })
                    .unwrap_or(end);
                lines.insert(after, item);
                end += 1;
            }
            None => {
                lines.insert(end, item);
                lines.insert(end, format!("{key}\n"));
                end += 2;
            }
        }
    }
    Some(lines.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "model: x\nhooks:\n    on_session_start:\n        - command: 'beacon start'\n    post_tool_call:\n        - matcher: .*\n          command: 'beacon post'\n          timeout: 10\nother: 1\n";

    fn hooks() -> Vec<(&'static str, String)> {
        vec![
            (
                "on_session_start",
                "/usr/local/bin/axon bus hook hermes on_session_start".into(),
            ),
            (
                "post_tool_call",
                "/usr/local/bin/axon bus hook hermes post_tool_call".into(),
            ),
            (
                "subagent_start",
                "/usr/local/bin/axon bus hook hermes subagent_start".into(),
            ),
        ]
    }

    #[test]
    fn merges_beside_other_hooks_and_round_trips() {
        let wired = wire(OWNER, &hooks()).unwrap();
        assert!(wired.contains("        - command: 'beacon start'\n        - command: '/usr/local/bin/axon bus hook hermes on_session_start'\n"));
        assert!(wired.contains("          timeout: 10\n        - command: '/usr/local/bin/axon bus hook hermes post_tool_call'\n"));
        assert!(wired.contains("    subagent_start:\n        - command: '/usr/local/bin/axon bus hook hermes subagent_start'\nother: 1\n"));
        assert_eq!(wire(&wired, &hooks()).unwrap(), wired, "idempotent");
        assert_eq!(unwire(&wired), OWNER, "uninstall restores the owner's text");
    }

    #[test]
    fn no_block_is_left_to_the_marker_path() {
        assert_eq!(wire("model: x\n", &hooks()), None);
    }
}

#[cfg(test)]
mod acceptance_review2 {
    use super::*;

    const INDENTLESS: &str = "model: x\nhooks: # owner hooks\n  on_session_start:\n  - command: 'beacon start'\n  pre_tool_call:\n  - command: 'beacon check'\nother: 1\n";

    fn hooks() -> Vec<(&'static str, String)> {
        ["on_session_start", "pre_tool_call", "on_session_end"]
            .iter()
            .map(|event| (*event, format!("axon bus hook hermes {event}")))
            .collect()
    }

    #[test]
    fn r4_commented_hooks_merge_once() {
        let wired = wire(INDENTLESS, &hooks()).expect("commented hooks must be mergeable");
        assert_eq!(
            wired
                .lines()
                .filter(|line| line.starts_with("hooks:"))
                .count(),
            1
        );
        assert!(wired.contains("hooks: # owner hooks"));
        for (event, command) in hooks() {
            assert_eq!(wired.matches(&format!("  {event}:\n")).count(), 1);
            assert!(wired.contains(&command));
        }
    }

    #[test]
    fn r4_unwire_retains_indentless_event_keys() {
        let wired = "hooks:\n  pre_tool_call:\n  - command: 'beacon check'\n  - command: 'axon bus hook hermes pre_tool_call'\nother: 1\n";
        assert_eq!(
            unwire(wired),
            "hooks:\n  pre_tool_call:\n  - command: 'beacon check'\nother: 1\n"
        );
    }

    #[test]
    fn r4_indentless_wire_unwire_restores_original() {
        assert_eq!(unwire(&wire(INDENTLESS, &hooks()).unwrap()), INDENTLESS);
    }

    #[test]
    fn r4_top_level_comment_inside_hooks_preserves_mapping_and_roundtrip() {
        let original = "hooks: # owner hooks\n# Owner documentation\n  on_session_start:\n  - command: 'beacon start'\nother: 1\n";
        let wired = wire(original, &hooks()).unwrap();
        assert!(
            wired.contains("  pre_tool_call:\n"),
            "event escaped hooks mapping:\n{wired}"
        );
        assert_eq!(unwire(&wired), original);
    }

    #[test]
    fn r4_plain_hooks_comment_preserves_existing_owner_event() {
        let original =
            "hooks:\n# Owner documentation\n  owner_event:\n    - command: 'beacon'\nother: 1\n";
        let hooks = [(
            "pre_tool_call",
            "axon bus hook hermes pre_tool_call".to_owned(),
        )];
        let wired = wire(original, &hooks).unwrap();
        assert!(
            wired.contains("  pre_tool_call:\n"),
            "event escaped hooks mapping:\n{wired}"
        );
        assert_eq!(unwire(&wired), original);
    }
}
