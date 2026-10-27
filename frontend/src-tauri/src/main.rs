#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use log;

fn main() {
    // A short-lived copy of the app reports TCC's uncached Audio Capture status
    // (see audio/permissions.rs). Exit before logging or Tauri start.
    #[cfg(target_os = "macos")]
    if std::env::args().nth(1).as_deref() == Some(app_lib::audio::permissions::AUDIO_CAPTURE_STATUS_ARG) {
        app_lib::audio::permissions::print_audio_capture_status_and_exit();
    }

    std::env::set_var("RUST_LOG", "info");
    // stderr plus ~/Library/Logs/Meetily/meetily.log (capped), so a copy
    // opened from Finder still leaves a log to read.
    app_lib::logging::init();

    // Async logger will be initialized lazily when first needed (after Tauri runtime starts)
    log::info!("Starting application...");
    app_lib::run();
}
