mod commands;
mod recorder;
mod audio;
mod library;
mod settings;
mod ffmpeg;
mod tray;
mod hotkey_listener;

use std::sync::Mutex;
use tauri::Manager;

/// Cached hardware encoder name, detected once at startup.
#[derive(Default)]
pub struct EncoderCache(pub Mutex<String>);

/// Handle to the global hotkey listener thread — wrapped in Option so it can be replaced.
pub struct HotkeyListenerHandle(pub Mutex<Option<hotkey_listener::HotkeyListener>>);

impl Default for HotkeyListenerHandle {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

/// Initialize logging to file and set up panic hook.
fn setup_logging() {
    let log_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".cliplite/debug.log");

    // Create directory if needed
    let log_dir = log_path.parent();
    if let Some(dir) = log_dir {
        let _ = std::fs::create_dir_all(dir);
    }

    // Truncate on each launch so it stays readable
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&log_path)
    {
        Ok(file) => {
            env_logger::Builder::new()
                .target(env_logger::Target::Pipe(Box::new(file)))
                .filter_level(log::LevelFilter::Debug)
                .format_timestamp_millis()
                .init();
        }
        Err(e) => {
            eprintln!("Warning: Could not open log file: {e}");
        }
    }

    // Set panic hook to write to log file
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("[PANIC] {:?}", info);
        log::error!("{msg}");
        
        let log_path = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".cliplite/debug.log");
        
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{msg}");
        }
    }));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    setup_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .manage(recorder::RecorderState::default())
        .manage(library::LibraryState::default())
        .manage(settings::SettingsState::default())
        .manage(EncoderCache::default())
        .manage(HotkeyListenerHandle::default())
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
            commands::get_debug_state,
            commands::simulate_hotkey,
        ])
        .setup(|app| {
            library::init_db().expect("Failed to initialize database");

            let settings = settings::Settings::load();
            *app.state::<settings::SettingsState>().0.lock().unwrap() = settings.clone();

            tray::setup_tray(app)?;

            // Start hotkey listener with global-hotkey crate
            let listener = hotkey_listener::HotkeyListener::start(
                app.handle().clone(),
                settings.hotkeys.clone(),
            );
            *app.state::<HotkeyListenerHandle>().0.lock().unwrap() = Some(listener);

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
                #[cfg(debug_assertions)]
                win.open_devtools();
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

/// Re-register all hotkeys using the new global-hotkey listener.
/// This replaces the old Tauri plugin-based approach.
pub fn reregister_hotkeys(
    app:     &tauri::AppHandle,
    hotkeys: &std::collections::HashMap<String, String>,
) {
    // Stop the old listener if it exists
    if let Ok(mut handle) = app.state::<HotkeyListenerHandle>().0.lock() {
        if let Some(listener) = handle.take() {
            listener.stop_and_wait();
        }
    }

    // Start a new listener with the updated hotkeys
    let listener = hotkey_listener::HotkeyListener::start(
        app.clone(),
        hotkeys.clone(),
    );

    if let Ok(mut handle) = app.state::<HotkeyListenerHandle>().0.lock() {
        *handle = Some(listener);
    }
}
