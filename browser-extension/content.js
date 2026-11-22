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

function resolvePageUrl(videoOrPlayer) {
  let pageUrl = location.href;
  try {
    const host = location.hostname || "";
    let selector = null;
    if (host.includes("x.com") || host.includes("twitter.com")) selector = 'a[href*="/status/"]';
    else if (host.includes("reddit.com")) selector = 'a[href*="/comments/"]';
    else if (host.includes("instagram.com")) selector = 'a[href*="/p/"], a[href*="/reel/"], a[href*="/reels/"]';
    if (selector) {
      const root = (videoOrPlayer && videoOrPlayer.closest) ? (videoOrPlayer.closest("article, shreddit-post, [data-testid='post-container']") || document) : document;
      const link = root.querySelector(selector);
      if (link && link.href) pageUrl = link.href;
    }
  } catch (e) {}
  return pageUrl;
}

function latestMediaUrl() {
  try {
    const entries = performance.getEntriesByType("resource");
    const mediaExt = /\.(mp4|webm|m3u8|mpd|mp3|m4a|mov)(\?|#|$)/i;
    const candidates = entries.filter((e) => mediaExt.test(e.name));
    if (candidates.length) return candidates[candidates.length - 1].name;
    const cdn = entries.filter((e) => /twimg\.com|v\.redd\.it|cdninstagram\.com|fbcdn\.net/i.test(e.name));
    if (cdn.length) return cdn[cdn.length - 1].name;
  } catch (e) {}
  return null;
}

function resolveInstagramContainer(video) {
  let node = video;
  let fallback = video && video.parentElement ? video.parentElement : null;
  while (node && node.parentElement && node.parentElement !== document.body) {
    const parent = node.parentElement;
    if (parent.querySelectorAll && parent.querySelectorAll("video").length > 1) break;
    let overlay = null;
    try {
      const groups = Array.from(parent.querySelectorAll('[role="group"]'));
      overlay = groups.find(g => !g.contains(video));
    } catch (e) {}
    if (overlay) return parent;
    node = parent;
    fallback = parent;
  }
  return fallback;
}

function getPlayerVideo(player) {
  try {
    if (player && player.shadowRoot) {
      const v = player.shadowRoot.querySelector("video");
      if (v) return v;
      const s = player.shadowRoot.querySelector("source[src]");
      if (s) return null;
    }
  } catch (e) {}
  return null;
}

function getPlayerSrc(player) {
  try {
    if (!player) return "";
    if (player.getAttribute && player.getAttribute("src")) return player.getAttribute("src");
    if (player.shadowRoot) {
      const v = player.shadowRoot.querySelector("video");
      if (v) return v.currentSrc || v.src || "";
      const s = player.shadowRoot.querySelector("source[src]");
      if (s) return s.src || "";
    }
  } catch (e) {}
  return "";
}

function attachRdmButton(target, containerOverride) {
  if (!target || target.dataset.rdmAttached === "1" || target.dataset.rdmHidden === "1") return;
  // Resolve the mount point first; if it is not ready yet, leave the
  // element unmarked so the next scan can retry.
  let parent = null;
  const isPlayer = target.tagName && /^shreddit-player/i.test(target.tagName);
  const video = (target.tagName === "VIDEO") ? target : getPlayerVideo(target);
  if (containerOverride) {
    parent = containerOverride;
  } else if (location.hostname.includes("instagram.com") && video) {
    parent = resolveInstagramContainer(video);
  } else {
    parent = target.parentElement;
  }
  if (!parent) return;
  target.dataset.rdmAttached = "1";

  try {
    if (getComputedStyle(parent).position === "static") parent.style.position = "relative";
  } catch (e) {}
  // Avoid attaching twice when the same video is re-scanned.
  if (parent.querySelector && parent.querySelector(":scope > .rdm-video-button-wrap")) return;

  const wrap = document.createElement("div");
  wrap.className = "rdm-video-button-wrap";
  Object.assign(wrap.style, {
    position: "absolute",
    top: "8px",
    right: "8px",
    zIndex: "2147483647",
    display: "flex",
    gap: "4px",
    alignItems: "center",
    pointerEvents: "auto",
    opacity: "0.35",
    transition: "opacity 0.25s ease"
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
    background: "rgba(30,30,34,0.9)",
    border: "1px solid rgba(255,255,255,0.25)",
    borderRadius: "6px",
    cursor: "pointer",
    pointerEvents: "auto"
  });

  const clickHandler = (e) => {
    e.preventDefault();
    e.stopPropagation();
    const pageUrl = resolvePageUrl(target);
    let url = "";
    if (target.tagName === "VIDEO") url = target.currentSrc || target.src || "";
    else url = getPlayerSrc(target);
    // Use the post/page URL (plus browser cookies) as the media source. This
    // keeps the correct video selected and lets yt-dlp handle age-restricted
    // tweets through the authenticated GraphQL endpoint.
    const targetUrl = (url && !url.startsWith("blob:")) ? url : pageUrl;
    const directFile = /\.(mp4|webm|mkv|mov|m4v|mp3|m4a|flac|wav|ogg)(\?|#|$)/i.test(targetUrl);
    try {
      chrome.runtime.sendMessage({ type: "capture", url: targetUrl, referrer: location.href, media: !directFile });
    } catch (e) {}
  };
  btn.addEventListener("click", clickHandler, true);

  const close = document.createElement("button");
  close.type = "button";
  close.textContent = "\u00d7";
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
    background: "rgba(30,30,34,0.9)",
    border: "1px solid rgba(255,255,255,0.25)",
    borderRadius: "6px",
    cursor: "pointer",
    pointerEvents: "auto"
  });
  close.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    wrap.style.display = "none";
    target.dataset.rdmHidden = "1";
  }, true);

  wrap.appendChild(btn);
  wrap.appendChild(close);

  // Auto-hide: stay faintly visible, become fully visible on hover,
  // and hide completely after 30s without hover.
  let hideTimer = null;
  const show = () => {
    clearTimeout(hideTimer);
    wrap.style.pointerEvents = "auto";
    wrap.style.opacity = "1";
  };
  const scheduleHide = () => {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => { wrap.style.opacity = "0"; }, 30000);
  };
  const reveal = () => {
    show();
    scheduleHide();
  };
  wrap.addEventListener("mouseenter", show, true);
  wrap.addEventListener("mouseleave", () => { scheduleHide(); }, true);
  btn.addEventListener("mouseenter", show, true);
  close.addEventListener("mouseenter", show, true);
  scheduleHide();

  // Hide while a video is in fullscreen.
  const onFsChange = () => {
    if (document.fullscreenElement || document.webkitFullscreenElement) {
      wrap.style.display = "none";
    } else {
      wrap.style.display = "flex";
      wrap.style.opacity = "0.35";
      scheduleHide();
    }
  };
  document.addEventListener("fullscreenchange", onFsChange);
  document.addEventListener("webkitfullscreenchange", onFsChange);

  parent.appendChild(wrap);
}

