// The brain's renderer. It runs in a worker on an OffscreenCanvas when the browser has
// one, else on the page's canvas; either way it only speaks messages (`handle`/`emit`),
// so the page never waits on a frame. Memory stays flat: halos are sprites drawn once
// per theme, particles and ripples come from fixed pools, nothing is allocated per frame.
//
// Motion is time-based throughout: every decay and approach is exponential in seconds,
// so it reads the same at 60 Hz and 120 Hz and a dropped frame never jumps. Positions
// live in unit space (a fraction of the canvas), so a resize rescales without a relayout,
// and a new layout glides from the old one instead of snapping.

import { createLabels } from "./brain-labels.js";
import { approach, cross, easeIn, easeInOut, easeOut, frame, halo, hsl, color, norm, peakCost, seeded } from "./brain-math.js";

const TAU = Math.PI * 2;
const PARTICLES = 96;
const RIPPLES = 24;
const STARS = 110;
const TRAIL = 5;
const AMBIENT = 30;
const INNER = 0.24;
const OUTER = 0.46;
const REST_SPIN = 0.085; // rad/s
const REST_PITCH = 0.2;
const BLOB = 20;
export function createRenderer(canvas, emit) {
  const ctx = canvas.getContext("2d");
  const labels = createLabels(ctx);
  let w = 0, h = 0, dpr = 1, base = 360;
  let theme = { dark: false, colors: {}, font: "monospace" };
  let nodes = [], edges = [], order = [], byId = new Map(), label = "";
  let sprites = {};
  let yaw = -0.6, yawTo = -0.6, pitch = REST_PITCH, pitchTo = REST_PITCH, spin = REST_SPIN;
  let drag = null, pointer = null, spawnDebt = 0;
  let hover = -1, running = false, visible = true, still = false, lastT = 0, clock = 0;
  const particles = Array.from({ length: PARTICLES }, () => ({ edge: -1, next: -1, age: 0, dur: 1, pulse: false, caption: "" }));
  const ripples = Array.from({ length: RIPPLES }, () => ({ node: -1, t: 1, caption: "" }));
  const stars = Array.from({ length: STARS }, (_, i) => {
    const y = 1 - (i / (STARS - 1)) * 2, ring = Math.sqrt(1 - y * y), th = i * 2.39996, r = OUTER * 1.9;
    return { p: [Math.cos(th) * ring * r, y * r, Math.sin(th) * ring * r], a: 0.15 + seeded(i) * 0.5, tw: seeded(i + 7) * TAU, x: 0, y: 0, s: 1, z: 0 };
  });

  function paint(n) {
    const tint = theme.colors[n.key] || theme.colors.muted || "#888888";
    const [hh, ss, ll] = hsl(n.kind === "core" ? theme.colors.signal || tint : tint);
    const hue = hh + n.hueShift;
    n.body = color(hue, ss, theme.dark ? ll + 4 : ll - 4);
    n.core = color(hue, Math.min(100, ss + 10), theme.dark ? 90 : Math.min(96, ll + 34));
    n.rim = color(hue, ss, theme.dark ? 80 : ll - 14, 0.8);
    n.dend = color(hue, ss, theme.dark ? 66 : ll, theme.dark ? 0.5 : 0.42);
    n.text = theme.dark ? color(hue, 60, 82) : color(hue, 45, Math.max(18, ll - 22));
    if (!sprites[n.sprite]) sprites[n.sprite] = halo(hue, ss, ll, theme.dark);
  }

  // Harnesses spread over an inner shell; each harness's models cluster on the outer
  // shell around its direction, so no line crosses the sphere. A node that already
  // exists keeps its place and state and glides to its new home; a new one grows out of
  // its parent.
  function layout(data) {
    const harnesses = data.nodes.filter((n) => n.kind === "harness");
    const dir = new Map();
    harnesses.forEach((n, i) => {
      const th = (i / harnesses.length) * TAU + 0.5;
      dir.set(n.id, norm([Math.cos(th), 0.45 * Math.sin(th * 2 + 1), Math.sin(th)]));
    });
    const counts = new Map();
    const peak = peakCost(data.nodes);
    const before = byId;
    const oldKeys = edges.map((e) => e.key);
    nodes = data.nodes.map((d, i) => {
      const was = before.get(d.id);
      const n = { ...d, i, seed: seeded(i + 1) * TAU, act: 0, flash: 0, lit: 1, grow: 0, hueShift: 0, sprite: d.kind === "core" ? "core" : d.key, x: 0, y: 0, s: 1, z: 0, depth: 1 };
      if (d.kind === "core") n.home = [0, 0, 0];
      else if (d.kind === "harness") n.home = dir.get(d.id).map((v) => v * INNER);
      else {
        const u = dir.get(d.parent) || [0, 1, 0];
        const j = counts.get(d.parent) || 0;
        counts.set(d.parent, j + 1);
        const a = Math.abs(u[1]) < 0.9 ? [0, 1, 0] : [1, 0, 0];
        const b1 = norm(cross(u, a)), b2 = cross(u, b1);
        const ang = j * 2.39996, spread = 0.28 + 0.34 * Math.sqrt(j / 6);
        n.home = norm(u.map((x, k) => x * Math.cos(spread) + (b1[k] * Math.cos(ang) + b2[k] * Math.sin(ang)) * Math.sin(spread))).map((x) => x * OUTER);
        n.hueShift = ((j % 5) - 2) * 7;
      }
      n.rTo = radius(n, peak);
      if (was) Object.assign(n, { p: was.p, r: was.r, act: was.act, flash: was.flash, lit: was.lit, grow: was.grow });
      else {
        const parent = before.get(d.parent);
        n.p = parent ? parent.p.slice() : n.home.slice();
        n.r = n.rTo;
      }
      const arms = d.kind === "core" ? 9 : d.kind === "harness" ? 7 : 5;
      n.dendrites = Array.from({ length: arms }, (_, k) => ({
        ang: (k / arms) * TAU + (seeded(i * 13 + k) - 0.5) * 0.9,
        len: 1.7 + seeded(i * 7 + k) * 1.2,
        ph: seeded(i * 3 + k) * TAU,
      }));
      return n;
    });
    byId = new Map(nodes.map((n) => [n.id, n]));
    edges = [];
    for (const n of nodes) {
      const parent = byId.get(n.parent);
      if (parent) edges.push({ a: parent.i, b: n.i, key: `${parent.id}>${n.id}`, weight: 0.2 + Math.sqrt(n.cost / peak), fire: 0, bend: (seeded(n.i + 31) - 0.5) * 0.3 });
    }
    // Particles in flight keep flying on the edge they were on, wherever it now sits.
    const index = new Map(edges.map((e, i) => [e.key, i]));
    for (const p of particles) {
      if (p.edge < 0) continue;
      p.edge = index.get(oldKeys[p.edge]) ?? -1;
      p.next = p.next >= 0 ? index.get(oldKeys[p.next]) ?? -1 : -1;
    }
    order = nodes.slice();
    for (const n of nodes) paint(n);
    const labelled = nodes.filter((n) => n.kind === "model").sort((a, b) => b.cost - a.cost).slice(0, 7);
    for (const n of labelled) n.labelled = true;
  }

  function radius(n, peak) {
    if (n.kind === "core") return 13;
    return n.kind === "harness" ? 6 + 9 * Math.sqrt(n.cost / peak) : 3 + 8 * Math.sqrt(n.cost / peak);
  }

  function project(p, out) {
    const cy = Math.cos(yaw), sy = Math.sin(yaw), cx = Math.cos(pitch), sx = Math.sin(pitch);
    const x1 = p[0] * cy + p[2] * sy, z1 = -p[0] * sy + p[2] * cy;
    const y1 = p[1] * cx - z1 * sx, z2 = p[1] * sx + z1 * cx;
    const s = 1.25 / (1.25 + z2);
    out.x = w / 2 + x1 * base * s * 1.25;
    out.y = h / 2 + y1 * base * s;
    out.s = s;
    out.z = z2;
  }

  // Flow is only where real turns landed lately (`live`, from the page) or just now
  // (`act`, from a pulse); an idle edge carries nothing.
  const flow = (e) => nodes[e.b].live + nodes[e.b].act;

  function pickEdge() {
    let sum = 0;
    for (const e of edges) sum += flow(e);
    if (sum <= 0) return -1;
    let r = Math.random() * sum;
    for (let i = 0; i < edges.length; i++) {
      r -= flow(edges[i]);
      if (r <= 0) return i;
    }
    return edges.length - 1;
  }

  function launch(edge, next, pulse, caption = "") {
    if (edge < 0) return;
    const free = particles.find((p) => p.edge < 0);
    if (!free) return;
    free.edge = edge;
    free.next = next;
    free.age = 0;
    free.pulse = pulse;
    free.caption = caption;
    free.dur = pulse ? 0.7 : 2.2 + Math.random() * 1.4;
  }

  // `caption` ("repo · model") is shown above the node while the ripple lasts.
  function ripple(node, caption = "") {
    const free = ripples.find((r) => r.t >= 1);
    if (free) Object.assign(free, { node, t: 0, caption });
  }

  // The point at t along an edge's gentle arc, into `out`.
  function along(e, t, out) {
    const a = nodes[e.a], b = nodes[e.b];
    const mx = (a.x + b.x) / 2 - (b.y - a.y) * e.bend, my = (a.y + b.y) / 2 + (b.x - a.x) * e.bend;
    const u = 1 - t;
    out.x = u * u * a.x + 2 * u * t * mx + t * t * b.x;
    out.y = u * u * a.y + 2 * u * t * my + t * t * b.y;
    out.s = a.s + (b.s - a.s) * t;
  }
  const spot = { x: 0, y: 0, s: 1 };
  const rimX = new Float32Array(BLOB), rimY = new Float32Array(BLOB);

  function drawNode(n, time) {
    const g = easeOut(n.grow);
    const pr = (n.r * n.s * (1 + 0.05 * Math.sin(time * 1.3 + n.i)) + n.flash * 4 * n.s) * g;
    if (pr < 0.3) return;
    const alpha = n.depth * Math.max(0.14, n.lit) * g;
    const sprite = sprites[n.sprite];
    ctx.globalCompositeOperation = theme.dark ? "lighter" : "multiply";
    ctx.globalAlpha = Math.min(1, alpha * ((n.kind === "model" ? 0.55 : 0.8) + n.flash * 0.5 + Math.min(1, n.act + n.live) * 0.3));
    const R = pr * 4.2;
    if (sprite) ctx.drawImage(sprite, n.x - R, n.y - R, R * 2, R * 2);
    ctx.globalCompositeOperation = "source-over";
    ctx.globalAlpha = alpha;
    if (pr > 2.5) {
      ctx.strokeStyle = n.dend;
      ctx.lineWidth = Math.max(0.6, 1.2 * n.s);
      ctx.beginPath();
      for (const d of n.dendrites) {
        const ang = d.ang + 0.18 * Math.sin(time * 0.9 + d.ph), len = pr * d.len;
        const ex = n.x + Math.cos(ang) * len, ey = n.y + Math.sin(ang) * len;
        ctx.moveTo(n.x, n.y);
        ctx.quadraticCurveTo(n.x + Math.cos(ang + 0.35) * len * 0.55, n.y + Math.sin(ang + 0.35) * len * 0.55, ex, ey);
        ctx.moveTo(ex + Math.cos(ang - 0.6) * len * 0.22, ey + Math.sin(ang - 0.6) * len * 0.22);
        ctx.lineTo(ex, ey);
        ctx.lineTo(ex + Math.cos(ang + 0.6) * len * 0.22, ey + Math.sin(ang + 0.6) * len * 0.22);
      }
      ctx.stroke();
    }
    // The membrane: a closed curve through midpoints, so the wobble has no corners.
    ctx.fillStyle = n.body;
    for (let k = 0; k < BLOB; k++) {
      const a = (k / BLOB) * TAU;
      const rr = pr * (1 + 0.12 * Math.sin(a * 3 + n.seed + time * 0.8) + 0.05 * Math.sin(a * 5 - n.seed - time * 0.5));
      rimX[k] = n.x + Math.cos(a) * rr;
      rimY[k] = n.y + Math.sin(a) * rr;
    }
    ctx.beginPath();
    ctx.moveTo((rimX[BLOB - 1] + rimX[0]) / 2, (rimY[BLOB - 1] + rimY[0]) / 2);
    for (let k = 0; k < BLOB; k++) {
      const j = (k + 1) % BLOB;
      ctx.quadraticCurveTo(rimX[k], rimY[k], (rimX[k] + rimX[j]) / 2, (rimY[k] + rimY[j]) / 2);
    }
    ctx.closePath();
    ctx.fill();
    ctx.strokeStyle = n.rim;
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.fillStyle = n.core;
    ctx.beginPath();
    ctx.arc(n.x - pr * 0.12, n.y - pr * 0.12, pr * 0.38 * (1 + 0.12 * Math.sin(time * 1.9 + n.i)), 0, TAU);
    ctx.fill();
  }

  function step(dt) {
    if (drag) {
      yaw = approach(yaw, yawTo, 22, dt);
      pitch = approach(pitch, pitchTo, 22, dt);
    } else {
      spin = approach(spin, REST_SPIN, 1.2, dt);
      yaw += spin * dt;
      pitch = approach(pitch, pitchTo, 1.5, dt);
    }
    const focus = hover >= 0 ? nodes[hover] : null;
    const focusParent = focus ? byId.get(focus.parent) : null;
    for (const n of nodes) {
      for (let k = 0; k < 3; k++) n.p[k] = approach(n.p[k], n.home[k], 3.2, dt);
      n.r = approach(n.r, n.rTo, 2, dt);
      n.grow = Math.min(1, n.grow + dt / 0.9);
      n.flash *= Math.exp(-dt * 3.5);
      n.act *= Math.exp(-dt * 0.25);
      const near = !focus || n === focus || n === focusParent || n.parent === focus.id || n.kind === "core";
      n.lit = approach(n.lit, near ? 1 : 0.18, 9, dt);
    }
    for (const e of edges) e.fire *= Math.exp(-dt * 4);
    for (const r of ripples) if (r.t < 1) r.t = Math.min(1, r.t + dt / 1.3);
    // Ambient particles arrive at a steady rate rather than in refills.
    let alive = 0;
    for (const p of particles) {
      if (p.edge < 0) continue;
      alive++;
      p.age += dt;
      if (p.age < p.dur) continue;
      const e = edges[p.edge];
      if (e) {
        const b = nodes[e.b];
        b.flash = Math.max(b.flash, p.pulse ? 1 : 0.35);
        e.fire = Math.max(e.fire, p.pulse ? 1 : 0.5);
        if (p.pulse && p.next < 0) ripple(e.b, p.caption);
      }
      if (p.next >= 0) Object.assign(p, { edge: p.next, next: -1, age: 0 });
      else p.edge = -1;
    }
    // As many in flight as the core is live: none at all when nothing ran lately.
    const target = Math.round(AMBIENT * Math.min(1, nodes.length ? nodes[0].live + nodes[0].act : 0));
    if (edges.length && alive < target) {
      spawnDebt += dt * (target / 2.9);
      while (spawnDebt >= 1) {
        spawnDebt -= 1;
        launch(pickEdge(), -1, false);
      }
    }
  }

  function draw(time) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (!nodes.length) return;
    const focus = hover >= 0 ? nodes[hover] : null;
    for (const n of nodes) {
      project(n.p, n);
      n.depth = 0.5 + 0.5 * Math.min(1, Math.max(0, 1 - (n.z + OUTER) / (2 * OUTER)));
    }
    ctx.fillStyle = theme.dark ? "#cfe8dc" : "#17251d";
    for (const s of stars) {
      project(s.p, s);
      ctx.globalAlpha = s.a * (theme.dark ? 0.45 : 0.14) * s.s * (0.75 + 0.25 * Math.sin(time * 0.7 + s.tw));
      ctx.fillRect(s.x, s.y, 1.3 * s.s, 1.3 * s.s);
    }
    ctx.lineCap = "round";
    for (const e of edges) {
      const a = nodes[e.a], b = nodes[e.b];
      ctx.globalAlpha = Math.min(a.lit, b.lit) * b.depth * easeOut(b.grow) * ((theme.dark ? 0.16 : 0.2) + 0.5 * e.fire);
      ctx.strokeStyle = b.dend;
      ctx.lineWidth = 0.8 + 1.4 * e.fire;
      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.quadraticCurveTo((a.x + b.x) / 2 - (b.y - a.y) * e.bend, (a.y + b.y) / 2 + (b.x - a.x) * e.bend, b.x, b.y);
      ctx.stroke();
    }
    const spark = sprites.core;
    ctx.globalCompositeOperation = theme.dark ? "lighter" : "source-over";
    for (const p of particles) {
      const e = edges[p.edge];
      if (!e) continue;
      const a = nodes[e.a], b = nodes[e.b];
      // Ambient drift eases in and out; a pulse accelerates out of the core and settles
      // into the model, so its two legs read as one motion.
      const u = Math.min(1, p.age / p.dur);
      const t = !p.pulse ? easeInOut(u) : p.next >= 0 ? easeIn(u) * 0.6 + u * 0.4 : easeOut(u);
      const fade = p.pulse ? 1 : Math.min(1, u * 6, (1 - u) * 6);
      const lit = Math.min(a.lit, b.lit) * b.depth * fade;
      for (let k = TRAIL; k >= 0; k--) {
        const tk = t - k * (p.pulse ? 0.035 : 0.022);
        if (tk < 0) continue;
        along(e, tk, spot);
        const tail = 1 - k / (TRAIL + 1);
        ctx.globalAlpha = lit * tail * tail * (theme.dark ? 0.9 : 0.7);
        if (k === 0 && spark) {
          const R = (p.pulse ? 10 : 6) * spot.s;
          ctx.drawImage(spark, spot.x - R, spot.y - R, R * 2, R * 2);
        }
        ctx.fillStyle = k ? b.dend : b.core;
        ctx.beginPath();
        ctx.arc(spot.x, spot.y, (k ? 1.1 : 1.7) * spot.s * (p.pulse ? 1.3 : 1), 0, TAU);
        ctx.fill();
      }
    }
    ctx.globalCompositeOperation = "source-over";
    for (const r of ripples) {
      if (r.t >= 1) continue;
      const n = nodes[r.node];
      if (!n) continue;
      const t = easeOut(r.t);
      ctx.globalAlpha = (1 - r.t) ** 2 * 0.7 * n.depth;
      ctx.strokeStyle = n.rim;
      ctx.lineWidth = 1.6 * (1 - t * 0.6);
      ctx.beginPath();
      ctx.arc(n.x, n.y, n.r * n.s * (1.2 + t * 3.2), 0, TAU);
      ctx.stroke();
    }
    order.sort(byDepth);
    for (const n of order) drawNode(n, time);
    labels.draw(nodes, { focus, label, font: theme.font });
    labels.captions(ripples, nodes, theme.font);
    ctx.globalAlpha = 1;
    if (pointer && !drag) pick();
  }

  function pick() {
    let best = -1, bestZ = Infinity;
    for (const n of nodes) {
      const d = Math.hypot(n.x - pointer.x, n.y - pointer.y);
      if (d <= n.r * n.s + 8 && n.z < bestZ) {
        best = n.i;
        bestZ = n.z;
      }
    }
    if (best !== hover) {
      hover = best;
      emit({ type: "hover", id: best >= 0 ? nodes[best].id : null });
    }
  }

  function settle() {
    for (const n of nodes) {
      n.p = n.home.slice();
      n.r = n.rTo;
      n.grow = 1;
    }
  }

  function loop(now) {
    if (!visible || still) {
      running = false;
      return;
    }
    // A long stall (a background tab, a busy page) resumes where it was, not ahead.
    const dt = lastT ? Math.min(0.05, (now - lastT) / 1000) : 1 / 60;
    lastT = now;
    clock += dt;
    step(dt);
    draw(clock);
    frame(loop);
  }

  function wake() {
    if (still) {
      settle();
      draw(0);
      return;
    }
    if (!running && visible) {
      running = true;
      lastT = 0;
      frame(loop);
    }
  }

  return {
    handle(msg) {
      switch (msg.type) {
        case "size":
          ({ w, h, dpr } = msg);
          base = Math.min(w, h) || 360;
          canvas.width = Math.round(w * dpr);
          canvas.height = Math.round(h * dpr);
          if (running) draw(clock); // No blank frame between the resize and the next tick.
          break;
        case "theme":
          theme = msg.theme;
          sprites = {};
          for (const n of nodes) paint(n);
          break;
        case "data": {
          label = msg.label;
          if (msg.shape) layout(msg);
          else {
            const peak = peakCost(msg.nodes);
            for (const d of msg.nodes) {
              const n = byId.get(d.id);
              if (n) Object.assign(n, { cost: d.cost, live: d.live || 0, rTo: radius({ kind: n.kind, cost: d.cost }, peak) });
            }
          }
          break;
        }
        case "turn": {
          const m = byId.get(msg.model);
          const hub = m && byId.get(m.parent);
          if (!m || !hub) break;
          m.act = Math.min(1, m.act + 0.6);
          hub.act = Math.min(1, hub.act + 0.4);
          nodes[0].act = Math.min(1, nodes[0].act + 0.3);
          const first = edges.findIndex((e) => e.b === hub.i && nodes[e.a].kind === "core");
          const second = edges.findIndex((e) => e.b === m.i);
          if (still) ripple(m.i, msg.caption || "");
          else launch(first >= 0 ? first : second, first >= 0 ? second : -1, true, msg.caption || "");
          break;
        }
        case "pointer": {
          const now = performance.now();
          pointer = msg.x === null ? null : { x: msg.x, y: msg.y };
          if (msg.down) drag = { x: msg.x, y: msg.y, yaw: yawTo = yaw, pitch: pitchTo = pitch, at: now, last: yaw };
          else if (msg.up && drag) {
            // Let go, and the sphere keeps the speed it was thrown with.
            const span = Math.max(16, now - drag.at) / 1000;
            spin = Math.max(-3, Math.min(3, (yawTo - drag.last) / span));
            drag = null;
          } else if (drag && pointer) {
            drag.last = yawTo;
            drag.at = now;
            yawTo = drag.yaw + (msg.x - drag.x) * 0.008;
            pitchTo = Math.max(-1.1, Math.min(1.1, drag.pitch + (msg.y - drag.y) * 0.006));
          }
          if (!pointer && hover >= 0) {
            hover = -1;
            emit({ type: "hover", id: null });
          }
          if (still && pointer) pick();
          break;
        }
        case "visible":
          visible = msg.visible;
          break;
        case "still":
          still = msg.still;
          break;
      }
      wake();
    },
  };
}

const byDepth = (a, b) => b.z - a.z;
