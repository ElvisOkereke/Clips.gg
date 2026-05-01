mod commands;
mod recorder;
mod audio;
mod library;
mod settings;
mod ffmpeg;
mod tray;

use std::sync::Mutex;
use tauri::{Manager, Emitter};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Cached hardware encoder name, detected once at startup.
#[derive(Default)]
pub struct EncoderCache(pub Mutex<String>);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn")
    ).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed { return; }
                    handle_shortcut(app, shortcut);
                })
                .build()
        )
        .plugin(tauri_plugin_notification::init())
        .manage(recorder::RecorderState::default())
        .manage(library::LibraryState::default())
        .manage(settings::SettingsState::default())
        .manage(EncoderCache::default())
        .invoke_handler(tauri::generate_handler![
            commands::find_ffmpeg,
            commands::detect_hw_encoder,
            commands::start_recording,
            commands::stop_recording,
            commands::pause_recording,
            commands::resume_recording,
            commands::get_recording_status,
            commands::start_replay,
            commands::stop_replay,
            commands::save_replay,
            commands::list_audio_devices,
            commands::list_system_audio_devices,
            commands::get_clips,
            commands::delete_clip,
            commands::update_clip_tags,
            commands::add_clip,
            commands::get_settings,
            commands::save_settings,
            commands::apply_hotkeys,
            commands::get_monitors,
            commands::open_path,
            commands::trim_clip,
            commands::generate_thumbnail,
        ])
        .setup(|app| {
            library::init_db().expect("Failed to initialize database");

            let settings = settings::Settings::load();
            *app.state::<settings::SettingsState>().0.lock().unwrap() = settings.clone();

            tray::setup_tray(app)?;

            // Register hotkeys from settings
            register_hotkeys(app, &settings.hotkeys)?;

            // Detect hardware encoder once at startup
            let app_handle = app.handle().clone();
            let hw_pref   = settings.hw_encoder.clone();
            std::thread::spawn(move || {
                let encoder = if hw_pref == "auto" {
                    ffmpeg::detect_hw_encoder()
                } else {
                    hw_pref
                };
                log::info!("Encoder detected: {encoder}");
                *app_handle.state::<EncoderCache>().inner().0.lock().unwrap() = encoder;
            });

            if let Some(win) = app.get_webview_window("main") {
                win.show().ok();
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } = event {
                if label == "main" {
                    api.prevent_close();
                    if let Some(win) = app.get_webview_window("main") {
                        win.hide().ok();
                    }
                }
            }
        });
}

/// Register all hotkeys from the settings map.
fn register_hotkeys(
    app: &mut tauri::App,
    hotkeys: &std::collections::HashMap<String, String>,
) -> anyhow::Result<()> {
    for (action, key_str) in hotkeys {
        if key_str.is_empty() { continue; }
        if let Ok(shortcut) = parse_shortcut(key_str) {
            if let Err(e) = app.global_shortcut().register(shortcut) {
                log::warn!("Failed to register hotkey '{key_str}' for '{action}': {e}");
            }
        }
    }
    Ok(())
}

/// Re-register all hotkeys from an AppHandle (used after settings are saved).
/// Unregisters each known shortcut individually before re-registering.
pub fn reregister_hotkeys(
    app:     &tauri::AppHandle,
    hotkeys: &std::collections::HashMap<String, String>,
) {
    let gs = app.global_shortcut();

    // Unregister existing shortcuts one-by-one (unregister_all is unreliable on Windows)
    for key_str in hotkeys.values() {
        if key_str.is_empty() { continue; }
        if let Ok(shortcut) = parse_shortcut(key_str) {
            let _ = gs.unregister(shortcut);
        }
    }

    // Re-register with new bindings
    for (action, key_str) in hotkeys {
        if key_str.is_empty() { continue; }
        if let Ok(shortcut) = parse_shortcut(key_str) {
            if let Err(e) = gs.register(shortcut) {
                log::warn!("Failed to register hotkey '{key_str}' for '{action}': {e}");
            }
        }
    }
}