function deepQuerySelectorAll(selector, root) {
  root = root || document;
  const results = Array.from(root.querySelectorAll(selector));
  const all = root.querySelectorAll ? Array.from(root.querySelectorAll("*")) : [];
  for (const el of all) {
    if (el.shadowRoot) {
      results.push(...deepQuerySelectorAll(selector, el.shadowRoot));
    }
  }
  return results;
}

function scanVideos() {
  if (location.hostname.includes("reddit.com")) {
    deepQuerySelectorAll("shreddit-post").forEach((post) => {
      const type = (post.getAttribute && (post.getAttribute("post-type") || "")) || "";
      const hasPlayer = !!(post.querySelector && (post.querySelector("shreddit-player, shreddit-player-2, shreddit-player-static, shreddit-player-static-hlsjs, shreddit-async-loader, video") || (post.shadowRoot && post.shadowRoot.querySelector("video"))));
      if (!/video/i.test(type) && !hasPlayer) return;
      let container = null;
      try { container = post.querySelector('[slot="post-media-container"]'); } catch (e) {}
      attachRdmButton(post, container || post);
    });
    return;
  }
  deepQuerySelectorAll("video").forEach(attachRdmButton);
  deepQuerySelectorAll("shreddit-player, shreddit-player-2, shreddit-player-static, shreddit-player-static-hlsjs").forEach(attachRdmButton);
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
setInterval(scanVideos, 2000);