// The operator's composer. The human has no node of its own: a message travels along an
// existing edge, sent as the agent at the other end of it (BUS-PLAN §7), and the form
// says so. `routes` lists who can be addressed, each with the agent it is sent as;
// `question` is a pending question the operator may answer on its addressee's behalf.

import { el } from "./dom.js";
import { authFetch, showSignIn } from "./signin.js";

const MAX = 400;
const CONFIRM_MS = 4000;

const ACTIONS = [
  ["answer", "Answer", (r) => `Answer ${r.to}; it is waiting on this`],
  ["redirect", "Redirect", (r) => `What should ${r.to} do instead?`],
  ["question", "Ask", (r) => `Ask ${r.to}; the answer lands in this thread`],
  ["sync", "Note", (r) => `Context ${r.to} should have`],
];

export function createComposer({ routes, thread = null, question = null, drafts, draftKey }) {
  const form = el("form", "composer");
  const answer = question && { to: question.from, from: question.to };
  const actions = ACTIONS.filter(([value]) => value !== "answer" || answer);
  const kinds = choice("kind", "Action", actions.map(([value, label]) => [value, label]), answer ? "answer" : "redirect");
  const targets = routes.length > 1 ? choice("to", "Recipient", routes.map((r, i) => [String(i), r.to]), "0") : null;
  const route = el("p", "route-line");
  const body = el("textarea");
  body.rows = 3;
  body.maxLength = MAX;
  body.setAttribute("aria-label", "Message");
  body.value = drafts.get(draftKey) || "";
  const count = el("span", "count");
  const stop = el("button", "stop-button", "Stop…");
  stop.type = "button";
  const send = el("button", "send-button", "Send");
  send.type = "submit";
  const status = el("output");
  const kindOf = () => form.elements.kind.value;
  const routeOf = () => (kindOf() === "answer" ? answer : routes[targets ? Number(form.elements.to.value) : 0]);

  function sync() {
    const kind = kindOf();
    const r = routeOf();
    if (targets) targets.hidden = kind === "answer";
    route.replaceChildren(el("span", null, "To "), el("b", null, r.to), el("span", null, ` · sent as ${r.from}`));
    body.placeholder = ACTIONS.find(([value]) => value === kind)[2](r);
    send.textContent = kind === "answer" ? "Answer" : "Send";
    count.textContent = `${body.value.length}/${MAX}`;
    stop.hidden = kind === "answer";
  }
  form.addEventListener("change", sync);
  body.addEventListener("input", () => {
    drafts.set(draftKey, body.value);
    count.textContent = `${body.value.length}/${MAX}`;
  });
  // Enter breaks the line; Cmd or Ctrl+Enter sends.
  body.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && !event.isComposing) {
      event.preventDefault();
      form.requestSubmit();
    }
  });

  async function post(kind) {
    const r = routeOf();
    const text = body.value.trim() || (kind === "stop" ? "Stopped by the operator" : "");
    if (!text) {
      body.focus();
      return;
    }
    const payload = kind === "answer"
      ? { from_id: r.from, to_id: r.to, kind, body: text, reply_to: question.id }
      : { from_id: r.from, to_id: r.to, kind, body: text, thread };
    send.disabled = true;
    const response = await authFetch("/api/msg", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }).catch(() => null);
    send.disabled = false;
    if (response && response.status === 401) showSignIn();
    const reply = response ? await response.json().catch(() => ({})) : {};
    status.dataset.ok = String(Boolean(response && response.ok));
    status.textContent = !response ? "The dashboard lost its server. Retry once it reconnects." : response.ok ? `${kind === "stop" ? "Stop sent" : "Sent"} to ${r.to}.` : reply.error || `Refused (${response.status}).`;
    if (response && response.ok) {
      body.value = "";
      drafts.delete(draftKey);
      sync();
    }
  }

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    post(kindOf());
  });
  // A stop halts the whole subtree, so it takes a second, deliberate click.
  let armed = null;
  stop.addEventListener("click", () => {
    if (armed) {
      clearTimeout(armed);
      armed = null;
      stop.classList.remove("armed");
      stop.textContent = "Stop…";
      post("stop");
      return;
    }
    stop.classList.add("armed");
    stop.textContent = `Stop ${routeOf().to} and its subagents`;
    armed = setTimeout(() => {
      armed = null;
      stop.classList.remove("armed");
      stop.textContent = "Stop…";
    }, CONFIRM_MS);
  });

  const foot = el("div", "composer-foot");
  const keys = el("span", "keys", "⌘↵ to send");
  foot.append(keys, count, stop, send);
  form.append(kinds);
  if (targets) form.append(targets);
  form.append(route, body, foot, status);
  sync();
  return form;
}

// Real radios, so arrow keys and screen readers work.
function choice(name, legend, options, checked) {
  const set = el("fieldset", "choice");
  set.append(el("legend", "sr-only", legend));
  for (const [value, label] of options) {
    const option = el("label", `choice-option c-${value}`);
    const radio = el("input");
    radio.type = "radio";
    radio.name = name;
    radio.value = value;
    radio.checked = value === checked;
    option.append(radio, el("span", null, label));
    set.append(option);
  }
  return set;
}

// Ends open harness sessions (their processes get a terminate signal), for sessions the
// bus cannot reach. Two deliberate clicks, like a stop; the server re-checks every id.
// `compact` drops the status line: the outcome shows on the button, the reason in its title.
export function createEnder({ sessions, label, confirm, compact = false }) {
  const box = el("div", compact ? "card-end" : "ender");
  const button = el("button", "stop-button", label);
  button.type = "button";
  const status = el("output");
  let armed = null;
  const disarm = () => {
    clearTimeout(armed);
    armed = null;
    button.classList.remove("armed");
    button.textContent = label;
    button.title = "";
  };
  button.addEventListener("click", async () => {
    if (!armed) {
      button.classList.add("armed");
      button.textContent = confirm;
      armed = setTimeout(disarm, CONFIRM_MS);
      return;
    }
    disarm();
    button.disabled = true;
    const response = await authFetch("/api/end", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      // The start time pins each process, so a pid reused since the page drew is refused.
      body: JSON.stringify({ agents: sessions.map((s) => ({ id: s.id, started_ms: s.started_ms })) }),
    }).catch(() => null);
    button.disabled = false;
    if (response && response.status === 401) showSignIn();
    const reply = response ? await response.json().catch(() => ({})) : {};
    const ended = (reply.ended || []).length;
    const refused = reply.refused || [];
    status.dataset.ok = String(Boolean(response && response.ok && !refused.length));
    status.textContent = !response
      ? "The dashboard lost its server. Retry once it reconnects."
      : !response.ok
        ? reply.error || `Refused (${response.status}).`
        : [ended ? `Ended ${ended} ${ended === 1 ? "session" : "sessions"}; it leaves the board within a few seconds.` : null, refused.length ? `${refused.length} not ended: ${refused[0].why}.` : null].filter(Boolean).join(" ");
    if (compact) {
      button.textContent = status.dataset.ok === "true" ? "Ended" : "Not ended";
      button.title = status.textContent;
    }
  });
  box.append(button);
  if (!compact) box.append(status);
  return box;
}
