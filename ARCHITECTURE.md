# ClipLite — Architecture & Technical Reference

> **Stack:** Tauri v2.10 · Rust 1.77 · React 18 / TypeScript · Windows  
> **Last updated:** 2026-05-02  
> **Status:** All features working, including global hotkeys.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Repository Layout](#2-repository-layout)
3. [Process Architecture](#3-process-architecture)
4. [Rust Modules — `cliplite.exe`](#4-rust-modules--clipliteexe)
5. [Rust Binary — `cliplite-recorder.exe`](#5-rust-binary--cliplite-recorderexe)
6. [React Frontend](#6-react-frontend)
7. [Global Hotkey System](#7-global-hotkey-system)
8. [IPC Protocol](#8-ipc-protocol)
9. [Settings & Persistent State](#9-settings--persistent-state)
10. [Critical Windows Details](#10-critical-windows-details)
11. [FFmpeg Command Reference](#11-ffmpeg-command-reference)
12. [Build Instructions](#12-build-instructions)
13. [Debugging Checklist](#13-debugging-checklist)
14. [Known Pitfalls](#14-known-pitfalls)

---

## 1. Overview

ClipLite is a Tauri v2 desktop screen recorder with a **two-process design**:

- **`cliplite.exe`** — Tauri UI process. Hosts WebView2 (React) and all Tauri commands. Never touches FFmpeg or WASAPI directly during recording.
- **`cliplite-recorder.exe`** — Standalone Rust binary. No Tauri, no WebView2, no GPU renderer. Runs the entire FFmpeg pipeline + WASAPI audio capture in isolation.

**Why two processes?** WebView2 opens a D3D11 device that competes with `ddagrab`'s `AcquireNextFrame` GPU lock. The recorder binary has zero GPU attachment, eliminating this interference. It also spawns with `CREATE_BREAKAWAY_FROM_JOB` to exit Tauri's Windows Job Object, which otherwise throttles GPU scheduling.

---

## 2. Repository Layout

```
LW_Clipper/
├── ARCHITECTURE.md          ← This file (architecture + learnings)
├── QUICKSTART.md            ← Build, run, and dev workflow
├── cliplite/                ← Python prototype (abandoned, reference only)
└── cliplite-tauri/          ← Active Tauri app
    ├── src/                 ← React + TypeScript frontend
    │   ├── App.tsx          ← Root: nav, view routing, settings load, event listeners
    │   ├── api.ts           ← Typed invoke() wrappers for all Tauri commands
    │   ├── types.ts         ← Shared TS types (Region, Clip, Settings…)
    │   ├── styles.css       ← Dark theme design system
    │   └── components/
    │       ├── RecorderView.tsx  ← Recorder UI, hotkey listeners, optimistic state
    │       ├── LibraryView.tsx   ← Clip grid, search, delete, trim
    │       ├── TrimDialog.tsx    ← In/out sliders, stream-copy export
    │       ├── SettingsView.tsx  ← All settings + keybind capture UI
    │       └── DebugPanel.tsx    ← Dev-only: backend state, simulate hotkeys
    ├── src-tauri/
    │   ├── Cargo.toml            ← Two binary targets: cliplite + cliplite-recorder
    │   ├── tauri.conf.json       ← Window config, WebView2 browser args
    │   ├── build.rs              ← Embeds Windows DPI/compat manifest via tauri-build
    │   ├── capabilities/
    │   │   └── default.json      ← Tauri v2 ACL — REQUIRED for listen() to work
    │   └── src/
    │       ├── main.rs           ← Entry point for cliplite.exe
    │       ├── lib.rs            ← Tauri builder, plugins, state, setup
    │       ├── commands.rs       ← All #[tauri::command] handlers
    │       ├── hotkey_listener.rs← Win32 WM_HOTKEY message pump thread
    │       ├── ffmpeg.rs         ← FFmpeg discovery and encoder detection
    │       ├── recorder.rs       ← Direct FFmpeg management (fallback path)
    │       ├── audio.rs          ← WASAPI enumeration
    │       ├── library.rs        ← SQLite CRUD (rusqlite, bundled)
    │       ├── settings.rs       ← Settings struct + JSON load/save
    │       ├── tray.rs           ← System tray icon and menu
    │       ├── recorder_bin.rs   ← Entry point for cliplite-recorder.exe
    │       ├── rec_recorder.rs   ← Recorder binary's process management
    │       ├── rec_audio.rs      ← Recorder binary's WASAPI capture
    │       └── rec_ffmpeg.rs     ← Recorder binary's FFmpeg command builder
    ├── package.json
    └── vite.config.ts
```

---

## 3. Process Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  cliplite.exe  (Tauri — 15 MB)                               │
│  Main thread:  Tauri event loop                              │
│  Renderer:     WebView2 (React UI) — --disable-gpu           │
│                                                              │
│  On Record:                                                  │
│    Tauri command → spawn cliplite-recorder.exe               │
│    Send JSON start command via stdin                         │
│    Store RecorderChild handle in RecorderState               │
│                                                              │
│  On Stop:                                                    │
│    Send JSON stop command via stdin                          │
│    Close stdin (EOF → recorder exits its loop)               │
│    Read "stopped" event from stdout                          │
│    Wait for process exit, then add clip to SQLite            │
└──────────────────────┬───────────────────────────────────────┘
                       │ spawns with CREATE_BREAKAWAY_FROM_JOB
                       ▼
┌──────────────────────────────────────────────────────────────┐
│  cliplite-recorder.exe  (Standalone Rust — 0.5 MB)           │
│  No Tauri. No WebView2. No GPU renderer.                     │
│                                                              │
│  Thread "sys-audio":                                         │
│    WASAPI IAudioClient (loopback + mic, event-driven)        │
│    WaitForSingleObject(audio_event, 100ms)                   │
│    f32le PCM → \\.\pipe\cliplite_sysaudio                    │
│                                                              │
│  FFmpeg (CREATE_NO_WINDOW, ABOVE_NORMAL_PRIORITY):           │
│    -f f32le -ar 48000 -ac 2 -i \\.\pipe\cliplite_sysaudio   │
│    -f lavfi -i "ddagrab=output_idx=0:framerate=60"           │
│    -c:v h264_nvenc -cq 28 ...                                │
│    -c:a aac -b:a 128k -movflags +faststart output.mp4       │
└──────────────────────────────────────────────────────────────┘
```

---

## 4. Rust Modules — `cliplite.exe`

### `lib.rs` — Tauri builder
Registers plugins, manages shared state (`RecorderState`, `SettingsState`, `EncoderCache`, `HotkeyListenerHandle`). Close button hides window to tray instead of quitting. Opens DevTools automatically in debug builds.

### `commands.rs` — Tauri IPC commands

**`start_recording`:** Resolves encoder from `EncoderCache` (detected once at startup, non-blocking), builds output path, spawns `cliplite-recorder.exe` via Win32 `CreateProcessW` with handle whitelist (only pipe handles cross the boundary — no D3D handles).

**`stop_recording`:** Sends `{"cmd":"stop"}`, closes stdin (EOF), reads `stopped` event from stdout, waits up to 25s for process exit (FFmpeg needs time to write the `moov` atom). Adds clip to library in background thread.

**`simulate_hotkey`:** Emits a hotkey event directly to the main webview window — used by `DebugPanel` for testing without pressing physical keys.

**`apply_hotkeys`:** Re-registers all hotkeys from current settings. Call after the user saves settings.

> **Critical:** Never use blocking I/O (`read_line`) inside `async fn` Tauri commands on the Tokio thread pool. The stop sequence is structured to do its blocking read synchronously at the end of the async fn.

### `hotkey_listener.rs` — Global hotkey thread
Runs a dedicated Win32 thread with a `PeekMessageW` message pump. Registers hotkeys via `GlobalHotKeyManager` (global-hotkey crate). On `WM_HOTKEY` (0x0312), looks up the event name and calls `win.emit(event_name, ())` targeting the `"main"` webview window specifically (not `app.emit()` globally).

The thread checks a `stop_flag` every 50ms and exits cleanly. Wrapped in `HotkeyListenerHandle` (Mutex<Option<...>>) so it can be replaced when hotkeys are re-registered from settings.

### `settings.rs`
`#[serde(default)]` on the `Settings` struct is critical — without it, adding any new field to the struct causes all settings to reset to defaults (deserialization fails on old JSON files).

### `tray.rs`
`set_recording_state()` updates tray tooltip and menu label. Single left-click toggles window visibility.

---

## 5. Rust Binary — `cliplite-recorder.exe`

Source: `recorder_bin.rs` (entry), `rec_recorder.rs`, `rec_audio.rs`, `rec_ffmpeg.rs`

Reads newline-delimited JSON from stdin. Dispatches to:
- `start` → starts WASAPI capture, spawns FFmpeg
- `stop` → stops audio, sends `q` to FFmpeg stdin, waits for exit
- `pause` / `resume` → thread suspend/resume via ToolHelp32
- `start_replay` → starts a second FFmpeg writing a rolling MKV
- `stop_replay` → stops replay FFmpeg, deletes temp MKV
- `save_replay` → stream-copies last N seconds to final MP4
- `status` → queries process liveness

### WASAPI Capture (`rec_audio.rs`)
Uses `AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`. Sample rate read from `IAudioClient::GetMixFormat()` — never hardcoded.

**Key ordering:** `IAudioClient::Start()` is called **before** `ConnectNamedPipe` blocks. After `ConnectNamedPipe` returns, a 50ms pre-roll discard loop drains pre-buffered audio to eliminate choppy startup.

### Replay Buffer
Uses MKV (not MP4) for the temp file — MKV is seekable while still being written; MP4 requires the `moov` atom at the end and is unreadable mid-recording. Keyframe interval `-g {fps}` ensures accurate seek. Duration tracked with `Instant::now()` — never `ffprobe` on the locked file.

---

## 6. React Frontend

### `App.tsx`
Root component. Manages global state (current view, settings, status bar, replay toast). Event listeners registered once on mount with `[]` deps:
- `"tray-start-recording"` → switch to recorder view
- `"hotkey-open-library"` → switch to library view
- `"replay-saved"` → show `ReplaySavedToast`

### `RecorderView.tsx`
**Always mounted** (uses `display: none` not conditional render) so recording state survives tab switches.

Uses **optimistic UI** — `isRecording` flips immediately on button press, no waiting for backend confirmation. A 5s background poll detects unexpected FFmpeg crashes only (not 1s — frequent polling adds GPU scheduler pressure that interferes with ddagrab).

**Hotkey listeners:** Registered exactly once on mount via `useEffect([], [])`. Handler refs are updated synchronously in the render body on every render so the listeners always call the current handler with current state — no stale closures, no re-registration race.

```tsx
// Refs updated synchronously during render (not in useEffect)
handleRecordRef.current       = handleRecord;
handlePauseResumeRef.current  = handlePauseResume;
handleReplayToggleRef.current = handleReplayToggle;
handleSaveReplayRef.current   = handleSaveReplay;

// Listeners registered once, always call current ref
useEffect(() => {
  const promises = [
    listen("hotkey-start-recording", () => handleRecordRef.current()),
    // ...
  ];
  return () => { promises.forEach(p => p.then(f => f())); };
}, []);
```

### `SettingsView.tsx`
Keybind capture: click field → green highlight → press keys → `onKeyDown` converts `e.code` (layout-independent physical key) to `"CommandOrControl+Shift+R"` format → saves to state. `e.code` not `e.key` ensures the format matches what Rust's `parse_hotkey()` expects.

---

## 7. Global Hotkey System

### How It Works (End to End)

```
User presses Ctrl+Shift+R
    ↓
Windows OS → WM_HOTKEY posted to thread message queue
    ↓
hotkey_listener.rs: PeekMessageW loop detects msg.message == 0x0312
    ↓
id_to_event lookup: 34078756 → "hotkey-start-recording"
    ↓
app.get_webview_window("main").emit("hotkey-start-recording", ())
    ↓
RecorderView.tsx: listen() callback fires → handleRecord()
    ↓
Recording starts
```

### Why `win.emit()` Not `app.emit()`
`app.emit()` is a global broadcast. `listen()` in the JS API registers on the current webview. For events emitted from a background Rust thread, targeting the window directly with `win.emit()` is more reliable. `app.emit()` is fine for events emitted from command handlers (which run in the webview context already).

### Why a Custom Win32 Message Pump
The `tauri-plugin-global-shortcut` plugin registers hotkeys successfully but its handler callback never fires on this system (likely a Windows RAWINPUT/message loop interaction with WebView2). The custom `hotkey_listener.rs` uses raw Win32 `RegisterHotKey` via the `global-hotkey` crate + a dedicated `PeekMessageW` thread, which works reliably.

### Tauri v2 Capabilities — REQUIRED for `listen()`

`listen()` from `@tauri-apps/api/event` requires `core:event:allow-listen` to be granted, even for custom app events. This is enforced by Tauri v2's ACL system at runtime. Without the capabilities file, `listen()` throws:

```
event.listen not allowed. Permissions associated with this command:
core:event:allow-listen, core:event:default
```

**Fix:** `src-tauri/capabilities/default.json` with `"core:default"` (which includes `core:event:default`, bundling `allow-listen`, `allow-emit`, and `allow-unlisten`).

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "shell:allow-open",
    "dialog:default",
    "fs:default",
    "notification:default"
  ]
}
```

> **Note:** The `gen/schemas/capabilities.json` file in the `gen/` directory is auto-generated and empty — it is not the capabilities configuration. The actual config must be in `src-tauri/capabilities/`.

---

## 8. IPC Protocol (Tauri ↔ Recorder)

Newline-terminated JSON on stdin/stdout.

**Tauri → Recorder (stdin):**
```jsonc
{"cmd":"start","args":{"ffmpeg_path":"...","output_path":"...","region":{"x":0,"y":0,"width":2560,"height":1440,"monitor":0},"audio_cfg":{"mic_device":null,"sys_device_id":"...","mic_device_id":null},"enc_cfg":{"fps":60,"quality_crf":28,"format":"mp4","hw_encoder":"h264_nvenc"}}}
{"cmd":"stop"}
{"cmd":"pause"}
{"cmd":"resume"}
{"cmd":"start_replay","args":{...}}
{"cmd":"stop_replay"}
{"cmd":"save_replay","secs":60,"output_path":"..."}
```

**Recorder → Tauri (stdout):**
```jsonc
{"event":"started","path":"/path/to/output.mp4"}
{"event":"stopped","path":"/path/to/output.mp4"}
{"event":"replay_saved","path":"/path/to/replay.mp4"}
{"event":"error","message":"..."}
```

`save_replay` in `commands.rs` loops on `read_line()` until it sees `replay_saved` or `error`, discarding intermediate events. Recorder process lives from start to stop; stdin EOF exits its main loop.

---

## 9. Settings & Persistent State

| File | Contents |
|---|---|
| `~/.cliplite/settings.json` | App settings |
| `~/.cliplite/library.db` | SQLite clip library |
| `~/.cliplite/thumbnails/{id}.jpg` | Clip thumbnails |
| `~/Videos/ClipLite/` | Default output directory |
| `%USERPROFILE%\AppData\Local\Temp\cliplite_replay_{pid}.mkv` | Replay buffer temp file |

**Hotkey string format:** `"Modifier+Modifier+Key"` — e.g. `"CommandOrControl+Shift+R"`. Parsed by `hotkey_listener.rs::parse_hotkey()`. Empty string = unbound.

---

## 10. Critical Windows Details

### GPU Stall Sources (in order of severity)

| Cause | Effect | Fix |
|---|---|---|
| **NVIDIA GPU audio** selected for system audio | 460ms periodic stalls, ~4fps | Use USB/Realtek audio; UI warns and auto-excludes GPU devices |
| `CREATE_BREAKAWAY_FROM_JOB` missing | ~17fps, GPU scheduling throttled inside Tauri's Job Object | Flag set in `spawn_recorder_isolated()` |
| `Sleep()` in audio thread | DWM vsync interference | `WaitForSingleObject(audio_event, 100ms)` — event-driven |
| WebView2 GPU active during record | D3D resource contention | `--disable-gpu` in `tauri.conf.json` |
| HAGS enabled | Scheduler latency | `HKLM:\...\GraphicsDrivers\HwSchMode = 1` (reboot required) |
| No DPI manifest | ddagrab throttled to 17fps | `build.rs` embeds manifest via `tauri_build::WindowsAttributes` |
| dshow mic in FFmpeg | D3D stall identical to NVIDIA GPU audio | Mic captured in Rust via WASAPI, piped as PCM to FFmpeg |

### NVIDIA GPU Audio — The Primary Stall Cause
The NVIDIA audio driver (HDMI/DisplayPort endpoints) and DXGI Desktop Duplication share a GPU kernel lock. WASAPI loopback on an NVIDIA audio endpoint acquires this lock every ~460ms, blocking `AcquireNextFrame`.

**Evidence:**
```
USB speakers (Realtek):  495 frames, 0 stalls, 59.76fps ✅
NVIDIA HDMI audio:        33 frames, catastrophic stalls, 3.98fps ❌
```

### Recorder Spawn — Win32 Handle Whitelist
`CreateProcessW` with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` set to only `[stdin_read, stdout_write]`. Prevents all D3D/GPU handles from the Tauri process crossing into the recorder. `std::process::Command::spawn()` inherits ALL parent handles — do not use it for the recorder.

### Audio Startup Fixes
- **Choppy start:** `IAudioClient::Start()` called before `ConnectNamedPipe`, then 50ms pre-roll discard after connect. Eliminates WASAPI kernel buffer burst.
- **Progressive desync:** `aresample=async=1000` added to FFmpeg audio filter. Corrects the 22ms video/audio start offset within the first few seconds.

### WASAPI Format
Always request `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY`. WASAPI converts from the device's native format to your requested float32. Never trust `wBitsPerSample` from `GetMixFormat()` — `32` can mean float32 OR 24-bit PCM packed in 32-bit containers.

### Named Pipe Temp Path
`std::env::temp_dir()` may return an 8.3 short path (e.g. `C:\Users\AHEEEE~1\...`). FFmpeg cannot read 8.3 paths. Always use `std::env::var("USERPROFILE")` for temp file paths.

### EncoderCache
Hardware encoder detection runs once at startup in a background thread. Cached in `EncoderCache(Mutex<String>)`. `start_recording` reads from cache — never blocks on FFmpeg test encode at record time.

---

## 11. FFmpeg Command Reference

### Full recording (NVENC, 60fps, with system audio)
```bash
ffmpeg -y \
  -f f32le -ar 48000 -ac 2 -thread_queue_size 4096 -i \\.\pipe\cliplite_sysaudio \
  -f lavfi -i "ddagrab=output_idx=0:framerate=60:draw_mouse=1" \
  -map 1:v -map 0:a \
  -c:v h264_nvenc -cq 28 -preset p2 -tune hq -maxrate 16M -bufsize 32M -bf 2 \
  -c:a aac -b:a 128k \
  -af aresample=async=1000 \
  -movflags +faststart output.mp4
```

> **No `hwdownload`** — NVENC accepts d3d11 frames directly from ddagrab. Adding `hwdownload` caps throughput at ~18fps at 1440p.

### Trim (stream copy, no re-encode)
```bash
ffmpeg -y -ss 00:00:05.000 -to 00:00:38.000 -i input.mp4 -c copy output_trimmed.mp4
```

### Thumbnail
```bash
ffmpeg -y -ss 0 -i input.mp4 -frames:v 1 -vf scale=320:-1 thumb.jpg
```

---

## 12. Build Instructions

### Prerequisites
- Rust 1.77+ (`rustup install stable`)
- Node.js 18+ + npm
- `ffmpeg.exe` + `ffprobe.exe` in `cliplite-tauri/src-tauri/bin/`
- Visual Studio Build Tools (MSVC linker)
- WebView2 Runtime (bundled with Windows 10 22H2+)

### Development
```powershell
cd "Y:\CODING PROJECTS\LW_Clipper\cliplite-tauri"
npm install
npx tauri dev    # starts Vite dev server + Rust watcher, auto-reloads on changes
```

> **Important:** Frontend changes (`.tsx`) are applied via HMR immediately. Rust changes trigger a full recompile (~30s). The `dist/` folder is only used for production builds — when running `tauri dev`, Vite serves from memory.

### Production build
```powershell
cd "Y:\CODING PROJECTS\LW_Clipper\cliplite-tauri"
npx tauri build
# Outputs:
#   src-tauri/target/release/cliplite.exe
#   src-tauri/target/release/cliplite-recorder.exe
#   src-tauri/target/release/bundle/nsis/ClipLite_1.0.0_x64-setup.exe
```

### Manual rebuild (without tauri CLI)
```powershell
# 1. Build frontend
npm run build

# 2. Kill running cliplite.exe first (it locks the binary)
Stop-Process -Name "cliplite" -Force -ErrorAction SilentlyContinue

# 3. Build Rust
cargo build --manifest-path src-tauri/Cargo.toml
```

**Both binaries must be in the same directory.** `find_recorder()` looks for `cliplite-recorder.exe` next to `cliplite.exe`.

---

## 13. Debugging Checklist

### Video is choppy / low FPS
1. Is system audio set to a **GPU device** (NVIDIA/AMD)? → Most common cause of 460ms stalls. Switch to USB/Realtek or disable system audio.
2. Is HAGS disabled? → `(Get-ItemProperty HKLM:\SYSTEM\CurrentControlSet\Control\GraphicsDrivers).HwSchMode` should be `1`.
3. Is `cliplite-recorder.exe` next to `cliplite.exe`? → Without it, fallback path runs FFmpeg inside Tauri's Job Object → 17fps.

### Analyze frame stalls
```powershell
ffmpeg -i recording.mp4 -vf "showinfo" -f null NUL 2>&1 |
  Select-String "duration_time" | ForEach-Object {
    if ($_ -match "duration_time:([\d.]+)") { [double]$matches[1] }
  } | Group-Object | Sort-Object { [double]$_.Name }
```

### Hotkeys not working
1. Check `~/.cliplite/debug.log` for `[hotkeys] FIRED:` and `emit_to main OK:` lines.
2. Check browser DevTools console for `[hotkeys] ✓ registered` lines — if you see `✗ FAILED ... core:event:allow-listen`, the `capabilities/default.json` file is missing.
3. Check that `src-tauri/capabilities/default.json` exists with `"core:default"` in permissions.
4. Use the Debug Panel (🐛 button) to simulate hotkeys and verify events reach the frontend.

### Verify DPI manifest is embedded
```powershell
$b = [IO.File]::ReadAllBytes("cliplite.exe")
$t = [Text.Encoding]::UTF8.GetString($b)
$t.Contains("dpiAware")                              # Must be True
$t.Contains("Microsoft.Windows.Common-Controls")     # Must be True
```

### Audio issues
- **Choppy start:** Should be fixed by 50ms pre-roll discard in `rec_audio.rs`. Check ordering: `Start()` must be called before `ConnectNamedPipe`.
- **Progressive desync:** Should be fixed by `aresample=async=1000` in `rec_ffmpeg.rs`. If still occurring with large desync (>1s), try `async=4096`.
- **Wrong pitch:** Read `(*mix_fmt).nSamplesPerSec` from `GetMixFormat()`, never hardcode sample rate.

### Windows Defender locks newly-built exe
Defender scans new executables for 30-120 seconds. Add `target\` to Defender exclusions, or use a temp build dir:
```powershell
$env:CARGO_TARGET_DIR = "$env:TEMP\cliplite_build"
cargo build --release --bin cliplite-recorder
```

---

## 14. Known Pitfalls

| Pitfall | Consequence | Fix |
|---|---|---|
| **NVIDIA/GPU audio for system audio** | 460ms stalls throughout recording | Use USB/Realtek; UI warns + auto-excludes GPU devices |
| `capabilities/default.json` missing | `listen()` throws permission error, hotkeys silently broken | Create `src-tauri/capabilities/default.json` with `"core:default"` |
| `app.emit()` from background thread | Events may not reach webview listeners | Use `app.get_webview_window("main").emit()` instead |
| Hotkey `useEffect` re-registering on state change | Race window where no listener is active | Register once with `[]` deps; use refs for current handlers |
| Frontend changes not reflected in running app | Editing `.tsx` while running compiled exe | Use `npx tauri dev` — HMR applies changes instantly |
| `dist/` stale when running compiled exe | Old frontend code runs despite source changes | `npm run build` then rebuild Rust (or use `tauri dev`) |
| `CREATE_BREAKAWAY_FROM_JOB` missing | Recorder throttled inside Tauri's Job Object → 17fps | Flag set in `spawn_recorder_isolated()` — never remove |
| `hwdownload` in NVENC pipeline | Caps at ~18fps at 1440p | Remove it — NVENC accepts d3d11 directly from ddagrab |
| `Sleep()` in audio thread | DWM vsync interference → stalls | `WaitForSingleObject(audio_event, 100ms)` — event-driven |
| `std::process::Command::spawn()` for recorder | All parent D3D handles inherited by child | Win32 `CreateProcessW` + `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` |
| dshow mic in FFmpeg | D3D stall identical to NVIDIA GPU audio | Capture mic in Rust via WASAPI, pipe PCM to FFmpeg |
| Hardcoded sample rate | Audio pitched wrong | Read from `IAudioClient::GetMixFormat().nSamplesPerSec` |
| `std::env::temp_dir()` for named pipe path | 8.3 short path — FFmpeg can't open it | Use `std::env::var("USERPROFILE")` |
| `#[serde(default)]` missing on Settings | All settings reset when a new field is added | Attribute is on the Settings struct — never remove |
| MP4 for replay buffer temp file | Unreadable while writing (moov at end) | MKV — seekable while open for write |
| `ffprobe` on locked replay MKV | Fails — file is exclusively locked by FFmpeg | Track duration with `Instant::now()` from replay start |
| Blocking `read_line()` in async Tauri command | Tokio thread pool deadlock | Do blocking reads synchronously at end of async fn |
| `detectHwEncoder()` in RecorderView | Spawns FFmpeg on every tab switch (5-15s block) | Call once in `App.tsx` on mount; pass result as prop |
| 1s background poll during recording | GPU scheduler pressure interferes with ddagrab | Poll every 5s minimum (crash detection only) |
