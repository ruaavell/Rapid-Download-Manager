import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import {
  Archive, ArrowDownToLine, CheckCircle2, CirclePause, Clock3, File,
  FileText, Film, FolderOpen, Gauge, HardDriveDownload, Music, Pause,
  Play, Plus, RefreshCw, Settings, Trash2, Video, X, Zap
} from "lucide-react";

type Status = "queued" | "downloading" | "paused" | "completed" | "failed" | "cancelled";
type Task = {
  id: string; url: string; filename: string; destination: string;
  downloaded: number; total: number | null; status: Status;
  speed_limit: number | null; error: string | null; created_at: number;
};
type Pending = {
  id: string; url: string; referrer: string | null;
  filename: string; size: number | null; content_type: string | null;
};

const fmtBytes = (n: number) => {
  if (!Number.isFinite(n)) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n, i = 0;
  while (value >= 1024 && i < units.length - 1) { value /= 1024; i++; }
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
};
const pct = (t: Task) => t.total && t.total > 0 ? Math.min(100, t.downloaded / t.total * 100) : 0;

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
  const currentPending = pending[0];

  async function refresh() {
    try {
      setTasks(await invoke<Task[]>("list_downloads"));
    } catch (e) { setError(String(e)); }
    // Links sent from the browser extension wait here until confirmed.
    try {
      setPending(await invoke<Pending[]>("list_pending_downloads"));
    } catch { /* keep the previous list */ }
  }
  useEffect(() => {
    setPendingName(currentPending ? currentPending.filename : "");
    setPendingDest("");
  }, [currentPending?.id]);
  useEffect(() => {
    invoke<string>("default_download_dir").then(setDestination).catch(e => setError(String(e)));
    getVersion().then(setAppVersion).catch(() => setAppVersion("1.0.5"));
    invoke<[string,string] | null>("check_for_update").then((res) => {
      if (res) setUpdateInfo({ version: res[0], url: res[1] });
    }).catch(() => {});
    refresh();
    const timer = window.setInterval(refresh, 1200);
    return () => window.clearInterval(timer);
  }, []);

  const visible = useMemo(() => tasks.filter(t => {
    if (filter === "all") return true;
    if (filter === "active") return t.status === "downloading" || t.status === "queued";
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
        speed_limit: effectiveSpeed ? Number(effectiveSpeed) * 1024 : null
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
        speed_limit: globalSpeed ? Number(globalSpeed) * 1024 : null
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
    try { await invoke(name, { id, ...extra }); await refresh(); }
    catch (e) { setError(String(e)); }
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
        <SideItem icon={<ArrowDownToLine/>} label="Active" count={tasks.filter(t=>t.status==="downloading"||t.status==="queued").length} active={filter==="active"} onClick={()=>setFilter("active")}/>
        <SideItem icon={<CirclePause/>} label="Paused" count={tasks.filter(t=>t.status==="paused").length} active={filter==="paused"} onClick={()=>setFilter("paused")}/>
        <SideItem icon={<CheckCircle2/>} label="Completed" count={tasks.filter(t=>t.status==="completed").length} active={filter==="completed"} onClick={()=>setFilter("completed")}/>
        <div className="side-heading categories">CATEGORIES</div>
        <SideItem icon={<Archive/>} label="Compressed" count={tasks.filter(t=>categoryOf(t.filename)==="compressed").length} active={filter==="compressed"} onClick={()=>setFilter("compressed")}/>
        <SideItem icon={<FileText/>} label="Documents" count={tasks.filter(t=>categoryOf(t.filename)==="documents").length} active={filter==="documents"} onClick={()=>setFilter("documents")}/>
        <SideItem icon={<Music/>} label="Music" count={tasks.filter(t=>categoryOf(t.filename)==="music").length} active={filter==="music"} onClick={()=>setFilter("music")}/>
        <SideItem icon={<Video/>} label="Videos" count={tasks.filter(t=>categoryOf(t.filename)==="videos").length} active={filter==="videos"} onClick={()=>setFilter("videos")}/>
        <SideItem icon={<File/>} label="Programs" count={tasks.filter(t=>categoryOf(t.filename)==="programs").length} active={filter==="programs"} onClick={()=>setFilter("programs")}/>
        <div className="sidebar-bottom"><Gauge size={15}/><span>Queue manager</span><b>{tasks.filter(t=>t.status==="downloading").length}</b></div>
      </aside>

      <main className="content">
        {updateInfo && <div className="error-banner" style={{background:"#1f3b5b"}}><span>Update available: RDM {updateInfo.version}</span><button onClick={()=>invoke("open_url",{url:updateInfo.url})}>Download</button><button onClick={()=>setUpdateInfo(null)}><X size={15}/></button></div>}
        <div className="content-heading"><div><h1>{filter === "all" ? "All downloads" : filter[0].toUpperCase()+filter.slice(1)}</h1><p>Manage and monitor your downloads</p></div><button className="add-button" onClick={()=>setModal(true)}><Plus size={17}/> New download</button></div>
        {error && <div className="error-banner"><span>{error}</span><button onClick={()=>setError("")}><X size={15}/></button></div>}
        <div className="table-head"><span>FILE NAME</span><span>SIZE / PROGRESS</span><span>STATUS</span><span>ACTIONS</span></div>
        <div className="task-list">
          {visible.length === 0 && <div className="empty"><div className="empty-icon"><ArrowDownToLine size={30}/></div><h3>No downloads here</h3><p>Add a URL to start your first download.</p><button className="add-button" onClick={()=>setModal(true)}><Plus size={16}/> Add URL</button></div>}
          {visible.map(task => <div className="task-row" key={task.id}>
            <div className="file-cell"><div className="file-icon"><File size={20}/></div><div className="file-meta"><b title={task.filename}>{task.filename}</b><small title={task.url}>{task.url}</small><div className="progress-track"><div className={`progress-fill ${task.status}`} style={{width:`${pct(task)}%`}}/></div></div></div>
            <div className="size-cell"><b>{fmtBytes(task.downloaded)}{task.total ? ` / ${fmtBytes(task.total)}` : ""}</b><small>{task.total ? `${pct(task).toFixed(1)}%` : "Size unknown"}</small></div>
            <div><StatusPill status={task.status}/>{task.error && <small className="task-error" title={task.error}>{task.error}</small>}</div>
            <div className="row-actions">
              {task.status === "downloading" ? <button title="Pause" onClick={()=>action("pause_download",task.id)}><Pause size={16}/></button> :
                (task.status === "paused" || task.status === "failed" || task.status === "queued") ? <button title="Resume" onClick={()=>action("resume_download",task.id)}><Play size={16}/></button> : null}
              {task.status !== "completed" && task.status !== "cancelled" && <button title="Cancel" onClick={()=>action("cancel_download",task.id)}><X size={16}/></button>}
              <button title="Remove" onClick={()=>action("remove_download",task.id,{deleteFile:false})}><Trash2 size={16}/></button>
            </div>
          </div>)}
        </div>
        <footer className="statusbar"><span><span className="live-dot"/> Engine ready</span><span>{tasks.length} tasks</span><span>{tasks.filter(t=>t.status==="downloading").length} active</span><span>RDM {appVersion || "1.0.5"}</span></footer>
      </main>
    </div>

    {modal && <div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)setModal(false);}}>
      <form className="modal" onSubmit={addDownload}>
        <div className="modal-title"><div><h2>Add download</h2><p>Enter a direct HTTP or HTTPS link</p></div><button type="button" className="icon-button" onClick={()=>setModal(false)}><X/></button></div>
        <label>Download URL<input autoFocus required type="url" placeholder="https://example.com/file.zip" value={url} onChange={e=>setUrl(e.target.value)}/></label>
        <label>Save to<input required value={destination} onChange={e=>setDestination(e.target.value)}/><small>Enter a folder or full destination file path.</small></label>
        <label>Filename (optional)<input value={filename} onChange={e=>setFilename(e.target.value)} placeholder="Automatically detected"/></label>
        <label>Speed limit (KB/s, optional)<input type="number" min="1" value={speedLimit} onChange={e=>setSpeedLimit(e.target.value)} placeholder="Unlimited"/></label>
        <div className="modal-actions"><button type="button" className="secondary-button" onClick={()=>setModal(false)}>Cancel</button><button className="add-button" disabled={busy}>{busy?"Adding…":<><ArrowDownToLine size={16}/> Start download</>}</button></div>
      </form>
    </div>}

    {currentPending && <div className="modal-backdrop">
      <div className="modal">
        <div className="modal-title"><div><h2>Download from browser</h2><p>{currentPending.size ? fmtBytes(currentPending.size) : "Size unknown"}{pending.length > 1 ? ` · ${pending.length - 1} more waiting` : ""}</p></div></div>
        <label>URL<input readOnly value={currentPending.url}/></label>
        <label>Filename<input value={pendingName} onChange={e=>setPendingName(e.target.value)}/></label>
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

  </div>;
}

function SideItem({icon,label,count,active,onClick}:{icon:React.ReactNode;label:string;count?:number;active?:boolean;onClick:()=>void}) {
  return <button className={`side-item ${active?"active":""}`} onClick={onClick}>{icon}<span>{label}</span>{count!==undefined&&<small>{count}</small>}</button>;
}
function StatusPill({status}:{status:Status}) {
  const labels:Record<Status,string>={queued:"Queued",downloading:"Downloading",paused:"Paused",completed:"Completed",failed:"Failed",cancelled:"Cancelled"};
  return <span className={`status-pill ${status}`}><i/>{labels[status]}</span>;
}
