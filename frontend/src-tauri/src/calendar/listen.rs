//! Listening for a calendar meeting to start (opt-in, `listen_for_start`).
//! While a meeting is on and has not been prompted, the recording microphone is
//! metered for a few seconds each minute; speech-level sound queues the usual
//! calendar prompt. This catches calls no app reports, such as one on a Desk
//! Pro or room system. Only loudness is computed: no audio is kept, written or
//! transcribed. Opening the microphone shows the macOS microphone indicator.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tauri::{AppHandle, Emitter, Runtime};

pub(crate) const SAMPLE: Duration = Duration::from_secs(10);
const FRAME_SECS: f32 = 0.05;
/// About -40 dBFS: quiet speech, or a call on a nearby speaker. Room tone and
/// a laptop fan sit well below it.
const SPEECH_RMS: f32 = 0.01;
const HEARD_SECS: f32 = 2.0;

/// Enough loud 50 ms frames in one sample to be people talking.
pub(crate) fn heard_speech(frame_rms: &[f32]) -> bool {
    frame_rms.iter().filter(|rms| **rms >= SPEECH_RMS).count() as f32 * FRAME_SECS >= HEARD_SECS
}

/// Running RMS over fixed frames, fed from the capture callback.
#[derive(Default)]
struct Frames {
    rms: Vec<f32>,
    sum: f64,
    count: usize,
    frame_len: usize,
}
impl Frames {
    fn push(&mut self, samples: impl Iterator<Item = f32>) {
        for sample in samples {
            self.sum += f64::from(sample) * f64::from(sample);
            self.count += 1;
            if self.count >= self.frame_len {
                self.rms.push((self.sum / self.count as f64).sqrt() as f32);
                self.sum = 0.0;
                self.count = 0;
            }
        }
    }
}

/// The saved recording microphone as the OS names it, read without logging;
/// None means default. Preferences store "Name (input)"; CoreAudio says "Name".
pub(crate) fn preferred_mic<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    use tauri_plugin_store::StoreExt;
    let preferences = app.store("recording_preferences.json").ok()?.get("preferences")?;
    preferences.get("preferred_mic_device")?.as_str().map(device_name)
}

fn device_name(saved: &str) -> String {
    crate::audio::devices::AudioDevice::from_name(saved).map_or_else(|_| saved.to_string(), |device| device.name)
}

fn open_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, frames: Arc<Mutex<Frames>>) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            if let Ok(mut frames) = frames.lock() {
                frames.push(data.iter().map(|sample| sample.to_sample::<f32>()));
            }
        },
        |error| log::warn!("Calendar listening stream error: {error}"),
        None,
    )?)
}

/// Meter the microphone for `duration` and return per-frame RMS. Blocking.
pub(crate) fn sample_frames(device_name: Option<&str>, duration: Duration) -> anyhow::Result<Vec<f32>> {
    let host = cpal::default_host();
    let named = device_name.and_then(|wanted| {
        host.input_devices().ok()?.find(|device| device.name().is_ok_and(|name| name == wanted))
    });
    let device = named
        .or_else(|| host.default_input_device())
        .ok_or_else(|| anyhow::anyhow!("no microphone"))?;
    let supported = device.default_input_config()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let frame_len = ((config.sample_rate.0 as f32 * f32::from(config.channels) * FRAME_SECS) as usize).max(1);
    let frames = Arc::new(Mutex::new(Frames { frame_len, ..Frames::default() }));
    let stream = match format {
        cpal::SampleFormat::F32 => open_stream::<f32>(&device, &config, frames.clone())?,
        cpal::SampleFormat::I16 => open_stream::<i16>(&device, &config, frames.clone())?,
        cpal::SampleFormat::I32 => open_stream::<i32>(&device, &config, frames.clone())?,
        cpal::SampleFormat::U16 => open_stream::<u16>(&device, &config, frames.clone())?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;
    std::thread::sleep(duration);
    drop(stream);
    let rms = std::mem::take(&mut frames.lock().map_err(|_| anyhow::anyhow!("meter poisoned"))?.rms);
    Ok(rms)
}

pub fn initialize<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_error: Option<String> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let now = chrono::Utc::now().timestamp_millis();
            // Never compete with a recording or the Settings level meter.
            if !super::state().lock().unwrap().wants_listening(now)
                || crate::audio::recording_commands::is_recording_active()
                || crate::audio::simple_level_monitor::is_monitoring()
            {
                continue;
            }
            let device = preferred_mic(&app);
            let sampled = tauri::async_runtime::spawn_blocking(move || sample_frames(device.as_deref(), SAMPLE)).await;
            let heard = match sampled {
                Ok(Ok(frames)) => {
                    last_error = None;
                    heard_speech(&frames)
                }
                Ok(Err(error)) => {
                    let error = error.to_string();
                    if last_error.as_ref() != Some(&error) {
                        log::warn!("Calendar listening could not read the microphone: {error}");
                        last_error = Some(error);
                    }
                    false
                }
                Err(_) => false,
            };
            if !heard {
                continue;
            }
            let now = chrono::Utc::now().timestamp_millis();
            let recording = crate::audio::recording_commands::is_recording_active();
            if super::state().lock().unwrap().queue_heard(now, recording) {
                log::info!("Calendar listening heard a meeting start; prompting");
                super::notify_prompt(&app, "Your calendar meeting has started");
                let _ = app.emit("calendar-changed", ());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_microphone_is_matched_by_its_os_name() {
        assert_eq!(device_name("Jabra Speak 710 (input)"), "Jabra Speak 710");
        assert_eq!(device_name("MacBook Pro Microphone"), "MacBook Pro Microphone");
    }

    #[test]
    fn speech_needs_two_seconds_of_loud_frames() {
        let quiet = vec![0.002; 200];
        assert!(!heard_speech(&quiet));
        let mut cough = quiet.clone();
        cough[10..30].fill(0.2); // one second
        assert!(!heard_speech(&cough));
        let mut talk = quiet.clone();
        for start in [20, 60, 100, 140] {
            talk[start..start + 12].fill(0.03); // 4 × 0.6 s, with pauses
        }
        assert!(heard_speech(&talk));
    }

    #[test]
    fn frames_are_root_mean_square_over_fixed_lengths() {
        let mut frames = Frames { frame_len: 4, ..Frames::default() };
        frames.push([0.5, -0.5, 0.5, -0.5, 0.0, 0.0].into_iter());
        assert_eq!(frames.rms, vec![0.5]);
        frames.push([0.0, 0.0].into_iter());
        assert_eq!(frames.rms, vec![0.5, 0.0]);
    }
}
