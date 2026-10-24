// The daily chart on Usage: one line per harness over the period, in that harness's colour,
// with a crosshair that names every harness's figure for the day under the pointer. Inline
// SVG, redrawn from the summary's `by_day` on every poll.

import { el, harnessKey, svg, tokens, displayMoney } from "./dom.js";

const HEIGHT = 300;
const PAD = { top: 16, right: 16, bottom: 28, left: 56 };
const TICKS = 4;
const DAY_MS = 86_400_000;

export const HARNESS = { "claude-code": "claude", claude: "claude", codex: "codex", opencode: "opencode", hermes: "hermes" };
export const colour = (harness) => `var(--h-${harnessKey(HARNESS[harness] || "virtual")})`;

/// `tokens` counts every token the model handled; `cost` is what they cost.
export const processed = (r) => r.tokens_in + r.tokens_out + (r.cache_read || 0) + (r.cache_write || 0);
export const metricValue = (metric, r) => (metric === "cost" ? r.cost_eur : processed(r));
export const metricLabel = (metric, v) => (metric === "cost" ? displayMoney(v) : tokens(v));

// "Nice" axis maximum: 1, 2, 5 times a power of ten, at or above `max`.
function niceMax(max) {
  if (max <= 0) return 1;
  const power = 10 ** Math.floor(Math.log10(max));
  for (const step of [1, 2, 5, 10]) if (step * power >= max) return step * power;
  return 10 * power;
}

const dayMs = (day) => Date.parse(`${day}T00:00:00`);
const dayLabel = (ms) => new Date(ms).toLocaleDateString("en-US", { month: "short", day: "numeric" }).toUpperCase();

// The days on the x axis: the range's span, or the data's for "all". A rolling range starts
// `days` ago at this hour, so its first day is a partial one and is shown.
function span(byDay, range) {
  const localDay = (ms) => dayMs(new Date(ms).toLocaleDateString("sv-SE"));
  const today = localDay(Date.now());
  const days = { "7d": 7, "30d": 30, "90d": 90 }[range];
  let first = range === "today" ? today : days ? localDay(Date.now() - days * DAY_MS) : byDay.length ? dayMs(byDay[0].day) : today;
  if (Number.isNaN(first)) first = today;
  return { first, last: today };
}

