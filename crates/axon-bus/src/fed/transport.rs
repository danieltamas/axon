//! The wire side of the service: who may connect, how an `axon/fed/1` connection is served,
//! and how this node keeps a heartbeat going to each peer it dials.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use iroh::endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{EndpointAddr, EndpointId};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};

use super::codec::{read_frame, write_frame};
use super::health::Path;
use super::service::{Access, Shared};
use super::{now_ms, FED_ALPN, PAIR_ALPN};

/// Close codes, so a peer's log says why it was dropped.
const CLOSE_NOT_A_PEER: u32 = 1;
const CLOSE_BAD_FRAME: u32 = 2;
const CLOSE_PAIR_UNAVAILABLE: u32 = 3;

pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A request that gets no whole answer in this time has failed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

pub type AccessMap = Arc<RwLock<HashMap<EndpointId, Access>>>;

/// Refuses an `axon/fed/1` connection from any node that is not a live peer, right after
/// the TLS handshake names it and before a stream can be opened.
#[derive(Debug)]
pub struct Gate(pub AccessMap);

impl EndpointHooks for Gate {
    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        let incoming_fed = conn.side().is_server() && conn.alpn() == FED_ALPN;
        if incoming_fed && !is_live_peer(&self.0, &conn.remote_id()) {
            return AfterHandshakeOutcome::Reject {
                error_code: VarInt::from_u32(CLOSE_NOT_A_PEER),
                reason: b"not a peer".to_vec(),
            };
        }
        AfterHandshakeOutcome::Accept
    }
}

fn is_live_peer(access: &AccessMap, node: &EndpointId) -> bool {
    access
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(node)
        .is_some_and(Access::is_live)
}

/// Serves `axon/fed/1`: one request per bidirectional stream, one response frame back.
pub struct FedProtocol(pub Arc<Shared>);

impl fmt::Debug for FedProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FedProtocol")
    }
}

impl ProtocolHandler for FedProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let node = conn.remote_id();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            // Re-checked per stream: a peer paused or removed since the handshake is cut off.
            let Some(access) = self.0.access_of(&node).filter(Access::is_live) else {
                conn.close(VarInt::from_u32(CLOSE_NOT_A_PEER), b"not a peer");
                break;
            };
            self.0.note_paths(&node, &conn);
            let (shared, conn) = (self.0.clone(), conn.clone());
            tokio::spawn(async move {
                let request = match timeout(REQUEST_TIMEOUT, read_frame(&mut recv)).await {
                    Ok(Ok(request)) => request,
                    // Oversized, malformed or stalled: drop the whole connection.
                    _ => return conn.close(VarInt::from_u32(CLOSE_BAD_FRAME), b"bad frame"),
                };
                let response = shared.dispatch(&node, &access, request).await;
                if write_frame(&mut send, &response).await.is_ok() {
                    let _ = send.finish();
                    // Hold the connection until the requester has read the answer.
                    let _ = timeout(REQUEST_TIMEOUT, send.stopped()).await;
                }
            });
        }
        Ok(())
    }
}

/// Pairing connections served at once. The listener cannot know the joiner beforehand, so
/// this bounds what an unknown dialer can hold open.
const PAIR_CONCURRENCY: usize = 8;
/// A pairing exchange that is not whole in this time is dropped.
const PAIR_TIMEOUT: Duration = Duration::from_secs(15);

/// Serves `axon/pair/1`: one request, one response, then the connection ends. The handler
/// (see `Handle::on_pair`) decides; with none registered the connection is refused.
pub struct PairProtocol {
    shared: Arc<Shared>,
    busy: Arc<Semaphore>,
}

impl PairProtocol {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            busy: Arc::new(Semaphore::new(PAIR_CONCURRENCY)),
        }
    }
}

impl fmt::Debug for PairProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairProtocol")
    }
}

impl ProtocolHandler for PairProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let (Ok(_slot), Some(handler)) = (
            self.busy.clone().try_acquire_owned(),
            self.shared.pair_handler(),
        ) else {
            conn.close(
                VarInt::from_u32(CLOSE_PAIR_UNAVAILABLE),
                b"pairing unavailable",
            );
            return Ok(());
        };
        // What the joiner's connection arrived on, so the inviter can dial back.
        let remote = EndpointAddr::from_parts(
            conn.remote_id(),
            conn.paths().iter().map(|p| p.remote_addr().clone()),
        );
        let served = async {
            let (mut send, mut recv) = conn.accept_bi().await.ok()?;
            let request = read_frame(&mut recv).await.ok()?;
            write_frame(&mut send, &handler(remote, request).await)
                .await
                .ok()?;
            send.finish().ok()?;
            // Hold the connection until the joiner has read the answer.
            let _ = send.stopped().await;
            Some(())
        };
        if !matches!(timeout(PAIR_TIMEOUT, served).await, Ok(Some(()))) {
            conn.close(VarInt::from_u32(CLOSE_BAD_FRAME), b"bad frame");
        }
        Ok(())
    }
}

/// Dial `addr` on `axon/pair/1`, send `frame` and read the one answer.
pub async fn pair_request(
    shared: &Shared,
    addr: EndpointAddr,
    frame: &Value,
) -> anyhow::Result<Value> {
    let conn = timeout(CONNECT_TIMEOUT, shared.endpoint.connect(addr, PAIR_ALPN))
        .await
        .map_err(|_| anyhow!("connect timed out"))?
        .context("connect")?;
    let reply = request(&conn, frame).await;
    conn.close(VarInt::from_u32(0), b"done");
    reply
}

