//! The federation service (docs/P2P-SPEC.md §3): one per data dir, started by `axon` and
//! by `serve::run` through `start`. It owns the iroh endpoint, keeps a heartbeat to each
//! live peer, and keeps their health in memory for `GET /api/fed`.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{bail, Context};
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{presets, Connection};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use super::health::PeerHealth;
use super::lock::{self, Acquire, ServiceLock};
use super::transport::{self, AccessMap, FedProtocol, Gate, PairProtocol};
use super::{identity, now_ms, seam, FED_ALPN, PAIR_ALPN};
use crate::store;

/// Health is pushed at least this often while a peer is live, so ages stay current.
const PUSH_EVERY: Duration = Duration::from_secs(5);

/// A handler for one frame `type` on `axon/fed/1`: gets the sender's node id and the
/// request, returns the one response frame.
pub type FrameHandler =
    Arc<dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync>;

/// The inviter's handler for the one request of `axon/pair/1`: gets the joiner's observed
/// address (its id plus the paths the connection arrived on) and the request.
pub type PairHandler =
    Arc<dyn Fn(EndpointAddr, Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync>;

/// What the gate and the dispatcher need to know about a peer row.
#[derive(Clone, Debug)]
pub struct Access {
    pub peer_id: String,
    pub generation: i64,
    pub state: String,
}

impl Access {
    /// Only these states may talk to us; `paused` and `removed` are cut off.
    pub fn is_live(&self) -> bool {
        matches!(self.state.as_str(), "active" | "pending_confirm")
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Relay {
    Default,
    Disabled,
    Custom(RelayUrl),
}

impl Relay {
    fn parse(setting: &str) -> anyhow::Result<Self> {
        if seam("AXON_FED_RELAY").as_deref() == Some("disabled") {
            return Ok(Self::Disabled);
        }
        match setting {
            "default" => Ok(Self::Default),
            url => Ok(Self::Custom(url.parse().with_context(|| {
                format!("fed_relay {url:?} is neither \"default\" nor a URL")
            })?)),
        }
    }
}

/// State shared by the accept side, the dial tasks and the manager.
pub struct Shared {
    db_path: PathBuf,
    pub(super) endpoint: Endpoint,
    relay: Relay,
    lookup: MemoryLookup,
    access: AccessMap,
    health: Mutex<HashMap<EndpointId, PeerHealth>>,
    connections: Mutex<HashMap<EndpointId, Connection>>,
    handlers: RwLock<HashMap<String, FrameHandler>>,
    pair_handler: RwLock<Option<PairHandler>>,
    changed: watch::Sender<u64>,
    reload: Notify,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ping {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    t: i64,
}

impl Shared {
    pub(super) fn access_of(&self, node: &EndpointId) -> Option<Access> {
        self.access
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(node)
            .cloned()
    }

    pub(super) fn update(&self, node: &EndpointId, change: impl FnOnce(&mut PeerHealth)) {
        if let Some(peer) = locked(&self.health).get_mut(node) {
            change(peer);
        }
        self.changed.send_modify(|version| *version += 1);
    }

    pub(super) fn pair_handler(&self) -> Option<PairHandler> {
        let handler = self.pair_handler.read();
        handler.unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn add_addr(&self, addr: EndpointAddr) {
        self.lookup.add_endpoint_info(addr);
        self.reload.notify_one();
    }

    pub(super) fn set_connection(&self, node: EndpointId, conn: Connection) {
        locked(&self.connections).insert(node, conn);
    }

    pub(super) fn drop_connection(&self, node: &EndpointId) {
        if let Some(conn) = locked(&self.connections).remove(node) {
            conn.close(0u32.into(), b"bye");
        }
        self.update(node, |h| h.connected = false);
    }

    /// With a self-hosted relay the peer is reachable through it; with the default relays
    /// discovery finds the peer's home relay from its node id alone.
    pub(super) fn dial_address(&self, node: EndpointId) -> EndpointAddr {
        match &self.relay {
            Relay::Custom(url) => EndpointAddr::new(node).with_relay_url(url.clone()),
            _ => EndpointAddr::new(node),
        }
    }

    /// Answer one request: built-in `ping`, else the handler registered for its `type`.
    pub(super) async fn dispatch(
        &self,
        node: &EndpointId,
        access: &Access,
        request: Value,
    ) -> Value {
        let kind = request["type"].as_str().unwrap_or_default().to_owned();
        if kind == "ping" {
            return ping_reply(access, request);
        }
        let handler = self
            .handlers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&kind)
            .cloned();
        match handler {
            Some(handler) => handler(node.to_string(), request).await,
            None => json!({"type": "error", "reason": "unknown_frame"}),
        }
    }

    fn notify_if_live(&self) {
        let any_live = self
            .access
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .any(Access::is_live);
        if any_live {
            self.changed.send_modify(|version| *version += 1);
        }
    }
}

fn ping_reply(access: &Access, request: Value) -> Value {
    let Ok(ping) = serde_json::from_value::<Ping>(request) else {
        return json!({"type": "error", "reason": "bad_frame"});
    };
    if ping.v != 1 {
        json!({"type": "error", "reason": "unsupported_version"})
    } else if ping.generation != access.generation {
        json!({"type": "error", "reason": "stale_generation"})
    } else {
        json!({"type": "pong", "t": ping.t})
    }
}

struct PeerRow {
    peer_id: String,
    node_id: String,
    label: String,
    generation: i64,
    state: String,
}

fn load_peers(db_path: &Path) -> anyhow::Result<Vec<PeerRow>> {
    let conn = store::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT peer_id, node_id, label, generation, state FROM peers WHERE state <> 'removed'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PeerRow {
            peer_id: r.get(0)?,
            node_id: r.get(1)?,
            label: r.get(2)?,
            generation: r.get(3)?,
            state: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Re-read the peers table: refresh the gate, the health entries, and start or stop the
/// dial task of each peer as it becomes live or stops being so.
async fn sync_peers(shared: &Arc<Shared>, dialers: &mut HashMap<EndpointId, JoinHandle<()>>) {
    let db_path = shared.db_path.clone();
    let rows = match tokio::task::spawn_blocking(move || load_peers(&db_path)).await {
        Ok(Ok(rows)) => rows,
        Ok(Err(err)) => return eprintln!("axon-bus: federation could not read peers: {err:#}"),
        Err(_) => return,
    };
    let mut access = HashMap::new();
    for row in rows {
        let Ok(node) = row.node_id.parse::<EndpointId>() else {
            eprintln!(
                "axon-bus: peer {} has an unreadable node id; ignored",
                row.peer_id
            );
            continue;
        };
        {
            let mut health = locked(&shared.health);
            let entry = health.entry(node).or_insert_with(|| {
                let print = identity::fingerprint(&node);
                PeerHealth::new(row.peer_id.clone(), row.label.clone(), print)
            });
            entry.label.clone_from(&row.label);
            entry.stored_state.clone_from(&row.state);
        }
        access.insert(
            node,
            Access {
                peer_id: row.peer_id,
                generation: row.generation,
                state: row.state,
            },
        );
    }
    locked(&shared.health).retain(|node, _| access.contains_key(node));
    for (node, entry) in &access {
        if entry.is_live() {
            dialers
                .entry(*node)
                .or_insert_with(|| tokio::spawn(transport::dial_loop(shared.clone(), *node)));
        }
    }
    dialers.retain(|node, task| {
        let keep = access.get(node).is_some_and(Access::is_live);
        if !keep {
            task.abort();
            shared.drop_connection(node);
        }
        keep
    });
    *shared.access.write().unwrap_or_else(|p| p.into_inner()) = access;
    shared.changed.send_modify(|version| *version += 1);
}

async fn manage(shared: Arc<Shared>) {
    let mut dialers = HashMap::new();
    let mut push = tokio::time::interval(PUSH_EVERY);
    loop {
        tokio::select! {
            _ = shared.reload.notified() => sync_peers(&shared, &mut dialers).await,
            _ = push.tick() => shared.notify_if_live(),
        }
    }
}

/// A running service. Cloning shares it; `shutdown` stops it for every clone.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
    stop: Arc<Stop>,
    relay_setting: Arc<str>,
}

struct Stop {
    router: Router,
    manager: Mutex<Option<JoinHandle<()>>>,
    lock: Mutex<Option<ServiceLock>>,
}

impl Handle {
    /// This node's address (its id plus bound sockets and relay), for invites.
    pub fn addr(&self) -> EndpointAddr {
        self.shared.endpoint.addr()
    }

    /// `GET /api/fed`'s node and peer health, derived from memory at call time.
    pub fn health(&self) -> Value {
        let now = now_ms();
        let mut peers: Vec<_> = locked(&self.shared.health).values().cloned().collect();
        peers.sort_by(|a, b| a.label.cmp(&b.label));
        json!({
            "enabled": true,
            "node_id": self.shared.endpoint.id().to_string(),
            "fingerprint": identity::fingerprint(&self.shared.endpoint.id()),
            "relay": &*self.relay_setting,
            "peers": peers.iter().map(|p| p.to_json(now)).collect::<Vec<_>>(),
        })
    }

    /// Fires on every health change, and at least every 5 s while a peer is live.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.changed.subscribe()
    }

    /// Re-read the peers after the database changed. A changed `fed_relay` applies on the
    /// next `start`: the relay and discovery wiring are fixed when the endpoint is built.
    pub fn reload(&self) {
        self.shared.reload.notify_one();
    }

    /// Register the handler of one frame `type` on `axon/fed/1` (`ping` is built in).
    pub fn on_frame(&self, kind: &str, handler: FrameHandler) {
        self.shared
            .handlers
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(kind.to_owned(), handler);
    }

    /// Send one request frame to a connected peer and wait for its one response.
    pub async fn request(&self, node: &EndpointId, frame: &Value) -> anyhow::Result<Value> {
        let conn = locked(&self.shared.connections).get(node).cloned();
        let Some(conn) = conn else {
            bail!("peer is not connected");
        };
        transport::request(&conn, frame).await
    }

    /// Register the handler of `axon/pair/1`; without one every pairing connection is closed.
    pub fn on_pair(&self, handler: PairHandler) {
        let slot = self.shared.pair_handler.write();
        *slot.unwrap_or_else(|p| p.into_inner()) = Some(handler);
    }

    /// Dial `addr` on `axon/pair/1` and exchange one request for one response. iroh checks
    /// the remote key against `addr.id` before a byte is sent, so the pin is verified first.
    pub async fn pair_request(&self, addr: EndpointAddr, frame: &Value) -> anyhow::Result<Value> {
        transport::pair_request(&self.shared, addr, frame).await
    }

    /// A view for handlers stored inside the service: a `Handle` there would keep the
    /// service alive through its own handler table.
    pub fn link(&self) -> Link {
        Link(Arc::downgrade(&self.shared))
    }

    /// True once `shutdown` ran; background work tied to this service ends then.
    pub fn stopped(&self) -> bool {
        locked(&self.stop.manager).is_none()
    }

    /// Where `node` may be dialed besides discovery, e.g. the addresses in an invite.
    pub fn add_addr(&self, addr: EndpointAddr) {
        self.shared.add_addr(addr);
    }

    pub async fn shutdown(&self) {
        if let Some(manager) = locked(&self.stop.manager).take() {
            manager.abort();
        }
        let _ = self.stop.router.shutdown().await;
        locked(&self.stop.lock).take();
    }
}

/// Start federation for this data dir, or return `None` when it is off, when another
/// `axon` on the same data dir already runs it, or when it cannot be started safely (each
/// logged). `data_dir` holds `fed/`; `db_path` is the hub database.
pub async fn start(data_dir: &Path, db_path: &Path) -> Option<Handle> {
    match try_start(data_dir, db_path).await {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("axon-bus: federation not started: {err:#}");
            None
        }
    }
}

async fn try_start(data_dir: &Path, db_path: &Path) -> anyhow::Result<Option<Handle>> {
    let conn = store::open(db_path)?;
    if !super::enabled(&conn) {
        return Ok(None);
    }
    let relay_setting = super::relay_setting(&conn);
    let relay = Relay::parse(&relay_setting)?;
    let peers_exist = super::live_peer_count(&conn)? > 0;
    let fed_dir = identity::fed_dir(data_dir);
    identity::ensure_private_dir(&fed_dir)?;
    let lock = match lock::acquire(&fed_dir).context("take the federation lock")? {
        Acquire::Held(lock) => lock,
        Acquire::HeldBy(holder) => {
            eprintln!("axon-bus: federation already runs for this data dir ({holder})");
            return Ok(None);
        }
    };
    let key = identity::load_or_create(data_dir, peers_exist).inspect_err(|_| {
        // Surfaced per peer in Settings; the owner decides, nothing is regenerated.
        let _ = conn.execute(
            "UPDATE peers SET last_error=?1 WHERE state<>'removed'",
            [identity::KEY_LOST],
        );
    })?;
    drop(conn);

    let access: AccessMap = Arc::default();
    let lookup = MemoryLookup::new();
    let mut builder = match relay {
        Relay::Default => Endpoint::builder(presets::N0),
        _ => Endpoint::builder(presets::Minimal),
    }
    .secret_key(key)
    .address_lookup(lookup.clone())
    .hooks(Gate(access.clone()));
    match &relay {
        Relay::Default => {}
        Relay::Disabled => builder = builder.relay_mode(RelayMode::Disabled),
        Relay::Custom(url) => builder = builder.relay_mode(RelayMode::custom([url.clone()])),
    }
    if let Some(bind) = seam("AXON_FED_BIND") {
        let bind: SocketAddr = bind.parse().context("AXON_FED_BIND")?;
        builder = builder.clear_ip_transports().bind_addr(bind)?;
    }
    let endpoint = builder
        .bind()
        .await
        .context("bind the federation endpoint")?;

    let (changed, _) = watch::channel(0);
    let shared = Arc::new(Shared {
        db_path: db_path.to_owned(),
        endpoint: endpoint.clone(),
        relay,
        lookup,
        access,
        health: Mutex::default(),
        connections: Mutex::default(),
        handlers: RwLock::default(),
        pair_handler: RwLock::default(),
        changed,
        reload: Notify::new(),
    });
    let router = Router::builder(endpoint)
        .accept(PAIR_ALPN, PairProtocol::new(shared.clone()))
        .accept(FED_ALPN, FedProtocol(shared.clone()))
        .spawn();
    shared.reload.notify_one();
    let manager = tokio::spawn(manage(shared.clone()));
    Ok(Some(Handle {
        shared,
        stop: Arc::new(Stop {
            router,
            manager: Mutex::new(Some(manager)),
            lock: Mutex::new(Some(lock)),
        }),
        relay_setting: relay_setting.into(),
    }))
}

mod link;
pub use link::Link;
#[cfg(test)]
mod tests;
