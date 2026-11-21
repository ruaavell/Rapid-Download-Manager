import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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
  const [pending, setPending] = useState<Pending[]>([]);
  const [pendingDest, setPendingDest] = useState("");
  const [pendingName, setPendingName] = useState("");

  async function refresh() {
    try {
      try { await invoke("import_browser_inbox"); } catch (e) { await invoke("importBrowserInbox"); }
      setTasks(await invoke<Task[]>("list_downloads"));
      const p = await invoke<Pending[]>("list_pending_downloads");
      setPending(p);
      if (p.length && !pendingDest) {
        const dir = await invoke<string>("default_download_dir");
        setPendingDest(dir);
      }
      if (p.length && !pendingName) setPendingName(p[0].filename);
    } catch (e) { setError(String(e)); }
  }
  useEffect(() => {
    invoke<string>("default_download_dir").then(setDestination).catch(e => setError(String(e)));
    refresh();
    const timer = window.setInterval(refresh, 1200);
    return () => window.clearInterval(timer);
  }, []);

  const visible = useMemo(() => tasks.filter(t => {
    if (filter === "all") return true;
    if (filter === "active") return t.status === "downloading" || t.status === "queued";
    if (filter === "completed") return t.status === "completed";
    if (filter === "paused") return t.status === "paused";
    return true;
  }), [tasks, filter]);

  async function confirmPending() {
    if (!pending.length) return;
    setBusy(true); setError("");
    try {
      await invoke("confirm_pending_download", { payload: {
        id: pending[0].id, destination: pendingDest,
        filename: pendingName || null
      }});
      setPending(prev => prev.slice(1));
      setPendingName("");
      await refresh();
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function dismissPending() {
    if (!pending.length) return;
    try {
      await invoke("dismiss_pending_download", { id: pending[0].id });
      setPending(prev => prev.slice(1));
      setPendingName("");
      await refresh();
    } catch (e) { setError(String(e)); }
  }

  async function addDownload(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError("");
    try {
      await invoke("add_download", { payload: {
        url, destination, filename: filename || null,
        speedLimit: speedLimit ? Number(speedLimit) * 1024 : null
      }});
      setUrl(""); setFilename(""); setSpeedLimit(""); setModal(false);
      await refresh();
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
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
        <button className="icon-button" title="Settings"><Settings size={19}/></button>
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
        <SideItem icon={<Archive/>} label="Compressed" onClick={()=>setFilter("all")}/>
        <SideItem icon={<FileText/>} label="Documents" onClick={()=>setFilter("all")}/>
        <SideItem icon={<Music/>} label="Music" onClick={()=>setFilter("all")}/>
        <SideItem icon={<Video/>} label="Videos" onClick={()=>setFilter("all")}/>
        <SideItem icon={<File/>} label="Programs" onClick={()=>setFilter("all")}/>
        <div className="sidebar-bottom"><Gauge size={15}/><span>Queue manager</span><b>{tasks.filter(t=>t.status==="downloading").length}</b></div>
      </aside>

      <main className="content">
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
        <footer className="statusbar"><span><span className="live-dot"/> Engine ready</span><span>{tasks.length} tasks</span><span>{tasks.filter(t=>t.status==="downloading").length} active</span><span>RDM 0.2.0</span></footer>
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

    {pending.length > 0 && <div className="modal-backdrop">
      <div className="modal">
        <div className="modal-title"><div><h2>Download detected</h2><p>Confirm where to save this file</p></div></div>
        <div className="file-meta" style={{marginBottom:12}}>
          <b>{pending[0].filename}</b>
          <small title={pending[0].url}>{pending[0].url}</small>
          <small>{pending[0].size ? fmtBytes(pending[0].size) : "Size unknown"}{pending[0].content_type ? ` • ${pending[0].content_type}` : ""}</small>
        </div>
        <label>Save to<input required value={pendingDest} onChange={e=>setPendingDest(e.target.value)}/></label>
        <label>Filename<input required value={pendingName} onChange={e=>setPendingName(e.target.value)}/></label>
        <div className="modal-actions">
          <button type="button" className="secondary-button" onClick={dismissPending} disabled={busy}>Cancel</button>
          <button type="button" className="add-button" onClick={confirmPending} disabled={busy}>{busy?"Starting…":"Download"}</button>
        </div>
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
