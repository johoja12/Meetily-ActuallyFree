//! CoreML (Neural Engine) encoders for Whisper on macOS.
//!
//! whisper-rs is built with whisper.cpp's CoreML support (with fallback):
//! when `ggml-<model>-encoder.mlmodelc` sits next to `ggml-<model>[-qX_Y].bin`,
//! whisper.cpp runs the encoder through CoreML and keeps the decoder on Metal.
//! Without the folder it silently uses Metal for both. The app never shipped
//! the encoder, so this module fetches the pinned conversion published by
//! whisper.cpp for an installed model, unpacks it next to the model and
//! compiles it once while the app is idle.
//!
//! Measured on an M4 with a 27 min room meeting (`perf_bench::post_call`):
//! post-call Whisper large-v3-turbo q5_0 took 110 s on Metal and 58 s with the
//! CoreML encoder (CPU time 5.2 vs 5.3 s; 3,062 vs 3,092 words). The first
//! load on a machine compiles the encoder for the Neural Engine (~45 s).
//!
//! The decoder state is reused across windows (`WhisperEngine::batch_state`),
//! because whisper.cpp loads the CoreML encoder whenever a state is created.
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

const REPO: &str = "ggerganov/whisper.cpp";
/// Hugging Face revision the hashes below were taken from.
const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

/// (base model, zip file, size, SHA-256). The zip holds one top-level folder,
/// `ggml-<base>-encoder.mlmodelc`. Quantized models share their base encoder.
const ENCODERS: &[(&str, &str, u64, &str)] = &[(
    "large-v3-turbo",
    "ggml-large-v3-turbo-encoder.mlmodelc.zip",
    1_173_393_014,
    "84bedfe895bd7b5de6e8e89a0803dfc5addf8c0c5bc4c937451716bf7cf7988a",
)];

/// The base model of a ggml file name: `ggml-large-v3-turbo-q5_0.bin` →
/// `large-v3-turbo` (the same rule whisper.cpp uses to find the encoder).
pub fn base_model(file_name: &str) -> Option<String> {
    let stem = file_name.strip_prefix("ggml-")?.strip_suffix(".bin")?;
    let base = match stem.rfind('-') {
        Some(pos) => {
            let suffix = &stem[pos..];
            if suffix.len() == 5 && suffix.as_bytes()[1] == b'q' && suffix.as_bytes()[3] == b'_' { &stem[..pos] } else { stem }
        }
        None => stem,
    };
    Some(base.to_string())
}

/// Where whisper.cpp looks for the encoder of a base model.
pub fn encoder_dir(models_dir: &Path, base: &str) -> PathBuf {
    models_dir.join(format!("ggml-{base}-encoder.mlmodelc"))
}

fn installed(dir: &Path) -> bool {
    dir.join("coremldata.bin").is_file() && dir.join("model.mil").is_file()
}

/// Remove the encoder of a deleted model unless another installed variant of
/// the same base model (another quantization) still uses it.
pub fn remove_unused(models_dir: &Path, deleted_file: &str) {
    let Some(base) = base_model(deleted_file) else { return };
    let still_used = std::fs::read_dir(models_dir)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name != deleted_file && base_model(&name).as_deref() == Some(base.as_str())
            })
        })
        .unwrap_or(false);
    let dir = encoder_dir(models_dir, &base);
    if !still_used && dir.exists() {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => log::info!("Removed Whisper CoreML encoder {}", dir.display()),
            Err(e) => log::warn!("Could not remove Whisper CoreML encoder {}: {e}", dir.display()),
        }
    }
}

/// Unpack `zip` (one `<name>.mlmodelc/` folder) into `dest`, via a temporary
/// folder renamed into place, refusing entries that would escape it.
fn unpack(zip_path: &Path, dest: &Path) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| anyhow!("encoder folder has no parent"))?;
    let name = dest.file_name().ok_or_else(|| anyhow!("encoder folder has no name"))?;
    let staging = parent.join(format!(".{}.partial", name.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).context("reading the encoder zip")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(anyhow!("encoder zip has an unsafe path: {}", entry.name()));
        };
        // Only the encoder folder; skip macOS resource forks (__MACOSX/).
        let Ok(inner) = relative.strip_prefix(name) else { continue };
        let out = staging.join(inner);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut writer = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut writer)?;
        }
    }
    if !installed(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(anyhow!("encoder zip did not contain {}", name.to_string_lossy()));
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&staging, dest)?;
    Ok(())
}

/// Fetch, verify and unpack the encoder for every installed model that has a
/// published one. Quiet: Whisper keeps using Metal until the folder exists.
pub async fn install_for_installed_models(models_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut ready = Vec::new();
    let Ok(entries) = std::fs::read_dir(models_dir) else { return Ok(ready) };
    let bases: std::collections::BTreeSet<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| base_model(&e.file_name().to_string_lossy()))
        .collect();
    for base in bases {
        let Some(&(_, zip_name, size, sha256)) = ENCODERS.iter().find(|(b, ..)| *b == base) else { continue };
        let dir = encoder_dir(models_dir, &base);
        if !installed(&dir) {
            log::info!("Whisper CoreML: fetching the {base} encoder ({} MB)", size / 1_000_000);
            let zip_path = models_dir.join(format!(".{zip_name}"));
            let asset = crate::diarization::download::Asset {
                name: format!("Whisper CoreML encoder {base}"),
                dest: zip_path.clone(),
                size,
                url: format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{zip_name}"),
                sha256,
            };
            crate::diarization::download::install_assets::<tauri::Wry>(None, &[asset], "Whisper CoreML encoder downloaded").await?;
            let (zip_for_task, dir_for_task) = (zip_path.clone(), dir.clone());
            tauri::async_runtime::spawn_blocking(move || unpack(&zip_for_task, &dir_for_task)).await??;
            let _ = std::fs::remove_file(&zip_path);
        }
        ready.push(dir);
    }
    Ok(ready)
}

