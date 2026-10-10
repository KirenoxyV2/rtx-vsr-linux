//! RTX VSR for Linux: a local proxy that upscales browser video with
//! NVIDIA's Video Super Resolution model, and the window that controls it.

mod autostart;
mod browsers;
mod display;
mod engine;
mod gpu;
mod instance;
mod server;
mod state;
mod tray;
mod ui;

use std::sync::Arc;

use state::{Runtime, Settings, Shared};

const USAGE: &str = "\
RTX VSR: AI upscaling for browser video on NVIDIA RTX cards

Usage: rtx-vsr [options]

  (no options)     open the window; the proxy runs for as long as the app does,
                   which with a tray is until \"Quit\" in the tray icon's menu
  --autostart      as started at login: honours the \"Start minimized\" setting
  --headless       run the proxy without a window
  --port <n>       listen on this port instead of the configured one
  --install-runtime  download NVIDIA's VSR runtime (what the window's
                   Download button does) and exit
  --browsers       list the browsers found and whether they are connected
  --connect <b>    make a browser load the extension by itself; <b> is a name
                   from --browsers, or \"all\"
  --disconnect <b> undo --connect
  --version        print the version
  --help           print this text
";

struct Args {
    autostart: bool,
    headless: bool,
    port: Option<u16>,
    screenshot: Option<std::path::PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        autostart: false,
        headless: false,
        port: None,
        screenshot: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--autostart" => args.autostart = true,
            "--headless" => args.headless = true,
            "--port" => {
                let value = it.next().ok_or("--port needs a number")?;
                args.port = Some(value.parse().map_err(|_| format!("bad port: {value}"))?);
            }
            "--install-runtime" => std::process::exit(install_runtime_command()),
            "--browsers" => std::process::exit(browsers_command(None, false)),
            "--connect" | "--disconnect" => {
                let which = it.next().ok_or(format!("{arg} needs a browser name"))?;
                std::process::exit(browsers_command(Some(&which), arg == "--connect"));
            }
            // developer aid, not in the usage text: save a picture of the window and quit
            "--screenshot" => {
                args.screenshot = Some(it.next().ok_or("--screenshot needs a file")?.into())
            }
            "--list-monitors" => std::process::exit(display::list_monitors_main()),
            "--version" | "-V" => {
                println!("rtx-vsr {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option: {other}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

/// `--install-runtime`: the download, with progress on the terminal.
fn install_runtime_command() -> i32 {
    let shared = Arc::new(Shared::new(Settings::load()));
    let worker = {
        let shared = shared.clone();
        std::thread::spawn(move || engine::runtime::install(&shared))
    };
    let mut shown = u64::MAX;
    while !worker.is_finished() {
        if let Runtime::Downloading { done, total } = &*shared.runtime.lock().unwrap() {
            if done >> 24 != shown {
                shown = done >> 24;
                eprintln!("downloading… {} of {} MB", done >> 20, total >> 20);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let state = shared.runtime.lock().unwrap().clone();
    match state {
        Runtime::Ready(dir) => {
            println!("installed in {}", dir.display());
            0
        }
        Runtime::Failed(e) => {
            eprintln!("{e}");
            1
        }
        _ => 1,
    }
}

/// `--browsers`, `--connect` and `--disconnect`: what the window's Browsers
/// section does, for scripts.
fn browsers_command(which: Option<&str>, connect: bool) -> i32 {
    let mut found = browsers::detect();
    let Some(which) = which else {
        for b in &found {
            let state = if b.integrated {
                "connected"
            } else {
                "not connected"
            };
            println!(
                "{}\t{}\t{state}",
                b.file.trim_end_matches(".desktop"),
                b.name
            );
        }
        return 0;
    };
    let mut code = 1;
    for b in found
        .iter_mut()
        .filter(|b| which == "all" || b.file.trim_end_matches(".desktop") == which)
    {
        match if connect {
            b.integrate()
        } else {
            b.disconnect()
        } {
            Ok(()) => {
                println!(
                    "{}: {}",
                    b.name,
                    if connect {
                        "connected; restart it to apply"
                    } else {
                        "disconnected"
                    }
                );
                code = code.min(0);
            }
            Err(e) => {
                eprintln!("{}: {e}", b.name);
                code = 1;
            }
        }
    }
    if code != 0
        && !found
            .iter()
            .any(|b| which == "all" || b.file.trim_end_matches(".desktop") == which)
    {
        eprintln!("no such browser: {which} (see --browsers)");
    }
    code
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let mut settings = Settings::load();
    if let Some(port) = args.port {
        settings.port = port;
    }
    let port = settings.port;
    let start_minimized = args.autostart && settings.start_minimized;
    let shared = Arc::new(Shared::new(settings));

    if !args.headless {
        if !instance::claim() {
            println!("RTX VSR is already running; its window has been brought up.");
            return;
        }
        autostart::refresh();
        browsers::refresh();
        if args.screenshot.is_none() {
            tray::start(shared.clone());
        }
    }

    if let Some(libs) = engine::runtime::find() {
        *shared.runtime.lock().unwrap() = Runtime::Ready(libs);
    }
    *shared.monitors.lock().unwrap() = display::detect();
    let engine = Arc::new(engine::Engine::start(shared.clone()));
    let listening = server::start(shared.clone(), engine, port);

    let code = if args.headless {
        if let Err(e) = listening {
            eprintln!("rtx-vsr: {e}");
            std::process::exit(1);
        }
        eprintln!("rtx-vsr: listening on http://127.0.0.1:{port}");
        loop {
            std::thread::park();
        }
    } else {
        let screenshot = args.screenshot;
        let result = ui::run(ui::Options {
            shared,
            listening,
            start_minimized,
            screenshot,
        });
        instance::release();
        match result {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("rtx-vsr: cannot open a window: {e}");
                1
            }
        }
    };
    // NVIDIA's libraries do not survive an orderly unload (their teardown
    // double-frees), so leave without running it.
    unsafe { libc::_exit(code) }
}
