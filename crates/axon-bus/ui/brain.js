// The usage brain: all spend at the core, each harness around it, each harness's models
// on the outer shell, sized by cost. Ambient flow follows where the money goes; a turn
// that lands sends a pulse core → harness → model and a ripple on arrival. Drawing runs
// in a worker when the browser can hand it the canvas, so the page never waits on it.

import { createRenderer } from "./brain-render.js";
import { displayMoney, tokens } from "./dom.js";

const HARNESS = { "claude-code": "claude", claude: "claude", codex: "codex", opencode: "opencode", hermes: "hermes" };
const still = matchMedia("(prefers-reduced-motion: reduce)");
const darkScheme = matchMedia("(prefers-color-scheme: dark)");

// Summary rows carry no harness per model; the id's family names it.
export function modelHarness(model) {
  if (model.startsWith("claude-")) return "claude";
  if (model.startsWith("gpt-") || model.startsWith("codex")) return "codex";
  return "opencode";
}

// A worker when the canvas can be transferred; else the same renderer on this thread.
function connect(canvas, onMessage) {
  if (canvas.transferControlToOffscreen && typeof Worker === "function") {
    try {
      const worker = new Worker(new URL("./brain-worker.js", import.meta.url), { type: "module" });
      const offscreen = canvas.transferControlToOffscreen();
      worker.postMessage({ type: "init", canvas: offscreen }, [offscreen]);
      worker.onmessage = ({ data }) => onMessage(data);
      return (msg) => worker.postMessage(msg);
    } catch {
      // Fall through to the page thread.
    }
  }
  const renderer = createRenderer(canvas, onMessage);
  return (msg) => renderer.handle(msg);
}

export function createBrain(canvas, tip) {
  let byId = new Map();
  let total = 0;
  let lastTs = 0;
  let shape = "";
  let pointerAt = { x: 0, y: 0 };
  let onScreen = true;
  const send = connect(canvas, (msg) => msg.type === "hover" && showTip(byId.get(msg.id)));

  function theme() {
    const css = getComputedStyle(document.documentElement);
    const v = (name) => css.getPropertyValue(name).trim();
    const forced = document.documentElement.dataset.theme;
    const colors = { signal: v("--signal"), muted: v("--muted"), claude: v("--h-claude"), codex: v("--h-codex"), opencode: v("--h-opencode"), hermes: v("--h-hermes"), virtual: v("--h-virtual") };
    send({ type: "theme", theme: { dark: forced ? forced === "dark" : darkScheme.matches, colors, font: v("--mono") || "monospace" } });
  }

  function size() {
    send({ type: "size", w: canvas.clientWidth, h: canvas.clientHeight, dpr: Math.min(devicePixelRatio || 1, 2) });
  }

  function placeTip() {
    const x = Math.min(pointerAt.x + 14, canvas.clientWidth - tip.offsetWidth - 4);
    tip.style.transform = `translate(${Math.max(0, x)}px, ${pointerAt.y + 14}px)`;
  }

  function showTip(n) {
    if (!n) {
      tip.hidden = true;
      return;
    }
    const share = total > 0 && n.kind !== "core" ? ` · ${((n.cost / total) * 100).toFixed(1)}% of spend` : "";
    const name = document.createElement("b");
    name.textContent = n.name;
    const cost = document.createElement("span");
    cost.textContent = `${n.unpriced ? "unpriced" : displayMoney(n.cost)}${share}`;
    const more = document.createElement("span");
    more.textContent = `${n.turns.toLocaleString("en-US")} turns · ${tokens(n.out)} tokens out`;
    tip.replaceChildren(name, cost, more);
    tip.hidden = false;
    placeTip();
  }

  const point = (e, extra = {}) => {
    pointerAt = { x: e.offsetX, y: e.offsetY };
    send({ type: "pointer", x: e.offsetX, y: e.offsetY, ...extra });
    if (!tip.hidden) placeTip();
  };
  canvas.addEventListener("pointermove", (e) => point(e));
  canvas.addEventListener("pointerdown", (e) => {
    canvas.setPointerCapture(e.pointerId);
    point(e, { down: true });
  });
  canvas.addEventListener("pointerup", (e) => point(e, { up: true }));
  canvas.addEventListener("pointerleave", () => {
    send({ type: "pointer", x: null, y: null, up: true });
    tip.hidden = true;
  });
  const sendVisible = () => send({ type: "visible", visible: onScreen && !document.hidden });
  new IntersectionObserver(([entry]) => {
    onScreen = entry.isIntersecting;
    sendVisible();
  }).observe(canvas);
  document.addEventListener("visibilitychange", sendVisible);
  new MutationObserver(theme).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  darkScheme.addEventListener("change", theme);
  still.addEventListener("change", () => send({ type: "still", still: still.matches }));
  new ResizeObserver(size).observe(canvas);
  send({ type: "still", still: still.matches });
  size();
  theme();

  return {
    update(summary) {
      const harnesses = summary.by_harness.filter((h) => h.events);
      const models = summary.by_model.filter((m) => m.events);
      const key = (harness) => HARNESS[harness] || "opencode";
      const hubs = new Set(harnesses.map((h) => key(h.harness)));
      const nodes = [
        { id: "core", kind: "core", key: "core", name: "All usage", cost: summary.cost_eur, out: summary.tokens_out, turns: summary.events },
        ...harnesses.map((h) => ({ id: `h:${key(h.harness)}`, kind: "harness", key: key(h.harness), name: h.harness, cost: h.cost_eur, out: h.tokens_out, turns: h.events, parent: "core" })),
        ...models.map((m) => {
          const family = modelHarness(m.model);
          return { id: `m:${m.model}`, kind: "model", key: family, name: m.model, cost: m.cost_eur, out: m.tokens_out, turns: m.events, unpriced: m.unpriced, parent: hubs.has(family) ? `h:${family}` : "core" };
        }),
      ];
      byId = new Map(nodes.map((n) => [n.id, n]));
      total = summary.cost_eur;
      const nextShape = nodes.map((n) => n.id).join("|");
      send({ type: "data", nodes, label: displayMoney(summary.cost_eur), shape: nextShape !== shape });
      shape = nextShape;
      canvas.setAttribute("aria-label", `${harnesses.length} harnesses and ${models.length} models; ${displayMoney(summary.cost_eur)} in all`);
      // Only turns newer than the last update pulse; the first load only sets the mark.
      const recent = summary.recent || [];
      if (lastTs) recent.filter((r) => r.ts > lastTs).slice(0, 8).forEach((r, i) => setTimeout(() => send({ type: "turn", model: `m:${r.model}` }), i * 160));
      lastTs = Math.max(lastTs, ...recent.map((r) => r.ts));
    },
  };
}
