/// Tauri command handlers — pure Rust, no Python, no external scripts.
use std::path::PathBuf;
use std::process::Command;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::{
    audio, ffmpeg,
    library::{self, ClipMeta},
    recorder::{RecorderState, RecorderChild, RecordingStatus},
    settings::{Settings, SettingsState},
    tray,
    EncoderCache,
};

// ── Shared types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub x:       i32,
    pub y:       i32,
    pub width:   i32,
    pub height:  i32,
    pub monitor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Legacy dshow name — kept for compatibility, not used for capture.
    pub mic_device:    Option<String>,
    /// WASAPI endpoint ID for system audio loopback.
    pub sys_device_id: Option<String>,
    /// WASAPI endpoint ID for microphone — captured in Rust, no dshow.
    pub mic_device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncConfig {
    pub fps:         u32,
    pub quality_crf: Option<u32>,
    pub format:      Option<String>,
    pub hw_encoder:  Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index:        u32,
    pub name:         String,
    pub x:            i32,
    pub y:            i32,
    pub width:        u32,
    pub height:       u32,
    pub is_primary:   bool,
    pub refresh_rate: f64,
}

// ── FFmpeg ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn find_ffmpeg() -> Result<String, String> {
    ffmpeg::find_ffmpeg()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn detect_hw_encoder() -> String {
    ffmpeg::detect_hw_encoder()
}

// ── Recorder spawn (Win32, no handle inheritance) ─────────────────────────────

