// The Peers panel: one card per paired machine, holding everything about it. A card that
// awaits confirmation shows the pair code; a live one shows its health, its shared projects
// and Pause, Resume and Disconnect. Cards are built once and updated in place, so a confirm,
// a half-typed form or a focused control survives each push of GET /api/fed.

import { el } from "./dom.js";
import { connectPanel } from "./settings-pair.js";
import { healthFacts, STATE, trafficFacts } from "./settings-peer-facts.js";
import { addShare, shareRow } from "./settings-shares.js";
import { confirmAction, reason, request } from "./settings-kit.js";

const PAUSABLE = ["connected", "reconnecting", "offline", "incompatible"];
const REMOVE_WARNING =
  "Disconnecting ends every share with this peer, cancels what is queued for it and deletes its messages that no agent has seen yet. Pairing again starts from nothing.";
const CONFIRMED_KEY = "axon-fed-confirmed";

// The API does not say whether this side already confirmed, so the page remembers it for the tab.
function confirmedIds() {
  try {
    return new Set(JSON.parse(sessionStorage.getItem(CONFIRMED_KEY) || "[]"));
  } catch {
    return new Set();
  }
}
function remember(id) {
  const ids = confirmedIds().add(id);
  try {
    sessionStorage.setItem(CONFIRMED_KEY, JSON.stringify([...ids]));
  } catch {
    // Without storage the waiting note is lost on reload; the pairing itself is unaffected.
  }
}

function pendingBlock({ peer, refresh }) {
  const root = el("div", "pending");
  const lede = el("p", "pending-lede");
  const code = el("p", "pair-code");
  code.setAttribute("aria-label", "Pair code");
  const confirm = el("button", "btn primary", "Confirm, it matches");
  const reject = el("button", "btn", "Reject");
  for (const button of [confirm, reject]) button.type = "button";
  const buttons = el("div", "act-buttons");
  buttons.append(confirm, reject);
  const waiting = el("p", "pending-wait");
  const status = el("output", "set-status");
  root.append(lede, code, buttons, waiting, status);

  confirm.addEventListener("click", () => {
    const { peer_id: id, pair_code: pairCode } = peer();
    confirm.disabled = reject.disabled = true;
    status.dataset.tone = "";
    status.textContent = "Confirming";
    request("POST", `/api/fed/peers/${id}/confirm`, { pair_code: pairCode }).then((res) => {
      confirm.disabled = reject.disabled = false;
      status.textContent = "";
      if (res.ok) remember(id);
      else {
        status.dataset.tone = "bad";
        status.textContent = reason(res);
      }
      // The data is unchanged when only this page remembers the confirm, so redraw regardless.
      refresh(true);
    });
  });
  reject.addEventListener("click", async () => {
    confirm.disabled = reject.disabled = true;
    const res = await request("POST", `/api/fed/peers/${peer().peer_id}/reject`);
    confirm.disabled = reject.disabled = false;
    if (!res.ok) {
      status.dataset.tone = "bad";
      status.textContent = reason(res);
    }
    refresh();
  });

  return {
    root,
    update(next) {
      const confirmed = confirmedIds().has(next.peer_id);
      code.textContent = next.pair_code || "";
      confirm.disabled = !next.pair_code;
      lede.textContent = confirmed
        ? `You confirmed the code.`
        : `Check that this matches the code on ${next.label}'s screen, digit for digit, then confirm. Compare it out loud or in a chat you already trust.`;
      buttons.hidden = confirmed;
      waiting.hidden = !confirmed;
      waiting.textContent = `Waiting for ${next.label} to confirm on their side. The pairing is dropped if they do not within 10 minutes.`;
    },
  };
}

