// The link map: this machine as a spine on the left, one wire per paired machine. A wire
// reads as its state (solid when connected, dashed while it reconnects, dotted when paused),
// carries what crossed it each way, and a dot runs along it when a message does. Each end
// lists the agents a shared project reaches there, by harness.

import { el, glyph } from "./dom.js";
import { size, STATE } from "./settings-peer-facts.js";

const AVAILABILITY = ["active", "idle", "away"];

// Per harness, how many agents sit at each availability; one project is one share, so
// summing shares counts each agent once per peer.
function tally(shares, side) {
  const counts = new Map();
  for (const share of shares || [])
    for (const { harness, availability } of (share.reach && share.reach[side]) || []) {
      const row = counts.get(harness) || { active: 0, idle: 0, away: 0 };
      row[AVAILABILITY.includes(availability) ? availability : "idle"] += 1;
      counts.set(harness, row);
    }
  return counts;
}

function chips(counts, empty) {
  const list = el("ul", "conn-chips");
  for (const [harness, row] of [...counts].sort(([a], [b]) => a.localeCompare(b))) {
    const total = row.active + row.idle + row.away;
    const chip = el("li", "conn-chip");
    chip.dataset.availability = row.active ? "active" : row.idle ? "idle" : "away";
    chip.title = `${harness}: ${AVAILABILITY.filter((a) => row[a]).map((a) => `${row[a]} ${a}`).join(", ")}`;
    chip.append(glyph(harness), el("span", null, harness), el("b", null, total));
    list.append(chip);
  }
  if (!list.children.length) list.append(el("li", "conn-chip none", empty));
  return list;
}

const calm = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

// A dot crossing the wire: outward for a message sent, inward for one received.
function pulse(wire, outward) {
  if (calm()) return;
  const dot = el("i", "conn-dot");
  wire.append(dot);
  const span = wire.clientWidth - 8;
  const [from, to] = outward ? [0, span] : [span, 0];
  dot.animate([{ transform: `translateX(${from}px)`, opacity: 0 }, { opacity: 1, offset: 0.15 }, { opacity: 1, offset: 0.85 }, { transform: `translateX(${to}px)`, opacity: 0 }], {
    duration: 900,
    easing: "cubic-bezier(0.25, 1, 0.5, 1)",
  }).finished.then(() => dot.remove(), () => dot.remove());
}

function linkRow(onPick) {
  const root = el("li", "conn-link");
  const wire = el("div", "conn-wire");
  const out = el("span", "conn-flow out");
  const back = el("span", "conn-flow in");
  const queued = el("span", "conn-queued");
  wire.append(out, queued, back);
  const node = el("button", "conn-node peer");
  node.type = "button";
  const name = el("b");
  const state = el("span", "conn-state");
  const head = el("span", "conn-node-head");
  head.append(name, state);
  let reach = chips(new Map(), "");
  node.append(head, reach);
  root.append(wire, node);
  let last = null;
  let id = "";
  node.addEventListener("click", () => onPick(id));
  return {
    root,
    update(peer) {
      id = peer.peer_id;
      root.dataset.state = peer.state;
      name.textContent = peer.label || "Unnamed peer";
      state.textContent = peer.remote_paused ? "Paused by them" : STATE[peer.state] || peer.state;
      const c = peer.counters || {};
      const t = peer.traffic || {};
      const sent = c.sent_accepted || 0;
      const received = c.received || 0;
      out.textContent = `→ ${sent} · ${size(t.bytes_sent || 0)}`;
      back.textContent = `← ${received} · ${size(t.bytes_received || 0)}`;
      out.setAttribute("aria-label", `Sent ${sent} messages, ${size(t.bytes_sent || 0)}`);
      back.setAttribute("aria-label", `Received ${received} messages, ${size(t.bytes_received || 0)}`);
      const q = peer.queue || {};
      queued.hidden = !q.count;
      queued.textContent = q.count ? `${q.count} waiting · ${size(q.bytes || 0)}` : "";
      const next = chips(tally(peer.shares, "theirs"), peer.state === "connected" ? "No agents in a shared project" : "No agents seen");
      reach.replaceWith(next);
      reach = next;
      if (last) {
        if (sent > last.sent) pulse(wire, true);
        if (received > last.received) pulse(wire, false);
      }
      last = { sent, received };
    },
  };
}

export function linkMap({ onPick }) {
  const root = el("div", "conn-map");
  const hub = el("div", "conn-node hub");
  const hubName = el("b", null, "This machine");
  const hubPrint = el("code");
  const hubHead = el("span", "conn-node-head");
  hubHead.append(hubName, hubPrint);
  let hubReach = chips(new Map(), "");
  hub.append(hubHead, hubReach);
  const links = el("ul", "conn-links");
  // The place a machine would be: drawn when there is none, so the page shows what pairing makes.
  const ghost = el("li", "conn-link ghost");
  const ghostWire = el("div", "conn-wire");
  const ghostNode = el("div", "conn-node peer");
  ghostNode.append(el("b", null, "Another machine"), el("span", "conn-note", "A teammate's, or your own laptop"));
  ghost.append(ghostWire, ghostNode);
  root.append(hub, links);
  const rows = new Map();

  return {
    root,
    update(fed) {
      root.dataset.enabled = String(Boolean(fed.enabled));
      hubPrint.textContent = fed.fingerprint || (fed.enabled ? "starting" : "federation off");
      const peers = (fed.peers || []).filter((peer) => peer.state !== "removed");
      // This side's agents per harness: a project shared with two machines counts once.
      const mine = new Map();
      for (const peer of peers)
        for (const [harness, row] of tally(peer.shares, "mine")) {
          const seen = mine.get(harness);
          if (!seen || row.active + row.idle > seen.active + seen.idle) mine.set(harness, row);
        }
      const next = chips(mine, peers.length ? "No agents in a shared project" : "Pair to share a project");
      hubReach.replaceWith(next);
      hubReach = next;
      const live = new Set(peers.map((peer) => peer.peer_id));
      for (const [id, row] of rows)
        if (!live.has(id)) {
          row.root.remove();
          rows.delete(id);
        }
      const ordered = peers.map((peer) => {
        if (!rows.has(peer.peer_id)) rows.set(peer.peer_id, linkRow(onPick));
        const row = rows.get(peer.peer_id);
        row.update(peer);
        return row.root;
      });
      const shown = ordered.length ? ordered : [ghost];
      if (shown.some((node, at) => links.children[at] !== node) || links.children.length !== shown.length) links.replaceChildren(...shown);
    },
  };
}
