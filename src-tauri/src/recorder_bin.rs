/// cliplite-recorder — standalone recording process
///
/// IPC protocol (stdin/stdout, newline-delimited JSON):
///   {"cmd":"start",       "args":{...}}
///   {"cmd":"stop"}
///   {"cmd":"pause"} / {"cmd":"resume"} / {"cmd":"status"}
///   {"cmd":"start_replay", "args":{...}}
///   {"cmd":"stop_replay"}
///   {"cmd":"save_replay",  "secs":60, "output_path":"..."}

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

#[path = "rec_recorder.rs"] mod recorder;
#[path = "rec_audio.rs"]   mod audio;
#[path = "rec_ffmpeg.rs"]  mod ffmpeg;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Start       { args: StartArgs },
    Stop,
    Pause,
    Resume,
    Status,
    StartReplay { args: StartArgs },
    StopReplay,
    SaveReplay  { secs: u32, output_path: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartArgs {
    pub ffmpeg_path:  String,
    pub output_path:  String,
    pub region:       Region,
    pub audio_cfg:    AudioConfig,
    pub enc_cfg:      EncConfig,
    /// Named pipe for audio. Recording uses cliplite_sysaudio, replay uses cliplite_sysaudio_replay.
    /// If absent, defaults to cliplite_sysaudio.
    #[serde(default)]
    pub audio_pipe:   Option<String>,
    /// Max size in MB for temporary replay buffer files (0 = unlimited).
    /// When exceeded, oldest temp files are automatically cleaned up.
    #[serde(default)]
    pub max_replay_buffer_size_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Region {
    pub x: i32, pub y: i32, pub width: i32, pub height: i32, pub monitor: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    pub mic_device:    Option<String>,
    pub sys_device_id: Option<String>,
    pub mic_device_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EncConfig {
    pub fps: u32, pub quality_crf: u32, pub format: String, pub hw_encoder: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Started       { path: String },
    Stopped       { path: String },
    ReplayStarted { path: String },
    ReplayStopped,
    ReplaySaved   { path: String },
    Status        { is_recording: bool, is_paused: bool, elapsed: f64, replay_active: bool },
    Error         { message: String },
}

fn emit(event: &Event) {
    let json = serde_json::to_string(event).unwrap_or_default();
    println!("{json}");
    std::io::stdout().flush().ok();
}

/// Get the ClipLite app directory (~/.cliplite).
/// All app files (settings, library, temp files, etc.) are stored here.
fn get_app_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".cliplite")
}

/// Get the replay temp directory (~/.cliplite/temp).
/// Ensures the directory exists.
fn get_replay_temp_dir() -> std::path::PathBuf {
    let tmp_dir = get_app_dir().join("temp");
    std::fs::create_dir_all(&tmp_dir).ok();
    tmp_dir
}

/// Clean up old replay temp files if total size exceeds the limit.
/// Deletes oldest files first until total size is under the limit.
fn cleanup_old_replay_files(max_size_mb: u32) {
    if max_size_mb == 0 {
        return; // unlimited — no cleanup needed
    }

    let tmp_dir = get_replay_temp_dir();

    // Find all cliplite_replay_*.mkv files
    let mut files = match std::fs::read_dir(&tmp_dir) {
        Ok(entries) => {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.is_file() && path.file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.starts_with("cliplite_replay_") && s.ends_with(".mkv"))
                        .unwrap_or(false)
                    {
                        entry.metadata().ok().map(|m| (path, m.modified().ok(), m.len()))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        }
        Err(_) => return,
    };

    // Sort by modification time (oldest first)
    files.sort_by_key(|(_, mtime, _)| *mtime);

    // Calculate total size and delete oldest files until under limit
    let max_bytes = (max_size_mb as u64) * 1024 * 1024;
    let mut total_size: u64 = files.iter().map(|(_, _, size)| size).sum();

    for (path, _, size) in files {
        if total_size <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total_size = total_size.saturating_sub(size);
        }
    }
}

