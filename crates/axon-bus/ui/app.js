// Axon Bus dashboard: renders /api/stream snapshots. Agent-supplied text only ever goes
// through textContent, never markup.
"use strict";

const token = document.querySelector('meta[name="axon-token"]').content;
const state = { snapshot: { repos: [] }, selected: null, openRuns: new Set(), form: null };

// One original mark per harness: never a vendor logo.
const GLYPHS = {
  claude: "M12 3 21 12 12 21 3 12Z",
  codex: "M4 4h16v16H4Z",
  opencode: "M12 3a9 9 0 1 0 0.01 0Z",
  hermes: "M12 3 21 20H3Z",
  virtual: "M12 2 20.7 7v10L12 22 3.3 17V7Z",
};

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function glyph(harness) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", `glyph h-${GLYPHS[harness] ? harness : "virtual"}`);
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(ns, "path");
  path.setAttribute("d", GLYPHS[harness] || GLYPHS.virtual);
  svg.append(path);
  return svg;
}

function tokens(n) {
  if (!n) return "0";
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
  return String(n);
}

function walk(nodes, visit) {
  for (const node of nodes) {
    visit(node);
    walk(node.children || [], visit);
  }
}

function everyNode() {
  const all = [];
  for (const repo of state.snapshot.repos)
    for (const h of repo.harnesses) walk(h.roots, (n) => all.push(n));
  return all;
}

function renderCounts() {
  const all = everyNode();
  const count = (s) => all.filter((n) => n.status === s).length;
  const box = document.getElementById("counts");
  box.replaceChildren(
    ...[["active", count("active")], ["idle", count("idle")], ["agents", all.length]].map(([label, n]) => {
      const item = el("span", `count c-${label}`);
      item.append(el("b", null, n), el("span", null, label));
      return item;
    }),
  );
}

function nodeRow(node, depth) {
  const row = el("button", "node");
  row.type = "button";
  row.dataset.status = node.status;
  row.style.setProperty("--depth", depth);
  row.setAttribute("aria-pressed", String(state.selected === node.id));
  row.addEventListener("click", () => {
    state.selected = node.id;
    render();
  });
  const name = el("span", "name");
  name.append(el("span", "dot"), glyph(node.harness), el("span", "id", node.role ? `${node.role} · ${node.id}` : node.id));
  const meta = el("span", "meta");
  if (node.model) meta.append(el("span", "model", node.model));
  if (node.branch && depth === 0) meta.append(el("span", "badge branch", node.branch));
  if (node.repo_badge) meta.append(el("span", "badge repo", node.repo_badge.split("/").pop()));
  meta.append(el("span", "tok", tokens(node.tokens)));
  row.append(name, meta);
  const items = [row];
  for (const child of node.children || []) items.push(...nodeRow(child, depth + 1));
  return items;
}

function renderTree() {
  const tree = document.getElementById("tree");
  if (!state.snapshot.repos.length) {
    tree.replaceChildren(el("p", "empty", "No agents yet. Start a Claude, Codex, OpenCode or Hermes session with the hooks installed."));
    return;
  }
  tree.replaceChildren(
    ...state.snapshot.repos.map((repo) => {
      const group = el("section", "repo");
      const head = el("h2", "repo-name", repo.name);
      if (repo.repo) head.title = repo.repo;
      group.append(head);
      for (const h of repo.harnesses) {
        const lane = el("div", "harness");
        const label = el("h3", "harness-name");
        label.append(glyph(h.harness), el("span", null, h.harness));
        lane.append(label);
        for (const root of h.roots) lane.append(...nodeRow(root, 0));
        group.append(lane);
      }
      return group;
    }),
  );
}

