# ClipLite — Quick Start

## Run in dev mode (recommended)

```powershell
cd "Y:\CODING PROJECTS\LW_Clipper\cliplite-tauri"
npm install          # first time only
npx tauri dev
```

- Frontend (`.tsx`) changes apply instantly via HMR — no restart needed.
- Rust changes trigger an automatic recompile and restart (~30s).
- DevTools open automatically in debug builds.

## Production build

```powershell
cd "Y:\CODING PROJECTS\LW_Clipper\cliplite-tauri"
npx tauri build
```

Outputs to `src-tauri/target/release/`. Both `cliplite.exe` and `cliplite-recorder.exe` must be in the same directory.

## Key files to know

| File | Purpose |
|---|---|
| `src/components/RecorderView.tsx` | Main recorder UI + hotkey listeners |
| `src/components/SettingsView.tsx` | Settings + keybind capture UI |
| `src/App.tsx` | Root: nav, global event listeners, replay toast |
| `src/api.ts` | All `invoke()` wrappers for Tauri commands |
| `src-tauri/src/hotkey_listener.rs` | Win32 WM_HOTKEY message pump |
| `src-tauri/src/commands.rs` | All `#[tauri::command]` handlers |
| `src-tauri/src/lib.rs` | Tauri builder + app setup |
| `src-tauri/capabilities/default.json` | **Required** Tauri v2 ACL permissions |
| `src-tauri/tauri.conf.json` | Window config, WebView2 args |
| `~/.cliplite/debug.log` | Backend log (truncated on each launch) |

## Persistent data locations

| Path | Contents |
|---|---|
| `~/.cliplite/settings.json` | App settings |
| `~/.cliplite/library.db` | Clip library (SQLite) |
| `~/.cliplite/thumbnails/` | Clip thumbnails |
| `~/Videos/ClipLite/` | Default recording output |
| `~/.cliplite/debug.log` | Backend log |

## Default hotkeys

| Action | Default |
|---|---|
| Start recording | `Ctrl+Shift+R` |
| Stop recording | `Ctrl+Shift+S` |
| Pause/resume | `Ctrl+Shift+P` |
| Open library | `Ctrl+Shift+L` |
| Toggle replay buffer | `Alt+F1` |
| Save replay clip | `Alt+F2` |

All hotkeys are configurable in Settings → Save Settings to apply.

## Things that must never be removed

- `CREATE_BREAKAWAY_FROM_JOB` in `commands.rs::spawn_recorder_isolated()` — without it, recording throttles to 17fps inside Tauri's Windows Job Object.
- `#[serde(default)]` on the `Settings` struct in `settings.rs` — without it, all settings reset when any new field is added.
- `src-tauri/capabilities/default.json` with `"core:default"` — without it, `listen()` throws a permission error and hotkeys silently don't work.
- WASAPI event-driven capture (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`) in `rec_audio.rs` — `Sleep()` in the audio thread interferes with DWM vsync.
- MKV format for replay buffer temp file — MP4 is unreadable while FFmpeg is writing it.

## Things to never do

- Don't add `hwdownload` to the NVENC pipeline — it caps at ~18fps at 1440p.
- Don't use dshow for mic capture in FFmpeg — it causes the same GPU kernel lock stalls as NVIDIA audio.
- Don't call `detectHwEncoder()` in `RecorderView` — it spawns FFmpeg on every tab switch.
- Don't poll `getRecordingStatus()` more often than every 5s during recording — it adds GPU scheduler pressure.
- Don't use `std::env::temp_dir()` for named pipe paths — may return an 8.3 short path FFmpeg can't open.
- Don't hardcode audio sample rate as 44100 — read from `IAudioClient::GetMixFormat().nSamplesPerSec`.

## Troubleshooting hotkeys

1. Open Debug Panel (🐛 button, bottom-right of the app).
2. Click a simulate button — if the Recent Events list shows the event, frontend listeners work.
3. Press a physical hotkey — backend log (`~/.cliplite/debug.log`) should show `[hotkeys] FIRED:` and `emit_to main OK:`.
4. If browser console shows `event.listen not allowed` — `src-tauri/capabilities/default.json` is missing or malformed.

## Troubleshooting video stalls

Most common cause: system audio is set to an NVIDIA/GPU audio device. Switch to USB/Realtek audio in the Microphone or System Audio dropdown. The UI tags GPU devices with `⚠ GPU` and shows an orange warning.

See `ARCHITECTURE.md` §10 for the full stall root-cause tree.
