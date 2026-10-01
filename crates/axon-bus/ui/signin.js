// Owner sign-in (P2P-SPEC §1). `axon` and `axon open` print a link ending in `#login=<nonce>`;
// the page trades the nonce for a session cookie, which the browser then sends by itself.
// With no session, the dashboard is replaced by a panel that says how to get a link.

import { el } from "./dom.js";

// The cookie alone is not proof of the owner: browsers send it to every localhost port. The
// token travels in a header, and localStorage is scoped to this origin, so another port
// cannot read it.
const TOKEN_KEY = "axon-session";

export function sessionToken() {
  try {
    return localStorage.getItem(TOKEN_KEY) || "";
  } catch {
    return "";
  }
}

function saveToken(token) {
  try {
    localStorage.setItem(TOKEN_KEY, token);
  } catch {
    // Storage blocked: the page works until it is reloaded.
    memoryToken = token;
  }
}

let memoryToken = "";
const currentToken = () => sessionToken() || memoryToken;

// fetch for /api/*: adds the session header on every call.
export function authFetch(path, options = {}) {
  const headers = { ...options.headers, "x-axon-session": currentToken() };
  return fetch(path, { ...options, headers });
}

// EventSource cannot set headers, so the stream takes the token in the query.
export const streamUrl = () => `/api/stream?t=${encodeURIComponent(currentToken())}`;

export const hasSession = () => Boolean(currentToken());

export async function exchangeLogin() {
  const nonce = new URLSearchParams(location.hash.slice(1)).get("login");
  if (!nonce) return;
  // The nonce is single-use, so the fragment goes whether or not the exchange worked.
  history.replaceState(null, "", location.pathname + location.search);
  const response = await fetch("/api/session", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ nonce }),
  }).catch(() => null);
  const body = response && response.ok ? await response.json().catch(() => null) : null;
  if (body && body.token) saveToken(body.token);
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
