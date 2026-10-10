//! The window: a control panel for the proxy running in the same process.
//!
//! The app is an iced daemon with at most one window, so that a second
//! launch can bring up a fresh window (a minimised one cannot be raised on
//! Wayland). Closing the window quits, which also stops the proxy.

use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{
    button, column, container, pick_list, progress_bar, row, scrollable, space, text, toggler,
};
use iced::window;
use iced::{
    Background, Border, Center, Color, Element, Fill, Font, Size, Subscription, Task, Theme,
};

use crate::autostart;
use crate::browsers::{self, Browser};
use crate::display::{self, Monitor};
use crate::engine::pipeline::MODE_NAMES;
use crate::engine::runtime;
use crate::gpu::nvml::{self, GpuInfo};
use crate::server::WATCHING_FOR;
use crate::state::{Runtime, Settings, Shared, Stats, APP_ID};

const GREEN: Color = Color::from_rgb8(0x76, 0xb9, 0x00);
const SURFACE: Color = Color::from_rgb8(0x1a, 0x1f, 0x19);
const OUTLINE: Color = Color::from_rgb8(0x2c, 0x35, 0x2a);
const MUTED: Color = Color::from_rgb8(0x9a, 0xa5, 0x96);
const AMBER: Color = Color::from_rgb8(0xe3, 0xa0, 0x2f);
const RED: Color = Color::from_rgb8(0xe5, 0x5b, 0x4d);
const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};

const WINDOW_ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");
/// Size of the runtime download, for the button's label.
const DOWNLOAD_MB: u32 = 423;

pub struct Options {
    pub shared: Arc<Shared>,
    /// Why the proxy is not listening, if it is not.
    pub listening: Result<(), String>,
    pub start_minimized: bool,
    /// Developer aid: save a picture of the window here, then quit.
    pub screenshot: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Opened(window::Id),
    Closed(window::Id),
    Enabled(bool),
    Target(Resolution),
    Quality(Quality),
    Indicator(bool),
    Autostart(bool),
    StartMinimized(bool),
    Integrate(usize, bool),
    ShowExtension,
    DownloadRuntime,
    Shot(window::Screenshot),
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    size: (u32, u32),
    note: String,
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", display::label(self.size), self.note)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quality(u8);

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            1 => "Low — fastest",
            2 => "Medium",
            3 => "High",
            _ => "Ultra — sharpest, slowest",
        })
    }
}

const QUALITIES: [Quality; 4] = [Quality(1), Quality(2), Quality(3), Quality(4)];

struct App {
    shared: Arc<Shared>,
    listening: Result<(), String>,
    window: Option<window::Id>,
    minimize_when_open: bool,
    settings: Settings,
    monitors: Vec<Monitor>,
    autostart: bool,
    browsers: Vec<(Browser, bool)>,
    gpu: Result<GpuInfo, String>,
    stats: Stats,
    runtime: Runtime,
    /// What went wrong with the last thing the user asked for.
    notice: Option<String>,
    ticks: u64,
    screenshot: Option<std::path::PathBuf>,
}

pub fn run(options: Options) -> iced::Result {
    let options = std::cell::RefCell::new(Some(options));
    iced::daemon(
        move || App::new(options.borrow_mut().take().expect("booted twice")),
        App::update,
        App::view,
    )
    .title(|_: &App, _| "RTX VSR".to_string())
    .theme(|_: &App, _| theme())
    .subscription(App::subscription)
    .run()
}

fn theme() -> Theme {
    Theme::custom(
        "RTX VSR",
        iced::theme::Palette {
            background: Color::from_rgb8(0x11, 0x14, 0x10),
            text: Color::from_rgb8(0xe9, 0xed, 0xe6),
            primary: GREEN,
            success: GREEN,
            warning: AMBER,
            danger: RED,
        },
    )
}

