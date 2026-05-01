/// FFmpeg discovery, process spawning, and command building.
/// Pure Rust — no launcher.exe, no Python, no external shims.
///
/// Why the Tauri exe doesn't need a launcher:
/// The Tauri app itself is built with a proper Windows application manifest
/// (PerMonitorV2 DPI awareness, Windows 10/11 compatibility) via tauri.conf.json.
/// FFmpeg inherits this context as a child process, so ddagrab's AcquireNextFrame
/// delivers frames at the full monitor refresh rate — no shim needed.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use anyhow::{anyhow, Result};

// ── FFmpeg / ffprobe discovery ────────────────────────────────────────────────

/// Locate FFmpeg. Checks `<exe_dir>/bin/ffmpeg[.exe]` first, then PATH.
pub fn find_ffmpeg() -> Result<PathBuf> {
    let dir = bin_dir();

    #[cfg(windows)] let name = "ffmpeg.exe";
    #[cfg(not(windows))] let name = "ffmpeg";

    let bundled = dir.join(name);
    if bundled.exists() { return Ok(bundled); }

    which::which("ffmpeg").map_err(|_| anyhow!(
        "FFmpeg not found.\n\
         Place ffmpeg.exe in the bin/ folder next to the app,\n\
         or install it on PATH.\n\
         Download: https://ffmpeg.org/download.html"
    ))
}

/// Locate ffprobe in the same directory as ffmpeg.
pub fn find_ffprobe() -> Result<PathBuf> {
    let dir = find_ffmpeg()?.parent().unwrap_or(Path::new(".")).to_path_buf();

    #[cfg(windows)] let name = "ffprobe.exe";
    #[cfg(not(windows))] let name = "ffprobe";

    let p = dir.join(name);
    if p.exists() { return Ok(p); }
    which::which("ffprobe").map_err(|_| anyhow!("ffprobe not found"))
}

fn bin_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_default()
        .join("bin")
}

// ── Hardware encoder detection ────────────────────────────────────────────────

pub fn detect_hw_encoder() -> String {
    let ffmpeg = match find_ffmpeg() {
        Ok(p) => p,
        Err(_) => return "libx264".into(),
    };

    let tests: &[(&str, &[&str])] = &[
        ("h264_nvenc", &[
            "-f", "lavfi", "-i", "color=c=black:s=256x256:r=30,format=bgra",
            "-frames:v", "10", "-c:v", "h264_nvenc", "-pix_fmt", "yuv420p",
            "-f", "null", "-",
        ]),
        ("h264_videotoolbox", &[
            "-f", "lavfi", "-i", "nullsrc=s=128x128,format=yuv420p",
            "-frames:v", "5", "-c:v", "h264_videotoolbox", "-f", "null", "-",
        ]),
        ("h264_vaapi", &[
            "-f", "lavfi", "-i", "nullsrc=s=128x128,format=yuv420p",
            "-frames:v", "5", "-c:v", "h264_vaapi", "-f", "null", "-",
        ]),
    ];

    for (enc, args) in tests {
        let ok = Command::new(&ffmpeg)
            .args(*args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok { return enc.to_string(); }
    }
    "libx264".into()
}

// ── Process spawning ──────────────────────────────────────────────────────────

/// Spawn FFmpeg with piped stdin/stderr, no visible console window.
/// On Windows, CREATE_NO_WINDOW suppresses the terminal flash and the
/// process inherits this Tauri exe's desktop context (proper DPI manifest),
/// giving ddagrab full-rate AcquireNextFrame.
pub fn spawn_ffmpeg(cmd_args: &[String]) -> Result<std::process::Child> {
    let exe = cmd_args.first().ok_or_else(|| anyhow!("Empty command"))?;
    let args = &cmd_args[1..];

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let child = Command::new(exe)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn FFmpeg ({exe}): {e}"))?;

        // Elevate to high priority so ddagrab is scheduled promptly
        elevate_priority(child.id());
        return Ok(child);
    }

    #[cfg(not(windows))]
    Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn FFmpeg: {e}"))
}

