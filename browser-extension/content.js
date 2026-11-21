// Hand Ctrl/Cmd+click links to RDM instead of the browser.
document.addEventListener("click", event => {
  if (!(event.ctrlKey || event.metaKey)) return;
  const anchor = event.target && event.target.closest ? event.target.closest("a") : null;
  if (!anchor || !anchor.href) return;
  if (!/^https?:\/\//i.test(anchor.href)) return;
  event.preventDefault();
  event.stopPropagation();
  try {
    chrome.runtime.sendMessage({ type: "capture", url: anchor.href, referrer: location.href });
  } catch (e) {}
}, true);