impl App {
    fn new(options: Options) -> (Self, Task<Message>) {
        let shared = options.shared;
        let settings = shared.settings();
        let monitors = shared.monitors.lock().unwrap().clone();
        let runtime = shared.runtime.lock().unwrap().clone();
        let mut app = App {
            listening: options.listening,
            window: None,
            minimize_when_open: options.start_minimized,
            monitors,
            autostart: autostart::is_enabled(),
            browsers: Vec::new(),
            gpu: nvml::query(settings.gpu),
            stats: Stats::default(),
            runtime,
            settings,
            shared,
            notice: None,
            ticks: 0,
            screenshot: options.screenshot,
        };
        app.scan_browsers();
        let open = app.open_window();
        (app, open)
    }

    fn open_window(&mut self) -> Task<Message> {
        // A screenshot is of the whole panel, so make room for all of it.
        let height = if self.screenshot.is_some() {
            1280.0
        } else {
            780.0
        };
        let (id, open) = window::open(window::Settings {
            size: Size::new(460.0, height),
            min_size: Some(Size::new(420.0, 360.0)),
            icon: window::icon::from_rgba(WINDOW_ICON.to_vec(), 64, 64).ok(),
            platform_specific: window::settings::PlatformSpecific {
                application_id: APP_ID.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        self.window = Some(id);
        open.map(Message::Opened)
    }

    fn scan_browsers(&mut self) {
        self.browsers = browsers::detect()
            .into_iter()
            .map(|browser| {
                let loaded = browser.integrated && browser.loaded();
                (browser, loaded)
            })
            .collect();
    }

    /// Applies a change to the settings everywhere: here, for the proxy, on disk.
    fn change(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        *self.shared.settings.lock().unwrap() = self.settings.clone();
        self.notice = self.settings.save().err();
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
            window::open_events().map(Message::Opened),
            window::close_events().map(Message::Closed),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                self.ticks += 1;
                self.stats = self.shared.stats.lock().unwrap().clone();
                self.runtime = self.shared.runtime.lock().unwrap().clone();
                self.monitors = self.shared.monitors.lock().unwrap().clone();
                if self.ticks.is_multiple_of(4) {
                    self.gpu = nvml::query(self.settings.gpu);
                }
                if self.ticks.is_multiple_of(6) {
                    self.scan_browsers();
                }
                if self.ticks.is_multiple_of(20) {
                    // Displays come and go; look again off the UI thread.
                    let shared = self.shared.clone();
                    std::thread::spawn(move || {
                        let found = display::detect();
                        if !found.is_empty() {
                            *shared.monitors.lock().unwrap() = found;
                        }
                    });
                }
                if let (Some(_), Some(id), 4) = (&self.screenshot, self.window, self.ticks) {
                    return window::screenshot(id).map(Message::Shot);
                }
                if self.shared.show_window.swap(false, Ordering::Relaxed) {
                    // Launched again: a new window is the one thing every
                    // desktop will put in front of the user.
                    let old = self.window.take();
                    let open = self.open_window();
                    return match old {
                        Some(old) => Task::batch([window::close(old), open]),
                        None => open,
                    };
                }
            }
            Message::Opened(id) => {
                if Some(id) == self.window && std::mem::take(&mut self.minimize_when_open) {
                    return window::minimize(id, true);
                }
            }
            Message::Closed(id) => {
                if Some(id) == self.window {
                    return iced::exit();
                }
            }
            Message::Enabled(on) => self.change(|s| s.enabled = on),
            Message::Target(choice) => {
                // The largest entry means "whatever the largest display is".
                let follow = choice.size == display::largest(&self.monitors);
                self.change(|s| s.target = if follow { None } else { Some(choice.size) });
            }
            Message::Quality(q) => self.change(|s| s.quality = q.0),
            Message::Indicator(on) => self.change(|s| s.indicator = on),
            Message::Autostart(on) => {
                self.notice = autostart::set(on).err();
                self.autostart = autostart::is_enabled();
            }
            Message::StartMinimized(on) => self.change(|s| s.start_minimized = on),
            Message::Integrate(index, on) => {
                if let Some((browser, loaded)) = self.browsers.get_mut(index) {
                    let result = if on {
                        browser.integrate()
                    } else {
                        browser.disconnect()
                    };
                    *loaded = browser.integrated && browser.loaded();
                    self.notice = result.err();
                }
            }
            Message::ShowExtension => match browsers::export_extension() {
                Ok(dir) => {
                    self.notice = std::process::Command::new("xdg-open")
                        .arg(&dir)
                        .spawn()
                        .err()
                        .map(|_| format!("The extension is in {}", dir.display()));
                }
                Err(e) => self.notice = Some(e),
            },
            Message::DownloadRuntime => {
                if !matches!(
                    self.runtime,
                    Runtime::Downloading { .. } | Runtime::Unpacking
                ) {
                    self.runtime = Runtime::Downloading { done: 0, total: 0 };
                    *self.shared.runtime.lock().unwrap() = self.runtime.clone();
                    let shared = self.shared.clone();
                    std::thread::spawn(move || runtime::install(&shared));
                }
            }
            Message::Shot(shot) => {
                if let Some(path) = self.screenshot.take() {
                    if let Err(e) = write_png(&path, shot.size.width, shot.size.height, &shot.rgba)
                    {
                        eprintln!("rtx-vsr: {}: {e}", path.display());
                    }
                }
                return iced::exit();
            }
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }

    // ---- view -----------------------------------------------------------------

    fn headline(&self) -> (&'static str, Color, Option<String>) {
        if let Err(e) = &self.listening {
            return ("Not running", RED, Some(e.clone()));
        }
        if let Err(e) = &self.gpu {
            return ("No GPU", RED, Some(e.clone()));
        }
        match &self.runtime {
            Runtime::Ready(_) => {}
            Runtime::Downloading { .. } | Runtime::Unpacking => return ("Setting up", AMBER, None),
            _ => {
                let why =
                    "The NVIDIA runtime is needed before video can be upscaled — see Status below.";
                return ("Setup needed", AMBER, Some(why.into()));
            }
        }
        if !self.settings.enabled {
            return ("Off", MUTED, None);
        }
        if let Some(e) = &self.stats.error {
            return ("Error", RED, Some(e.clone()));
        }
        if self
            .stats
            .last_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            return ("Upscaling", GREEN, None);
        }
        ("Ready", GREEN, None)
    }

    fn view(&self, _window: window::Id) -> Element<'_, Message> {
        let (state, colour, detail) = self.headline();

        let logo = container(
            column![
                text("RTX").size(13).font(BOLD),
                text("VSR").size(13).font(BOLD)
            ]
            .align_x(Center),
        )
        .center(44)
        .style(|_| container::Style {
            background: Some(Background::Color(GREEN)),
            text_color: Some(Color::WHITE),
            border: Border {
                radius: 9.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
        let pill = container(text(state).size(13).font(BOLD))
            .padding([4, 12])
            .style(move |_| container::Style {
                background: Some(Background::Color(Color { a: 0.16, ..colour })),
                text_color: Some(colour),
                border: Border {
                    radius: 99.0.into(),
                    width: 1.0,
                    color: Color { a: 0.5, ..colour },
                },
                ..container::Style::default()
            });
        let header = row![
            logo,
            column![
                text("RTX Video Super Resolution").size(17).font(BOLD),
                hint("AI upscaling for browser video")
            ]
            .spacing(1)
            .width(Fill),
            pill,
        ]
        .spacing(12)
        .align_y(Center);

        let mut page = column![header].spacing(14).padding(18);
        if let Some(detail) = detail {
            page = page.push(text(detail).size(13).color(colour));
        }
        if let Some(notice) = &self.notice {
            page = page.push(text(notice).size(13).color(AMBER));
        }
        page = page
            .push(self.upscaling())
            .push(self.startup())
            .push(self.browser_card())
            .push(self.status())
            .push(
                row![
                    hint(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                    space::horizontal(),
                    button(text("Quit").size(13))
                        .style(button::secondary)
                        .on_press(Message::Quit),
                ]
                .align_y(Center),
            );
        scrollable(page).height(Fill).into()
    }

    fn upscaling(&self) -> Element<'_, Message> {
        let max = display::largest(&self.monitors);
        let choices: Vec<Resolution> = display::choices(&self.monitors)
            .into_iter()
            .map(|size| {
                let on: Vec<&str> = self
                    .monitors
                    .iter()
                    .filter(|m| (m.width, m.height) == size)
                    .map(|m| m.name.as_str())
                    .collect();
                let note = match (size == max, on.is_empty()) {
                    (true, true) => " — display".to_string(),
                    (true, false) => format!(" — {} (maximum)", on.join(", ")),
                    (false, false) => format!(" — {}", on.join(", ")),
                    (false, true) => String::new(),
                };
                Resolution { size, note }
            })
            .collect();
        let target = display::target(self.settings.target, &self.monitors);
        let selected = choices.iter().find(|c| c.size == target).cloned();

        let mut card = column![
            switch(
                "Upscale video",
                "Video below the chosen resolution is upscaled as it plays.",
                self.settings.enabled,
                Message::Enabled
            ),
            setting(
                "Upscale to",
                pick_list(choices, selected, Message::Target)
                    .width(250)
                    .text_size(14)
            ),
        ]
        .spacing(14);
        // While video is playing on a smaller display, say what happens to it.
        let watched = self
            .stats
            .watching
            .filter(|(_, at)| at.elapsed() < WATCHING_FOR)
            .and_then(|(screen, _)| {
                display::held_back(self.settings.target, &self.monitors, screen)
            });
        if let Some((monitor, size)) = watched {
            card = card.push(hint(format!(
                "The video playing on {} is upscaled to {}, that display's resolution.",
                monitor.name,
                display::label(size)
            )));
        }
        card = card
            .push(setting(
                "Quality",
                pick_list(
                    QUALITIES,
                    Some(Quality(self.settings.quality)),
                    Message::Quality,
                )
                .width(250)
                .text_size(14),
            ))
            .push(switch(
                "Show RTX VSR indicator",
                "Marks the top right corner of a video while it is being shown upscaled.",
                self.settings.indicator,
                Message::Indicator,
            ));
        section("Upscaling", card)
    }

    fn startup(&self) -> Element<'_, Message> {
        let mut card = column![switch(
            "Start at login",
            "Have upscaling ready whenever you are signed in.",
            self.autostart,
            Message::Autostart,
        )]
        .spacing(14);
        if self.autostart {
            card = card.push(switch(
                "Start minimized",
                "At login, keep the window out of the way.",
                self.settings.start_minimized,
                Message::StartMinimized,
            ));
        }
        section("Startup", card)
    }

    fn browser_card(&self) -> Element<'_, Message> {
        let mut card = column![].spacing(14);
        if self.browsers.is_empty() {
            card = card.push(hint(
                "No Chromium-based browser was found. The extension can still be loaded by hand.",
            ));
        }
        for (index, (browser, loaded)) in self.browsers.iter().enumerate() {
            let state = match (browser.integrated, *loaded) {
                (false, _) => {
                    "Loads the extension each time the browser starts. No developer mode needed."
                }
                (true, true) => "Connected — the extension is loaded.",
                (true, false) => "Restart the browser to finish connecting.",
            };
            card = card.push(switch(
                format!("Connect {}", browser.name),
                state,
                browser.integrated,
                move |on| Message::Integrate(index, on),
            ));
        }
        let seen = match self.stats.extension_at.map(|t| t.elapsed().as_secs()) {
            Some(s) if s < 90 => "A browser is talking to the app.".to_string(),
            Some(s) => format!("A browser last talked to the app {} min ago.", s / 60),
            None => "No browser has talked to the app yet.".to_string(),
        };
        card = card.push(
            row![
                hint(seen).width(Fill),
                button(text("Extension folder").size(13))
                    .style(button::secondary)
                    .on_press(Message::ShowExtension),
            ]
            .spacing(12)
            .align_y(Center),
        );
        section("Browsers", card)
    }

    fn status(&self) -> Element<'_, Message> {
        let gpu = match &self.gpu {
            Ok(g) => format!(
                "{} · driver {} · {:.1} of {:.1} GB free",
                self.stats.gpu.as_deref().unwrap_or(&g.name),
                g.driver,
                g.free_mb as f64 / 1024.0,
                g.total_mb as f64 / 1024.0
            ),
            Err(e) => e.clone(),
        };
        let mut card = column![fact("Graphics card", gpu)].spacing(12);

        card = card.push(match &self.runtime {
            Runtime::Ready(_) => fact("NVIDIA runtime", "Installed".to_string()),
            Runtime::Downloading { done, total } => {
                let total = (*total).max(*done).max(1);
                column![
                    fact("NVIDIA runtime", format!("Downloading… {} of {} MB", done >> 20, total >> 20)),
                    progress_bar(0.0..=total as f32, *done as f32).girth(6),
                ]
                .spacing(8)
                .into()
            }
            Runtime::Unpacking => fact("NVIDIA runtime", "Unpacking…".to_string()),
            missing => {
                let why = match missing {
                    Runtime::Failed(e) => e.clone(),
                    _ => "The VSR model ships in NVIDIA's VFX SDK, which is fetched from NVIDIA (pypi.nvidia.com) under NVIDIA's licence.".to_string(),
                };
                column![
                    fact("NVIDIA runtime", "Not installed".to_string()),
                    hint(why),
                    button(text(format!("Download ({DOWNLOAD_MB} MB)")).size(14)).on_press(Message::DownloadRuntime),
                ]
                .spacing(8)
                .into()
            }
        });

        let last = match &self.stats.last {
            Some(l) => {
                let mut line = format!(
                    "{} → {} · {} · {:.0} fps ({:.1}× playback speed)",
                    display::label(l.src),
                    display::label(l.out),
                    MODE_NAMES[l.mode as usize],
                    l.fps,
                    l.realtime
                );
                if l.mode < l.requested {
                    line += match self.stats.slowed_to {
                        Some(_) => " — quality lowered to keep up",
                        None => " — quality lowered to fit GPU memory",
                    };
                }
                line
            }
            None => "Nothing yet".to_string(),
        };
        card = card.push(fact("Last upscaled", last));
        if let Some(why) = &self.stats.skipped {
            card = card.push(fact("Last left as is", why.clone()));
        }
        card = card.push(fact(
            "This session",
            format!(
                "{} segments, {} frames",
                self.stats.segments, self.stats.frames
            ),
        ));
        card = card.push(fact(
            "Proxy",
            match &self.listening {
                Ok(()) => format!("127.0.0.1:{}", self.settings.port),
                Err(e) => e.clone(),
            },
        ));
        section("Status", card)
    }
}

