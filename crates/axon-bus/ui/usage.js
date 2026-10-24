// Usage: what the agents cost and where it went, from Axon's usage record
// (/api/summary). Served only by `axon`; under the `axon-bus serve` alias the endpoint is
// absent and the view reports itself unavailable.
//
// Top to bottom: the period's total with each harness under it beside the daily chart,
// the totals row, tokens by type, the model/day breakdown, then the brain with the latest
// turns, agents and the costliest tasks.

import { createBrain } from "./brain.js";
import { authFetch, showSignIn } from "./signin.js";
import { clock, displayMoney, duration, el, glyph, setText, taskCost, tokens } from "./dom.js";
import { createDailyChart, createTokenTypes, HARNESS } from "./usage-chart.js";
import { createBreakdown, createHero, createTotals } from "./usage-breakdown.js";

const RANGES = [["today", "Today"], ["7d", "7 days"], ["30d", "30 days"], ["90d", "90 days"], ["all", "All time"]];
const METRICS = [["tokens", "Tokens"], ["cost", "Cost"]];
const POLL_MS = 5000;
const FEED_ROWS = 12;
const TOP_TASKS = 8;

const count = (n) => Number(n || 0).toLocaleString("en-US");

function remembered(key, fallback, allowed) {
  try {
    const value = localStorage.getItem(key);
    return allowed.includes(value) ? value : fallback;
  } catch {
    return fallback;
  }
}

function remember(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage can be off; the choice then lasts for this page only.
  }
}

// A radio-style picker; `onPick` runs after the choice is marked.
function picker(label, options, current, onPick) {
  const box = el("div", "range");
  box.setAttribute("role", "radiogroup");
  box.setAttribute("aria-label", label);
  for (const [key, text] of options) {
    const option = el("button", null, text);
    option.type = "button";
    option.dataset.key = key;
    option.setAttribute("role", "radio");
    option.addEventListener("click", () => onPick(key));
    box.append(option);
  }
  const mark = (key) => {
    for (const option of box.children) option.setAttribute("aria-checked", String(option.dataset.key === key));
  };
  mark(current);
  return { node: box, mark };
}

// The vendor prefix repeats on every row; the full name stays in the tooltip.
function modelCell(model) {
  const cell = el("span", "f-model", model.replace(/^claude-/, ""));
  cell.title = model;
  return cell;
}

export function createUsage(container, { onUnavailable, projectOf }) {
  let range = remembered("axon-usage-range", "30d", RANGES.map(([k]) => k));
  let metric = remembered("axon-usage-metric", "tokens", METRICS.map(([k]) => k));
  let timer = 0;
  let showing = false;
  let last = null;

  const title = el("h1", null, "Usage");
  const metricPicker = picker("Metric", METRICS, metric, (key) => {
    metric = key;
    remember("axon-usage-metric", key);
    metricPicker.mark(key);
    if (last) render(last);
  });
  const rangePicker = picker("Period", RANGES, range, (key) => {
    range = key;
    remember("axon-usage-range", key);
    rangePicker.mark(key);
    load();
  });
  const controls = el("div", "usage-controls");
  controls.append(metricPicker.node, rangePicker.node);
  const titleRow = el("div", "usage-title");
  titleRow.append(title, controls);
  const periods = el("ul", "periods");
  const notices = el("div", "notices");
  const head = el("header", "usage-head");
  head.append(titleRow, periods, notices);

  const hero = createHero();
  const daily = createDailyChart();
  const headline = el("div", "usage-headline");
  headline.append(hero.node, daily.node);
  const totals = createTotals();
  const types = createTokenTypes();
  const breakdown = createBreakdown();

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

  const agents = el("tbody");
  const topTasks = el("tbody");
  const tables = el("div", "usage-tables");
  tables.append(
    table("Agents", ["Agent", "Turns", "Tokens in", "Tokens out", "Cost"], agents),
    table("Top tasks", ["Task", "Turns", "Ran", "Waited on you", "Cost"], topTasks),
  );
  const rtk = el("p", "rtk");
  container.append(head, headline, totals.node, types.node, breakdown.node, top, tables, rtk);

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
    renderPeriods(s);
    renderNotices(s);
    hero.draw(s, metric);
    daily.draw(s, metric, range);
    totals.draw(s);
    types.draw(s);
    breakdown.draw(s, metric);
    brain.update(s);
    renderFeed(s.recent || []);
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
      s.scanning && ["note", scanNotice(s)],
      s.unpriced_models.length && ["warn", `Unpriced, so cost is a floor: ${s.unpriced_models.join(", ")}`],
      s.credit_priced_models.length && ["note", `Included-plan credits (${s.total_credits.toFixed(0)}): ${s.credit_priced_models.join(", ")}`],
      s.preview_priced_models.length && ["note", `Research-preview limit, no published rate: ${s.preview_priced_models.join(", ")}`],
    ].filter(Boolean);
    notices.replaceChildren(...found.map(([level, text]) => el("p", `notice n-${level}`, text)));
  }

  // The figures shown are the store's; the scan adds what the logs hold beyond it.
  function scanNotice(s) {
    const progress = s.scan_total ? ` ${count(s.scan_read)} of ${count(s.scan_total)} files read.` : "";
    return (s.events ? "Reading new logs; the figures update when it finishes." : "Reading the logs for the first time; the figures land when it finishes.") + progress;
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

// One row per agent, costliest first, with its share of spend as a bar.
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
