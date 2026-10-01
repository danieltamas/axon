// Settings that act on the machine rather than store a value: the hooks each harness
// calls, the database file, and the browsers signed in. Every action waits for the server;
// the ones that cannot be undone ask twice.

import { el, glyph } from "./dom.js";
import { band, confirmAction, reason, request } from "./settings-kit.js";

const HARNESS_NAME = { claude: "Claude Code", codex: "Codex", opencode: "OpenCode", hermes: "Hermes" };

export function size(n) {
  if (!n) return "0 B";
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(2)} GB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MB`;
  if (n >= 1 << 10) return `${Math.round(n / (1 << 10))} KB`;
  return `${n} B`;
}

function hookRow(hook, apply) {
  const row = el("li", "hook");
  row.dataset.installed = String(hook.installed);
  const name = el("span", "hook-name");
  name.append(glyph(hook.harness), HARNESS_NAME[hook.harness] || hook.harness);
  const state = el("span", "hook-state", hook.installed ? "Hooks installed" : "Not installed");
  const path = el("span", "hook-path", hook.config_path);
  const status = el("output", "set-status");
  const act = hook.installed
    ? confirmAction({
        label: "Uninstall",
        confirm: "Uninstall hooks",
        warning: `Removes Axon's hook entries from ${hook.config_path}. Other tools' entries stay. ${HARNESS_NAME[hook.harness] || hook.harness} sessions stop reporting to Axon.`,
        run: async () => {
          const res = await request("POST", `/api/settings/hooks/${hook.harness}/uninstall`);
          if (res.ok) apply(res.data);
          return res;
        },
      }).root
    : installButton(hook, status, apply);
  row.append(name, state, act, path);
  if (!hook.installed) row.append(status);
  return row;
}

function installButton(hook, status, apply) {
  const button = el("button", "btn primary", "Install");
  button.type = "button";
  button.addEventListener("click", async () => {
    button.disabled = true;
    status.dataset.tone = "";
    status.textContent = "Installing";
    const res = await request("POST", `/api/settings/hooks/${hook.harness}/install`);
    button.disabled = false;
    if (res.ok) {
      apply(res.data);
      return;
    }
    status.dataset.tone = "bad";
    status.textContent = reason(res);
  });
  return button;
}

export function hooksSection({ apply }) {
  const { root, body } = band("hooks", "Hooks", "Each harness reports to Axon through hooks in its own config. This is the state doctor sees.");
  const list = el("ul", "hooks");
  list.setAttribute("aria-label", "Harness hooks");
  body.append(list);
  let key = "";
  return {
    root,
    sync({ hooks }) {
      const next = JSON.stringify(hooks);
      if (next === key) return;
      key = next;
      list.replaceChildren(...hooks.map((hook) => hookRow(hook, apply)));
    },
  };
}

function readout(items) {
  const list = el("dl", "readout");
  for (const [label, value] of items) {
    const item = el("div");
    item.append(el("dt", null, label), el("dd", null, value));
    list.append(item);
  }
  return list;
}

export function storageSection({ apply }) {
  const { root, body } = band("storage", "Storage", "The hub database. Compacting returns space that deleted rows left behind.");
  const figures = el("div", "readout-slot");
  const compact = confirmAction({
    label: "Compact database",
    confirm: "Compact now",
    warning: "Rewrites the database file and holds its write lock while it runs. If a hook is writing, nothing is changed and you will be told.",
    run: async () => {
      const res = await request("POST", "/api/settings/storage/compact");
      if (res.ok) apply(res.data);
      return res;
    },
    tone: "primary",
  });
  body.append(figures, compact.root);
  return {
    root,
    sync({ storage }) {
      figures.replaceChildren(readout([["Database", size(storage.db_bytes)], ["Write-ahead log", size(storage.wal_bytes)]]));
    },
  };
}

export function sessionsSection({ apply }) {
  const { root, body } = band("sessions", "Sessions", "Browsers signed in to this dashboard. Signing out the others leaves this one open.");
  const figures = el("div", "readout-slot");
  const note = el("p", "set-note");
  const out = confirmAction({
    label: "Sign out other browsers",
    confirm: "Sign out others",
    warning: "Every other browser signed in to this dashboard is signed out and needs a new login link from axon open.",
    run: async () => {
      const res = await request("POST", "/api/settings/sessions/revoke_others");
      if (res.ok) apply(res.data);
      return res;
    },
  });
  body.append(figures, note, out.root);
  return {
    root,
    sync({ storage }) {
      const live = storage.sessions;
      figures.replaceChildren(readout([["Signed in", String(live)]]));
      const alone = live <= 1;
      note.textContent = alone ? "This is the only browser signed in." : "";
      note.hidden = !alone;
      out.start.disabled = alone;
    },
  };
}