/// Spawn cliplite-recorder.exe using Win32 CreateProcessW with
/// PROC_THREAD_ATTRIBUTE_HANDLE_LIST set to ONLY the pipe handles.
///
/// This prevents any D3D11/GPU handles from the Tauri/WebView2 process
/// from being inherited by the recorder subprocess. Without this,
/// the NVIDIA driver sees the recorder as part of the same D3D device
/// family as WebView2, causing 460ms periodic stalls in ddagrab.
#[cfg(windows)]
fn spawn_recorder_isolated(
    recorder_exe: &std::path::Path,
) -> anyhow::Result<RecorderChild> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE, BOOL, SetHandleInformation, HANDLE_FLAG_INHERIT},
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes::CreatePipe,
            Threading::{
                CreateProcessW, InitializeProcThreadAttributeList,
                UpdateProcThreadAttribute, DeleteProcThreadAttributeList,
                LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS,
                EXTENDED_STARTUPINFO_PRESENT, CREATE_NEW_PROCESS_GROUP,
                CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW,
                PROCESS_INFORMATION, STARTUPINFOW, STARTUPINFOEXW,
                SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
                STARTF_USESTDHANDLES,
            },
        },
    };

    // Create inheritable pipe security attributes
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: BOOL(1),
    };

    // stdin pipe: Tauri writes commands → recorder reads
    let mut stdin_read  = HANDLE::default();
    let mut stdin_write = HANDLE::default();
    // stdout pipe: recorder writes events → Tauri reads
    let mut stdout_read  = HANDLE::default();
    let mut stdout_write = HANDLE::default();

    unsafe {
        CreatePipe(&mut stdin_read,  &mut stdin_write, Some(&sa), 0)
            .map_err(|e| anyhow::anyhow!("CreatePipe stdin: {e}"))?;
        CreatePipe(&mut stdout_read, &mut stdout_write, Some(&sa), 0)
            .map_err(|e| anyhow::anyhow!("CreatePipe stdout: {e}"))?;

        // Mark parent-side handles non-inheritable
        // (child only gets stdin_read + stdout_write via the attribute whitelist)
        use windows::Win32::Foundation::HANDLE_FLAGS;
        SetHandleInformation(stdin_write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).ok();
        SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).ok();
    }

    // Build PROC_THREAD_ATTRIBUTE_LIST that whitelists ONLY the two pipe ends
    let mut attr_size: usize = 0;
    unsafe {
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            1, 0, &mut attr_size,
        );
    }

    let mut attr_buf = vec![0u8; attr_size];
    let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);

    unsafe {
        InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size)
            .map_err(|e| anyhow::anyhow!("InitializeProcThreadAttributeList: {e}"))?;
    }

    // PROC_THREAD_ATTRIBUTE_HANDLE_LIST = 0x00020002
    // Only stdin_read and stdout_write cross the process boundary.
    // Every other handle (D3D11 devices, WebView2 GPU contexts) is blocked.
    let inherit_handles: [HANDLE; 2] = [stdin_read, stdout_write];
    const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x00020002;

    unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            Some(inherit_handles.as_ptr() as *const std::ffi::c_void),
            std::mem::size_of_val(&inherit_handles),
            None,
            None,
        ).map_err(|e| anyhow::anyhow!("UpdateProcThreadAttribute: {e}"))?;
    }

    // Build STARTUPINFOEXW
    let mut si_ex = STARTUPINFOEXW::default();
    si_ex.StartupInfo.cb      = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si_ex.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si_ex.StartupInfo.hStdInput  = stdin_read;
    si_ex.StartupInfo.hStdOutput = stdout_write;
    si_ex.StartupInfo.hStdError  = HANDLE::default();
    si_ex.lpAttributeList = attr_list;

    // Build executable path as wide string (CreateProcessW reads from lpApplicationName)
    let exe_wide: Vec<u16> = OsStr::new(recorder_exe)
        .encode_wide().chain([0]).collect();

    let mut pi = PROCESS_INFORMATION::default();
    // CREATE_BREAKAWAY_FROM_JOB is the critical flag.
    // Tauri associates all its child processes with a Windows Job Object for
    // lifecycle management. This Job Object applies GPU scheduling constraints
    // that cause the NVIDIA audio driver to conflict with ddagrab's D3D lock,
    // producing the 460ms stall pattern. BREAKAWAY_FROM_JOB removes the
    // recorder from Tauri's Job Object, giving it the same clean scheduling
    // context as a process spawned directly from a terminal.
    let flags = PROCESS_CREATION_FLAGS(
        EXTENDED_STARTUPINFO_PRESENT.0
        | CREATE_NEW_PROCESS_GROUP.0
        | CREATE_BREAKAWAY_FROM_JOB.0
        | CREATE_NO_WINDOW.0          // suppress console window flash
    );

    unsafe {
        CreateProcessW(
            windows::core::PCWSTR(exe_wide.as_ptr()),
            // lpCommandLine must be PWSTR (mutable); None = use lpApplicationName only
            windows::core::PWSTR::null(),
            None, None,
            BOOL(1), // bInheritHandles — only whitelisted handles cross over
            flags,
            None, None,
            // Cast STARTUPINFOEXW* → STARTUPINFOW* (required with EXTENDED_STARTUPINFO_PRESENT)
            &si_ex.StartupInfo as *const STARTUPINFOW,
            &mut pi,
        ).map_err(|e| anyhow::anyhow!("CreateProcessW: {e}"))?;

        DeleteProcThreadAttributeList(attr_list);
        CloseHandle(stdin_read).ok();
        CloseHandle(stdout_write).ok();
        SetPriorityClass(pi.hProcess, ABOVE_NORMAL_PRIORITY_CLASS).ok();
    }

    Ok(RecorderChild {
        process_handle: pi.hProcess,
        thread_handle:  pi.hThread,
        pid:            pi.dwProcessId,
        stdin_write,
        stdout_read,
    })
}

// ── Find recorder binary ──────────────────────────────────────────────────────

fn find_recorder() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();

    #[cfg(windows)] let name = "cliplite-recorder.exe";
    #[cfg(not(windows))] let name = "cliplite-recorder";

    let p = dir.join(name);
    if p.exists() { Some(p) } else { None }
}

