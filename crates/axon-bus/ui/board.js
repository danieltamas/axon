// One project's topology (BUS-PLAN §7.1): a section per harness, its sessions grouped by
// state, each with its subagents beside it. Cards are kept by agent id and updated in place,
// so a snapshot at 50 events a second changes text, not DOM, and a pulse never restarts.

import { activityChart } from "./activity.js";
import { bytes, el, glyph, isProcess, mark, setRing, setText, since, STATUS, tokens, money, walk } from "./dom.js";
import { createEnder } from "./send.js";

const HARNESS_NAMES = { claude: "Claude", codex: "Codex", opencode: "OpenCode", hermes: "Hermes" };

export function harnessName(harness) {
  return HARNESS_NAMES[harness] || harness;
}

export function createBoard(container, scroller, token, onSelect) {
  const cards = new Map();
  const sums = new Map();
  let shape = "";

  // A session is a card; a subagent is one aligned row beside it. The same agent keeps
  // its element across snapshots unless its place in the tree changes kind.
  function card(node, depth) {
    const kind = depth === 0 ? "root" : "sub";
    let entry = cards.get(node.id);
    if (entry && entry.kind === kind) return entry;
    const button = el("button", `node ${kind}`);
    button.type = "button";
    button.dataset.id = node.id;
    button.addEventListener("click", () => onSelect(node.id));
    const ring = mark(node.harness);
    const who = el("span", "who");
    const role = el("span", "role");
    const id = el("span", "id");
    who.append(role, id);
    const status = el("span", "state-word");
    const mission = el("span", "mission");
    const model = el("span", "model");
    const metrics = el("span", "metrics");
    const notes = el("span", "notes");
    entry = { kind, button, ring, role, id, status, mission, model, metrics, notes };
    if (kind === "root") {
      const head = el("span", "n-head");
      head.append(ring, who, status);
      const doing = el("span", "doing");
      const doingLabel = el("b");
      const doingText = el("span");
      doing.append(doingLabel, doingText);
      const spark = el("span", "spark");
      const foot = el("span", "n-foot");
      foot.append(model, metrics, notes);
      button.append(head, mission, doing, spark, foot);
      Object.assign(entry, { doing, doingLabel, doingText, spark });
    } else {
      who.append(mission);
      button.append(ring, who, model, metrics, status);
    }
    cards.set(node.id, entry);
    return entry;
  }

  function update(entry, node, selected, now) {
    const b = entry.button;
    if (b.dataset.status !== node.status) b.dataset.status = node.status;
    b.classList.toggle("observed", Boolean(node.observed));
    b.setAttribute("aria-pressed", String(selected === node.id));
    b.setAttribute("aria-label", `${node.role || "agent"} ${node.id}, ${STATUS[node.status] || node.status}`);
    setRing(entry.ring, node.budget);
    setText(entry.role, roleLabel(node));
    setText(entry.id, node.observed ? `pid ${node.pid || node.id.split("-").pop()}` : shortId(node.id));
    entry.id.title = node.id;
    setText(entry.status, STATUS[node.status] || node.status);
    setText(entry.model, node.model || (node.observed ? "no turns yet" : ""));
    if (entry.kind === "sub") {
      setText(entry.mission, node.mission || "");
      setText(entry.metrics, costLine(node));
      return;
    }
    // An observed session has no mission; when it last worked is the next best thing.
    setText(entry.mission, node.mission || (node.observed && node.last_ts ? `Last turn ${since(node.last_ts, now)}` : ""));
    const latest = doingNow(node.narrative || []);
    entry.doing.hidden = !latest;
    setText(entry.doingLabel, latest ? (node.status === "active" ? "Now" : latest.verb) : "");
    setText(entry.doingText, latest ? latest.text : "");
    const hours = node.activity ? node.activity.hours : null;
    const key = hours ? hours.join() : "";
    if (entry.spark.dataset.key !== key) {
      entry.spark.dataset.key = key;
      entry.spark.replaceChildren(...(hours ? [activityChart(hours, null, true)] : []));
    }
    setText(entry.metrics, [costLine(node), node.rss ? bytes(node.rss) : null].filter(Boolean).join(" · "));
    setText(entry.notes, notesLine(node));
  }

  // A subagent and, nested under it, its own subagents.
  function branch(node) {
    const box = el("div", "branch");
    box.append(card(node, 1).button);
    if (node.children && node.children.length) {
      const kids = el("div", "kids");
      kids.append(...node.children.map(branch));
      box.append(kids);
    }
    return box;
  }

  function family(root) {
    const box = el("div", "family");
    const subs = el("div", "subs");
    subs.append(...(root.children || []).map(branch));
    box.classList.toggle("solo", !subs.childElementCount);
    box.append(card(root, 0).button);
    // A session left open at a prompt is ended from its own card; the ender sits over the
    // card's corner as a sibling, since a button cannot hold another.
    if (isProcess(root)) {
      box.classList.add("endable");
      box.append(createEnder({ token, sessions: [root], label: "End", confirm: "Click to end", compact: true }));
    }
    if (subs.childElementCount) box.append(subs);
    return box;
  }

  function skeleton(repo) {
    sums.clear();
    if (!repo || !repo.harnesses.length) return [empty()];
    return repo.harnesses.map((lane) => {
      const section = el("section", "lane");
      const head = el("header", "lane-head");
      const name = el("h3");
      name.append(glyph(lane.harness), el("span", null, harnessName(lane.harness)));
      const sum = el("span", "sum");
      sums.set(lane.harness, sum);
      head.append(name, sum);
      section.append(head);
      for (const { label, roots } of bands(lane.roots)) {
        const band = el("div", "band");
        const title = el("h4", "band-head");
        title.append(el("span", null, label), el("span", "count", roots.length));
        const grid = el("div", "band-grid");
        grid.append(...roots.map(family));
        band.append(title, grid);
        section.append(band);
      }
      return section;
    });
  }

  // The skeleton is rebuilt only when the tree's shape changes; everything else is an
  // in-place update.
  function render(repo, selected) {
    const nextShape = repo ? JSON.stringify(repo.harnesses.map((h) => [h.harness, bands(h.roots).map((b) => [b.label, ids(b.roots)])])) : "";
    if (nextShape !== shape || !container.childElementCount) {
      shape = nextShape;
      container.replaceChildren(...skeleton(repo));
    }
    const now = Date.now();
    const seen = new Set();
    for (const lane of repo ? repo.harnesses : []) {
      let count = 0;
      let active = 0;
      let cost = 0;
      walk(lane.roots, (node, depth) => {
        seen.add(node.id);
        if (depth === 0) count += 1;
        if (depth === 0 && node.status === "active") active += 1;
        cost += node.cost_usd || 0;
        update(card(node, depth), node, selected, now);
      });
      const sum = sums.get(lane.harness);
      if (sum) setText(sum, [`${active} of ${count} working`, cost ? money(cost) : null].filter(Boolean).join(", "));
    }
    for (const id of cards.keys()) if (!seen.has(id)) cards.delete(id);
  }

  return {
    render,
    // Where an agent sits, in the scroller's content coordinates: its mark's centre and
    // left edge, and its card's right edge. Null when not shown.
    anchor(id) {
      const entry = cards.get(id);
      if (!entry || !entry.button.isConnected || !entry.button.offsetParent) return null;
      const origin = scroller.getBoundingClientRect();
      const dx = scroller.scrollLeft - origin.left;
      const dy = scroller.scrollTop - origin.top;
      const ring = entry.ring.getBoundingClientRect();
      const box = entry.button.getBoundingClientRect();
      return {
        x: ring.left + dx + ring.width / 2,
        y: ring.top + dy + ring.height / 2,
        left: ring.left + dx,
        right: box.right + dx,
      };
    },
    flash(id, kind) {
      const entry = cards.get(id);
      if (!entry) return;
      entry.button.dataset.hit = kind;
      entry.button.classList.remove("hit");
      void entry.button.offsetWidth;
      entry.button.classList.add("hit");
    },
  };
}

