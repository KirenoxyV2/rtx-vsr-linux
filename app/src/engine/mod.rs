//! The GPU worker: one thread that owns the CUDA context and everything
//! built on it, fed through a channel. Requests are served one at a time, so
//! concurrent fragments never share decoder, model or encoder state. What a
//! request produces goes back through a channel of its own, part by part.

pub mod h264;
pub mod mp4;
pub mod pipeline;
pub mod runtime;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gpu::Gpu;
use crate::state::{LastJob, Runtime, Shared};
use pipeline::{Control, Event, Models, Outcome, Session, Speeds};
pub use pipeline::{Position, Start, Summary};

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
    rate: f64,
    reply: mpsc::Sender<Reply>,
    control: Arc<Control>,
}

/// What the caller is told about its fragment, in this order: either
/// `Passthrough`, or `Start`, the parts and `Done`. `Failed` can come at any
/// point and ends it: whatever came before is then not to be relied on.
pub enum Reply {
    /// Leave the original alone. `why` is for the status display; `src` is
    /// the video's size, when the input got far enough to tell.
    Passthrough {
        why: String,
        src: Option<(u32, u32)>,
    },
    Start(Start),
    /// `moof`+`mdat` with the next frames; `last` on the final one.
    Part {
        media: Vec<u8>,
        last: bool,
    },
    Done(Summary),
    Failed(String),
}

/// A fragment that is being worked on.
pub struct Ticket {
    pub replies: mpsc::Receiver<Reply>,
    pub handle: Handle,
}

/// The caller's say in the work on its fragment. Dropping it stops the
/// work: then nobody is left who wants the rest.
pub struct Handle(Arc<Control>);

impl Handle {
    pub fn cancel(&self) {
        self.0.cancel();
    }

    /// The player has moved on.
    pub fn set_position(&self, position: Position) {
        self.0.set_position(position);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.cancel();
    }
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

    /// Queues one fragment (init segment + `moof`/`mdat`) for upscaling;
    /// the ticket's channel tells how it goes. `rate` is the speed the
    /// player plays at (1.0 is normal) and `position` where it is.
    pub fn submit(
        &self,
        data: Vec<u8>,
        quality: u8,
        target: (u32, u32),
        rate: f64,
        position: Position,
    ) -> Ticket {
        let (reply, replies) = mpsc::channel();
        let control = Arc::new(Control::new(position));
        // If the GPU thread is gone the job is dropped here, and the
        // ticket's channel says so by being closed.
        let _ = self.jobs.send(Job {
            data,
            quality,
            target,
            rate,
            reply,
            control: control.clone(),
        });
        Ticket {
            replies,
            handle: Handle(control),
        }
    }
}

struct Worker {
    shared: Arc<Shared>,
    gpu: Option<Gpu>,
    gpu_failed: Option<(Instant, String)>,
    sessions: Vec<(u64, Session)>,
    models: Models,
    speeds: Speeds,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Worker {
            shared,
            gpu: None,
            gpu_failed: None,
            sessions: Vec::new(),
            models: Models::default(),
            speeds: Speeds::default(),
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Job>) {
        loop {
            match rx.recv_timeout(IDLE) {
                Ok(job) => {
                    if job.control.cancelled() {
                        continue; // given up on while it waited
                    }
                    // A bug in here must not take the proxy down with it.
                    let mut started = false;
                    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.handle(&job, &mut started)
                    }));
                    let result = attempt.unwrap_or_else(|_| {
                        self.sessions.clear();
                        self.models.clear();
                        Err("internal error while processing a fragment".into())
                    });
                    self.record(&job, &result);
                    let _ = job.reply.send(match result {
                        Ok(Outcome::Done(summary)) => Reply::Done(summary),
                        // The caller has parts of an upscaled fragment by now.
                        Ok(Outcome::Passthrough { why, .. }) if started => Reply::Failed(why),
                        Ok(Outcome::Passthrough { why, src }) => Reply::Passthrough { why, src },
                        Ok(Outcome::Cancelled) => continue,
                        Err(e) => Reply::Failed(e),
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.sessions.clear();
                    self.models.clear();
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

    /// `started` is set once a part of the output has gone to the caller.
    fn handle(&mut self, job: &Job, started: &mut bool) -> Result<Outcome, String> {
        let Some(key) = pipeline::stream_key(&job.data) else {
            return Ok(Outcome::Passthrough {
                why: "no init segment".into(),
                src: None,
            });
        };
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
        let request = pipeline::Request {
            data: &job.data,
            quality: job.quality,
            target: job.target,
            rate: job.rate,
            control: &job.control,
        };
        let result = pipeline::transform(
            gpu,
            &mut self.sessions[index].1,
            &mut self.models,
            &mut self.speeds,
            &request,
            &mut |event| {
                *started = true;
                job.reply
                    .send(match event {
                        Event::Start(start) => Reply::Start(start),
                        Event::Part { media, last } => Reply::Part { media, last },
                    })
                    .is_ok()
            },
        );
        if result.is_err() {
            // Whatever state the failure left behind is not worth trusting.
            self.sessions.remove(index);
            self.models.clear();
        }
        result
    }

    fn record(&self, job: &Job, result: &Result<Outcome, String>) {
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
                stats.slowed_to = out.slowed.then_some(out.mode);
            }
            Ok(Outcome::Cancelled) => {}
            Ok(Outcome::Passthrough { why, .. }) => stats.skipped = Some(why.clone()),
            Err(e) => stats.error = Some(e.clone()),
        }
    }
}
