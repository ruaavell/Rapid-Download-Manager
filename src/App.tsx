import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import {
  Archive, ArrowDownToLine, CheckCircle2, ChevronDown, ChevronUp, CirclePause,
  File, FileText, FolderOpen, Gauge, HardDriveDownload, Layers, ListPlus, ListX,
  Music, Pause, Play, Plus, RefreshCw, Settings, Trash2, Video, X, Zap
} from "lucide-react";

type Status = "queued" | "downloading" | "paused" | "merging" | "verifying" | "completed" | "failed" | "cancelled";
type Segment = {
  index: number; start: number; end: number;
  downloaded: number; total: number; complete: boolean;
};
type Task = {
  id: string; url: string; filename: string; destination: string;
  downloaded: number; total: number | null; status: Status;
  speed_limit: number | null; error: string | null; created_at: number;
  completed_at: number | null;
  segments: Segment[]; speed: number;
};
type RowMenu = { task: Task; x: number; y: number };
type Pending = {
  id: string; url: string; referrer: string | null;
  filename: string; size: number | null; content_type: string | null; media?: boolean;
};

const fmtBytes = (n: number) => {
  if (!Number.isFinite(n)) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n, i = 0;
  while (value >= 1024 && i < units.length - 1) { value /= 1024; i++; }
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
};
const pct = (t: Task) => t.total && t.total > 0 ? Math.min(100, t.downloaded / t.total * 100) : 0;
const etaOf = (t: Task) => t.status === "downloading" && t.total && t.speed > 0
  ? Math.max(0, (t.total - t.downloaded) / t.speed)
  : null;
