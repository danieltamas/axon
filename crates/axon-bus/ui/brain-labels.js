// The brain's text: node labels placed without overlap, and the caption a landing turn shows.
// Labels go in priority order (the core, then hubs, then models by cost, the hovered node
// first of all); one that would overlap a label already placed, or cover a hub, tries above
// its node and otherwise stays hidden, so a small node never overprints a larger one.

import { easeOut } from "./brain-math.js";

const RANK = { core: 0, harness: 1, model: 2 };

export function createLabels(ctx) {
  const placed = [];

  function clear(box, own, nodes) {
    for (const p of placed) if (box.x < p.x + p.w && p.x < box.x + box.w && box.y < p.y + p.h && p.y < box.y + box.h) return false;
    for (const n of nodes) {
      if (n === own || n.kind === "model") continue;
      const cx = Math.max(box.x, Math.min(n.x, box.x + box.w));
      const cy = Math.max(box.y, Math.min(n.y, box.y + box.h));
      if (Math.hypot(n.x - cx, n.y - cy) < n.r * n.s) return false;
    }
    return true;
  }

  return {
    draw(nodes, { focus, label, font }) {
      placed.length = 0;
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      const queue = nodes
        .filter((n) => n.kind !== "model" || n.labelled || n === focus)
        .sort((a, b) => (b === focus) - (a === focus) || RANK[a.kind] - RANK[b.kind] || b.cost - a.cost);
      for (const n of queue) {
        ctx.font = `${n.kind === "model" ? 500 : 600} 11px ${font}`;
        const text = n.kind === "core" ? label : n.kind === "model" ? n.name.replace(/^claude-/, "") : n.name;
        const width = ctx.measureText(text).width;
        const gap = n.r * n.s * 1.6 + 8;
        const spot = [n.y + gap, n.y - gap - 13]
          .map((y) => ({ x: n.x - width / 2 - 2, y: y - 1, w: width + 4, h: 15 }))
          .find((box) => clear(box, n, nodes));
        if (!spot) continue;
        placed.push(spot);
        ctx.globalAlpha = Math.max(0.2, n.lit) * n.depth * easeOut(n.grow);
        ctx.fillStyle = n.text;
        ctx.fillText(text, n.x, spot.y + 1);
      }
    },

    // "repo · model" above a node while its landing ripple lasts.
    captions(ripples, nodes, font) {
      ctx.font = `500 11px ${font}`;
      ctx.textAlign = "center";
      ctx.textBaseline = "bottom";
      for (const r of ripples) {
        if (r.t >= 1 || !r.caption) continue;
        const n = nodes[r.node];
        if (!n) continue;
        ctx.globalAlpha = Math.min(1, (1 - r.t) * 3) * n.depth;
        ctx.fillStyle = n.text;
        ctx.fillText(r.caption, n.x, n.y - n.r * n.s * 1.6 - 6);
      }
    },
  };
}
