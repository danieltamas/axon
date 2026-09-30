// The projects overview: one row per project answering what the operator glances for —
// what is being worked on, how many sessions are working, what it uses, and what needs
// them. Rows keep a stable alphabetical order; selecting one opens the project.

import { bytes, el, glyph, setText, since, tokens, usd, walk } from "./dom.js";

const RECENT_MS = 15 * 60 * 1000;

export function projectKey(repo) {
  return repo.repo || "none";
}

// Everything shown about one project, derived from the snapshot.
export function summarize(repo, messages, now) {
  const ids = new Set();
  const harnesses = new Map();
  const attention = [];
  const work = [];
  const facts = { sessions: 0, working: 0, agents: 0, rss: 0, tokens: 0, cost: 0, unpriced: 0, last: 0 };
  for (const lane of repo.harnesses) {
    const h = harnesses.get(lane.harness) || { total: 0, working: 0 };
    walk(lane.roots, (node, depth) => {
      ids.add(node.id);
      facts.agents += 1;
      if (depth === 0) {
        facts.sessions += 1;
        h.total += 1;
        if (node.status === "active") {
          facts.working += 1;
          h.working += 1;
          work.push(node);
        }
      }
      facts.rss += node.rss || 0;
      facts.tokens += node.tokens || 0;
      facts.cost += node.cost_usd || 0;
      if (node.unpriced && node.tokens) facts.unpriced += 1;
      const rows = node.narrative || [];
      facts.last = Math.max(facts.last, node.last_ts || 0, rows.length ? rows[rows.length - 1].ts || 0 : 0);
      if (node.status === "orphaned") attention.push({ level: "stop", text: `${node.id} lost its session`, agent: node.id });
      if (node.budget) {
        const used = Math.round(node.budget.used * 100);
        if (node.budget.state === "stopped") attention.push({ level: "stop", text: `${node.id} stopped by its budget`, agent: node.id });
        else if (node.budget.state === "warned" || node.budget.used >= 0.8) attention.push({ level: "warn", text: `${node.id} at ${used}% of its budget`, agent: node.id });
      }
    });
    harnesses.set(lane.harness, h);
  }
  const talk = messages.filter((m) => ids.has(m.from) || ids.has(m.to));
  for (const m of talk) {
    facts.last = Math.max(facts.last, m.sent_at || 0);
    if (m.needs_reply && !m.acked_at) attention.push({ level: "ask", text: `${m.to} owes ${m.from} an answer`, thread: m.thread, since: m.sent_at });
  }
  return { ids, harnesses, attention: group(attention), work, facts, recent: talk.filter((m) => m.sent_at && now - m.sent_at < RECENT_MS).length };
}

// The same alert more than once reads as one, counted, pointing at its oldest instance.
// Stops first, then unanswered questions, then budget warnings.
function group(attention) {
  const rank = { stop: 0, ask: 1, warn: 2 };
  const seen = new Map();
  for (const a of attention) {
    const known = seen.get(a.text);
    if (known) {
      known.count += 1;
      if (a.since && a.since < known.since) Object.assign(known, { since: a.since, thread: a.thread });
    } else seen.set(a.text, { ...a, count: 1 });
  }
  return [...seen.values()].sort((a, b) => rank[a.level] - rank[b.level] || (a.since || 0) - (b.since || 0));
}