export function createDailyChart() {
  const figure = el("figure", "daily");
  const title = el("figcaption", null, "Daily processed tokens");
  const plot = svg("svg", { class: "daily-plot", role: "img", "aria-label": "Daily processed tokens by harness" });
  const tip = el("div", "daily-tip");
  tip.hidden = true;
  const wrap = el("div", "daily-wrap");
  wrap.append(plot, tip);
  figure.append(title, wrap);

  let model = null; // { days, series, first, width }

  function draw(summary, metric, range) {
    title.textContent = metric === "cost" ? "Daily cost" : "Daily processed tokens";
    plot.setAttribute("aria-label", title.textContent + " by harness");
    const width = Math.max(320, wrap.clientWidth || 640);
    plot.setAttribute("viewBox", `0 0 ${width} ${HEIGHT}`);
    plot.replaceChildren();
    const { first, last } = span(summary.by_day, range);
    const count = Math.max(1, Math.round((last - first) / DAY_MS) + 1);
    const harnesses = summary.by_harness.map((h) => h.harness);
    const series = new Map(harnesses.map((h) => [h, new Array(count).fill(0)]));
    for (const d of summary.by_day) {
      const i = Math.round((dayMs(d.day) - first) / DAY_MS);
      if (i >= 0 && i < count && series.has(d.harness)) series.get(d.harness)[i] += metricValue(metric, d);
    }
    const peak = Math.max(0, ...[...series.values()].flat());
    const top = niceMax(peak);
    const x0 = PAD.left;
    const x1 = width - PAD.right;
    const y0 = HEIGHT - PAD.bottom;
    const y1 = PAD.top;
    const x = (i) => (count === 1 ? (x0 + x1) / 2 : x0 + ((x1 - x0) * i) / (count - 1));
    const y = (v) => y0 - ((y0 - y1) * v) / top;

    for (let t = 0; t <= TICKS; t++) {
      const v = (top * t) / TICKS;
      plot.append(svg("line", { class: "grid", x1: x0, x2: x1, y1: y(v), y2: y(v) }));
      const label = svg("text", { class: "tick", x: x0 - 8, y: y(v) + 4, "text-anchor": "end" });
      label.textContent = metric === "cost" ? displayMoney(v) : tokens(v);
      plot.append(label);
    }
    const marks = count > 2 ? [0, Math.floor((count - 1) / 2), count - 1] : [0, count - 1];
    for (const i of new Set(marks)) {
      const label = svg("text", { class: "tick", x: x(i), y: y0 + 18, "text-anchor": i === 0 ? "start" : i === count - 1 ? "end" : "middle" });
      label.textContent = dayLabel(first + i * DAY_MS);
      plot.append(label);
    }

    // Costliest harness drawn last, so it sits on top; its fill is faint.
    for (const harness of [...harnesses].reverse()) {
      const values = series.get(harness);
      if (!values.some(Boolean)) continue;
      const line = values.map((v, i) => `${i ? "L" : "M"}${x(i).toFixed(1)} ${y(v).toFixed(1)}`).join(" ");
      const area = `${line} L${x(count - 1).toFixed(1)} ${y0} L${x(0).toFixed(1)} ${y0} Z`;
      plot.append(svg("path", { class: "area", d: area, fill: colour(harness) }));
      plot.append(svg("path", { class: "line", d: line, stroke: colour(harness) }));
      // A one-day span has no line to see; the value is a dot.
      if (count === 1) plot.append(svg("circle", { class: "dot", cx: x(0), cy: y(values[0]), r: 4, fill: colour(harness) }));
    }
    const cursor = svg("line", { class: "cursor", y1, y2: y0 });
    cursor.setAttribute("visibility", "hidden");
    plot.append(cursor);
    model = { count, series, first, x, cursor, metric, width };
  }

  function hover(event) {
    if (!model) return;
    const box = plot.getBoundingClientRect();
    const px = ((event.clientX - box.left) / box.width) * model.width;
    const i = Math.round(((px - PAD.left) / (model.width - PAD.left - PAD.right)) * (model.count - 1));
    if (i < 0 || i >= model.count) return leave();
    model.cursor.setAttribute("x1", model.x(i));
    model.cursor.setAttribute("x2", model.x(i));
    model.cursor.setAttribute("visibility", "visible");
    tip.replaceChildren(el("b", null, dayLabel(model.first + i * DAY_MS)));
    for (const [harness, values] of model.series) {
      if (!values[i]) continue;
      const row = el("span");
      const dot = el("i");
      dot.style.background = colour(harness);
      row.append(dot, el("span", null, harness), el("span", "daily-tip-num", metricLabel(model.metric, values[i])));
      tip.append(row);
    }
    tip.hidden = tip.childElementCount < 2;
    const left = (model.x(i) / model.width) * box.width;
    tip.style.left = `${Math.min(left + 12, box.width - tip.offsetWidth - 4)}px`;
  }
  function leave() {
    tip.hidden = true;
    if (model) model.cursor.setAttribute("visibility", "hidden");
  }
  plot.addEventListener("pointermove", hover);
  plot.addEventListener("pointerleave", leave);

  return { node: figure, draw };
}

// Tokens by type: one bar split into input, cache read, cache write and output, with the
// legend carrying the figures. Shares below 0.5% are kept visible at that width.
export function createTokenTypes() {
  const box = el("section", "types");
  const bar = el("div", "types-bar");
  bar.setAttribute("role", "img");
  const legend = el("ul", "types-legend");
  box.append(el("h2", null, "Tokens by type"), bar, legend);

  function draw(s) {
    const parts = [
      ["input", "Input", s.tokens_in],
      ["cache-read", "Cache read", s.cache_read],
      ["cache-write", "Cache write", s.cache_write_5m + s.cache_write_1h],
      ["output", "Output", s.tokens_out],
    ];
    const total = parts.reduce((sum, [, , v]) => sum + v, 0);
    bar.setAttribute("aria-label", parts.map(([, name, v]) => `${name} ${tokens(v)}`).join(", "));
    bar.replaceChildren(
      ...parts.filter(([, , v]) => v > 0).map(([key, , v]) => {
        const seg = el("i", `t-${key}`);
        seg.style.flexGrow = String(Math.max(0.005, total ? v / total : 0));
        return seg;
      }),
    );
    legend.replaceChildren(
      ...parts.map(([key, name, v]) => {
        const item = el("li");
        item.append(el("i", `t-${key}`), el("span", null, name), el("b", null, tokens(v)));
        return item;
      }),
    );
  }
  return { node: box, draw };
}