#[cfg(windows)]
fn elevate_priority(pid: u32) {
    use windows::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, PROCESS_ALL_ACCESS,
    };
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_ALL_ACCESS, false, pid) {
            let _ = SetPriorityClass(h, HIGH_PRIORITY_CLASS);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

// ── FFmpeg command builder ────────────────────────────────────────────────────

pub fn build_record_command(
    ffmpeg:           &Path,
    region:           &crate::commands::Region,
    audio_cfg:        &crate::commands::AudioConfig,
    enc_cfg:          &crate::commands::EncConfig,
    output_path:      &str,
    // Actual WASAPI format from SysAudioCapture — None when no system audio.
    // Used to set correct -ar/-ac on the pipe input so audio isn't pitch-shifted.
    sys_audio_format: Option<&crate::audio::AudioFormat>,
) -> Vec<String> {
    let mut cmd = vec![ffmpeg.to_string_lossy().into_owned(), "-y".into()];

    let fps     = enc_cfg.fps;
    let enc     = enc_cfg.hw_encoder.as_deref().unwrap_or("libx264");
    let crf     = enc_cfg.quality_crf.unwrap_or(28);
    let fmt     = enc_cfg.format.as_deref().unwrap_or("mp4");
    let has_sys = audio_cfg.sys_device_id.is_some();
    let has_mic = audio_cfg.mic_device.is_some();
    let n_audio = usize::from(has_sys) + usize::from(has_mic);
    // GPU encoders (NVENC) accept d3d11 frames directly; CPU needs hwdownload
    let needs_dl = enc == "libx264";

    // ── System audio (Windows named pipe, input index 0) ─────────────────────
    // Use the actual WASAPI device sample rate — not a hardcoded value.
    // Mismatch causes time-stretched (pitched down/up) audio in the recording.
    #[cfg(windows)]
    if has_sys {
        let rate = sys_audio_format.map(|f| f.sample_rate).unwrap_or(48000);
        let ch   = sys_audio_format.map(|f| f.channels as u32).unwrap_or(2);
        cmd.extend([
            "-f".into(), "f32le".into(),
            "-ar".into(), rate.to_string(),
            "-ac".into(), ch.to_string(),
            "-thread_queue_size".into(), "4096".into(),
            "-i".into(), r"\\.\pipe\cliplite_sysaudio".into(),
        ]);
    }

    // ── Video input ───────────────────────────────────────────────────────────
    add_video_input(&mut cmd, region, fps);

    // ── Microphone ────────────────────────────────────────────────────────────
    if let Some(mic) = &audio_cfg.mic_device {
        add_mic_input(&mut cmd, mic);
    }

    // ── Stream mapping ────────────────────────────────────────────────────────
    #[cfg(windows)]
    {
        let vid = if has_sys { 1usize } else { 0 };
        let mic = if has_sys { 2usize } else { 1 };
        let dl  = "hwdownload,format=bgra";

        if n_audio == 2 {
            // Mix sys + mic audio; optionally download video for CPU encode
            let fc = if needs_dl {
                format!("[{vid}:v]{dl}[vout];[0:a][{mic}:a]amix=inputs=2:duration=first:dropout_transition=0[aout]")
            } else {
                format!("[0:a][{mic}:a]amix=inputs=2:duration=first:dropout_transition=0[aout]")
            };
            cmd.extend(["-filter_complex".into(), fc]);
            if needs_dl {
                cmd.extend(["-map".into(), "[vout]".into(), "-map".into(), "[aout]".into()]);
            } else {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-map".into(), "[aout]".into()]);
            }
        } else if has_sys {
            if needs_dl {
                cmd.extend(["-map".into(), "1:v".into(), "-vf".into(), dl.into(), "-map".into(), "0:a".into()]);
            } else {
                cmd.extend(["-map".into(), "1:v".into(), "-map".into(), "0:a".into()]);
            }
        } else if has_mic {
            if needs_dl {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-vf".into(), dl.into(), "-map".into(), format!("{mic}:a")]);
            } else {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-map".into(), format!("{mic}:a")]);
            }
        } else {
            // Video only
            if needs_dl {
                cmd.extend(["-map".into(), format!("{vid}:v"), "-vf".into(), dl.into()]);
            } else {
                cmd.extend(["-map".into(), format!("{vid}:v")]);
            }
        }
    }

    #[cfg(not(windows))]
    {
        if n_audio == 2 {
            cmd.extend([
                "-filter_complex".into(),
                "[0:a][1:a]amix=inputs=2:duration=first:dropout_transition=0[aout]".into(),
                "-map".into(), "0:v".into(), "-map".into(), "[aout]".into(),
            ]);
        } else if n_audio == 1 {
            cmd.extend(["-map".into(), "0:v".into(), "-map".into(), "1:a".into()]);
        } else {
            cmd.extend(["-map".into(), "0:v".into()]);
        }
    }

    // ── Video encoder ─────────────────────────────────────────────────────────
    let maxrate = if region.width >= 2560 { "16M" } else { "8M" };
    let bufsize = if region.width >= 2560 { "32M" } else { "16M" };

    cmd.extend(["-c:v".into(), enc.into()]);
    match enc {
        "h264_nvenc" => cmd.extend([
            "-cq".into(), crf.to_string(),
            "-preset".into(), "p2".into(),
            "-tune".into(), "hq".into(),
            "-maxrate".into(), maxrate.into(),
            "-bufsize".into(), bufsize.into(),
            "-bf".into(), "2".into(),
        ]),
        "libx264" => cmd.extend([
            "-pix_fmt".into(), "yuv420p".into(),
            "-crf".into(), crf.to_string(),
            "-preset".into(), "superfast".into(),
            "-maxrate".into(), maxrate.into(),
            "-bufsize".into(), bufsize.into(),
            "-threads".into(), "0".into(),
        ]),
        _ => cmd.extend(["-pix_fmt".into(), "yuv420p".into(), "-b:v".into(), maxrate.into()]),
    }

    // ── Audio encoder ─────────────────────────────────────────────────────────
    if n_audio > 0 && fmt != "gif" {
        if fmt == "webm" {
            cmd.extend(["-c:a".into(), "libopus".into(), "-b:a".into(), "128k".into()]);
        } else {
            cmd.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "128k".into()]);
        }
    } else {
        cmd.push("-an".into());
    }

    // ── Container ─────────────────────────────────────────────────────────────
    if fmt == "mp4" { cmd.extend(["-movflags".into(), "+faststart".into()]); }

    cmd.push(output_path.into());
    cmd
}