export function createOverview(container, onOpen) {
  const rows = new Map();
  const summary = el("p", "summary");
  const list = el("div", "projects");
  const head = el("div", "p-cols");
  head.setAttribute("aria-hidden", "true");
  for (const name of ["Project", "Current work", "Sessions", "Usage", "Needs you"]) head.append(el("span", null, name));

  function row(key) {
    let entry = rows.get(key);
    if (entry) return entry;
    const button = el("button", "project");
    button.type = "button";
    button.addEventListener("click", () => onOpen(key));
    const who = el("span", "p-who");
    const name = el("span", "p-name");
    const path = el("span", "p-path");
    const kinds = el("span", "p-kinds");
    const where = el("span", "p-where");
    where.append(path, kinds);
    who.append(name, where);
    const work = el("span", "p-work");
    const sessions = el("span", "p-sessions");
    const working = el("b");
    const total = el("small");
    sessions.append(working, total);
    const usage = el("span", "p-usage");
    const usageTop = el("span");
    const usageSub = el("small");
    usage.append(usageTop, usageSub);
    const needs = el("span", "p-needs");
    button.append(who, work, sessions, usage, needs);
    entry = { button, name, path, kinds, work, working, total, usageTop, usageSub, needs, keys: {} };
    rows.set(key, entry);
    return entry;
  }

  // Redraw a piece only when what it shows changed, so a busy bus stays cheap.
  function once(entry, part, key, draw) {
    if (entry.keys[part] === key) return;
    entry.keys[part] = key;
    draw();
  }

  function update(entry, repo, s, now) {
    setText(entry.name, repo.name);
    setText(entry.path, repo.repo ? shortPath(repo.repo) : "outside any repository");
    entry.path.title = repo.repo || "";
    entry.button.dataset.working = String(s.facts.working > 0);
    entry.button.setAttribute("aria-label", `Open ${repo.name}: ${s.facts.working} of ${s.facts.sessions} sessions working`);
    once(entry, "kinds", JSON.stringify([...s.harnesses]), () =>
      entry.kinds.replaceChildren(
        ...[...s.harnesses].map(([harness, h]) => {
          const kind = el("span", "p-kind");
          kind.title = `${harness}: ${h.working} of ${h.total} working`;
          kind.append(glyph(harness), el("span", null, String(h.total)));
          return kind;
        }),
      ),
    );
    const lines = workLines(s, now);
    once(entry, "work", JSON.stringify(lines), () => entry.work.replaceChildren(...lines.map(([cls, text]) => el("span", cls, text))));
    setText(entry.working, s.facts.working ? `${s.facts.working} working` : "idle");
    setText(entry.total, `${s.facts.sessions} open`);
    setText(entry.usageTop, `${tokens(s.facts.tokens)} tok`);
    setText(entry.usageSub, [s.facts.cost ? usd(s.facts.cost) : null, s.facts.rss ? bytes(s.facts.rss) : null].filter(Boolean).join(" · "));
    once(entry, "needs", JSON.stringify(s.attention), () => {
      if (!s.attention.length) {
        entry.needs.replaceChildren(el("span", "p-none", "—"));
        return;
      }
      const first = s.attention[0];
      const total = s.attention.reduce((n, a) => n + a.count, 0);
      entry.needs.replaceChildren(el("b", `n-${first.level}`, String(total)), el("span", null, first.text));
    });
  }

  return {
    render(snapshot, now) {
      // Working projects first, then the rest; alphabetical within each, so rows only
      // move when a project starts or stops working.
      const working = (repo) => repo.harnesses.some((lane) => lane.roots.some((root) => root.status === "active"));
      const repos = (snapshot.repos || []).slice().sort((a, b) => working(b) - working(a) || a.name.localeCompare(b.name) || projectKey(a).localeCompare(projectKey(b)));
      const seen = new Set();
      let sessions = 0;
      let busy = 0;
      let rss = 0;
      let needs = 0;
      const order = repos.map((repo) => {
        const key = projectKey(repo);
        seen.add(key);
        const s = summarize(repo, snapshot.messages || [], now);
        sessions += s.facts.sessions;
        busy += s.facts.working;
        rss += s.facts.rss;
        needs += s.attention.length ? 1 : 0;
        const entry = row(key);
        update(entry, repo, s, now);
        return entry.button;
      });
      for (const key of rows.keys()) if (!seen.has(key)) rows.delete(key);
      if (!summary.isConnected) container.replaceChildren(summary, head, list);
      setText(
        summary,
        repos.length
          ? [`${repos.length} ${repos.length === 1 ? "project" : "projects"}`, `${sessions} sessions open`, `${busy} working now`, rss ? `${bytes(rss)} memory` : null, needs ? `${needs} need you` : null].filter(Boolean).join("  ·  ")
          : "",
      );
      if (!repos.length) {
        list.replaceChildren(empty());
        return;
      }
      const current = [...list.children];
      if (current.length !== order.length || current.some((node, i) => node !== order[i])) list.replaceChildren(...order);
    },
  };
}

// What the project is doing: the working roots' missions, or for sessions seen only from
// their process, their model and last turn; a quiet project says when it last worked.
function workLines(s, now) {
  if (!s.work.length) return [["p-quiet", s.facts.last ? `Quiet · last turn ${since(s.facts.last, now)}` : "Quiet"]];
  const lines = s.work.slice(0, 2).map((node) => ["p-line", node.mission || [node.model, node.last_ts ? `turn ${since(node.last_ts, now)}` : null].filter(Boolean).join(" · ") || node.id]);
  if (s.work.length > 2) lines.push(["p-more", `and ${s.work.length - 2} more`]);
  return lines;
}

function shortPath(path) {
  const parts = path.replace(/^\/Users\/[^/]+/, "~").split("/").filter(Boolean);
  return parts.length > 3 ? `…/${parts.slice(-2).join("/")}` : parts.join("/");
}

function empty() {
  const box = el("div", "empty-board");
  box.append(
    el("p", "empty-title", "No sessions open"),
    el("p", null, "Start Claude Code, Codex, OpenCode or Hermes in a repository. Axon lists it here within a few seconds; run axon-bus install to message, budget and stop it."),
  );
  return box;
}
