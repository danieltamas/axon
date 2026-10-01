//! Credential masking for every captured text before it is stored or shown.

/// Credential shapes, masked before anything reaches the database. Each pattern's last
/// group is the secret; anything before it (a key name, `Bearer`, a URL's user) stays.
fn secret_patterns() -> &'static [regex::Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Vendor-prefixed API keys and tokens.
            r"\b((?:sk-|sk_live_|sk_test_|rk_live_|rk_test_|ghp_|gho_|ghs_|ghu_|ghr_|github_pat_|glpat-|xox[abpsr]-|xapp-|npm_|hf_|AKIA|ASIA|AIza|ya29\.)[A-Za-z0-9_\-.]{12,})",
            // JSON Web Tokens.
            r"\b(eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})",
            // Authorization headers.
            r"(?i)\b(?:bearer|basic|token)\s+([A-Za-z0-9_\-.=+/]{16,})",
            // Passwords in URLs: scheme://user:secret@host.
            r"[A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]+:([^@\s/]+)@",
            // Assignments to secret-named keys: PASSWORD=…, api_key: "…".
            // The key may be quoted, as in JSON: {"password":"…"}.
            r#"(?i)\b[A-Z0-9_]*(?:password|passwd|secret|token|api_?key|access_?key|private_?key|credential)[A-Z0-9_]*["']?\s*[=:]\s*["'`]?([^\s"'`,}]{6,})"#,
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("valid secret pattern"))
        .collect()
    })
}

pub fn redact(text: &str) -> String {
    if text.contains("PRIVATE KEY-----") {
        return "[redacted: private key]".to_owned();
    }
    let mut out = text.to_owned();
    for pattern in secret_patterns() {
        out = pattern
            .replace_all(&out, |caps: &regex::Captures| {
                let whole = caps.get(0).expect("match");
                let secret = caps.iter().flatten().last().expect("secret group");
                let head = &out[whole.start()..secret.start()];
                // A bare number after `input_tokens:` is a count; after `access_token=` or
                // `password=` it is a secret, so only count-shaped key names are exempt.
                let key = head
                    .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if is_usage_count(&key) && secret.as_str().bytes().all(|b| b.is_ascii_digit()) {
                    return whole.as_str().to_owned();
                }
                let tail = &out[secret.end()..whole.end()];
                format!("{head}[redacted]{tail}")
            })
            .into_owned();
    }
    out
}

/// Usage fields that hold token counts: `tokens`, `token_count`, or `<kind>_tokens` /
/// `<kind>_token_count` for the kinds the harnesses report. `access_tokens` is not one.
fn is_usage_count(key: &str) -> bool {
    const KINDS: [&str; 8] = [
        "input",
        "output",
        "prompt",
        "completion",
        "total",
        "max",
        "cached",
        "reasoning",
    ];
    if key == "tokens" || key == "token_count" {
        return true;
    }
    let stem = key
        .strip_suffix("_tokens")
        .or_else(|| key.strip_suffix("_token_count"));
    stem.and_then(|stem| stem.rsplit('_').next())
        .is_some_and(|kind| KINDS.contains(&kind))
}
