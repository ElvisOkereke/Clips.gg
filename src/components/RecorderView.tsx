import { useEffect, useState, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  startRecording, stopRecording, pauseRecording, resumeRecording,
  getRecordingStatus, listAudioDevices, listSystemAudioDevices,
  getMonitors, startReplay, stopReplay, saveReplay,
} from "../api";
import type {
  AudioConfig, AudioDevice, EncConfig, MonitorInfo,
  Region, Settings,
} from "../types";
import { FPS_OPTIONS, FORMAT_OPTIONS, QUALITY_PRESETS as QP } from "../types";

interface Props {
  settings:       Settings;
  hwEncoder:      string;   // from App-level cache — no FFmpeg spawn on tab switch
  onStatus:       (msg: string) => void;
  onSettingsChange: (partial: Partial<Settings>) => void;
}

const REPLAY_OPTIONS = [
  { label: "Off",    secs: 0   },
  { label: "1 min",  secs: 60  },
  { label: "2 min",  secs: 120 },
  { label: "3 min",  secs: 180 },
  { label: "5 min",  secs: 300 },
] as const;

export function RecorderView({ settings, hwEncoder, onStatus, onSettingsChange }: Props) {
  // Devices (loaded once; refreshed via the ↻ button)
  const [micDevices, setMicDevices] = useState<AudioDevice[]>([]);
  const [sysDevices, setSysDevices] = useState<AudioDevice[]>([]);
  const [monitors,   setMonitors]   = useState<MonitorInfo[]>([]);

  // ── Config — initialised from persisted settings ──────────────────────────
  const [selectedMonitor, setSelectedMonitor] = useState(0);
  const [fps,         setFps]        = useState(settings.default_fps);
  const [format,      setFormat]     = useState(settings.default_format);
  const [qualityIdx,  setQualityIdx] = useState(() => {
    const idx = QP.findIndex(p => p.crf === settings.default_quality_crf);
    return idx >= 0 ? idx : 2; // default High
  });
  const [micDevice,   setMicDevice]   = useState<string | null>(null);
  const [micDeviceId, setMicDeviceId] = useState<string | null>(settings.mic_device_id ?? null);
  const [sysDeviceId, setSysDeviceId] = useState<string | null>(settings.sys_audio_device_id ?? null);
  const [replayIdx,   setReplayIdx]   = useState(() => {
    const idx = REPLAY_OPTIONS.findIndex(o => o.secs === settings.replay_buffer_duration_secs);
    return idx >= 0 ? idx : 0;
  });

  // ── Recording state ───────────────────────────────────────────────────────
  const [isRecording,  setIsRecording]  = useState(false);
  const [isPaused,     setIsPaused]     = useState(false);
  const [elapsedSecs,  setElapsedSecs]  = useState(0);
  const [busy,         setBusy]         = useState(false);
  const [replayActive, setReplayActive] = useState(false);
  const [replayBusy,   setReplayBusy]   = useState(false);

  const pollRef       = useRef<number>();
  const startedAtRef  = useRef<number>(0);
  const pausedDurRef  = useRef<number>(0);
  const pauseStartRef = useRef<number>(0);

  // ── Load devices on mount — no detectHwEncoder here (that spawns FFmpeg) ──
  // hwEncoder is passed from App where it's cached at startup.
  useEffect(() => {
    loadDevices();
    getMonitors().then(setMonitors).catch(() => {});
  }, []);

  const loadDevices = () => {
    listAudioDevices().then(devs => {
      setMicDevices(devs);
      // Restore saved mic selection or auto-select first available
      const savedId = settings.mic_device_id;
      if (savedId && devs.find(d => d.id === savedId)) {
        // Restore previously selected mic
        setMicDeviceId(savedId);
        setMicDevice(devs.find(d => d.id === savedId)?.name ?? null);
      } else if (devs.length > 0) {
        // Auto-select first available microphone to ensure audio is captured
        const firstMic = devs[0];
        setMicDeviceId(firstMic.id);
        setMicDevice(firstMic.name);
      }
    }).catch(() => {});

    listSystemAudioDevices().then(devs => {
      setSysDevices(devs);
      // Restore saved sys audio or pick best non-GPU device
      const savedId = settings.sys_audio_device_id;
      if (savedId && devs.find(d => d.id === savedId)) {
        setSysDeviceId(savedId);
      } else {
        const nonGpu = devs.find(d =>
          !isGpuAudioDevice(d.name) && (d.kind === "output" || d.kind === "loopback")
        );
        const any = devs.find(d => d.kind === "output" || d.kind === "loopback");
        const preferred = nonGpu ?? any;
        if (preferred) setSysDeviceId(preferred.id);
      }
    }).catch(() => {});
  };

  // ── Persist settings when config changes ──────────────────────────────────
  const persist = useCallback((changes: Partial<Settings>) => {
    onSettingsChange(changes);
  }, [onSettingsChange]);

  // ── Elapsed timer ─────────────────────────────────────────────────────────
  useEffect(() => {
    if (!isRecording) { setElapsedSecs(0); return; }
    const id = window.setInterval(() => {
      if (isPaused) return;
      setElapsedSecs(Math.max(0, (Date.now() - startedAtRef.current - pausedDurRef.current) / 1000));
    }, 250);
    return () => clearInterval(id);
  }, [isRecording, isPaused]);

  // ── Crash detection poll (5s, 3s warmup) ──────────────────────────────────
  useEffect(() => {
    if (!isRecording) return;
    const warmup = window.setTimeout(() => {
      pollRef.current = window.setInterval(async () => {
        try {
          const s = await getRecordingStatus();
          if (!s.is_recording) {
            setIsRecording(false); setIsPaused(false); setElapsedSecs(0);
          }
        } catch { /* ignore */ }
      }, 5000);
    }, 3000);
    return () => { clearTimeout(warmup); clearInterval(pollRef.current); };
  }, [isRecording]);

  // ── Global hotkey refs ────────────────────────────────────────────────────
  // Handlers are defined later in the file, so initialize refs with no-ops.
  // They are updated on every render via the useEffect blocks below, so
  // the listener callbacks always invoke the current handler with current state.
  const handleRecordRef       = useRef<() => void>(() => {});
  const handlePauseResumeRef  = useRef<() => void>(() => {});
  const handleReplayToggleRef = useRef<() => void>(() => {});
  const handleSaveReplayRef   = useRef<() => void>(() => {});

  const elapsedStr = useCallback((secs: number) => {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = Math.floor(secs % 60);
    return `${String(h).padStart(2,"0")}:${String(m).padStart(2,"0")}:${String(s).padStart(2,"0")}`;
  }, []);

  // ── GPU audio detection ───────────────────────────────────────────────────
  const isGpuAudioDevice = (name: string) => {
    const l = name.toLowerCase();
    return l.includes("nvidia") || l.includes("amd") ||
           (l.includes("high definition audio") && !l.includes("realtek")) ||
           l.includes("radeon");
  };
  const isGpuSelected = sysDeviceId
    ? isGpuAudioDevice(sysDevices.find(d => d.id === sysDeviceId)?.name ?? "")
    : false;

  // ── Recording handlers ────────────────────────────────────────────────────
  const buildAudioCfg = (): AudioConfig => ({
    mic_device:    micDevice,
    sys_device_id: sysDeviceId,
    mic_device_id: micDeviceId,
  });

  const buildEncCfg = (): EncConfig => ({
    fps,
    quality_crf: QP[qualityIdx].crf,
    format,
    hw_encoder: hwEncoder,
  });

  const buildRegion = (): Region => {
    const m = monitors[selectedMonitor] ?? { index:0, x:0, y:0, width:1920, height:1080 };
    return { x: m.x??0, y: m.y??0, width: m.width??1920, height: m.height??1080, monitor: selectedMonitor };
  };

  const handleRecord = async () => {
    if (busy) return;
    setBusy(true);
    if (isRecording) {
      setIsRecording(false); setIsPaused(false); setElapsedSecs(0);
      onStatus("Stopping…");
      try {
        const path = await stopRecording();
        onStatus(path ? `Saved: ${path.split(/[/\\]/).pop()}` : "Stopped.");
      } catch (e: any) {
        onStatus(`Stop error: ${e}`);
      } finally { setBusy(false); }
      return;
    }
    onStatus("Starting…");
    try {
      const path = await startRecording(buildRegion(), buildAudioCfg(), buildEncCfg());
      startedAtRef.current = Date.now(); pausedDurRef.current = 0;
      setIsRecording(true); setIsPaused(false);
      onStatus(`Recording → ${path.split(/[/\\]/).pop()}`);
    } catch (e: any) {
      onStatus(`Failed: ${e}`);
    } finally { setBusy(false); }
  };

  const handlePauseResume = async () => {
    if (!isRecording || busy) return;
    try {
      if (isPaused) {
        await resumeRecording();
        pausedDurRef.current += Date.now() - pauseStartRef.current;
        setIsPaused(false); onStatus("Resumed.");
      } else {
        await pauseRecording();
        pauseStartRef.current = Date.now();
        setIsPaused(true); onStatus("Paused.");
      }
    } catch (e: any) { onStatus(`Error: ${e}`); }
  };

  // ── Replay buffer handlers ────────────────────────────────────────────────
  const handleReplayToggle = async () => {
    if (replayBusy || REPLAY_OPTIONS[replayIdx].secs === 0) return;
    setReplayBusy(true);
    try {
      if (replayActive) {
        await stopReplay();
        setReplayActive(false);
        onStatus("Replay buffer stopped.");
      } else {
        await startReplay(buildRegion(), buildAudioCfg(), buildEncCfg());
        setReplayActive(true);
        onStatus(`Replay buffer active (${REPLAY_OPTIONS[replayIdx].label}).`);
      }
    } catch (e: any) {
      onStatus(`Replay error: ${e}`);
    } finally { setReplayBusy(false); }
  };

  const handleSaveReplay = async () => {
    if (!replayActive || replayBusy) return;
    setReplayBusy(true);
    const secs = REPLAY_OPTIONS[replayIdx].secs;
    if (secs === 0) { onStatus("Set a replay duration first."); setReplayBusy(false); return; }
    onStatus("Saving replay…");
    try {
      const path = await saveReplay(secs);
      onStatus(path ? `Replay saved: ${path.split(/[/\\]/).pop()}` : "Replay saved.");
    } catch (e: any) {
      onStatus(`Replay save error: ${e}`);
    } finally { setReplayBusy(false); }
  };

  // ── Keep hotkey refs pointing at latest handlers (runs after every render) ─
  // This is a synchronous assignment, not a useEffect, so refs are always
  // current before any event handler could fire.
  handleRecordRef.current       = handleRecord;
  handlePauseResumeRef.current  = handlePauseResume;
  handleReplayToggleRef.current = handleReplayToggle;
  handleSaveReplayRef.current   = handleSaveReplay;

  // ── Register hotkey listeners exactly once on mount ───────────────────────
  useEffect(() => {
    console.log("[hotkeys] Registering listeners...");
    const events = [
      "hotkey-start-recording",
      "hotkey-stop-recording",
      "hotkey-pause-recording",
      "hotkey-replay-toggle",
      "hotkey-replay-save",
    ] as const;

    const handlers: Record<string, () => void> = {
      "hotkey-start-recording": () => { console.log("[hotkeys] start-recording fired"); handleRecordRef.current(); },
      "hotkey-stop-recording":  () => { console.log("[hotkeys] stop-recording fired");  handleRecordRef.current(); },
      "hotkey-pause-recording": () => { console.log("[hotkeys] pause-recording fired"); handlePauseResumeRef.current(); },
      "hotkey-replay-toggle":   () => { console.log("[hotkeys] replay-toggle fired");   handleReplayToggleRef.current(); },
      "hotkey-replay-save":     () => { console.log("[hotkeys] replay-save fired");     handleSaveReplayRef.current(); },
    };

    const promises = events.map(name =>
      listen(name, () => handlers[name]())
        .then(unlisten => { console.log(`[hotkeys] ✓ registered ${name}`); return unlisten; })
        .catch(err => { console.error(`[hotkeys] ✗ FAILED to register ${name}:`, err); return () => {}; })
    );

    return () => { promises.forEach(p => p.then(f => f())); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Render ────────────────────────────────────────────────────────────────
  return (
    <div>
      {/* Capture */}
      <div className="card">
        <div className="card-title">Capture</div>
        <div className="form-row">
          <span className="form-label">Monitor</span>
          <select value={selectedMonitor} onChange={e => setSelectedMonitor(+e.target.value)} disabled={isRecording}>
            {monitors.length > 0
              ? monitors.map((m, i) => <option key={i} value={i}>{m.name || `Display ${i+1}`} ({m.width}×{m.height})</option>)
              : <option value={0}>Display 1 (1920×1080)</option>}
          </select>
        </div>
        <div className="form-row">
          <span className="form-label">FPS</span>
          <select value={fps} onChange={e => { const v=+e.target.value; setFps(v); persist({ default_fps: v }); }} disabled={isRecording}>
            {FPS_OPTIONS.map(f => <option key={f} value={f}>{f}</option>)}
          </select>
        </div>
      </div>

      {/* Audio */}
      <div className="card">
        <div className="card-title" style={{ display:"flex", justifyContent:"space-between", alignItems:"center" }}>
          Audio
          <button
            className="btn btn-secondary"
            style={{ padding:"1px 8px", fontSize:11 }}
            title="Re-scan audio devices"
            onClick={loadDevices}
            disabled={isRecording}
          >↻</button>
        </div>

        <div className="form-row">
          <span className="form-label">Microphone</span>
          <select
            value={micDeviceId ?? ""}
            onChange={e => {
              const id = e.target.value || null;
              setMicDeviceId(id);
              const dev = micDevices.find(d => d.id === id);
              setMicDevice(dev?.name ?? null);
              persist({ mic_device_id: id ?? undefined, mic_device: dev?.name ?? undefined });
            }}
            disabled={isRecording}
          >
            <option value="">None</option>
            {micDevices.map(d => <option key={d.id} value={d.id}>{d.name}</option>)}
          </select>
        </div>

        <div className="form-row">
          <span className="form-label" title="Captures speaker output. Avoid NVIDIA/GPU audio — causes dropped frames.">
            System audio
          </span>
          <select
            value={sysDeviceId ?? ""}
            onChange={e => {
              const id = e.target.value || null;
              setSysDeviceId(id);
              persist({ sys_audio_device_id: id ?? undefined });
            }}
            disabled={isRecording}
            style={isGpuSelected ? { borderColor:"#cc6600", color:"#cc6600" } : {}}
          >
            <option value="">None</option>
            {sysDevices.map(d => (
              <option key={d.id} value={d.id}>
                {d.name}{d.kind === "loopback" ? " [Loopback]" : ""}{isGpuAudioDevice(d.name) ? " ⚠ GPU" : ""}
              </option>
            ))}
          </select>
        </div>
        {isGpuSelected && (
          <div style={{ fontSize:10, color:"#cc6600", marginTop:-4, marginBottom:4, padding:"0 4px" }}>
            ⚠ GPU audio shares hardware with the capture engine and may cause dropped frames.
          </div>
        )}
      </div>

      {/* Output */}
      <div className="card">
        <div className="card-title">Output</div>
        <div className="form-row">
          <span className="form-label">Format</span>
          <select value={format} onChange={e => { setFormat(e.target.value); persist({ default_format: e.target.value }); }} disabled={isRecording}>
            {FORMAT_OPTIONS.map(f => <option key={f} value={f}>{f.toUpperCase()}</option>)}
          </select>
          <span className="form-label" style={{ marginLeft:12 }}>Quality</span>
          <select
            value={qualityIdx}
            onChange={e => { const i=+e.target.value; setQualityIdx(i); persist({ default_quality_crf: QP[i].crf }); }}
            disabled={isRecording}
          >
            {QP.map((p, i) => <option key={p.label} value={i}>{p.label}</option>)}
          </select>
        </div>
        <div className="form-row" style={{ fontSize:11, color:"var(--text-muted)" }}>
          <span className="form-label">Encoder</span>
          <span style={{ color:"var(--text)" }}>{hwEncoder}</span>
        </div>
      </div>

      {/* Replay buffer */}
      <div className="card">
        <div className="card-title">Replay Buffer</div>
        <div className="form-row">
          <span className="form-label" title="Keep a rolling buffer of recent footage. Hit 'Save Replay' to clip the last N minutes instantly.">
            Duration
          </span>
          <select
            value={replayIdx}
            onChange={e => { const i=+e.target.value; setReplayIdx(i); persist({ replay_buffer_duration_secs: REPLAY_OPTIONS[i].secs }); }}
            disabled={isRecording && replayActive}
          >
            {REPLAY_OPTIONS.map((o, i) => <option key={o.label} value={i}>{o.label}</option>)}
          </select>

          <button
            className={`btn btn-sm ${replayActive ? "btn-replay active" : "btn-replay"}`}
            style={{ marginLeft:8 }}
            onClick={handleReplayToggle}
            disabled={replayBusy || REPLAY_OPTIONS[replayIdx].secs === 0}
            title={REPLAY_OPTIONS[replayIdx].secs === 0
              ? "Select a duration above to enable replay buffer"
              : replayActive
              ? "Stop replay buffer"
              : "Start replay buffer — records continuously in background"}
          >
            {replayActive ? "⏹ Stop" : "⏺ Start"}
          </button>

          <button
            className="btn btn-primary btn-sm"
            style={{ marginLeft:4 }}
            onClick={handleSaveReplay}
            disabled={!replayActive || replayBusy}
            title={replayActive
              ? `Save the last ${REPLAY_OPTIONS[replayIdx].label} to a clip`
              : "Start the replay buffer first"}
          >
            💾 Save
          </button>
        </div>
        {replayActive && (
          <div style={{ fontSize:10, color:"var(--accent-green)", marginTop:-4, padding:"0 4px" }}>
            ● Replay buffer recording ({REPLAY_OPTIONS[replayIdx].label})
          </div>
        )}
      </div>

      {/* Controls */}
      <div style={{ display:"flex", gap:8, marginBottom:8 }}>
        <button
          className={`btn btn-record ${isRecording ? "recording" : ""}`}
          onClick={handleRecord}
          disabled={busy}
        >
          {busy
            ? (isRecording ? "Stopping…" : "Starting…")
            : isRecording ? "■  Stop Recording" : "●  Start Recording"}
        </button>
        {isRecording && (
          <button className="btn btn-secondary" onClick={handlePauseResume} disabled={busy}>
            {isPaused ? "▶ Resume" : "⏸ Pause"}
          </button>
        )}
      </div>

      <div className="timer" style={isPaused ? { color:"var(--text-muted)" } : {}}>
        {elapsedStr(isRecording ? elapsedSecs : 0)}
      </div>
    </div>
  );
}
