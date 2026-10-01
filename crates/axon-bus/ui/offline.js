// The page the service worker shows when nothing answers on this port: how to start
// axon, and a quiet probe that brings the dashboard back once it does.
const INSTALL = {
  windows: 'powershell -ExecutionPolicy Bypass -c "irm https://github.com/danieltamas/axon/releases/latest/download/axon-installer.ps1 | iex"',
  unix: "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/danieltamas/axon/releases/latest/download/axon-installer.sh | sh",
};
const platform = navigator.userAgentData?.platform || navigator.platform || "";
document.getElementById("install").textContent = /win/i.test(platform) ? INSTALL.windows : INSTALL.unix;

for (const button of document.querySelectorAll("[data-copy]")) {
  const label = button.querySelector("span");
  button.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(document.getElementById(button.dataset.copy).textContent);
      label.textContent = "Copied";
    } catch {
      label.textContent = "Select and copy";
    }
    setTimeout(() => { label.textContent = "Copy"; }, 1600);
  });
}

const status = document.getElementById("status");
const port = location.port || (location.protocol === "https:" ? "443" : "80");
status.textContent = `Waiting for axon on port ${port}`;

// The manifest is not in the worker's cache, so this reaches the network or fails.
async function answering() {
  try {
    return (await fetch(`/manifest.webmanifest?probe=${Date.now()}`, { cache: "no-store" })).ok;
  } catch {
    return false;
  }
}

let timer = 0;
async function probe() {
  clearTimeout(timer);
  if (await answering()) {
    document.body.dataset.state = "back";
    document.getElementById("title").textContent = "Axon is running";
    document.getElementById("lede").textContent = "Opening the dashboard.";
    status.textContent = "Connected";
    const settle = matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : 700;
    setTimeout(() => location.reload(), settle);
    return;
  }
  timer = setTimeout(probe, document.hidden ? 10000 : 2000);
}
document.addEventListener("visibilitychange", () => { if (!document.hidden) probe(); });
addEventListener("online", probe);
probe();