// ── Recording ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_recording(
    app:            AppHandle,
    region:         Region,
    audio_cfg:      AudioConfig,
    enc_cfg:        EncConfig,
    recorder:       State<'_, RecorderState>,
    settings_state: State<'_, SettingsState>,
    encoder_cache:  State<'_, EncoderCache>,
) -> Result<String, String> {
    let settings = settings_state.0.lock().unwrap().clone();

    // Read encoder from cache (detected at startup — no blocking test encode)
    let cached_encoder = encoder_cache.inner().0.lock().unwrap().clone();
    let hw_encoder = {
        let from_enc_cfg = enc_cfg.hw_encoder.as_deref().unwrap_or("auto");
        if from_enc_cfg == "auto" || from_enc_cfg.is_empty() {
            if cached_encoder.is_empty() {
                // Cache not ready yet (startup detection still running)
                if settings.hw_encoder == "auto" {
                    "h264_nvenc".to_string() // optimistic default
                } else {
                    settings.hw_encoder.clone()
                }
            } else {
                cached_encoder
            }
        } else {
            from_enc_cfg.to_string()
        }
    };

    let enc_cfg = EncConfig { hw_encoder: Some(hw_encoder), ..enc_cfg };

    // Build output path
    let ext = match enc_cfg.format.as_deref().unwrap_or("mp4") {
        "webm" => ".webm", "gif" => ".gif", _ => ".mp4",
    };
    let output_path = settings.build_output_path(ext);
    let output_str  = output_path.to_string_lossy().to_string();
    std::fs::create_dir_all(output_path.parent().unwrap_or(&output_path)).ok();

    let ffmpeg_path = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;

    // ── Preferred: spawn recorder with isolated Win32 CreateProcessW ──────────
    // No D3D handle inheritance → no 460ms stall pattern from WebView2's GPU context
    if let Some(recorder_exe) = find_recorder() {
        #[cfg(windows)]
        {
            let mut child = spawn_recorder_isolated(&recorder_exe)
                .map_err(|e| format!("Failed to spawn recorder: {e}"))?;

            // Send start command
            let cmd_json = serde_json::json!({
                "cmd": "start",
                "args": {
                    "ffmpeg_path": ffmpeg_path.to_string_lossy(),
                    "output_path": output_str,
                    "region": {
                        "x": region.x, "y": region.y,
                        "width": region.width, "height": region.height,
                        "monitor": region.monitor
                    },
                    "audio_cfg": {
                        "mic_device":    audio_cfg.mic_device,
                        "sys_device_id": audio_cfg.sys_device_id,
                        "mic_device_id": audio_cfg.mic_device_id,
                    },
                    "enc_cfg": {
                        "fps": enc_cfg.fps,
                        "quality_crf": enc_cfg.quality_crf.unwrap_or(28),
                        "format": enc_cfg.format.as_deref().unwrap_or("mp4"),
                        "hw_encoder": enc_cfg.hw_encoder.as_deref().unwrap_or("h264_nvenc")
                    }
                }
            });

            let line = serde_json::to_string(&cmd_json).unwrap();
            child.send_line(&line).map_err(|e| format!("IPC send failed: {e}"))?;

            // Store child handle — no blocking stdout read here
            {
                let mut rec = recorder.0.lock().unwrap();
                rec.recorder_child = Some(child);
                rec.output_path    = Some(output_str.clone());
                rec.start_time     = Some(std::time::Instant::now());
                rec.paused_duration = 0.0;
                rec.is_paused      = false;
            }

            tray::set_recording_state(&app, true);
            return Ok(output_str);
        }

        // Non-Windows: fall through to std::process path
        #[cfg(not(windows))]
        {
            // On non-Windows just use std::process::Command
            use std::process::{Command, Stdio};
            use std::io::Write;

            let mut proc = Command::new(&recorder_exe)
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("Failed to spawn recorder: {e}"))?;

            let cmd_json = serde_json::json!({
                "cmd": "start",
                "args": {
                    "ffmpeg_path": ffmpeg_path.to_string_lossy(),
                    "output_path": output_str,
                    "region": {"x":region.x,"y":region.y,"width":region.width,"height":region.height,"monitor":region.monitor},
                    "audio_cfg": {"mic_device":audio_cfg.mic_device,"sys_device_id":audio_cfg.sys_device_id},
                    "enc_cfg": {"fps":enc_cfg.fps,"quality_crf":enc_cfg.quality_crf.unwrap_or(28),"format":enc_cfg.format.as_deref().unwrap_or("mp4"),"hw_encoder":enc_cfg.hw_encoder.as_deref().unwrap_or("libx264")}
                }
            });
            if let Some(stdin) = &mut proc.stdin {
                let line = serde_json::to_string(&cmd_json).unwrap() + "\n";
                stdin.write_all(line.as_bytes()).ok();
                stdin.flush().ok();
            }
            {
                let mut rec = recorder.0.lock().unwrap();
                rec.recorder_proc  = Some(proc);
                rec.output_path    = Some(output_str.clone());
                rec.start_time     = Some(std::time::Instant::now());
                rec.paused_duration = 0.0;
                rec.is_paused      = false;
            }
            tray::set_recording_state(&app, true);
            return Ok(output_str);
        }
    }

    // ── Error: recorder binary not found ─────────────────────────────────────
    // We do NOT fall back to direct FFmpeg — that produces choppy recordings.
    // The user needs to have cliplite-recorder.exe next to cliplite.exe.
    Err(format!(
        "cliplite-recorder.exe not found next to cliplite.exe.\n\
         Both files must be in the same directory.\n\
         Expected: {}",
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("cliplite-recorder.exe").to_string_lossy().into_owned()))
            .unwrap_or_default()
    ))
}