// ---- building blocks ------------------------------------------------------------

fn hint<'a>(content: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(content).size(12).color(MUTED)
}

fn section<'a>(title: &'a str, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(column![text(title).size(12).font(BOLD).color(GREEN), body.into()].spacing(12))
        .padding(16)
        .width(Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(SURFACE)),
            border: Border {
                radius: 12.0.into(),
                width: 1.0,
                color: OUTLINE,
            },
            ..container::Style::default()
        })
        .into()
}

/// A labelled on/off switch with a line of explanation.
fn switch<'a>(
    label: impl text::IntoFragment<'a>,
    explanation: impl text::IntoFragment<'a>,
    on: bool,
    message: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    row![
        column![text(label).size(14), hint(explanation)]
            .spacing(2)
            .width(Fill),
        toggler(on).on_toggle(message).size(22),
    ]
    .spacing(12)
    .align_y(Center)
    .into()
}

fn setting<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).size(14).width(Fill), control.into()]
        .spacing(12)
        .align_y(Center)
        .into()
}

fn fact<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    column![hint(label), text(value).size(13)].spacing(2).into()
}

fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for line in rgba.chunks_exact(width as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(line);
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(&raw)?;
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (b"IHDR", header),
        (b"IDAT", zlib.finish()?),
        (b"IEND", Vec::new()),
    ] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(&data);
        png.extend_from_slice(kind);
        png.extend_from_slice(&data);
        png.extend_from_slice(&crc.sum().to_be_bytes());
    }
    std::fs::write(path, png)
}
