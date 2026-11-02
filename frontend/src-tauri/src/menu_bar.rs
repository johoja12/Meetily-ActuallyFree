//! macOS menu-bar mode. Closing the window and ⌘Q hide Meetily to the menu bar
//! (no window, no Dock icon) instead of quitting, so meeting detection, calendar
//! listening and automation keep running. The WebView stays loaded: those
//! features listen for native events there. Only the menu-bar icon's Quit (or
//! the Dock's, while the icon shows) ends the app.
//!
//! "Start at login in the menu bar" is a per-user LaunchAgent that opens the
//! app with `--menu-bar`, which starts it hidden. Windows already closes to
//! the tray; its "Start at login in the system tray" is a per-user Run entry
//! with the same flag. Linux reports the setting as unsupported.
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

/// Windows: a per-user Run entry. Task Manager's Startup apps toggle leaves it
/// in place and records the choice under StartupApproved instead.
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const STARTUP_APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "Meetily - Actually Free";

#[cfg(any(windows, test))]
fn run_command(exe: &std::path::Path) -> String {
    format!("\"{}\" {LAUNCH_FLAG}", exe.display())
}

/// StartupApproved data starts with 0x02 (enabled) or 0x03 (disabled); an
/// odd first byte is how Task Manager marks an entry turned off.
#[cfg(any(windows, test))]
fn startup_disabled(approved: &[u8]) -> bool {
    approved.first().is_some_and(|flags| flags & 1 == 1)
}

#[cfg(windows)]
fn run_entry_enabled() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let present = user.open_subkey(RUN_KEY).is_ok_and(|key| key.get_value::<String, _>(RUN_VALUE).is_ok());
    let disabled = user
        .open_subkey(STARTUP_APPROVED_KEY)
        .and_then(|key| key.get_raw_value(RUN_VALUE))
        .is_ok_and(|value| startup_disabled(&value.bytes));
    present && !disabled
}

#[cfg(windows)]
fn write_run_entry() -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = user.create_subkey(RUN_KEY).map_err(|error| error.to_string())?;
    run.set_value(RUN_VALUE, &run_command(&exe)).map_err(|error| error.to_string())?;
    // Turning the switch on also clears a Task Manager "Disabled".
    if let Ok(approved) = user.open_subkey_with_flags(STARTUP_APPROVED_KEY, winreg::enums::KEY_SET_VALUE) {
        let _ = approved.delete_value(RUN_VALUE);
    }
    Ok(())
}

#[cfg(windows)]
fn remove_run_entry() -> Result<(), String> {
    use winreg::{enums::*, RegKey};
    let user = RegKey::predef(HKEY_CURRENT_USER);
    for path in [RUN_KEY, STARTUP_APPROVED_KEY] {
        if let Ok(key) = user.open_subkey_with_flags(path, KEY_SET_VALUE) {
            match key.delete_value(RUN_VALUE) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.to_string()),
                _ => {}
            }
        }
    }
    Ok(())
}

/// An enabled login item is rewritten at startup so it follows a moved app.
pub fn refresh_login_item() {
    #[cfg(target_os = "macos")]
    if agent_path().is_some_and(|path| path.exists()) {
        if let Err(error) = write_agent() {
            log::warn!("Could not refresh the menu-bar login item: {error}");
        }
    }
    // Rewriting only the command keeps a Task Manager "Disabled" choice.
    #[cfg(windows)]
    if run_entry_enabled() {
        use winreg::{enums::*, RegKey};
        let written = std::env::current_exe().map_err(|error| error.to_string()).and_then(|exe| {
            RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
                .and_then(|run| run.set_value(RUN_VALUE, &run_command(&exe)))
                .map_err(|error| error.to_string())
        });
        if let Err(error) = written {
            log::warn!("Could not refresh the tray login item: {error}");
        }
    }
}

/// None where starting at login is not offered (Linux).
#[tauri::command]
pub fn get_menu_bar_login() -> Option<bool> {
    #[cfg(target_os = "macos")]
    return Some(agent_path().is_some_and(|path| path.exists()));
    #[cfg(windows)]
    return Some(run_entry_enabled());
    #[cfg(not(any(target_os = "macos", windows)))]
    None
}

/// Takes effect at the next login; nothing is started now.
#[tauri::command]
pub fn set_menu_bar_login(enabled: bool) -> Result<(), String> {
    set_login_item(enabled)?;
    log::info!("Start at login hidden: {enabled}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_login_item(enabled: bool) -> Result<(), String> {
    let path = agent_path().ok_or("no home folder")?;
    if enabled {
        write_agent()
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(|error| error.to_string())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn set_login_item(enabled: bool) -> Result<(), String> {
    if enabled {
        write_run_entry()
    } else {
        remove_run_entry()
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn set_login_item(_enabled: bool) -> Result<(), String> {
    Err("Starting at login is not available on this platform".into())
}

/// Diagnostics from `HiddenTimerProbe`: whether the WebView's timers are
/// throttled while the window is hidden (issue #12).
#[tauri::command]
pub fn log_webview_timing<R: Runtime>(app: AppHandle<R>, event: String, visibility: String, late_ms: Option<u64>) {
    let window_visible = app.get_webview_window("main").and_then(|window| window.is_visible().ok());
    let event: String = event.chars().take(32).collect();
    let visibility: String = visibility.chars().take(16).collect();
    match late_ms {
        Some(late) => log::warn!("WebView timer ran {late} ms late (page {visibility}, window visible: {window_visible:?})"),
        None => log::info!("WebView {event}: page {visibility}, window visible: {window_visible:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn windows_run_entry_quotes_the_exe_and_starts_hidden() {
        assert_eq!(
            run_command(Path::new(r"C:\Program Files\Meetily\meetily.exe")),
            format!("\"C:\\Program Files\\Meetily\\meetily.exe\" {LAUNCH_FLAG}")
        );
    }

    #[test]
    fn task_manager_disabled_flag_is_an_odd_first_byte() {
        assert!(!startup_disabled(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(startup_disabled(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!startup_disabled(&[]));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
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
