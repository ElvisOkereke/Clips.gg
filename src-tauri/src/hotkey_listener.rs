use global_hotkey::{
    GlobalHotKeyManager,
    hotkey::{Code, HotKey, Modifiers},
};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    thread,
};
use tauri::{AppHandle, Emitter, Manager};

pub struct HotkeyListener {
    stop_flag: Arc<AtomicBool>,
}

impl HotkeyListener {
    pub fn start(app: AppHandle, hotkeys: HashMap<String, String>) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_flag.clone();

        thread::spawn(move || {
            // Manager MUST be created on this thread (Win32 message pump thread)
            let _manager = match GlobalHotKeyManager::new() {
                Ok(m) => m,
                Err(e) => {
                    log::error!("[hotkeys] Failed to create manager: {e}");
                    return;
                }
            };

            // Build action → event name map
            let action_events: HashMap<&str, &str> = [
                ("start",         "hotkey-start-recording"),
                ("stop",          "hotkey-stop-recording"),
                ("pause",         "hotkey-pause-recording"),
                ("library",       "hotkey-open-library"),
                ("replay_toggle", "hotkey-replay-toggle"),
                ("replay_save",   "hotkey-replay-save"),
                ("annotate",      "hotkey-annotate"),
            ].into_iter().collect();

            // Register hotkeys
            let mut registered_hotkeys: Vec<(u32, String)> = Vec::new();

            for (action, key_str) in &hotkeys {
                if key_str.is_empty() { continue; }
                let Some(&event_name) = action_events.get(action.as_str()) else { continue };
                match parse_hotkey(key_str) {
                    Some(hk) => {
                        let id = hk.id();
                        if let Err(e) = _manager.register(hk) {
                            log::error!("[hotkeys] Failed to register {key_str}: {e}");
                        } else {
                            registered_hotkeys.push((id, event_name.to_string()));
                            log::info!("[hotkeys] Registered {key_str} → {event_name}");
                        }
                    }
                    None => log::warn!("[hotkeys] Could not parse hotkey: {key_str}"),
                }
            }

            log::info!("[hotkeys] Event loop started, waiting for hotkey presses...");
            log::info!("[hotkeys] Registered hotkey IDs: {:?}", registered_hotkeys.iter().map(|(id, _)| id).collect::<Vec<_>>());

            // Use Windows message pump with PeekMessage to check for hotkey events
            #[cfg(windows)]
            {
                use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, DispatchMessageW, MSG};
                use windows::Win32::Foundation::HWND;

                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    
                    loop {
                        if stop_clone.load(Ordering::Relaxed) {
                            log::info!("[hotkeys] Stop flag set, exiting");
                            break;
                        }

                        // Use PeekMessageW to avoid blocking completely
                        let has_msg = PeekMessageW(&mut msg, HWND::default(), 0, 0, windows::Win32::UI::WindowsAndMessaging::PM_REMOVE).as_bool();
                        
                        if has_msg {
                            // Check if it's a WM_HOTKEY message (message id 0x0312)
                            if msg.message == 0x0312 {
                                let hotkey_id = msg.wParam.0 as u32;
                                log::info!("[hotkeys] *** WM_HOTKEY RECEIVED *** id: {}", hotkey_id);
                                
                                // Find which hotkey this is
                                for (id, event_name) in &registered_hotkeys {
                                    if *id == hotkey_id {
                                        log::info!("[hotkeys] FIRED: {event_name}");
                                        // Emit directly to the main webview window so the
                                        // frontend listen() callbacks are guaranteed to fire.
                                        if let Some(win) = app.get_webview_window("main") {
                                            if let Err(e) = win.emit(event_name, ()) {
                                                log::error!("[hotkeys] emit_to main failed: {e}");
                                            } else {
                                                log::info!("[hotkeys] emit_to main OK: {event_name}");
                                            }
                                        } else {
                                            // Window not available (e.g. hidden to tray) — fall back to global
                                            log::warn!("[hotkeys] main window not found, using global emit");
                                            let _ = app.emit(event_name, ());
                                        }
                                        break;
                                    }
                                }
                            }
                            DispatchMessageW(&msg);
                        } else {
                            // No message, sleep briefly to avoid busy-waiting
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                }
            }

            #[cfg(not(windows))]
            {
                loop {
                    if stop_clone.load(Ordering::Relaxed) { break; }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }

            log::info!("[hotkeys] Listener stopped");
        });

        Self { stop_flag }
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    pub fn stop_and_wait(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn parse_hotkey(s: &str) -> Option<HotKey> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let key_str = parts.last()?.to_string();
    let mod_parts = &parts[..parts.len() - 1];

    let mut mods = Modifiers::empty();
    for m in mod_parts {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            "meta" | "super" | "command" | "cmd" => mods |= Modifiers::META,
            _ => {}
        }
    }

    // Strip "Key" or "Digit" prefix if present (from web UI key names)
    let key_normalized = if key_str.starts_with("Key") {
        &key_str[3..]
    } else if key_str.starts_with("Digit") {
        &key_str[5..]
    } else {
        &key_str
    };

    let code = match key_normalized.to_uppercase().as_str() {
        // Letters A-Z
        "A" => Code::KeyA, "B" => Code::KeyB, "C" => Code::KeyC, "D" => Code::KeyD,
        "E" => Code::KeyE, "F" => Code::KeyF, "G" => Code::KeyG, "H" => Code::KeyH,
        "I" => Code::KeyI, "J" => Code::KeyJ, "K" => Code::KeyK, "L" => Code::KeyL,
        "M" => Code::KeyM, "N" => Code::KeyN, "O" => Code::KeyO, "P" => Code::KeyP,
        "Q" => Code::KeyQ, "R" => Code::KeyR, "S" => Code::KeyS, "T" => Code::KeyT,
        "U" => Code::KeyU, "V" => Code::KeyV, "W" => Code::KeyW, "X" => Code::KeyX,
        "Y" => Code::KeyY, "Z" => Code::KeyZ,
        
        // Numbers 0-9
        "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2,
        "3" => Code::Digit3, "4" => Code::Digit4, "5" => Code::Digit5,
        "6" => Code::Digit6, "7" => Code::Digit7, "8" => Code::Digit8,
        "9" => Code::Digit9,
        
        // Function keys
        "F1"  => Code::F1,  "F2"  => Code::F2,  "F3"  => Code::F3,  "F4"  => Code::F4,
        "F5"  => Code::F5,  "F6"  => Code::F6,  "F7"  => Code::F7,  "F8"  => Code::F8,
        "F9"  => Code::F9,  "F10" => Code::F10, "F11" => Code::F11, "F12" => Code::F12,
        
        // Special keys
        "SPACE" | " " => Code::Space,
        "RETURN" | "ENTER" => Code::Enter,
        "TAB" => Code::Tab,
        "ESCAPE" | "ESC" => Code::Escape,
        "BACKSPACE" => Code::Backspace,
        "DELETE" | "DEL" => Code::Delete,
        "INSERT" | "INS" => Code::Insert,
        "HOME" => Code::Home,
        "END" => Code::End,
        "PAGEUP" => Code::PageUp,
        "PAGEDOWN" => Code::PageDown,
        
        // Arrow keys
        "ARROWLEFT" | "LEFT" => Code::ArrowLeft,
        "ARROWRIGHT" | "RIGHT" => Code::ArrowRight,
        "ARROWUP" | "UP" => Code::ArrowUp,
        "ARROWDOWN" | "DOWN" => Code::ArrowDown,
        
        // Numeric keypad
        "NUMPAD0" | "NUM0" => Code::Numpad0,
        "NUMPAD1" | "NUM1" => Code::Numpad1,
        "NUMPAD2" | "NUM2" => Code::Numpad2,
        "NUMPAD3" | "NUM3" => Code::Numpad3,
        "NUMPAD4" | "NUM4" => Code::Numpad4,
        "NUMPAD5" | "NUM5" => Code::Numpad5,
        "NUMPAD6" | "NUM6" => Code::Numpad6,
        "NUMPAD7" | "NUM7" => Code::Numpad7,
        "NUMPAD8" | "NUM8" => Code::Numpad8,
        "NUMPAD9" | "NUM9" => Code::Numpad9,
        "NUMPADADD" | "NUMADD" => Code::NumpadAdd,
        "NUMPADSUBTRACT" | "NUMSUB" => Code::NumpadSubtract,
        "NUMPADMULTIPLY" | "NUMMUL" => Code::NumpadMultiply,
        "NUMPADDIVIDE" | "NUMDIV" => Code::NumpadDivide,
        "NUMPADENTER" | "NUMENTER" => Code::NumpadEnter,
        "NUMPADDECIMAL" | "NUMDECIMAL" => Code::NumpadDecimal,
        
        // Punctuation & symbols
        "SEMICOLON" | ";" => Code::Semicolon,
        "COMMA" | "," => Code::Comma,
        "PERIOD" | "." => Code::Period,
        "SLASH" | "/" => Code::Slash,
        "BACKSLASH" | "\\" => Code::Backslash,
        "QUOTE" | "'" => Code::Quote,
        "BACKTICK" | "`" => Code::Backquote,
        "EQUAL" | "=" => Code::Equal,
        "MINUS" | "-" => Code::Minus,
        "BRACKETLEFT" | "[" => Code::BracketLeft,
        "BRACKETRIGHT" | "]" => Code::BracketRight,
        
        // Media keys
        "MEDIAPLAYPAUSE" => Code::MediaPlayPause,
        "MEDIASTOP" => Code::MediaStop,
        "MEDIATRACKPREVIOUS" => Code::MediaTrackPrevious,
        "MEDIATRACKNEXT" => Code::MediaTrackNext,
        "AUDIOVOLUMEMUTE" => Code::AudioVolumeMute,
        "AUDIOVOLUMEDOWN" => Code::AudioVolumeDown,
        "AUDIOVOLUMEUP" => Code::AudioVolumeUp,
        
        // Modifier keys
        "SHIFTLEFT" | "SHIFT" => Code::ShiftLeft,
        "SHIFTRIGHT" => Code::ShiftRight,
        "CONTROLLEFT" | "CONTROL" => Code::ControlLeft,
        "CONTROLRIGHT" => Code::ControlRight,
        "ALTLEFT" | "ALT" => Code::AltLeft,
        "ALTRIGHT" => Code::AltRight,
        "METALEFT" | "META" => Code::MetaLeft,
        "METARIGHT" => Code::MetaRight,
        
        _ => return None,
    };

    Some(HotKey::new(if mods.is_empty() { None } else { Some(mods) }, code))
}
