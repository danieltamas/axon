//! The bus CLI as seen from inside a governed session (audit SEC-1): an agent may run
//! `axon-bus` only as itself, and never the verbs reserved for the human (raising a
//! budget, turning content capture on). The gate reads the shell command of each tool
//! call before it runs; indirection through scripts or variables is not parsed, so this
//! stops forged calls a prompt-injected agent types, not a determined local attacker.

use serde_json::Value;

/// Verbs whose acting agent is `--from`.
pub(crate) const FROM_VERBS: [&str; 6] = ["send", "ask", "reply", "grant", "link", "accept"];
/// Verbs whose acting agent is `--agent`.
pub(crate) const AGENT_VERBS: [&str; 3] = ["claim", "release", "peers"];

/// The shell command of a tool call, in each harness's payload shape.
pub(crate) fn command(payload: &Value) -> Option<&str> {
    ["/tool_input/command", "/output/args/command"]
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
}

/// Shell words, with `;`, `&`, `|` and newlines as their own separator words and unquoted
/// `<` and `>` as their own words. Quotes and POSIX backslash escapes are honoured (on
/// Windows a bare backslash is a path separator); expansions are left as written. An
/// unterminated quote ends in a separator: the shell would not run that command as read.
pub(crate) fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            // Inside double quotes a backslash escapes only these; `"C:\Program Files"` keeps it.
            (Some('"'), '\\') => match chars.next() {
                Some(next @ ('$' | '`' | '"' | '\\' | '\n')) => word.push(next),
                Some(next) => word.extend(['\\', next]),
                None => word.push('\\'),
            },
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            // cmd and PowerShell separate paths with it; only POSIX shells escape with it.
            (None, '\\') if cfg!(windows) => {
                word.push('\\');
                in_word = true;
            }
            (None, '\\') => {
                word.extend(chars.next());
                in_word = true;
            }
            (None, ';' | '&' | '|' | '\n') => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                words.push(";".to_owned());
            }
            (None, '<' | '>') => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                words.push(c.to_string());
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if quote.is_some() {
        words.push(";".to_owned());
    }
    words
}

/// The value of `--name VALUE` or `--name=VALUE` among `args`.
pub(crate) fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let long = format!("--{name}");
    let joined = format!("--{name}=");
    args.iter().enumerate().find_map(|(i, arg)| {
        if *arg == long {
            args.get(i + 1).map(String::as_str)
        } else {
            arg.strip_prefix(&joined)
        }
    })
}

/// Words that may stand before a program without making it an argument.
const PREFIXES: [&str; 6] = ["exec", "env", "nohup", "time", "command", "sudo"];

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Invocation {
    /// `axon-bus …` or `axon bus …`; its arguments start at this word.
    Bus(usize),
    /// The `axon` dashboard, which serves captured content like `axon-bus serve`.
    Dashboard(usize),
}

/// `NAME=value` with a shell identifier for NAME; `/tmp/a=b/axon` is a path, not one.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether `words[i]` runs a bus command: `axon-bus` anywhere (as before), `axon` only in
/// command position, since a path ending in `/axon` is usually a directory.
pub(crate) fn invocation(words: &[String], i: usize) -> Option<Invocation> {
    // `DIR=/x/axon` assigns a variable; it runs nothing.
    if is_assignment(&words[i]) {
        return None;
    }
    let file = words[i].rsplit(['/', '\\']).next()?;
    match file.strip_suffix(".exe").unwrap_or(file) {
        "axon-bus" => Some(Invocation::Bus(i + 1)),
        "axon" => {
            let before = i.checked_sub(1).map(|b| words[b].as_str());
            let command_position =
                before.is_none_or(|w| w == ";" || PREFIXES.contains(&w) || is_assignment(w));
            if !command_position {
                None
            } else if words.get(i + 1).map(String::as_str) == Some("bus") {
                Some(Invocation::Bus(i + 2))
            } else {
                Some(Invocation::Dashboard(i + 1))
            }
        }
        _ => None,
    }
}

/// Capture is the default everywhere, so only an explicit, unconflicted structure-only
/// serve is an agent's; `--content` beside `--no-content` would win in clap's order.
fn serves_content(args: &[String]) -> bool {
    let set = |name: &str| {
        args.iter()
            .any(|a| a == name || a.starts_with(&format!("{name}=")))
    };
    set("--content") || !set("--no-content")
}

const CAPTURE_REASON: &str =
    "content capture is turned on by the human only; an agent may run serve --no-content";

