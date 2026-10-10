//! The tray icon (a StatusNotifierItem): where the app stays while its
//! window is closed. On a desktop without a tray nothing is shown, and
//! `Shared::tray` tells the window that closing it has to mean quitting.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};

use crate::state::{Shared, APP_ID};
use crate::ui::{self, Request};

const ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");

struct Tray {
    shared: Arc<Shared>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "RTX VSR".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // The tray wants ARGB.
        let data = ICON
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, a]| [a, r, g, b])
            .collect();
        vec![ksni::Icon {
            width: 64,
            height: 64,
            data,
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        ui::request(Request::Show);
    }

    // Having this makes the menu be read again each time it is opened, so
    // the checkmark follows the switch in the window.
    fn menu_about_to_show(&mut self) {}

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open RTX VSR".into(),
                activate: Box::new(|_| ui::request(Request::Show)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Upscale video".into(),
                checked: self.shared.settings().enabled,
                activate: Box::new(|tray: &mut Self| {
                    let mut settings = tray.shared.settings.lock().unwrap();
                    settings.enabled = !settings.enabled;
                    let _ = settings.save();
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_| ui::request(Request::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_online(&self) {
        self.shared.tray.store(true, Ordering::Relaxed);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.shared.tray.store(false, Ordering::Relaxed);
        true // stay around: the desktop's tray may (re)appear
    }
}

/// Puts the icon in the tray, now or whenever the desktop gets one.
pub fn start(shared: Arc<Shared>) {
    // No tray yet is reported through `watcher_offline` before `spawn` returns.
    shared.tray.store(true, Ordering::Relaxed);
    let tray = Tray {
        shared: shared.clone(),
    };
    if let Err(e) = tray.assume_sni_available(true).spawn() {
        shared.tray.store(false, Ordering::Relaxed);
        eprintln!("rtx-vsr: no tray icon: {e}");
    }
}
