import { useEffect, useState, useCallback } from "react";
import { getClips, deleteClip, openPath } from "../api";
import { TrimDialog } from "./TrimDialog";
import type { Clip } from "../types";
import { convertFileSrc } from "@tauri-apps/api/core";

interface Props {
  onStatus: (msg: string) => void;
}

export function LibraryView({ onStatus }: Props) {
  const [clips, setClips] = useState<Clip[]>([]);
  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<Clip | null>(null);
  const [trimClip, setTrimClip] = useState<Clip | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async (q = "") => {
    setLoading(true);
    try {
      const cs = await getClips(q);
      setClips(cs);
    } catch (e: any) {
      onStatus(`Library error: ${e}`);
    } finally {
      setLoading(false);
    }
  }, [onStatus]);

  useEffect(() => { load(); }, [load]);

  const handleSearch = (e: React.ChangeEvent<HTMLInputElement>) => {
    setSearch(e.target.value);
    load(e.target.value);
  };

  const handleDelete = async () => {
    if (!selected) return;
    if (!confirm(`Delete "${selected.filename}" and its file?`)) return;
    try {
      await deleteClip(selected.id, true);
      setSelected(null);
      load(search);
      onStatus("Deleted.");
    } catch (e: any) {
      onStatus(`Delete failed: ${e}`);
    }
  };

  const handleOpenFolder = () => {
    if (!selected) return;
    const folder = selected.filepath.replace(/[/\\][^/\\]+$/, "");
    openPath(folder);
  };

  const handleCopyPath = () => {
    if (!selected) return;
    navigator.clipboard.writeText(selected.filepath);
    onStatus("Path copied.");
  };

  const formatDuration = (s: number) => {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = Math.floor(s % 60);
    return h > 0
      ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`
      : `${m}:${String(sec).padStart(2, "0")}`;
  };

  const formatSize = (b: number) => {
    if (b < 1024) return `${b} B`;
    if (b < 1024 ** 2) return `${(b / 1024).toFixed(1)} KB`;
    if (b < 1024 ** 3) return `${(b / 1024 ** 2).toFixed(1)} MB`;
    return `${(b / 1024 ** 3).toFixed(1)} GB`;
  };

  return (
    <div>
      {trimClip && (
        <TrimDialog
          clip={trimClip}
          onClose={() => setTrimClip(null)}
          onStatus={onStatus}
        />
      )}

      <div className="library-search">
        <input
          type="text"
          placeholder="🔍 Search clips…"
          value={search}
          onChange={handleSearch}
        />
      </div>

      {loading && <div style={{ textAlign: "center", color: "var(--text-muted)", padding: 20 }}>Loading…</div>}

      {!loading && clips.length === 0 && (
        <div className="empty-state">
          <div className="empty-icon">🎬</div>
          <div>No clips yet. Record something!</div>
        </div>
      )}

      <div className="clip-grid">
        {clips.map(clip => (
          <div
            key={clip.id}
            className={`clip-card ${selected?.id === clip.id ? "selected" : ""}`}
            onClick={() => setSelected(clip)}
            onDoubleClick={() => setTrimClip(clip)}
            title={`${clip.filename}\n${formatDuration(clip.duration_s)} • ${formatSize(clip.filesize_b)}`}
          >
            {clip.thumbnail ? (
              <img
                className="clip-thumb"
                src={convertFileSrc(clip.thumbnail)}
                alt={clip.filename}
                loading="lazy"
              />
            ) : (
              <div className="clip-thumb-placeholder">🎬</div>
            )}
            <div className="clip-info">
              <div className="clip-name">{clip.filename.replace(/\.[^.]+$/, "")}</div>
              <div className="clip-meta">
                {formatDuration(clip.duration_s)} · {clip.format?.split(",")[0]?.toUpperCase() || "MP4"}
              </div>
            </div>
          </div>
        ))}
      </div>

      {selected && (
        <div className="library-toolbar">
          <button className="btn btn-secondary btn-sm" onClick={handleOpenFolder}>📂 Open folder</button>
          <button className="btn btn-secondary btn-sm" onClick={() => setTrimClip(selected)}>✂ Trim</button>
          <button className="btn btn-secondary btn-sm" onClick={handleCopyPath}>📋 Copy path</button>
          <button className="btn btn-danger btn-sm" onClick={handleDelete}>🗑 Delete</button>
          <span style={{ color: "var(--text-muted)", fontSize: 11, alignSelf: "center", marginLeft: 4 }}>
            {selected.width}×{selected.height} · {selected.fps.toFixed(0)}fps · {formatSize(selected.filesize_b)}
          </span>
        </div>
      )}
    </div>
  );
}
