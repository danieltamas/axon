//! `install` / `uninstall` / `doctor`: wire `axon-bus hook` into each harness's own config
//! (BUS-PLAN §00: a plug-in, never a wrapper).
//!
//! Each config is rewritten from its pristine copy — the `<file>.axon-bus.bak` backup once
//! one exists — so installing again produces the same bytes. Uninstall puts the backup back.
//! A config the user edited after install is wired or unwired in place instead, so those
//! edits are never rolled back.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{json, Value};

use crate::Harness;

const BACKUP_SUFFIX: &str = ".axon-bus.bak";

pub(crate) const CLAUDE_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "SessionEnd",
];
pub(crate) const CODEX_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];
const HERMES_EVENTS: [&str; 8] = [
    "on_session_start",
    "pre_llm_call",
    "pre_tool_call",
    "post_tool_call",
    "post_llm_call",
    "subagent_start",
    "subagent_stop",
    "on_session_end",
];

/// Where a harness keeps what `install` touches.
pub(crate) struct Layout {
    pub(crate) config: PathBuf,
    /// A file the installer owns outright (the OpenCode plugin shim).
    pub(crate) plugin: Option<PathBuf>,
}

pub(crate) fn env_dir(var: &str, fallback: &[&str]) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        fallback.iter().fold(
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()),
            |dir, part| dir.join(part),
        )
    })
}

pub(crate) fn layout(harness: Harness) -> Layout {
    match harness {
        Harness::Claude => Layout {
            config: env_dir("CLAUDE_CONFIG_DIR", &[".claude"]).join("settings.json"),
            plugin: None,
        },
        Harness::Codex => Layout {
            config: env_dir("CODEX_HOME", &[".codex"]).join("config.toml"),
            plugin: None,
        },
        Harness::Opencode => {
            let dir = env_dir("XDG_CONFIG_HOME", &[".config"]).join("opencode");
            Layout {
                config: dir.join("opencode.json"),
                plugin: Some(dir.join("axon-bus").join("plugin.js")),
            }
        }
        Harness::Hermes => Layout {
            config: env_dir("HERMES_HOME", &[".hermes"]).join("config.yaml"),
            plugin: None,
        },
    }
}

pub(crate) fn backup_path(config: &Path) -> PathBuf {
    let mut name = config.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    config.with_file_name(name)
}

/// The installed binary, quoted for the shell-style command lines harnesses run.
fn command(exe: &str, harness: Harness, event: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c);
    let exe = if exe.chars().all(safe) {
        exe.to_owned()
    } else {
        format!("'{}'", exe.replace('\'', r"'\''"))
    };
    format!("{exe}{} hook {} {event}", bus_verb(&exe), harness.as_str())
}

/// `axon` runs the bus as a subcommand; the `axon-bus` alias takes the verb directly.
fn bus_verb(exe: &str) -> &'static str {
    let name = exe.trim_matches('\'').rsplit('/').next().unwrap_or_default();
    if name == "axon" || name == "axon.exe" {
        " bus"
    } else {
        ""
    }
}

/// Whether a configured hook command is this bus's hook for `event`, whichever binary
/// path installed it, so a reinstall or uninstall from another path still finds it.
pub(crate) fn is_bus_command(text: &str, harness: Harness, event: &str) -> bool {
    let Some(exe) = text.strip_suffix(&format!(" hook {} {event}", harness.as_str())) else {
        return false;
    };
    // `axon bus hook …`, `axon-bus hook …`, and the bare `axon hook …` an earlier
    // install wrote by mistake, so a reinstall repoints it.
    let exe = exe.strip_suffix(" bus").unwrap_or(exe).trim_matches('\'');
    let name = exe.rsplit('/').next().unwrap_or_default();
    name == "axon-bus" || name == "axon"
}

/// Whether a Codex hook entry (inline or `[[hooks.Event]]` table) runs this bus hook.
pub(crate) fn is_codex_bus_entry(
    entry: &dyn toml_edit::TableLike,
    harness: Harness,
    event: &str,
) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| is_bus_command(c, harness, event))
}

