import { useEffect, useState, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { RecorderView } from "./components/RecorderView";
import { LibraryView } from "./components/LibraryView";
import { SettingsView } from "./components/SettingsView";
import { DebugPanel } from "./components/DebugPanel";
import { getSettings, saveSettings, applyHotkeys, detectHwEncoder } from "./api";
import type { Settings, View } from "./types";

const DEFAULT_SETTINGS: Settings = {
  output_dir: "",
  default_format: "mp4",
  default_fps: 30,
  default_quality_crf: 28,
  mic_device: null,
  mic_device_id: null,
  sys_audio_device: null,
  sys_audio_device_id: null,
  capture_cursor: true,
  show_keystroke_hud: true,
  hotkeys: {
    start:         "CommandOrControl+Shift+R",
    stop:          "CommandOrControl+Shift+S",
    pause:         "CommandOrControl+Shift+P",
    library:       "CommandOrControl+Shift+L",
    replay_toggle: "CommandOrControl+Shift+B",
    replay_save:   "CommandOrControl+Shift+F",
  },
  hw_encoder: "auto",
  minimize_to_tray: true,
  filename_template: "recording_{datetime}",
  replay_buffer_duration_secs: 0,
  replay_output_dir: "",
  replay_filename_template: "replay_{datetime}",
};

/** Floating toast shown briefly when a replay clip is saved. */
function ReplaySavedToast({ filename, onDone }: { filename: string; onDone: () => void }) {
  // Auto-dismiss after 3 seconds
  useEffect(() => {
    const id = window.setTimeout(onDone, 3000);
    return () => clearTimeout(id);
  }, [onDone]);

  return (
    <div style={{
      position: "fixed",
      bottom: 40,
      left: "50%",
      transform: "translateX(-50%)",
      background: "rgba(15,155,88,0.95)",
      color: "#fff",
      padding: "10px 20px",
      borderRadius: 8,
      fontSize: 13,
      fontWeight: 600,
      boxShadow: "0 4px 20px rgba(0,0,0,0.5)",
      zIndex: 9999,
      display: "flex",
      alignItems: "center",
      gap: 10,
      maxWidth: 380,
      animation: "fadeInUp 0.2s ease",
    }}>
      <span style={{ fontSize: 18 }}>✅</span>
      <div>
        <div style={{ fontSize: 12, opacity: 0.85 }}>Replay saved</div>
        <div style={{ fontSize: 13 }}>{filename}</div>
      </div>
      <button
        onClick={onDone}
        style={{ marginLeft: "auto", background: "none", border: "none", color: "#fff",
                 cursor: "pointer", fontSize: 16, padding: "0 4px" }}
      >×</button>
    </div>
  );
}

export default function App() {
  const [view,          setView]         = useState<View>("recorder");
  const [settings,      setSettings]     = useState<Settings>(DEFAULT_SETTINGS);
  const [status,        setStatus]       = useState("");
  const [hwEncoder,     setHwEncoder]    = useState("h264_nvenc");
  const [replayToast,   setReplayToast]  = useState<string | null>(null); // filename of saved replay

  useEffect(() => {
    getSettings().then(setSettings).catch(console.error);
    detectHwEncoder().then(setHwEncoder).catch(() => {});

    // Listen for tray, hotkey, and replay events
    const unlisteners = [
      listen("tray-start-recording", () => setView("recorder")),
      listen("hotkey-open-library",  () => setView("library")),
      listen<string>("replay-saved", (e) => {
        // Show the toast with just the filename
        const filename = e.payload.split(/[/\\]/).pop() ?? e.payload;
        setReplayToast(filename);
      }),
    ];

    return () => { unlisteners.forEach(u => u.then(f => f())); };
  }, []);

  // Persist a partial settings change immediately
  const handleSettingsChange = useCallback(async (partial: Partial<Settings>) => {
    setSettings(prev => {
      const next = { ...prev, ...partial };
      // Fire-and-forget save — errors are silent to avoid interrupting recording
      saveSettings(next).catch(console.error);
      return next;
    });
  }, []);

  const handleSaveSettings = useCallback(async (s: Settings) => {
    await saveSettings(s);
    setSettings(s);
    // Re-register hotkeys with the new bindings — only here, not on every
    // incremental save, so rapid RecorderView changes don't spam reregister.
    await applyHotkeys().catch(console.error);
    setStatus("Settings saved.");
    setTimeout(() => setStatus(""), 2000);
  }, []);

  return (
    <div className="app">
      <nav className="nav">
        {(["recorder", "library", "settings"] as View[]).map(v => (
          <button
            key={v}
            className={`nav-btn ${view === v ? "active" : ""}`}
            onClick={() => setView(v)}
          >
            {v === "recorder" ? "⏺ Record" : v === "library" ? "📁 Library" : "⚙ Settings"}
          </button>
        ))}
      </nav>

      <div className="view">
        {/* Keep RecorderView mounted always so recording state isn't lost on tab switch */}
        <div style={{ display: view === "recorder" ? "block" : "none" }}>
          <RecorderView
            settings={settings}
            hwEncoder={hwEncoder}
            onStatus={setStatus}
            onSettingsChange={handleSettingsChange}
          />
        </div>
        {view === "library"  && <LibraryView onStatus={setStatus} />}
        {view === "settings" && <SettingsView settings={settings} onSave={handleSaveSettings} />}
      </div>

      <div className="status-bar" style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {status || "Ready"}
        </span>
        {status && /error|failed|cannot|invalid/i.test(status) && (
          <button
            title="Copy error to clipboard"
            onClick={() => navigator.clipboard.writeText(status).catch(() => {})}
            style={{
              flexShrink: 0,
              background: "rgba(255,255,255,0.08)",
              border: "1px solid rgba(255,255,255,0.15)",
              borderRadius: 4,
              color: "var(--text-muted)",
              cursor: "pointer",
              fontSize: 11,
              padding: "1px 6px",
              lineHeight: "16px",
            }}
          >
            Copy
          </button>
        )}
      </div>

      {/* Replay saved toast — appears for 3 seconds over everything */}
      {replayToast && (
        <ReplaySavedToast
          filename={replayToast}
          onDone={() => setReplayToast(null)}
        />
      )}

      {/* Debug Panel — always available in bottom-right */}
      <DebugPanel />
    </div>
  );
}
