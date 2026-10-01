// The pieces every Settings section is built from: the API call, a labelled field that can
// carry the server's complaint beside it, a switch, and a two-step confirm for actions
// that cannot be undone. Nothing here shows a value the server has not confirmed.

import { el } from "./dom.js";
import { showSignIn } from "./signin.js";

const REASONS = {
  busy: "The database is busy. Nothing was changed; try again in a moment.",
  sign_in: "This browser is signed out. Run axon open in a terminal.",
};

// One request, one answer shape. A lost connection reads as status 0.
export async function request(method, path, body) {
  const options = { method, headers: {} };
  if (body !== undefined) {
    options.headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(body);
  }
  const response = await fetch(path, options).catch(() => null);
  if (!response) return { ok: false, status: 0, data: { error: "The dashboard lost its server. Nothing was changed." } };
  if (response.status === 401) showSignIn();
  const data = await response.json().catch(() => ({}));
  return { ok: response.ok, status: response.status, data };
}

export function reason(res) {
  const text = res.data && res.data.error;
  return REASONS[text] || text || `Refused (${res.status}).`;
}

let serial = 0;
export const uid = (prefix) => `${prefix}-${++serial}`;

// A section is a ruled band: what it is on the left, its controls on the right.
export function band(id, title, lede) {
  const root = el("section", "set");
  root.id = `set-${id}`;
  const heading = el("h2", null, title);
  heading.id = `set-${id}-h`;
  root.setAttribute("aria-labelledby", heading.id);
  const intro = el("header", "set-intro");
  intro.append(heading, el("p", null, lede));
  const body = el("div", "set-body");
  root.append(intro, body);
  return { root, body };
}

// A label, a control, a hint, and a place for the server's complaint about this field.
export function field({ name, label, hint, invalid, input, unit }) {
  const id = uid("f");
  const root = el("div", "fld");
  root.dataset.field = name;
  const caption = el("label", "fld-label", label);
  caption.htmlFor = id;
  input.id = id;
  const error = el("p", "fld-error");
  error.id = `${id}-err`;
  const note = hint ? el("p", "fld-hint", hint) : null;
  if (note) note.id = `${id}-hint`;
  input.setAttribute("aria-describedby", [note && note.id, error.id].filter(Boolean).join(" "));
  const row = el("div", "fld-row");
  if (unit) row.append(el("span", "fld-unit", unit));
  row.append(input);
  root.append(caption, row, ...(note ? [note] : []), error);
  return {
    root,
    input,
    name,
    fail() {
      error.textContent = invalid;
      input.setAttribute("aria-invalid", "true");
      root.dataset.invalid = "true";
    },
    clear() {
      error.textContent = "";
      input.removeAttribute("aria-invalid");
      delete root.dataset.invalid;
    },
  };
}

export function numberInput({ min, max, step = 1, placeholder = "" }) {
  const input = el("input", "num");
  input.type = "number";
  input.inputMode = "decimal";
  input.min = min;
  if (max !== undefined) input.max = max;
  input.step = step;
  input.placeholder = placeholder;
  return input;
}

export function textInput(placeholder) {
  const input = el("input", "txt");
  input.type = "text";
  input.spellcheck = false;
  input.autocomplete = "off";
  input.placeholder = placeholder;
  return input;
}

// A checkbox that reads as a switch. Its position is set by `set` only, so a click that the
// server refuses leaves it where it was.
export function switchRow({ label, hint, onToggle }) {
  const id = uid("sw");
  const root = el("div", "fld sw");
  const input = el("input");
  input.type = "checkbox";
  input.id = id;
  input.setAttribute("role", "switch");
  const track = el("span", "sw-track");
  track.setAttribute("aria-hidden", "true");
  const text = el("span", "sw-text");
  const caption = el("span", "fld-label", label);
  text.append(caption);
  const lines = [];
  if (hint) {
    const note = el("span", "fld-hint", hint);
    note.id = `${id}-hint`;
    text.append(note);
    lines.push(note.id);
  }
  const status = el("p", "fld-error");
  status.id = `${id}-err`;
  lines.push(status.id);
  input.setAttribute("aria-describedby", lines.join(" "));
  const wrap = el("label", "sw-hit");
  wrap.append(input, track, text);
  root.append(wrap, status);
  input.addEventListener("click", (event) => {
    event.preventDefault();
    onToggle(!input.checked);
  });
  return {
    root,
    input,
    set: (on) => {
      input.checked = Boolean(on);
    },
    lock: (locked) => {
      input.disabled = locked;
      root.dataset.locked = String(locked);
    },
    say: (text) => {
      status.textContent = text;
    },
  };
}

