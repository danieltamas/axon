// Connections: the machines this one is paired with, at a glance and in full. The link map
// shows each wire's state and what crossed it; below it, connecting another machine and one
// card per peer (settings-peers.js). Live bodies arrive as `event: fed` on /api/stream,
// with a poll as the fallback while the view is open.

import { linkMap } from "./connections-map.js";
import { el } from "./dom.js";
import { reason, request } from "./settings-kit.js";
import { peersPanel } from "./settings-peers.js";

const POLL_MS = 5000;

// The off state: what federation does, and the one action that starts it.
function offPanel(onEnabled) {
  const root = el("div", "conn-off");
  const text = el("p", null, "Federation is off. Turn it on to pair this machine with another over an encrypted link, then share a project so the agents on both can message each other.");
  const start = el("button", "btn primary", "Turn on federation");
  start.type = "button";
  const more = el("a", "conn-more", "Relay and other options in Settings");
  more.href = "#/settings";
  const status = el("output", "set-status");
  const row = el("div", "act-buttons");
  row.append(start, more);
  root.append(text, row, status);
  start.addEventListener("click", async () => {
    start.disabled = true;
    status.dataset.tone = "";
    status.textContent = "Starting";
    const res = await request("PUT", "/api/settings/federation", { enabled: true });
    start.disabled = false;
    status.textContent = "";
    if (res.ok) onEnabled();
    else {
      status.dataset.tone = "bad";
      status.textContent = reason(res);
    }
  });
  return root;
}

export function createConnections(container, { onCount }) {
  const head = el("header", "conn-head");
  head.append(el("h1", null, "Connections"), el("p", null, "Machines paired with this one, what crosses each link, and the projects they share."));
  const map = linkMap({
    onPick: (id) => {
      const card = document.getElementById(`peer-${id}`);
      if (!card) return;
      card.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" });
      card.dataset.flash = "true";
      setTimeout(() => delete card.dataset.flash, 1200);
    },
  });
  const peers = peersPanel({ onFed: draw });
  const off = offPanel(() => peers.refresh(true));
  off.hidden = true;
  container.append(head, map.root, off, peers.root);

  function draw(fed) {
    map.update(fed);
    off.hidden = Boolean(fed.enabled);
    peers.root.hidden = !fed.enabled;
  }

  let timer = 0;
  let showing = false;
  return {
    fed(body) {
      if (!Array.isArray(body.peers)) return;
      onCount(body.peers.filter((peer) => peer.state === "connected").length);
      if (showing) peers.push(body);
    },
    show() {
      showing = true;
      peers.refresh(true);
      clearInterval(timer);
      timer = setInterval(() => peers.refresh(), POLL_MS);
    },
    hide() {
      showing = false;
      clearInterval(timer);
    },
  };
}
