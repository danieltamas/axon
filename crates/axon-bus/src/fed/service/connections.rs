//! The connections of each peer: the one sends go over, and every one being served (a
//! revocation closes them all). See `transport::serve` for how a connection is adopted.

use iroh::endpoint::Connection;
use iroh::EndpointId;

use super::{locked, Access, Shared};

impl Shared {
    pub(in crate::fed) fn set_connection(&self, node: EndpointId, conn: Connection) {
        locked(&self.connections).insert(node, conn);
    }

    /// A peer that connected to us is alive at that address now, which a connection to its
    /// former process is not: send over the newest connection, whichever side opened it.
    pub(in crate::fed) fn adopt_connection(&self, node: EndpointId, conn: &Connection) {
        let mut connections = locked(&self.connections);
        if connections.get(&node).map(Connection::stable_id) != Some(conn.stable_id()) {
            connections.insert(node, conn.clone());
        }
    }

    /// The send path to `node` if it is still open.
    pub(in crate::fed) fn live_connection(&self, node: &EndpointId) -> Option<Connection> {
        locked(&self.connections)
            .get(node)
            .filter(|conn| conn.close_reason().is_none())
            .cloned()
    }

    /// An adopted connection ended: stop sending over it unless a newer one replaced it. The
    /// session's heartbeat puts its own connection back (`vouch_for`).
    pub(in crate::fed) fn forget_connection(&self, node: &EndpointId, conn: &Connection) {
        let mut connections = locked(&self.connections);
        if connections.get(node).map(Connection::stable_id) == Some(conn.stable_id()) {
            connections.remove(node);
        }
    }

    /// A heartbeat answered on `conn`: the peer is connected, and sends have a path again if
    /// an adopted connection that carried them has since ended.
    pub(in crate::fed) fn vouch_for(&self, node: &EndpointId, conn: &Connection) {
        locked(&self.connections)
            .entry(*node)
            .or_insert_with(|| conn.clone());
    }

    /// A session ended: close its connection and forget it unless a newer one replaced it.
    pub(in crate::fed) fn release_connection(&self, node: &EndpointId, conn: &Connection) {
        conn.close(0u32.into(), b"bye");
        self.forget_connection(node, conn);
        self.update(node, |h| h.connected = false);
    }

    pub(in crate::fed) fn track(&self, node: EndpointId, conn: &Connection) {
        locked(&self.open)
            .entry(node)
            .or_default()
            .push(conn.clone());
    }

    pub(in crate::fed) fn untrack(&self, node: &EndpointId, conn: &Connection) {
        let mut open = locked(&self.open);
        if let Some(conns) = open.get_mut(node) {
            conns.retain(|c| c.stable_id() != conn.stable_id());
            if conns.is_empty() {
                open.remove(node);
            }
        }
    }

    /// Close every connection of `node`, the send path and the older or probing ones alike;
    /// their handlers end with them.
    pub(in crate::fed) fn drop_connection(&self, node: &EndpointId) {
        let open = locked(&self.open).remove(node).unwrap_or_default();
        let sending = locked(&self.connections).remove(node);
        for conn in open.into_iter().chain(sending) {
            conn.close(0u32.into(), b"bye");
        }
        self.update(node, |h| h.connected = false);
    }

    /// Close the connections of every peer that is not live now.
    pub(in crate::fed) fn drop_unlive_connections(&self) {
        let nodes: Vec<EndpointId> = locked(&self.open)
            .keys()
            .chain(locked(&self.connections).keys())
            .copied()
            .collect();
        for node in nodes {
            if self.access_of(&node).filter(Access::is_live).is_none() {
                self.drop_connection(&node);
            }
        }
    }
}
