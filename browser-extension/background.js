const HOST = "io.github.ruaavell.rdm";

function setIcon(ok) {
  const sizes = [16,32,48,128];
  const icons = {};
  for (const s of sizes) icons[s] = ok ? `icons/icon${s}.png` : `icons/icon-error${s}.png`;
  try { chrome.action.setIcon({ path: icons }); } catch (e) {}
}

function isEnabled(cb) {
  chrome.storage.local.get(["enabled"], (res) => cb(res.enabled !== false));
}

function checkConnection(cb) {
  chrome.runtime.sendNativeMessage(HOST, { ping: true }, (resp) => {
    const ok = !chrome.runtime.lastError && !!resp?.ok;
    cb(ok, chrome.runtime.lastError?.message || resp?.message || "");
  });
}

function openErrorPage() {
  chrome.tabs.create({ url: chrome.runtime.getURL("error.html") });
}

// Cookie header for a URL, so servers that need the browser session (Google
// Drive, login-protected sites) serve the real file instead of a web page.
function getCookieHeader(url, cb) {
  try {
    chrome.cookies.getAll({ url }, (cookies) => {
      if (chrome.runtime.lastError || !cookies || !cookies.length) return cb(null);
      const header = cookies.map((c) => `${c.name}=${c.value}`).join("; ");
      cb(header.length <= 60000 ? header : null);
    });
  } catch (e) {
    cb(null);
  }
}

function sendToRdm(url, referrer, filename, media) {
  isEnabled((enabled) => {
    if (!enabled) return;
    if (!url || !(url.startsWith("http://") || url.startsWith("https://"))) return;
    checkConnection((ok, msg) => {
      if (!ok) {
        setIcon(false);
        openErrorPage();
        return;
      }
      setIcon(true);
      getCookieHeader(url, (cookies) => {
      const message = { action: "add", url, referrer: referrer || null };
      if (filename) message.filename = filename;
      if (cookies) message.cookies = cookies;
      if (media) message.media = true;
      chrome.runtime.sendNativeMessage(HOST, message, (response) => {
        if (chrome.runtime.lastError || !response?.ok) {
          setIcon(false);
          openErrorPage();
        } else {
          chrome.notifications.create({
            type: "basic",
            iconUrl: "icons/icon48.png",
            title: "RDM",
            message: "Download sent to RDM."
          });
        }
      });
      });
    });
  });
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: "rdm-download-link", title: "Download with RDM", contexts: ["link"] });
  chrome.contextMenus.create({ id: "rdm-download-page", title: "Send page URL to RDM", contexts: ["page"] });
  chrome.storage.local.get(["firstRunDone"], (res) => {
    checkConnection((ok) => {
      if (ok) {
        setIcon(true);
        if (!res.firstRunDone) {
          chrome.notifications.create({
            type: "basic",
            iconUrl: "icons/icon48.png",
            title: "RDM",
            message: "Installation successful. RDM is ready."
          });
          chrome.storage.local.set({ firstRunDone: true });
        }
      } else {
        setIcon(false);
        openErrorPage();
      }
    });
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  sendToRdm(info.linkUrl || tab?.url, tab?.url);
});

chrome.runtime.onMessage.addListener((message, sender) => {
  if (message && message.type === "capture") {
    sendToRdm(message.url, message.referrer || sender?.tab?.url, null, !!message.media);
  }
});

function baseName(path) {
  if (!path) return "";
  const parts = String(path).split(/[\\/]/);
  return parts[parts.length - 1] || "";
}

// Hand a browser-initiated download over to RDM. `filename` is the name the
// browser worked out from Content-Disposition; the URL alone often says
// nothing useful (e.g. Google Drive links end in "download").
function captureBrowserDownload(downloadItem, filename) {
  isEnabled((enabled) => {
    if (!enabled) return;
    const url = downloadItem.finalUrl || downloadItem.url;
    if (!url || !(url.startsWith("http://") || url.startsWith("https://"))) return;
    checkConnection((ok) => {
      if (!ok) { setIcon(false); openErrorPage(); return; }
      try { chrome.downloads.cancel(downloadItem.id); } catch (e) {}
      try { chrome.downloads.erase({ id: downloadItem.id }); } catch (e) {}
      sendToRdm(url, downloadItem.referrer || null, filename || null);
    });
  });
}

if (chrome.downloads.onDeterminingFilename) {
  // Chromium: the suggested name is final here, unlike in onCreated.
  chrome.downloads.onDeterminingFilename.addListener((downloadItem) => {
    captureBrowserDownload(downloadItem, baseName(downloadItem.filename));
  });
} else {
  chrome.downloads.onCreated.addListener((downloadItem) => {
    captureBrowserDownload(downloadItem, baseName(downloadItem.filename));
  });
}