/// Install encoders in the background and compile them once (whisper.cpp
/// uses all compute units, as here, so the compiled model is cached for it).
/// Compiling waits until the app has been idle for a minute.
pub fn prepare_in_background() {
    tauri::async_runtime::spawn(async {
        static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let Ok(_install) = INSTALL.try_lock() else { return };
        let models_dir = crate::paths::models_dir();
        let ready = match install_for_installed_models(&models_dir).await {
            Ok(ready) => ready,
            Err(error) => {
                log::warn!("Whisper CoreML encoder unavailable; Whisper stays on Metal: {error:#}");
                return;
            }
        };
        for dir in ready {
            let marker = dir.join(".meetily-compiled");
            if marker.exists() {
                continue;
            }
            tauri::async_runtime::spawn_blocking(crate::audio::room_cleanup::wait_until_idle).await.ok();
            let started = std::time::Instant::now();
            let warm = dir.clone();
            match tauri::async_runtime::spawn_blocking(move || crate::coreml::Model::load(&warm, crate::coreml::COMPUTE_UNITS_ALL).map(drop)).await {
                Ok(Ok(())) => {
                    let _ = std::fs::write(&marker, b"");
                    log::info!("Whisper CoreML encoder {} ready in {:.1} s", dir.display(), started.elapsed().as_secs_f32());
                }
                Ok(Err(error)) => log::warn!("Whisper CoreML encoder failed to compile; Whisper stays on Metal: {error}"),
                Err(error) => log::warn!("Whisper CoreML warm-up task failed: {error}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_model_follows_whisper_cpp_naming() {
        assert_eq!(base_model("ggml-large-v3-turbo-q5_0.bin").as_deref(), Some("large-v3-turbo"));
        assert_eq!(base_model("ggml-large-v3-turbo.bin").as_deref(), Some("large-v3-turbo"));
        assert_eq!(base_model("ggml-small.en-q8_0.bin").as_deref(), Some("small.en"));
        assert_eq!(base_model("ggml-large-v3-turbo-encoder.mlmodelc"), None);
        assert_eq!(base_model("other.bin"), None);
    }

    #[test]
    fn unpack_keeps_only_the_encoder_folder_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("enc.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            for (name, body) in [
                ("ggml-x-encoder.mlmodelc/coremldata.bin", "a"),
                ("ggml-x-encoder.mlmodelc/model.mil", "b"),
                ("ggml-x-encoder.mlmodelc/weights/weight.bin", "c"),
                ("__MACOSX/ggml-x-encoder.mlmodelc/._model.mil", "junk"),
            ] {
                zip.start_file(name, options).unwrap();
                std::io::Write::write_all(&mut zip, body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let dest = encoder_dir(dir.path(), "x");
        unpack(&zip_path, &dest).unwrap();
        assert!(installed(&dest));
        assert_eq!(std::fs::read_to_string(dest.join("weights/weight.bin")).unwrap(), "c");
        assert!(!dir.path().join("__MACOSX").exists());
        assert!(!dir.path().join(".ggml-x-encoder.mlmodelc.partial").exists());
    }

    #[test]
    fn deleting_one_quantization_keeps_the_shared_encoder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(encoder_dir(dir.path(), "large-v3-turbo")).unwrap();
        std::fs::write(dir.path().join("ggml-large-v3-turbo.bin"), b"").unwrap();
        remove_unused(dir.path(), "ggml-large-v3-turbo-q5_0.bin");
        assert!(encoder_dir(dir.path(), "large-v3-turbo").exists());
        std::fs::remove_file(dir.path().join("ggml-large-v3-turbo.bin")).unwrap();
        remove_unused(dir.path(), "ggml-large-v3-turbo.bin");
        assert!(!encoder_dir(dir.path(), "large-v3-turbo").exists());
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;

    /// Downloads the pinned encoder (~1.2 GB) through the real install path
    /// into a temporary models folder and loads it with CoreML.
    #[tokio::test]
    #[ignore]
    async fn install_downloads_verifies_unpacks_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-large-v3-turbo-q5_0.bin"), b"placeholder").unwrap();
        let ready = install_for_installed_models(dir.path()).await.unwrap();
        assert_eq!(ready, vec![encoder_dir(dir.path(), "large-v3-turbo")]);
        assert!(installed(&ready[0]));
        assert!(!dir.path().join(".ggml-large-v3-turbo-encoder.mlmodelc.zip").exists());
        crate::coreml::Model::load(&ready[0], crate::coreml::COMPUTE_UNITS_ALL).unwrap();
    }
}