/// The config text with the bus hooks added to `original` (None: no config yet).
pub(crate) fn wire(
    harness: Harness,
    original: Option<&str>,
    exe: &str,
    layout: &Layout,
) -> anyhow::Result<String> {
    match harness {
        Harness::Claude => {
            let mut settings: Value = serde_json::from_str(original.unwrap_or("{}"))?;
            let hooks = settings
                .as_object_mut()
                .context("settings.json is not an object")?
                .entry("hooks")
                .or_insert_with(|| json!({}));
            for event in CLAUDE_EVENTS {
                let cmd = command(exe, harness, event);
                let entries = hooks
                    .as_object_mut()
                    .context("settings.json `hooks` is not an object")?
                    .entry(event)
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .with_context(|| format!("settings.json `hooks.{event}` is not a list"))?;
                // An existing bus hook is repointed in place, keeping the user's order.
                let mut wired = false;
                for hook in entries
                    .iter_mut()
                    .filter_map(|e| e["hooks"].as_array_mut())
                    .flatten()
                    .filter(|h| {
                        h["command"]
                            .as_str()
                            .is_some_and(|c| is_bus_command(c, harness, event))
                    })
                {
                    hook["command"] = json!(cmd);
                    wired = true;
                }
                if !wired {
                    entries.push(json!({"hooks": [{"type": "command", "command": cmd}]}));
                }
            }
            Ok(serde_json::to_string_pretty(&settings)? + "\n")
        }
        Harness::Codex => {
            let mut doc: toml_edit::DocumentMut = original.unwrap_or("").parse()?;
            let hooks = doc
                .entry("hooks")
                .or_insert(toml_edit::table())
                .as_table_like_mut()
                .context("config.toml `hooks` is not a table")?;
            for event in CODEX_EVENTS {
                let cmd = command(exe, harness, event);
                let item = hooks
                    .entry(event)
                    .or_insert(toml_edit::value(toml_edit::Array::new()));
                // A config editor may rewrite inline entries as `[[hooks.Event]]` tables.
                if let Some(tables) = item.as_array_of_tables_mut() {
                    let mut wired = false;
                    for entry in tables
                        .iter_mut()
                        .filter(|t| is_codex_bus_entry(*t, harness, event))
                    {
                        entry.insert("command", toml_edit::value(cmd.as_str()));
                        wired = true;
                    }
                    if !wired {
                        let mut entry = toml_edit::Table::new();
                        entry.insert("command", toml_edit::value(cmd));
                        tables.push(entry);
                    }
                    continue;
                }
                let entries = item
                    .as_array_mut()
                    .with_context(|| format!("config.toml `hooks.{event}` is not an array"))?;
                let mut wired = false;
                for entry in entries
                    .iter_mut()
                    .filter_map(|e| e.as_inline_table_mut())
                    .filter(|t| is_codex_bus_entry(*t, harness, event))
                {
                    entry.insert("command", cmd.as_str().into());
                    wired = true;
                }
                if !wired {
                    let mut entry = toml_edit::InlineTable::new();
                    entry.insert("command", cmd.into());
                    entries.push(entry);
                }
            }
            Ok(doc.to_string())
        }
        Harness::Opencode => {
            let mut config: Value = serde_json::from_str(original.unwrap_or("{}"))
                .context("opencode.json must be plain JSON (no comments) to be edited")?;
            let plugin = layout.plugin.as_ref().expect("opencode has a plugin shim");
            let url = format!("file://{}", plugin.display());
            let plugins = config
                .as_object_mut()
                .context("opencode.json is not an object")?
                .entry("plugin")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .context("opencode.json `plugin` is not a list")?;
            if !plugins.contains(&json!(url)) {
                plugins.push(json!(url));
            }
            Ok(serde_json::to_string_pretty(&config)? + "\n")
        }
        Harness::Hermes => {
            let original = original.unwrap_or("");
            // The owner's `hooks:` block already holds other tools' hooks: join them there.
            if !original.contains(HERMES_MARKER) {
                let hooks: Vec<(&str, String)> = HERMES_EVENTS
                    .iter()
                    .map(|event| (*event, command(exe, harness, event)))
                    .collect();
                if let Some(merged) = crate::hermes_hooks::wire(original, &hooks) {
                    return Ok(merged);
                }
            }
            let mut text = original.to_owned();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&hermes_block(exe));
            Ok(text)
        }
    }
}

pub(crate) const HERMES_MARKER: &str = "# axon-bus hooks; `axon-bus uninstall` removes them\n";

fn hermes_block(exe: &str) -> String {
    let mut block = format!("{HERMES_MARKER}hooks:\n");
    for event in HERMES_EVENTS {
        let cmd = command(exe, Harness::Hermes, event).replace('\'', "''");
        block.push_str(&format!("  {event}:\n    - command: '{cmd}'\n"));
    }
    block
}

fn plugin_source(exe: &str) -> String {
    let hook = if bus_verb(exe).is_empty() { json!([exe, "hook"]) } else { json!([exe, "bus", "hook"]) };
    format!(
        r#"// axon-bus plugin shim; `axon-bus uninstall` removes it.
const HOOK = {hook};
function hook(event, payload) {{
  const run = Bun.spawnSync([...HOOK, "opencode", event], {{
    stdin: Buffer.from(JSON.stringify(payload)),
  }});
  try {{
    const out = run.stdout.toString().trim();
    return out ? JSON.parse(out) : null;
  }} catch {{
    return null;
  }}
}}
export const AxonBus = async () => ({{
  event: async ({{ event }}) => {{
    if (["session.created", "session.idle", "session.deleted"].includes(event.type)) hook(event.type, event);
  }},
  "tool.execute.before": async (input, output) => {{
    const reply = hook("tool.execute.before", {{ input, output }});
    if (reply?.decision === "deny") throw new Error(reply.reason);
  }},
  "tool.execute.after": async (input, output) => {{
    hook("tool.execute.after", {{ input, output }});
  }},
}});
"#
    )
}

