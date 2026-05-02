import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Settings } from "../types";
import { FPS_OPTIONS, FORMAT_OPTIONS, QUALITY_PRESETS } from "../types";

interface Props {
  settings: Settings;
  onSave: (s: Settings) => void;
}

/** Convert a KeyboardEvent into a hotkey string like "CommandOrControl+Shift+R".
 *  Key names must match what the Rust `parse_hotkey()` function in hotkey_listener.rs expects.
 */
function keyEventToHotkeyString(e: React.KeyboardEvent<HTMLInputElement>): string | null {
  // Ignore bare modifier keys — they can't be hotkeys on their own
  if (["Control", "Shift", "Alt", "Meta", "CapsLock", "NumLock", "ScrollLock",
       "ContextMenu", "OS", "Dead", "Unidentified"].includes(e.key)) {
    return null;
  }

  const parts: string[] = [];
  if (e.ctrlKey || e.metaKey) parts.push("CommandOrControl");
  if (e.shiftKey)             parts.push("Shift");
  if (e.altKey)               parts.push("Alt");

  // Map e.code → Rust parser's expected key name
  const code = e.code;

  // Letters: "KeyA" → "A"
  if (code.startsWith("Key")) { parts.push(code.slice(3)); return parts.join("+"); }
  // Digits: "Digit1" → "1"
  if (code.startsWith("Digit")) { parts.push(code.slice(5)); return parts.join("+"); }
  // Function keys: "F1"–"F12" pass through as-is
  if (/^F\d+$/.test(code)) { parts.push(code); return parts.join("+"); }
  // Numpad: "Numpad0" → "Numpad0", etc.
  if (code.startsWith("Numpad")) { parts.push(code); return parts.join("+"); }

  // Everything else — map to the exact strings the Rust parse_hotkey() handles
  const codeMap: Record<string, string> = {
    "Space":        "Space",
    "Enter":        "Enter",
    "NumpadEnter":  "NumpadEnter",
    "Tab":          "Tab",
    "Escape":       "Escape",
    "Backspace":    "Backspace",
    "Delete":       "Delete",
    "Insert":       "Insert",
    "Home":         "Home",
    "End":          "End",
    "PageUp":       "PageUp",
    "PageDown":     "PageDown",
    "ArrowLeft":    "ArrowLeft",
    "ArrowRight":   "ArrowRight",
    "ArrowUp":      "ArrowUp",
    "ArrowDown":    "ArrowDown",
    "Semicolon":    "Semicolon",
    "Comma":        "Comma",
    "Period":       "Period",
    "Slash":        "Slash",
    "Backslash":    "Backslash",
    "Quote":        "Quote",
    "Backquote":    "Backquote",
    "Equal":        "Equal",
    "Minus":        "Minus",
    "BracketLeft":  "BracketLeft",
    "BracketRight": "BracketRight",
    // Media keys
    "MediaPlayPause":       "MediaPlayPause",
    "MediaStop":            "MediaStop",
    "MediaTrackPrevious":   "MediaTrackPrevious",
    "MediaTrackNext":       "MediaTrackNext",
    "AudioVolumeMute":      "AudioVolumeMute",
    "AudioVolumeDown":      "AudioVolumeDown",
    "AudioVolumeUp":        "AudioVolumeUp",
  };

  const mapped = codeMap[code];
  if (mapped) { parts.push(mapped); return parts.join("+"); }

  // Unknown key — skip
  return null;
}

