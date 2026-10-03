// Settings that live in the hub's settings table and the config file: what is captured,
// how long usage is kept, the spend caps, and how sessions link. Each section is
// `{ root, sync(settings) }`.

import { el } from "./dom.js";
import { band, field, numberInput, reason, request, settingsForm, switchRow } from "./settings-kit.js";

export function captureSection({ apply }) {
  const { root, body } = band("capture", "Capture", "What Axon keeps of what agents say and do, and for how long.");
  const toggle = switchRow({
    label: "Record message and tool content",
    hint: "Off keeps only counts and timings.",
    onToggle: (on) => {
      toggle.set(on);
      toggle.input.dispatchEvent(new Event("input", { bubbles: true }));
    },
  });
  const days = field({
    name: "narrative_days",
    label: "Keep narrative for",
    hint: "Days, 1 to 365.",
    invalid: "Enter a whole number of days from 1 to 365.",
    input: numberInput({ min: 1, max: 365 }),
    unit: "days",
  });
  const forced = el("p", "set-note");
  forced.hidden = true;
  const form = settingsForm({
    fields: [days],
    children: [forced, toggle.root, days.root],
    send: () => request("PUT", "/api/settings/capture", { enabled: toggle.input.checked, narrative_days: Number(days.input.value) }),
    applied: apply,
  });
  body.append(form.root);
  return {
    root,
    sync({ capture }) {
      forced.hidden = !capture.forced_off;
      forced.textContent = "This server was started with --no-content, so capture stays off until it is restarted without it.";
      toggle.lock(capture.forced_off);
      if (form.isEditing()) return;
      toggle.set(capture.enabled && !capture.forced_off);
      days.input.value = capture.narrative_days;
    },
  };
}

export function usageSection({ apply }) {
  const { root, body } = band("usage", "Usage", "How long the spend record is kept. Older rows are deleted by the poller.");
  const group = el("fieldset", "choice-set");
  group.append(el("legend", "fld-label", "Retention"));
  const options = el("div", "choice");
  const radios = {};
  for (const [value, label] of [["forever", "Keep everything"], ["days", "Delete after"]]) {
    const option = el("label", "choice-option");
    const input = el("input");
    input.type = "radio";
    input.name = "usage-retention";
    input.value = value;
    radios[value] = input;
    option.append(input, el("span", null, label));
    options.append(option);
  }
  group.append(options);
  const days = field({
    name: "retention_days",
    label: "Days to keep",
    hint: "1 to 3650.",
    invalid: "Enter a whole number of days from 1 to 3650.",
    input: numberInput({ min: 1, max: 3650 }),
    unit: "days",
  });
  const reveal = () => {
    days.root.hidden = !radios.days.checked;
  };
  group.addEventListener("change", reveal);
  const form = settingsForm({
    fields: [days],
    children: [group, days.root],
    send: () => request("PUT", "/api/settings/usage", { retention_days: radios.days.checked ? Number(days.input.value) : null }),
    applied: apply,
  });
  body.append(form.root);
  return {
    root,
    sync({ usage }) {
      if (form.isEditing()) return;
      const kept = usage.retention_days;
      radios[kept === null ? "forever" : "days"].checked = true;
      days.input.value = kept === null ? "" : kept;
      reveal();
    },
  };
}

const CAPS = [
  ["eur_per_day", "Per day"],
  ["eur_per_week", "Per week"],
  ["eur_per_month", "Per month"],
];

export function budgetsSection({ apply }) {
  const { root, body } = band("budgets", "Budgets", "Spend caps in euros, kept in the config file the CLI reads. Leave a cap empty for no limit.");
  const caps = CAPS.map(([name, label]) =>
    field({
      name,
      label,
      invalid: "Enter an amount of zero or more, or leave it empty.",
      input: numberInput({ min: 0, step: "0.01", placeholder: "No limit" }),
      unit: "EUR",
    }),
  );
  const grid = el("div", "fld-grid");
  grid.append(...caps.map((cap) => cap.root));
  const form = settingsForm({
    fields: caps,
    children: [grid],
    send: () => {
      const values = {};
      for (const cap of caps) values[cap.name] = cap.input.value === "" ? null : Number(cap.input.value);
      return request("PUT", "/api/settings/budgets", values);
    },
    applied: apply,
  });
  body.append(form.root);
  return {
    root,
    sync({ budgets }) {
      if (form.isEditing()) return;
      for (const cap of caps) cap.input.value = budgets[cap.name] === null || budgets[cap.name] === undefined ? "" : budgets[cap.name];
    },
  };
}

export function agentsSection({ apply }) {
  const { root, body } = band("agents", "Agents", "How the sessions on this machine reach each other.");
  const toggle = switchRow({
    label: "Link sessions in the same repository",
    hint: "Sessions working in one repository, worktrees included, can message each other without asking. Off, one proposes a link and the other accepts.",
    async: true,
    onToggle: async (on) => {
      toggle.lock(true);
      toggle.say("");
      const res = await request("PUT", "/api/settings/bus", { auto_link: on });
      toggle.lock(false);
      if (res.ok) apply(res.data);
      else toggle.say(reason(res));
    },
  });
  body.append(toggle.root);
  return {
    root,
    sync({ bus }) {
      toggle.set(Boolean(bus && bus.auto_link));
    },
  };
}
