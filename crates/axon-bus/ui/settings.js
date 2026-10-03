// Settings: every knob the dashboard owns, read from /api/settings and written back one
// section at a time. A write returns the full new settings, which every section adopts, so
// the page only ever shows what the server holds. Sections live in settings-*.js.

import { el } from "./dom.js";
import { federationSection } from "./settings-fed.js";
import { reason, request } from "./settings-kit.js";
import { agentsSection, budgetsSection, captureSection, usageSection } from "./settings-sections.js";
import { hooksSection, sessionsSection, storageSection } from "./settings-system.js";

export function createSettings(container) {
  const title = el("h1", null, "Settings");
  const index = el("nav", "set-index");
  index.setAttribute("aria-label", "Settings sections");
  const head = el("header", "set-head");
  head.append(title, index);
  const notice = el("div", "set-load");
  notice.setAttribute("role", "status");
  const stack = el("div", "set-stack");
  container.append(head, notice, stack);

  let sections = [];

  function apply(settings) {
    for (const section of sections) section.sync(settings);
  }

  function build() {
    const api = { apply };
    sections = [captureSection(api), usageSection(api), budgetsSection(api), hooksSection(api), storageSection(api), sessionsSection(api), agentsSection(api), federationSection(api)];
    stack.replaceChildren(...sections.map((section) => section.root));
    index.replaceChildren(
      ...sections.map((section) => {
        const heading = section.root.querySelector("h2");
        const jump = el("button", null, heading.textContent);
        jump.type = "button";
        jump.addEventListener("click", () => {
          const calm = matchMedia("(prefers-reduced-motion: reduce)").matches;
          section.root.scrollIntoView({ behavior: calm ? "auto" : "smooth", block: "start" });
          heading.tabIndex = -1;
          heading.focus({ preventScroll: true });
        });
        return jump;
      }),
    );
  }

  function say(text, retry) {
    notice.replaceChildren();
    notice.hidden = !text;
    if (!text) return;
    notice.append(el("p", null, text));
    if (retry) {
      const again = el("button", "btn", "Try again");
      again.type = "button";
      again.addEventListener("click", load);
      notice.append(again);
    }
  }

  async function load() {
    if (!sections.length) say("Loading settings");
    const res = await request("GET", "/api/settings");
    if (!res.ok) {
      if (!sections.length) say(`Settings could not be loaded. ${reason(res)}`, true);
      return;
    }
    if (!sections.length) build();
    say("");
    apply(res.data);
  }

  return { show: load };
}
