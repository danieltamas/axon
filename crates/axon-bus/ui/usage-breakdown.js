// Usage's headline block and breakdown table. The hero names the period's total in the
// chosen metric and lists each harness under it; the breakdown ranks models or days by that
// metric, each row carrying its share as a bar in the harness colour.

import { el, glyph, displayMoney, tokens } from "./dom.js";
import { modelHarness } from "./brain.js";
import { HARNESS, colour, metricLabel, metricValue, processed } from "./usage-chart.js";

const PAGE = 10;
const NAMES = { "claude-code": "Claude Code", claude: "Claude Code", codex: "Codex", opencode: "OpenCode", hermes: "Hermes" };
const count = (n) => Number(n || 0).toLocaleString("en-US");
const sessions = (n) => `${count(n)} ${n === 1 ? "session" : "sessions"}`;
const percent = (part, whole) => {
  if (!whole || !part) return "0%";
  const p = (100 * part) / whole;
  return p < 0.1 ? "<0.1%" : `${p.toFixed(1)}%`;
};

// Total in the metric, the period's sessions, then one entry per harness: its glyph, name,
// sessions and figure, with its share of the metric and the other figure beneath.
export function createHero() {
  const box = el("section", "hero");
  const figure = el("p", "hero-figure");
  const under = el("p", "hero-under");
  const list = el("ul", "hero-harnesses");
  box.append(figure, under, list);

  function draw(s, metric) {
    const total = metricValue(metric, { tokens_in: s.tokens_in, tokens_out: s.tokens_out, cache_read: s.cache_read, cache_write: s.cache_write_5m + s.cache_write_1h, cost_eur: s.cost_eur });
    figure.textContent = metricLabel(metric, total);
    under.textContent = sessions(s.sessions);
    list.replaceChildren(
      ...s.by_harness.map((h) => {
        const item = el("li");
        const head = el("div", "hero-harness");
        const dot = el("i");
        dot.style.background = colour(h.harness);
        head.append(dot, glyph(HARNESS[h.harness] || h.harness), el("b", null, NAMES[h.harness] || h.harness), el("small", null, sessions(h.sessions)), el("span", "hero-value", metricLabel(metric, metricValue(metric, h))));
        const other = metric === "cost" ? `${tokens(processed(h))} tokens` : displayMoney(h.cost_eur);
        item.append(head, el("p", "hero-share", `${percent(metricValue(metric, h), total)} of ${metric === "cost" ? "cost" : "tokens"} · ${other}`));
        return item;
      }),
    );
  }
  return { node: box, draw };
}

// Five figures in a row.
export function createTotals() {
  const box = el("section", "totals-row");
  const list = el("dl");
  box.append(el("h2", null, "Totals"), list);
  function draw(s) {
    const cacheWrite = s.cache_write_5m + s.cache_write_1h;
    const items = [
      ["Processed tokens", tokens(s.tokens_in + s.tokens_out + s.cache_read + cacheWrite)],
      ["Cached input", tokens(s.cache_read)],
      ["Uncached input", tokens(s.tokens_in)],
      ["Output", tokens(s.tokens_out)],
      ["Cost", displayMoney(s.cost_eur)],
    ];
    list.replaceChildren(
      ...items.flatMap(([name, value]) => {
        const group = el("div");
        group.append(el("dt", null, name), el("dd", null, value));
        return group;
      }),
    );
  }
  return { node: box, draw };
}

