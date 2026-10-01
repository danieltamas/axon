// Axon as an installed app: the service worker that keeps the offline page, the banner
// that offers installation, and the hand-off to the offline page when the server stops.
import { el } from "./dom.js";

const DISMISSED = "axon-install-dismissed";
const installed = () => matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;

export function registerWorker() {
  // Workers need a secure context; 127.0.0.1 is one, a LAN address is not.
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("/sw.js").catch(() => {});
}

/// Called when the live stream drops. If the server is really gone and the worker can
/// stand in, reload so the worker shows how to start it again.
export function onServerLost() {
  if (!navigator.serviceWorker?.controller) return;
  setTimeout(async () => {
    try {
      await fetch(`/manifest.webmanifest?probe=${Date.now()}`, { cache: "no-store" });
    } catch {
      location.reload();
    }
  }, 1500);
}

function remember() {
  try {
    localStorage.setItem(DISMISSED, "1");
  } catch {
    // Without storage the banner returns on the next visit, which is harmless.
  }
}

function dismissed() {
  try {
    return localStorage.getItem(DISMISSED) === "1";
  } catch {
    return false;
  }
}

// Safari installs from its menu and fires no install event; Chromium browsers do.
const safari = /^((?!chrome|chromium|crios|edg|android).)*safari/i.test(navigator.userAgent);

function banner(action) {
  const card = el("aside", "install");
  card.setAttribute("aria-label", "Install Axon");
  const mark = document.querySelector(".brand-mark").cloneNode(true);
  mark.setAttribute("class", "install-mark");
  const copy = el("div", "install-copy");
  copy.append(el("strong", "", "Install Axon as an app"), el("p", "", "Its own window and Dock icon. When axon is not running, it shows how to start it."));
  const later = el("button", "install-later", "Not now");
  later.type = "button";
  const close = () => {
    remember();
    card.dataset.state = "leaving";
    setTimeout(() => card.remove(), 300);
  };
  later.addEventListener("click", close);
  const actions = el("div", "install-actions");
  actions.append(action(close), later);
  card.append(mark, copy, actions);
  document.body.append(card);
  requestAnimationFrame(() => requestAnimationFrame(() => { card.dataset.state = "shown"; }));
}

// An installed app is bound to the port it came from; only offer it where `axon` serves
// by default, so a one-off `--port` run never becomes the app that waits on a dead port.
const DEFAULT_PORT = "7777";

export function offerInstall() {
  if (installed() || dismissed() || location.port !== DEFAULT_PORT) return;
  if (safari) {
    banner(() => el("span", "install-hint", "File, then Add to Dock"));
    return;
  }
  addEventListener("beforeinstallprompt", (event) => {
    event.preventDefault();
    banner((close) => {
      const button = el("button", "install-go", "Install");
      button.type = "button";
      button.addEventListener("click", async () => {
        event.prompt();
        await event.userChoice;
        close();
      });
      return button;
    });
  }, { once: true });
}
