// The project's context rail: its conversations by default, one agent (narrative and
// composer) when an agent is selected, one thread (messages and composer) when a
// conversation is opened. Never an empty column. Agent text only goes through textContent.

import { activityChart, activityFacts } from "./activity.js";
import { doingNow, harnessName, metricsLine } from "./board.js";
import { bytes, clock, el, mark, setRing, since, STATUS, tokens, money } from "./dom.js";
import { createComposer } from "./send.js";

const KIND_LABELS = { question: "Asked", answer: "Answered", redirect: "Redirected", sync: "Noted", stop: "Stopped", ack: "Acknowledged", handoff: "Handed off" };

export function createContext(container, { token, onThread, onAgent, onBack }) {
  const head = el("header", "ctx-head");
  const body = el("div", "ctx-body");
  const foot = el("footer", "ctx-foot");
  container.replaceChildren(head, body, foot);
  const drafts = new Map();
  const openRuns = new Set();
  // Details under an agent's head stay as the operator left them, across agents.
  let detailsOpen = false;
  const keys = { head: "", body: "", foot: "" };
  let composer = null;

  function part(name, key, draw) {
    if (keys[name] === key) return;
    keys[name] = key;
    draw();
  }

  function back(label) {
    const button = el("button", "back", `← ${label}`);
    button.type = "button";
    button.addEventListener("click", onBack);
    return button;
  }

  function threadsOf(messages) {
    const threads = new Map();
    for (const m of messages) {
      const t = threads.get(m.thread) || { id: m.thread, messages: [], people: new Set() };
      t.messages.push(m);
      t.people.add(m.from).add(m.to);
      threads.set(m.thread, t);
    }
    return [...threads.values()].sort((a, b) => last(b).sent_at - last(a).sent_at);
  }

  // ---- conversations ----
  function renderList(messages, roots, now) {
    const threads = threadsOf(messages);
    if (!threads.length) {
      renderLatest(roots, now);
      return;
    }
    part("head", `list|${threads.length}`, () => head.replaceChildren(el("h2", null, "Conversations"), el("span", "ctx-sub", `${threads.length} in this project`)));
    part("body", `list|${threads.map((t) => `${t.id}:${t.messages.length}:${pending(t) ? 1 : 0}`).join()}|${Math.floor(now / 30000)}`, () => {
      body.replaceChildren(
        ...threads.map((t) => {
          const m = last(t);
          const item = el("button", "thread-item");
          item.type = "button";
          item.addEventListener("click", () => onThread(t.id));
          const top = el("span", "ti-top");
          top.append(el("span", "ti-people", [...t.people].join(" ↔ ")), el("span", "ti-when", since(m.sent_at, now)));
          const meta = el("span", "ti-meta", `${t.messages.length} ${t.messages.length === 1 ? "message" : "messages"}`);
          if (pending(t)) item.dataset.waiting = "true";
          item.append(top, el("span", "ti-body", m.body), meta);
          if (pending(t)) meta.append(el("b", "waiting", ` · ${pending(t).to} owes an answer`));
          return item;
        }),
      );
    });
    part("foot", "list", () => {
      composer = null;
      foot.replaceChildren();
    });
  }

  // Without bus conversations the rail follows the sessions: what each last said or ran,
  // newest first, each a way into that session.
  function renderLatest(roots, now) {
    const latest = roots
      .map((node) => ({ node, said: doingNow(node.narrative || []) }))
      .filter((x) => x.said)
      .sort((a, b) => (b.node.last_ts || 0) - (a.node.last_ts || 0));
    part("head", "latest", () => head.replaceChildren(el("h2", null, "Latest from sessions"), el("span", "ctx-sub", "No bus conversations in this project")));
    part("body", `latest|${latest.map((x) => `${x.node.id}:${x.node.status}:${x.said.text}`).join()}|${Math.floor(now / 30000)}`, () => {
      if (!latest.length) {
        body.replaceChildren(teach("Nothing to follow yet", "When agents in this project ask, answer, redirect or hand off on the bus, each exchange appears here as a thread you can open and step into."));
        return;
      }
      body.replaceChildren(
        ...latest.map(({ node, said }) => {
          const item = el("button", "thread-item");
          item.type = "button";
          item.addEventListener("click", () => onAgent(node.id));
          const top = el("span", "ti-top");
          const who = node.observed ? `${harnessName(node.harness)} · pid ${node.pid}` : `${harnessName(node.harness)} · ${node.id}`;
          top.append(el("span", "ti-people", who), el("span", "ti-when", node.status === "active" ? "working" : node.last_ts ? since(node.last_ts, now) : ""));
          if (node.status === "active") top.lastChild.dataset.status = "active";
          item.append(top, el("span", "ti-body", `${said.verb === "Ran" ? "Ran " : ""}${said.text}`));
          if (node.mission) item.append(el("span", "ti-meta", node.mission));
          return item;
        }),
      );
    });
    part("foot", "list", () => {
      composer = null;
      foot.replaceChildren();
    });
  }

  // ---- one thread ----
  function renderThread(threadId, messages, now) {
    const inThread = messages.filter((m) => m.thread === threadId);
    if (!inThread.length) {
      onBack();
      return;
    }
    const people = [...new Set(inThread.flatMap((m) => [m.from, m.to]))];
    part("head", `thread|${threadId}|${people}`, () => {
      head.replaceChildren(back("Conversations"), el("h2", null, people.join(" ↔ ")), el("span", "ctx-sub", "Thread"));
    });
    part("body", `thread|${threadId}|${inThread.map((m) => `${m.id}${m.delivered_at ? "d" : ""}${m.acked_at ? "a" : ""}`).join()}`, () => {
      const shown = new Set(inThread.map((m) => m.id));
      body.replaceChildren(...inThread.map((m) => message(m, now, shown)));
      body.scrollTop = body.scrollHeight;
    });
    const peer = new Map();
    for (const m of inThread) {
      peer.set(m.to, m.from);
      peer.set(m.from, m.to);
    }
    const routes = [...peer].map(([to, from]) => ({ to, from }));
    const question = inThread.filter((m) => m.kind === "question" && m.needs_reply && !m.acked_at).pop() || null;
    part("foot", `thread|${threadId}|${JSON.stringify(routes)}|${question ? question.id : ""}`, () => {
      const intro = el("p", "foot-intro", question ? `${question.to} owes ${question.from} an answer. Answer on its behalf, or step in.` : "Step in on either side of this thread.");
      composer = createComposer({ token, routes, thread: threadId, question, drafts, draftKey: `thread:${threadId}` });
      foot.replaceChildren(intro, composer);
    });
  }

  function message(m, now, shown) {
    const item = el("article", `message k-${m.kind}`);
    const top = el("header");
    top.append(el("span", "m-kind", KIND_LABELS[m.kind] || m.kind), el("span", "m-route", `${m.from} → ${m.to}`), el("time", null, clock(m.sent_at)));
    const state = m.acked_at ? (m.needs_reply ? `answered ${since(m.acked_at, now)}` : `acknowledged ${since(m.acked_at, now)}`) : m.delivered_at ? (m.needs_reply ? "delivered · awaiting an answer" : "delivered") : "not delivered yet";
    item.append(top, el("p", null, m.body), el("span", "m-state", state));
    // A reference to a message already on screen (an answer's question) says nothing new.
    const refs = (m.refs || []).filter((ref) => !shown.has(ref));
    if (refs.length) {
      const list = el("ul", "m-refs");
      for (const ref of refs) list.append(el("li", null, ref));
      item.append(list);
    }
    return item;
  }

  // Where an observed session's narrative comes from, when there is none to show.
  const TRANSCRIPTS = {
    claude: "Its transcript was not found under ~/.claude/projects, or it has no turns yet.",
    codex: "Its transcript was not found under ~/.codex/sessions in the last 8 days.",
    opencode: "OpenCode keeps its transcript in its own store. Run axon bus install to see what it says.",
    hermes: "Hermes keeps its transcript in its own store. Run axon bus install to see what it says.",
  };

  // ---- one agent ----
  function renderAgent(found, messages, links, content, now) {
    const node = found.node;
    part("head", `agent|${node.id}|${node.status}|${JSON.stringify(node.budget)}|${node.mission}|${metricsLine(node)}|${node.model}|${node.branch}|${JSON.stringify(node.activity)}`, () => {
      const who = el("div", "agent");
      const ring = mark(node.harness);
      setRing(ring, node.budget);
      const names = el("div", "names");
      names.append(el("h2", null, cap(node.role || "agent")), el("span", "agent-id", node.observed ? `${harnessName(node.harness)} · pid ${node.pid || "—"}` : `${harnessName(node.harness)} · ${node.id}`));
      const status = el("span", "state-word", STATUS[node.status] || node.status);
      status.dataset.status = node.status;
      who.append(ring, names, status);
      const facts = el("dl", "facts");
      for (const [k, v] of [
        ["Model", node.model],
        ["Branch", node.branch],
        ["Tokens", tokens(node.tokens)],
        ["Cost", node.unpriced ? (node.tokens ? "unpriced model" : null) : money(node.cost_usd)],
        ["Memory", node.rss ? bytes(node.rss) : node.shares_process ? "shared with its parent" : null],
        ["Last turn", node.last_ts ? since(node.last_ts, now) : null],
      ]) {
        if (!v) continue;
        const pair = el("div");
        pair.append(el("dt", null, k), el("dd", null, v));
        facts.append(pair);
      }
      head.replaceChildren(back("Conversations"), who);
      // An observed session's mission is the operator's latest prompt, from its transcript.
      if (node.mission) {
        const mission = el("p", "agent-mission", node.mission);
        mission.title = node.mission;
        head.append(mission);
      }
      if (node.budget) head.append(gauge(node.budget));
      // The narrative is what the rail is for; the rest folds away under one line.
      const details = el("details", "agent-more");
      details.open = detailsOpen;
      details.addEventListener("toggle", () => (detailsOpen = details.open));
      const summary = el("summary", null, [node.model, node.tokens ? tokens(node.tokens) : null, node.last_ts ? since(node.last_ts, now) : null].filter(Boolean).join(" · ") || "Details");
      details.append(summary, facts);
      if (node.activity) details.append(activityChart(node.activity.hours, "turns per hour"), activityFacts(node.activity));
      head.append(details);
    });
    const rows = fold(node.narrative || []);
    part("body", `agent|${node.id}|${content}|${rows.length}|${rows.length ? rows[rows.length - 1].ts : 0}`, () => {
      const stuck = body.scrollHeight - body.scrollTop - body.clientHeight < 40;
      const list = [];
      if (node.observed && !rows.length) list.push(teach("No transcript to read", TRANSCRIPTS[node.harness] || "Axon has not ingested a turn from this session yet."));
      else if (!content) list.push(el("p", "capture-off", "Content capture is off: turns, tokens and tool runs only. Restart axon without --no-content to see what agents say."));
      if (!node.observed && !rows.length) list.push(el("p", "empty", "Nothing recorded for this agent yet."));
      rows.forEach((row, i) => list.push(narrativeRow(node.id, row, i)));
      body.replaceChildren(...list);
      if (stuck) body.scrollTop = body.scrollHeight;
    });
    const routes = node.observed ? [] : routesTo(found, links);
    part("foot", `agent|${node.id}|${JSON.stringify(routes)}`, () => {
      if (!routes.length) {
        composer = null;
        foot.replaceChildren(node.observed ? el("p", "foot-intro", "Messages need the bus: run axon bus install, then restart this session.") : el("p", "foot-intro", "Nothing on the bus connects to this agent yet, so there is no edge to message it along."));
        return;
      }
      composer = createComposer({ token, routes, drafts, draftKey: `agent:${node.id}` });
      foot.replaceChildren(composer);
    });
  }

  function narrativeRow(agent, row, index) {
    if (row.kind === "tool_run") {
      const key = `${agent}:${index}`;
      const run = el("details", "row tools");
      run.open = openRuns.has(key);
      run.addEventListener("toggle", () => (run.open ? openRuns.add(key) : openRuns.delete(key)));
      const counts = {};
      for (const t of row.tools) counts[t.name] = (counts[t.name] || 0) + 1;
      const summary = el("summary");
      for (const [name, n] of Object.entries(counts)) summary.append(el("span", "chip", `${n} ${name}`));
      const failed = row.tools.filter((t) => t.failed).length;
      if (failed) summary.append(el("span", "chip failed", `${failed} failed`));
      const list = el("ul");
      for (const t of row.tools) list.append(el("li", t.failed ? "failed" : null, t.detail ? `${t.name}  ${t.detail}` : t.name));
      run.append(summary, list);
      return run;
    }
    const recorded = row.kind !== "reasoning" || row.recorded;
    const item = el("article", `row ${row.kind}${recorded ? "" : " unrecorded"}`);
    const top = el("header");
    top.append(el("span", "row-kind", recorded ? (row.kind === "assistant" ? "Says" : cap(row.kind)) : row.label));
    if (row.tokens) top.append(el("span", "row-tok", `${tokens(row.tokens)} tok`));
    item.append(top);
    if (row.text) item.append(el("p", null, row.text));
    else if (recorded) item.append(el("p", "structure", "Text withheld: content capture is off."));
    return item;
  }

  return {
    // `view` is {kind: "list"} | {kind: "agent", found} | {kind: "thread", id}.
    render(view, { messages, links, content, roots = [] }) {
      const now = Date.now();
      container.dataset.mode = view.kind;
      if (view.kind === "agent") renderAgent(view.found, messages, links, content, now);
      else if (view.kind === "thread") renderThread(view.id, messages, now);
      else renderList(messages, roots, now);
    },
    // Keep a half-typed message when the page re-renders around it.
    focused: () => Boolean(composer && composer.contains(document.activeElement)),
  };
}