fn add_video_input(cmd: &mut Vec<String>, r: &crate::commands::Region, fps: u32) {
    #[cfg(windows)]
    cmd.extend([
        "-f".into(), "lavfi".into(),
        "-i".into(), format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", r.monitor, fps),
    ]);

    #[cfg(target_os = "linux")]
    {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
        let wayland = std::env::var("XDG_SESSION_TYPE").unwrap_or_default().to_lowercase().contains("wayland");
        if wayland {
            cmd.extend(["-f".into(), "pipewire".into(), "-i".into(), "0".into()]);
        } else {
            cmd.extend([
                "-f".into(), "x11grab".into(),
                "-r".into(), fps.to_string(),
                "-s".into(), format!("{}x{}", r.width, r.height),
                "-i".into(), format!("{}+{},{}", display, r.x, r.y),
            ]);
        }
    }

    #[cfg(target_os = "macos")]
    cmd.extend([
        "-f".into(), "avfoundation".into(),
        "-framerate".into(), fps.to_string(),
        "-i".into(), format!("{}:none", r.monitor),
        "-vf".into(), format!("crop={}:{}:{}:{}", r.width, r.height, r.x, r.y),
    ]);
}

fn add_mic_input(cmd: &mut Vec<String>, device: &str) {
    #[cfg(windows)]
    cmd.extend(["-f".into(), "dshow".into(), "-i".into(), format!("audio={device}")]);
    #[cfg(target_os = "linux")]
    cmd.extend(["-f".into(), "pulse".into(), "-i".into(), device.into()]);
    #[cfg(target_os = "macos")]
    cmd.extend(["-f".into(), "avfoundation".into(), "-i".into(), format!("none:{device}")]);
}
