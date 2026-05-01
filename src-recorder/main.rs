/// cliplite-recorder — standalone recording process
///
/// Architecture:
///   cliplite.exe (Tauri/WebView2)
///       ↓  spawns
///   cliplite-recorder.exe  ←→ IPC pipe  ←→  cliplite.exe
///       ↓  spawns
///   ffmpeg.exe + WASAPI capture thread
///
/// This binary has ZERO Tauri/WebView2 dependency. It handles the entire
/// FFmpeg pipeline and WASAPI audio capture. Because it's a plain native
/// exe with no GPU renderer attached, it doesn't compete with ddagrab
/// for the D3D11 device — eliminating the 460ms stall pattern.
///
/// IPC protocol (stdin/stdout, newline-delimited JSON):
///   Tauri → Recorder:  {"cmd":"start", "args":{...}}
///                      {"cmd":"stop"}
///                      {"cmd":"pause"}
///                      {"cmd":"resume"}
///                      {"cmd":"status"}
///   Recorder → Tauri:  {"event":"started",  "path":"..."}
///                      {"event":"stopped",  "path":"..."}
///                      {"event":"status",   "is_recording":bool, "elapsed":f64}
///                      {"event":"error",    "message":"..."}

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

mod recorder;
mod audio;
mod ffmpeg;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Start { args: StartArgs },
    Stop,
    Pause,
    Resume,
    Status,
}

#[derive(Debug, Deserialize)]
pub struct StartArgs {
    pub ffmpeg_path:      String,
    pub output_path:      String,
    pub region:           Region,
    pub audio_cfg:        AudioConfig,
    pub enc_cfg:          EncConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Region {
    pub x:       i32,
    pub y:       i32,
    pub width:   i32,
    pub height:  i32,
    pub monitor: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    pub mic_device:    Option<String>,
    pub sys_device_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EncConfig {
    pub fps:         u32,
    pub quality_crf: u32,
    pub format:      String,
    pub hw_encoder:  String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Started  { path: String },
    Stopped  { path: String },
    Status   { is_recording: bool, is_paused: bool, elapsed: f64 },
    Error    { message: String },
}

fn emit(event: &Event) {
    let json = serde_json::to_string(event).unwrap_or_default();
    println!("{json}");
    std::io::stdout().flush().ok();
}

fn main() {
    // The recorder process is intentionally minimal — no Tauri, no WebView2,
    // no GPU renderer. This ensures it doesn't compete with ddagrab's D3D11.

    let state = Arc::new(Mutex::new(recorder::RecorderInner::default()));

    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };

        let cmd: Command = match serde_json::from_str(&line) {
            Ok(c) => c,
            Err(e) => {
                emit(&Event::Error { message: format!("Parse error: {e}: {line}") });
                continue;
            }
        };

        let mut rec = state.lock().unwrap();

        match cmd {
            Command::Start { args } => {
                // Start system audio capture (Windows WASAPI loopback)
                #[cfg(windows)]
                if let Some(ref device_id) = args.audio_cfg.sys_device_id {
                    match audio::SysAudioCapture::start(device_id.clone()) {
                        Ok(cap) => rec.sys_audio = Some(cap),
                        Err(e) => eprintln!("[recorder] sys audio failed: {e}"),
                    }
                }

                // Build FFmpeg command
                let sys_fmt = {
                    #[cfg(windows)]
                    { rec.sys_audio.as_ref().map(|c| c.format.clone()) }
                    #[cfg(not(windows))]
                    { None::<audio::AudioFormat> }
                };

                let cmd_args = ffmpeg::build_record_command(
                    std::path::Path::new(&args.ffmpeg_path),
                    &args.region,
                    &args.audio_cfg,
                    &args.enc_cfg,
                    &args.output_path,
                    sys_fmt.as_ref(),
                );

                let path = args.output_path.clone();
                match rec.start(&cmd_args, path.clone()) {
                    Ok(_)  => emit(&Event::Started { path }),
                    Err(e) => emit(&Event::Error   { message: e.to_string() }),
                }
            }

            Command::Stop => {
                match rec.stop() {
                    Ok(path) => emit(&Event::Stopped { path }),
                    Err(e)   => emit(&Event::Error   { message: e.to_string() }),
                }
            }

            Command::Pause => {
                if let Err(e) = rec.pause() {
                    emit(&Event::Error { message: e.to_string() });
                }
            }

            Command::Resume => {
                if let Err(e) = rec.resume() {
                    emit(&Event::Error { message: e.to_string() });
                }
            }

            Command::Status => {
                let s = rec.status();
                emit(&Event::Status {
                    is_recording: s.is_recording,
                    is_paused:    s.is_paused,
                    elapsed:      s.elapsed_seconds,
                });
            }
        }
    }
}
