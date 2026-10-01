// Markdown as agents write it, built as DOM nodes: agent text never becomes markup, so a
// message cannot inject HTML. Covers what agents use: fenced code, headings, lists,
// quotes, tables, inline code, bold and links. Emphasis stays plain text.

import { el, jsonFields } from "./dom.js";

const FENCE = /^\s*(```|~~~)/;
const HEADING = /^(#{1,6})\s+(.*)$/;
const ITEM = /^\s*([-*+]|\d+[.)])\s+(.*)$/;
const QUOTE = /^\s*>\s?(.*)$/;
const ROW = /^\s*\|.*\|\s*$/;
const RULE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;
const INLINE = /(`+)([^`]|[^`][\s\S]*?[^`])\1|\*\*([^*]+)\*\*|__([^_]+)__|\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g;

// Agent text: a JSON object as the list of its fields, anything else as markdown.
export function prose(text, className = null) {
  const fields = jsonFields(text);
  if (!fields) return markdown(text, className);
  const list = el("dl", className ? `${className} fields` : "fields");
  for (const [label, value] of fields) list.append(el("dt", null, label), el("dd", null, value));
  return list;
}

export function markdown(text, className = null) {
  const root = el("div", className ? `${className} md` : "md");
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const fence = line.match(FENCE);
    if (fence) {
      const body = [];
      i += 1;
      while (i < lines.length && !lines[i].trimStart().startsWith(fence[1])) body.push(lines[i++]);
      i += 1;
      const pre = el("pre");
      pre.append(el("code", null, body.join("\n")));
      root.append(pre);
      continue;
    }
    const heading = line.match(HEADING);
    if (heading) {
      root.append(inline(el("p", `md-h md-h${heading[1].length}`), heading[2]));
      i += 1;
      continue;
    }
    if (ROW.test(line) && i + 1 < lines.length && RULE.test(lines[i + 1])) {
      const rows = [line];
      i += 2;
      while (i < lines.length && ROW.test(lines[i])) rows.push(lines[i++]);
      root.append(table(rows));
      continue;
    }
    if (ITEM.test(line)) {
      const ordered = /^\s*\d/.test(line);
      const list = el(ordered ? "ol" : "ul");
      while (i < lines.length && ITEM.test(lines[i])) {
        const item = inline(el("li"), lines[i].match(ITEM)[2]);
        i += 1;
        // An indented line under an item continues it.
        while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !ITEM.test(lines[i])) {
          item.append("\n");
          inline(item, lines[i++].trim());
        }
        list.append(item);
      }
      root.append(list);
      continue;
    }
    if (QUOTE.test(line)) {
      const quoted = [];
      while (i < lines.length && QUOTE.test(lines[i])) quoted.push(lines[i++].match(QUOTE)[1]);
      root.append(inline(el("blockquote"), quoted.join("\n")));
      continue;
    }
    const paragraph = [];
    while (i < lines.length && lines[i].trim() && !startsBlock(lines, i)) paragraph.push(lines[i++]);
    root.append(inline(el("p"), paragraph.join("\n")));
  }
  return root;
}

function startsBlock(lines, i) {
  const line = lines[i];
  return FENCE.test(line) || HEADING.test(line) || ITEM.test(line) || QUOTE.test(line) || (ROW.test(line) && RULE.test(lines[i + 1] || ""));
}

function table(rows) {
  const cells = (row) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  const box = el("div", "md-table");
  const grid = el("table");
  const head = el("tr");
  for (const cell of cells(rows[0])) head.append(inline(el("th"), cell));
  grid.append(head);
  for (const row of rows.slice(1)) {
    const line = el("tr");
    for (const cell of cells(row)) line.append(inline(el("td"), cell));
    grid.append(line);
  }
  box.append(grid);
  return box;
}

// Appends `text` to `node` with its inline code, bold and links.
function inline(node, text) {
  let at = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > at) node.append(text.slice(at, m.index));
    if (m[2] !== undefined) node.append(el("code", null, m[2]));
    else if (m[3] !== undefined || m[4] !== undefined) node.append(el("strong", null, m[3] ?? m[4]));
    else {
      const link = el("a", null, m[5]);
      link.href = m[6];
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      node.append(link);
    }
    at = m.index + m[0].length;
  }
  if (at < text.length) node.append(text.slice(at));
  return node;
}
