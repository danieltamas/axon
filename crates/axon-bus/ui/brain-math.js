// Pure helpers for the brain renderer: timing, easing, colour, sprites and vectors.

// Workers without requestAnimationFrame get a timer that corrects its own drift.
export const frame =
  typeof requestAnimationFrame === "function"
    ? requestAnimationFrame
    : (() => {
        let next = 0;
        return (f) => {
          const now = performance.now();
          next = Math.max(now, next + 1000 / 60);
          setTimeout(() => f(performance.now()), next - now);
        };
      })();

export const approach = (from, to, rate, dt) => from + (to - from) * (1 - Math.exp(-rate * dt));
export const easeInOut = (t) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);
export const easeOut = (t) => 1 - (1 - t) ** 3;
export const easeIn = (t) => t * t * t;

export function surface(size) {
  if (typeof OffscreenCanvas === "function") return new OffscreenCanvas(size, size);
  const c = document.createElement("canvas");
  c.width = c.height = size;
  return c;
}

export function hsl(hex) {
  const n = parseInt(hex.replace("#", "").padEnd(6, "0").slice(0, 6), 16);
  const r = ((n >> 16) & 255) / 255, g = ((n >> 8) & 255) / 255, b = (n & 255) / 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
  if (!d) return [0, 0, l * 100];
  const s = d / (1 - Math.abs(2 * l - 1));
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [(h * 60 + 360) % 360, s * 100, l * 100];
}

export const color = (h, s, l, a = 1) => `hsla(${Math.round(h)},${Math.round(s)}%,${Math.round(l)}%,${a})`;

// A soft radial glow in one hue, drawn once and then stamped with drawImage.
export function halo(h, s, l, dark) {
  const size = 128, c = surface(size), g = c.getContext("2d");
  const grad = g.createRadialGradient(size / 2, size / 2, 0, size / 2, size / 2, size / 2);
  grad.addColorStop(0, color(h, s, dark ? l + 8 : l, dark ? 0.85 : 0.5));
  grad.addColorStop(0.25, color(h, s, l, dark ? 0.34 : 0.2));
  grad.addColorStop(0.6, color(h, s, l, dark ? 0.08 : 0.05));
  grad.addColorStop(1, color(h, s, l, 0));
  g.fillStyle = grad;
  g.fillRect(0, 0, size, size);
  return c;
}

export function seeded(i) {
  const x = Math.sin(i * 99.7) * 43758.5453;
  return x - Math.floor(x);
}


export function peakCost(list) {
  let peak = 1e-9;
  for (const n of list) if (n.kind !== "core" && n.cost > peak) peak = n.cost;
  return peak;
}

export function cross(a, b) {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
}

export function norm(v) {
  const l = Math.hypot(v[0], v[1], v[2]) || 1;
  return [v[0] / l, v[1] / l, v[2] / l];
}