/// Why `actor` may not run this tool call's bus command, or None.
pub fn refusal(actor: &str, payload: &Value) -> Option<String> {
    let command = command(payload)?;
    if !command.contains("axon") {
        return None;
    }
    let words = words(command);
    for i in 0..words.len() {
        let Some(invocation) = invocation(&words, i) else {
            continue;
        };
        let (Invocation::Bus(start) | Invocation::Dashboard(start)) = invocation;
        let args: Vec<String> = words[start.min(words.len())..]
            .iter()
            .take_while(|w| *w != ";")
            .cloned()
            .collect();
        if invocation == Invocation::Dashboard(start) {
            // `axon --scan-only`, `--help` and `--version` serve nothing.
            let inert = args
                .iter()
                .any(|a| ["--scan-only", "--help", "-h", "--version", "-V"].contains(&a.as_str()));
            if !inert && serves_content(&args) {
                return Some(CAPTURE_REASON.to_owned());
            }
            continue;
        }
        let verb = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .map(String::as_str);
        let reason = match verb {
            Some("budget") if args.iter().any(|a| a == "set") => Some(
                "raising or replacing a budget is reserved for the human; ask your root to escalate"
                    .to_owned(),
            ),
            Some("serve") if serves_content(&args) => Some(CAPTURE_REASON.to_owned()),
            Some(verb) if FROM_VERBS.contains(&verb) => impersonation(actor, verb, flag(&args, "from")),
            Some(verb) if AGENT_VERBS.contains(&verb) => impersonation(actor, verb, flag(&args, "agent")),
            _ => None,
        };
        if reason.is_some() {
            return reason;
        }
    }
    None
}

fn impersonation(actor: &str, verb: &str, claimed: Option<&str>) -> Option<String> {
    claimed
        .filter(|c| *c != actor)
        .map(|claimed| format!("axon-bus {verb} must run as yourself ({actor}), not as {claimed}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(command: &str) -> Value {
        json!({"tool_input": {"command": command}})
    }

    #[test]
    fn forged_sender_and_human_verbs_are_refused() {
        let forged = bash("cd x && axon-bus send --from orch --to w --kind stop --body hi");
        assert!(refusal("w1", &forged).unwrap().contains("as yourself"));
        assert!(refusal("w1", &bash("~/.cargo/bin/axon-bus budget set root 9Mtok")).is_some());
        assert!(refusal("w1", &bash("axon-bus serve --content")).is_some());
        assert!(refusal("w1", &bash("axon-bus serve --port 0")).is_some());
        assert!(refusal("w1", &bash("axon-bus serve --no-content")).is_none());
        assert!(refusal("w1", &bash("axon-bus release --agent=other src")).is_some());
        assert!(refusal("w1", &bash("axon-bus serve --no-content --content")).is_some());
        assert!(refusal("w1", &bash("axon bus send --from orch --to w --body hi")).is_some());
        assert!(refusal("w1", &bash("axon bus serve --port 0")).is_some());
        assert!(refusal("w1", &bash("./target/debug/axon --port 7777")).is_some());
        assert!(refusal("w1", &bash("axon --no-content")).is_none());
        assert!(refusal("w1", &bash("axon --scan-only")).is_none());
    }

    #[test]
    fn own_sends_and_other_commands_pass() {
        assert_eq!(
            refusal(
                "w1",
                &bash("axon-bus send --from w1 --to orch --body 'a; b'")
            ),
            None
        );
        assert_eq!(
            refusal("w1", &bash("axon-bus budget show root --json")),
            None
        );
        assert_eq!(refusal("w1", &bash("echo axon-bus --from orch")), None);
        assert_eq!(refusal("w1", &bash("cd /Users/me/Sites/axon && ls")), None);
        assert_eq!(
            refusal("w1", &json!({"output": {"args": {"command": "ls"}}})),
            None
        );
    }
}

#[cfg(test)]
mod acceptance_review2 {
    use super::*;
    use serde_json::json;

    #[test]
    fn guard_assignment_is_not_an_invocation() {
        assert_eq!(
            refusal(
                "worker",
                &json!({"tool_input":{"command":"D=/tmp/x/axon && ls"}})
            ),
            None
        );
    }

    #[test]
    fn guard_assignment_prefix_does_not_hide_dashboard() {
        assert!(refusal(
            "worker",
            &json!({"tool_input":{"command":"FOO=1 axon --port 7777"}})
        )
        .is_some());
    }

    #[test]
    fn r5_invocation_recognizes_windows_binary_names() {
        for exe in [
            "axon.exe",
            r"C:\Users\Alice\axon.exe",
            r"C:\Users\Alice\axon",
        ] {
            let words = vec![exe.to_owned(), "bus".into(), "serve".into()];
            assert!(invocation(&words, 0) == Some(Invocation::Bus(2)), "{exe}");
        }
        for exe in ["axon-bus.exe", r"C:\Users\Alice\axon-bus.exe"] {
            let words = vec![exe.to_owned(), "serve".into()];
            assert!(invocation(&words, 0) == Some(Invocation::Bus(1)), "{exe}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn r5_windows_backslash_command_is_guarded() {
        for command in [
            r"C:\Users\Alice\axon.exe --port 7777",
            r#""C:\Program Files\Axon\axon.exe" --port 7777"#,
        ] {
            assert!(
                refusal("worker", &json!({"tool_input":{"command":command}})).is_some(),
                "{command}"
            );
        }
    }

    #[test]
    fn guard_equals_in_executable_path_is_still_an_invocation() {
        let payload = json!({"tool_input":{"command":"'/tmp/a=b/axon' --port 7777"}});
        assert!(refusal("worker", &payload).is_some());
    }
}