/// Start a recording with optional extra FFmpeg args inserted before the output path.
/// Uses args.audio_pipe to determine which named pipe to use (recording vs replay).

fn do_start_with_extra(
    rec: &mut recorder::RecorderInner,
    args: StartArgs,
    extra: &[&str],
) -> Result<String, String> {
    let pipe = args.audio_pipe.clone()
        .unwrap_or_else(|| r"\\.\pipe\cliplite_sysaudio".to_string());

    // effective_audio_cfg is set to None if audio failed to start,
    // so the FFmpeg command is built without any audio input.
    let effective_audio_cfg;

    #[cfg(windows)]
    {
        let sys_id = args.audio_cfg.sys_device_id.clone();
        let mic_id = args.audio_cfg.mic_device_id.clone();
        if sys_id.is_some() || mic_id.is_some() {
            match audio::SysAudioCapture::start_with_pipe(sys_id, mic_id, pipe.clone()) {
                Ok(Some(cap)) => {
                    rec.sys_audio = Some(cap);
                    effective_audio_cfg = args.audio_cfg.clone();
                }
                Ok(None) => {
                    // Audio device failed — build FFmpeg command without audio
                    eprintln!("[recorder] audio disabled (device failed), recording video only");
                    effective_audio_cfg = AudioConfig { mic_device: None, sys_device_id: None, mic_device_id: None };
                }
                Err(e) => {
                    eprintln!("[recorder] audio thread spawn failed: {e}");
                    effective_audio_cfg = AudioConfig { mic_device: None, sys_device_id: None, mic_device_id: None };
                }
            }
        } else {
            effective_audio_cfg = args.audio_cfg.clone();
        }
    }
    #[cfg(not(windows))]
    { effective_audio_cfg = args.audio_cfg.clone(); }

    let sys_fmt = {
        #[cfg(windows)]      { rec.sys_audio.as_ref().map(|c| c.format.clone()) }
        #[cfg(not(windows))] { None::<audio::AudioFormat> }
    };
    let mut cmd_args = ffmpeg::build_record_command_with_pipe(
        std::path::Path::new(&args.ffmpeg_path),
        &args.region,
        &effective_audio_cfg,
        &args.enc_cfg,
        &args.output_path,
        sys_fmt.as_ref(),
        &pipe,
    );
    // Insert extra args before the output path (last element)
    if !extra.is_empty() {
        if let Some(out) = cmd_args.pop() {
            for e in extra { cmd_args.push(e.to_string()); }
            cmd_args.push(out);
        }
    }
    let path = args.output_path.clone();
    rec.start(&cmd_args, path.clone()).map_err(|e| e.to_string())?;
    Ok(path)
}

fn do_start(rec: &mut recorder::RecorderInner, args: StartArgs) -> Result<String, String> {
    let pipe = args.audio_pipe.clone()
        .unwrap_or_else(|| r"\\.\pipe\cliplite_sysaudio".to_string());

    let effective_audio_cfg;

    #[cfg(windows)]
    {
        let sys_id = args.audio_cfg.sys_device_id.clone();
        let mic_id = args.audio_cfg.mic_device_id.clone();
        if sys_id.is_some() || mic_id.is_some() {
            match audio::SysAudioCapture::start_with_pipe(sys_id, mic_id, pipe.clone()) {
                Ok(Some(cap)) => {
                    rec.sys_audio = Some(cap);
                    effective_audio_cfg = args.audio_cfg.clone();
                }
                Ok(None) => {
                    eprintln!("[recorder] audio disabled (device failed), recording video only");
                    effective_audio_cfg = AudioConfig { mic_device: None, sys_device_id: None, mic_device_id: None };
                }
                Err(e) => {
                    eprintln!("[recorder] audio thread spawn failed: {e}");
                    effective_audio_cfg = AudioConfig { mic_device: None, sys_device_id: None, mic_device_id: None };
                }
            }
        } else {
            effective_audio_cfg = args.audio_cfg.clone();
        }
    }
    #[cfg(not(windows))]
    { effective_audio_cfg = args.audio_cfg.clone(); }

    let sys_fmt = {
        #[cfg(windows)]       { rec.sys_audio.as_ref().map(|c| c.format.clone()) }
        #[cfg(not(windows))]  { None::<audio::AudioFormat> }
    };

    let cmd_args = ffmpeg::build_record_command_with_pipe(
        std::path::Path::new(&args.ffmpeg_path),
        &args.region,
        &effective_audio_cfg,
        &args.enc_cfg,
        &args.output_path,
        sys_fmt.as_ref(),
        &pipe,
    );

    let path = args.output_path.clone();
    rec.start(&cmd_args, path.clone()).map_err(|e| e.to_string())?;
    Ok(path)
}