/// One request and its single response on a fresh stream of `conn`.
pub async fn request(conn: &Connection, frame: &Value) -> anyhow::Result<Value> {
    let exchange = async {
        let (mut send, mut recv) = conn.open_bi().await.context("open a stream")?;
        write_frame(&mut send, frame).await?;
        send.finish().context("finish the request")?;
        Ok(read_frame(&mut recv).await?)
    };
    timeout(REQUEST_TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("request timed out"))?
}

/// Jittered exponential backoff: 1 s doubling to a 60 s cap, drawn from the upper half of
/// the step so a crowd of reconnecting peers spreads out. `jitter` is in `[0, 1)`.
pub fn backoff(failures: u32, jitter: f64) -> Duration {
    let step = BACKOFF_FIRST
        .saturating_mul(1u32.checked_shl(failures).unwrap_or(u32::MAX))
        .min(BACKOFF_CAP);
    step.mul_f64(0.5 + 0.5 * jitter)
}

fn jitter() -> f64 {
    let mut bytes = [0u8; 4];
    // Unpredictable jitter is a nicety; a failed read just means no spread.
    let _ = getrandom::getrandom(&mut bytes);
    f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
}

/// Keep a connection to `node` and a heartbeat over it, forever; the manager aborts this
/// task when the peer stops being live.
pub async fn dial_loop(shared: Arc<Shared>, node: EndpointId) {
    let mut failures = 0u32;
    loop {
        match session(&shared, node, &mut failures).await {
            Ok(()) => shared.update(&node, |h| h.last_error = Some("connection closed".into())),
            Err(err) => shared.update(&node, |h| h.last_error = Some(format!("{err:#}"))),
        }
        shared.drop_connection(&node);
        let delay = backoff(failures, jitter());
        failures = failures.saturating_add(1);
        let retry_at = now_ms() + delay.as_millis() as i64;
        shared.update(&node, |h| h.next_retry_at = Some(retry_at));
        sleep(delay).await;
    }
}

/// Dial once and ping until the connection fails. `failures` resets after the first pong,
/// so a peer that connects and then flaps does not climb to the cap.
async fn session(shared: &Arc<Shared>, node: EndpointId, failures: &mut u32) -> anyhow::Result<()> {
    let addr: EndpointAddr = shared.dial_address(node);
    // While this attempt is in flight the next one is due when it gives up.
    let gives_up = now_ms() + CONNECT_TIMEOUT.as_millis() as i64;
    shared.update(&node, |h| h.next_retry_at = Some(gives_up));
    let conn = timeout(CONNECT_TIMEOUT, shared.endpoint.connect(addr, FED_ALPN))
        .await
        .map_err(|_| anyhow!("connect timed out"))?
        .context("connect")?;
    shared.set_connection(node, conn.clone());
    shared.update(&node, |h| {
        h.connected = true;
        h.next_retry_at = None;
        h.last_handshake_at = Some(now_ms());
    });

    let mut beat = interval(HEARTBEAT_EVERY);
    beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = conn.closed() => return Ok(()),
            _ = beat.tick() => {
                ping(shared, node, &conn).await?;
                *failures = 0;
            }
        }
    }
}

async fn ping(shared: &Arc<Shared>, node: EndpointId, conn: &Connection) -> anyhow::Result<()> {
    let generation = shared.access_of(&node).map_or(0, |a| a.generation);
    let sent_at = Instant::now();
    let reply = request(
        conn,
        &json!({"type": "ping", "v": 1, "generation": generation, "t": now_ms()}),
    )
    .await?;
    let rtt_ms = sent_at.elapsed().as_secs_f64() * 1000.0;
    let path = selected_path(conn);
    shared.note_paths(&node, conn);
    match reply["type"].as_str() {
        Some("pong") => shared.update(&node, |h| {
            let now = now_ms();
            h.last_response_at = Some(now);
            h.record_rtt(rtt_ms, now);
            h.path = path;
            h.last_error = None;
            h.incompatible = false;
        }),
        _ => {
            let reason = reply["reason"]
                .as_str()
                .unwrap_or("unexpected reply")
                .to_owned();
            shared.update(&node, |h| {
                h.incompatible = reason == "unsupported_version";
                h.last_error = Some(reason);
            });
        }
    }
    Ok(())
}

fn selected_path(conn: &Connection) -> Path {
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) if p.is_ip() => Path::Direct,
        Some(_) => Path::Relay,
        None => Path::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_one_second_and_caps_at_sixty() {
        let steps: Vec<u64> = (0..9).map(|n| backoff(n, 0.999).as_secs()).collect();
        assert_eq!(steps, [0, 1, 3, 7, 15, 31, 59, 59, 59]);
    }

    #[test]
    fn backoff_jitter_stays_in_the_upper_half_of_the_step() {
        assert_eq!(backoff(3, 0.0), Duration::from_secs(4));
        assert!(backoff(3, 0.9999) < Duration::from_secs(8));
        assert_eq!(backoff(40, 0.0), Duration::from_secs(30));
    }
}