/// Parse a shortcut string like "CommandOrControl+Shift+R" into a Shortcut.
pub fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let mut mods = Modifiers::empty();
    let mut code = None;

    for part in &parts {
        match *part {
            "CommandOrControl" | "CmdOrCtrl" | "Ctrl" | "Control" => {
                mods |= Modifiers::CONTROL;
            }
            "Shift" => { mods |= Modifiers::SHIFT; }
            "Alt"   => { mods |= Modifiers::ALT; }
            "Meta" | "Cmd" | "Super" => { mods |= Modifiers::META; }
            key => {
                code = Some(str_to_code(key)?);
            }
        }
    }

    let c = code.ok_or_else(|| format!("No key in '{s}'"))?;
    Ok(Shortcut::new(Some(mods), c))
}

/// Convert a key name string to a Code.
fn str_to_code(s: &str) -> Result<Code, String> {
    match s.to_uppercase().as_str() {
        "A" => Ok(Code::KeyA), "B" => Ok(Code::KeyB), "C" => Ok(Code::KeyC),
        "D" => Ok(Code::KeyD), "E" => Ok(Code::KeyE), "F" => Ok(Code::KeyF),
        "G" => Ok(Code::KeyG), "H" => Ok(Code::KeyH), "I" => Ok(Code::KeyI),
        "J" => Ok(Code::KeyJ), "K" => Ok(Code::KeyK), "L" => Ok(Code::KeyL),
        "M" => Ok(Code::KeyM), "N" => Ok(Code::KeyN), "O" => Ok(Code::KeyO),
        "P" => Ok(Code::KeyP), "Q" => Ok(Code::KeyQ), "R" => Ok(Code::KeyR),
        "S" => Ok(Code::KeyS), "T" => Ok(Code::KeyT), "U" => Ok(Code::KeyU),
        "V" => Ok(Code::KeyV), "W" => Ok(Code::KeyW), "X" => Ok(Code::KeyX),
        "Y" => Ok(Code::KeyY), "Z" => Ok(Code::KeyZ),
        "0" => Ok(Code::Digit0), "1" => Ok(Code::Digit1), "2" => Ok(Code::Digit2),
        "3" => Ok(Code::Digit3), "4" => Ok(Code::Digit4), "5" => Ok(Code::Digit5),
        "6" => Ok(Code::Digit6), "7" => Ok(Code::Digit7), "8" => Ok(Code::Digit8),
        "9" => Ok(Code::Digit9),
        "F1"  => Ok(Code::F1),  "F2"  => Ok(Code::F2),  "F3"  => Ok(Code::F3),
        "F4"  => Ok(Code::F4),  "F5"  => Ok(Code::F5),  "F6"  => Ok(Code::F6),
        "F7"  => Ok(Code::F7),  "F8"  => Ok(Code::F8),  "F9"  => Ok(Code::F9),
        "F10" => Ok(Code::F10), "F11" => Ok(Code::F11), "F12" => Ok(Code::F12),
        "SPACE" => Ok(Code::Space),
        "ENTER" | "RETURN" => Ok(Code::Enter),
        "TAB"   => Ok(Code::Tab),
        "ESCAPE" | "ESC" => Ok(Code::Escape),
        _ => Err(format!("Unknown key: {s}")),
    }
}

/// Called by the global shortcut handler for every registered shortcut press.
fn handle_shortcut(app: &tauri::AppHandle, shortcut: &Shortcut) {
    let settings = app.state::<settings::SettingsState>();
    let hotkeys  = settings.0.lock().unwrap().hotkeys.clone();

    // Find which action this shortcut corresponds to
    let action = hotkeys.iter().find_map(|(action, key_str)| {
        parse_shortcut(key_str).ok().and_then(|s| {
            if s.mods == shortcut.mods && s.key == shortcut.key {
                Some(action.clone())
            } else {
                None
            }
        })
    });

    let Some(action) = action else { return };

    match action.as_str() {
        "start" => {
            // Emit event to frontend to start recording
            app.emit("hotkey-start-recording", ()).ok();
        }
        "stop" => {
            app.emit("hotkey-stop-recording", ()).ok();
        }
        "pause" => {
            app.emit("hotkey-pause-recording", ()).ok();
        }
        "library" => {
            if let Some(win) = app.get_webview_window("main") {
                win.show().ok();
                win.set_focus().ok();
            }
            app.emit("hotkey-open-library", ()).ok();
        }
        "replay_toggle" => {
            app.emit("hotkey-replay-toggle", ()).ok();
        }
        "replay_save" => {
            app.emit("hotkey-replay-save", ()).ok();
        }
        _ => {}
    }
}
