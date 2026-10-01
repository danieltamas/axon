//! Codex hooks in `config.toml`. Codex reads matcher groups, each holding its handlers:
//! `Event = [{ hooks = [{ type = "command", command = "…" }] }]`. A flat
//! `Event = [{ command = "…" }]` entry parses but never runs, which is what older Axon
//! versions wrote; install upgrades those in place.

use anyhow::Context;
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::install::{command, is_bus_command, CODEX_EVENTS};
use crate::Harness;

/// The entries of an event's list (or of a group's `hooks` list), as `[[…]]` tables or
/// inline tables.
fn entries_mut(item: &mut Item) -> Vec<&mut dyn TableLike> {
    match item {
        Item::ArrayOfTables(tables) => tables.iter_mut().map(|t| t as &mut dyn TableLike).collect(),
        Item::Value(Value::Array(array)) => array
            .iter_mut()
            .filter_map(Value::as_inline_table_mut)
            .map(|t| t as &mut dyn TableLike)
            .collect(),
        _ => Vec::new(),
    }
}

fn entries(item: &Item) -> Vec<&dyn TableLike> {
    match item {
        Item::ArrayOfTables(tables) => tables.iter().map(|t| t as &dyn TableLike).collect(),
        Item::Value(Value::Array(array)) => array
            .iter()
            .filter_map(Value::as_inline_table)
            .map(|t| t as &dyn TableLike)
            .collect(),
        _ => Vec::new(),
    }
}

fn runs(entry: &dyn TableLike, event: &str) -> bool {
    entry
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|c| is_bus_command(c, Harness::Codex, event))
}

fn handler(cmd: &str) -> Item {
    let mut hook = InlineTable::new();
    hook.insert("type", "command".into());
    hook.insert("command", cmd.into());
    let mut hooks = Array::new();
    hooks.push(hook);
    value(hooks)
}

/// Keep the entries of `item` for which `keep` holds, in order over the table entries.
fn retain_entries(item: &mut Item, mut keep: impl FnMut(&dyn TableLike) -> bool) {
    match item {
        Item::ArrayOfTables(tables) => tables.retain(|t| keep(t)),
        Item::Value(Value::Array(array)) => {
            array.retain(|v| v.as_inline_table().is_none_or(|t| keep(t)));
        }
        _ => {}
    }
}

fn is_empty(item: &Item) -> bool {
    match item {
        Item::ArrayOfTables(tables) => tables.is_empty(),
        Item::Value(Value::Array(array)) => array.is_empty(),
        _ => false,
    }
}

/// `original` with the bus hooks running `exe` on every Codex event.
pub(crate) fn wire(original: Option<&str>, exe: &str) -> anyhow::Result<String> {
    let mut doc: DocumentMut = original.unwrap_or("").parse()?;
    let hooks = doc
        .entry("hooks")
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .context("config.toml `hooks` is not a table")?;
    for event in CODEX_EVENTS {
        let cmd = command(exe, Harness::Codex, event);
        let item = hooks.entry(event).or_insert(value(Array::new()));
        let mut wired = false;
        for group in entries_mut(item) {
            if runs(group, event) {
                // The flat shape Codex ignores: its group is rewritten where it stands.
                group.remove("command");
                group.insert("hooks", handler(&cmd));
                wired = true;
            } else if let Some(inner) = group.get_mut("hooks") {
                for hook in entries_mut(inner).into_iter().filter(|h| runs(*h, event)) {
                    hook.insert("type", value("command"));
                    hook.insert("command", value(cmd.as_str()));
                    wired = true;
                }
            }
        }
        if wired {
            continue;
        }
        if let Some(tables) = item.as_array_of_tables_mut() {
            let mut group = Table::new();
            group.insert("hooks", handler(&cmd));
            tables.push(group);
        } else if let Some(array) = item.as_array_mut() {
            let mut group = InlineTable::new();
            group.insert("hooks", handler(&cmd).into_value().expect("an array value"));
            array.push(group);
        } else {
            anyhow::bail!("config.toml `hooks.{event}` is not an array");
        }
    }
    Ok(doc.to_string())
}

