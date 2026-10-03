//! Bus commands an agent types, run by its hook. A sandboxed shell cannot write the hub,
//! which lives outside the project, so an agent's `send` would fail there; the hook runs
//! outside any sandbox and already knows who the agent is. It runs the command as that
//! agent and answers the tool call with the result in place of the command's output.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli_guard::{self, Invocation, AGENT_VERBS, FROM_VERBS};
use crate::gate::deny;
use crate::install::bus_verb;

/// Verbs the hook runs for the agent: messaging, links, and what the agent may read.
const RELAYED: [&str; 14] = [
    "send", "reply", "grant", "link", "accept", "peers", "claim", "release", "claims", "budget",
    "take", "done", "drop", "handled",
];
/// Longest command output passed back; claims can list a whole checkout.
const MAX_OUTPUT: usize = 4000;
/// Most bytes read from each of the command's streams; the rest is never buffered.
const MAX_CAPTURE: u64 = 64 * 1024;
/// How long the hook waits for the command before killing it, so a stuck hub cannot stall
/// the agent's tool call.
const DEADLINE: Duration = Duration::from_secs(10);
/// Tool names of each harness's shell tool. Any other tool that carries a `command` field
/// (an MCP preview, say) never runs it, so neither does the relay.
const SHELL_TOOLS: [&str; 5] = ["bash", "shell", "terminal", "exec_command", "local_shell"];

fn shell_tool(payload: &Value) -> bool {
    ["/tool_name", "/input/tool"]
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
        .is_some_and(|tool| SHELL_TOOLS.contains(&tool.to_ascii_lowercase().as_str()))
}

/// The arguments of this tool call's bus command, when the command is nothing else: one
/// bare invocation, no separators, redirections or substitutions the hook would not run.
fn sole_bus_command(payload: &Value) -> Option<Vec<String>> {
    if !shell_tool(payload) {
        return None;
    }
    let command = cli_guard::command(payload)?;
    if !command.contains("axon") {
        return None;
    }
    let words = cli_guard::words(command);
    let Some(Invocation::Bus(start)) = cli_guard::invocation(&words, 0) else {
        return None;
    };
    let shell =
        |w: &String| [";", "<", ">"].contains(&w.as_str()) || w.contains("$(") || w.contains('`');
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
    } else if AGENT_VERBS.contains(&verb.as_str()) {
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
    let failed = |why: &str| {
        format!(
            "axon ran `{verb}` for you through its hook and it failed; do not run it again \
             unchanged.\n{why}"
        )
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return failed(&err.to_string()),
    };
    let mut command = Command::new(&exe);
    if !bus_verb(&exe.to_string_lossy()).is_empty() {
        command.arg("bus");
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Claims are relative to the agent's checkout; another one must never stand in for it.
    if let Some(cwd) = payload["cwd"].as_str() {
        if !Path::new(cwd).is_dir() {
            return failed(&format!("your working directory {cwd} does not exist"));
        }
        command.current_dir(cwd);
    }
    let (success, stdout, stderr) = match run_bounded(command) {
        Ok(done) => done,
        Err(err) => return failed(&err.to_string()),
    };
    let text = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes).trim().to_owned();
        match text.char_indices().nth(MAX_OUTPUT) {
            Some((cut, _)) => format!("{}… (truncated)", &text[..cut]),
            None => text,
        }
    };
    if success {
        let out = text(&stdout);
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
        failed(&text(&stderr))
    }
}

/// Runs `command` with each stream read up to `MAX_CAPTURE` and the whole run within
/// `DEADLINE`: whether it succeeded, its stdout and its stderr.
fn run_bounded(mut command: Command) -> std::io::Result<(bool, Vec<u8>, Vec<u8>)> {
    let mut child = command.spawn()?;
    let capture = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stream) = stream {
                // Dropping the pipe after the cap makes a chattier command fail, not block.
                let _ = stream.take(MAX_CAPTURE).read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = capture(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = capture(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let status = wait_until(&mut child, Instant::now() + DEADLINE)?;
    let joined = |handle: std::thread::JoinHandle<Vec<u8>>| handle.join().unwrap_or_default();
    let (stdout, mut stderr) = (joined(stdout), joined(stderr));
    match status {
        Some(status) => Ok((status.success(), stdout, stderr)),
        None => {
            stderr = format!("it did not finish within {} s", DEADLINE.as_secs()).into_bytes();
            Ok((false, stdout, stderr))
        }
    }
}

/// The child's exit status, or None after killing it at `deadline`.
fn wait_until(
    child: &mut Child,
    deadline: Instant,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(command: &str) -> Value {
        json!({"tool_name": "Bash", "tool_input": {"command": command}})
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
