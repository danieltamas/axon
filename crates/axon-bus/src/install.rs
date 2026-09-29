//! `install` / `uninstall` / `doctor`: wire `axon-bus hook` into each harness's own config
//! (BUS-PLAN §00: a plug-in, never a wrapper).
//!
//! Each config is rewritten from its pristine copy — the `<file>.axon-bus.bak` backup once
//! one exists — so installing again produces the same bytes. Uninstall puts the backup back.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::Harness;

const BACKUP_SUFFIX: &str = ".axon-bus.bak";

const CLAUDE_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "SessionEnd",
];
const CODEX_EVENTS: [&str; 7] = [
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
struct Layout {
    config: PathBuf,
    /// A file the installer owns outright (the OpenCode plugin shim).
    plugin: Option<PathBuf>,
}

fn env_dir(var: &str, fallback: &[&str]) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        fallback.iter().fold(
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()),
            |dir, part| dir.join(part),
        )
    })
}

fn layout(harness: Harness) -> Layout {
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

fn backup_path(config: &Path) -> PathBuf {
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
    format!("{exe} hook {} {event}", harness.as_str())
}

/// The config text with the bus hooks added to `original` (None: no config yet).
fn wire(
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
                if !entries.iter().any(|e| e.to_string().contains(&cmd)) {
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
                let entries = hooks
                    .entry(event)
                    .or_insert(toml_edit::value(toml_edit::Array::new()))
                    .as_array_mut()
                    .with_context(|| format!("config.toml `hooks.{event}` is not an array"))?;
                let wired = entries.iter().any(|e| {
                    e.as_inline_table()
                        .and_then(|t| t.get("command"))
                        .and_then(|c| c.as_str())
                        == Some(cmd.as_str())
                });
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
            if original.lines().any(|l| l.starts_with("hooks:")) {
                bail!("config.yaml already has a `hooks:` block; add the axon-bus hooks to it by hand");
            }
            let mut text = original.to_owned();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("# axon-bus hooks; `axon-bus uninstall` removes them\nhooks:\n");
            for event in HERMES_EVENTS {
                let cmd = command(exe, harness, event).replace('\'', "''");
                text.push_str(&format!("  {event}:\n    - command: '{cmd}'\n"));
            }
            Ok(text)
        }
    }
}

fn plugin_source(exe: &str) -> String {
    let exe = json!(exe);
    format!(
        r#"// axon-bus plugin shim; `axon-bus uninstall` removes it.
const EXE = {exe};
function hook(event, payload) {{
  const run = Bun.spawnSync([EXE, "hook", "opencode", event], {{
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

fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
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

fn current_exe() -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("locate the axon-bus binary")?;
    exe.to_str()
        .map(str::to_owned)
        .context("the axon-bus binary path is not UTF-8")
}

pub fn install(harness: Harness) -> anyhow::Result<()> {
    let exe = current_exe()?;
    let layout = layout(harness);
    let original = pristine(harness, &exe, &layout)?;
    let wired = wire(harness, original.as_deref(), &exe, &layout)
        .with_context(|| format!("wire {}", layout.config.display()))?;
    if let Some(original) = &original {
        write_if_changed(&backup_path(&layout.config), original)?;
    }
    write_if_changed(&layout.config, &wired)?;
    if let Some(plugin) = &layout.plugin {
        write_if_changed(plugin, &plugin_source(&exe))?;
    }
    println!("{}: wired {}", harness.as_str(), layout.config.display());
    Ok(())
}

pub fn uninstall(harness: Harness) -> anyhow::Result<()> {
    let exe = current_exe()?;
    let layout = layout(harness);
    let backup = backup_path(&layout.config);
    let current = read_optional(&layout.config)?;
    if let Some(original) = read_optional(&backup)? {
        if current.is_some() && current != Some(wire(harness, Some(&original), &exe, &layout)?) {
            eprintln!(
                "{}: {} changed after install; restoring the pre-install backup",
                harness.as_str(),
                layout.config.display()
            );
        }
        fs::write(&layout.config, original)?;
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

/// Harnesses whose config directory exists on this machine.
pub fn detected() -> Vec<Harness> {
    Harness::ALL
        .into_iter()
        .filter(|h| layout(*h).config.parent().is_some_and(Path::exists))
        .collect()
}

/// Print one line per harness and the hub; true when every detected harness is wired to
/// this binary.
pub fn doctor(db: &Path) -> anyhow::Result<bool> {
    let exe = current_exe()?;
    let mut healthy = true;
    for harness in detected() {
        let layout = layout(harness);
        let marker = command(&exe, harness, "");
        let config = read_optional(&layout.config)?.unwrap_or_default();
        let (state, detail) = if config.contains(marker.trim_end()) {
            ("ok", "hooks point at this binary")
        } else if config.contains("axon-bus") {
            healthy = false;
            (
                "warn",
                "hooks point at another axon-bus binary; run `axon-bus install`",
            )
        } else {
            healthy = false;
            ("missing", "not wired; run `axon-bus install`")
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
                "missing hub       {} (hooks stay inert; run `axon-bus init`)",
                db.display()
            );
        }
    }
    Ok(healthy)
}
