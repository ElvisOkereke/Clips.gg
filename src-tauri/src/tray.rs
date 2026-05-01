/// System tray setup.
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, Emitter, Manager, Runtime,
};

pub fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Open ClipLite", true, None::<&str>)?;
    let start_item = MenuItem::with_id(app, "start", "Start Recording", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_item, &start_item, &quit_item])?;

    TrayIconBuilder::with_id("main-tray")
        .tooltip("ClipLite")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "start" => {
                let _ = app.emit("tray-start-recording", ());
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                let app = tray.app_handle();
                // Single click: show/hide window
                if let Some(win) = app.get_webview_window("main") {
                    if win.is_visible().unwrap_or(false) {
                        let _ = win.hide();
                    } else {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(())
}

/// Update tray icon and menu item label to reflect recording state.
pub fn set_recording_state<R: Runtime>(app: &tauri::AppHandle<R>, is_recording: bool) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let tooltip = if is_recording {
            "ClipLite — Recording…"
        } else {
            "ClipLite"
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }
}
