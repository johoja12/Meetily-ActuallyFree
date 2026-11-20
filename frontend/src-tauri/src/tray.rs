use std::sync::atomic::{AtomicU8, Ordering};
use tauri::{
    image::Image,
    include_image,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Manager, Runtime,
};

const TOOLTIP: &str = "Meetily - Actually Free";
const TOOLTIP_RECORDING: &str = "Meetily - Actually Free (recording)";

// Tray images, generated from icons/source/*.svg by icons/source/generate.sh.
// macOS idle is a template image (macOS tints it black or white to match the
// menu bar); a template cannot hold colour, so the recording one is a
// full-colour red bubble that reads on either. Elsewhere the tray shows the app
// tile, with a red dot while recording.
#[cfg(target_os = "macos")]
const TRAY_IDLE: Image<'static> = include_image!("./icons/tray/tray-macos.png");
#[cfg(target_os = "macos")]
const TRAY_RECORDING: Image<'static> = include_image!("./icons/tray/tray-macos-recording.png");
#[cfg(not(target_os = "macos"))]
const TRAY_IDLE: Image<'static> = include_image!("./icons/tray/tray-windows.png");
#[cfg(not(target_os = "macos"))]
const TRAY_RECORDING: Image<'static> = include_image!("./icons/tray/tray-windows-recording.png");
#[cfg(target_os = "windows")]
const TASKBAR_OVERLAY_RECORDING: Image<'static> = include_image!("./icons/tray/overlay-recording.png");

/// The tray image last applied (0 = none yet, 1 = idle, 2 = recording). The
/// menu is rebuilt on every state refresh; this keeps the icon from being
/// re-uploaded (and flickering) when only the menu changed.
static SHOWN_ICON: AtomicU8 = AtomicU8::new(0);

#[derive(Debug, Clone)]
pub enum RecordingState {
    Stopped,
    Starting,
    Recording,
    Pausing,
    Paused,
    Resuming,
    Stopping,
}

pub fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Start with default menu, will update with actual state after initialization
    // Pass can_record=true initially, will be updated by update_tray_menu immediately
    let menu = build_menu(app, RecordingState::Stopped, true)?;

    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        // Tauri shows the tray menu on left-click by default, which swallows the
        // click before any handler runs — so clicking the icon only ever opened
        // the menu and never the app. Reserve left-click for "open the window"
        // and leave the menu on right-click, which is what people expect.
        .show_menu_on_left_click(false)
        .tooltip(TOOLTIP)
        .icon(TRAY_IDLE)
        .icon_as_template(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| handle_menu_event(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            // Act on release rather than press, so a click-and-drag doesn't
            // count, and only for the left button.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                focus_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    SHOWN_ICON.store(1, Ordering::Relaxed);

    // Update tray menu with actual recording state after creation
    update_tray_menu(app);

    Ok(())
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, item_id: &str) {
    match item_id {
        "toggle_recording" => toggle_recording_handler(app),
        "pause_recording" => pause_recording_handler(app),
        "resume_recording" => resume_recording_handler(app),
        "stop_recording" => stop_recording_handler(app),
        "open_window" => focus_main_window(app),
        "settings" => {
            focus_main_window(app);
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("window.location.assign('/settings')");
            }
        }
        "check_updates" => check_updates_handler(app),
        "quit" => app.exit(0),
        _ => {}
    }
}
fn toggle_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    focus_main_window(app);
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        if crate::is_recording().await {
            // Immediately show stopping state
            set_tray_state(&app_clone, RecordingState::Stopping);

            log::info!("Tray toggle: Stopping recording...");

            // Generate save path (same as RecordingControls.tsx)
            let data_dir = crate::paths::install_data_root();

            let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
            let save_path = data_dir.join(format!("recording-{}.wav", timestamp));

            // Call Rust stop_recording command (like pause/resume pattern)
            let stop_result = crate::audio::recording_commands::stop_recording(
                app_clone.clone(),
                crate::audio::recording_commands::RecordingArgs {
                    save_path: save_path.to_string_lossy().to_string(),
                },
            )
            .await;

            // Handle result
            match stop_result {
                Ok(crate::audio::recording_commands::StopOutcome::Completed) => {
                    log::info!("Tray toggle: Recording stopped successfully");
                }
                Ok(crate::audio::recording_commands::StopOutcome::AlreadyStopping) => {
                    log::info!("Tray toggle: Recording stop is already in progress");
                }
                Ok(crate::audio::recording_commands::StopOutcome::AlreadyStopped) => {
                    log::info!("Tray toggle: Recording was already stopped");
                    update_tray_menu_async(&app_clone).await;
                }
                Err(e) => {
                    log::error!("Tray toggle: Failed to stop recording: {}", e);
                    // Revert tray state on error
                    update_tray_menu_async(&app_clone).await;
                }
            }
        } else {
            // Immediately show starting state
            set_tray_state(&app_clone, RecordingState::Starting);

            log::info!("Emitting start recording event from tray");
            if let Some(window) = app_clone.get_webview_window("main") {
                let _ = window.eval("sessionStorage.setItem('autoStartRecording', 'true')"); // Set the flag to start recording automatically
                let _ = window.eval("window.location.assign('/')");
            }
        }
    });
}