export function SettingsView({ settings, onSave }: Props) {
  const [s, setS] = useState<Settings>({ ...settings });
  const [capturingKey, setCapturingKey] = useState<string | null>(null); // which action is being captured

  const update = (key: keyof Settings, value: any) =>
    setS(prev => ({ ...prev, [key]: value }));

  const updateHotkey = (action: string, value: string) =>
    setS(prev => ({ ...prev, hotkeys: { ...prev.hotkeys, [action]: value } }));

  const handleKeybindKeyDown = (e: React.KeyboardEvent<HTMLInputElement>, action: string) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      // Escape cancels capture without clearing
      setCapturingKey(null);
      return;
    }
    const hotkey = keyEventToHotkeyString(e);
    if (hotkey) {
      updateHotkey(action, hotkey);
      setCapturingKey(null);
    }
  };

  const browseOutputDir = async () => {
    const dir = await open({ directory: true, defaultPath: s.output_dir });
    if (dir) update("output_dir", dir);
  };

  const browseReplayDir = async () => {
    const dir = await open({ directory: true, defaultPath: s.replay_output_dir || s.output_dir });
    if (dir) update("replay_output_dir", dir);
  };

  const qualityIdx = QUALITY_PRESETS.findIndex(p => p.crf === s.default_quality_crf);

  return (
    <div>
      {/* General */}
      <div className="settings-section">
        <h3>General</h3>
        <div className="card">
          <div className="form-row">
            <span className="form-label">Output folder</span>
            <input type="text" value={s.output_dir} onChange={e => update("output_dir", e.target.value)} style={{ flex: 1 }} />
            <button className="btn btn-secondary btn-sm" style={{ marginLeft: 6 }} onClick={browseOutputDir}>Browse</button>
          </div>

          <div className="form-row">
            <span className="form-label">Filename</span>
            <input type="text" value={s.filename_template} onChange={e => update("filename_template", e.target.value)} />
          </div>

          <div className="form-row">
            <span className="form-label">Format</span>
            <select value={s.default_format} onChange={e => update("default_format", e.target.value)}>
              {FORMAT_OPTIONS.map(f => <option key={f} value={f}>{f.toUpperCase()}</option>)}
            </select>
            <span className="form-label" style={{ marginLeft: 12 }}>FPS</span>
            <select value={s.default_fps} onChange={e => update("default_fps", +e.target.value)}>
              {FPS_OPTIONS.map(f => <option key={f} value={f}>{f}</option>)}
            </select>
          </div>

          <div className="form-row">
            <span className="form-label">Quality</span>
            <select
              value={qualityIdx >= 0 ? qualityIdx : 2}
              onChange={e => update("default_quality_crf", QUALITY_PRESETS[+e.target.value].crf)}
            >
              {QUALITY_PRESETS.map((p, i) => <option key={p.label} value={i}>{p.label}</option>)}
            </select>
          </div>

          <div className="form-row">
            <span className="form-label">Encoder</span>
            <select value={s.hw_encoder} onChange={e => update("hw_encoder", e.target.value)}>
              <option value="auto">Auto-detect</option>
              <option value="h264_nvenc">NVENC (NVIDIA)</option>
              <option value="h264_videotoolbox">VideoToolbox (macOS)</option>
              <option value="h264_vaapi">VAAPI (Linux)</option>
              <option value="libx264">Software (libx264)</option>
            </select>
          </div>

          <div className="checkbox-row">
            <input type="checkbox" id="cursor" checked={s.capture_cursor} onChange={e => update("capture_cursor", e.target.checked)} />
            <label htmlFor="cursor">Capture mouse cursor</label>
          </div>

          <div className="checkbox-row">
            <input type="checkbox" id="tray" checked={s.minimize_to_tray} onChange={e => update("minimize_to_tray", e.target.checked)} />
            <label htmlFor="tray">Minimize to tray when recording</label>
          </div>
        </div>
      </div>

      {/* Replay Buffer */}
      <div className="settings-section">
        <h3>Replay Buffer</h3>
        <div className="card">
          <div className="form-row">
            <span className="form-label" title="Where replay clips are saved. Leave empty to use the same folder as recordings.">
              Save folder
            </span>
            <input
              type="text"
              value={s.replay_output_dir}
              placeholder={`Same as recordings (${s.output_dir || "~/Videos/ClipLite"})`}
              onChange={e => update("replay_output_dir", e.target.value)}
              style={{ flex: 1 }}
            />
            <button className="btn btn-secondary btn-sm" style={{ marginLeft: 6 }} onClick={browseReplayDir}>Browse</button>
          </div>

          <div className="form-row">
            <span className="form-label" title="Filename template for replay clips. Tokens: {datetime} {date} {time}">
              Filename
            </span>
            <input
              type="text"
              value={s.replay_filename_template}
              onChange={e => update("replay_filename_template", e.target.value)}
              placeholder="replay_{datetime}"
            />
          </div>

          <p style={{ fontSize: 10, color: "var(--text-muted)", marginTop: 4 }}>
            Tokens: <code>{"{datetime}"}</code> → 20240101_143022 &nbsp;
            <code>{"{date}"}</code> → 20240101 &nbsp;
            <code>{"{time}"}</code> → 143022
          </p>

          <p style={{ fontSize: 10, color: "var(--text-muted)", marginTop: 2 }}>
            Current save path: <strong>
              {(s.replay_output_dir || s.output_dir || "~/Videos/ClipLite")}
              /{(s.replay_filename_template || "replay_{datetime}")}.mp4
            </strong>
          </p>
        </div>
      </div>

      {/* Hotkeys */}
      <div className="settings-section">
        <h3>Hotkeys</h3>
        <div className="card">
          {[
            ["start",         "Start recording"],
            ["stop",          "Stop recording"],
            ["pause",         "Pause / resume"],
            ["library",       "Open library"],
            ["replay_toggle", "Start / Stop replay buffer"],
            ["replay_save",   "Save replay clip"],
          ].map(([key, label]) => {
            const isCapturing = capturingKey === key;
            return (
              <div className="form-row" key={key}>
                <span className="form-label">{label}</span>
                <input
                  type="text"
                  readOnly
                  value={isCapturing ? "Press keys…" : (s.hotkeys[key] ?? "")}
                  placeholder="Click to set hotkey"
                  title="Click the field then press your desired key combination"
                  style={isCapturing ? { color: "var(--accent-green)", borderColor: "var(--accent-green)", outline: "1px solid var(--accent-green)" } : {}}
                  onFocus={() => setCapturingKey(key)}
                  onBlur={() => setCapturingKey(null)}
                  onKeyDown={e => handleKeybindKeyDown(e, key)}
                />
                {s.hotkeys[key] && (
                  <button
                    className="btn btn-secondary btn-sm"
                    style={{ marginLeft: 4, padding: "1px 6px", fontSize: 11 }}
                    title="Clear hotkey"
                    onMouseDown={e => { e.preventDefault(); updateHotkey(key, ""); }}
                  >×</button>
                )}
              </div>
            );
          })}
          <p style={{ fontSize: 10, color: "var(--text-muted)", marginTop: 4 }}>
            Click a field and press your key combination (e.g. Ctrl+Shift+R). Press Escape to cancel.
          </p>
        </div>
      </div>

      <button className="btn btn-primary" style={{ width: "100%" }} onClick={() => onSave(s)}>
        Save Settings
      </button>
    </div>
  );
}
