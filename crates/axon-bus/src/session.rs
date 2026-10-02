//! The owner session (P2P-SPEC §1). `axon` and `axon open` write a single-use login nonce
//! into the database; the page trades it at `POST /api/session` for an `HttpOnly` cookie.
//! Nonces and sessions are stored only as SHA-256 hashes, so a copy of the database yields
//! no way in. A hash is looked up by its primary key, so no comparison of a secret against
//! another secret happens in the process and there is no timing to read.
//!
//! A cookie alone is not proof of the owner: browsers send it to every port of 127.0.0.1.
//! Each session therefore also has a token, handed to the page once at login (kept in its
//! origin-scoped `localStorage`) and sent on every request as `x-axon-session`, so a server
//! on another port that receives the cookie still cannot use it.

use anyhow::Context;
use axum::http::{header, HeaderMap};
use rusqlite::{params, Connection};

use sha2::{Digest, Sha256};

use crate::store;

pub const COOKIE: &str = "axon_session";
pub const TOKEN_HEADER: &str = "x-axon-session";
pub const SESSION_SECS: i64 = 30 * 24 * 3600;
const NONCE_MS: i64 = 60_000;
/// `last_used_at` moves at most this often, so a polling page does not write on every call.
const TOUCH_MS: i64 = 60_000;
/// The port a running server bound, so `open` can name it.
const PORT_KEY: &str = "dashboard_port";
const DEFAULT_PORT: u16 = 7777;

/// The digest of `input` as 64 lowercase hex characters.
pub(crate) fn sha256_hex(input: &[u8]) -> String {
    Sha256::digest(input)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 32 bytes from the OS, base64url without padding: 43 characters.
pub(crate) fn random_secret() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(base64url(&bytes))
}

pub(crate) const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub(crate) fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(BASE64URL_ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
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
        "INSERT INTO login_nonces (nonce_hash, expires_at) VALUES (?1, ?2)",
        params![sha256_hex(nonce.as_bytes()), now + NONCE_MS],
    )?;
    Ok(nonce)
}

/// A database that held sessions before they had tokens keeps them, unusable: with an empty
/// token hash they match nothing, so their browsers sign in again.
fn add_token_column_if_missing(conn: &Connection) -> rusqlite::Result<()> {
    let has: bool = conn.query_row(
        "SELECT count(*) > 0 FROM pragma_table_info('dashboard_sessions') WHERE name = 'token_hash'",
        [],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute(
            "ALTER TABLE dashboard_sessions ADD COLUMN token_hash TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    Ok(())
}

/// What a login hands the browser: the cookie's secret and the token for the header.
pub struct Granted {
    pub secret: String,
    pub token: String,
}

/// Trade a nonce for a session. A wrong, used or expired nonce is the same `None`; the
/// `DELETE` is what makes a nonce single-use even under concurrent exchanges.
pub fn exchange(conn: &Connection, nonce: &str) -> anyhow::Result<Option<Granted>> {
    add_token_column_if_missing(conn)?;
    let now = store::now_ms();
    let spent = conn.execute(
        "DELETE FROM login_nonces WHERE nonce_hash = ?1 AND expires_at > ?2",
        params![sha256_hex(nonce.as_bytes()), now],
    )?;
    if spent != 1 {
        return Ok(None);
    }
    let granted = Granted {
        secret: random_secret()?,
        token: random_secret()?,
    };
    conn.execute(
        "INSERT INTO dashboard_sessions (session_hash, token_hash, created_at, last_used_at, expires_at)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        params![
            sha256_hex(granted.secret.as_bytes()),
            sha256_hex(granted.token.as_bytes()),
            now,
            now + SESSION_SECS * 1000
        ],
    )?;
    Ok(Some(granted))
}

/// The `last_used_at` of the one live session `secret` and `token` together name. A pure
/// read, so it cannot wait on a writer. The cookie without its token, or the reverse, is nobody.
pub fn live(conn: &Connection, secret: &str, token: &str) -> anyhow::Result<Option<i64>> {
    conn.query_row(
        "SELECT last_used_at FROM dashboard_sessions
         WHERE session_hash = ?1 AND token_hash = ?2 AND expires_at > ?3",
        params![
            sha256_hex(secret.as_bytes()),
            sha256_hex(token.as_bytes()),
            store::now_ms()
        ],
        |row| row.get(0),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other.into()),
    })
}

/// Whether the credentials name a live session; refreshes `last_used_at` when it is stale.
pub fn valid(conn: &Connection, secret: &str, token: &str) -> anyhow::Result<bool> {
    let Some(last_used) = live(conn, secret, token)? else {
        return Ok(false);
    };
    let now = store::now_ms();
    if now - last_used >= TOUCH_MS {
        conn.execute(
            "UPDATE dashboard_sessions SET last_used_at = ?2 WHERE session_hash = ?1",
            params![sha256_hex(secret.as_bytes()), now],
        )?;
    }
    Ok(true)
}

