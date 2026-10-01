//! The owner session (P2P-SPEC §1). `axon` and `axon open` write a single-use login nonce
//! into the database; the page trades it at `POST /api/session` for an `HttpOnly` cookie.
//! Nonces and sessions are stored only as SHA-256 hashes, so a copy of the database yields
//! no way in. A hash is looked up by its primary key, so no comparison of a secret against
//! another secret happens in the process and there is no timing to read.

use anyhow::Context;
use axum::http::{header, HeaderMap};
use rusqlite::{params, Connection};

use crate::{sha256, store};

pub const COOKIE: &str = "axon_session";
pub const SESSION_SECS: i64 = 30 * 24 * 3600;
const NONCE_MS: i64 = 60_000;
/// `last_used_at` moves at most this often, so a polling page does not write on every call.
const TOUCH_MS: i64 = 60_000;
/// The port a running server bound, so `open` can name it.
const PORT_KEY: &str = "dashboard_port";
const DEFAULT_PORT: u16 = 7777;

/// 32 bytes from the OS, base64url without padding: 43 characters.
fn random_secret() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(base64url(&bytes))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// A fresh nonce, valid for 60 s. Expired ones are swept on the way.
pub fn issue_nonce(conn: &Connection) -> anyhow::Result<String> {
    let nonce = random_secret()?;
    let now = store::now_ms();
    conn.execute("DELETE FROM login_nonces WHERE expires_at <= ?1", [now])?;
    conn.execute(
        "INSERT INTO login_nonces (hash, created_at, expires_at) VALUES (?1, ?2, ?3)",
        params![sha256::hex(nonce.as_bytes()), now, now + NONCE_MS],
    )?;
    Ok(nonce)
}

/// Trade a nonce for a session secret. A wrong, used or expired nonce is the same `None`;
/// the `DELETE` is what makes a nonce single-use even under concurrent exchanges.
pub fn exchange(conn: &Connection, nonce: &str) -> anyhow::Result<Option<String>> {
    let now = store::now_ms();
    let spent = conn.execute(
        "DELETE FROM login_nonces WHERE hash = ?1 AND expires_at > ?2",
        params![sha256::hex(nonce.as_bytes()), now],
    )?;
    if spent != 1 {
        return Ok(None);
    }
    let secret = random_secret()?;
    conn.execute(
        "INSERT INTO dashboard_sessions (hash, created_at, last_used_at, expires_at)
         VALUES (?1, ?2, ?2, ?3)",
        params![
            sha256::hex(secret.as_bytes()),
            now,
            now + SESSION_SECS * 1000
        ],
    )?;
    Ok(Some(secret))
}

/// Whether `secret` names a live session; refreshes `last_used_at` when it is stale.
pub fn valid(conn: &Connection, secret: &str) -> anyhow::Result<bool> {
    let now = store::now_ms();
    let hash = sha256::hex(secret.as_bytes());
    let last_used: Option<i64> = conn
        .query_row(
            "SELECT last_used_at FROM dashboard_sessions WHERE hash = ?1 AND expires_at > ?2",
            params![hash, now],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some(last_used) = last_used else {
        return Ok(false);
    };
    if now - last_used >= TOUCH_MS {
        conn.execute(
            "UPDATE dashboard_sessions SET last_used_at = ?2 WHERE hash = ?1",
            params![hash, now],
        )?;
    }
    Ok(true)
}

pub fn cookie_header(secret: &str) -> String {
    format!("{COOKIE}={secret}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_SECS}")
}

/// The session secret a request presents, if any.
pub fn presented(headers: &HeaderMap) -> Option<&str> {
    let prefix = format!("{COOKIE}=");
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|line| line.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(prefix.as_str()))
}

/// Remember the port this server bound, for `open`.
pub fn record_port(conn: &Connection, port: u16) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![PORT_KEY, port.to_string()],
    )?;
    Ok(())
}

pub fn login_url(port: u16, nonce: &str) -> String {
    format!("http://127.0.0.1:{port}/#login={nonce}")
}

/// `axon open` and `axon-bus open`: a fresh login link for the server that is running.
#[derive(clap::Args)]
pub struct OpenArgs {
    /// Print the link instead of opening the browser.
    #[arg(long)]
    pub print: bool,
    /// The dashboard's port (default: the one the last server recorded, else 7777).
    #[arg(long)]
    pub port: Option<u16>,
}

/// A login link for the server on `port`, with a fresh nonce written to `db`.
pub fn login_link(db: &std::path::Path, port: u16) -> anyhow::Result<String> {
    let conn = store::open(db).context("no hub; start `axon` first")?;
    Ok(login_url(port, &issue_nonce(&conn)?))
}

/// `open`'s link: the port given, else the one the last server recorded, else 7777.
pub fn open_link(db: &std::path::Path, args: &OpenArgs) -> anyhow::Result<String> {
    let recorded = store::open(db).ok().and_then(|conn| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [PORT_KEY],
            |r| r.get::<_, String>(0),
        )
        .ok()
    });
    let port = args
        .port
        .or_else(|| recorded.and_then(|text| text.parse().ok()))
        .unwrap_or(DEFAULT_PORT);
    login_link(db, port)
}

/// `axon open [--print] [--port N]`: print the link, and hand it to `opener` unless `--print`.
pub fn open_main(
    db: &std::path::Path,
    args: impl Iterator<Item = std::ffi::OsString>,
    opener: fn(&str),
) -> std::process::ExitCode {
    #[derive(clap::Parser)]
    #[command(name = "axon open")]
    struct OpenCli {
        #[command(flatten)]
        args: OpenArgs,
    }
    let cli = <OpenCli as clap::Parser>::parse_from(args);
    match open_link(db, &cli.args) {
        Ok(link) => {
            println!("{link}");
            if !cli.args.print {
                opener(&link);
            }
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Error: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::init(&dir.path().join("hub.db")).unwrap();
        (dir, conn)
    }

    #[test]
    fn base64url_is_43_unpadded_characters_for_32_bytes() {
        let text = base64url(&[0xfb; 32]);
        assert_eq!(text.len(), 43);
        assert!(text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_eq!(base64url(b"Ma"), "TWE");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn a_nonce_trades_once_for_a_session() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        let secret = exchange(&conn, &nonce).unwrap().expect("first exchange");
        assert_eq!(secret.len(), 43);
        assert!(valid(&conn, &secret).unwrap());
        assert!(exchange(&conn, &nonce).unwrap().is_none(), "single use");
        assert!(exchange(&conn, "wrong").unwrap().is_none());
        assert!(!valid(&conn, "wrong").unwrap());
    }

    #[test]
    fn expired_nonces_and_sessions_are_refused() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        conn.execute("UPDATE login_nonces SET expires_at = 1", [])
            .unwrap();
        assert!(exchange(&conn, &nonce).unwrap().is_none());
        let nonce = issue_nonce(&conn).unwrap();
        let secret = exchange(&conn, &nonce).unwrap().unwrap();
        conn.execute("UPDATE dashboard_sessions SET expires_at = 1", [])
            .unwrap();
        assert!(!valid(&conn, &secret).unwrap());
    }

    #[test]
    fn only_hashes_reach_the_database() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        let stored: String = conn
            .query_row("SELECT hash FROM login_nonces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, sha256::hex(nonce.as_bytes()));
        assert_ne!(stored, nonce);
    }

    #[test]
    fn the_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "a=1; axon_session=abc; b=2".parse().unwrap(),
        );
        assert_eq!(presented(&headers), Some("abc"));
    }
}
