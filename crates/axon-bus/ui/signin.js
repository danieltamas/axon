// Owner sign-in (P2P-SPEC §1). `axon` and `axon open` print a link ending in `#login=<nonce>`;
// the page trades the nonce for a session cookie, which the browser then sends by itself.
// With no session, the dashboard is replaced by a panel that says how to get a link.

import { el } from "./dom.js";

export async function exchangeLogin() {
  const nonce = new URLSearchParams(location.hash.slice(1)).get("login");
  if (!nonce) return;
  // The nonce is single-use, so the fragment goes whether or not the exchange worked.
  history.replaceState(null, "", location.pathname + location.search);
  await fetch("/api/session", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ nonce }),
  }).catch(() => null);
}

export function showSignIn() {
  if (document.getElementById("signin")) return;
  const command = el("div", "off-command");
  command.append(el("code", null, "axon open"));
  const panel = el("section", "off signin");
  panel.id = "signin";
  panel.append(
    el("h1", "off-title", "Sign in"),
    el("p", "off-lede", "Run `axon open` in a terminal."),
    command,
  );
  document.body.dataset.auth = "signin";
  document.body.append(panel);
}
