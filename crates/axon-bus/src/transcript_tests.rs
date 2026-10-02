//! Unit tests of the native-record narrative mapping and redaction.
use super::*;
use serde_json::json;

#[test]
fn redact_masks_tokens_and_keeps_prose() {
    assert_eq!(
        redact("use sk-abcdefghijklmnopqrstu now"),
        "use [redacted] now"
    );
    assert_eq!(
        redact("KEY=sk-abcdefghijklmnopqrstu `ghp_abcdefghijklmnopqrst`"),
        "KEY=[redacted] `[redacted]`"
    );
    for (text, expected) in [
        (
            "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuv'",
            "Bearer [redacted]",
        ),
        (
            "postgres://app:hunter22@db/main",
            "postgres://app:[redacted]@db/main",
        ),
        (
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENG",
            "AWS_SECRET_ACCESS_KEY=[redacted]",
        ),
        ("password: 'correct-horse'", "password: '[redacted]'"),
        (
            "jwt eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2Q",
            "jwt [redacted]",
        ),
        (
            "ghs_abcdefghijklmnop and hf_abcdefghijklmnop",
            "[redacted] and [redacted]",
        ),
    ] {
        let redacted = redact(text);
        assert!(redacted.contains(expected), "{text} -> {redacted}");
    }
    assert_eq!(redact("input_tokens: 123456"), "input_tokens: 123456");
    assert_eq!(
        redact(r#"{"output_tokens":123456}"#),
        r#"{"output_tokens":123456}"#
    );
    assert_eq!(
        redact(r#"{"password":"correct-horse-battery"}"#),
        r#"{"password":"[redacted]"}"#
    );
    assert_eq!(redact("password=12345678"), "password=[redacted]");
    assert_eq!(redact("plain words stay"), "plain words stay");
}

#[test]
fn claude_signature_only_thinking_is_not_recorded() {
    let record = json!({"type":"assistant","message":{"content":[
        {"type":"thinking","thinking":"","signature":"sig"}]}});
    assert_eq!(
        rows("claude", &record, Some(3)),
        vec![Row::Reasoning {
            text: None,
            tokens: Some(3)
        }]
    );
}

mod acceptance_review2 {
    use super::super::redact;

    #[test]
    fn r10_numeric_access_token_is_masked() {
        assert_eq!(
            redact(r#"{"access_token":"12345678"}"#),
            r#"{"access_token":"[redacted]"}"#
        );
    }

    #[test]
    fn r10_numeric_id_token_is_masked() {
        assert_eq!(redact("id_token=987654"), "id_token=[redacted]");
    }

    #[test]
    fn r10_numeric_refresh_token_is_masked() {
        assert_eq!(redact("refresh_token: 555555"), "refresh_token: [redacted]");
    }

    #[test]
    fn r10_explicit_usage_counts_remain_readable() {
        for text in [
            r#""input_tokens": 1234"#,
            "max_tokens=4096",
            "token_count: 99",
            r#""input_tokens": 12345678"#,
            "max_tokens=12345678",
            "token_count: 12345678",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn r10_secret_named_fields_are_not_usage_counts() {
        for key in ["password_tokens", "secret_token_count", "access_tokens"] {
            assert_eq!(
                redact(&format!("{key}=12345678")),
                format!("{key}=[redacted]"),
                "{key}"
            );
        }
    }
}

mod subagent {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_first_sidechain_line_is_the_brief_and_later_user_lines_are_not() {
        let brief = json!({"type":"user","isSidechain":true,"parentUuid":null,
            "message":{"role":"user","content":"  Port the contract.\nStop on a blocker.  "}});
        assert_eq!(
            rows("claude", &brief, None),
            vec![Row::Task("Port the contract.\nStop on a blocker.".into())]
        );
        let blocks = json!({"type":"user","isSidechain":true,"parentUuid":null,
            "message":{"content":[{"type":"text","text":"One"},{"type":"text","text":"Two"}]}});
        assert_eq!(
            rows("claude", &blocks, None),
            vec![Row::Task("One\nTwo".into())]
        );
        let later = json!({"type":"user","isSidechain":true,"parentUuid":"u1",
            "message":{"content":"a tool result"}});
        assert!(rows("claude", &later, None).is_empty());
        let main_thread = json!({"type":"user","isSidechain":false,"parentUuid":null,
            "message":{"content":"the operator's prompt"}});
        assert!(rows("claude", &main_thread, None).is_empty());
    }

    #[test]
    fn a_handback_is_what_the_subagent_says_not_a_tool_call() {
        let record = json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"SubagentHandback","input":{"message":"Commit f1eddd6 is done."}},
            {"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}});
        let rows = rows("claude", &record, None);
        assert_eq!(rows[0], Row::Assistant("Commit f1eddd6 is done.".into()));
        assert!(matches!(&rows[1], Row::Tool { name, .. } if name == "Bash"));
    }
}
