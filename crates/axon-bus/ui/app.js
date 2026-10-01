// The Axon dashboard. Projects renders /api/stream snapshots at two levels — every
// project, and one project's topology beside its context rail (conversations, one agent,
// or one thread). Usage renders Axon's spend record. Agent-supplied text only ever goes
// through textContent, never markup.

import { activityChart, sumHours } from "./activity.js";
import { createArcs } from "./arcs.js";
import { createBoard } from "./board.js";
import { createContext } from "./context.js";
import { agents, bytes, el, isProcess, money, setCurrency, setText, stats, tokens } from "./dom.js";
import { createOverview, projectKey, summarize } from "./overview.js";
import { offerInstall, onServerLost, registerWorker } from "./pwa.js";
import { createEnder } from "./send.js";
import { createSettings } from "./settings.js";
import { exchangeLogin, showSignIn } from "./signin.js";
import { createUsage } from "./usage.js";

const $ = (id) => document.getElementById(id);
// `selected` (an agent) and `thread` are exclusive: the rail shows one context at a time.
const state = { snapshot: { repos: [] }, project: null, selected: null, thread: null };

const layout = $("layout");
const overview = createOverview($("overview"), (key) => {
  location.hash = `#/p/${encodeURIComponent(key)}`;
});
const board = createBoard($("tree"), $("board"), (id) => focus({ selected: state.selected === id ? null : id }));
const arcs = createArcs({ board, boardEl: $("board"), overlay: $("arcs") });
const context = createContext($("context"), {
  onThread: (thread) => focus({ thread }),
  onAgent: (selected) => focus({ selected }),
  onBack: () => focus({}),
});

// Under the `axon-bus serve` alias there is no usage record; the view leaves the nav.
const usage = createUsage($("usage"), {
  onUnavailable: () => {
    document.querySelector('[data-view="usage"]').hidden = true;
    if (location.hash === "#/usage") location.hash = "#/";
  },
});

const settings = createSettings($("settings"));

function focus({ selected = null, thread = null }) {
  state.selected = selected;
  state.thread = thread;
  if (selected || thread) setView("activity");
  render();
}

// Below 860px one pane shows at a time: the agents, or the activity rail.
function setView(view) {
  layout.dataset.view = view;
  for (const tab of document.querySelectorAll("[data-tab]")) tab.setAttribute("aria-selected", String(tab.dataset.tab === view));
}
for (const tab of document.querySelectorAll("[data-tab]")) tab.addEventListener("click", () => setView(tab.dataset.tab));

