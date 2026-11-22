// Hand Ctrl/Cmd+click links to RDM instead of the browser.
document.addEventListener("click", event => {
  if (!(event.ctrlKey || event.metaKey)) return;
  const anchor = event.target && event.target.closest ? event.target.closest("a") : null;
  if (!anchor || !anchor.href) return;
  if (!(anchor.href.startsWith("http://") || anchor.href.startsWith("https://"))) return;
  event.preventDefault();
  event.stopPropagation();
  try {
    chrome.runtime.sendMessage({ type: "capture", url: anchor.href, referrer: location.href });
  } catch (e) {}
}, true);

// Inject a "Download with RDM" button on every <video>.
function attachRdmButton(video) {
  if (!video || video.dataset.rdmAttached === "1" || video.dataset.rdmHidden === "1") return;
  video.dataset.rdmAttached = "1";
  const parent = video.parentElement;
  if (!parent) return;
  if (getComputedStyle(parent).position === "static") parent.style.position = "relative";

  const wrap = document.createElement("div");
  wrap.className = "rdm-video-button-wrap";
  Object.assign(wrap.style, {
    position: "absolute",
    top: "8px",
    right: "8px",
    zIndex: "2147483647",
    display: "flex",
    gap: "4px",
    alignItems: "center"
  });

  const btn = document.createElement("button");
  btn.type = "button";
  btn.textContent = "Download with RDM";
  btn.className = "rdm-video-button";
  Object.assign(btn.style, {
    padding: "4px 8px",
    fontSize: "12px",
    fontWeight: "600",
    color: "#fff",
    background: "rgba(30,30,34,0.85)",
    border: "1px solid rgba(255,255,255,0.2)",
    borderRadius: "6px",
    cursor: "pointer"
  });
  btn.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const url = video.currentSrc || video.src;
    const target = (url && !url.startsWith("blob:")) ? url : location.href;
    try {
      chrome.runtime.sendMessage({ type: "capture", url: target, referrer: location.href, media: true });
    } catch (e) {}
  });

  const close = document.createElement("button");
  close.type = "button";
  close.textContent = "×";
  close.setAttribute("aria-label", "Hide Download with RDM button");
  close.className = "rdm-video-button-close";
  Object.assign(close.style, {
    width: "20px",
    height: "20px",
    padding: "0",
    fontSize: "14px",
    lineHeight: "18px",
    fontWeight: "700",
    color: "#fff",
    background: "rgba(30,30,34,0.85)",
    border: "1px solid rgba(255,255,255,0.2)",
    borderRadius: "6px",
    cursor: "pointer"
  });
  close.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    wrap.style.display = "none";
    video.dataset.rdmHidden = "1";
  });

  wrap.appendChild(btn);
  wrap.appendChild(close);
  parent.appendChild(wrap);
}

function scanVideos() {
  document.querySelectorAll("video").forEach(attachRdmButton);
}

if (document.documentElement) {
  const observer = new MutationObserver(scanVideos);
  observer.observe(document.documentElement, { childList: true, subtree: true });
}
if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", scanVideos);
} else {
  scanVideos();
}