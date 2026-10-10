//! Settings, and the state the window, the HTTP server and the GPU worker
//! share.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::display::{Monitor, Screen};

pub const APP_ID: &str = "rtx-vsr";
pub const DEFAULT_PORT: u16 = 8730;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Master switch: when off, every fragment is passed through untouched.
    pub enabled: bool,
    /// VSR mode, 1 LOW … 4 ULTRA.
    pub quality: u8,
    /// Resolution to upscale to; `None` follows the largest display.
    pub target: Option<(u32, u32)>,
    /// Draw the "RTX VSR" indicator on video that is being upscaled.
    pub indicator: bool,
    /// When launched at login, start with the window minimised.
    pub start_minimized: bool,
    pub port: u16,
    /// CUDA device index.
    pub gpu: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            quality: 2,
            target: None,
            indicator: false,
            start_minimized: false,
            port: DEFAULT_PORT,
            gpu: 0,
        }
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(fallback),
    }
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

/// Where the app keeps what it downloads and generates.
pub fn data_dir() -> PathBuf {
    data_home().join(APP_ID)
}

fn settings_path() -> PathBuf {
    config_home().join(APP_ID).join("config.json")
}

impl Settings {
    pub fn load() -> Self {
        let mut s: Settings = std::fs::read(settings_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        s.quality = s.quality.clamp(1, 4);
        s
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path();
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The NVIDIA libraries the model runs on.
#[derive(Clone, Debug, PartialEq)]
pub enum Runtime {
    Missing,
    Downloading { done: u64, total: u64 },
    Unpacking,
    Ready(PathBuf),
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LastJob {
    pub src: (u32, u32),
    pub out: (u32, u32),
    pub mode: u8,
    pub requested: u8,
    /// Frames processed per second.
    pub fps: f64,
    /// Processing speed relative to playback speed.
    pub realtime: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// GPU the pipeline runs on, once it has been opened.
    pub gpu: Option<String>,
    pub error: Option<String>,
    pub last: Option<LastJob>,
    /// When the most recent fragment was upscaled.
    pub last_at: Option<Instant>,
    /// Why the most recent fragment was left alone, if it was.
    pub skipped: Option<String>,
    pub segments: u64,
    pub frames: u64,
    /// The mode was lowered because the GPU could not keep up.
    pub slowed_to: Option<u8>,
    /// When the browser extension last talked to us.
    pub extension_at: Option<Instant>,
    /// The screen of the page that last said it has upscaled video playing,
    /// and when it said so.
    pub watching: Option<(Screen, Instant)>,
}

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub monitors: Mutex<Vec<Monitor>>,
    pub stats: Mutex<Stats>,
    pub runtime: Mutex<Runtime>,
    /// The desktop has a tray and the app's icon is in it.
    pub tray: AtomicBool,
}

impl Shared {
    pub fn new(settings: Settings) -> Self {
        Shared {
            settings: Mutex::new(settings),
            monitors: Mutex::new(Vec::new()),
            stats: Mutex::new(Stats::default()),
            runtime: Mutex::new(Runtime::Missing),
            tray: AtomicBool::new(false),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
}