#[tauri::command]
pub async fn stop_recording(
    app:      AppHandle,
    recorder: State<'_, RecorderState>,
) -> Result<String, String> {
    let path = {
        let mut rec = recorder.0.lock().unwrap();

        // ── Win32 recorder child path ─────────────────────────────────────
        if let Some(mut child) = rec.recorder_child.take() {
            let output_path = rec.output_path.take().unwrap_or_default();
            rec.start_time = None;
            rec.is_paused  = false;

            // 1. Send stop command
            child.send_line("{\"cmd\":\"stop\"}").ok();

            // 2. Close stdin → sends EOF to recorder's BufReader → main loop exits
            child.close_stdin();

            // 3. Read "stopped" event from stdout
            let mut final_path = output_path.clone();
            if let Some(line) = child.read_line() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if v["event"] == "stopped" {
                        final_path = v["path"].as_str().unwrap_or(&output_path).to_string();
                    }
                }
            }

            // 4. Wait for recorder to exit (writes moov atom then returns)
            let t0 = std::time::Instant::now();
            loop {
                if child.try_wait().is_some() { break; }
                if t0.elapsed().as_secs() > 25 { child.kill(); break; }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }

            // child drops here, remaining handles closed via Drop
            final_path
        }
        // ── std::process::Child recorder path (non-Windows fallback) ──────
        else if let Some(mut proc) = rec.recorder_proc.take() {
            use std::io::Write;
            let output_path = rec.output_path.take().unwrap_or_default();
            rec.start_time = None;

            if let Some(stdin) = proc.stdin.as_mut() {
                let _ = stdin.write_all(b"{\"cmd\":\"stop\"}\n");
                let _ = stdin.flush();
            }
            drop(proc.stdin.take());

            let mut final_path = output_path;
            if let Some(stdout) = proc.stdout.take() {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(stdout);
                let mut line = String::new();
                for _ in 0..10 {
                    line.clear();
                    if reader.read_line(&mut line).is_err() { break; }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                        if v["event"] == "stopped" {
                            final_path = v["path"].as_str().unwrap_or("").to_string();
                            break;
                        }
                    }
                }
            }
            let t0 = std::time::Instant::now();
            loop {
                if let Ok(Some(_)) = proc.try_wait() { break; }
                if t0.elapsed().as_secs() > 25 { let _ = proc.kill(); break; }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            final_path
        }
        // ── Direct FFmpeg fallback ─────────────────────────────────────────
        else {
            rec.stop().map_err(|e| e.to_string())?
        }
    };

    tray::set_recording_state(&app, false);

    // Add to library in background
    let path_clone = path.clone();
    if !path_clone.is_empty() && std::path::Path::new(&path_clone).exists() {
        let p = path_clone.clone();
        std::thread::spawn(move || {
            if let Ok(meta) = probe_file(&p) {
                if let Ok(clip) = library::add_clip_to_db(&p, &meta) {
                    if let Ok(ffmpeg) = ffmpeg::find_ffmpeg() {
                        if let Ok(thumb) = generate_thumb(&ffmpeg, &p, clip.id) {
                            let _ = library::update_thumbnail(clip.id, &thumb);
                        }
                    }
                }
            }
        });
    }

    Ok(path_clone)
}