// Models or days, ranked by the metric, ten to a page. A model row is coloured by the family
// its id names; a day row sums every harness and takes the costliest harness's colour.
export function createBreakdown() {
  const box = el("section", "breakdown");
  const head = el("div", "breakdown-head");
  const picker = el("div", "range");
  picker.setAttribute("role", "radiogroup");
  picker.setAttribute("aria-label", "Breakdown by");
  let by = "model";
  for (const [key, label] of [["model", "Model"], ["day", "Day"]]) {
    const option = el("button", null, label);
    option.type = "button";
    option.dataset.by = key;
    option.setAttribute("role", "radio");
    option.addEventListener("click", () => {
      by = key;
      page = 0;
      mark();
      if (last) draw(...last);
    });
    picker.append(option);
  }
  head.append(el("h2", null, "Breakdown"), picker);
  const body = el("tbody");
  const table = el("table");
  const columns = el("tr");
  columns.append(el("th", "rank", "#"), el("th", null, "Model"), el("th", "num", "Cost"), el("th", "num", "Share"), el("th", "num", "Tokens"));
  const thead = el("thead");
  thead.append(columns);
  table.append(thead, body);
  const foot = el("div", "breakdown-foot");
  const where = el("span");
  const pager = el("div", "range");
  const prev = el("button", null, "Previous");
  const next = el("button", null, "Next");
  for (const [button, step] of [[prev, -1], [next, 1]]) {
    button.type = "button";
    button.addEventListener("click", () => {
      page += step;
      if (last) draw(...last);
    });
  }
  pager.append(prev, next);
  foot.append(where, pager);
  box.append(head, table, foot);
  let last = null;
  let page = 0;
  mark();

  function mark() {
    for (const option of picker.children) option.setAttribute("aria-checked", String(option.dataset.by === by));
  }

  function rows(s) {
    if (by === "model") {
      return s.by_model.map((m) => ({ name: m.model, tag: m.unpriced ? "unpriced" : null, colour: `var(--h-${modelHarness(m.model)})`, cost_eur: m.cost_eur, tokens: processed(m), glyph: modelHarness(m.model) }));
    }
    const days = new Map();
    for (const d of s.by_day) {
      const row = days.get(d.day) || { name: d.day, cost_eur: 0, tokens: 0, top: null, topValue: -1 };
      row.cost_eur += d.cost_eur;
      row.tokens += processed(d);
      if (d.cost_eur > row.topValue) Object.assign(row, { top: d.harness, topValue: d.cost_eur });
      days.set(d.day, row);
    }
    return [...days.values()].map((r) => ({ ...r, colour: colour(r.top), glyph: null }));
  }

  function draw(s, metric) {
    if (last && last[1] !== metric) page = 0;
    last = [s, metric];
    columns.children[1].textContent = by === "model" ? "Model" : "Day";
    const list = rows(s);
    const value = (r) => (metric === "cost" ? r.cost_eur : r.tokens);
    list.sort((a, b) => value(b) - value(a) || a.name.localeCompare(b.name));
    const total = list.reduce((sum, r) => sum + value(r), 0);
    const pages = Math.max(1, Math.ceil(list.length / PAGE));
    page = Math.min(Math.max(0, page), pages - 1);
    const first = page * PAGE;
    const shown = list.slice(first, first + PAGE);
    foot.hidden = list.length <= PAGE;
    where.textContent = `${first + 1}-${first + shown.length} of ${list.length}`;
    prev.disabled = page === 0;
    next.disabled = page >= pages - 1;
    body.replaceChildren(
      ...shown.map((r, i) => {
        const name = el("td", "b-name");
        const label = el("span", "u-name");
        if (r.glyph) label.append(glyph(r.glyph));
        label.append(el("span", null, r.name));
        name.append(label);
        if (r.tag) name.append(el("small", "u-tag", r.tag));
        const share = el("span", "share");
        const fill = el("i");
        fill.style.width = `${total > 0 ? Math.max(0.5, (value(r) / total) * 100) : 0}%`;
        fill.style.background = r.colour;
        share.append(fill);
        name.append(share);
        const row = el("tr");
        row.append(el("td", "rank", String(first + i + 1)), name, el("td", "num", displayMoney(r.cost_eur)), el("td", "num", percent(value(r), total)), el("td", "num", tokens(r.tokens)));
        return row;
      }),
    );
    if (!list.length) {
      const row = el("tr");
      const cell = el("td", "f-empty", "Nothing in this period.");
      cell.colSpan = 5;
      row.append(cell);
      body.append(row);
    }
  }
  return { node: box, draw };
}