function roleLabel(node) {
  const role = node.role || "agent";
  // Two-letter roles are initialisms (QA, PM), not words.
  return role.length <= 2 ? role.toUpperCase() : role.charAt(0).toUpperCase() + role.slice(1);
}

// Tokens, price, memory the agent owns, and its budget: whatever is known, nothing padded.
export function metricsLine(node) {
  const parts = [`${tokens(node.tokens)} tok`];
  if (node.cost_usd !== null && node.cost_usd !== undefined && !node.unpriced) parts.push(money(node.cost_usd));
  else if (node.unpriced && node.tokens) parts.push("unpriced");
  if (node.rss) parts.push(bytes(node.rss));
  if (node.budget) parts.push(`${Math.round(node.budget.used * 100)}% budget`);
  return parts.join(" · ");
}

// The newest thing the agent said or ran. A reasoning row with nothing recorded and a
// line withheld by content capture say nothing, so the search goes further back.
export function doingNow(rows) {
  for (let i = rows.length - 1; i >= 0; i -= 1) {
    const row = rows[i];
    if (row.kind === "tool_run") {
      const tool = row.tools[row.tools.length - 1];
      const text = tool.detail ? `${tool.name} ${tool.detail}` : `${tool.name}${row.count > 1 ? ` · ${row.count} tools in this run` : ""}`;
      return { verb: "Ran", text };
    }
    if (row.text) return { verb: "Said", text: row.text };
  }
  return null;
}

