// Messages on the tree (BUS-PLAN §7.2): each recent message is an arc between its sender
// and receiver, and a new one travels as a pulse. The open thread stays lit; the rest fade.

import { svg } from "./dom.js";

const ARCS = 16; // recent messages drawn as arcs
const FADE_MS = 90_000; // an arc fades out over this age
const PULSE_MS = 1100;

export function createArcs({ board, boardEl, overlay }) {
  const seen = new Set();
  let first = true;
  let last = { messages: [], links: [], visible: new Set(), thread: null };
  const reduced = matchMedia("(prefers-reduced-motion: reduce)");
  // Pulses get their own layer, so redrawing the arcs on a snapshot never cuts one short.
  const arcLayer = svg("g");
  const pulseLayer = svg("g");
  overlay.append(arcLayer, pulseLayer);

  // Between columns (a root and its subagent) a message runs card edge to mark; within
  // one column it bows out left of the marks.
  function arcPath(from, to) {
    const a = board.anchor(from);
    const b = board.anchor(to);
    if (!a || !b) return null;
    if (a.right <= b.left || b.right <= a.left) {
      const ax = a.right <= b.left ? a.right : a.left;
      const bx = a.right <= b.left ? b.left : b.right;
      const mid = (ax + bx) / 2;
      return `M${ax} ${a.y}C${mid} ${a.y} ${mid} ${b.y} ${bx} ${b.y}`;
    }
    // Clamped so a narrow outline never pushes the bow off the board's left edge.
    const bow = Math.max(2, Math.min(a.left, b.left) - 10 - Math.min(36, Math.abs(b.y - a.y) * 0.08));
    return `M${a.left} ${a.y}C${bow} ${a.y} ${bow} ${b.y} ${b.left} ${b.y}`;
  }

  function draw() {
    const { messages, links, visible, thread } = last;
    overlay.setAttribute("height", boardEl.scrollHeight);
    overlay.setAttribute("width", boardEl.clientWidth);
    const paths = [];
    for (const link of links) {
      if (!visible.has(link.a) || !visible.has(link.b)) continue;
      const d = arcPath(link.a, link.b);
      if (d) paths.push(svg("path", { d, class: "arc link" }));
    }
    const now = Date.now();
    const recent = messages.filter((m) => visible.has(m.from) && visible.has(m.to) && m.from !== m.to);
    for (const m of recent.slice(-ARCS)) {
      const d = arcPath(m.from, m.to);
      if (!d) continue;
      const path = svg("path", { d, class: `arc said k-${m.kind}` });
      const age = m.sent_at ? now - m.sent_at : FADE_MS / 2;
      const strength = thread ? (m.thread === thread ? 1 : 0.08) : Math.max(0.25, 1 - age / FADE_MS);
      path.style.setProperty("--strength", strength.toFixed(2));
      paths.push(path);
    }
    arcLayer.replaceChildren(...paths);
  }

  function pulse(m) {
    if (reduced.matches) {
      board.flash(m.to, m.kind);
      return;
    }
    const d = arcPath(m.from, m.to);
    if (!d) return;
    const trail = svg("path", { d, class: `pulse k-${m.kind}` });
    pulseLayer.append(trail);
    const length = trail.getTotalLength();
    const dash = Math.min(46, length * 0.35);
    trail.style.setProperty("stroke-dasharray", `${dash} ${length + dash}`);
    const run = trail.animate([{ strokeDashoffset: dash }, { strokeDashoffset: -length }], {
      duration: PULSE_MS,
      easing: "cubic-bezier(0.25, 1, 0.5, 1)",
    });
    board.flash(m.from, "send");
    run.onfinish = () => {
      trail.remove();
      board.flash(m.to, m.kind);
    };
  }

  return {
    update(snapshot, { visible, thread }) {
      const messages = snapshot.messages || [];
      last = { messages, links: snapshot.links || [], visible, thread };
      const fresh = messages.filter((m) => !seen.has(m.id));
      for (const m of fresh) seen.add(m.id);
      draw();
      if (!first) for (const m of fresh) if (visible.has(m.from) && visible.has(m.to)) pulse(m);
      first = false;
    },
    redraw: draw,
  };
}
