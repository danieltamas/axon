// The project's tasks (BUS-PLAN §3b–3c): the handled ledger — work items that are not files,
// a lead, an issue, a URL, taken or finished here or on a paired machine — and the tasks the
// work went to, ledger takes and operator requests, with what each cost. Read from
// GET /api/handled and GET /api/tasks while a project is open; hidden while both are empty.

import { duration, el, since, taskCost } from "./dom.js";
import { authFetch } from "./signin.js";

const POLL_MS = 5000;
const SHOWN = 12;
const TASKS_SHOWN = 8;

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

function taskRow(task) {
  const item = el("li", "hd-row hd-task");
  item.dataset.state = task.closed_at ? "done" : "taken";
  const name = el(task.kind === "declared" ? "code" : "span", "hd-key", task.name);
  name.title = task.name;
  const facts = [`${task.turns} turns`, duration(task.elapsed_ms), taskCost(task)];
  if (task.cost.tools_unpriced.length) facts.push(`unpriced: ${task.cost.tools_unpriced.join(", ")}`);
  item.append(el("span", "hd-state", task.closed_at ? "Closed" : "Open"), name, el("span", "hd-who", facts.join(" · ")));
  return item;
}

async function fetchJson(url) {
  const response = await authFetch(url, { cache: "no-store" }).catch(() => null);
  if (!response || !response.ok) return null;
  return response.json().catch(() => null);
}

export function createHandled(root) {
  const title = el("h2", null, "Tasks");
  const count = el("span", "hd-count");
  const head = el("div", "hd-head");
  head.append(title, count);
  const list = el("ul", "hd-list");
  const tasks = el("ul", "hd-list hd-tasks");
  root.append(head, list, tasks);
  root.hidden = true;
  let repo = null;
  let timer = 0;
  let drawn = "";

  async function load() {
    const asked = repo;
    if (!asked) return;
    const query = encodeURIComponent(asked);
    const [ledger, work] = await Promise.all([fetchJson(`/api/handled?repo=${query}`), fetchJson(`/api/tasks?range=all&repo=${query}`)]);
    if (asked !== repo || (!ledger && !work)) return;
    const entries = (ledger && ledger.entries) || [];
    const recent = ((work && work.tasks) || []).slice(0, TASKS_SHOWN);
    const key = JSON.stringify([entries, recent.map((t) => [t.id, t.turns, t.closed_at, t.cost])]);
    if (key === drawn) return;
    drawn = key;
    root.hidden = !entries.length && !recent.length;
    const taken = entries.filter((e) => e.state === "taken").length;
    const open = recent.filter((t) => !t.closed_at).length;
    count.textContent = `${taken} taken · ${entries.length - taken} done · ${open} open`;
    const now = Date.now();
    list.replaceChildren(...entries.slice(0, SHOWN).map((e) => row(e, now)));
    if (entries.length > SHOWN) list.append(el("li", "hd-more", `${entries.length - SHOWN} more: axon bus handled`));
    tasks.replaceChildren(...recent.map(taskRow));
    if (recent.length) tasks.append(el("li", "hd-more", "all tasks: axon bus tasks --repo"));
  }

  return {
    // `path` is the open project's repository, or null when none is open.
    show(path) {
      if (path === repo) return;
      repo = path;
      drawn = "";
      root.hidden = true;
      list.replaceChildren();
      tasks.replaceChildren();
      clearInterval(timer);
      if (!repo) return;
      load();
      timer = setInterval(load, POLL_MS);
    },
  };
}
