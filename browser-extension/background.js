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

function sendToRdm(url, referrer) {
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
      chrome.runtime.sendNativeMessage(HOST, { action: "add", url, referrer: referrer || null }, (response) => {
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
    sendToRdm(message.url, message.referrer || sender?.tab?.url);
  }
});

chrome.downloads.onCreated.addListener((downloadItem) => {
  isEnabled((enabled) => {
    if (!enabled) return;
    const url = downloadItem.finalUrl || downloadItem.url;
    if (!url || !(url.startsWith("http://") || url.startsWith("https://"))) return;
    checkConnection((ok) => {
      if (!ok) { setIcon(false); openErrorPage(); return; }
      try { chrome.downloads.cancel(downloadItem.id); } catch (e) {}
      try { chrome.downloads.erase({ id: downloadItem.id }); } catch (e) {}
      sendToRdm(url, null);
    });
  });
});