#[tauri::command]
pub async fn pause_recording(recorder: State<'_, RecorderState>) -> Result<(), String> {
    recorder.0.lock().unwrap().pause().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resume_recording(recorder: State<'_, RecorderState>) -> Result<(), String> {
    recorder.0.lock().unwrap().resume().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_recording_status(recorder: State<'_, RecorderState>) -> RecordingStatus {
    recorder.0.lock().unwrap().status()
}

// ── Replay buffer ─────────────────────────────────────────────────────────────
//
// The replay buffer runs as a background recording in the same recorder process.
// The recorder binary keeps a rolling temp file; SaveReplay extracts the last
// N seconds using FFmpeg's -sseof (seek from end of file).

/// Send a raw JSON command line to the recorder child (Win32 or std path).
fn send_to_recorder(rec: &crate::recorder::RecorderInner, line: &str) -> bool {
    use std::io::Write;
    #[cfg(windows)]
    if let Some(child) = &rec.recorder_child {
        return child.send_line(line).is_ok();
    }
    if let Some(proc) = &rec.recorder_proc {
        // For std::process::Child we can't easily write without &mut
        // Fall through to false — replay only works with Win32 path
    }
    false
}

#[tauri::command]
pub async fn start_replay(
    app:            AppHandle,
    region:         Region,
    audio_cfg:      AudioConfig,
    enc_cfg:        EncConfig,
    recorder:       State<'_, RecorderState>,
    settings_state: State<'_, SettingsState>,
    encoder_cache:  State<'_, EncoderCache>,
) -> Result<String, String> {
    let settings       = settings_state.0.lock().unwrap().clone();
    let cached_encoder = encoder_cache.inner().0.lock().unwrap().clone();
    let hw_encoder     = if cached_encoder.is_empty() { "h264_nvenc".into() } else { cached_encoder };
    let enc_cfg        = EncConfig { hw_encoder: Some(hw_encoder), ..enc_cfg };
    let ffmpeg_path    = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;

    let cmd_json = serde_json::json!({
        "cmd": "start_replay",
        "args": {
            "ffmpeg_path": ffmpeg_path.to_string_lossy(),
            "output_path": "",   // recorder generates its own temp path
            "region": {
                "x": region.x, "y": region.y,
                "width": region.width, "height": region.height,
                "monitor": region.monitor
            },
            "audio_cfg": {
                "mic_device":    audio_cfg.mic_device,
                "sys_device_id": audio_cfg.sys_device_id,
                "mic_device_id": audio_cfg.mic_device_id,
            },
            "enc_cfg": {
                "fps": enc_cfg.fps,
                "quality_crf": enc_cfg.quality_crf.unwrap_or(28),
                "format": enc_cfg.format.as_deref().unwrap_or("mp4"),
                "hw_encoder": enc_cfg.hw_encoder.as_deref().unwrap_or("h264_nvenc")
            }
        }
    });
    let line = serde_json::to_string(&cmd_json).unwrap();

    // If a recorder child is already running (main recording active), reuse it
    #[cfg(windows)]
    {
        let rec = recorder.0.lock().unwrap();
        if let Some(child) = &rec.recorder_child {
            child.send_line(&line).map_err(|e| e.to_string())?;
            return Ok("replay_started".into());
        }
    }

    // No recorder running — spawn one just for the replay buffer.
    // This lets the replay buffer work independently without a main recording.
    if let Some(recorder_exe) = find_recorder() {
        #[cfg(windows)]
        {
            let mut child = spawn_recorder_isolated(&recorder_exe)
                .map_err(|e| format!("Failed to spawn recorder: {e}"))?;

            child.send_line(&line).map_err(|e| format!("IPC send failed: {e}"))?;

            // Store as the recorder child so stop_replay / save_replay can find it
            // Mark output_path empty so stop_recording doesn't try to save it as a clip
            {
                let mut rec = recorder.0.lock().unwrap();
                rec.recorder_child = Some(child);
                rec.output_path    = None;  // replay-only — no main recording path
            }
            return Ok("replay_started".into());
        }
    }

    Err("cliplite-recorder.exe not found".into())
}

#[tauri::command]
pub async fn stop_replay(recorder: State<'_, RecorderState>) -> Result<(), String> {
    let mut rec = recorder.0.lock().unwrap();

    #[cfg(windows)]
    {
        // Is the recorder child shared with a main recording or replay-only?
        let is_replay_only = rec.output_path.is_none() && rec.start_time.is_none();

        if is_replay_only {
            // Replay-only mode: take the child out, stop it completely, discard it.
            // This prevents the "handle is invalid" error when stop → start again,
            // because the next StartReplay will spawn a fresh child.
            if let Some(mut child) = rec.recorder_child.take() {
                child.send_line("{\"cmd\":\"stop_replay\"}").ok();
                child.send_line("{\"cmd\":\"stop\"}").ok();
                // Give the recorder a moment to write its StopReplay event
                std::thread::sleep(std::time::Duration::from_millis(200));
                child.close_stdin();
                // child drops here, all handles closed via Drop
            } else {
                return Err("Replay buffer not running".into());
            }
        } else {
            // Shared with a main recording — just send stop_replay, keep the child alive
            if let Some(child) = &rec.recorder_child {
                child.send_line("{\"cmd\":\"stop_replay\"}").map_err(|e| e.to_string())?;
            } else {
                return Err("Recorder process not running".into());
            }
        }
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        let is_replay_only = rec.output_path.is_none() && rec.start_time.is_none();
        if let Some(mut proc) = if is_replay_only { rec.recorder_proc.take() } else { None } {
            use std::io::Write;
            if let Some(stdin) = proc.stdin.as_mut() {
                let _ = stdin.write_all(b"{\"cmd\":\"stop_replay\"}\n");
                let _ = stdin.write_all(b"{\"cmd\":\"stop\"}\n");
                let _ = stdin.flush();
            }
            drop(proc.stdin.take());
            proc.wait().ok();
        } else if let Some(proc) = &rec.recorder_proc {
            use std::io::Write;
            if let Some(stdin) = unsafe { &mut *(proc as *const _ as *mut std::process::Child) }.stdin.as_mut() {
                let _ = stdin.write_all(b"{\"cmd\":\"stop_replay\"}\n");
                let _ = stdin.flush();
            }
        } else {
            return Err("Recorder process not running".into());
        }
        return Ok(());
    }
}

#[tauri::command]
pub async fn save_replay(
    app:            AppHandle,
    secs:           u32,
    recorder:       State<'_, RecorderState>,
    settings_state: State<'_, SettingsState>,
) -> Result<String, String> {
    let settings   = settings_state.0.lock().unwrap().clone();
    let output_str = settings.build_replay_output_path(".mp4").to_string_lossy().to_string();

    let cmd = serde_json::json!({
        "cmd": "save_replay",
        "secs": secs,
        "output_path": output_str,
    });

    let line = serde_json::to_string(&cmd).unwrap();
    let rec  = recorder.0.lock().unwrap();

    #[cfg(windows)]
    if let Some(child) = &rec.recorder_child {
        child.send_line(&line).map_err(|e| e.to_string())?;
        // Loop until we receive replay_saved or error — intermediate lines
        // (e.g. buffered status events) are discarded so we never miss the response.
        loop {
            let resp = match child.read_line() {
                Some(r) => r,
                None => return Err("Recorder process closed pipe unexpectedly".into()),
            };
            let v = match serde_json::from_str::<serde_json::Value>(&resp) {
                Ok(v) => v,
                Err(_) => continue, // malformed line — keep reading
            };
            if v["event"] == "replay_saved" {
                let path = v["path"].as_str().unwrap_or("").to_string();
                let filename = std::path::Path::new(&path)
                    .file_name().unwrap_or_default()
                    .to_string_lossy().to_string();
                // Emit event so the frontend toast appears
                app.emit("replay-saved", &path).ok();
                // Update tray tooltip so user sees confirmation even when minimised
                if let Some(tray) = app.tray_by_id("main-tray") {
                    let _ = tray.set_tooltip(Some(&format!("Replay saved: {filename}")));
                }
                // Fire OS notification so the user is notified even when the window is hidden
                use tauri_plugin_notification::NotificationExt;
                let _ = app.notification()
                    .builder()
                    .title("Replay saved")
                    .body(&filename)
                    .show();
                return Ok(path);
            } else if v["event"] == "error" {
                return Err(v["message"].as_str().unwrap_or("save failed").to_string());
            }
            // Any other event (e.g. "status") — skip and keep waiting
        }
    }

    #[cfg(not(windows))]
    let _ = (line, rec);

    Err("Recorder process not running".into())
}

// ── Audio ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_input_devices()
}

#[tauri::command]
pub fn list_system_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_output_devices()
}

