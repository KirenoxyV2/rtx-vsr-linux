//! The local HTTP endpoint the browser extension talks to.
//!
//!   POST /transform?sw=&sh=&dpr=   body: init segment + one fragment
//!        200  upscaled fMP4; the first X-VSR-Init-Length bytes are its init
//!             segment, X-VSR-Meta describes what was done
//!        204  leave the original alone (X-VSR-Meta says why)
//!   GET  /config   what the extension needs to know about the app's settings;
//!        with ?playing=1&sw=&sh=&dpr= the page also reports that upscaled
//!        video is playing on that screen
//!   GET  /health   liveness, for the extension's popup
//!   GET  /status   what the pipeline has been doing
//!
//! Only this machine can connect, and of the pages running on it only
//! browser extensions may: a web site must not be able to drive the GPU.

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::display::{self, Screen};
use crate::engine::pipeline::MODE_NAMES;
use crate::engine::{Engine, Outcome};
use crate::gpu::nvml;
use crate::state::Shared;

const THREADS: usize = 4;
/// Largest request body accepted; a fragment is a few MB.
const MAX_BODY: usize = 256 << 20;
/// "Active" in /status for this long after a fragment was upscaled.
const ACTIVE_FOR: Duration = Duration::from_secs(5);
/// Upscaled video counts as playing for this long after a page said so
/// (pages repeat it every two seconds).
pub const WATCHING_FOR: Duration = Duration::from_secs(5);

pub fn start(shared: Arc<Shared>, engine: Arc<Engine>, port: u16) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| {
        let in_use = e
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse);
        if in_use {
            format!(
                "Port {port} is taken by another program. If the old Python proxy (vsr_proxy.py) \
                 is still running, stop it and start RTX VSR again."
            )
        } else {
            format!("Cannot listen on 127.0.0.1:{port}: {e}")
        }
    })?;
    let server = Arc::new(server);
    for i in 0..THREADS {
        let (server, shared, engine) = (server.clone(), shared.clone(), engine.clone());
        std::thread::Builder::new()
            .name(format!("http-{i}"))
            .spawn(move || {
                while let Ok(request) = server.recv() {
                    handle(request, &shared, &engine, port);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn header(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_owned())
}

fn query(url: &str, name: &str) -> Option<f64> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            value.parse().ok()
        } else {
            None
        }
    })
}

