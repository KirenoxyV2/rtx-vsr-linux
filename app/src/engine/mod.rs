//! The GPU worker: one thread that owns the CUDA context and everything
//! built on it, fed through a channel. Requests are served one at a time, so
//! concurrent fragments never share decoder, model or encoder state.

pub mod h264;
pub mod mp4;
pub mod pipeline;
pub mod runtime;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gpu::Gpu;
use crate::state::{LastJob, Runtime, Shared};
pub use pipeline::Outcome;
use pipeline::Session;

/// GPU memory held for a stream is released after this long without work.
const IDLE: Duration = Duration::from_secs(45);
/// Streams kept warm at once (two videos playing side by side).
const MAX_SESSIONS: usize = 2;
/// How long to wait before trying to open the GPU again after a failure.
const RETRY: Duration = Duration::from_secs(20);

struct Job {
    data: Vec<u8>,
    quality: u8,
    target: (u32, u32),
    reply: mpsc::Sender<Result<Outcome, String>>,
}

pub struct Engine {
    jobs: mpsc::Sender<Job>,
}

impl Engine {
    pub fn start(shared: Arc<Shared>) -> Self {
        let (jobs, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("gpu".into())
            .spawn(move || Worker::new(shared).run(rx))
            .expect("cannot start the GPU thread");
        Engine { jobs }
    }

    /// Upscales one fragment (init segment + `moof`/`mdat`); blocks until done.
    pub fn transform(
        &self,
        data: Vec<u8>,
        quality: u8,
        target: (u32, u32),
    ) -> Result<Outcome, String> {
        let (reply, result) = mpsc::channel();
        self.jobs
            .send(Job {
                data,
                quality,
                target,
                reply,
            })
            .map_err(|_| "the GPU thread has stopped".to_string())?;
        result
            .recv()
            .map_err(|_| "the GPU thread has stopped".to_string())?
    }
}

struct Worker {
    shared: Arc<Shared>,
    gpu: Option<Gpu>,
    gpu_failed: Option<(Instant, String)>,
    sessions: Vec<(u64, Session)>,
    /// Highest mode the GPU has kept up with, and for which requested mode.
    speed_cap: (u8, u8),
    slow_streak: u32,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Worker {
            shared,
            gpu: None,
            gpu_failed: None,
            sessions: Vec::new(),
            speed_cap: (4, 0),
            slow_streak: 0,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Job>) {
        loop {
            match rx.recv_timeout(IDLE) {
                Ok(job) => {
                    // A bug in here must not take the proxy down with it.
                    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.handle(&job)
                    }));
                    let result = attempt.unwrap_or_else(|_| {
                        self.sessions.clear();
                        Err("internal error while processing a fragment".into())
                    });
                    self.record(&job, &result);
                    let _ = job.reply.send(result);
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.sessions.clear();
                    self.speed_cap = (4, 0);
                    self.shared.stats.lock().unwrap().slowed_to = None;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn open_gpu(&mut self) -> Result<Option<&Gpu>, String> {
        if self.gpu.is_none() {
            if let Some((at, why)) = &self.gpu_failed {
                if at.elapsed() < RETRY {
                    return Err(why.clone());
                }
            }
            let libs = match &*self.shared.runtime.lock().unwrap() {
                Runtime::Ready(dir) => dir.clone(),
                _ => return Ok(None),
            };
            let device = self.shared.settings.lock().unwrap().gpu as i32;
            match Gpu::open(&libs, device) {
                Ok(gpu) => {
                    self.shared.stats.lock().unwrap().gpu = Some(gpu.name.clone());
                    self.gpu_failed = None;
                    self.gpu = Some(gpu);
                }
                Err(e) => {
                    self.gpu_failed = Some((Instant::now(), e.clone()));
                    return Err(e);
                }
            }
        }
        Ok(self.gpu.as_ref())
    }

    fn handle(&mut self, job: &Job) -> Result<Outcome, String> {
        let Some(key) = pipeline::stream_key(&job.data) else {
            return Ok(Outcome::Passthrough {
                why: "no init segment".into(),
                src: None,
            });
        };
        if self.speed_cap.1 != job.quality {
            self.speed_cap = (4, job.quality);
            self.slow_streak = 0;
        }
        let quality = job.quality.min(self.speed_cap.0);
        if self.open_gpu()?.is_none() {
            let why = "the NVIDIA runtime is not installed".into();
            return Ok(Outcome::Passthrough { why, src: None });
        }
        let gpu = self.gpu.as_ref().unwrap();

        let index = match self.sessions.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                if self.sessions.len() >= MAX_SESSIONS {
                    let oldest = (0..self.sessions.len())
                        .min_by_key(|i| self.sessions[*i].1.last_used)
                        .unwrap();
                    self.sessions.remove(oldest);
                }
                self.sessions.push((key, Session::new(gpu)));
                self.sessions.len() - 1
            }
        };
        let result = pipeline::transform(
            gpu,
            &mut self.sessions[index].1,
            &job.data,
            quality,
            job.target,
        );
        if result.is_err() {
            // Whatever state the failure left behind is not worth trusting.
            self.sessions.remove(index);
        }
        result
    }

    fn record(&mut self, job: &Job, result: &Result<Outcome, String>) {
        let mut stats = self.shared.stats.lock().unwrap();
        match result {
            Ok(Outcome::Done(out)) => {
                let realtime = out.media_seconds / out.work_seconds.max(1e-6);
                stats.error = None;
                stats.skipped = None;
                stats.segments += 1;
                stats.frames += out.frames as u64;
                stats.last_at = Some(Instant::now());
                stats.last = Some(LastJob {
                    src: out.src,
                    out: out.out,
                    mode: out.mode,
                    requested: job.quality,
                    fps: out.frames as f64 / out.work_seconds.max(1e-6),
                    realtime,
                });
                // Slower than playback twice in a row: step the mode down
                // rather than let the video stall.
                if realtime < 1.0 && out.mode > 1 {
                    self.slow_streak += 1;
                    if self.slow_streak >= 2 {
                        self.speed_cap.0 = out.mode - 1;
                        self.slow_streak = 0;
                        stats.slowed_to = Some(out.mode - 1);
                    }
                } else {
                    self.slow_streak = 0;
                }
            }
            Ok(Outcome::Passthrough { why, .. }) => stats.skipped = Some(why.clone()),
            Err(e) => stats.error = Some(e.clone()),
        }
    }
}