// ── Monitors ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_monitors() -> Vec<MonitorInfo> {
    #[cfg(windows)] return enumerate_monitors_windows();
    #[cfg(not(windows))] return vec![MonitorInfo {
        index:0, name:"Display 1".into(), x:0, y:0, width:1920, height:1080,
        is_primary:true, refresh_rate:60.0,
    }];
}

#[cfg(windows)]
fn enumerate_monitors_windows() -> Vec<MonitorInfo> {
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MONITORINFOEXW, HDC, HMONITOR};
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};

    struct Context { monitors: Vec<MonitorInfo> }

    unsafe extern "system" fn callback(hmon: HMONITOR, _hdc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        let ctx = &mut *(data.0 as *mut Context);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
            return BOOL(1);
        }
        let r = info.monitorInfo.rcMonitor;
        let is_primary = (info.monitorInfo.dwFlags & 1) != 0;
        let name_end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        let name = String::from_utf16_lossy(&info.szDevice[..name_end]);

        let mut dm = windows::Win32::Graphics::Gdi::DEVMODEW::default();
        dm.dmSize = std::mem::size_of::<windows::Win32::Graphics::Gdi::DEVMODEW>() as u16;
        let dev_name: Vec<u16> = info.szDevice.iter().cloned().collect();
        let refresh = if windows::Win32::Graphics::Gdi::EnumDisplaySettingsW(
            windows::core::PCWSTR(dev_name.as_ptr()),
            windows::Win32::Graphics::Gdi::ENUM_CURRENT_SETTINGS,
            &mut dm,
        ).as_bool() { dm.dmDisplayFrequency as f64 } else { 60.0 };

        ctx.monitors.push(MonitorInfo {
            index: ctx.monitors.len() as u32, name: name.trim_matches('\0').into(),
            x: r.left, y: r.top, width: (r.right-r.left) as u32, height: (r.bottom-r.top) as u32,
            is_primary, refresh_rate: refresh,
        });
        BOOL(1)
    }

    let mut ctx = Context { monitors: vec![] };
    unsafe { EnumDisplayMonitors(HDC::default(), None, Some(callback), LPARAM(&mut ctx as *mut _ as isize)); }
    ctx.monitors
}