// Tokens and price, the two figures every agent row carries.
function costLine(node) {
  const price = node.unpriced ? (node.tokens ? "unpriced" : null) : node.cost_usd ? money(node.cost_usd) : null;
  return [`${tokens(node.tokens)} tok`, price].filter(Boolean).join(" · ");
}

// A session UUID is noise at a glance; its first block tells sessions apart.
function shortId(id) {
  return /^[0-9a-f]{8}-[0-9a-f-]{27}$/.test(id) ? id.slice(0, 8) : id;
}

function notesLine(node) {
  const notes = [];
  if (node.branch && node.branch !== "main" && node.branch !== "master") notes.push(`on ${node.branch}`);
  if (node.repo_badge) notes.push(`working in ${node.repo_badge.split("/").pop()}`);
  return notes.join(" · ");
}

// Sessions grouped by what the operator does next: the ones that need them, the ones
// working, then the quiet ones. Quiet groups put the latest activity first; working ones
// keep the lane's order, so a turn landing does not reshuffle the cards being watched.
const BANDS = [
  ["Needs you", (n) => n.status === "orphaned" || (n.budget && n.budget.state === "stopped")],
  ["Working", (n) => n.status === "active"],
  ["Idle", (n) => n.status === "idle"],
  ["Closed", () => true],
];

function bands(roots) {
  const latest = (node) => Math.max(node.last_ts || 0, ...(node.children || []).map(latest));
  const recent = [...roots].sort((a, b) => latest(b) - latest(a));
  const left = new Set(roots);
  const out = [];
  for (const [label, test] of BANDS) {
    const picked = (label === "Working" ? roots : recent).filter((n) => left.has(n) && test(n));
    picked.forEach((n) => left.delete(n));
    if (picked.length) out.push({ label, roots: picked });
  }
  return out;
}

function ids(roots) {
  return roots.map((n) => [n.id, ids(n.children || [])]);
}

function empty() {
  const box = el("div", "empty-board");
  box.append(
    el("p", "empty-title", "Nothing is running here"),
    el("p", null, "Sessions in this project closed more than 15 minutes ago. Start one in the repository and it appears within a few seconds."),
  );
  return box;
}