fn pause_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    // Immediately show pausing state
    set_tray_state(app, RecordingState::Pausing);

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = crate::audio::recording_commands::pause_recording(app_clone.clone()).await {
            log::error!("Failed to pause recording from tray: {}", e);
            // Revert to current state on error
            update_tray_menu_async(&app_clone).await;
        } else {
            log::info!("Recording paused from tray");
            // The pause_recording function will call update_tray_menu, so no need to call it here
        }
    });
}

fn resume_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    // Immediately show resuming state
    set_tray_state(app, RecordingState::Resuming);

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = crate::audio::recording_commands::resume_recording(app_clone.clone()).await
        {
            log::error!("Failed to resume recording from tray: {}", e);
            // Revert to current state on error
            update_tray_menu_async(&app_clone).await;
        } else {
            log::info!("Recording resumed from tray");
            // The resume_recording function will call update_tray_menu, so no need to call it here
        }
    });
}

fn stop_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    // Immediately show stopping state
    set_tray_state(app, RecordingState::Stopping);

    focus_main_window(app);
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        log::info!("Tray: Stopping recording...");

        // Generate save path (same as RecordingControls.tsx)
        let data_dir = crate::paths::install_data_root();

        let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let save_path = data_dir.join(format!("recording-{}.wav", timestamp));

        // Call Rust stop_recording command (like pause/resume pattern)
        let stop_result = crate::audio::recording_commands::stop_recording(
            app_clone.clone(),
            crate::audio::recording_commands::RecordingArgs {
                save_path: save_path.to_string_lossy().to_string(),
            },
        )
        .await;

        // Handle result
        match stop_result {
            Ok(crate::audio::recording_commands::StopOutcome::Completed) => {
                log::info!("Tray: Recording stopped successfully");
            }
            Ok(crate::audio::recording_commands::StopOutcome::AlreadyStopping) => {
                log::info!("Tray: Recording stop is already in progress");
            }
            Ok(crate::audio::recording_commands::StopOutcome::AlreadyStopped) => {
                log::info!("Tray: Recording was already stopped");
                update_tray_menu_async(&app_clone).await;
            }
            Err(e) => {
                log::error!("Tray: Failed to stop recording: {}", e);
                // Revert tray state on error
                update_tray_menu_async(&app_clone).await;
            }
        }
    });
}

fn check_updates_handler<R: Runtime>(app: &AppHandle<R>) {
    focus_main_window(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(
            "window.dispatchEvent(new CustomEvent('check-updates-from-tray'))"
        );
    }
}

pub fn update_tray_menu<R: Runtime>(app: &AppHandle<R>) {
    // For sync update, spawn async task to get current state
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        // Small delay to ensure recording state has been updated
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        update_tray_menu_async(&app_clone).await;
    });
}

