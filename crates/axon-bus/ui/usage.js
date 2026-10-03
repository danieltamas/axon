// Usage: what the agents cost and where it went, from Axon's usage record
// (/api/summary). Served only by `axon`; under the `axon-bus serve` alias the endpoint is
// absent and the view reports itself unavailable.

import { createBrain, modelHarness } from "./brain.js";
import { authFetch, showSignIn } from "./signin.js";
import { clock, displayMoney, duration, el, glyph, setText, taskCost, tokens } from "./dom.js";

const RANGES = [["today", "Today"], ["7d", "7 days"], ["30d", "30 days"], ["all", "All time"]];
const POLL_MS = 5000;
const FEED_ROWS = 12;
const TOP_TASKS = 8;
const HARNESS = { "claude-code": "claude" };

const count = (n) => Number(n || 0).toLocaleString("en-US");
const lines = (n) => (n >= 1e3 ? tokens(n) : String(n));

function remembered() {
  try {
    return localStorage.getItem("axon-usage-range") || "7d";
  } catch {
    return "7d";
  }
}

// The vendor prefix repeats on every row; the full name stays in the tooltip.
function modelCell(model) {
  const cell = el("span", "f-model", model.replace(/^claude-/, ""));
  cell.title = model;
  return cell;
}

export function createUsage(container, { onUnavailable, projectOf }) {
  let range = remembered();
  let timer = 0;
  let showing = false;
  let last = null;

  const title = el("h1", null, "Usage");
  const picker = el("div", "range");
  picker.setAttribute("role", "radiogroup");
  picker.setAttribute("aria-label", "Period");
  for (const [key, label] of RANGES) {
    const option = el("button", null, label);
    option.type = "button";
    option.dataset.range = key;
    option.setAttribute("role", "radio");
    option.addEventListener("click", () => {
      range = key;
      try {
        localStorage.setItem("axon-usage-range", key);
      } catch {
        // Storage can be off; the period then lasts for this page only.
      }
      markRange();
      load();
    });
    picker.append(option);
  }
  const titleRow = el("div", "usage-title");
  titleRow.append(title, picker);
  const totals = el("p", "totals");
  const periods = el("ul", "periods");
  const notices = el("div", "notices");
  const head = el("header", "usage-head");
  head.append(titleRow, totals, periods, notices);

  const canvas = el("canvas", "brain");
  canvas.setAttribute("role", "img");
  const tip = el("div", "brain-tip");
  tip.hidden = true;
  const legend = el("figcaption", "brain-legend", "Spend at the center, harnesses around it, models outside. Size is cost; flow is turns from the last few minutes, and a pulse is one landing now.");
  const figure = el("figure", "brain-wrap");
  figure.append(canvas, tip, legend);
  const brain = createBrain(canvas, tip);

  const feed = el("ol", "feed");
  const feedBox = el("section", "usage-feed");
  feedBox.append(el("h2", null, "Latest turns"), feed);
  const top = el("div", "usage-top");
  top.append(figure, feedBox);

  const models = el("tbody");
  const agents = el("tbody");
  const topTasks = el("tbody");
  const tables = el("div", "usage-tables");
  tables.append(
    table("Models", ["Model", "Turns", "Tokens in", "Tokens out", "Cost"], models),
    table("Agents", ["Agent", "Turns", "Tokens in", "Tokens out", "Cost"], agents),
    table("Top tasks", ["Task", "Turns", "Ran", "Waited on you", "Cost"], topTasks),
  );
  const rtk = el("p", "rtk");
  container.append(head, top, tables, rtk);
  markRange();

  function markRange() {
    for (const option of picker.children) option.setAttribute("aria-checked", String(option.dataset.range === range));
  }

  async function load() {
    const asked = range;
    let response;
    try {
      response = await authFetch(`/api/summary?range=${asked}`);
    } catch {
      return; // The server is restarting; the next poll retries.
    }
    if (response.status === 401) {
      showSignIn();
      return;
    }
    if (response.status === 404) {
      onUnavailable();
      return;
    }
    if (!response.ok || asked !== range) return;
    last = await response.json();
    render(last);
    loadTasks(asked);
  }

  // The costliest tasks of the period; tasks sum only the period's turns, like the totals.
  async function loadTasks(asked) {
    const response = await authFetch(`/api/tasks?range=${asked}&order=cost`).catch(() => null);
    if (!response || !response.ok || asked !== range) return;
    const { tasks = [] } = await response.json().catch(() => ({}));
    const costliest = [...tasks].sort((a, b) => b.cost.measured - a.cost.measured).slice(0, TOP_TASKS);
    topTasks.replaceChildren(
      ...costliest.map((t) => {
        const name = el("td");
        name.append(el("span", "u-name", t.name));
        name.title = t.key || t.name;
        name.append(el("small", "u-tag", t.kind === "declared" ? "ledger" : "request"));
        const row = el("tr");
        row.append(name, el("td", "num", count(t.turns)), el("td", "num", duration(t.elapsed_ms)), el("td", "num", duration(t.human_wait_ms)), el("td", "num", taskCost(t)));
        return row;
      }),
    );
    if (!costliest.length) {
      const row = el("tr");
      const cell = el("td", "f-empty", "No tasks in this period.");
      cell.colSpan = 5;
      row.append(cell);
      topTasks.append(row);
    }
  }

  function render(s) {
    setText(
      totals,
      [
        displayMoney(s.cost_eur),
        `${count(s.sessions)} sessions`,
        `${count(s.events)} turns`,
        `${tokens(s.tokens_in)} in`,
        `${tokens(s.tokens_out)} out`,
        `${tokens(s.cache_read)} cache read`,
        s.loc_added || s.loc_removed ? `+${lines(s.loc_added)} −${lines(s.loc_removed)} lines` : null,
      ]
        .filter(Boolean)
        .join("  ·  "),
    );
    renderPeriods(s);
    renderNotices(s);
    brain.update(s);
    renderFeed(s.recent || []);
    renderRows(models, s.by_model, s.cost_eur, (m) => {
      const name = el("span", "u-name");
      name.append(glyph(modelHarness(m.model)), el("span", null, m.model));
      return [name, m.unpriced ? "unpriced" : null];
    });
    renderRows(agents, s.by_agent, s.cost_eur, (a) => [el("span", "u-name", a.agent), a.is_subagent ? "subagent" : null]);
    const saved = s.rtk;
    setText(rtk, saved && saved.tokens_saved ? `rtk kept ${tokens(saved.tokens_saved)} tokens (${saved.saved_pct.toFixed(1)}%) out of context across ${count(saved.commands)} commands, all time.` : "");
  }

  function renderPeriods(s) {
    periods.replaceChildren();
    for (const [label, spent, cap] of [["Today", s.today_cost_eur, s.budget_day_eur], ["This week", s.week_cost_eur, s.budget_week_eur], ["This month", s.month_cost_eur, s.budget_month_eur]]) {
      const item = el("li");
      item.append(el("span", null, label), el("b", null, displayMoney(spent)));
      if (cap) {
        const used = spent / cap;
        item.append(el("small", null, `of ${displayMoney(cap)}`));
        const meter = el("span", "meter");
        const fill = el("i");
        fill.style.width = `${Math.min(100, used * 100)}%`;
        meter.dataset.level = used >= 1 ? "over" : used >= 0.8 ? "near" : "ok";
        meter.append(fill);
        item.append(meter);
      }
      periods.append(item);
    }
  }

  function renderNotices(s) {
    const found = [
      s.unpriced_models.length && ["warn", `Unpriced, so cost is a floor: ${s.unpriced_models.join(", ")}`],
      s.credit_priced_models.length && ["note", `Included-plan credits (${s.total_credits.toFixed(0)}): ${s.credit_priced_models.join(", ")}`],
      s.preview_priced_models.length && ["note", `Research-preview limit, no published rate: ${s.preview_priced_models.join(", ")}`],
    ].filter(Boolean);
    notices.replaceChildren(...found.map(([level, text]) => el("p", `notice n-${level}`, text)));
  }

  function renderFeed(recent) {
    feed.replaceChildren(
      ...recent.slice(0, FEED_ROWS).map((r) => {
        const row = el("li");
        const who = el("span", "f-who");
        who.append(glyph(HARNESS[r.harness] || r.harness), el("span", null, r.agent));
        // The repo opens its project when the dashboard knows it; otherwise it is plain text.
        const key = r.repo && projectOf(r.repo);
        const repo = el(key ? "a" : "span", r.repo ? "f-repo" : "f-repo none", r.repo || "no repo");
        if (r.repo) repo.title = r.repo;
        if (key) repo.href = `#/p/${encodeURIComponent(key)}`;
        row.append(el("time", null, clock(r.ts)), who, el("code", "f-session", r.session || ""), repo, modelCell(r.model), el("span", "f-num", tokens(r.tokens_out)), el("span", "f-num", r.pricing_kind === "unknown" ? "—" : displayMoney(r.cost_eur)));
        return row;
      }),
    );
    if (!recent.length) feed.append(el("li", "f-empty", "No turns in this period."));
  }

  return {
    show() {
      if (showing) return;
      showing = true;
      load();
      timer = setInterval(load, POLL_MS);
    },
    hide() {
      showing = false;
      clearInterval(timer);
    },
    redraw() {
      if (last) render(last);
    },
  };
}

function table(caption, columns, body) {
  const box = el("section", "usage-table");
  const grid = el("table");
  const head = el("tr");
  columns.forEach((c, i) => head.append(el("th", i ? "num" : null, c)));
  const thead = el("thead");
  thead.append(head);
  grid.append(thead, body);
  box.append(el("h2", null, caption), grid);
  return box;
}

// One row per model or agent, costliest first, with its share of spend as a bar.
function renderRows(body, rows, total, label) {
  body.replaceChildren(
    ...rows.map((r) => {
      const [name, tag] = label(r);
      const first = el("td");
      first.append(name);
      if (tag) first.append(el("small", "u-tag", tag));
      const share = el("span", "share");
      const fill = el("i");
      fill.style.width = `${total > 0 ? Math.max(0.5, (r.cost_eur / total) * 100) : 0}%`;
      share.append(fill);
      first.append(share);
      const row = el("tr");
      row.append(first, el("td", "num", count(r.events)), el("td", "num", tokens(r.tokens_in)), el("td", "num", tokens(r.tokens_out)), el("td", "num", displayMoney(r.cost_eur)));
      return row;
    }),
  );
}