/// The screen of the page a request is made for, if it says.
fn screen(url: &str) -> Option<Screen> {
    match (query(url, "sw"), query(url, "sh"), query(url, "dpr")) {
        (Some(width), Some(height), dpr) => Some(Screen {
            width,
            height,
            dpr: dpr.unwrap_or(1.0),
        }),
        _ => None,
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
}

impl Reply {
    fn text(status: u16, text: &str) -> Self {
        Reply {
            status,
            body: text.as_bytes().to_vec(),
            content_type: "text/plain; charset=utf-8",
            headers: Vec::new(),
        }
    }
    fn json(value: serde_json::Value) -> Self {
        Reply {
            status: 200,
            body: value.to_string().into_bytes(),
            content_type: "application/json",
            headers: vec![("Cache-Control", "no-store".into())],
        }
    }
}

fn handle(mut request: Request, shared: &Shared, engine: &Engine, port: u16) {
    let origin = header(&request, "Origin");
    let from_extension = origin
        .as_deref()
        .is_some_and(|o| o.starts_with("chrome-extension://") || o.starts_with("moz-extension://"));
    let own = [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ];
    let host = header(&request, "Host").unwrap_or_default();
    let allowed = own.iter().any(|o| o.ends_with(&format!("//{host}")))
        && match origin.as_deref() {
            None => true,
            Some(o) => from_extension || own.iter().any(|x| x == o),
        };

    let reply = if !allowed {
        Reply::text(403, "forbidden")
    } else {
        if from_extension {
            shared.stats.lock().unwrap().extension_at = Some(Instant::now());
        }
        let url = request.url().to_owned();
        let path = url.split('?').next().unwrap_or("");
        match (request.method(), path) {
            (Method::Options, _) => Reply::text(204, ""),
            (Method::Get, "/") => Reply::text(200, "RTX VSR is running.\n"),
            (Method::Get, "/health") => health(shared),
            (Method::Get, "/config") => config(&url, shared),
            (Method::Get, "/status") => status(shared),
            (Method::Post, "/transform") => transform(&mut request, &url, shared, engine),
            _ => Reply::text(404, "not found"),
        }
    };

    let mut response = Response::from_data(reply.body).with_status_code(reply.status);
    let mut add = |name: &str, value: &str| {
        if let Ok(h) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(h);
        }
    };
    add("Content-Type", reply.content_type);
    if let Some(origin) = origin.as_deref().filter(|_| allowed) {
        add("Access-Control-Allow-Origin", origin);
        add("Vary", "Origin");
        add("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
        add("Access-Control-Allow-Headers", "Content-Type");
        add(
            "Access-Control-Expose-Headers",
            "X-VSR-Meta, X-VSR-Init-Length",
        );
        add("Access-Control-Allow-Private-Network", "true");
        add("Access-Control-Max-Age", "86400");
    }
    for (name, value) in &reply.headers {
        add(name, value);
    }
    let _ = request.respond(response);
}

fn health(shared: &Shared) -> Reply {
    let settings = shared.settings();
    let stats = shared.stats.lock().unwrap().clone();
    let gpu = nvml::query(settings.gpu);
    let runtime = matches!(
        &*shared.runtime.lock().unwrap(),
        crate::state::Runtime::Ready(_)
    );
    let err = match (&gpu, runtime) {
        (Err(e), _) => Some(e.clone()),
        (_, false) => Some("the NVIDIA runtime is not installed: open the RTX VSR app".to_string()),
        _ => stats.error,
    };
    let gpu = gpu.unwrap_or_default();
    Reply::json(json!({
        "app": "rtx-vsr",
        "version": env!("CARGO_PKG_VERSION"),
        "ok": err.is_none(),
        "enabled": settings.enabled,
        "gpu": stats.gpu.unwrap_or(gpu.name),
        "free_mb": gpu.free_mb,
        "total_mb": gpu.total_mb,
        "err": err,
    }))
}

fn config(url: &str, shared: &Shared) -> Reply {
    if query(url, "playing") == Some(1.0) {
        if let Some(screen) = screen(url) {
            shared.stats.lock().unwrap().watching = Some((screen, Instant::now()));
        }
    }
    let settings = shared.settings();
    let monitors = shared.monitors.lock().unwrap().clone();
    let target = display::target(settings.target, &monitors);
    Reply::json(json!({
        "app": "rtx-vsr",
        "version": env!("CARGO_PKG_VERSION"),
        "enabled": settings.enabled,
        "indicator": settings.indicator,
        "quality": settings.quality,
        "target": [target.0, target.1],
        "displays": monitors,
    }))
}

fn status(shared: &Shared) -> Reply {
    let stats = shared.stats.lock().unwrap().clone();
    let size = |s: (u32, u32)| format!("{}x{}", s.0, s.1);
    let last = stats.last.as_ref();
    Reply::json(json!({
        "active": stats.last_at.is_some_and(|t| t.elapsed() < ACTIVE_FOR),
        "watching": stats.watching.is_some_and(|(_, t)| t.elapsed() < WATCHING_FOR),
        "gpu": stats.gpu,
        "error": stats.error,
        "skipped": stats.skipped,
        "total_transforms": stats.segments,
        "total_frames": stats.frames,
        "last_in": last.map(|l| size(l.src)),
        "last_out": last.map(|l| size(l.out)),
        "last_mode": last.map(|l| MODE_NAMES[l.mode as usize]),
        "downshifted": last.is_some_and(|l| l.mode < l.requested),
        "fps": last.map(|l| l.fps),
    }))
}

fn transform(request: &mut Request, url: &str, shared: &Shared, engine: &Engine) -> Reply {
    let settings = shared.settings();
    let skip = |why: &str, src: Option<(u32, u32)>| {
        Reply {
        status: 204,
        body: Vec::new(),
        content_type: "application/octet-stream",
        headers: vec![(
            "X-VSR-Meta",
            json!({ "passthrough": true, "note": why, "in": src.map(|s| format!("{}x{}", s.0, s.1)) }).to_string(),
        )],
    }
    };
    if !settings.enabled {
        return skip("upscaling is switched off", None);
    }
    let length = request.body_length().unwrap_or(0);
    if length == 0 || length > MAX_BODY {
        return Reply::text(400, "expected an init segment and a fragment in the body");
    }
    let mut data = Vec::with_capacity(length);
    if request
        .as_reader()
        .take(MAX_BODY as u64)
        .read_to_end(&mut data)
        .is_err()
    {
        return Reply::text(400, "could not read the request body");
    }

    let target = display::target_for(
        settings.target,
        &shared.monitors.lock().unwrap(),
        screen(url),
    );

    match engine.transform(data, settings.quality, target) {
        Ok(Outcome::Passthrough { why, src }) => skip(&why, src),
        Ok(Outcome::Done(out)) => {
            let meta = json!({
                "in": format!("{}x{}", out.src.0, out.src.1),
                "out": format!("{}x{}", out.out.0, out.out.1),
                "mode": out.mode,
                "mode_name": MODE_NAMES[out.mode as usize],
                "mode_requested": settings.quality,
                "downshifted": out.mode < settings.quality,
                "frames": out.frames,
                "ms": (out.work_seconds * 1000.0).round(),
                "init_len": out.init_len,
                "indicator": settings.indicator,
            });
            Reply {
                status: 200,
                body: out.bytes,
                content_type: "video/mp4",
                headers: vec![
                    ("X-VSR-Meta", meta.to_string()),
                    ("X-VSR-Init-Length", out.init_len.to_string()),
                ],
            }
        }
        Err(e) => {
            eprintln!("rtx-vsr: transform failed: {e}");
            Reply::text(500, &e)
        }
    }
}