function narrativeRow(row, index) {
  if (row.kind === "tool_run") {
    const key = `${state.selected}:${index}`;
    const run = el("details", "row tools");
    run.open = state.openRuns.has(key);
    run.addEventListener("toggle", () => (run.open ? state.openRuns.add(key) : state.openRuns.delete(key)));
    const counts = {};
    for (const t of row.tools) counts[t.name] = (counts[t.name] || 0) + 1;
    const summary = el("summary", null, Object.entries(counts).map(([n, c]) => `${c} ${n}`).join(" · "));
    const failed = row.tools.filter((t) => t.failed).length;
    if (failed) summary.append(el("span", "failed", ` · ${failed} failed`));
    const list = el("ul");
    for (const t of row.tools) list.append(el("li", t.failed ? "failed" : null, t.detail ? `${t.name}  ${t.detail}` : t.name));
    run.append(summary, list);
    return run;
  }
  const item = el("article", `row ${row.kind}`);
  const label = row.kind === "reasoning" && !row.recorded ? row.label : row.kind;
  const head = el("header", null, label);
  if (row.tokens) head.append(el("span", "tok", `${tokens(row.tokens)} tokens`));
  item.append(head);
  if (row.text) item.append(el("p", null, row.text));
  else if (row.kind !== "reasoning" || row.recorded) item.append(el("p", "structure", "content capture is off"));
  return item;
}

function senderFor(node) {
  const all = everyNode();
  const parent = all.find((n) => (n.children || []).some((c) => c.id === node.id));
  return parent ? parent.id : null;
}

function messageForm(node) {
  const form = el("form", "send");
  const from = senderFor(node);
  if (!from) {
    form.append(el("p", "hint", "Roots take messages from their children or linked roots."));
    return form;
  }
  const kind = el("select");
  for (const k of ["stop", "redirect", "question", "sync"]) kind.append(new Option(k, k));
  const body = el("input");
  body.maxLength = 400;
  body.required = true;
  body.placeholder = `Message ${node.id} as ${from}`;
  const button = el("button", null, "Send");
  const status = el("output");
  form.append(kind, body, button, status);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const response = await fetch("/api/msg", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Axon-Token": token },
      body: JSON.stringify({ from_id: from, to_id: node.id, kind: kind.value, body: body.value }),
    });
    const reply = await response.json().catch(() => ({}));
    status.textContent = response.ok ? "sent" : reply.error || `failed (${response.status})`;
    if (response.ok) body.value = "";
  });
  return form;
}

function renderDetail() {
  const detail = document.getElementById("detail");
  const node = everyNode().find((n) => n.id === state.selected);
  if (!node) {
    detail.replaceChildren(el("p", "empty", "Select an agent to follow what it says and thinks."));
    return;
  }
  const head = el("header", "agent");
  const title = el("h2");
  title.append(glyph(node.harness), el("span", null, node.id));
  const facts = el("dl");
  for (const [k, v] of [["status", node.status], ["model", node.model], ["role", node.role], ["branch", node.branch], ["tokens", tokens(node.tokens)]]) {
    if (v) facts.append(el("dt", null, k), el("dd", null, v));
  }
  head.append(title, facts);
  if (node.mission) head.append(el("p", "mission", node.mission));
  const stream = el("div", "narrative");
  const rows = node.narrative || [];
  if (!rows.length) stream.append(el("p", "empty", "Nothing recorded yet."));
  rows.forEach((row, i) => stream.append(narrativeRow(row, i)));
  // The form survives snapshot updates in place, so a half-typed message keeps its text
  // and focus; it is rebuilt only when the recipient or the sender changes.
  const formKey = `${node.id}\n${senderFor(node)}`;
  if (state.form && state.form.key === formKey && state.form.element.parentNode === detail) {
    while (detail.firstChild !== state.form.element) detail.firstChild.remove();
    detail.insertBefore(head, state.form.element);
    detail.insertBefore(stream, state.form.element);
    return;
  }
  state.form = { key: formKey, element: messageForm(node) };
  detail.replaceChildren(head, stream, state.form.element);
}

function render() {
  renderCounts();
  renderTree();
  renderDetail();
}

function connect() {
  const link = document.getElementById("link");
  const source = new EventSource("/api/stream");
  source.addEventListener("open", () => {
    link.dataset.state = "live";
    link.textContent = "live";
  });
  source.addEventListener("snapshot", (event) => {
    state.snapshot = JSON.parse(event.data);
    render();
  });
  source.addEventListener("error", () => {
    link.dataset.state = "down";
    link.textContent = "reconnecting";
  });
}

connect();
