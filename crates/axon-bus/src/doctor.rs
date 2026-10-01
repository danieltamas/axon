//! `doctor`, and the check startup uses to decide whether hooks need re-wiring.

use std::path::Path;

use serde_json::{json, Value};

use crate::install::{
    command, current_exe, detected, layout, plugin_source, read_optional, CLAUDE_EVENTS,
    CODEX_EVENTS, HERMES_EVENTS,
};
use crate::Harness;

/// Whether every hook `harness` needs runs `exe`: one missing event (say a removed
/// PreToolUse) leaves tool gating off, so startup must repair it. OpenCode needs both the
/// shim naming the binary and its registration in the config.
pub fn is_wired(harness: Harness, exe: &str) -> anyhow::Result<bool> {
    let layout = layout(harness);
    let Some(config) = read_optional(&layout.config)? else {
        return Ok(false);
    };
    let runs = |event: &str| command(exe, harness, event);
    Ok(match harness {
        Harness::Claude => serde_json::from_str::<Value>(&config).is_ok_and(|settings| {
            CLAUDE_EVENTS.iter().all(|event| {
                settings["hooks"][event].as_array().is_some_and(|entries| {
                    entries
                        .iter()
                        .filter_map(|e| e["hooks"].as_array())
                        .flatten()
                        .any(|h| h["command"].as_str() == Some(runs(event).as_str()))
                })
            })
        }),
        Harness::Codex => config.parse::<toml_edit::DocumentMut>().is_ok_and(|doc| {
            CODEX_EVENTS.iter().all(|event| {
                let cmd = runs(event);
                let runs_cmd = |entry: &dyn toml_edit::TableLike| {
                    entry.get("command").and_then(|c| c.as_str()) == Some(cmd.as_str())
                };
                match doc.get("hooks").and_then(|h| h.get(event)) {
                    Some(toml_edit::Item::ArrayOfTables(tables)) => {
                        tables.iter().any(|t| runs_cmd(t))
                    }
                    Some(item) => item.as_array().is_some_and(|entries| {
                        entries
                            .iter()
                            .filter_map(|e| e.as_inline_table())
                            .any(|t| runs_cmd(t))
                    }),
                    None => false,
                }
            })
        }),
        Harness::Opencode => {
            let plugin = layout.plugin.as_ref().expect("opencode has a plugin shim");
            let url = format!("file://{}", plugin.display());
            let registered = serde_json::from_str::<Value>(&config).is_ok_and(|c| {
                c["plugin"]
                    .as_array()
                    .is_some_and(|p| p.contains(&json!(url)))
            });
            registered && read_optional(plugin)?.is_some_and(|text| text == plugin_source(exe))
        }
        Harness::Hermes => HERMES_EVENTS.iter().all(|event| {
            config.contains(&format!("- command: '{}'", runs(event).replace('\'', "''")))
        }),
    })
}

/// Print one line per harness and the hub; true when every detected harness is wired to
/// this binary.
pub fn doctor(db: &Path) -> anyhow::Result<bool> {
    let exe = current_exe()?;
    let mut healthy = true;
    for harness in detected() {
        let layout = layout(harness);
        let config = read_optional(&layout.config)?.unwrap_or_default();
        let (state, detail) = if is_wired(harness, &exe)? {
            ("ok", "hooks point at this binary")
        } else if config.contains("axon-bus") || config.contains("axon bus hook") {
            healthy = false;
            (
                "warn",
                "hooks point at another axon binary; run `axon bus install`",
            )
        } else {
            healthy = false;
            ("missing", "not wired; run `axon bus install`")
        };
        println!(
            "{state:<8}{:<10}{} ({detail})",
            harness.as_str(),
            layout.config.display()
        );
        if harness == Harness::Hermes && state == "ok" {
            println!("note    hermes    Hermes asks to approve each hook command on first use");
        }
    }
    match crate::store::open(db)
        .and_then(|c| Ok(c.query_row("SELECT count(*) FROM agents", [], |r| r.get::<_, i64>(0))?))
    {
        Ok(agents) => println!("ok      hub       {} ({agents} agents)", db.display()),
        Err(_) => {
            healthy = false;
            println!(
                "missing hub       {} (hooks stay inert; run `axon bus init`)",
                db.display()
            );
        }
    }
    Ok(healthy)
}
