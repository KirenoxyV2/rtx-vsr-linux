//! Displays: which ones there are, which resolutions the picker offers, and
//! how far to upscale a video shown on a particular one.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    /// Native pixels.
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// What a web page knows about the screen its window is on (CSS pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub width: f64,
    pub height: f64,
    pub dpr: f64,
}

/// Used when no display can be detected (headless sessions).
const FALLBACK: (u32, u32) = (1920, 1080);

/// Standard sizes offered below the display's own resolution.
const LADDER: [(u32, u32); 6] = [
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (5120, 2880),
    (7680, 4320),
];

fn pixels(size: (u32, u32)) -> u64 {
    size.0 as u64 * size.1 as u64
}

/// The resolution of the largest display.
pub fn largest(monitors: &[Monitor]) -> (u32, u32) {
    monitors
        .iter()
        .map(|m| (m.width, m.height))
        .max_by_key(|s| pixels(*s))
        .unwrap_or(FALLBACK)
}

/// Resolutions for the picker, smallest first: the standard sizes that fit
/// the largest display, plus every display's own. The last is the maximum.
pub fn choices(monitors: &[Monitor]) -> Vec<(u32, u32)> {
    let max = largest(monitors);
    let mut list: Vec<(u32, u32)> = LADDER
        .into_iter()
        .filter(|s| s.0 <= max.0 && s.1 <= max.1)
        .chain(monitors.iter().map(|m| (m.width, m.height)))
        .chain([max])
        .collect();
    list.sort_by_key(|s| (pixels(*s), s.0));
    list.dedup();
    list
}

/// The chosen resolution, held to what the displays can show.
pub fn target(chosen: Option<(u32, u32)>, monitors: &[Monitor]) -> (u32, u32) {
    let max = largest(monitors);
    match chosen {
        Some(c) => (c.0.min(max.0), c.1.min(max.1)),
        None => max,
    }
}

/// The display a page is on, judged by the screen size it reports.
pub fn find(monitors: &[Monitor], screen: Screen) -> Option<&Monitor> {
    if !(screen.width > 0.0 && screen.height > 0.0 && screen.dpr > 0.0) {
        return None;
    }
    let close = |a: f64, b: f64, tolerance: f64| (a - b).abs() <= tolerance * b.max(1.0);
    // The page's dpr includes its zoom level, so prefer a display whose own
    // scale explains the reported size; among several, the nearest scale.
    let logical = |m: &&Monitor| {
        let (w, h) = (m.width as f64 / m.scale, m.height as f64 / m.scale);
        (close(screen.width, w, 0.01) && close(screen.height, h, 0.01))
            || (close(screen.width, h, 0.01) && close(screen.height, w, 0.01))
    };
    let by_scale = |a: &&Monitor, b: &&Monitor| {
        let d = |m: &Monitor| (m.scale - screen.dpr).abs();
        d(a).total_cmp(&d(b))
            .then(pixels((b.width, b.height)).cmp(&pixels((a.width, a.height))))
    };
    if let Some(m) = monitors.iter().filter(logical).min_by(by_scale) {
        return Some(m);
    }
    // Otherwise (fractional scaling the windowing system reports differently)
    // go by the native size the page's numbers imply.
    let (w, h) = (screen.width * screen.dpr, screen.height * screen.dpr);
    monitors.iter().find(|m| {
        (close(w, m.width as f64, 0.02) && close(h, m.height as f64, 0.02))
            || (close(w, m.height as f64, 0.02) && close(h, m.width as f64, 0.02))
    })
}

/// How large to make video for a page on `screen`: the chosen resolution,
/// but never more than the display the page is on can show.
pub fn target_for(
    chosen: Option<(u32, u32)>,
    monitors: &[Monitor],
    screen: Option<Screen>,
) -> (u32, u32) {
    let wanted = target(chosen, monitors);
    match screen.and_then(|s| find(monitors, s)) {
        Some(m) => (wanted.0.min(m.width), wanted.1.min(m.height)),
        None => wanted,
    }
}

pub fn label(size: (u32, u32)) -> String {
    format!("{} × {}", size.0, size.1)
}

// ---- detection ----------------------------------------------------------------

