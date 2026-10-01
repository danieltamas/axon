// Federation: the on/off switch, the relay, and the Peers panel. The panel shows each peer's
// live health from GET /api/fed (pushed as `event: fed` on /api/stream, polled as a fallback)
// and lets the owner pause, resume or disconnect it. Pairing is not driven from here.

import { el, since } from "./dom.js";
import { band, confirmAction, field, reason, request, settingsForm, switchRow, textInput } from "./settings-kit.js";

const STATE = {
  pending_confirm: "Awaiting confirmation",
  connected: "Connected",
  reconnecting: "Reconnecting",
  offline: "Offline",
  paused: "Paused",
  removed: "Removed",
  incompatible: "Incompatible",
};
const PAUSABLE = ["connected", "reconnecting", "offline", "incompatible"];
const REMOVE_WARNING =
  "Disconnecting ends every share with this peer, cancels what is queued for it and deletes its messages that no agent has seen yet. Pairing again starts from nothing.";

const size = (n) => (n < 1024 ? `${n} B` : n < 1 << 20 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / (1 << 20)).toFixed(1)} MiB`);

// "3s", "4m": how long, for a glance rather than an audit.
const span = (ms) => (ms < 45000 ? `${Math.max(1, Math.round(ms / 1000))}s` : since(Date.now() - ms).replace(" ago", ""));

function healthFacts(peer) {
  const facts = [];
  if (peer.path && peer.path !== "none") facts.push(peer.path === "relay" ? "via relay" : "direct");
  if (peer.rtt_ms !== null && peer.rtt_ms !== undefined) facts.push(`${peer.rtt_ms} ms${peer.rtt_stale ? " (stale)" : ""}`);
  if (peer.heartbeat_age_ms !== null && peer.heartbeat_age_ms !== undefined) facts.push(`heard ${span(peer.heartbeat_age_ms)} ago`);
  if (peer.last_handshake_at) facts.push(`handshake ${since(peer.last_handshake_at)}`);
  const retry = peer.next_retry_at - Date.now();
  if (["reconnecting", "offline"].includes(peer.state) && retry > 0) facts.push(`next try in ${span(retry)}`);
  return facts.join(" · ");
}

function trafficFacts(peer) {
  const { queue, counters } = peer;
  const facts = [];
  if (queue && queue.count) facts.push(`${queue.count} queued, ${size(queue.bytes)}${queue.oldest_at ? `, oldest ${since(queue.oldest_at)}` : ""}`);
  if (counters) {
    facts.push(`sent ${counters.sent_accepted}`, `received ${counters.received}`);
    for (const name of ["expired", "rejected", "cancelled"]) if (counters[name]) facts.push(`${name} ${counters[name]}`);
  }
  return facts.join(" · ");
}

function shareFacts(shares) {
  return (shares || [])
    .map((share) => {
      if (share.state !== "active") return `${share.label} (offered)`;
      const flows = [];
      if (share.outbound && share.remote_inbound) flows.push("sending");
      if (share.inbound && share.remote_outbound) flows.push("receiving");
      return `${share.label} (${flows.join(", ") || "no traffic allowed"})`;
    })
    .join(" · ");
}

// One peer. The row is built once and updated in place, so a confirm the owner has open
// survives the next push.
function peerRow(refresh) {
  const root = el("li", "peer");
  const label = el("b");
  const print = el("code");
  const who = el("span", "peer-who");
  who.append(label, print);
  const state = el("span", "peer-state");
  const health = el("span", "peer-facts");
  const traffic = el("span", "peer-facts");
  const shares = el("span", "peer-facts");
  const error = el("span", "peer-error");
  const status = el("output", "set-status");
  const pause = el("button", "btn", "Pause");
  const resume = el("button", "btn", "Resume");
  for (const button of [pause, resume]) button.type = "button";
  let id = "";
  const toggle = (button, verb) => async () => {
    button.disabled = true;
    status.dataset.tone = "";
    status.textContent = "";
    const res = await request("POST", `/api/fed/peers/${id}/${verb}`);
    button.disabled = false;
    if (!res.ok) {
      status.dataset.tone = "bad";
      status.textContent = reason(res);
    }
    refresh();
  };
  pause.addEventListener("click", toggle(pause, "pause"));
  resume.addEventListener("click", toggle(resume, "resume"));
  const disconnect = confirmAction({
    label: "Disconnect",
    confirm: "Disconnect and forget",
    warning: REMOVE_WARNING,
    run: async () => {
      const res = await request("DELETE", `/api/fed/peers/${id}`);
      refresh();
      return res;
    },
  });
  const actions = el("div", "peer-actions");
  actions.append(pause, resume, disconnect.root, status);
  root.append(who, state, health, traffic, shares, error, actions);
  return {
    root,
    update(peer) {
      id = peer.peer_id;
      root.dataset.state = peer.state;
      label.textContent = peer.label || "Unnamed peer";
      print.textContent = peer.fingerprint || peer.peer_id;
      state.textContent = STATE[peer.state] || peer.state;
      health.textContent = healthFacts(peer);
      traffic.textContent = trafficFacts(peer);
      shares.textContent = shareFacts(peer.shares);
      error.textContent = peer.last_error || "";
      pause.hidden = !PAUSABLE.includes(peer.state);
      resume.hidden = peer.state !== "paused";
      actions.hidden = peer.state === "pending_confirm";
    },
  };
}

function peersPanel() {
  const root = el("div", "peers");
  const out = el("div", "peers-body");
  root.append(el("h3", null, "Peers"), out);
  root.setAttribute("aria-live", "polite");
  out.append(el("p", "set-note", "Loading peers"));
  const identity = el("dl", "readout");
  const identityItem = el("div");
  const identityValue = el("dd");
  identityItem.append(el("dt", null, "This node"), identityValue);
  identity.append(identityItem);
  const list = el("ul", "peer-list");
  const empty = el("p", "set-note", "No peers paired.");
  const rows = new Map();

  // The sections of a federation body, shown or hidden as it says.
  function show(fed) {
    if (!fed.enabled) return out.replaceChildren(el("p", "set-note", "Federation is off, so no peers are connected."));
    identityValue.textContent = fed.fingerprint || fed.node_id || "No identity yet";
    const peers = fed.peers.filter((peer) => peer.state !== "removed");
    const live = new Set(peers.map((peer) => peer.peer_id));
    for (const [id, row] of rows) {
      if (!live.has(id)) {
        row.root.remove();
        rows.delete(id);
      }
    }
    const ordered = peers.map((peer) => {
      if (!rows.has(peer.peer_id)) rows.set(peer.peer_id, peerRow(refresh));
      const row = rows.get(peer.peer_id);
      row.update(peer);
      return row.root;
    });
    if ([...list.children].some((node, at) => node !== ordered[at]) || list.children.length !== ordered.length) list.replaceChildren(...ordered);
    out.replaceChildren(identity, ...(peers.length ? [list] : [empty]));
  }

  let key = "";
  function render(res) {
    const next = JSON.stringify([res.status, res.data]);
    if (next === key) return;
    key = next;
    if (res.status === 404) return out.replaceChildren(el("p", "set-note", "Peers are not available in this build."));
    if (!res.ok) {
      key = "";
      return out.replaceChildren(el("p", "set-note bad", `Could not load peers. ${reason(res)}`));
    }
    if (Array.isArray(res.data.peers)) show(res.data);
  }

  async function refresh() {
    render(await request("GET", "/api/fed"));
  }
  return { root, refresh, push: (fed) => render({ ok: true, status: 200, data: fed }) };
}

export function federationSection({ apply }) {
  const { root, body } = band("federation", "Federation", "Connect this machine to others over an encrypted link. Changing either setting restarts the link.");
  const toggle = switchRow({
    label: "Federation",
    hint: "When off, no connection is made or accepted.",
    onToggle: async (on) => {
      toggle.lock(true);
      toggle.say("");
      const res = await request("PUT", "/api/settings/federation", { enabled: on });
      toggle.lock(false);
      if (res.ok) apply(res.data);
      else toggle.say(res.status === 400 ? "The server rejected this change." : reason(res));
    },
  });
  const relay = field({
    name: "relay",
    label: "Relay",
    hint: "default, or the https:// address of your own relay.",
    invalid: "Enter default, or an address that starts with https://.",
    input: textInput("default"),
  });
  const form = settingsForm({
    fields: [relay],
    children: [relay.root],
    send: () => {
      const value = relay.input.value.trim();
      return request("PUT", "/api/settings/federation", { relay: value === "" ? "default" : value });
    },
    applied: apply,
  });
  const peers = peersPanel();
  body.append(toggle.root, form.root, peers.root);
  return {
    root,
    refreshPeers: peers.refresh,
    pushPeers: peers.push,
    sync({ federation }) {
      toggle.set(federation.enabled);
      if (form.isEditing()) return;
      relay.input.value = federation.relay;
    },
  };
}