/// Sign out every browser but the one holding `secret`.
pub fn revoke_others(conn: &Connection, secret: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM dashboard_sessions WHERE session_hash <> ?1",
        [sha256_hex(secret.as_bytes())],
    )
    .map(|_| ())
}

/// Sessions that can still sign in.
pub fn live_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT count(*) FROM dashboard_sessions WHERE expires_at > ?1",
        [store::now_ms()],
        |r| r.get(0),
    )
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

/// The token a request presents: the header, or for the event stream (which cannot set
/// headers) `?t=`. Only that one path reads the query, so a token never rides in other URLs.
pub fn presented_token<'a>(headers: &'a HeaderMap, uri: &'a axum::http::Uri) -> Option<&'a str> {
    if let Some(value) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) {
        return Some(value);
    }
    if uri.path() != "/api/stream" {
        return None;
    }
    uri.query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
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

/// `record_port` for a caller that has no connection yet (the root binary, before it binds).
pub fn remember_port(db: &std::path::Path, port: u16) -> anyhow::Result<()> {
    record_port(&store::open(db)?, port)
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
            println!("Dashboard: {link}");
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
    fn live_reads_a_session_without_touching_it() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        let granted = exchange(&conn, &nonce).unwrap().unwrap();
        conn.execute("UPDATE dashboard_sessions SET last_used_at = 1", [])
            .unwrap();
        assert_eq!(
            live(&conn, &granted.secret, &granted.token).unwrap(),
            Some(1)
        );
        assert_eq!(live(&conn, &granted.secret, "x").unwrap(), None);
        assert!(valid(&conn, &granted.secret, &granted.token).unwrap());
        assert_ne!(
            live(&conn, &granted.secret, &granted.token).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn a_nonce_trades_once_for_a_session() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        let granted = exchange(&conn, &nonce).unwrap().expect("first exchange");
        assert_eq!(granted.secret.len(), 43);
        assert_eq!(granted.token.len(), 43);
        assert_ne!(granted.secret, granted.token);
        assert!(valid(&conn, &granted.secret, &granted.token).unwrap());
        assert!(exchange(&conn, &nonce).unwrap().is_none(), "single use");
        assert!(exchange(&conn, "wrong").unwrap().is_none());
        assert!(!valid(&conn, "wrong", &granted.token).unwrap());
    }

    #[test]
    fn the_cookie_without_its_token_is_nobody() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        let granted = exchange(&conn, &nonce).unwrap().unwrap();
        assert!(!valid(&conn, &granted.secret, "").unwrap());
        assert!(!valid(&conn, &granted.secret, &granted.secret).unwrap());
        let other = exchange(&conn, &issue_nonce(&conn).unwrap())
            .unwrap()
            .unwrap();
        assert!(
            !valid(&conn, &granted.secret, &other.token).unwrap(),
            "token of another session"
        );
    }

    #[test]
    fn sessions_from_before_tokens_stay_signed_out() {
        let (_dir, conn) = hub();
        conn.execute_batch(
            "DROP TABLE dashboard_sessions;
             CREATE TABLE dashboard_sessions (session_hash TEXT PRIMARY KEY, created_at INTEGER NOT NULL,
               last_used_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
             INSERT INTO dashboard_sessions VALUES ('old', 1, 1, 9999999999999);",
        )
        .unwrap();
        let granted = exchange(&conn, &issue_nonce(&conn).unwrap())
            .unwrap()
            .unwrap();
        assert!(valid(&conn, &granted.secret, &granted.token).unwrap());
        assert!(!valid(&conn, "old", "").unwrap());
    }

    #[test]
    fn revoking_a_session_ends_its_token() {
        let (_dir, conn) = hub();
        let mine = exchange(&conn, &issue_nonce(&conn).unwrap())
            .unwrap()
            .unwrap();
        let theirs = exchange(&conn, &issue_nonce(&conn).unwrap())
            .unwrap()
            .unwrap();
        revoke_others(&conn, &mine.secret).unwrap();
        assert!(valid(&conn, &mine.secret, &mine.token).unwrap());
        assert!(!valid(&conn, &theirs.secret, &theirs.token).unwrap());
    }

    #[test]
    fn expired_nonces_and_sessions_are_refused() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        conn.execute("UPDATE login_nonces SET expires_at = 1", [])
            .unwrap();
        assert!(exchange(&conn, &nonce).unwrap().is_none());
        let nonce = issue_nonce(&conn).unwrap();
        let granted = exchange(&conn, &nonce).unwrap().unwrap();
        conn.execute("UPDATE dashboard_sessions SET expires_at = 1", [])
            .unwrap();
        assert!(!valid(&conn, &granted.secret, &granted.token).unwrap());
    }

    #[test]
    fn only_hashes_reach_the_database() {
        let (_dir, conn) = hub();
        let nonce = issue_nonce(&conn).unwrap();
        let stored: String = conn
            .query_row("SELECT nonce_hash FROM login_nonces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, sha256_hex(nonce.as_bytes()));
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
