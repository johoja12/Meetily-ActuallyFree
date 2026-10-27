//! The DTLN-aec echo-cancellation models (MIT, see resources/dtln-aec/LICENSE)
//! are compiled into the app with include_bytes!, but model binaries are not
//! kept in git. Fetch them once from a pinned upstream commit and verify their
//! SHA-256 before the crate compiles; a mismatch stops the build.
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, time::Duration};

/// fastrepl/anarlog crates/aec/data/models at this commit, identical to the
/// files upstream PR #55 committed.
const SOURCE: &str = "https://raw.githubusercontent.com/fastrepl/anarlog/b14809dcd80fe80c5158c8a9f5a19fa173878b12/crates/aec/data/models";

const MODELS: [(&str, &str); 2] = [
    ("model_128_1.onnx", "a060e46a6bebed03d6360262d814851dc6c2806cec7c1b6a388e617cae7c082c"),
    ("model_128_2.onnx", "6b6e312f701a3fad2aac2981ca1ed978d25dffa0bc8da9f33235a2e91f56705a"),
];

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn ensure_dtln_models() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let folder = Path::new(&manifest).join("resources").join("dtln-aec");
    for (name, expected) in MODELS {
        let path = folder.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        if fs::read(&path).is_ok_and(|bytes| sha256(&bytes) == expected) {
            continue;
        }
        let url = format!("{SOURCE}/{name}");
        println!("cargo:warning=Downloading echo-cancellation model {name}");
        let bytes = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .and_then(|client| client.get(&url).send())
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.bytes())
            .unwrap_or_else(|error| panic!("Could not download {url}: {error}. Place {name} in {} to build offline.", folder.display()));
        let actual = sha256(&bytes);
        if actual != expected {
            panic!("{name} from {url} has SHA-256 {actual}, expected {expected}");
        }
        fs::create_dir_all(&folder).expect("create resources/dtln-aec");
        let partial = path.with_extension("onnx.part");
        fs::write(&partial, &bytes).and_then(|_| fs::rename(&partial, &path))
            .unwrap_or_else(|error| panic!("Could not save {}: {error}", path.display()));
    }
}
