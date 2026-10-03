// Shares inside a peer card: which projects are shared with this peer, in which directions,
// and what the other side has agreed to. A direction carries traffic only when this side's
// flag and the peer's matching flag are both on, so each toggle says which one is missing.

import { el } from "./dom.js";
import { confirmAction, field, reason, request, settingsForm, switchRow, textInput } from "./settings-kit.js";

const UNSHARE_WARNING =
  "Unsharing stops the project for both sides. Messages still queued for it are cancelled, and incoming ones no agent has read are deleted.";
const OTHER = "__other";

// Projects with agents Axon has seen, from the live snapshot; the share takes a repository path.
async function localRepos() {
  const res = await request("GET", "/api/snapshot");
  const seen = new Map();
  for (const repo of res.ok ? res.data.repos || [] : []) if (repo.repo) seen.set(repo.repo, repo.name || repo.repo);
  return [...seen].map(([path, name]) => ({ path, name }));
}

// A select of local projects, with a way to type a folder the snapshot does not list.
function repoPicker(label) {
  const select = el("select", "sel");
  const path = textInput("/path/to/the/repository");
  const byPick = field({ name: "local_repo", label, invalid: "Choose a project.", input: select });
  const byPath = field({
    name: "local_repo",
    label: "Folder",
    hint: "A git repository on this machine.",
    invalid: "That is not a repository this machine can share.",
    input: path,
  });
  byPath.root.hidden = true;
  const active = () => (byPath.root.hidden ? byPick : byPath);
  const root = el("div", "picker");
  root.append(byPick.root, byPath.root);
  const listeners = [];
  let listed = null;
  select.addEventListener("change", () => {
    byPath.root.hidden = select.value !== OTHER;
    for (const listen of listeners) listen(select.selectedOptions[0].dataset.name || "");
  });
  return {
    root,
    // One field to the form: an error lands on whichever input is showing.
    field: {
      name: "local_repo",
      codes: ["already_shared", "offered_to_you", "peer_not_active", "too_many_shares"],
      input: { focus: () => active().input.focus() },
      clear() {
        byPick.clear();
        byPath.clear();
      },
      fail: (text) => active().fail(text),
    },
    value: () => (select.value === OTHER ? path.value.trim() : select.value),
    onName: (listen) => listeners.push(listen),
    // `preferred`: the folder to pick when nothing is chosen yet, such as the one holding
    // the same project as an offer.
    async load(preferred) {
      const repos = await localRepos();
      // Rebuilding the options closes the dropdown if it is open: only when the list changed.
      const key = JSON.stringify(repos);
      if (key === listed) return;
      listed = key;
      const chosen = select.value;
      select.replaceChildren(new Option(repos.length ? "Choose a project" : "No running projects found", ""));
      for (const repo of repos) {
        const option = new Option(`${repo.name}  ${repo.path}`, repo.path);
        option.dataset.name = repo.name;
        select.append(option);
      }
      select.append(new Option("Another folder", OTHER));
      const listed = (value) => value && [...select.options].some((option) => option.value === value);
      if (listed(chosen)) select.value = chosen;
      else if (listed(preferred)) select.value = preferred;
      else if (!repos.length) select.value = OTHER;
      byPath.root.hidden = select.value !== OTHER;
    },
  };
}

// The two directions as one pair of switches that only set a value; the form saves them.
function directions(initial) {
  const mark = (item) => item.input.dispatchEvent(new Event("input", { bubbles: true }));
  const send = switchRow({ label: "Send", hint: "My agents can message theirs.", onToggle: (on) => (send.set(on), mark(send)) });
  const receive = switchRow({ label: "Receive", hint: "Their agents can message mine.", onToggle: (on) => (receive.set(on), mark(receive)) });
  send.set(initial.outbound);
  receive.set(initial.inbound);
  const root = el("div", "directions");
  root.append(send.root, receive.root);
  return { root, values: () => ({ outbound: send.input.checked, inbound: receive.input.checked }) };
}