/// `current` without the bus hooks (flat or nested); everything else is left as it was.
pub(crate) fn unwire(current: &str) -> anyhow::Result<String> {
    let mut doc: DocumentMut = current.parse()?;
    if let Some(hooks) = doc.get_mut("hooks").and_then(Item::as_table_like_mut) {
        for event in CODEX_EVENTS {
            let Some(item) = hooks.get_mut(event) else {
                continue;
            };
            let had_entries = !entries(item).is_empty();
            // A group whose last handler was ours goes with it; the user's own groups stay.
            let mut emptied_by_us = Vec::new();
            for group in entries_mut(item) {
                let flat = runs(group, event);
                let mut emptied = false;
                if let Some(inner) = group.get_mut("hooks") {
                    let before = entries(inner).len();
                    retain_entries(inner, |h| !runs(h, event));
                    emptied = before > 0 && is_empty(inner);
                }
                emptied_by_us.push(flat || emptied);
            }
            let mut index = 0;
            retain_entries(item, |_| {
                index += 1;
                !emptied_by_us[index - 1]
            });
            if had_entries && is_empty(item) {
                hooks.remove(event);
            }
        }
        if hooks.is_empty() {
            doc.remove("hooks");
        }
    }
    Ok(doc.to_string())
}

/// Whether every Codex event has a matcher group whose handler runs exactly `exe`.
pub(crate) fn is_wired(config: &str, exe: &str) -> bool {
    let Ok(doc) = config.parse::<DocumentMut>() else {
        return false;
    };
    CODEX_EVENTS.iter().all(|event| {
        let cmd = command(exe, Harness::Codex, event);
        doc.get("hooks")
            .and_then(|h| h.get(event))
            .is_some_and(|item| {
                entries(item)
                    .iter()
                    .filter_map(|group| group.get("hooks"))
                    .flat_map(entries)
                    .any(|hook| {
                        hook.get("type").and_then(Item::as_str) == Some("command")
                            && hook.get("command").and_then(Item::as_str) == Some(cmd.as_str())
                    })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/bin/axon";
    const OWN: &str =
        "# keep\n[hooks]\nStop = [{ hooks = [{ type = \"command\", command = \"mine\" }] }]\n";

    fn flat(exe: &str) -> String {
        let mut text = String::from("model = \"x\" # note\n\n[hooks]\n");
        for event in CODEX_EVENTS {
            let cmd = command(exe, Harness::Codex, event);
            text.push_str(&format!("{event} = [{{ command = \"{cmd}\" }}]\n"));
        }
        text.push_str("\n[hooks.state]\n");
        text
    }

    #[test]
    fn wire_writes_matcher_groups_codex_reads() {
        let text = wire(None, EXE).unwrap();
        let doc: DocumentMut = text.parse().unwrap();
        let hook = &doc["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(hook["type"].as_str(), Some("command"));
        assert!(hook["command"]
            .as_str()
            .unwrap()
            .ends_with("bus hook codex Stop"));
        assert!(is_wired(&text, EXE));
        assert_eq!(wire(Some(&text), EXE).unwrap(), text);
    }

    #[test]
    fn wire_upgrades_a_flat_entry_in_place_without_duplicates() {
        let old = flat("/old/axon");
        assert!(!is_wired(&old, "/old/axon"));
        let text = wire(Some(&old), EXE).unwrap();
        assert!(is_wired(&text, EXE));
        assert!(text.contains("model = \"x\" # note"));
        assert!(text.contains("[hooks.state]"));
        assert_eq!(text.matches("bus hook codex Stop").count(), 1);
        assert_eq!(wire(Some(&text), EXE).unwrap(), text);
    }

    #[test]
    fn wire_and_unwire_keep_the_owners_own_hooks() {
        let text = wire(Some(OWN), EXE).unwrap();
        assert!(text.contains("command = \"mine\""));
        assert_eq!(text.matches("bus hook codex Stop").count(), 1);
        assert_eq!(unwire(&text).unwrap(), OWN);
    }

    #[test]
    fn unwire_removes_flat_entries() {
        assert_eq!(
            unwire(&flat(EXE)).unwrap(),
            "model = \"x\" # note\n\n[hooks]\n\n[hooks.state]\n"
        );
    }

    #[test]
    fn wire_handles_array_of_tables_groups() {
        let own = "[[hooks.Stop]]\nmatcher = \"x\"\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"mine\"\n";
        let text = wire(Some(own), EXE).unwrap();
        assert!(is_wired(&text, EXE));
        let back = unwire(&text).unwrap();
        assert!(back.contains("command = \"mine\"") && !back.contains("bus hook"));
    }
}
