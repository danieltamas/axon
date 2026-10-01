//! What the service knows about each peer, in memory only (docs/P2P-SPEC.md §10): the live
//! state is derived here from timestamps, so ages stay current without database writes.

use serde_json::{json, Value};

/// `rtt_ms` is an exponentially weighted average with this weight on the newest sample.
const RTT_ALPHA: f64 = 0.3;
/// An rtt older than this is flagged stale.
pub const RTT_STALE_MS: i64 = 30_000;
/// No authenticated response for this long and a peer is `offline`.
pub const OFFLINE_AFTER_MS: i64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    Direct,
    Relay,
    None,
}

impl Path {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
            Self::None => "none",
        }
    }
}

/// One peer as the service sees it. `stored_state` is the `peers.state` column.
#[derive(Clone, Debug)]
pub struct PeerHealth {
    pub peer_id: String,
    pub label: String,
    pub fingerprint: String,
    pub stored_state: String,
    pub connected: bool,
    pub incompatible: bool,
    pub path: Path,
    pub last_handshake_at: Option<i64>,
    pub last_response_at: Option<i64>,
    pub rtt_ms: Option<f64>,
    pub rtt_at: Option<i64>,
    pub last_error: Option<String>,
    pub next_retry_at: Option<i64>,
}

impl PeerHealth {
    pub fn new(peer_id: String, label: String, fingerprint: String) -> Self {
        Self {
            peer_id,
            label,
            fingerprint,
            stored_state: String::new(),
            connected: false,
            incompatible: false,
            path: Path::None,
            last_handshake_at: None,
            last_response_at: None,
            rtt_ms: None,
            rtt_at: None,
            last_error: None,
            next_retry_at: None,
        }
    }

    pub fn record_rtt(&mut self, sample_ms: f64, now: i64) {
        self.rtt_ms = Some(match self.rtt_ms {
            Some(previous) => RTT_ALPHA * sample_ms + (1.0 - RTT_ALPHA) * previous,
            None => sample_ms,
        });
        self.rtt_at = Some(now);
    }

    /// The stored state is the floor: `paused`, `removed` and `pending_confirm` always win.
    pub fn live_state(&self, now: i64) -> &'static str {
        match self.stored_state.as_str() {
            "paused" => "paused",
            "removed" => "removed",
            "pending_confirm" => "pending_confirm",
            _ if self.incompatible => "incompatible",
            _ => match self.heartbeat_age_ms(now) {
                Some(age) if age <= OFFLINE_AFTER_MS && self.connected => "connected",
                Some(age) if age <= OFFLINE_AFTER_MS => "reconnecting",
                _ => "offline",
            },
        }
    }

    pub fn heartbeat_age_ms(&self, now: i64) -> Option<i64> {
        self.last_response_at.map(|at| (now - at).max(0))
    }

    /// The per-peer object of `GET /api/fed`, minus the parts the database owns.
    pub fn to_json(&self, now: i64) -> Value {
        json!({
            "peer_id": self.peer_id,
            "label": self.label,
            "fingerprint": self.fingerprint,
            "state": self.live_state(now),
            "path": self.path.as_str(),
            "last_handshake_at": self.last_handshake_at,
            "heartbeat_age_ms": self.heartbeat_age_ms(now),
            "rtt_ms": self.rtt_ms.map(|rtt| rtt.round() as i64),
            "rtt_stale": self.rtt_at.is_none_or(|at| now - at > RTT_STALE_MS),
            "last_error": self.last_error,
            "next_retry_at": self.next_retry_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(now: i64) -> PeerHealth {
        let mut peer = PeerHealth::new("p".into(), "alice".into(), "f".into());
        peer.stored_state = "active".into();
        peer.connected = true;
        peer.last_response_at = Some(now);
        peer
    }

    #[test]
    fn rtt_is_an_ewma_with_alpha_0_3() {
        let mut peer = active(0);
        peer.record_rtt(100.0, 0);
        peer.record_rtt(200.0, 1);
        assert!((peer.rtt_ms.unwrap() - 130.0).abs() < 1e-9);
    }

    #[test]
    fn rtt_goes_stale_after_30_s() {
        let mut peer = active(0);
        peer.record_rtt(50.0, 0);
        assert_eq!(peer.to_json(30_000)["rtt_stale"], false);
        assert_eq!(peer.to_json(30_001)["rtt_stale"], true);
        assert_eq!(peer.to_json(0)["rtt_ms"], 50);
    }

    #[test]
    fn live_state_follows_the_response_clock() {
        let mut peer = active(1_000);
        assert_eq!(peer.live_state(1_000), "connected");
        peer.connected = false;
        assert_eq!(peer.live_state(31_000), "reconnecting");
        assert_eq!(peer.live_state(31_001), "offline");
        peer.last_response_at = None;
        assert_eq!(peer.live_state(0), "offline");
    }

    #[test]
    fn the_stored_state_is_a_floor() {
        for stored in ["paused", "removed", "pending_confirm"] {
            let mut peer = active(0);
            peer.stored_state = stored.into();
            assert_eq!(peer.live_state(0), stored);
        }
        let mut peer = active(0);
        peer.incompatible = true;
        assert_eq!(peer.live_state(0), "incompatible");
    }
}
