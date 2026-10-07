// Notification-area icon: Open, auto-hide, Settings, Pause, Quit.

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

use crate::island::WINDOW_LABEL;
use crate::Shared;

struct AutoHideMenu(CheckMenuItem<tauri::Wry>);

pub fn sync_auto_hide(app: &AppHandle, enabled: bool) {
    if let Some(item) = app.try_state::<AutoHideMenu>() {
        let _ = item.0.set_checked(enabled);
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Coucou", true, None::<&str>)?;
    let auto_hide_enabled = app.state::<Shared>().settings.lock().unwrap().auto_hide;
    let auto_hide = CheckMenuItem::with_id(
        app,
        "auto_hide",
        "Auto-hide mini header",
        true,
        auto_hide_enabled,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(
        app,
        &[&open, &auto_hide, &sep1, &settings, &pause, &sep2, &quit],
    )?;
    let auto_hide_event = auto_hide.clone();
    app.manage(AutoHideMenu(auto_hide));

    let mut builder = TrayIconBuilder::with_id("coucou")
        .tooltip("Coucou")
        .menu(&menu)
        .on_menu_event(move |app: &AppHandle, event| match event.id.as_ref() {
            "quit" => app.exit(0),
            "settings" => crate::show_settings_window(app),
            "auto_hide" => {
                let enabled = auto_hide_event.is_checked().unwrap_or(true);
                let updated = {
                    let shared = app.state::<Shared>();
                    let mut current = shared.settings.lock().unwrap();
                    current.auto_hide = enabled;
                    let _ = crate::settings::save(&current);
                    current.clone()
                };
                let _ = app.emit("settings-changed", updated);
            }
            id => {
                let _ = app.emit_to(WINDOW_LABEL, "tray", id.to_string());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}
