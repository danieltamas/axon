//! Wires `axon bus hook` into each harness's own config (BUS-PLAN §00: a plug-in, not a wrapper).
//!
//! Each config is rewritten from its pristine copy — the `<file>.axon-bus.bak` backup once
//! one exists — so installing again produces the same bytes. Uninstall puts the backup back.
//! A config the user edited after install is wired or unwired in place instead, so those
//! edits are never rolled back.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{json, Value};

use crate::hermes_hooks::{hermes_block, HERMES_MARKER};
use crate::opencode_plugin::plugin_source;
use crate::Harness;

const BACKUP_SUFFIX: &str = ".axon-bus.bak";
const WROTE_SUFFIX: &str = ".axon-bus.wrote";

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
pub(crate) const HERMES_EVENTS: [&str; 8] = [
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

/// The copy of a Codex config exactly as install last wrote it. A live file that still
/// matches it holds no edits of the owner's, whatever their formatting or comments.
pub(crate) fn wrote_path(config: &Path) -> PathBuf {
    let mut name = config.file_name().unwrap_or_default().to_os_string();
    name.push(WROTE_SUFFIX);
    config.with_file_name(name)
}

/// The installed binary, quoted for the shell-style command lines harnesses run. Windows
/// paths use forward slashes, which cmd, PowerShell and Git Bash all accept unquoted, and
/// double quotes when they hold a space, since cmd has no single quotes.
pub(crate) fn command(exe: &str, harness: Harness, event: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c);
    let exe = if cfg!(windows) {
        exe.replace('\\', "/")
    } else {
        exe.to_owned()
    };
    let exe = if exe.chars().all(safe) {
        exe
    } else if cfg!(windows) {
        format!("\"{exe}\"")
    } else {
        format!("'{}'", exe.replace('\'', r"'\''"))
    };
    format!("{exe}{} hook {} {event}", bus_verb(&exe), harness.as_str())
}

/// `axon` runs the bus as a subcommand; the `axon-bus` alias takes the verb directly.
pub(crate) fn bus_verb(exe: &str) -> &'static str {
    if binary_name(exe) == "axon" {
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
    let name = binary_name(exe.strip_suffix(" bus").unwrap_or(exe));
    name == "axon-bus" || name == "axon"
}