// A form that saves only what changed and shows the server's answer where it belongs.
// `send` returns the response; `fields` are the field objects a 400 may name.
export function settingsForm({ fields, children, send, applied, save = "Save" }) {
  const form = el("form", "set-form");
  form.noValidate = true;
  const button = el("button", "btn primary", save);
  button.type = "submit";
  button.disabled = true;
  const status = el("output", "set-status");
  const foot = el("div", "set-foot");
  foot.append(button, status);
  form.append(...children, foot);

  const markDirty = () => {
    form.dataset.dirty = "true";
    button.disabled = false;
    status.textContent = "";
    status.dataset.tone = "";
  };
  form.addEventListener("input", markDirty);
  form.addEventListener("change", markDirty);

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    for (const item of fields) item.clear();
    status.dataset.tone = "";
    status.textContent = "Saving";
    form.dataset.busy = "true";
    button.disabled = true;
    const res = await send();
    delete form.dataset.busy;
    if (res.ok) {
      delete form.dataset.dirty;
      status.textContent = "Saved";
      status.dataset.tone = "ok";
      applied(res.data);
      return;
    }
    button.disabled = false;
    status.dataset.tone = "bad";
    const named = res.status === 400 && fields.find((item) => item.name === res.data.field);
    if (named) {
      named.fail();
      named.input.focus();
      status.textContent = "Not saved";
    } else {
      status.textContent = res.status === 400 ? `The server rejected ${res.data.field || "the request"}.` : reason(res);
    }
  });

  return {
    root: form,
    // Adopt the server's values unless the owner is mid-edit in this form.
    isEditing: () => form.dataset.dirty === "true",
  };
}

// An action that cannot be undone. The first press only asks; the second, on a button that
// names the consequence, does it. Escape or Cancel backs out. Nothing is changed until the
// server confirms, and a refusal is shown beside the button.
export function confirmAction({ label, confirm, warning, run, tone = "stop" }) {
  const root = el("div", "act");
  const start = el("button", "btn", label);
  start.type = "button";
  const ask = el("div", `act-ask ${tone}`);
  ask.hidden = true;
  const yes = el("button", `btn ${tone}`, confirm);
  yes.type = "button";
  const no = el("button", "btn quiet", "Cancel");
  no.type = "button";
  const buttons = el("div", "act-buttons");
  buttons.append(yes, no);
  ask.append(el("p", "act-warning", warning), buttons);
  const status = el("output", "set-status");
  root.append(start, ask, status);

  const open = (on) => {
    ask.hidden = !on;
    start.hidden = on;
    if (on) yes.focus();
    else start.focus();
  };
  start.addEventListener("click", () => {
    status.textContent = "";
    open(true);
  });
  no.addEventListener("click", () => open(false));
  ask.addEventListener("keydown", (event) => {
    if (event.key === "Escape") open(false);
  });
  yes.addEventListener("click", async () => {
    yes.disabled = true;
    no.disabled = true;
    status.dataset.tone = "";
    status.textContent = "Working";
    const res = await run();
    yes.disabled = false;
    no.disabled = false;
    ask.hidden = true;
    start.hidden = false;
    status.dataset.tone = res.ok ? "ok" : "bad";
    status.textContent = res.ok ? "Done" : reason(res);
    start.focus();
  });
  return { root, start, status };
}
