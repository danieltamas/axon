// Small DOM and formatting helpers. Agent-supplied text only ever goes through
// textContent, never markup.

const SVG = "http://www.w3.org/2000/svg";

// One original mark per harness: never a vendor logo.
const GLYPHS = {
  claude: "M12 4 20 12 12 20 4 12Z",
  codex: "M5.5 5.5h13v13h-13Z",
  opencode: "M12 4.5a7.5 7.5 0 1 0 .01 0Z",
  hermes: "M12 4.5 19.5 18.5h-15Z",
  virtual: "M12 3.8 19.1 7.9v8.2L12 20.2 4.9 16.1V7.9Z",
};

export function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

export function svg(tag, attrs = {}) {
  const node = document.createElementNS(SVG, tag);
  for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, value);
  return node;
}

export function harnessKey(harness) {
  return GLYPHS[harness] ? harness : "virtual";
}

export function glyph(harness, className = "glyph") {
  const mark = svg("svg", { viewBox: "0 0 24 24", class: `${className} h-${harnessKey(harness)}`, "aria-hidden": "true" });
  mark.append(svg("path", { d: GLYPHS[harnessKey(harness)] }));
  return mark;
}

// The harness glyph inside a budget ring; the ring's arc is the fraction used.
export function mark(harness) {
  const box = svg("svg", { viewBox: "0 0 28 28", class: `mark h-${harnessKey(harness)}`, "aria-hidden": "true" });
  box.append(
    svg("circle", { class: "ring-track", cx: 14, cy: 14, r: 12.5, pathLength: 100 }),
    svg("circle", { class: "ring-used", cx: 14, cy: 14, r: 12.5, pathLength: 100 }),
  );
  const shape = svg("path", { class: "shape", d: GLYPHS[harnessKey(harness)], transform: "translate(4.4 4.4) scale(0.8)" });
  box.append(shape);
  return box;
}

// Set the ring to `budget` (or clear it); the arc caps at a full circle.
export function setRing(box, budget) {
  const used = box.querySelector(".ring-used");
  const fraction = budget ? Math.min(1, Math.max(0, budget.used || 0)) : 0;
  // The ring warns from 80% on, before the gate has recorded it; a stop is the gate's.
  box.dataset.budget = !budget ? "none" : budget.state === "stopped" || fraction >= 1 ? "stopped" : budget.state === "warned" || fraction >= 0.8 ? "warned" : "ok";
  used.style.setProperty("stroke-dasharray", `${(fraction * 100).toFixed(1)} 100`);
}

// Update text only when it changed, so an unchanged row costs no layout.
export function setText(node, text) {
  const value = text === undefined || text === null ? "" : String(text);
  if (node.textContent !== value) node.textContent = value;
}

export function tokens(n) {
  if (!n) return "0";
  if (n >= 1e9) return `${(n / 1e9).toFixed(n >= 1e10 ? 0 : 1)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(n >= 1e7 ? 0 : 1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(n >= 1e4 ? 0 : 1)}K`;
  return String(n);
}

// Snapshot costs are USD; everything on the page shows in the display currency.
let currency = { code: "USD", per_usd: 1 };
const SYMBOLS = { USD: "$", EUR: "€", GBP: "£" };
// True when the currency changed, so views already drawn can redraw.
export function setCurrency(next) {
  if (!next || !next.code || !(next.per_usd > 0)) return false;
  const changed = next.code !== currency.code || next.per_usd !== currency.per_usd;
  currency = next;
  return changed;
}

// A value already in the display currency (Axon's usage summary).
export function displayMoney(value) {
  if (value === null || value === undefined) return "—";
  const symbol = SYMBOLS[currency.code] || `${currency.code} `;
  if (value < 0.01) return value > 0 ? `<${symbol}0.01` : `${symbol}0`;
  return `${symbol}${value < 100 ? value.toFixed(2) : Math.round(value).toLocaleString("en-US")}`;
}

export function money(usd) {
  return displayMoney(usd === null || usd === undefined ? usd : usd * currency.per_usd);
}

// "now", "4m", "3h", "2d": how long ago, for a glance rather than an audit.
export function ago(ms, now = Date.now()) {
  if (!ms) return "";
  const s = Math.max(0, (now - ms) / 1000);
  if (s < 45) return "now";
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

export function since(ms, now = Date.now()) {
  const when = ago(ms, now);
  return when === "now" ? "just now" : `${when} ago`;
}

// What the operator reads for an agent's status.
export const STATUS = { active: "working", idle: "idle", closed: "closed", orphaned: "lost session" };

export function bytes(n) {
  if (!n) return "—";
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)} GB`;
  return `${Math.round(n / (1 << 20))} MB`;
}

export function clock(ms) {
  if (!ms) return "--:--:--";
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}

export function walk(nodes, visit, depth = 0, parent = null) {
  for (const node of nodes) {
    visit(node, depth, parent);
    walk(node.children || [], visit, depth + 1, node);
  }
}

// Every agent in the snapshot, with its depth and parent id.
export function agents(snapshot) {
  const all = [];
  for (const repo of snapshot.repos || [])
    for (const lane of repo.harnesses)
      walk(lane.roots, (node, depth, parent) => all.push({ node, depth, parent: parent && parent.id }));
  return all;
}