// The hash is the level: `#/` is every project, `#/p/<repo>` one project and `#/usage`
// the spend record, `#/settings` the settings, so the back button walks out of a project.
function route() {
  const match = location.hash.match(/^#\/p\/(.+)$/);
  const project = match ? decodeURIComponent(match[1]) : null;
  if (project !== state.project) {
    state.project = project;
    state.selected = null;
    state.thread = null;
  }
  const onUsage = location.hash === "#/usage";
  const onSettings = location.hash === "#/settings";
  layout.dataset.level = onUsage ? "usage" : onSettings ? "settings" : project ? "project" : "overview";
  for (const link of document.querySelectorAll("[data-view]")) {
    const here = link.dataset.view === (onUsage ? "usage" : onSettings ? "settings" : "projects");
    if (here) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  if (onUsage) usage.show();
  else usage.hide();
  if (onSettings) settings.show();
  else settings.hide();
  document.body.dataset.level = layout.dataset.level;
  setView("agents");
  render();
}
window.addEventListener("hashchange", route);

function currentRepo() {
  return (state.snapshot.repos || []).find((repo) => projectKey(repo) === state.project) || null;
}

function renderCrumbs(repo) {
  const crumbs = $("crumbs");
  const key = state.project ? `${state.project}|${repo ? repo.name : ""}` : "";
  if (crumbs.dataset.key === key) return;
  crumbs.dataset.key = key;
  if (!state.project) {
    crumbs.replaceChildren();
    return;
  }
  const here = el("span", "crumb here", repo ? repo.name : "Closed project");
  here.setAttribute("aria-current", "page");
  crumbs.replaceChildren(el("span", "crumb-sep", "/"), here);
}

// The project's name, one line of totals, and what needs the operator, each item a
// shortcut to the thread or agent it is about.
function renderProjectHead(repo, s) {
  const head = $("project-head");
  const roots = repo ? repo.harnesses.flatMap((h) => h.roots) : [];
  const hours = sumHours(roots.map((r) => r.activity));
  // Sessions left open at a prompt, which only a signal can close (see isProcess).
  const idle = roots.filter((r) => isProcess(r) && r.status === "idle");
  const key = repo ? JSON.stringify([repo.name, repo.repo, s.facts, s.attention, hours, idle.map((r) => `${r.id}@${r.started_ms}`)]) : "";
  if (head.dataset.key === key) return;
  head.dataset.key = key;
  if (!repo) {
    head.replaceChildren();
    return;
  }
  const f = s.facts;
  const who = el("div", "ph-who");
  const title = el("h1", null, repo.name);
  who.append(title, el("p", "ph-path", repo.repo || "outside any repository"));
  who.append(
    stats([
      ["Sessions working", `${f.working} of ${f.sessions}`, f.working ? "signal" : null],
      ["Subagents", f.agents > f.sessions ? String(f.agents - f.sessions) : null],
      ["Tokens", tokens(f.tokens)],
      [f.unpriced ? "Spend, partly unpriced" : "Spend", f.cost ? money(f.cost) : null],
      ["Memory", f.rss ? bytes(f.rss) : null],
    ]),
  );
  head.replaceChildren(who);
  // Only observed sessions carry Axon's hourly record; a bus-only project has none.
  if (roots.some((r) => r.activity)) head.append(activityChart(hours, "turns per hour"));
  if (idle.length) {
    const n = idle.length;
    head.append(createEnder({ sessions: idle, label: `End ${n} idle ${n === 1 ? "session" : "sessions"}…`, confirm: `End ${n} idle: click again` }));
  }
  if (!s.attention.length) return;
  const list = el("ul", "needs");
  for (const a of s.attention) {
    const item = el("li");
    const button = el("button", `need n-${a.level}`);
    button.type = "button";
    button.append(el("span", null, a.text), el("b", null, a.count > 1 ? `×${a.count}` : ""));
    button.addEventListener("click", () => (a.thread ? focus({ thread: a.thread }) : focus({ selected: a.agent })));
    item.append(button);
    list.append(item);
  }
  head.append(list);
}

function render() {
  const repo = state.project ? currentRepo() : null;
  renderCrumbs(repo);
  if (layout.dataset.level === "usage" || layout.dataset.level === "settings") return;
  if (!state.project) {
    overview.render(state.snapshot, Date.now());
    return;
  }
  const messages = state.snapshot.messages || [];
  const s = repo ? summarize(repo, messages, Date.now()) : { ids: new Set(), facts: {}, attention: [] };
  if (state.selected && !s.ids.has(state.selected)) state.selected = null;
  renderProjectHead(repo, s);
  board.render(repo, state.selected);
  arcs.update(state.snapshot, { visible: s.ids, thread: state.thread });
  const scoped = messages.filter((m) => s.ids.has(m.from) || s.ids.has(m.to));
  const found = state.selected && agents(state.snapshot).find((a) => a.node.id === state.selected);
  const view = found ? { kind: "agent", found } : state.thread ? { kind: "thread", id: state.thread } : { kind: "list" };
  const roots = repo ? repo.harnesses.flatMap((h) => h.roots) : [];
  context.render(view, { messages: scoped, links: state.snapshot.links || [], content: state.snapshot.content, roots });
}

function connect() {
  const link = $("link");
  const source = new EventSource("/api/stream");
  source.addEventListener("open", () => {
    link.dataset.state = "live";
    setText(link, "Live");
  });
  source.addEventListener("snapshot", (event) => {
    state.snapshot = JSON.parse(event.data);
    if (setCurrency(state.snapshot.currency)) usage.redraw();
    requestAnimationFrame(render);
  });
  source.addEventListener("error", async () => {
    link.dataset.state = "down";
    setText(link, "Reconnecting");
    // An EventSource error carries no status; a plain request tells a lost session from a lost server.
    const probe = await fetch("/api/snapshot", { cache: "no-store" }).catch(() => null);
    if (probe && probe.status === 401) {
      source.close();
      showSignIn();
    } else {
      onServerLost();
    }
  });
}

const THEMES = ["auto", "light", "dark"];
function applyTheme(theme) {
  if (theme === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  $("theme").dataset.mode = theme;
  $("theme").setAttribute("aria-label", `Theme: ${theme}`);
  try {
    localStorage.setItem("axon-bus-theme", theme);
  } catch {
    // Storage can be off; the theme then lasts for this page only.
  }
}
$("theme").addEventListener("click", () => applyTheme(THEMES[(THEMES.indexOf($("theme").dataset.mode) + 1) % THEMES.length]));
let saved = "auto";
try {
  saved = localStorage.getItem("axon-bus-theme") || "auto";
} catch {
  // As above: no storage, no remembered theme.
}
applyTheme(THEMES.includes(saved) ? saved : "auto");

new ResizeObserver(() => arcs.redraw()).observe($("tree"));
exchangeLogin().then(() => {
  route();
  connect();
});
registerWorker();
offerInstall();