function peerCard(refresh) {
  const root = el("li", "peer");
  let current = null;
  const peer = () => current;
  const label = el("b");
  const print = el("code");
  const who = el("span", "peer-who");
  who.append(label, print);
  const state = el("span", "peer-state");
  const head = el("div", "peer-head");
  head.append(who, state);

  const pending = pendingBlock({ peer, refresh });

  const health = el("p", "peer-facts");
  const traffic = el("p", "peer-facts");
  const error = el("p", "peer-error");
  const status = el("div", "peer-status");
  status.append(health, traffic, error);

  const list = el("ul", "share-list");
  const none = el("p", "set-note", "No project shared yet. Pairing alone lets nothing through.");
  const add = addShare({ peer, refresh });
  const shares = el("div", "peer-shares");
  const shareHead = el("h4", null, "Shared projects");
  shares.append(shareHead, list, none, add.root);
  const rows = new Map();

  const note = el("output", "set-status");
  const pause = el("button", "btn", "Pause");
  const resume = el("button", "btn", "Resume");
  for (const button of [pause, resume]) button.type = "button";
  const toggle = (button, verb) => async () => {
    button.disabled = true;
    note.dataset.tone = "";
    note.textContent = "";
    const res = await request("POST", `/api/fed/peers/${current.peer_id}/${verb}`);
    button.disabled = false;
    if (!res.ok) {
      note.dataset.tone = "bad";
      note.textContent = reason(res);
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
      const res = await request("DELETE", `/api/fed/peers/${current.peer_id}`);
      refresh();
      return res;
    },
  });
  const actions = el("div", "peer-actions");
  actions.append(pause, resume, disconnect.root, note);

  root.append(head, pending.root, status, shares, actions);
  return {
    root,
    update(next) {
      current = next;
      const waitingOnYou = next.state === "pending_confirm";
      root.dataset.state = next.state;
      label.textContent = next.label || "Unnamed peer";
      print.textContent = next.fingerprint || next.peer_id;
      state.textContent = waitingOnYou && confirmedIds().has(next.peer_id) ? "Waiting for them" : STATE[next.state] || next.state;
      pending.root.hidden = !waitingOnYou;
      if (waitingOnYou) pending.update(next);
      status.hidden = waitingOnYou;
      shares.hidden = waitingOnYou;
      health.textContent = healthFacts(next);
      traffic.textContent = trafficFacts(next);
      error.textContent = next.last_error || "";
      const live = new Set((next.shares || []).map((share) => share.share_id));
      for (const [id, row] of rows)
        if (!live.has(id)) {
          row.root.remove();
          rows.delete(id);
        }
      const ordered = (next.shares || []).map((share) => {
        if (!rows.has(share.share_id)) rows.set(share.share_id, shareRow({ peer, refresh }));
        const row = rows.get(share.share_id);
        row.update(share);
        return row.root;
      });
      if (ordered.some((node, at) => list.children[at] !== node) || list.children.length !== ordered.length) list.replaceChildren(...ordered);
      none.hidden = ordered.length > 0;
      add.root.hidden = next.state === "paused" || next.state === "incompatible";
      pause.hidden = !PAUSABLE.includes(next.state);
      resume.hidden = next.state !== "paused";
      actions.hidden = waitingOnYou;
    },
  };
}

export function peersPanel() {
  const root = el("div", "peers");
  const connect = connectPanel({ refresh: () => refresh() });
  connect.root.hidden = true;
  const out = el("div", "peers-body");
  root.append(connect.root, el("h3", null, "Peers"), out);
  out.setAttribute("aria-live", "polite");
  out.append(el("p", "set-note", "Loading peers"));
  const identity = el("dl", "readout");
  const identityItem = el("div");
  const identityValue = el("dd");
  identityItem.append(el("dt", null, "This node"), identityValue);
  identity.append(identityItem);
  const list = el("ul", "peer-list");
  const empty = el("p", "set-note", "No peers paired yet. Invite a machine above, or join with a link you were sent.");
  const cards = new Map();

  function show(fed) {
    connect.root.hidden = !fed.enabled;
    if (!fed.enabled) return out.replaceChildren(el("p", "set-note", "Federation is off, so no peers are connected."));
    connect.sync(fed);
    identityValue.textContent = fed.fingerprint || fed.node_id || "No identity yet";
    const peers = fed.peers.filter((peer) => peer.state !== "removed");
    const live = new Set(peers.map((peer) => peer.peer_id));
    for (const [id, card] of cards)
      if (!live.has(id)) {
        card.root.remove();
        cards.delete(id);
      }
    const ordered = peers.map((peer) => {
      if (!cards.has(peer.peer_id)) cards.set(peer.peer_id, peerCard(refresh));
      const card = cards.get(peer.peer_id);
      card.update(peer);
      return card.root;
    });
    if ([...list.children].some((node, at) => node !== ordered[at]) || list.children.length !== ordered.length) list.replaceChildren(...ordered);
    // With peers, the cards come first and connecting another machine moves below them.
    root.dataset.peers = String(peers.length > 0);
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

  async function refresh(force = false) {
    if (force === true) key = "";
    render(await request("GET", "/api/fed"));
  }
  return { root, refresh, push: (fed) => render({ ok: true, status: 200, data: fed }) };
}
