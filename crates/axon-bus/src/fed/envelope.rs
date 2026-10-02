//! What a remote message may carry (docs/P2P-SPEC.md §7, §8). The sender checks it before
//! queueing and the receiver checks it again: the receiver never trusts the sender.

use std::sync::OnceLock;

use regex::Regex;

/// The only kinds that cross; everything else is control and stays local.
pub const KINDS: [&str; 4] = ["sync", "question", "answer", "ack"];
pub const MAX_BODY_CHARS: usize = 400;
const MAX_REFS: usize = 8;
const MAX_REF_BYTES: usize = 256;
const MAX_THREAD_BYTES: usize = 128;
/// A message lives this long, from creation, and no longer.
pub const LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;
/// How far ahead of the receiver's clock a creation time may be.
pub const CLOCK_SKEW_MS: i64 = 120_000;

/// Line and paragraph separators and the bidirectional controls: they would let a body fake
/// a line or reorder the text around it in a renderer that honours them.
fn is_framing_char(c: char) -> bool {
    matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

pub fn body_ok(body: &str) -> bool {
    body.chars().count() <= MAX_BODY_CHARS
        && body
            .chars()
            .all(|c| (!c.is_control() || matches!(c, '\n' | '\t')) && !is_framing_char(c))
}

pub fn thread_ok(thread: &str) -> bool {
    !thread.is_empty()
        && thread.len() <= MAX_THREAD_BYTES
        && thread
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b':' | b'.' | b'_' | b'-'))
}

fn ref_shape() -> &'static Regex {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    SHAPE.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9._/-]+(:L[0-9]+(-[0-9]+)?)?(@[0-9a-f]{7,40})?$").expect("ref shape")
    })
}

/// A path-like pointer, metadata only: no `..` segment (also before a `:L` or `@` suffix),
/// no leading `/` and no leading `-` (which a tool would read as an option).
pub fn refs_ok(refs: &[String]) -> bool {
    refs.len() <= MAX_REFS
        && refs.iter().all(|r| {
            let path = r.split([':', '@']).next().unwrap_or_default();
            r.len() <= MAX_REF_BYTES
                && ref_shape().is_match(r)
                && !r.starts_with(['/', '-'])
                && !path.split('/').any(|segment| segment == "..")
        })
}

/// `8-4-4-4-12` hex, lowercase, as `uuid_v4` writes it.
pub fn is_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
}

/// A random (version 4) UUID.
pub fn uuid_v4() -> anyhow::Result<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_keep_newlines_and_tabs_but_no_other_control_characters() {
        assert!(body_ok("line one\n\tline two"));
        assert!(!body_ok("bell\u{7}"));
        assert!(!body_ok("escape\u{1b}[2J"));
        for framing in ['\u{2028}', '\u{2029}', '\u{202E}', '\u{2066}', '\u{2069}'] {
            assert!(!body_ok(&format!("a{framing}b")), "{framing:?}");
        }
        assert!(body_ok(&"é".repeat(MAX_BODY_CHARS)));
        assert!(!body_ok(&"é".repeat(MAX_BODY_CHARS + 1)));
    }

    #[test]
    fn refs_are_relative_paths_with_optional_line_and_commit() {
        let ok = |r: &str| refs_ok(&[r.to_owned()]);
        assert!(ok("src/lib.rs"));
        assert!(ok("src/lib.rs:L10-20@abcdef1"));
        assert!(!ok("/etc/passwd"));
        assert!(!ok("a/../b"));
        assert!(!ok("a/..:L10"), "a suffix does not hide a parent segment");
        assert!(!ok("..@abcdef1"));
        assert!(!ok("-rf"));
        assert!(!ok("a b"));
        assert!(!ok("https://example.invalid/x"));
        assert!(!refs_ok(&vec!["a".to_owned(); MAX_REFS + 1]));
    }

    #[test]
    fn threads_and_uuids_are_checked() {
        assert!(thread_ok("t:1.a_b-c"));
        assert!(!thread_ok("has space"));
        assert!(!thread_ok(&"x".repeat(MAX_THREAD_BYTES + 1)));
        let id = uuid_v4().unwrap();
        assert!(is_uuid(&id));
        assert!(!is_uuid("not-a-uuid"));
    }
}
