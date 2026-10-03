// Federation: the on/off switch and the relay. Pairing, peers and shared projects live in
// the Connections view (connections.js).

import { el } from "./dom.js";
import { band, field, reason, request, settingsForm, switchRow, textInput } from "./settings-kit.js";

export function federationSection({ apply }) {
  const { root, body } = band("federation", "Federation", "Connect this machine to others over an encrypted link. Changing either setting restarts the link.");
  const toggle = switchRow({
    label: "Federation",
    hint: "When off, no connection is made or accepted.",
    async: true,
    onToggle: async (on) => {
      toggle.lock(true);
      toggle.say("");
      const res = await request("PUT", "/api/settings/federation", { enabled: on });
      toggle.lock(false);
      if (res.ok) apply(res.data);
      else toggle.say(res.status === 400 ? "The server rejected this change." : reason(res));
    },
  });
  const relay = field({
    name: "relay",
    label: "Relay",
    hint: "default, or the https:// address of your own relay.",
    invalid: "Enter default, or an address that starts with https://.",
    input: textInput("default"),
  });
  const form = settingsForm({
    fields: [relay],
    children: [relay.root],
    send: () => {
      const value = relay.input.value.trim();
      return request("PUT", "/api/settings/federation", { relay: value === "" ? "default" : value });
    },
    applied: apply,
  });
  const where = el("p", "set-note");
  const go = el("a", null, "Connections");
  go.href = "#/connections";
  where.append("Pair machines, share projects and watch each link in ", go, ".");
  body.append(toggle.root, form.root, where);
  return {
    root,
    sync({ federation }) {
      toggle.set(federation.enabled);
      if (form.isEditing()) return;
      relay.input.value = federation.relay;
    },
  };
}
