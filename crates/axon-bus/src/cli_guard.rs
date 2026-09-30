//! The bus CLI as seen from inside a governed session (audit SEC-1): an agent may run
//! `axon-bus` only as itself, and never the verbs reserved for the human (raising a
//! budget, turning content capture on). The gate reads the shell command of each tool
//! call before it runs; indirection through scripts or variables is not parsed, so this
//! stops forged calls a prompt-injected agent types, not a determined local attacker.

use serde_json::Value;

/// Verbs whose acting agent is `--from`.
const FROM_VERBS: [&str; 6] = ["send", "ask", "reply", "grant", "link", "accept"];
/// Verbs whose acting agent is `--agent`.
const AGENT_VERBS: [&str; 2] = ["claim", "release"];

/// The shell command of a tool call, in each harness's payload shape.
fn command(payload: &Value) -> Option<&str> {
    ["/tool_input/command", "/output/args/command"]
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
}

/// Shell words, with `;`, `&`, `|` and newlines as their own separator words. Quotes and
/// backslashes are honoured; expansions are left as written.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => word.extend(chars.next()),
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
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
    words
}

/// The value of `--name VALUE` or `--name=VALUE` among `args`.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
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

/// Why `actor` may not run this tool call's bus command, or None.
pub fn refusal(actor: &str, payload: &Value) -> Option<String> {
    let command = command(payload)?;
    if !command.contains("axon-bus") {
        return None;
    }
    let words = words(command);
    for (i, word) in words.iter().enumerate() {
        if word.rsplit('/').next() != Some("axon-bus") {
            continue;
        }
        let args: Vec<String> = words[i + 1..]
            .iter()
            .take_while(|w| *w != ";")
            .cloned()
            .collect();
        let verb = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .map(String::as_str);
        let reason = match verb {
            Some("budget") if args.iter().any(|a| a == "set") => Some(
                "raising or replacing a budget is reserved for the human; ask your root to escalate"
                    .to_owned(),
            ),
            Some("serve") if args.iter().any(|a| a == "--content") => {
                Some("content capture is turned on by the human only".to_owned())
            }
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
        assert!(refusal("w1", &bash("axon-bus release --agent=other src")).is_some());
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
        assert_eq!(
            refusal("w1", &json!({"output": {"args": {"command": "ls"}}})),
            None
        );
    }
}
