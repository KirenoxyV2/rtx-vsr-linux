//! One window per user: a second launch asks the first to show its window
//! and exits.

use crate::state::APP_ID;
use crate::ui::{self, Request};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("{APP_ID}.sock"))
}

/// Returns `false` if another instance is running (it has been told to show
/// its window); otherwise starts listening for later launches.
pub fn claim() -> bool {
    let path = socket_path();
    if let Ok(mut other) = UnixStream::connect(&path) {
        let _ = other.write_all(b"show");
        return false;
    }
    let _ = std::fs::remove_file(&path); // left behind by a crash
    let Ok(listener) = UnixListener::bind(&path) else {
        return true;
    };
    let _ = std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut word = [0u8; 4];
                let mut stream = stream;
                if stream.read_exact(&mut word).is_ok() && &word == b"show" {
                    ui::request(Request::Show);
                }
            }
        });
    true
}

pub fn release() {
    let _ = std::fs::remove_file(socket_path());
}