// ── Library ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_clips(search: String) -> Result<Vec<library::Clip>, String> {
    library::get_all_clips(&search).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_clip(clip_id: i64, delete_file: bool) -> Result<(), String> {
    let (filepath, thumbnail) = library::delete_clip_from_db(clip_id).map_err(|e| e.to_string())?;
    if delete_file {
        for p in [&filepath, &thumbnail] {
            if !p.is_empty() && std::path::Path::new(p).exists() {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn update_clip_tags(clip_id: i64, tags: String) -> Result<(), String> {
    library::update_tags_in_db(clip_id, &tags).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_clip(filepath: String) -> Result<library::Clip, String> {
    let meta = probe_file(&filepath).map_err(|e| e.to_string())?;
    let mut clip = library::add_clip_to_db(&filepath, &meta).map_err(|e| e.to_string())?;
    if let Ok(ffmpeg) = ffmpeg::find_ffmpeg() {
        if let Ok(thumb) = generate_thumb(&ffmpeg, &filepath, clip.id) {
            let _ = library::update_thumbnail(clip.id, &thumb);
            clip.thumbnail = thumb;
        }
    }
    Ok(clip)
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_settings(state: State<'_, SettingsState>) -> Settings {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(settings: Settings, state: State<'_, SettingsState>) -> Result<(), String> {
    settings.save().map_err(|e| e.to_string())?;
    *state.0.lock().unwrap() = settings;
    Ok(())
}

/// Re-register global hotkeys from the current settings.
/// Called explicitly after the user confirms changes in SettingsView —
/// NOT on every incremental save (which fires for every slider/dropdown change).
#[tauri::command]
pub fn apply_hotkeys(app: AppHandle, state: State<'_, SettingsState>) -> Result<(), String> {
    let hotkeys = state.0.lock().unwrap().hotkeys.clone();
    crate::reregister_hotkeys(&app, &hotkeys);
    Ok(())
}

// ── Utility ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    #[cfg(windows)] Command::new("explorer").arg(&path).spawn().ok();
    #[cfg(target_os="macos")] Command::new("open").arg(&path).spawn().ok();
    #[cfg(target_os="linux")] Command::new("xdg-open").arg(&path).spawn().ok();
    Ok(())
}

#[tauri::command]
pub fn trim_clip(input_path: String, output_path: String, start_time: f64, end_time: f64) -> Result<(), String> {
    let ffmpeg = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
    let ok = Command::new(&ffmpeg)
        .args(["-y", "-ss", &fmt_time(start_time), "-to", &fmt_time(end_time),
               "-i", &input_path, "-c", "copy", &output_path])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false);
    if ok { Ok(()) } else { Err("FFmpeg trim failed".into()) }
}

#[tauri::command]
pub fn generate_thumbnail(filepath: String, clip_id: i64) -> Result<String, String> {
    let ffmpeg = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
    generate_thumb(&ffmpeg, &filepath, clip_id).map_err(|e| e.to_string())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn probe_file(filepath: &str) -> anyhow::Result<ClipMeta> {
    let ffprobe = ffmpeg::find_ffprobe()?;
    let out = Command::new(ffprobe)
        .args(["-v","quiet","-print_format","json","-show_streams","-show_format",filepath])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .output()?;
    let data: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let fmt = &data["format"];
    let video = data["streams"].as_array()
        .and_then(|s| s.iter().find(|s| s["codec_type"] == "video"));
    let (w, h, fps) = video.map(|v| (
        v["width"].as_i64().unwrap_or(0),
        v["height"].as_i64().unwrap_or(0),
        parse_fps(v["r_frame_rate"].as_str().unwrap_or("30/1")),
    )).unwrap_or((0, 0, 30.0));
    Ok(ClipMeta {
        duration_s: fmt["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        width: w, height: h, fps,
        filesize_b: fmt["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
        format: fmt["format_name"].as_str().unwrap_or("").into(),
    })
}

fn parse_fps(s: &str) -> f64 {
    s.split_once('/').map(|(n, d)| {
        let n: f64 = n.parse().unwrap_or(30.0);
        let d: f64 = d.parse().unwrap_or(1.0);
        if d != 0.0 { n / d } else { 30.0 }
    }).unwrap_or_else(|| s.parse().unwrap_or(30.0))
}

fn generate_thumb(ffmpeg: &PathBuf, filepath: &str, clip_id: i64) -> anyhow::Result<String> {
    let dir = dirs::home_dir().unwrap_or_default().join(".cliplite").join("thumbnails");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!("{clip_id}.jpg"));
    Command::new(ffmpeg)
        .args(["-y","-ss","0","-i",filepath,"-frames:v","1","-vf","scale=320:-1",
               out.to_str().unwrap_or("")])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .output()?;
    Ok(out.to_string_lossy().into())
}

fn fmt_time(s: f64) -> String {
    let h = (s / 3600.0) as u64;
    let m = ((s % 3600.0) / 60.0) as u64;
    let sec = s % 60.0;
    format!("{h:02}:{m:02}:{sec:06.3}")
}
