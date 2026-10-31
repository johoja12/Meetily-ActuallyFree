//! macOS menu-bar mode. Closing the window and ⌘Q hide Meetily to the menu bar
//! (no window, no Dock icon) instead of quitting, so meeting detection, calendar
//! listening and automation keep running. The WebView stays loaded: those
//! features listen for native events there. Only the menu-bar icon's Quit (or
//! the Dock's, while the icon shows) ends the app.
//!
//! "Start at login in the menu bar" is a per-user LaunchAgent that opens the
//! app with `--menu-bar`, which starts it hidden. Other platforms keep their
//! close-to-tray behavior and report the setting as unsupported.
use tauri::{AppHandle, Manager, Runtime};

/// Passed by the login item: start with only the menu-bar icon.
pub const LAUNCH_FLAG: &str = "--menu-bar";
#[cfg(target_os = "macos")]
const HIDE_MENU_ID: &str = "hide_to_menu_bar";
#[cfg(target_os = "macos")]
const AGENT_LABEL: &str = "com.meetily.ai.menu-bar";

pub fn launched_for_menu_bar() -> bool {
    std::env::args().any(|arg| arg == LAUNCH_FLAG)
}

/// Hide the main window, and on macOS the Dock icon with it.
pub fn hide_to_menu_bar<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.hide() {
            log::error!("Failed to hide main window: {error}");
        }
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        log::warn!("Could not hide the Dock icon: {error}");
    }
    log::info!("Main window hidden to the menu bar");
}

/// Call before showing the main window: it needs the Dock icon back to take focus.
pub fn show_in_dock<R: Runtime>(_app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    if let Err(error) = _app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        log::warn!("Could not show the Dock icon: {error}");
    }
}

/// The standard app menu with Quit replaced: macOS terminates on the
/// predefined Quit item without an event that could keep the app alive.
#[cfg(target_os = "macos")]
pub fn install_app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItemBuilder, MenuItemKind};
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        for item in app_menu.items()? {
            if let MenuItemKind::Predefined(predefined) = &item {
                if predefined.text()?.starts_with("Quit") {
                    app_menu.remove(predefined)?;
                }
            }
        }
        app_menu.append(
            &MenuItemBuilder::with_id(HIDE_MENU_ID, "Close to Menu Bar")
                .accelerator("CmdOrCtrl+Q")
                .build(app)?,
        )?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id().as_ref() == HIDE_MENU_ID {
            hide_to_menu_bar(app);
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn agent_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join("Library/LaunchAgents").join(format!("{AGENT_LABEL}.plist")))
}

/// The `.app` bundle holding this executable, else the executable itself (dev).
#[cfg(target_os = "macos")]
fn launch_arguments(exe: &std::path::Path) -> Vec<String> {
    let bundle = exe.ancestors().find(|path| path.extension().is_some_and(|ext| ext == "app"));
    match bundle {
        // Through LaunchServices, like a Finder launch; -g keeps it in the background.
        Some(bundle) => vec![
            "/usr/bin/open".into(),
            "-g".into(),
            "-a".into(),
            bundle.display().to_string(),
            "--args".into(),
            LAUNCH_FLAG.into(),
        ],
        None => vec![exe.display().to_string(), LAUNCH_FLAG.into()],
    }
}

#[cfg(target_os = "macos")]
fn agent_plist(arguments: &[String]) -> String {
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let arguments: String = arguments.iter().map(|arg| format!("\n        <string>{}</string>", escape(arg))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>{arguments}
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
    )
}

#[cfg(target_os = "macos")]
fn write_agent() -> Result<(), String> {
    let path = agent_path().ok_or("no home folder")?;
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    std::fs::create_dir_all(path.parent().ok_or("no LaunchAgents folder")?).map_err(|error| error.to_string())?;
    std::fs::write(&path, agent_plist(&launch_arguments(&exe))).map_err(|error| error.to_string())
}

/// An enabled login item is rewritten at startup so it follows a moved app.
pub fn refresh_login_item() {
    #[cfg(target_os = "macos")]
    if agent_path().is_some_and(|path| path.exists()) {
        if let Err(error) = write_agent() {
            log::warn!("Could not refresh the menu-bar login item: {error}");
        }
    }
}

/// None where menu-bar mode is not offered.
#[tauri::command]
pub fn get_menu_bar_login() -> Option<bool> {
    #[cfg(target_os = "macos")]
    return Some(agent_path().is_some_and(|path| path.exists()));
    #[cfg(not(target_os = "macos"))]
    None
}

/// Takes effect at the next login; nothing is loaded into launchd now.
#[tauri::command]
pub fn set_menu_bar_login(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let path = agent_path().ok_or("no home folder")?;
        if enabled {
            write_agent()?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        }
        log::info!("Start at login in the menu bar: {enabled}");
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = enabled;
        Err("Start at login in the menu bar is only available on macOS".into())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn login_item_opens_the_bundle_hidden_in_the_background() {
        let args = launch_arguments(Path::new("/Applications/Meetily - Actually Free.app/Contents/MacOS/meetily"));
        assert_eq!(args, ["/usr/bin/open", "-g", "-a", "/Applications/Meetily - Actually Free.app", "--args", LAUNCH_FLAG]);
        // Outside a bundle (a dev build) the binary runs directly.
        assert_eq!(launch_arguments(Path::new("/tmp/target/debug/meetily")), ["/tmp/target/debug/meetily", LAUNCH_FLAG]);
    }

    #[test]
    fn login_item_plist_escapes_paths() {
        let plist = agent_plist(&["/Apps/R&D <x>.app".into()]);
        assert!(plist.contains("<string>/Apps/R&amp;D &lt;x&gt;.app</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains(AGENT_LABEL));
    }
}