fn main() {
    let state        = Arc::new(Mutex::new(recorder::RecorderInner::default()));
    let replay_state = Arc::new(Mutex::new(recorder::RecorderInner::default()));
    let replay_path  = Arc::new(Mutex::new(String::new()));

    // Store the ffmpeg path, replay start time, and original StartArgs.
    // We track duration ourselves (not via ffprobe) because the MKV is
    // exclusively locked by the writer FFmpeg — no other process can read it.
    // StartArgs is stored so we can restart the writer after a save.
    let replay_ffmpeg_path  = Arc::new(Mutex::new(String::new()));
    let replay_start_time: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
    let replay_start_args: Arc<Mutex<Option<StartArgs>>> = Arc::new(Mutex::new(None));

    let stdin  = std::io::stdin();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };

        let cmd: Command = match serde_json::from_str(&line) {
            Ok(c)  => c,
            Err(e) => { emit(&Event::Error { message: format!("Parse error: {e}") }); continue; }
        };

        match cmd {
            Command::Start { args } => {
                let mut rec = state.lock().unwrap();
                match do_start(&mut rec, args) {
                    Ok(path) => emit(&Event::Started { path }),
                    Err(msg) => emit(&Event::Error   { message: msg }),
                }
            }
            Command::Stop => {
                let mut rec = state.lock().unwrap();
                match rec.stop() {
                    Ok(path) => emit(&Event::Stopped { path }),
                    Err(e)   => emit(&Event::Error   { message: e.to_string() }),
                }
            }
            Command::Pause => {
                let mut rec = state.lock().unwrap();
                if let Err(e) = rec.pause() { emit(&Event::Error { message: e.to_string() }); }
            }
            Command::Resume => {
                let mut rec = state.lock().unwrap();
                if let Err(e) = rec.resume() { emit(&Event::Error { message: e.to_string() }); }
            }
            Command::Status => {
                let s  = state.lock().unwrap().status();
                let rb = replay_state.lock().unwrap().status();
                emit(&Event::Status {
                    is_recording:  s.is_recording,
                    is_paused:     s.is_paused,
                    elapsed:       s.elapsed_seconds,
                    replay_active: rb.is_recording,
                });
            }

            // ── Replay buffer ─────────────────────────────────────────────────
            Command::StartReplay { args } => {
                // Clean up old temp files if buffer size limit exceeded
                cleanup_old_replay_files(args.max_replay_buffer_size_mb);

                // Store ffmpeg path and full args for use in SaveReplay (restart after save)
                *replay_ffmpeg_path.lock().unwrap() = args.ffmpeg_path.clone();
                *replay_start_args.lock().unwrap()  = Some(args.clone());

                // Use MKV (not MP4) as temp format — readable while FFmpeg writes.
                // Store in ~/.cliplite/temp for easy storage analysis.
                let tmp_dir = get_replay_temp_dir();
                let tmp = tmp_dir.join(format!("cliplite_replay_{}.mkv", std::process::id()));
                let tmp_str = tmp.to_string_lossy().to_string();
                *replay_path.lock().unwrap() = tmp_str.clone();

                // This subprocess is always dedicated to replay — no shared-pipe conflict.
                // audio_pipe is set to cliplite_sysaudio_replay by the parent process.
                let fps = args.enc_cfg.fps;
                let replay_args = StartArgs {
                    output_path: tmp_str.clone(),
                    ..args
                };
                let mut rr = replay_state.lock().unwrap();
                // Build the command then insert -g (keyframe every second).
                match do_start_with_extra(&mut rr, replay_args, &["-g", &fps.to_string()]) {
                    Ok(_) => {
                        // Check if FFmpeg started successfully by waiting briefly
                        // and reading any immediate stderr errors
                        std::thread::sleep(std::time::Duration::from_millis(800));
                        let alive = if let Some(p) = &mut rr.process {
                            matches!(p.try_wait(), Ok(None))
                        } else { false };

                        if alive {
                            // Record when replay started — used in SaveReplay to
                            // compute duration without probing the locked MKV file
                            *replay_start_time.lock().unwrap() = Some(std::time::Instant::now());
                            emit(&Event::ReplayStarted { path: tmp_str });
                        } else {
                            // FFmpeg exited immediately — read its stderr for the error
                            let err_msg = if let Some(mut p) = rr.process.take() {
                                let mut buf = Vec::new();
                                if let Some(mut stderr) = p.stderr.take() {
                                    use std::io::Read;
                                    stderr.read_to_end(&mut buf).ok();
                                }
                                String::from_utf8_lossy(&buf).chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()
                            } else { "FFmpeg exited immediately".into() };
                            *replay_path.lock().unwrap() = String::new();
                            emit(&Event::Error { message: format!("Replay FFmpeg failed: {err_msg}") });
                        }
                    }
                    Err(msg) => emit(&Event::Error { message: msg }),
                }
            }

            Command::StopReplay => {
                let mut rr = replay_state.lock().unwrap();
                match rr.stop() {
                    Ok(path) => {
                        // Delete the temp MKV file
                        if !path.is_empty() {
                            std::fs::remove_file(&path).ok();
                        }
                        *replay_path.lock().unwrap()       = String::new();
                        *replay_ffmpeg_path.lock().unwrap() = String::new();
                        *replay_start_time.lock().unwrap() = None;
                        *replay_start_args.lock().unwrap() = None;
                        emit(&Event::ReplayStopped);
                    }
                    Err(e) => emit(&Event::Error { message: e.to_string() }),
                }
            }

            Command::SaveReplay { secs, output_path } => {
                let src = replay_path.lock().unwrap().clone();
                if src.is_empty() {
                    emit(&Event::Error { message: "Replay buffer not active — start it first".into() });
                    continue;
                }

                // Resolve FFmpeg binary
                let ffmpeg = {
                    let stored = replay_ffmpeg_path.lock().unwrap().clone();
                    if !stored.is_empty() && std::path::Path::new(&stored).exists() {
                        stored
                    } else {
                        let exe_dir = std::env::current_exe()
                            .ok()
                            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                            .unwrap_or_default();
                        let bundled = exe_dir.join("bin").join("ffmpeg.exe");
                        if bundled.exists() { bundled.to_string_lossy().to_string() }
                        else { which::which("ffmpeg").map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "ffmpeg".into()) }
                    }
                };

                // ── How long has the buffer been running? ────────────────────
                let total_duration_secs = replay_start_time.lock().unwrap()
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0);

                if total_duration_secs < 2.0 {
                    emit(&Event::Error { message: format!("Replay buffer too short ({total_duration_secs:.1}s) — wait longer before saving") });
                    continue;
                }

                let want_secs  = secs as f64;
                let start_secs = (total_duration_secs - want_secs).max(0.0);

                // ── STEP 1: Stop the writer so MKV is fully sealed ───────────
                // An MKV being written by FFmpeg cannot be read by another
                // process — EBML header / cluster boundaries are incomplete.
                // We must gracefully stop the writer (sends 'q', waits for
                // moov/EBML finalization), then read the sealed file.
                {
                    let mut rr = replay_state.lock().unwrap();
                    rr.stop().ok(); // stops writer FFmpeg, waits for exit
                }
                *replay_start_time.lock().unwrap() = None;

                // Verify the sealed file is usable
                if !std::path::Path::new(&src).exists() {
                    emit(&Event::Error { message: format!("Replay temp file missing after stop: {src}") });
                    continue;
                }

                // Create output directory
                if let Some(parent) = std::path::Path::new(&output_path).parent() {
                    std::fs::create_dir_all(parent).ok();
                }

                // ── STEP 2: Extract the clip from the sealed MKV ─────────────
                let mut ffmpeg_stderr = String::new();

                #[cfg(windows)]
                let extract_ok = {
                    use std::os::windows::process::CommandExt;
                    const CREATE_NO_WINDOW: u32 = 0x08000000;
                    let mut cmd = std::process::Command::new(&ffmpeg);
                    cmd.args([
                        "-y",
                        "-ss", &format!("{start_secs:.3}"),
                        "-i", &src,
                        "-c:v", "copy",
                        "-c:a", "copy",
                        "-avoid_negative_ts", "make_zero",
                        "-movflags", "+faststart",
                        &output_path,
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .creation_flags(CREATE_NO_WINDOW);
                    
                    match cmd.spawn() {
                        Err(e) => { ffmpeg_stderr = format!("Failed to spawn FFmpeg: {e}"); false }
                        Ok(mut child) => {
                            // Read stderr before waiting to avoid deadlock
                            let stderr_content = if let Some(mut se) = child.stderr.take() {
                                use std::io::Read;
                                let mut buf = String::new();
                                se.read_to_string(&mut buf).ok();
                                buf
                            } else {
                                String::new()
                            };
                            ffmpeg_stderr = stderr_content;
                            child.wait().map(|s| s.success()).unwrap_or(false)
                        }
                    }
                };
                #[cfg(not(windows))]
                let extract_ok = {
                    match std::process::Command::new(&ffmpeg)
                        .args(["-y", "-ss", &format!("{start_secs:.3}"), "-i", &src,
                               "-c:v", "copy", "-c:a", "copy",
                               "-avoid_negative_ts", "make_zero",
                               "-movflags", "+faststart", &output_path])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                    {
                        Ok(s) => s.success(),
                        Err(e) => { ffmpeg_stderr = e.to_string(); false }
                    }
                };

                // Delete the old sealed temp file regardless of success
                std::fs::remove_file(&src).ok();
                *replay_path.lock().unwrap() = String::new();

                // ── STEP 3: Clean up old temp files (don't restart here) ───
                // Restarting the buffer can block and cause app freeze. Let the UI
                // call start_replay again if the user wants to continue recording.
                let saved_args = replay_start_args.lock().unwrap().clone();
                if let Some(restart_args) = saved_args {
                    cleanup_old_replay_files(restart_args.max_replay_buffer_size_mb);
                }

                // Clear the replay args so the buffer is stopped
                *replay_start_args.lock().unwrap() = None;
                *replay_ffmpeg_path.lock().unwrap() = String::new();

                // ── STEP 4: Emit result ───────────────────────────────────────
                if extract_ok {
                    emit(&Event::ReplaySaved { path: output_path });
                } else {
                    let msg = if ffmpeg_stderr.is_empty() {
                        "FFmpeg exited with non-zero status".to_string()
                    } else {
                        format!("FFmpeg error: {ffmpeg_stderr}")
                    };
                    emit(&Event::Error { message: msg });
                }
            }
        }
    }
}