/// Lists the displays by asking a short-lived copy of this program, which
/// can start its own windowing-system connection (the UI owns the only one
/// this process may have).
pub fn detect() -> Vec<Monitor> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let output = std::process::Command::new(exe)
        .arg("--list-monitors")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => serde_json::from_slice(&out.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `--list-monitors`: prints the displays as JSON and exits.
pub fn list_monitors_main() -> i32 {
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::WindowId;

    struct Probe(Vec<Monitor>);
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.0 = event_loop
                .available_monitors()
                .map(|m| {
                    let size = m.size();
                    Monitor {
                        name: m.name().unwrap_or_default(),
                        width: size.width,
                        height: size.height,
                        scale: m.scale_factor(),
                    }
                })
                .filter(|m| m.width > 0 && m.height > 0)
                .collect();
            event_loop.exit();
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    let Ok(event_loop) = EventLoop::new() else {
        return 1;
    };
    let mut probe = Probe(Vec::new());
    if event_loop.run_app(&mut probe).is_err() {
        return 1;
    }
    println!(
        "{}",
        serde_json::to_string(&probe.0).unwrap_or_else(|_| "[]".into())
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, width: u32, height: u32, scale: f64) -> Monitor {
        Monitor {
            name: name.into(),
            width,
            height,
            scale,
        }
    }

    #[test]
    fn picker_tops_out_at_the_largest_display() {
        let two = [monitor("A", 2560, 1440, 1.0), monitor("B", 1920, 1200, 1.0)];
        assert_eq!(
            choices(&two),
            [(1280, 720), (1920, 1080), (1920, 1200), (2560, 1440)]
        );
        assert_eq!(
            choices(&[monitor("A", 1920, 1080, 1.0)]),
            [(1280, 720), (1920, 1080)]
        );
        assert_eq!(
            choices(&[monitor("A", 3440, 1440, 1.0)]).last(),
            Some(&(3440, 1440))
        );
        assert_eq!(choices(&[]), [(1280, 720), (1920, 1080)]);
    }

    #[test]
    fn video_is_capped_to_the_display_it_is_on() {
        let two = [monitor("A", 2560, 1440, 1.0), monitor("B", 1920, 1200, 1.0)];
        let on = |w, h| {
            Some(Screen {
                width: w,
                height: h,
                dpr: 1.0,
            })
        };
        assert_eq!(target_for(None, &two, on(2560.0, 1440.0)), (2560, 1440));
        assert_eq!(target_for(None, &two, on(1920.0, 1200.0)), (1920, 1200));
        assert_eq!(
            target_for(Some((1920, 1080)), &two, on(2560.0, 1440.0)),
            (1920, 1080)
        );
        assert_eq!(
            target_for(Some((2560, 1440)), &two, on(1920.0, 1200.0)),
            (1920, 1200)
        );
        // unknown screen: the chosen resolution stands
        assert_eq!(target_for(None, &two, None), (2560, 1440));
        assert_eq!(target_for(None, &two, on(800.0, 600.0)), (2560, 1440));
        // a choice left over from a larger display that is gone
        assert_eq!(target_for(Some((3840, 2160)), &two, None), (2560, 1440));
    }

    #[test]
    fn scaled_displays_are_recognised() {
        let hidpi = [
            monitor("4K", 3840, 2160, 2.0),
            monitor("HD", 1920, 1080, 1.0),
        ];
        let page = |dpr| Screen {
            width: 1920.0,
            height: 1080.0,
            dpr,
        };
        assert_eq!(find(&hidpi, page(2.0)).unwrap().name, "4K");
        assert_eq!(find(&hidpi, page(1.0)).unwrap().name, "HD");
        // zoomed page on the 4K display: dpr 2.2 is nearer 2 than 1
        assert_eq!(find(&hidpi, page(2.2)).unwrap().name, "4K");
        // fractional scaling reported as an integer by the windowing system
        let fractional = [monitor("QHD", 2560, 1440, 2.0)];
        let page = Screen {
            width: 1706.67,
            height: 960.0,
            dpr: 1.5,
        };
        assert_eq!(find(&fractional, page).unwrap().name, "QHD");
    }
}
