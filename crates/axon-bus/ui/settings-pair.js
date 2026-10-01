// Connecting a machine: create an invite for someone else, or join with one you were sent.
// The invite's secret exists only inside the axon1: link, which is shown once, here, because
// GET /api/fed lists an open invite by id and expiry alone.

import { el } from "./dom.js";
import { field, reason, request, settingsForm, textInput } from "./settings-kit.js";

const INTRO = "Create a link and send it to the other person over any channel. It works once and expires after 10 minutes.";

const clock = (left) => {
  const s = Math.max(0, Math.ceil(left / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

// What the owner can do with the invite they made: copy it, watch it run out, cancel it.
function invitePanel({ refresh }) {
  const root = el("div", "pair-block");
  const intro = el("p", "set-note", INTRO);
  const create = el("button", "btn primary", "Invite a machine");
  create.type = "button";
  const link = textInput("");
  link.readOnly = true;
  link.setAttribute("aria-label", "Your invite link");
  link.addEventListener("focus", () => link.select());
  const copy = el("button", "btn", "Copy link");
  copy.type = "button";
  const cancel = el("button", "btn quiet", "Cancel invite");
  cancel.type = "button";
  const left = el("span", "invite-left");
  const status = el("output", "set-status");
  const shown = el("div", "invite");
  const row = el("div", "invite-row");
  row.append(link, copy);
  const foot = el("div", "invite-foot");
  foot.append(left, cancel);
  shown.append(row, foot, el("p", "set-note", "Anyone with this link can pair with this machine until it is used or expires. Share it only with the person you mean to."));
  shown.hidden = true;
  root.append(intro, create, shown, status);

  let current = null; // { id, link, expires } while we hold the link
  let timer = 0;

  function tick() {
    if (!current) return;
    const remaining = current.expires - Date.now();
    left.textContent = remaining > 0 ? `Expires in ${clock(remaining)}` : "Expired";
    if (remaining <= 0) forget();
  }
  function forget() {
    current = null;
    clearInterval(timer);
    shown.hidden = true;
    create.hidden = false;
    intro.hidden = false;
    link.value = "";
  }
  function hold(next) {
    current = next;
    link.value = next.link;
    shown.hidden = false;
    create.hidden = true;
    intro.hidden = true;
    tick();
    clearInterval(timer);
    timer = setInterval(tick, 1000);
  }

  create.addEventListener("click", async () => {
    create.disabled = true;
    status.dataset.tone = "";
    status.textContent = "";
    const res = await request("POST", "/api/fed/invites", {});
    create.disabled = false;
    if (!res.ok) {
      status.dataset.tone = "bad";
      status.textContent = reason(res);
      return;
    }
    hold({ id: res.data.invite_id, link: res.data.invite, expires: res.data.expires_at });
    link.focus();
    refresh();
  });
  copy.addEventListener("click", async () => {
    status.dataset.tone = "";
    try {
      await navigator.clipboard.writeText(link.value);
      status.dataset.tone = "ok";
      status.textContent = "Copied";
    } catch {
      link.select();
      status.textContent = "Copy was blocked. The link is selected; copy it by hand.";
    }
  });
  cancel.addEventListener("click", async () => {
    cancel.disabled = true;
    const res = await request("DELETE", `/api/fed/invites/${current.id}`);
    cancel.disabled = false;
    if (res.ok || res.status === 400) {
      forget();
      status.textContent = "";
      create.focus();
      refresh();
    } else {
      status.dataset.tone = "bad";
      status.textContent = reason(res);
    }
  });

  return {
    root,
    // The server's list is the truth: an invite it no longer lists was used, cancelled or expired.
    sync(open) {
      if (!current) {
        // After a reload the secret is gone with the page; only the server's record is left.
        const left = open.length ? clock(open[0].expires_at - Date.now()) : "";
        intro.textContent = open.length
          ? `An invite from an earlier visit is still open for ${left}. Its link cannot be shown again; creating a new one cancels it.`
          : INTRO;
      }
      if (current && !open.some((invite) => invite.invite_id === current.id)) {
        const used = Date.now() < current.expires;
        forget();
        status.dataset.tone = "";
        status.textContent = used ? "The invite was used or cancelled." : "";
        setTimeout(() => (status.textContent = ""), 8000);
      }
    },
    dispose: () => clearInterval(timer),
  };
}

// Paste a link, name the peer, and the server says why if it will not take it.
function joinPanel({ refresh }) {
  const invite = field({
    name: "invite",
    label: "Link you were sent",
    hint: "Starts with axon1:",
    invalid: "That is not an invite link. It starts with axon1:",
    codes: ["invalid_invite", "peer_unreachable", "already_paired"],
    input: textInput("axon1:"),
  });
  const label = field({
    name: "label",
    label: "Name this peer",
    hint: "Letters, digits, dot, dash or underscore, up to 32. Agents see it as peer:<name>.",
    invalid: "Use 1 to 32 letters, digits, dots, dashes or underscores.",
    codes: ["label_taken"],
    input: textInput("alice"),
  });
  const form = settingsForm({
    fields: [invite, label],
    children: [invite.root, label.root],
    save: "Join",
    busy: "Connecting",
    done: "Joined. Confirm the code below.",
    refused: "Not joined",
    send: () => request("POST", "/api/fed/join", { invite: invite.input.value.trim(), label: label.input.value.trim() }),
    applied: () => {
      invite.input.value = "";
      label.input.value = "";
      refresh();
    },
  });
  const root = el("div", "pair-block");
  root.append(form.root);
  return { root };
}

export function connectPanel({ refresh }) {
  const root = el("div", "connect");
  root.append(el("h3", null, "Connect a machine"));
  const invite = invitePanel({ refresh });
  const join = joinPanel({ refresh });
  const pair = el("div", "pair");
  const inviteHead = el("h4", null, "Invite someone");
  const joinHead = el("h4", null, "Join with an invite");
  const left = el("section", "pair-col");
  left.append(inviteHead, invite.root);
  const right = el("section", "pair-col");
  right.append(joinHead, join.root);
  pair.append(left, right);
  root.append(pair);
  return { root, sync: (fed) => invite.sync(fed.invites || []), dispose: invite.dispose };
}
