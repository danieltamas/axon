// How a peer's live health and traffic read as short lines. Pure functions over one entry of
// GET /api/fed, shared by the peer card.

import { since } from "./dom.js";

export const STATE = {
  pending_confirm: "Awaiting confirmation",
  connected: "Connected",
  reconnecting: "Reconnecting",
  offline: "Offline",
  paused: "Paused",
  removed: "Removed",
  incompatible: "Incompatible",
};

const size = (n) => (n < 1024 ? `${n} B` : n < 1 << 20 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / (1 << 20)).toFixed(1)} MiB`);

// "3s", "4m": how long, for a glance rather than an audit.
export const span = (ms) => (ms < 45000 ? `${Math.max(1, Math.round(ms / 1000))}s` : since(Date.now() - ms).replace(" ago", ""));

export function healthFacts(peer) {
  const facts = [];
  if (peer.path && peer.path !== "none") facts.push(peer.path === "relay" ? "via relay" : "direct");
  if (peer.rtt_ms !== null && peer.rtt_ms !== undefined) facts.push(`${peer.rtt_ms} ms${peer.rtt_stale ? " (stale)" : ""}`);
  if (peer.heartbeat_age_ms !== null && peer.heartbeat_age_ms !== undefined) facts.push(`heard ${span(peer.heartbeat_age_ms)} ago`);
  if (peer.last_handshake_at) facts.push(`handshake ${since(peer.last_handshake_at)}`);
  const retry = peer.next_retry_at - Date.now();
  if (["reconnecting", "offline"].includes(peer.state) && retry > 0) facts.push(`next try in ${span(retry)}`);
  return facts.join(" · ");
}

export function trafficFacts(peer) {
  const { queue, counters } = peer;
  const facts = [];
  if (queue && queue.count) facts.push(`${queue.count} queued, ${size(queue.bytes)}${queue.oldest_at ? `, oldest ${since(queue.oldest_at)}` : ""}`);
  if (counters) {
    facts.push(`sent ${counters.sent_accepted}`, `received ${counters.received}`);
    for (const name of ["expired", "rejected", "cancelled"]) if (counters[name]) facts.push(`${name} ${counters[name]}`);
  }
  return facts.join(" · ");
}