/// The binary's name without quotes, directories or a Windows `.exe`.
fn binary_name(exe: &str) -> &str {
    let file = exe
        .trim_matches(['\'', '"'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    file.strip_suffix(".exe").unwrap_or(file)
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
        Harness::Codex => crate::codex_hooks::wire(original, exe),
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
                anyhow::ensure!(
                    !crate::hermes_hooks::has_unmergeable_hooks(original),
                    "config.yaml has a `hooks:` value this installer cannot merge into; \
                     write it as a block mapping (`hooks:` on its own line) and run install again"
                );
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
    if harness == Harness::Codex {
        if let Some(wrote) = read_optional(&wrote_path(&layout.config))? {
            return Ok(live != wrote);
        }
    }
    let installed = wire(harness, Some(original), exe, layout)?;
    if installed == live {
        return Ok(false);
    }
    let without_bus = |text: &str| crate::uninstall::unwire(harness, text, layout).ok();
    let (live, installed) = (without_bus(live), without_bus(&installed));
    Ok(match (live, installed) {
        // TOML can be rewritten (inline tables to `[[tables]]`) without changing meaning.
        (Some(live), Some(installed)) if harness == Harness::Codex => {
            let parse = |text: &str| toml::from_str::<toml::Value>(text).ok();
            parse(&live).is_none() || parse(&live) != parse(&installed)
        }
        (live, installed) => live.is_none() || live != installed,
    })
}

pub fn install(harness: Harness) -> anyhow::Result<()> {
    let exe = current_exe()?;
    let layout = layout(harness);
    let current = read_optional(&layout.config)?;
    let backup = read_optional(&backup_path(&layout.config))?;
    let mut fresh = false;
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
            fresh = true;
            let original = pristine(harness, &exe, &layout)?;
            if let Some(original) = &original {
                write_if_changed(&backup_path(&layout.config), original)?;
            }
            wire(harness, original.as_deref(), &exe, &layout)
                .with_context(|| format!("wire {}", layout.config.display()))?
        }
    };
    write_if_changed(&layout.config, &wired)?;
    if harness == Harness::Codex && fresh {
        write_if_changed(&wrote_path(&layout.config), &wired)?;
    }
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

#[cfg(test)]
mod acceptance_review2 {
    use super::*;

    fn hermes_layout() -> Layout {
        // wire is a pure text edit; these paths are never accessed.
        Layout {
            config: PathBuf::from("review2/config.yaml"),
            plugin: None,
        }
    }

    #[test]
    fn r4_install_merges_commented_hooks_without_duplicate_mapping() {
        let original = "hooks: # owner\n  pre_tool_call:\n  - command: 'beacon check'\n";
        let wired = wire(Harness::Hermes, Some(original), "axon", &hermes_layout()).unwrap();
        assert_eq!(
            wired
                .lines()
                .filter(|line| line.starts_with("hooks:"))
                .count(),
            1
        );
        assert_eq!(crate::hermes_hooks::unwire(&wired), original);
    }

    #[test]
    fn r4_install_refuses_flow_mapping() {
        for original in ["hooks: {}\n", "hooks: {} # owner\n"] {
            assert!(wire(Harness::Hermes, Some(original), "axon", &hermes_layout()).is_err());
        }
    }

    #[test]
    fn r5_windows_binary_names_keep_dispatch_and_are_recognized() {
        for exe in [
            "axon.exe",
            r"C:\Users\Alice\bin\axon.exe",
            r"C:\Users\Alice\bin\axon",
            "C:/Program Files/Axon/axon.exe",
        ] {
            assert_eq!(bus_verb(exe), " bus", "{exe}");
            let hook = command(exe, Harness::Claude, "PreToolUse");
            assert!(hook.ends_with(" bus hook claude PreToolUse"), "{hook}");
            assert!(
                is_bus_command(&hook, Harness::Claude, "PreToolUse"),
                "{hook}"
            );
        }
        for exe in ["axon-bus.exe", r"C:\Users\Alice\bin\axon-bus.exe"] {
            assert_eq!(bus_verb(exe), "", "{exe}");
            let hook = command(exe, Harness::Claude, "PreToolUse");
            assert!(
                is_bus_command(&hook, Harness::Claude, "PreToolUse"),
                "{hook}"
            );
        }
    }

    #[test]
    fn r5_recognizes_existing_quoted_windows_hooks() {
        for hook in [
            r#""C:\Program Files\Axon\axon.exe" bus hook claude PreToolUse"#,
            r#""C:\Program Files\Axon\axon-bus.exe" hook claude PreToolUse"#,
        ] {
            assert!(
                is_bus_command(hook, Harness::Claude, "PreToolUse"),
                "{hook}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn r5_windows_command_uses_cmd_compatible_quotes() {
        assert_eq!(
            command(
                r"C:\Program Files\Axon\axon.exe",
                Harness::Claude,
                "PreToolUse"
            ),
            r#""C:/Program Files/Axon/axon.exe" bus hook claude PreToolUse"#
        );
        assert_eq!(
            command(
                r"C:\Program Files\Axon\axon-bus.exe",
                Harness::Claude,
                "PreToolUse"
            ),
            r#""C:/Program Files/Axon/axon-bus.exe" hook claude PreToolUse"#
        );
    }

    #[test]
    fn codex_edit_is_judged_against_the_bytes_install_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout {
            config: dir.path().join("config.toml"),
            plugin: None,
        };
        let original = "model = \"x\"\n";
        let wired = wire(Harness::Codex, Some(original), "axon", &layout).unwrap();
        fs::write(wrote_path(&layout.config), &wired).unwrap();
        let edited = |live: &str| {
            edited_since_install(Harness::Codex, Some(live), original, "axon", &layout).unwrap()
        };
        assert!(!edited(&wired));
        assert!(edited(&format!("# owner note\n{wired}")));
    }
}