pub(crate) fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn write_if_changed(path: &Path, text: &str) -> anyhow::Result<()> {
    if read_optional(path)?.as_deref() != Some(text) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// The config as it was before axon-bus touched it: the backup, or the live file unless
/// the live file is exactly what the installer writes from nothing.
fn pristine(harness: Harness, exe: &str, layout: &Layout) -> anyhow::Result<Option<String>> {
    if let Some(backup) = read_optional(&backup_path(&layout.config))? {
        return Ok(Some(backup));
    }
    let current = read_optional(&layout.config)?;
    if current.is_some() && current == Some(wire(harness, None, exe, layout)?) {
        return Ok(None);
    }
    Ok(current)
}

/// The binary the hooks run. Run as `axon`, it must be an installed Axon: hooks outlive
/// any build, so a binary under a cargo `target/` directory is never wired.
pub(crate) fn current_exe() -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("locate the axon binary")?;
    let is_axon = exe.file_stem().is_some_and(|stem| stem == "axon");
    let in_build = exe.components().any(|c| c.as_os_str() == "target");
    anyhow::ensure!(
        !(is_axon && in_build),
        "{} is a development build; hooks are wired from an installed axon (see Install in the README)",
        exe.display()
    );
    exe.to_str()
        .map(str::to_owned)
        .context("the axon-bus binary path is not UTF-8")
}

/// Whether the live config differs from what install wrote over the backup `original`,
/// ignoring which binary path the bus hooks point at.
pub(crate) fn edited_since_install(
    harness: Harness,
    current: Option<&str>,
    original: &str,
    exe: &str,
    layout: &Layout,
) -> anyhow::Result<bool> {
    let Some(live) = current else {
        return Ok(false);
    };
    let installed = wire(harness, Some(original), exe, layout)?;
    if installed == live {
        return Ok(false);
    }
    let without_bus = |text: &str| crate::uninstall::unwire(harness, text, layout).ok();
    Ok(without_bus(live).is_none() || without_bus(live) != without_bus(&installed))
}

pub fn install(harness: Harness) -> anyhow::Result<()> {
    let exe = current_exe()?;
    let layout = layout(harness);
    let current = read_optional(&layout.config)?;
    let backup = read_optional(&backup_path(&layout.config))?;
    let wired = match (&backup, &current) {
        // Edited after install: wire the live file so those edits survive.
        (Some(original), Some(live))
            if edited_since_install(harness, Some(live), original, &exe, &layout)? =>
        {
            // Hermes has one block to replace; the others are repointed in place.
            let live = if harness == Harness::Hermes && live.contains(HERMES_MARKER) {
                crate::uninstall::unwire(harness, live, &layout)?
            } else {
                live.clone()
            };
            wire(harness, Some(&live), &exe, &layout)
                .with_context(|| format!("wire {}", layout.config.display()))?
        }
        _ => {
            let original = pristine(harness, &exe, &layout)?;
            if let Some(original) = &original {
                write_if_changed(&backup_path(&layout.config), original)?;
            }
            wire(harness, original.as_deref(), &exe, &layout)
                .with_context(|| format!("wire {}", layout.config.display()))?
        }
    };
    write_if_changed(&layout.config, &wired)?;
    if let Some(plugin) = &layout.plugin {
        write_if_changed(plugin, &plugin_source(&exe))?;
    }
    println!("{}: wired {}", harness.as_str(), layout.config.display());
    Ok(())
}

/// Harnesses whose config directory exists on this machine.
pub fn detected() -> Vec<Harness> {
    Harness::ALL
        .into_iter()
        .filter(|h| layout(*h).config.parent().is_some_and(Path::exists))
        .collect()
}

/// Whether `harness`'s hooks run `exe`. OpenCode's config only names the plugin shim;
/// the shim names the binary.
pub fn is_wired(harness: Harness, exe: &str) -> anyhow::Result<bool> {
    let layout = layout(harness);
    Ok(match &layout.plugin {
        Some(plugin) => read_optional(plugin)?.is_some_and(|text| text == plugin_source(exe)),
        None => read_optional(&layout.config)?
            .is_some_and(|text| text.contains(command(exe, harness, "").trim_end())),
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
