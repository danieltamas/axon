// Federation: the on/off switch, the relay, and a read-only Peers panel over GET /api/fed.
// Pairing is not driven from here yet; the panel only reports identity and peers, and says
// plainly when the route does not exist.

import { ago, el } from "./dom.js";
import { band, field, reason, request, settingsForm, switchRow, textInput } from "./settings-kit.js";

const STATE = {
  pending_confirm: "Awaiting confirmation",
  connected: "Connected",
  reconnecting: "Reconnecting",
  offline: "Offline",
  paused: "Paused",
  removed: "Removed",
  incompatible: "Incompatible",
};

function peerRow(peer) {
  const row = el("li", "peer");
  row.dataset.state = peer.state;
  const who = el("span", "peer-who");
  who.append(el("b", null, peer.label || "Unnamed peer"), el("code", null, peer.fingerprint || peer.peer_id));
  const state = el("span", "peer-state", STATE[peer.state] || peer.state);
  const facts = [];
  if (peer.path && peer.path !== "none") facts.push(peer.path === "relay" ? "via relay" : "direct");
  if (peer.rtt_ms !== null && peer.rtt_ms !== undefined) facts.push(`${peer.rtt_ms} ms${peer.rtt_stale ? " (stale)" : ""}`);
  if (peer.queue && peer.queue.count) facts.push(`${peer.queue.count} queued`);
  if (peer.last_handshake_at) facts.push(`handshake ${ago(peer.last_handshake_at)} ago`);
  row.append(who, state, el("span", "peer-facts", facts.join(" · ")));
  if (peer.last_error) row.append(el("span", "peer-error", peer.last_error));
  return row;
}

function peersPanel() {
  const root = el("div", "peers");
  const head = el("h3", null, "Peers");
  const out = el("div", "peers-body");
  root.append(head, out);
  root.setAttribute("aria-live", "polite");
  out.append(el("p", "set-note", "Loading peers"));
  let key = "";
  return {
    root,
    async refresh() {
      const res = await request("GET", "/api/fed");
      const next = JSON.stringify([res.status, res.data]);
      if (next === key) return;
      key = next;
      if (res.status === 404) {
        out.replaceChildren(el("p", "set-note", "Peers are not available yet in this build. Pairing arrives with a later update."));
        return;
      }
      if (!res.ok) {
        const failed = el("p", "set-note bad", `Could not load peers. ${reason(res)}`);
        out.replaceChildren(failed);
        key = "";
        return;
      }
      const fed = res.data;
      const nodes = [];
      if (!fed.enabled) {
        nodes.push(el("p", "set-note", "Federation is off, so no peers are connected."));
      } else {
        const id = el("dl", "readout");
        const item = el("div");
        item.append(el("dt", null, "This node"), el("dd", null, fed.fingerprint || fed.node_id || "No identity yet"));
        id.append(item);
        nodes.push(id);
        if (!fed.peers.length) nodes.push(el("p", "set-note", "No peers paired."));
        else {
          const list = el("ul", "peer-list");
          list.append(...fed.peers.map(peerRow));
          nodes.push(list);
        }
      }
      out.replaceChildren(...nodes);
    },
  };
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
    sync({ federation }) {
      toggle.set(federation.enabled);
      if (form.isEditing()) return;
      relay.input.value = federation.relay;
    },
  };
}
