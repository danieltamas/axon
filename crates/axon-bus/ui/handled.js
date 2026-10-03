// The project's handled ledger (BUS-PLAN §3b): work items that are not files — a lead, an
// issue, a URL — that an agent here or on a paired machine has taken or finished, so the
// operator sees what nobody needs to start again. Read from GET /api/handled while a
// project is open; hidden while the ledger is empty.

import { el, since } from "./dom.js";
import { authFetch } from "./signin.js";

const POLL_MS = 5000;
const SHOWN = 12;

function row(entry, now) {
  const item = el("li", "hd-row");
  item.dataset.state = entry.state;
  const key = el("code", "hd-key", entry.key);
  key.title = entry.key;
  const who = el("span", "hd-who", `${entry.holder.replace(/^peer:/, "")} · ${since(entry.at, now)}`);
  item.append(el("span", "hd-state", entry.state === "done" ? "Done" : "Taken"), key, who);
  if (entry.note) item.append(el("span", "hd-note", entry.note));
  return item;
}

export function createHandled(root) {
  const title = el("h2", null, "Handled work");
  const count = el("span", "hd-count");
  const head = el("div", "hd-head");
  head.append(title, count);
  const list = el("ul", "hd-list");
  root.append(head, list);
  root.hidden = true;
  let repo = null;
  let timer = 0;
  let drawn = "";

  async function load() {
    const asked = repo;
    if (!asked) return;
    const response = await authFetch(`/api/handled?repo=${encodeURIComponent(asked)}`, { cache: "no-store" }).catch(() => null);
    if (!response || !response.ok || asked !== repo) return;
    const { entries = [] } = await response.json().catch(() => ({}));
    const key = JSON.stringify(entries);
    if (key === drawn) return;
    drawn = key;
    root.hidden = !entries.length;
    const taken = entries.filter((e) => e.state === "taken").length;
    count.textContent = `${taken} taken · ${entries.length - taken} done`;
    const now = Date.now();
    list.replaceChildren(...entries.slice(0, SHOWN).map((e) => row(e, now)));
    if (entries.length > SHOWN) list.append(el("li", "hd-more", `${entries.length - SHOWN} more: axon bus handled`));
  }

  return {
    // `path` is the open project's repository, or null when none is open.
    show(path) {
      if (path === repo) return;
      repo = path;
      drawn = "";
      root.hidden = true;
      list.replaceChildren();
      clearInterval(timer);
      if (!repo) return;
      load();
      timer = setInterval(load, POLL_MS);
    },
  };
}
