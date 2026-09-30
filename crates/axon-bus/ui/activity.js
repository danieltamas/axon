// A session's or project's last 24 hours from Axon's usage record: turns per clock hour,
// the current hour last, with the lines it changed and the skills it used. Needs no
// content capture — it is counts only.

import { el, svg } from "./dom.js";

const BAR = 8;
const GAP = 3;
const HEIGHT = 36;
const COMPACT_HEIGHT = 20;

// Hours summed across sessions, for a project.
export function sumHours(activities) {
  const hours = new Array(24).fill(0);
  for (const a of activities) if (a) a.hours.forEach((n, i) => (hours[i] += n));
  return hours;
}

// `caption` labels the axis; a card's compact chart has neither.
export function activityChart(hours, caption, compact = false) {
  const height = compact ? COMPACT_HEIGHT : HEIGHT;
  const peak = Math.max(...hours);
  const figure = el("figure", compact ? "activity compact" : "activity");
  const width = hours.length * (BAR + GAP) - GAP;
  const chart = svg("svg", { viewBox: `0 0 ${width} ${height}`, width, height, role: "img" });
  const hoursAgo = (i) => (i === hours.length - 1 ? "this hour" : `${hours.length - 1 - i}h ago`);
  const total = hours.reduce((a, b) => a + b, 0);
  chart.setAttribute("aria-label", peak ? `${total} turns in 24 hours, busiest ${hoursAgo(hours.indexOf(peak))}` : "No turns in 24 hours");
  hours.forEach((n, i) => {
    // An idle hour keeps a 1px tick, so the baseline reads as time, not missing data.
    const h = n ? Math.max(3, Math.round((n / peak) * height)) : 1;
    const bar = svg("rect", { x: i * (BAR + GAP), y: height - h, width: BAR, height: h, rx: n ? 1.5 : 0, class: n ? "hour" : "hour idle" });
    const title = svg("title");
    title.textContent = `${n} ${n === 1 ? "turn" : "turns"}, ${hoursAgo(i)}`;
    bar.append(title);
    chart.append(bar);
  });
  figure.append(chart);
  if (compact) return figure;
  const axis = el("figcaption", "activity-axis");
  axis.append(el("span", null, "24h ago"), el("span", "activity-caption", caption), el("span", null, "now"));
  figure.append(axis);
  return figure;
}

// Turns and changed lines on one line; skills as chips beneath.
export function activityFacts(activity) {
  const box = el("div", "activity-facts");
  const parts = [`${activity.turns} ${activity.turns === 1 ? "turn" : "turns"}`];
  if (activity.lines_added || activity.lines_removed) parts.push(`+${activity.lines_added} −${activity.lines_removed} lines`);
  box.append(el("p", "activity-line", parts.join(" · ")));
  if (activity.skills.length) {
    const skills = el("ul", "skills");
    skills.setAttribute("aria-label", "Skills used");
    for (const s of activity.skills) {
      const chip = el("li", "skill", s.name);
      if (s.count > 1) chip.append(el("b", null, `×${s.count}`));
      skills.append(chip);
    }
    box.append(skills);
  }
  return box;
}
