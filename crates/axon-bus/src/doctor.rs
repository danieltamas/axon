//! `doctor`, and the check startup uses to decide whether hooks need re-wiring.

use std::path::Path;

use serde_json::{json, Value};

use crate::install::{
    command, current_exe, detected, layout, read_optional, CLAUDE_EVENTS, CODEX_EVENTS,
    HERMES_EVENTS,
};
use crate::opencode_plugin::plugin_source;
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
        Harness::Hermes => HERMES_EVENTS
            .iter()
            .all(|event| crate::hermes_hooks::has_hook(&config, event, &runs(event))),
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
    println!("{}", federation_line(db));
    Ok(healthy)
}

/// `federation: off | on (<n> peers)`; an unreadable hub reads as off, `hub` above says why.
fn federation_line(db: &Path) -> String {
    let Ok(conn) = crate::store::open(db) else {
        return "federation: off".into();
    };
    if !crate::fed::enabled(&conn) {
        return "federation: off".into();
    }
    let peers = crate::fed::live_peer_count(&conn).unwrap_or(0);
    format!("federation: on ({peers} peers)")
}

#[cfg(test)]
mod federation_line_tests {
    use super::*;

    #[test]
    fn reports_off_then_on_with_the_peer_count() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axon.db");
        let conn = crate::store::init(&db).unwrap();
        assert_eq!(federation_line(&db), "federation: off");
        crate::fed::enable(dir.path(), &conn, true).unwrap();
        assert_eq!(federation_line(&db), "federation: on (0 peers)");
        conn.execute(
            "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at)
             VALUES ('p','n','alice',1,'active',1)",
            [],
        )
        .unwrap();
        assert_eq!(federation_line(&db), "federation: on (1 peers)");
    }
}

#[cfg(test)]
mod acceptance_review2 {
    use super::*;
    use std::{ffi::OsString, fs, sync::Mutex};

    static ENV: Mutex<()> = Mutex::new(());

    struct IsolatedEnv {
        saved: Vec<(&'static str, Option<OsString>)>,
        _temp: tempfile::TempDir,
    }

    impl IsolatedEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let mut saved = Vec::new();
            for name in [
                "CLAUDE_CONFIG_DIR",
                "XDG_CONFIG_HOME",
                "CODEX_HOME",
                "HERMES_HOME",
            ] {
                let path = temp.path().join(name);
                fs::create_dir(&path).unwrap();
                saved.push((name, std::env::var_os(name)));
                std::env::set_var(name, path);
            }
            Self { saved, _temp: temp }
        }
    }

    impl Drop for IsolatedEnv {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn r9_claude_missing_pretooluse_is_not_wired() {
        let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _env = IsolatedEnv::new();
        let layout = layout(Harness::Claude);
        let text = crate::install::wire(Harness::Claude, None, "axon", &layout).unwrap();
        fs::write(&layout.config, &text).unwrap();
        assert!(is_wired(Harness::Claude, "axon").unwrap());
        let mut config: Value = serde_json::from_str(&text).unwrap();
        config["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("PreToolUse");
        assert!(config["hooks"]["SessionStart"].is_array());
        fs::write(&layout.config, config.to_string()).unwrap();
        assert!(!is_wired(Harness::Claude, "axon").unwrap());
    }

    #[test]
    fn r9_opencode_unregistered_shim_is_not_wired() {
        let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _env = IsolatedEnv::new();
        let layout = layout(Harness::Opencode);
        let plugin = layout.plugin.as_ref().unwrap();
        fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        fs::write(plugin, plugin_source("axon")).unwrap();
        fs::write(&layout.config, "{}").unwrap();
        assert!(!is_wired(Harness::Opencode, "axon").unwrap());
        let text = crate::install::wire(Harness::Opencode, Some("{}"), "axon", &layout).unwrap();
        fs::write(&layout.config, text).unwrap();
        assert!(is_wired(Harness::Opencode, "axon").unwrap());
    }

    #[test]
    fn r9_commented_hermes_hooks_are_not_wired() {
        let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _env = IsolatedEnv::new();
        let layout = layout(Harness::Hermes);
        let hooks = crate::install::wire(Harness::Hermes, None, "axon", &layout).unwrap();
        let commented = hooks
            .lines()
            .map(|line| format!("# {line}\n"))
            .collect::<String>();
        fs::write(&layout.config, commented).unwrap();
        assert!(!is_wired(Harness::Hermes, "axon").unwrap());
    }
}
