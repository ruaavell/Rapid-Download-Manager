const HOST = "io.github.ruaavell.rdm";

function sendToRdm(url, referrer) {
  if (!url || !/^https?:\/\//i.test(url)) return;
  chrome.runtime.sendNativeMessage(HOST, { action: "add", url, referrer: referrer || null }, response => {
    if (chrome.runtime.lastError || !response?.ok) {
      chrome.notifications.create({
        type: "basic",
        iconUrl: "icons/rdm.svg",
        title: "RDM",
        message: chrome.runtime.lastError?.message || response?.error || "Could not contact RDM."
      });
    } else {
      chrome.notifications.create({
        type: "basic",
        iconUrl: "icons/rdm.svg",
        title: "RDM",
        message: "Download sent to RDM."
      });
    }
  });
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: "rdm-download-link", title: "Download with RDM", contexts: ["link"] });
  chrome.contextMenus.create({ id: "rdm-download-page", title: "Send page URL to RDM", contexts: ["page"] });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  sendToRdm(info.linkUrl || tab?.url, tab?.url);
});

chrome.runtime.onMessage.addListener((message, sender) => {
  if (message && message.type === "capture") {
    sendToRdm(message.url, message.referrer || sender?.tab?.url);
  }
});

// Intercept automatic browser downloads and hand them to RDM.
chrome.downloads.onCreated.addListener(downloadItem => {
  const url = downloadItem.finalUrl || downloadItem.url;
  if (!url || !/^https?:\/\//i.test(url)) return;
  // Cancel the browser's own download, then erase it from history.
  try { chrome.downloads.cancel(downloadItem.id); } catch (e) {}
  try { chrome.downloads.erase({ id: downloadItem.id }); } catch (e) {}
  sendToRdm(url, null);
});