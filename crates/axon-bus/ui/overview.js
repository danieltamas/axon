// The projects overview: headline figures, then the projects with work in progress (or
// something waiting on the operator) above the quiet ones. Each row answers what is being
// worked on, by which harnesses, at what cost; selecting one opens the project.

import { doingNow } from "./board.js";
import { bytes, el, glyph, setText, since, stats, tokens, money, walk } from "./dom.js";

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
    if (m.needs_reply && !m.acked_at && !m.recipient_closed) attention.push({ level: "ask", text: `${m.to} owes ${m.from} an answer`, thread: m.thread, since: m.sent_at });
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
  const figures = el("div", "ov-figures");
  const working = group("Working now");
  const quiet = group("Quiet");
  quiet.section.classList.add("quiet");

  function group(title) {
    const section = el("section", "p-group");
    const head = el("h2", "p-group-head");
    const name = el("span", null, title);
    const count = el("span", "p-count");
    head.append(name, count);
    const list = el("div", "projects");
    section.append(head, list);
    return { section, count, list };
  }

  function row(key) {
    let entry = rows.get(key);
    if (entry) return entry;
    const button = el("button", "project");
    button.type = "button";
    button.addEventListener("click", () => onOpen(key));
    const who = el("span", "p-who");
    const name = el("span", "p-name");
    const path = el("span", "p-path");
    who.append(name, path);
    const work = el("span", "p-work");
    const kinds = el("span", "p-kinds");
    const usage = el("span", "p-usage");
    button.append(who, work, kinds, usage);
    entry = { button, name, path, kinds, work, usage, keys: {} };
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
          kind.dataset.working = String(h.working > 0);
          kind.append(glyph(harness), el("span", null, h.working ? `${h.working}/${h.total}` : String(h.total)));
          return kind;
        }),
      ),
    );
    const lines = workLines(s, now);
    once(entry, "work", JSON.stringify(lines), () =>
      entry.work.replaceChildren(
        ...lines.map(([cls, text, count]) => {
          const line = el("span", cls);
          if (count) line.append(el("b", null, String(count)));
          line.append(el("span", null, text));
          return line;
        }),
      ),
    );
    setText(entry.usage, [s.facts.tokens ? `${tokens(s.facts.tokens)} tok` : null, s.facts.cost ? money(s.facts.cost) : null].filter(Boolean).join(" · "));
  }

  function place(target, entries) {
    setText(target.count, String(entries.length));
    target.section.hidden = !entries.length;
    const current = [...target.list.children];
    if (current.length !== entries.length || current.some((node, i) => node !== entries[i])) target.list.replaceChildren(...entries);
  }

  return {
    render(snapshot, now) {
      // Alphabetical inside each group, so a project stays where the operator last saw
      // it until it starts or stops working. "no repo" (no path) goes last.
      const repos = (snapshot.repos || []).slice().sort((a, b) => !a.repo - !b.repo || a.name.localeCompare(b.name) || projectKey(a).localeCompare(projectKey(b)));
      const seen = new Set();
      const busyRows = [];
      const quietRows = [];
      const total = { sessions: 0, working: 0, rss: 0, needs: 0, cost: 0 };
      for (const repo of repos) {
        const key = projectKey(repo);
        seen.add(key);
        const s = summarize(repo, snapshot.messages || [], now);
        total.sessions += s.facts.sessions;
        total.working += s.facts.working;
        total.rss += s.facts.rss;
        total.cost += s.facts.cost;
        total.needs += s.attention.length ? 1 : 0;
        const entry = row(key);
        update(entry, repo, s, now);
        (s.facts.working || s.attention.length ? busyRows : quietRows).push(entry.button);
      }
      for (const key of rows.keys()) if (!seen.has(key)) rows.delete(key);
      if (!figures.isConnected) container.replaceChildren(figures, working.section, quiet.section);
      const key = JSON.stringify([repos.length, total]);
      if (figures.dataset.key !== key) {
        figures.dataset.key = key;
        figures.replaceChildren(
          ...(repos.length
            ? [
                stats([
                  ["Sessions working", `${total.working} of ${total.sessions}`, total.working ? "signal" : null],
                  ["Projects", String(repos.length)],
                  ["Session spend", total.cost ? money(total.cost) : null],
                  ["Memory", total.rss ? bytes(total.rss) : null],
                  ["Need you", total.needs ? String(total.needs) : null, "warn"],
                ]),
              ]
            : [empty()]),
        );
      }
      place(working, busyRows);
      place(quiet, quietRows);
    },
  };
}

// What the project is doing: what needs the operator first, then one line per working
// session (its mission, else what it last did); a quiet project says when it last worked.
function workLines(s, now) {
  const lines = [];
  if (s.attention.length) {
    const first = s.attention[0];
    lines.push([`p-need n-${first.level}`, first.text, s.attention.reduce((n, a) => n + a.count, 0)]);
  }
  if (!s.work.length) {
    lines.push(["p-quiet", s.facts.last ? `Last turn ${since(s.facts.last, now)}` : "No turns recorded"]);
    return lines;
  }
  for (const node of s.work.slice(0, 2)) {
    const said = doingNow(node.narrative || []);
    lines.push(["p-line", node.mission || (said ? said.text : node.model || "Working")]);
  }
  if (s.work.length > 2) lines.push(["p-more", `and ${s.work.length - 2} more working`]);
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
    el("p", null, "Start Claude Code, Codex, OpenCode or Hermes in a repository. Axon lists it here within a few seconds; run axon bus install to message, budget and stop it."),
  );
  return box;
}