pub fn set_tray_state<R: Runtime>(app: &AppHandle<R>, state: RecordingState) {
    log::info!("Tray: Setting intermediate state: {:?}", state);
    let recording = shows_recording(&state);
    // During recording state transitions, we assume recording is allowed (we're already recording)
    if let Ok(menu) = build_menu(app, state, true) {
        if let Some(tray) = app.tray_by_id("main-tray") {
            let result = tray.set_menu(Some(menu));
            log::info!("Tray: Intermediate state menu update result: {:?}", result);
            apply_recording_icon(app, &tray, recording);
        } else {
            log::warn!("Tray: Could not find tray with id 'main-tray'");
        }
    } else {
        log::error!("Tray: Failed to build menu for intermediate state");
    }
}

/// Audio is being captured (or about to stop) in this state, so the tray shows
/// the recording icon. Paused and Starting show the idle icon: nothing is being
/// captured yet / at the moment.
fn shows_recording(state: &RecordingState) -> bool {
    matches!(
        state,
        RecordingState::Recording | RecordingState::Pausing | RecordingState::Resuming | RecordingState::Stopping
    )
}

/// Swap the tray icon (and, on Windows, the taskbar-button overlay) between idle
/// and recording. No-op when that icon is already showing.
fn apply_recording_icon<R: Runtime>(app: &AppHandle<R>, tray: &TrayIcon<R>, recording: bool) {
    let wanted = if recording { 2 } else { 1 };
    if SHOWN_ICON.swap(wanted, Ordering::Relaxed) == wanted {
        return;
    }
    let image = if recording { TRAY_RECORDING } else { TRAY_IDLE };
    if let Err(error) = tray.set_icon(Some(image)) {
        log::warn!("Tray: could not set the {} icon: {error}", if recording { "recording" } else { "idle" });
        SHOWN_ICON.store(0, Ordering::Relaxed);
    }
    // Set after the image: macOS applies the template flag to the current image.
    let _ = tray.set_icon_as_template(cfg!(target_os = "macos") && !recording);
    let _ = tray.set_tooltip(Some(if recording { TOOLTIP_RECORDING } else { TOOLTIP }));

    #[cfg(target_os = "windows")]
    if let Some(window) = app.get_webview_window("main") {
        let overlay = recording.then_some(TASKBAR_OVERLAY_RECORDING);
        if let Err(error) = window.set_overlay_icon(overlay) {
            log::warn!("Taskbar: could not set the recording overlay: {error}");
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = app;
}

async fn get_current_recording_state() -> RecordingState {
    // Check if currently recording
    let is_recording = crate::audio::recording_commands::is_recording().await;
    log::info!(
        "Tray: get_current_recording_state - is_recording: {}",
        is_recording
    );

    if !is_recording {
        log::info!("Tray: Recording state is Stopped");
        return RecordingState::Stopped;
    }

    // Check if paused
    let is_paused = crate::audio::recording_commands::is_recording_paused().await;
    log::info!("Tray: is_paused: {}", is_paused);

    if is_paused {
        log::info!("Tray: Recording state is Paused");
        RecordingState::Paused
    } else {
        log::info!("Tray: Recording state is Recording");
        RecordingState::Recording
    }
}

/// Check if recording is allowed based on onboarding status and transcription model availability
/// Returns true if:
/// - Onboarding is complete (user may prefer Whisper later), OR
/// - Parakeet transcription model is ready (downloaded)
async fn check_can_record<R: Runtime>(app: &AppHandle<R>) -> bool {
    // First check if onboarding is complete
    let onboarding_complete = match crate::onboarding::load_onboarding_status(app).await {
        Ok(status) => status.completed,
        Err(e) => {
            log::warn!("Tray: Failed to load onboarding status: {}, assuming complete", e);
            true // Assume complete if we can't check (safe default)
        }
    };

    // If onboarding is complete, always allow recording
    // (user may prefer Whisper or have their own transcription setup)
    if onboarding_complete {
        return true;
    }

    // During onboarding, check if Parakeet transcription model is ready
    match crate::parakeet_engine::commands::parakeet_has_available_models().await {
        Ok(has_models) => has_models,
        Err(e) => {
            log::warn!("Tray: Failed to check Parakeet models: {}, assuming not ready", e);
            false
        }
    }
}

pub async fn update_tray_menu_async<R: Runtime>(app: &AppHandle<R>) {
    log::info!("Tray: update_tray_menu_async called");
    // Get the current recording state
    let recording_state = get_current_recording_state().await;
    log::info!("Tray: Current recording state: {:?}", recording_state);

    // Determine if recording should be allowed
    // Only block recording during incomplete onboarding when no transcription model is ready
    let can_record = check_can_record(app).await;
    log::info!("Tray: can_record: {}", can_record);

    let recording = shows_recording(&recording_state);
    if let Ok(menu) = build_menu(app, recording_state, can_record) {
        if let Some(tray) = app.tray_by_id("main-tray") {
            let result = tray.set_menu(Some(menu));
            log::info!("Tray: Menu update result: {:?}", result);
            apply_recording_icon(app, &tray, recording);
        } else {
            log::warn!("Tray: Could not find tray with id 'main-tray'");
        }
    } else {
        log::error!("Tray: Failed to build menu");
    }
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    state: RecordingState,
    can_record: bool, // True if recording is allowed (onboarding complete OR transcription model ready)
) -> tauri::Result<tauri::menu::Menu<R>> {
    let mut builder = MenuBuilder::new(app);

    // If recording is not allowed (during onboarding, no transcription model), show disabled message
    if !can_record {
        builder = builder.item(
            &MenuItemBuilder::new("⏳ Downloading transcription model...")
                .enabled(false)
                .build(app)?,
        );
    } else {
        match state {
            RecordingState::Stopped => {
                builder = builder
                    .item(&MenuItemBuilder::with_id("toggle_recording", "Start Recording").build(app)?);
            }
            RecordingState::Starting => {
                builder = builder.item(
                    &MenuItemBuilder::new("🔄 Starting Recording...")
                        .enabled(false)
                        .build(app)?,
                );
            }
            RecordingState::Recording => {
                builder = builder
                    .item(&MenuItemBuilder::with_id("pause_recording", "⏸ Pause Recording").build(app)?)
                    .item(&MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording").build(app)?);
            }
            RecordingState::Pausing => {
                builder = builder
                    .item(
                        &MenuItemBuilder::new("⏸ Pausing...")
                            .enabled(false)
                            .build(app)?,
                    )
                    .item(&MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording").build(app)?);
            }
            RecordingState::Paused => {
                builder = builder
                    .item(
                        &MenuItemBuilder::with_id("resume_recording", "▶ Resume Recording")
                            .build(app)?,
                    )
                    .item(&MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording").build(app)?);
            }
            RecordingState::Resuming => {
                builder = builder
                    .item(
                        &MenuItemBuilder::new("▶ Resuming...")
                            .enabled(false)
                            .build(app)?,
                    )
                    .item(&MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording").build(app)?);
            }
            RecordingState::Stopping => {
                builder = builder.item(
                    &MenuItemBuilder::new("⏹ Stopping...")
                        .enabled(false)
                        .build(app)?,
                );
            }
        }
    }

    builder = builder
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&MenuItemBuilder::with_id("open_window", "Open Main Window").build(app)?)
        .item(&MenuItemBuilder::with_id("settings", "Settings").build(app)?);

    #[cfg(not(target_os = "macos"))]
    {
        builder = builder
            .item(&MenuItemBuilder::with_id("check_updates", "Check for Updates").build(app)?);
    }

    builder
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&MenuItemBuilder::with_id("quit", "Quit Meetily").build(app)?)
        .build()
}

pub(crate) fn focus_main_window<R: Runtime>(app: &AppHandle<R>) {
    crate::menu_bar::show_in_dock(app);
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.unminimize() {
            log::error!("Failed to unminimize main window: {}", e);
        }

        if let Err(e) = window.show() {
            log::error!("Failed to show main window: {}", e);
        }

        if let Err(e) = window.set_focus() {
            log::error!("Failed to focus main window: {}", e);
        }

        if let Err(e) = window.eval("window.focus()") {
            log::error!("Failed to focus main webview: {}", e);
        }
    } else {
        log::warn!("Could not find main window");
    }
}