const fmtDuration = (seconds: number) => {
  const total = Math.max(0, Math.round(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
};
const etaLabel = (t: Task) => {
  const eta = etaOf(t);
  return eta === null ? null : `${fmtDuration(eta)} left`;
};
const fmtWhen = (epoch: number) => new Date(epoch * 1000).toLocaleString(undefined, {
  day: "2-digit", month: "short", hour: "2-digit", minute: "2-digit"
});
const MEDIA_HOSTS = ["youtube.com", "youtu.be", "googlevideo.com", "vimeo.com", "dailymotion.com", "twitch.tv", "tiktok.com", "instagram.com", "facebook.com", "x.com", "twitter.com", "reddit.com", "redd.it", "v.redd.it", "streamable.com", "bilibili.com"];
function isMediaCandidate(url?: string | null, page?: string | null): boolean {
  const values = [url, page].filter(Boolean).map(value => String(value).toLowerCase());
  if (values.some(value => MEDIA_HOSTS.some(host => value.includes(host)))) return true;
  const direct = String(url || "").toLowerCase();
  return /\.(m3u8|mpd)(\?|#|$)/.test(direct) || direct.includes("videoplayback");
}

function categoryOf(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() || "";
  if (["zip","rar","7z","tar","gz","bz2","xz","tgz"].includes(ext)) return "compressed";
  if (["mp3","flac","wav","aac","ogg","m4a"].includes(ext)) return "music";
  if (["mp4","mkv","avi","mov","wmv","webm","flv"].includes(ext)) return "videos";
  if (["pdf","doc","docx","xls","xlsx","ppt","pptx","txt","rtf","odt"].includes(ext)) return "documents";
  if (["exe","msi","deb","rpm","appimage","dmg","pkg","sh"].includes(ext)) return "programs";
  return "other";
}
const CATEGORY_LABEL: Record<string,string> = { compressed:"Compressed", documents:"Documents", music:"Music", videos:"Videos", programs:"Programs" };

export default function App() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [filter, setFilter] = useState("all");
  const [modal, setModal] = useState(false);
  const [url, setUrl] = useState("");
  const [destination, setDestination] = useState("");
  const [filename, setFilename] = useState("");
  const [speedLimit, setSpeedLimit] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [autostart, setAutostart] = useState(false);
  const [globalSpeed, setGlobalSpeed] = useState(localStorage.getItem("rdm-global-speed") || "");
  const [appVersion, setAppVersion] = useState("");
  const [updateInfo, setUpdateInfo] = useState<{version: string; url: string} | null>(null);
  const [pending, setPending] = useState<Pending[]>([]);
  const [pendingName, setPendingName] = useState("");
  const [pendingDest, setPendingDest] = useState("");
  const [menu, setMenu] = useState<RowMenu | null>(null);
  const [speedTask, setSpeedTask] = useState<Task | null>(null);
  const [speedValue, setSpeedValue] = useState("");
  const [mediaKind, setMediaKind] = useState("video");
  const [quality, setQuality] = useState("best");
  const [forceMedia, setForceMedia] = useState(false);
  const [pendingKind, setPendingKind] = useState("video");
  const [pendingQuality, setPendingQuality] = useState("best");
  const [deleteTask, setDeleteTask] = useState<Task | null>(null);
  const [dockOpen, setDockOpen] = useState(true);
  const samples = useRef(new Map<string, { bytes: number; time: number }>());
  const currentPending = pending[0];
  const addIsMedia = isMediaCandidate(url);
  const pendingIsMedia = !!currentPending?.media || isMediaCandidate(currentPending?.url, currentPending?.referrer);

  async function refresh() {
    try {
      const list = await invoke<Task[]>("list_downloads");
      const now = performance.now();
      const nextSamples = new Map<string, { bytes: number; time: number }>();
      // Speed is measured from the byte delta between polls, so the engine does
      // not need to keep a second speed value for the UI.
      const enriched = list.map((task) => {
        const previous = samples.current.get(task.id);
        let speed = 0;
        if (previous && now > previous.time) {
          speed = Math.max(0, (task.downloaded - previous.bytes) * 1000 / (now - previous.time));
        }
        nextSamples.set(task.id, { bytes: task.downloaded, time: now });
        return { ...task, segments: task.segments || [], speed };
      });
      samples.current = nextSamples;
      setTasks(enriched);
    } catch (e) { setError(String(e)); }
    // Links sent from the browser extension wait here until confirmed.
    try {
      setPending(await invoke<Pending[]>("list_pending_downloads"));
    } catch { /* keep the previous list */ }
  }
  useEffect(() => {
    setPendingName(currentPending ? currentPending.filename : "");
    setPendingDest("");
    setPendingKind("video");
    setPendingQuality("best");
  }, [currentPending?.id]);
  useEffect(() => {
    invoke<string>("default_download_dir").then(setDestination).catch(e => setError(String(e)));
    getVersion().then(setAppVersion).catch(() => setAppVersion("1.0.13"));
    invoke<[string,string] | null>("check_for_update").then((res) => {
      if (res) setUpdateInfo({ version: res[0], url: res[1] });
    }).catch(() => {});
    refresh();
    const timer = window.setInterval(refresh, 1200);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setMenu(null); };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

  const activeTasks = useMemo(
    () => tasks.filter(t => ["downloading", "merging", "verifying", "paused", "queued"].includes(t.status)),
    [tasks]
  );
  const totalSpeed = activeTasks.reduce((sum, task) => sum + task.speed, 0);
  const allSegments = activeTasks.flatMap(task => task.segments);
  const completedSegments = allSegments.filter(segment => segment.complete).length;

  const visible = useMemo(() => tasks.filter(t => {
    if (filter === "all") return true;
    if (filter === "active") return t.status === "downloading" || t.status === "merging" || t.status === "verifying" || t.status === "queued";
    if (filter === "completed") return t.status === "completed";
    if (filter === "paused") return t.status === "paused";
    if (CATEGORY_LABEL[filter]) return categoryOf(t.filename) === filter;
    return true;
  }), [tasks, filter]);


  function folderFor(url: string, name: string, base: string) {
    const cat = categoryOf(name || url.split("/").pop() || "");
    const label = CATEGORY_LABEL[cat];
    if (!label) return base;
    return base.replace(/\/+$/, "") + "/" + label;
  }

  async function addDownload(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError("");
    try {
      const effectiveSpeed = speedLimit || globalSpeed;
      await invoke("add_download", { payload: {
        url, destination: folderFor(url, filename, destination), filename: filename || null,
        speed_limit: effectiveSpeed ? Number(effectiveSpeed) * 1024 : null,
        media_kind: (addIsMedia || forceMedia) ? mediaKind : null,
        quality: (addIsMedia || forceMedia) ? quality : null
      }});
      setUrl(""); setFilename(""); setSpeedLimit(""); setModal(false);
      await refresh();
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  async function confirmPending() {
    if (!currentPending) return;
    setBusy(true); setError("");
    try {
      const name = pendingName.trim();
      const dest = pendingDest.trim() || folderFor(currentPending.url, name || currentPending.filename, destination);
      await invoke("confirm_pending_download", { payload: {
        id: currentPending.id, destination: dest, filename: name || null,
        speed_limit: globalSpeed ? Number(globalSpeed) * 1024 : null,
        media_kind: pendingIsMedia ? pendingKind : null,
        quality: pendingIsMedia ? pendingQuality : null
      }});
      await refresh();
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  async function dismissPending() {
    if (!currentPending) return;
    try { await invoke("dismiss_pending_download", { id: currentPending.id }); await refresh(); }
    catch (e) { setError(String(e)); }
  }

  async function action(name: string, id: string, extra: object = {}) {
    setMenu(null);
    try { await invoke(name, { id, ...extra }); await refresh(); }
    catch (e) { setError(String(e)); }
  }

  async function confirmDelete() {
    if (!deleteTask) return;
    const task = deleteTask;
    setDeleteTask(null);
    await action("remove_download", task.id, { deleteFile: true });
  }

  function openMenu(event: React.MouseEvent, task: Task) {
    event.preventDefault();
    const width = 235;
    const height = 330;
    setMenu({
      task,
      x: Math.max(8, Math.min(event.clientX, window.innerWidth - width - 8)),
      y: Math.max(8, Math.min(event.clientY, window.innerHeight - height - 8)),
    });
  }

  function openSpeedLimit(task: Task) {
    setMenu(null);
    setSpeedTask(task);
    setSpeedValue(task.speed_limit ? String(Math.round(task.speed_limit / 1024)) : "");
  }

  async function applySpeedLimit(kilobytes: number | null) {
    if (!speedTask) return;
    const bytes = kilobytes && kilobytes > 0 ? Math.round(kilobytes * 1024) : null;
    try {
      await invoke("set_speed_limit", { id: speedTask.id, speedLimit: bytes });
      setSpeedTask(null);
      await refresh();
    } catch (e) { setError(String(e)); }
  }

  return <div className="app-shell">
    <header className="topbar">
      <div className="brand"><div className="brand-icon"><ArrowDownToLine size={23}/></div><div><b>RDM</b><small>RAPID DOWNLOAD MANAGER</small></div></div>
      <div className="top-actions">
        <button className="toolbar primary" onClick={() => setModal(true)}><Plus size={17}/> Add URL</button>
        <button className="toolbar" onClick={refresh}><RefreshCw size={16}/> Refresh</button>
        <button className="icon-button" title="Settings" onClick={()=>setSettingsOpen(true)}><Settings size={19}/></button>
      </div>
    </header>

    <div className="toolbar-strip">
      <button onClick={() => setModal(true)}><Plus/> Add URL</button>
      <button disabled><Play/> Start</button>
      <button disabled><Pause/> Stop</button>
      <button disabled><Trash2/> Delete</button>
      <div className="strip-separator"/>
      <span><Zap size={14}/> Native Rust engine</span>
    </div>

    <div className="layout">
      <aside className="sidebar">
        <div className="side-heading">DOWNLOADS</div>
        <SideItem icon={<HardDriveDownload/>} label="All downloads" count={tasks.length} active={filter==="all"} onClick={()=>setFilter("all")}/>
        <SideItem icon={<ArrowDownToLine/>} label="Active" count={tasks.filter(t=>t.status==="downloading"||t.status==="merging"||t.status==="verifying"||t.status==="queued").length} active={filter==="active"} onClick={()=>setFilter("active")}/>
        <SideItem icon={<CirclePause/>} label="Paused" count={tasks.filter(t=>t.status==="paused").length} active={filter==="paused"} onClick={()=>setFilter("paused")}/>
        <SideItem icon={<CheckCircle2/>} label="Completed" count={tasks.filter(t=>t.status==="completed").length} active={filter==="completed"} onClick={()=>setFilter("completed")}/>
        <div className="side-heading categories">CATEGORIES</div>
        <SideItem icon={<Archive/>} label="Compressed" count={tasks.filter(t=>categoryOf(t.filename)==="compressed").length} active={filter==="compressed"} onClick={()=>setFilter("compressed")}/>
        <SideItem icon={<FileText/>} label="Documents" count={tasks.filter(t=>categoryOf(t.filename)==="documents").length} active={filter==="documents"} onClick={()=>setFilter("documents")}/>
        <SideItem icon={<Music/>} label="Music" count={tasks.filter(t=>categoryOf(t.filename)==="music").length} active={filter==="music"} onClick={()=>setFilter("music")}/>
        <SideItem icon={<Video/>} label="Videos" count={tasks.filter(t=>categoryOf(t.filename)==="videos").length} active={filter==="videos"} onClick={()=>setFilter("videos")}/>
        <SideItem icon={<File/>} label="Programs" count={tasks.filter(t=>categoryOf(t.filename)==="programs").length} active={filter==="programs"} onClick={()=>setFilter("programs")}/>
        <div className="sidebar-bottom"><Gauge size={15}/><span>Queue manager</span><b>{tasks.filter(t=>t.status==="queued").length}</b></div>
      </aside>

      <main className="content">
        {updateInfo && <div className="error-banner" style={{background:"#1f3b5b"}}><span>Update available: RDM {updateInfo.version}</span><button onClick={()=>invoke("open_url",{url:updateInfo.url})}>Download</button><button onClick={()=>setUpdateInfo(null)}><X size={15}/></button></div>}
        <div className="content-heading"><div><h1>{filter === "all" ? "All downloads" : filter[0].toUpperCase()+filter.slice(1)}</h1><p>Manage and monitor your downloads</p></div><button className="add-button" onClick={()=>setModal(true)}><Plus size={17}/> New download</button></div>
        {error && <div className="error-banner"><span>{error}</span><button onClick={()=>setError("")}><X size={15}/></button></div>}
        <div className="table-head"><span>FILE NAME</span><span>SIZE / PROGRESS</span><span>STATUS</span><span>ACTIONS</span></div>
        <div className="task-list">
          {visible.length === 0 && <div className="empty"><div className="empty-icon"><ArrowDownToLine size={30}/></div><h3>No downloads here</h3><p>Add a URL to start your first download.</p><button className="add-button" onClick={()=>setModal(true)}><Plus size={16}/> Add URL</button></div>}
          {visible.map(task => <div className="task-row" key={task.id} onContextMenu={e => openMenu(e, task)}>
            <div className="file-cell"><div className="file-icon"><File size={20}/></div><div className="file-meta"><b title={task.filename}>{task.filename}</b><small title={task.url}>{task.url}</small>{task.status === "completed" && <small className="task-when">Downloaded {fmtWhen(task.completed_at ?? task.created_at)}</small>}<div className="progress-track"><div className={`progress-fill ${task.status}`} style={{width:`${pct(task)}%`}}/></div></div></div>
            <div className="size-cell"><b>{fmtBytes(task.downloaded)}{task.total ? ` / ${fmtBytes(task.total)}` : ""}</b><small>{task.total ? `${pct(task).toFixed(1)}%${etaLabel(task) ? ` · ${etaLabel(task)}` : ""}` : "Size unknown"}</small></div>
            <div><StatusPill status={task.status}/>{task.error && <small className="task-error" title={task.error}>{task.error}</small>}</div>
            <div className="row-actions">
              {task.status === "downloading" ? <button title="Pause" onClick={()=>action("pause_download",task.id)}><Pause size={16}/></button> :
                (task.status === "paused" || task.status === "failed" || task.status === "queued") ? <button title="Resume" onClick={()=>action("resume_download",task.id)}><Play size={16}/></button> : null}
              {task.status !== "completed" && task.status !== "cancelled" && <button title="Cancel" onClick={()=>action("cancel_download",task.id)}><X size={16}/></button>}
              <button title="Delete…" onClick={()=>setDeleteTask(task)}><Trash2 size={16}/></button>
            </div>
          </div>)}
        </div>

        {activeTasks.length > 0 && <section className={`transfer-dock ${dockOpen ? "" : "collapsed"}`}>
          <div className="transfer-dock-head">
            <div><Layers size={14}/><b>Transfers</b><span>{activeTasks.length} active · {fmtBytes(totalSpeed)}/s</span></div>
            <button className="icon-button" title={dockOpen ? "Hide transfers" : "Show transfers"} onClick={()=>setDockOpen(v=>!v)}>{dockOpen ? <ChevronDown size={15}/> : <ChevronUp size={15}/>}</button>
          </div>
          {dockOpen && <div className="transfer-dock-list">
            {activeTasks.map(task => <div className="transfer-row" key={task.id} onContextMenu={e => openMenu(e, task)}>
              <div className="transfer-name"><b title={task.filename}>{task.filename}</b><small>{task.status === "merging" ? "Merging parts" : task.status === "verifying" ? "Verifying file" : task.status === "queued" ? "Waiting in queue" : `${fmtBytes(task.speed)}/s${etaLabel(task) ? ` · ${etaLabel(task)}` : ""}`}</small></div>
              <div className="transfer-rate"><b>{task.total ? `${pct(task).toFixed(1)}%` : fmtBytes(task.downloaded)}</b><small>{fmtBytes(task.downloaded)}{task.total ? ` / ${fmtBytes(task.total)}` : ""}</small></div>
              <div className="transfer-parts">
                <div className="segment-strip">
                  {task.segments.length > 0 ? task.segments.map(segment => <div
                    key={segment.index}
                    className={`segment ${segment.complete ? "complete" : task.status === "downloading" ? "active" : "pending"}`}
                    style={{ flexGrow: Math.max(1, segment.total) }}
                    title={`Part ${segment.index + 1}: ${fmtBytes(segment.downloaded)} / ${fmtBytes(segment.total)}`}
                  ><span style={{ width: `${segment.total > 0 ? segment.downloaded / segment.total * 100 : 0}%` }}/></div>)
                    : <div className={`segment single ${task.status}`} title={`${fmtBytes(task.downloaded)} downloaded`}><span style={{ width: `${pct(task)}%` }}/></div>}
                </div>
                <small>{task.segments.length > 0 ? `${task.segments.filter(segment => segment.complete).length}/${task.segments.length} parts` : task.total && task.total > 2 * 1024 * 1024 && task.status === "downloading" ? "preparing parts" : "single stream"}</small>
              </div>
            </div>)}
          </div>}
        </section>}

        <footer className="statusbar">
          <span><span className={`live-dot ${activeTasks.length ? "busy" : ""}`}/> {activeTasks.length ? "Transferring" : "Engine ready"}</span>
          <span>{fmtBytes(totalSpeed)}/s</span>
          {allSegments.length > 0 && <span>{completedSegments}/{allSegments.length} parts</span>}
          <span>{tasks.length} tasks</span>
          <span>{activeTasks.length} active</span>
          <span>RDM {appVersion || "1.0.13"}</span>
        </footer>
      </main>
    </div>

    {modal && <div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)setModal(false);}}>
      <form className="modal" onSubmit={addDownload}>
        <div className="modal-title"><div><h2>Add download</h2><p>Enter a direct HTTP or HTTPS link</p></div><button type="button" className="icon-button" onClick={()=>setModal(false)}><X/></button></div>
        <label>Download URL<input autoFocus required type="url" placeholder="https://example.com/file.zip" value={url} onChange={e=>setUrl(e.target.value)}/></label>
        <label>Save to<input required value={destination} onChange={e=>setDestination(e.target.value)}/><small>Enter a folder or full destination file path.</small></label>
        <label>Filename (optional)<input value={filename} onChange={e=>setFilename(e.target.value)} placeholder="Automatically detected"/></label>
        <label style={{display:"flex",alignItems:"center",gap:8}}><input type="checkbox" checked={forceMedia} onChange={e=>setForceMedia(e.target.checked)}/> Download as media (video/audio via yt-dlp) for any site</label>
        {(addIsMedia || forceMedia) && <div className="media-options">
          <label>Download as<select value={mediaKind} onChange={e=>setMediaKind(e.target.value)}><option value="video">Video (MP4)</option><option value="audio">Audio only (MP3)</option></select></label>
          {mediaKind === "video" && <label>Quality<select value={quality} onChange={e=>setQuality(e.target.value)}><option value="best">Best available</option><option value="1080">1080p</option><option value="720">720p</option><option value="480">480p</option><option value="360">360p</option></select></label>}
          <small className="media-hint">Streaming pages are downloaded with yt-dlp; audio is converted to MP3 with FFmpeg.</small>
        </div>}
        <label>Speed limit (KB/s, optional)<input type="number" min="1" value={speedLimit} onChange={e=>setSpeedLimit(e.target.value)} placeholder="Unlimited"/></label>
        <div className="modal-actions"><button type="button" className="secondary-button" onClick={()=>setModal(false)}>Cancel</button><button className="add-button" disabled={busy}>{busy?"Adding…":<><ArrowDownToLine size={16}/> Start download</>}</button></div>
      </form>
    </div>}

    {currentPending && <div className="modal-backdrop">
      <div className="modal">
        <div className="modal-title"><div><h2>Download from browser</h2><p>{currentPending.size ? fmtBytes(currentPending.size) : "Size unknown"}{pending.length > 1 ? ` · ${pending.length - 1} more waiting` : ""}</p></div></div>
        <label>URL<input readOnly value={currentPending.url}/></label>
        <label>Filename<input value={pendingName} onChange={e=>setPendingName(e.target.value)}/></label>
        {pendingIsMedia && <div className="media-options">
          <label>Download as<select value={pendingKind} onChange={e=>setPendingKind(e.target.value)}><option value="video">Video (MP4)</option><option value="audio">Audio only (MP3)</option></select></label>
          {pendingKind === "video" && <label>Quality<select value={pendingQuality} onChange={e=>setPendingQuality(e.target.value)}><option value="best">Best available</option><option value="1080">1080p</option><option value="720">720p</option><option value="480">480p</option><option value="360">360p</option></select></label>}
          <small className="media-hint">Uses the page as the media source so YouTube and streaming links save as a real MP4 or MP3.</small>
        </div>}
        <label>Save to<input value={pendingDest} placeholder={folderFor(currentPending.url, pendingName || currentPending.filename, destination)} onChange={e=>setPendingDest(e.target.value)}/><small>Leave empty to use the default folder for this file type.</small></label>
        <div className="modal-actions"><button type="button" className="secondary-button" onClick={dismissPending}>Dismiss</button><button type="button" className="add-button" disabled={busy} onClick={confirmPending}>{busy ? "Adding…" : <><ArrowDownToLine size={16}/> Download</>}</button></div>
      </div>
    </div>}

    {settingsOpen && <div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)setSettingsOpen(false);}}>
      <div className="modal">
        <div className="modal-title"><div><h2>Settings</h2><p>Application preferences</p></div><button type="button" className="icon-button" onClick={()=>setSettingsOpen(false)}><X/></button></div>
        <label>Default download folder<input value={destination} onChange={e=>setDestination(e.target.value)}/></label>
        <label>Global speed limit (KB/s)<input value={globalSpeed} onChange={e=>{setGlobalSpeed(e.target.value); localStorage.setItem("rdm-global-speed", e.target.value);}}/></label>
        <label style={{display:"flex",alignItems:"center",gap:8}}>
          <input type="checkbox" checked={autostart} onChange={e=>{setAutostart(e.target.checked); invoke("set_autostart",{enabled:e.target.checked}).catch(err=>setError(String(err)));}}/>
          Start RDM automatically when I log in
        </label>
        <div className="modal-actions"><button type="button" className="add-button" onClick={()=>setSettingsOpen(false)}>Close</button></div>
      </div>
    </div>}

    {menu && <div className="context-menu" style={{ left: menu.x, top: menu.y }} onMouseDown={e=>e.stopPropagation()} onContextMenu={e=>e.preventDefault()}>
      <button disabled={menu.task.status !== "downloading" && menu.task.status !== "merging"} onClick={()=>action("pause_download", menu.task.id)}><Pause size={15}/> Pause</button>
      <button disabled={!["paused", "failed", "queued"].includes(menu.task.status)} onClick={()=>action("resume_download", menu.task.id)}><Play size={15}/> Resume</button>
      <button className="danger" disabled={menu.task.status === "completed" || menu.task.status === "cancelled"} onClick={()=>action("cancel_download", menu.task.id)}><X size={15}/> Cancel</button>
      <div className="menu-sep"/>
      {["downloading", "merging", "verifying", "paused", "queued"].includes(menu.task.status) && (
        menu.task.status === "queued"
          ? <button onClick={()=>action("dequeue_download", menu.task.id)}><ListX size={15}/> Remove from queue</button>
          : <button onClick={()=>action("enqueue_download", menu.task.id)}><ListPlus size={15}/> Add to queue</button>
      )}
      <button onClick={()=>{ const task = menu.task; setMenu(null); invoke("open_folder", { path: task.destination }).catch(e=>setError(String(e))); }}><FolderOpen size={15}/> Go to file location</button>
      <div className="menu-sep"/>
      <button disabled={menu.task.status === "completed" || menu.task.status === "cancelled"} onClick={()=>openSpeedLimit(menu.task)}><Gauge size={15}/> Speed limit…{menu.task.speed_limit ? <small>{Math.round(menu.task.speed_limit / 1024)} KB/s</small> : null}</button>
      <button className="danger" onClick={()=>{ setDeleteTask(menu.task); setMenu(null); }}><Trash2 size={15}/> Delete…</button>
    </div>}

    {speedTask && <div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)setSpeedTask(null);}}>
      <div className="modal">
        <div className="modal-title"><div><h2>Speed limit</h2><p>{speedTask.filename}</p></div><button type="button" className="icon-button" onClick={()=>setSpeedTask(null)}><X/></button></div>
        <label>Limit (KB/s)<input autoFocus type="number" min="1" value={speedValue} onChange={e=>setSpeedValue(e.target.value)} placeholder="Unlimited"/><small>Applies immediately to the running download.</small></label>
        <div className="modal-actions">
          <button type="button" className="secondary-button" onClick={()=>applySpeedLimit(null)}>Remove limit</button>
          <button type="button" className="secondary-button" onClick={()=>setSpeedTask(null)}>Cancel</button>
          <button type="button" className="add-button" disabled={!speedValue || Number(speedValue) <= 0} onClick={()=>applySpeedLimit(Number(speedValue))}>Apply</button>
        </div>
      </div>
    </div>}

    {deleteTask && <div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)setDeleteTask(null);}}>
      <div className="modal">
        <div className="modal-title"><div><h2>Delete download?</h2><p>{deleteTask.filename}</p></div><button type="button" className="icon-button" onClick={()=>setDeleteTask(null)}><X/></button></div>
        <p className="delete-warning">This removes the download from the list and permanently deletes the file from disk. This cannot be undone.</p>
        <div className="modal-actions">
          <button type="button" className="secondary-button" onClick={()=>setDeleteTask(null)}>Cancel</button>
          <button type="button" className="danger-button" onClick={confirmDelete}><Trash2 size={15}/> Delete file</button>
        </div>
      </div>
    </div>}

  </div>;
}

function SideItem({icon,label,count,active,onClick}:{icon:React.ReactNode;label:string;count?:number;active?:boolean;onClick:()=>void}) {
  return <button className={`side-item ${active?"active":""}`} onClick={onClick}>{icon}<span>{label}</span>{count!==undefined&&<small>{count}</small>}</button>;
}
function StatusPill({status}:{status:Status}) {
  const labels:Record<Status,string>={queued:"In queue",downloading:"Downloading",paused:"Paused",merging:"Merging",verifying:"Verifying",completed:"Completed",failed:"Failed",cancelled:"Cancelled"};
  return <span className={`status-pill ${status}`}><i/>{labels[status]}</span>;
}
