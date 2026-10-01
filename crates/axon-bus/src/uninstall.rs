//! `uninstall`: take the bus hooks back out of each harness's config. An untouched config
//! gets its pre-install backup back byte for byte; one the user edited after install
//! loses only the bus hooks, wherever the binary that installed them lived.

use std::fs;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::install::{
    backup_path, current_exe, edited_since_install, is_bus_command, is_codex_bus_entry, layout,
    read_optional, wire, Layout, CLAUDE_EVENTS, CODEX_EVENTS, HERMES_MARKER,
};
use crate::Harness;

pub fn uninstall(harness: Harness) -> anyhow::Result<()> {
    let exe = current_exe()?;
    let layout = layout(harness);
    let backup = backup_path(&layout.config);
    let current = read_optional(&layout.config)?;
    if let Some(original) = read_optional(&backup)? {
        match current.as_deref() {
            // Edited after install: take out only the bus hooks, keep the edits.
            Some(live) if edited_since_install(harness, Some(live), &original, &exe, &layout)? => {
                let unwired = unwire(harness, live, &layout)
                    .with_context(|| format!("unwire {}", layout.config.display()))?;
                fs::write(&layout.config, unwired)?;
            }
            _ => fs::write(&layout.config, original)?,
        }
        fs::remove_file(&backup)?;
    } else if current.is_some() && current == Some(wire(harness, None, &exe, &layout)?) {
        fs::remove_file(&layout.config)?;
    }
    if let Some(plugin) = &layout.plugin {
        if plugin.exists() {
            fs::remove_file(plugin)?;
        }
        if let Some(dir) = plugin.parent() {
            // Only removes the directory when the installer's shim was all it held.
            let _ = fs::remove_dir(dir);
        }
    }
    Ok(())
}

/// `current` without the bus hooks, keeping every edit the user made after install.
/// Lists and tables the bus created are dropped once they are empty again.
pub(crate) fn unwire(harness: Harness, current: &str, layout: &Layout) -> anyhow::Result<String> {
    match harness {
        Harness::Claude => {
            let mut settings: Value = serde_json::from_str(current)?;
            if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
                for event in CLAUDE_EVENTS {
                    let Some(entries) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
                        continue;
                    };
                    let before = entries.len();
                    for entry in entries.iter_mut() {
                        if let Some(list) = entry["hooks"].as_array_mut() {
                            list.retain(|h| {
                                !h["command"]
                                    .as_str()
                                    .is_some_and(|c| is_bus_command(c, harness, event))
                            });
                        }
                    }
                    entries.retain(|e| e["hooks"].as_array().is_none_or(|list| !list.is_empty()));
                    if entries.is_empty() && before > 0 {
                        hooks.remove(event);
                    }
                }
                if hooks.is_empty() {
                    settings.as_object_mut().map(|s| s.remove("hooks"));
                }
            }
            Ok(serde_json::to_string_pretty(&settings)? + "\n")
        }
        Harness::Codex => {
            let mut doc: toml_edit::DocumentMut = current.parse()?;
            if let Some(hooks) = doc.get_mut("hooks").and_then(|h| h.as_table_like_mut()) {
                for event in CODEX_EVENTS {
                    let Some(item) = hooks.get_mut(event) else {
                        continue;
                    };
                    let emptied = if let Some(tables) = item.as_array_of_tables_mut() {
                        tables.retain(|t| !is_codex_bus_entry(t, harness, event));
                        tables.is_empty()
                    } else if let Some(entries) = item.as_array_mut() {
                        let before = entries.len();
                        entries.retain(|e| {
                            !e.as_inline_table()
                                .is_some_and(|t| is_codex_bus_entry(t, harness, event))
                        });
                        entries.is_empty() && before > 0
                    } else {
                        false
                    };
                    if emptied {
                        hooks.remove(event);
                    }
                }
                if hooks.is_empty() {
                    doc.remove("hooks");
                }
            }
            Ok(doc.to_string())
        }
        Harness::Opencode => {
            let mut config: Value = serde_json::from_str(current)?;
            let plugin = layout.plugin.as_ref().expect("opencode has a plugin shim");
            let url = json!(format!("file://{}", plugin.display()));
            if let Some(plugins) = config.get_mut("plugin").and_then(Value::as_array_mut) {
                plugins.retain(|p| *p != url);
                if plugins.is_empty() {
                    config.as_object_mut().map(|c| c.remove("plugin"));
                }
            }
            Ok(serde_json::to_string_pretty(&config)? + "\n")
        }
        Harness::Hermes => {
            // Without the marker the hooks were merged into the owner's own block.
            let Some(start) = current.find(HERMES_MARKER) else {
                return Ok(crate::hermes_hooks::unwire(current));
            };
            let body = &current[start + HERMES_MARKER.len()..];
            let Some(hooks) = body.strip_prefix("hooks:\n") else {
                bail!("config.yaml: the axon-bus hooks block was edited; remove it by hand");
            };
            let block_len = hooks
                .split_inclusive('\n')
                .take_while(|line| line.starts_with(' '))
                .map(str::len)
                .sum::<usize>();
            let end = current.len() - hooks.len() + block_len;
            Ok(format!("{}{}", &current[..start], &current[end..]))
        }
    }
}
