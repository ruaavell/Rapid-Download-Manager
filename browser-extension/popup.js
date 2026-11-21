const HOST = "io.github.ruaavell.rdm";

function setIcon(ok) {
  const sizes = [16,32,48,128];
  const icons = {};
  for (const s of sizes) icons[s] = ok ? `icons/icon${s}.png` : `icons/icon-error${s}.png`;
  chrome.action.setIcon({ path: icons });
}

function checkConnection(cb) {
  chrome.runtime.sendNativeMessage(HOST, { ping: true }, (resp) => {
    const ok = !chrome.runtime.lastError && !!resp?.ok;
    cb(ok, chrome.runtime.lastError?.message || resp?.message || "");
  });
}

const statusEl = document.getElementById("status");
const enabledEl = document.getElementById("enabled");

chrome.storage.local.get(["enabled"], (res) => {
  enabledEl.checked = res.enabled !== false;
});

enabledEl.addEventListener("change", () => {
  chrome.storage.local.set({ enabled: enabledEl.checked });
});

document.getElementById("check").addEventListener("click", () => {
  checkConnection((ok, msg) => {
    statusEl.textContent = ok ? "Working" : "Broken: " + (msg || "no connection");
    statusEl.className = "status " + (ok ? "ok" : "bad");
    setIcon(ok);
  });
});

document.getElementById("github").addEventListener("click", () => {
  window.open("https://github.com/ruaavell/Rapid-Download-Manager", "_blank");
});

checkConnection((ok, msg) => {
  statusEl.textContent = ok ? "Working" : "Broken: " + (msg || "no connection");
  statusEl.className = "status " + (ok ? "ok" : "bad");
  setIcon(ok);
});