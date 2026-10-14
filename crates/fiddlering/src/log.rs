//! Plain file log in %LOCALAPPDATA%\FiddleRing\fiddlering.log (the in-game oracle while testing).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

static LOG: Mutex<Option<(File, Instant)>> = Mutex::new(None);

pub fn data_dir() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("FiddleRing")
}

pub fn init() {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("fiddlering.log");
    let _ = std::fs::rename(&path, dir.join("fiddlering.prev.log"));
    if let Ok(f) = OpenOptions::new().create(true).write(true).truncate(true).open(path) {
        *LOG.lock().unwrap() = Some((f, Instant::now()));
    }
}

pub fn write(msg: &str) {
    if let Ok(mut g) = LOG.lock() {
        if let Some((f, t)) = g.as_mut() {
            let _ = writeln!(f, "[{:9.3}] {msg}", t.elapsed().as_secs_f32());
            let _ = f.flush();
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(&format!($($t)*)) };
}