// "Share a project", opened on demand so a card without sharing stays quiet.
export function addShare({ peer, refresh }) {
  const root = el("div", "share-add");
  const open = el("button", "btn", "Share a project");
  open.type = "button";
  const picker = repoPicker("Project");
  const label = field({
    name: "label",
    label: "Share name",
    hint: "What both sides call it.",
    invalid: "Enter a name for the share.",
    input: textInput("my-project"),
  });
  let named = false;
  label.input.addEventListener("input", () => (named = true));
  picker.onName((name) => {
    if (!named && name) label.input.value = name;
  });
  const sides = directions({ outbound: true, inbound: true });
  const form = settingsForm({
    fields: [picker.field, label],
    children: [picker.root, label.root, sides.root],
    save: "Offer share",
    busy: "Offering",
    done: "Offered",
    refused: "Not offered",
    send: () => request("POST", `/api/fed/peers/${peer().peer_id}/shares`, { local_repo: picker.value(), label: label.input.value.trim(), ...sides.values() }),
    applied: () => {
      form.root.hidden = true;
      open.hidden = false;
      refresh();
    },
  });
  const cancel = el("button", "btn quiet", "Cancel");
  cancel.type = "button";
  form.root.querySelector(".set-foot").append(cancel);
  form.root.hidden = true;
  const close = () => {
    form.root.hidden = true;
    open.hidden = false;
    open.focus();
  };
  open.addEventListener("click", async () => {
    open.hidden = true;
    form.root.hidden = false;
    await picker.load();
    form.root.querySelector("select").focus();
  });
  cancel.addEventListener("click", close);
  form.root.addEventListener("keydown", (event) => {
    if (event.key === "Escape") close();
  });
  root.append(open, form.root);
  return { root };
}

// Accepting what the peer offered means choosing which checkout on this machine it maps to.
function acceptForm({ share, refresh }) {
  const picker = repoPicker("Map to my project");
  const sides = directions({ outbound: true, inbound: true });
  const form = settingsForm({
    fields: [picker.field],
    children: [picker.root, sides.root],
    save: "Accept share",
    busy: "Accepting",
    done: "Accepted",
    refused: "Not accepted",
    send: () => request("POST", `/api/fed/shares/${share().share_id}/accept`, { local_repo: picker.value(), ...sides.values() }),
    applied: refresh,
  });
  let loaded = false;
  return {
    root: form.root,
    load() {
      if (loaded) return;
      loaded = true;
      picker.load(share().suggested_repo);
    },
  };
}

// One share, built once and updated in place so a half-typed form or an open confirm survives.
export function shareRow({ peer, refresh }) {
  const root = el("li", "fed-share");
  const label = el("b");
  const where = el("code", "share-where");
  const tag = el("span", "share-tag");
  const head = el("div", "share-head");
  head.append(label, tag);
  const waiting = el("p", "set-note");
  let current = null;
  const accept = acceptForm({ share: () => current, refresh });
  const flags = el("div", "directions");
  const change = async (key, on) => {
    for (const item of [send, receive]) item.lock(true);
    send.say("");
    receive.say("");
    const body = { inbound: current.inbound, outbound: current.outbound, [key]: on };
    const res = await request("PUT", `/api/fed/shares/${current.share_id}`, body);
    for (const item of [send, receive]) item.lock(false);
    if (!res.ok) (key === "outbound" ? send : receive).say(reason(res));
    refresh();
  };
  const send = switchRow({ label: "Send", async: true, onToggle: (on) => change("outbound", on) });
  const receive = switchRow({ label: "Receive", async: true, onToggle: (on) => change("inbound", on) });
  flags.append(send.root, receive.root);
  const drop = async () => {
    const res = await request("DELETE", `/api/fed/shares/${current.share_id}`);
    refresh();
    return res;
  };
  const unshare = confirmAction({ label: "Unshare", confirm: "Unshare project", warning: UNSHARE_WARNING, run: drop });
  const decline = confirmAction({
    label: "Decline",
    confirm: "Decline offer",
    warning: "The offer is removed on both sides. The other person can offer it again.",
    run: drop,
  });
  root.append(head, where, waiting, accept.root, flags, unshare.root, decline.root);
  return {
    root,
    update(share) {
      current = share;
      const who = peer().label;
      const live = share.state === "active";
      root.dataset.state = share.state;
      label.textContent = share.label;
      where.textContent = share.local_repo || "Not mapped to a project on this machine yet";
      tag.textContent = live ? "Shared" : share.state === "offered_in" ? "Offered to you" : "Waiting for them";
      waiting.hidden = share.state !== "offered_out";
      waiting.textContent = `Waiting for ${who} to accept. Nothing is exchanged until they do.`;
      accept.root.hidden = share.state !== "offered_in";
      if (share.state === "offered_in") accept.load();
      flags.hidden = !live;
      unshare.root.hidden = share.state === "offered_in";
      decline.root.hidden = share.state !== "offered_in";
      send.set(share.outbound);
      receive.set(share.inbound);
      send.describe(!share.outbound ? "Off: my agents cannot message theirs." : share.remote_inbound ? "On: their agents accept my messages." : `Waiting: ${who} is not receiving yet.`);
      receive.describe(!share.inbound ? "Off: their agents cannot message mine." : share.remote_outbound ? "On: my agents accept their messages." : `Waiting: ${who} is not sending yet.`);
    },
  };
}