// Who the operator can message an agent through: its parent, else a linked root, else
// its first child. Each is an edge the bus already allows.
function routesTo(found, links) {
  const id = found.node.id;
  if (found.parent) return [{ to: id, from: found.parent }];
  const link = links.find((l) => l.a === id || l.b === id);
  if (link) return [{ to: id, from: link.a === id ? link.b : link.a }];
  const child = (found.node.children || []).find((c) => !c.observed);
  return child ? [{ to: id, from: child.id }] : [];
}

function gauge(budget) {
  const box = el("div", `gauge g-${budget.state}`);
  const bar = el("div", "meter");
  const fill = el("span", "fill");
  fill.style.setProperty("--used", String(Math.min(1, budget.used)));
  bar.append(fill, el("span", "tick warn-tick"), el("span", "tick"));
  const ceiling = [budget.tokens_max && `${tokens(budget.tokens_max)} tok`, budget.usd_max && money(budget.usd_max)].filter(Boolean).join(" · ");
  const label = el("p", "gauge-label");
  label.append(el("b", null, `${Math.round(budget.used * 100)}%`), el("span", null, ` of its ${budget.kind} budget, ${ceiling}`));
  if (budget.state !== "ok") label.append(el("span", "gauge-state", budget.state));
  box.append(label, bar);
  return box;
}

// Reasoning the harness did not record says only that the model thought; dropped, the
// tool calls on either side of it read as the one run they were.
function fold(rows) {
  const out = [];
  for (const row of rows) {
    if (row.kind === "reasoning" && !row.recorded) continue;
    const last = out[out.length - 1];
    if (row.kind === "tool_run" && last && last.kind === "tool_run") {
      const tools = [...last.tools, ...row.tools];
      out[out.length - 1] = { ...last, tools, count: tools.length };
    } else out.push(row);
  }
  return out;
}

function teach(title, text) {
  const box = el("div", "teach");
  box.append(el("p", "teach-title", title), el("p", null, text));
  return box;
}

function last(thread) {
  return thread.messages[thread.messages.length - 1];
}

function pending(thread) {
  return thread.messages.filter((m) => m.kind === "question" && m.needs_reply && !m.acked_at).pop() || null;
}

function cap(text) {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
