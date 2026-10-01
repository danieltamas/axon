//! Bus commands an agent types, run by its hook. A sandboxed shell cannot write the hub,
//! which lives outside the project, so an agent's `send` would fail there; the hook runs
//! outside any sandbox and already knows who the agent is. It runs the command as that
//! agent and answers the tool call with the result in place of the command's output.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::cli_guard::{self, Invocation, AGENT_VERBS, FROM_VERBS};
use crate::gate::deny;
use crate::install::bus_verb;

/// Verbs the hook runs for the agent: messaging, links, and what the agent may read.
const RELAYED: [&str; 10] = [
    "send", "reply", "grant", "link", "accept", "peers", "claim", "release", "claims", "budget",
];
/// Longest command output passed back; claims can list a whole checkout.
const MAX_OUTPUT: usize = 4000;

/// The arguments of this tool call's bus command, when the command is nothing else: one
/// bare invocation, no separators, redirections or substitutions the hook would not run.
fn sole_bus_command(payload: &Value) -> Option<Vec<String>> {
    let command = cli_guard::command(payload)?;
    if !command.contains("axon") {
        return None;
    }
    let words = cli_guard::words(command);
    let Some(Invocation::Bus(start)) = cli_guard::invocation(&words, 0) else {
        return None;
    };
    let shell =
        |w: &String| w == ";" || w.starts_with(['>', '<']) || w.contains("$(") || w.contains('`');
    if words.iter().any(shell) {
        return None;
    }
    Some(words[start.min(words.len())..].to_vec())
}

/// The reply for `actor`'s tool call when it is a bus command the hook runs; None lets the
/// call through untouched.
pub fn run(harness: &str, actor: &str, payload: &Value) -> Option<Value> {
    let mut args = sole_bus_command(payload)?;
    let verb = args.iter().find(|a| !a.starts_with('-'))?.clone();
    if verb == "ask" {
        return Some(deny(
            harness,
            "ask waits for its answer, which a hook cannot do. Send the question with \
             `send --kind question` instead; the answer arrives in your context."
                .to_owned(),
        ));
    }
    if !RELAYED.contains(&verb.as_str()) {
        return None;
    }
    // The guard already refused a `--from` or `--agent` naming another agent.
    let actor_flag = if FROM_VERBS.contains(&verb.as_str()) {
        Some("from")
    } else if AGENT_VERBS.contains(&verb.as_str()) || verb == "peers" {
        Some("agent")
    } else {
        None
    };
    if let Some(name) = actor_flag {
        if cli_guard::flag(&args, name).is_none() {
            args.extend([format!("--{name}"), actor.to_owned()]);
        }
    }
    Some(deny(harness, outcome(&verb, &args, payload)))
}

/// What running the command produced, worded so the agent does not run it again.
fn outcome(verb: &str, args: &[String], payload: &Value) -> String {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return format!("axon could not run `{verb}` for you: {err}"),
    };
    let mut command = Command::new(&exe);
    if !bus_verb(&exe.to_string_lossy()).is_empty() {
        command.arg("bus");
    }
    command.args(args).stdin(Stdio::null());
    // Claims are relative to the agent's checkout.
    if let Some(cwd) = payload["cwd"].as_str().filter(|c| Path::new(c).is_dir()) {
        command.current_dir(cwd);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(err) => return format!("axon could not run `{verb}` for you: {err}"),
    };
    let text = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes).trim().to_owned();
        match text.char_indices().nth(MAX_OUTPUT) {
            Some((cut, _)) => format!("{}… (truncated)", &text[..cut]),
            None => text,
        }
    };
    if output.status.success() {
        let out = text(&output.stdout);
        let shown = if out.is_empty() {
            "done".to_owned()
        } else {
            out
        };
        format!(
            "axon ran `{verb}` for you through its hook (the bus lives outside your sandbox); \
             this is its result, so do not run it again.\n{shown}"
        )
    } else {
        format!(
            "axon ran `{verb}` for you through its hook and it failed: {}",
            text(&output.stderr)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(command: &str) -> Value {
        json!({"tool_input": {"command": command}})
    }

    #[test]
    fn only_a_bare_bus_command_is_taken() {
        let args = sole_bus_command(&bash("axon bus send --to w --kind sync --body 'a; b > c'"));
        assert_eq!(
            args.unwrap(),
            ["send", "--to", "w", "--kind", "sync", "--body", "a; b > c"]
        );
        assert!(sole_bus_command(&bash("/usr/local/bin/axon-bus peers")).is_some());
        for shell in [
            "axon bus send --to w --body x; rm -rf y",
            "axon bus claims > out.txt",
            "axon bus send --to w --body $(cat secret)",
            "cd x && axon bus peers",
            "echo axon bus peers",
        ] {
            assert!(sole_bus_command(&bash(shell)).is_none(), "{shell}");
        }
    }

    #[test]
    fn ask_is_refused_with_the_alternative() {
        let reply = run(
            "claude",
            "w1",
            &bash("axon bus ask --to o --body q --wait 9 --default n"),
        );
        let reason = reply.unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(reason.contains("--kind question"), "{reason}");
    }

    #[test]
    fn other_verbs_pass_through() {
        assert!(run("claude", "w1", &bash("axon bus doctor")).is_none());
        assert!(run("claude", "w1", &bash("ls -la")).is_none());
    }
}
