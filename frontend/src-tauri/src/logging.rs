//! The app log: every line goes to stderr (for `cargo tauri dev` and a
//! terminal launch) and to a log file, since an app opened from Finder has no
//! terminal. The file is capped: past MAX_BYTES it moves to `meetily.1.log`
//! (replacing the older one) and a new file starts. Lines can name meetings
//! and devices, so the file stays on this computer like the database.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// `~/Library/Logs/Meetily` on macOS (where Console looks), else `logs/` in
/// the app data folder.
pub fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Logs/Meetily");
    }
    crate::paths::install_data_root().join("logs")
}

pub struct RotatingFile {
    dir: PathBuf,
    max: u64,
    file: Option<File>,
    written: u64,
}

impl RotatingFile {
    pub fn open(dir: &Path, max: u64) -> RotatingFile {
        let mut log = RotatingFile { dir: dir.to_path_buf(), max, file: None, written: 0 };
        log.reopen();
        log
    }

    fn path(&self) -> PathBuf {
        self.dir.join("meetily.log")
    }

    /// Opens the current file, moving it aside first when it is already full.
    fn reopen(&mut self) {
        self.file = None;
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let size = std::fs::metadata(self.path()).map(|meta| meta.len()).unwrap_or(0);
        if size >= self.max {
            let _ = std::fs::rename(self.path(), self.dir.join("meetily.1.log"));
        }
        self.file = OpenOptions::new().create(true).append(true).open(self.path()).ok();
        self.written = std::fs::metadata(self.path()).map(|meta| meta.len()).unwrap_or(0);
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written >= self.max {
            self.reopen();
        }
        // Logging must never fail the app: a missing or unwritable folder
        // just drops lines from the file (stderr still gets them).
        if let Some(file) = self.file.as_mut() {
            if file.write_all(buf).is_ok() {
                self.written += buf.len() as u64;
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
        Ok(())
    }
}

struct Tee(Mutex<RotatingFile>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        if let Ok(mut file) = self.0.lock() {
            let _ = file.write_all(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Ok(mut file) = self.0.lock() {
            let _ = file.flush();
        }
        Ok(())
    }
}

/// Starts logging at `info` (or `RUST_LOG`) to stderr and the log file.
pub fn init() {
    let file = RotatingFile::open(&log_dir(), MAX_BYTES);
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(Tee(Mutex::new(file)))))
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meetily-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn lines_are_appended_to_the_log_file() {
        let dir = temp("append");
        let mut log = RotatingFile::open(&dir, 1_000);
        log.write_all(b"first\n").unwrap();
        drop(log);
        let mut log = RotatingFile::open(&dir, 1_000);
        log.write_all(b"second\n").unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("meetily.log")).unwrap(), "first\nsecond\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_full_log_moves_aside_and_a_new_one_starts() {
        let dir = temp("rotate");
        let mut log = RotatingFile::open(&dir, 10);
        log.write_all(b"0123456789").unwrap();
        log.write_all(b"next\n").unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("meetily.1.log")).unwrap(), "0123456789");
        assert_eq!(std::fs::read_to_string(dir.join("meetily.log")).unwrap(), "next\n");
        // A file already over the cap at start is moved aside too.
        drop(log);
        std::fs::write(dir.join("meetily.log"), b"0123456789AB").unwrap();
        let mut log = RotatingFile::open(&dir, 10);
        log.write_all(b"fresh\n").unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("meetily.1.log")).unwrap(), "0123456789AB");
        assert_eq!(std::fs::read_to_string(dir.join("meetily.log")).unwrap(), "fresh\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}
