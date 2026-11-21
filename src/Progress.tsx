import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, FolderOpen, Pause, Play, XCircle } from "lucide-react";

type Task = {
  id: string; url: string; filename: string; destination: string;
  status: string; downloaded: number; total: number | null;
  speedLimit: number | null; error: string | null;
};

const pct = (t: Task) => t.total && t.total > 0 ? Math.min(100, t.downloaded / t.total * 100) : 0;
const fmt = (b: number) => b < 1024 ? `${b} B` : b < 1048576 ? `${(b/1024).toFixed(1)} KB` : `${(b/1048576).toFixed(1)} MB`;

export default function Progress() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [completedPopup, setCompletedPopup] = useState<Task | null>(null);
  const [seenCompleted, setSeenCompleted] = useState<Set<string>>(new Set());

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const list = await invoke<Task[]>("list_downloads");
        if (!alive) return;
        setTasks(list);
        // detect newly completed
        for (const t of list) {
          if (t.status === "completed" && !seenCompleted.has(t.id)) {
            setSeenCompleted(prev => new Set(prev).add(t.id));
            setCompletedPopup(t);
          }
        }
      } catch {}
    };
    load();
    const id = window.setInterval(load, 500);
    return () => { alive = false; window.clearInterval(id); };
  }, [seenCompleted]);

  const active = tasks.filter(t => ["queued","downloading","paused","merging"].includes(t.status));
  const recentCompleted = tasks.filter(t => t.status === "completed").slice(-3);

  async function pause(id: string) { await invoke("pause_download", { id }); }
  async function resume(id: string) { await invoke("resume_download", { id }); }
  async function cancel(id: string) { await invoke("cancel_download", { id }); }
  async function openFolder(path: string) { await invoke("open_folder", { path }); }

  return (
    <div className="progress-window">
      <div className="progress-header">
        <h2>RDM Downloads</h2>
      </div>
      <div className="progress-list">
        {active.length === 0 && recentCompleted.length === 0 && (
          <div className="progress-empty">No active downloads</div>
        )}
        {active.map(t => (
          <div key={t.id} className="progress-item">
            <div className="progress-name" title={t.filename}>{t.filename}</div>
            <div className="progress-bar"><div className="progress-fill" style={{ width: `${pct(t)}%` }} /></div>
            <div className="progress-meta">
              <span>{fmt(t.downloaded)}{t.total ? ` / ${fmt(t.total)}` : ""} · {pct(t).toFixed(1)}%</span>
              <span className="progress-status">{t.status === "merging" ? "Merging segments…" : t.status}</span>
            </div>
            <div className="progress-actions">
              {t.status === "downloading" && <button onClick={() => pause(t.id)} title="Pause"><Pause size={14}/></button>}
              {t.status === "paused" && <button onClick={() => resume(t.id)} title="Resume"><Play size={14}/></button>}
              <button onClick={() => cancel(t.id)} title="Cancel"><XCircle size={14}/></button>
            </div>
          </div>
        ))}
        {recentCompleted.map(t => (
          <div key={t.id} className="progress-item completed">
            <div className="progress-name">{t.filename}</div>
            <div className="progress-meta"><span>Completed</span></div>
            <button className="progress-open" onClick={() => openFolder(t.destination)}><FolderOpen size={14}/> Open folder</button>
          </div>
        ))}
      </div>

      {completedPopup && (
        <div className="modal-backdrop">
          <div className="modal">
            <div className="modal-title"><div><h2>Download complete</h2><p>{completedPopup.filename}</p></div><button type="button" className="icon-button" onClick={() => setCompletedPopup(null)}><X/></button></div>
            <div className="modal-actions">
              <button type="button" className="secondary-button" onClick={() => setCompletedPopup(null)}>Close</button>
              <button type="button" className="add-button" onClick={() => { openFolder(completedPopup.destination); setCompletedPopup(null); }}><FolderOpen size={16}/> Open folder</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}