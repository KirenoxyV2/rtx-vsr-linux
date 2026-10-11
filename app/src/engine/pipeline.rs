//! One MSE fragment through the GPU: demux, NVDEC, VSR, NVENC, remux.
//!
//! Input is what the browser extension sends: a stream's init segment
//! followed by one (or more) `moof`+`mdat`. Output is a new init segment and
//! the same frames at the same times, upscaled, in `moof`+`mdat` parts that
//! are handed over as they get finished: a player can go on with the first
//! while the rest is still being worked on.
//!
//! The VSR mode is the best one, up to the requested one, that this GPU
//! runs faster than the video plays. That is measured, on the frames
//! themselves: a mode that turns out too slow is left in the middle of a
//! fragment, not after the player has run dry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{h264, mp4};
use crate::gpu::nvdec::{DecodedFrame, Decoder, StreamInfo};
use crate::gpu::nvenc::{Encoder, EncoderConfig};
use crate::gpu::sys::nvcv;
use crate::gpu::vfx::Vsr;
use crate::gpu::Gpu;

/// The model does not go beyond 4x.
const MAX_FACTOR: f64 = 4.0;
/// Below this factor upscaling is not worth the GPU time.
const MIN_FACTOR: f64 = 1.05;

/// A mode is used if frames come out of it this much faster than they are
/// played: what a player's buffer grows on, and what is left for whatever
/// else wants the GPU for a moment.
const HEADROOM: f64 = 1.15;
/// Frames a speed measurement is taken over, and frames left out of it after
/// a model was loaded.
const MEASURED: usize = 10;
const WARM_UP: u32 = 2;
/// A mode that was too slow by no more than this factor may have been held
/// up by something else, and is tried again after `RETRY_AFTER`.
const RETRY_WITHIN: f64 = 1.25;
const RETRY_AFTER: Duration = Duration::from_secs(120);

/// A player with less than `LEAD_SHORT` seconds buffered in front of the
/// playhead is about to run dry, or is waiting already (it has just started
/// or seeked): then the frames are made with the fastest model until it has
/// `LEAD_ENOUGH` in hand.
const LEAD_SHORT: f64 = 1.0;
const LEAD_ENOUGH: f64 = 2.0;

/// Seconds of video in the first part of the output: enough for a player to
/// start on, and to last until the second part is there. Later parts are cut
/// every `PART_EVERY` of work, which is more than that much video each.
const FIRST_PART: f64 = 0.4;
const PART_EVERY: Duration = Duration::from_millis(250);

pub const MODE_NAMES: [&str; 5] = ["", "LOW", "MEDIUM", "HIGH", "ULTRA"];

pub struct Request<'a> {
    pub data: &'a [u8],
    /// The requested VSR mode (1..=4).
    pub quality: u8,
    /// The box the output must fit in.
    pub target: (u32, u32),
    /// How fast the player plays (2.0 is double speed).
    pub rate: f64,
    pub control: &'a Control,
}

/// Where the player is with the video of a request. It moves on while the
/// request is being worked on, so this can be said again at any time.
#[derive(Clone, Copy)]
pub struct Position {
    /// Seconds at the start of the video that nobody is waiting to see: the
    /// playhead is past them, or the player waits for data from elsewhere.
    pub behind: f64,
    /// Seconds buffered in front of the playhead; infinite if the player
    /// is in no hurry (paused, or not saying).
    pub lead: f64,
}

/// What can still be said about a request while it is being worked on.
pub struct Control {
    cancelled: AtomicBool,
    behind: AtomicU64,
    lead: AtomicU64,
}

impl Control {
    pub fn new(position: Position) -> Self {
        let control = Control {
            cancelled: AtomicBool::new(false),
            behind: AtomicU64::new(0),
            lead: AtomicU64::new(0),
        };
        control.set_position(position);
        control
    }

    /// Nobody wants the rest of the output any more.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn set_position(&self, position: Position) {
        self.behind
            .store(position.behind.to_bits(), Ordering::Relaxed);
        self.lead.store(position.lead.to_bits(), Ordering::Relaxed);
    }

    fn position(&self) -> Position {
        Position {
            behind: f64::from_bits(self.behind.load(Ordering::Relaxed)),
            lead: f64::from_bits(self.lead.load(Ordering::Relaxed)),
        }
    }
}

/// What a request produces on its way: `Start`, then the parts.
pub enum Event {
    Start(Start),
    /// `moof`+`mdat` with the next frames; `last` on the final one.
    Part {
        media: Vec<u8>,
        last: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Start {
    /// The output's init segment.
    pub init: Vec<u8>,
    pub src: (u32, u32),
    pub out: (u32, u32),
    /// VSR mode the first frames are made with.
    pub mode: u8,
}

#[derive(Debug)]
pub enum Outcome {
    /// Leave the original bytes alone. `why` is for the status display;
    /// `src` is the video's size, when the input got far enough to tell.
    Passthrough {
        why: String,
        src: Option<(u32, u32)>,
    },
    Done(Summary),
    /// Stopped because of `Control::cancel`; the output is incomplete.
    Cancelled,
}

fn skip(why: impl Into<String>, src: Option<(u32, u32)>) -> Result<Outcome, String> {
    Ok(Outcome::Passthrough {
        why: why.into(),
        src,
    })
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub src: (u32, u32),
    pub out: (u32, u32),
    /// VSR mode the fragment was made with (but for the frames that were
    /// hurried through).
    pub mode: u8,
    /// It is below the requested one because that was too slow (and not
    /// because GPU memory is short).
    pub slowed: bool,
    pub frames: u32,
    /// Seconds of video in the fragment.
    pub media_seconds: f64,
    /// Time spent decoding, upscaling and encoding (not loading the model).
    pub work_seconds: f64,
}

/// Size to upscale `src` to so that it fits inside `target`, or `None` if it
/// is (nearly) there already.
pub fn plan_output(src: (u32, u32), target: (u32, u32)) -> Option<(u32, u32)> {
    if src.0 == 0 || src.1 == 0 {
        return None;
    }
    let factor = (target.0 as f64 / src.0 as f64)
        .min(target.1 as f64 / src.1 as f64)
        .min(MAX_FACTOR);
    if factor < MIN_FACTOR {
        return None;
    }
    let even = |v: f64| ((v / 2.0).round() as u32 * 2).max(2);
    Some((even(src.0 as f64 * factor), even(src.1 as f64 * factor)))
}

/// Rough VRAM need of a VSR mode in MB, from the measured figures in
/// docs/DESIGN.md §9b: per-mode cost at a 4K output plus headroom.
fn vram_estimate_mb(mode: u8, out: (u32, u32)) -> u64 {
    let base = match mode {
        1 => 200.0,
        2 => 400.0,
        _ => 600.0,
    } * 1.3;
    let megapixels = out.0 as f64 * out.1 as f64 / 1e6;
    let extra = (megapixels - 8.29).max(0.0) * 72.0;
    (base + extra + 150.0) as u64
}

/// The colour description to convert with and to write into the output.
/// A stream that does not say gets what Chromium assumes for its size, so the
/// upscaled picture (which is bigger, and would be guessed differently) is
/// shown with the colours the original would have had.
fn colour(info: &StreamInfo) -> Result<(u8, u8, u8), String> {
    let specified = |v: u8| v != 0 && v != 2;
    let (p, t, m) =
        if specified(info.primaries) && specified(info.transfer) && specified(info.matrix) {
            (info.primaries, info.transfer, info.matrix)
        } else if info.height < 720 {
            (6, 6, 6) // BT.601
        } else {
            (1, 1, 1) // BT.709
        };
    if matches!(t, 16 | 18) {
        return Err("HDR video".into());
    }
    Ok((p, t, m))
}

fn nvcv_colourspace(matrix: u8, full_range: bool) -> u32 {
    let matrix = match matrix {
        5 | 6 => nvcv::NVCV_601,
        9 | 10 => nvcv::NVCV_2020,
        _ => nvcv::NVCV_709,
    };
    let range = if full_range {
        nvcv::NVCV_FULL_RANGE
    } else {
        nvcv::NVCV_VIDEO_RANGE
    };
    matrix | range | nvcv::NVCV_CHROMA_COSITED
}

/// Input and output size of a stream.
type Shape = ((u32, u32), (u32, u32));

struct Model {
    vsr: Vsr,
    shape: Shape,
    mode: u8,
}

/// The model that is loaded. There is one at a time, whatever the stream:
/// models that are loaded together do not give their memory back when they
/// go (the SDK's doing; one at a time they do, all of it), and swapping
/// takes a tenth of a second. It is kept between fragments and given up
/// when there is no work (`clear`).
#[derive(Default)]
pub struct Models {
    loaded: Option<Model>,
}

impl Models {
    /// The model for `shape` and `mode`, loaded now unless it is the one
    /// that is there.
    fn get(&mut self, gpu: &Gpu, shape: Shape, mode: u8) -> Result<&mut Vsr, String> {
        if !self
            .loaded
            .as_ref()
            .is_some_and(|m| m.shape == shape && m.mode == mode)
        {
            self.loaded = None; // its memory first
            let (free_mb, need) = (
                gpu.cuda.mem_info()?.0 as u64 >> 20,
                vram_estimate_mb(mode, shape.1),
            );
            if need > free_mb {
                return Err(format!(
                    "not enough free GPU memory: {free_mb} MB, {} needs about {need} MB",
                    MODE_NAMES[mode as usize]
                ));
            }
            let (src, out) = shape;
            let vsr = Vsr::new(gpu.vfx.clone(), &gpu.cuda, src, out, mode as u32)?;
            self.loaded = Some(Model { vsr, shape, mode });
        }
        Ok(&mut self.loaded.as_mut().unwrap().vsr)
    }

    /// Releases the GPU memory.
    pub fn clear(&mut self) {
        self.loaded = None;
    }
}

/// Decoder and encoder of one stream; kept between fragments.
pub struct Session {
    decoder: Decoder,
    encoder: Option<Encoder>,
    sequence: u32,
    pub last_used: Instant,
}

impl Session {
    pub fn new(gpu: &Gpu) -> Self {
        Session {
            decoder: Decoder::new(gpu.cuvid.clone()),
            encoder: None,
            sequence: 0,
            last_used: Instant::now(),
        }
    }
}

/// How long a frame took in each mode on streams of each shape, and when
/// that was measured: what the choice of mode goes by. Kept for as long as
/// the app runs.
#[derive(Default)]
pub struct Speeds {
    seen: HashMap<(Shape, u8), (f64, Instant)>,
}

impl Speeds {
    fn note(&mut self, shape: Shape, mode: u8, ms: f64) {
        self.seen.insert((shape, mode), (ms, Instant::now()));
    }

    /// The best mode up to `quality` to make frames in `budget` ms with.
    fn choose(&self, shape: Shape, quality: u8, budget: f64) -> u8 {
        (1..=quality)
            .rev()
            .find(|mode| {
                let seen = self.seen.get(&(shape, *mode));
                worth_trying(seen.map(|(ms, at)| (*ms, at.elapsed())), budget)
            })
            .unwrap_or(1)
    }
}

/// Whether to use a mode that took `seen.0` ms per frame when it was last
/// measured, `seen.1` ago: yes if it has never been measured (then it will
/// be now), if that was fast enough, or if it missed narrowly long enough ago
/// to be given another chance.
fn worth_trying(seen: Option<(f64, Duration)>, budget: f64) -> bool {
    match seen {
        None => true,
        Some((ms, age)) => ms <= budget || (ms <= budget * RETRY_WITHIN && age >= RETRY_AFTER),
    }
}

/// Measures how long frames take while they are being made.
struct Meter {
    skip: u32,
    last: Option<Instant>,
    gaps: Vec<f64>,
}

impl Meter {
    fn new() -> Self {
        Meter {
            skip: WARM_UP,
            last: None,
            gaps: Vec::with_capacity(MEASURED),
        }
    }

    /// The frames that come next are not to be measured (they are made
    /// with another model): what was measured so far is kept.
    fn interrupt(&mut self) {
        self.last = None;
        self.skip = self.skip.max(1);
    }

    /// To be called when a frame is done. Every `MEASURED` frames it
    /// returns what one of them took, in ms.
    fn frame(&mut self) -> Option<f64> {
        let now = Instant::now();
        let since = self.last.replace(now);
        if self.skip > 0 {
            self.skip -= 1;
            return None;
        }
        self.gaps.push((now - since?).as_secs_f64() * 1000.0);
        if self.gaps.len() < MEASURED {
            return None;
        }
        // Without the longest: one hiccup (somebody else's burst of work on
        // the GPU) is not the mode's speed.
        let longest = self.gaps.iter().copied().fold(0.0, f64::max);
        let ms = (self.gaps.iter().sum::<f64>() - longest) / (self.gaps.len() - 1) as f64;
        self.gaps.clear();
        Some(ms)
    }
}

/// Identifies the stream a fragment belongs to, so that two videos playing
/// at once each keep their own session.
pub fn stream_key(data: &[u8]) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let moov = mp4::boxes(data, 0, data.len())
        .into_iter()
        .find(|b| &b.kind == b"moov")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data[moov.start..moov.end].hash(&mut hasher);
    Some(hasher.finish())
}

struct Fragment {
    units: Vec<(Vec<u8>, i64)>,
    /// Presentation time -> duration until the next frame.
    durations: HashMap<i64, u32>,
}

fn read_fragment(
    data: &[u8],
    moof: &mp4::BoxRef,
    mdat: &mp4::BoxRef,
    track: &mp4::Track,
) -> Result<Fragment, String> {
    let samples = mp4::parse_fragment(data, moof, mdat, track)?;
    if samples.is_empty() {
        return Err("fragment has no video samples".into());
    }
    let mut units = Vec::with_capacity(samples.len());
    for (i, s) in samples.iter().enumerate() {
        let mut unit = Vec::with_capacity(s.size + 64);
        if i == 0 {
            // A fragment is decoded on its own, so it starts with the
            // parameter sets (`avc1` keeps them only in the init segment).
            for set in track.sps.iter().chain(&track.pps) {
                h264::push_annex_b(&mut unit, set);
            }
        }
        h264::sample_to_annex_b(
            &data[s.offset..s.offset + s.size],
            track.nal_length_size,
            &mut unit,
        )?;
        units.push((unit, s.pts));
    }
    let mut by_time: Vec<&mp4::Sample> = samples.iter().collect();
    by_time.sort_by_key(|s| s.pts);
    let mut durations = HashMap::with_capacity(samples.len());
    for pair in by_time.windows(2) {
        let gap = pair[1].pts - pair[0].pts;
        if gap <= 0 || gap > u32::MAX as i64 {
            return Err("two samples share a presentation time".into());
        }
        durations.insert(pair[0].pts, gap as u32);
    }
    let last = by_time[by_time.len() - 1];
    durations.insert(last.pts, last.duration.max(1));
    Ok(Fragment { units, durations })
}

/// What turns the decoded pictures of one request into parts of the output.
struct Work<'a> {
    gpu: &'a Gpu,
    request: &'a Request<'a>,
    track: &'a mp4::Track,
    models: &'a mut Models,
    encoder: &'a mut Option<Encoder>,
    sequence: &'a mut u32,
    speeds: &'a mut Speeds,
    emit: &'a mut dyn FnMut(Event) -> bool,
    /// Frames in the request, and the ms each may take.
    total: usize,
    budget: f64,

    /// Of the fragment being decoded: how long each frame is shown, by its
    /// presentation time, and whether its first frame is still to come.
    durations: &'a HashMap<i64, u32>,
    fragment_start: bool,
    shape: Option<Shape>,
    /// The mode to use, as far as speed goes, and whether GPU memory was
    /// in the way of a better one.
    mode: u8,
    memory_short: bool,
    /// Measures `metered`, which is `mode` from the next frame on.
    meter: Meter,
    metered: u8,
    /// The last measurement was a little too slow.
    missed: bool,
    /// Time of the first frame, and whether the player is short of
    /// buffered video.
    first_time: i64,
    short: bool,
    setup: Duration,

    /// The picture the encoder is working on: its time, its duration and
    /// whether it has to come out as a keyframe.
    pending: Option<(i64, u32, bool)>,
    unit: Vec<u8>,
    /// Frames out of the encoder that are not in a part yet.
    samples: Vec<mp4::OutSample>,
    payload: Vec<u8>,
    part_time: i64,
    part_ticks: u64,
    parts: u32,
    last_part: Instant,
    last_time: Option<i64>,
    /// Frames out of the encoder, in this fragment and in all.
    in_fragment: usize,
    done: usize,
    started: bool,

    /// Why the pictures are not wanted after all, if they are not.
    skipped: Option<(String, (u32, u32))>,
    cancelled: bool,
}

fn fills(src: (u32, u32), target: (u32, u32)) -> String {
    format!(
        "{}x{} already fills {}x{}",
        src.0, src.1, target.0, target.1
    )
}

impl Work<'_> {
    /// One decoded picture: upscale it and start encoding it. An error
    /// stops the decoder; `skipped` and `cancelled` say if it is none.
    fn frame(&mut self, info: &StreamInfo, frame: &DecodedFrame) -> Result<(), String> {
        if self.request.control.cancelled() {
            self.cancelled = true;
            return Err("cancelled".into());
        }
        let src = (info.width, info.height);
        let Some(out) = plan_output(src, self.request.target) else {
            self.skipped = Some((fills(src, self.request.target), src));
            return Err("not upscaled".into());
        };
        let (primaries, transfer, matrix) = match colour(info) {
            Ok(c) => c,
            Err(why) => {
                self.skipped = Some((why, src));
                return Err("not upscaled".into());
            }
        };
        let shape = (src, out);
        if self.shape != Some(shape) {
            self.shape = Some(shape);
            let quality = self.request.quality.clamp(1, 4);
            self.mode = self.speeds.choose(shape, quality, self.budget);
        }
        // Frames behind the playhead are only there to be seeked back to;
        // the player waits for the ones after them. And a player that is
        // about to run dry needs frames now more than it needs them good.
        // Then hurry: the fastest mode.
        let at = self.request.control.position();
        let behind = (at.behind * self.track.timescale as f64) as i64;
        if at.lead < LEAD_SHORT {
            self.short = true;
        } else if at.lead >= LEAD_ENOUGH {
            self.short = false;
        }
        let hurry = self.short || frame.timestamp < self.first_time + behind;

        // The model for it, or the best one below that GPU memory allows.
        let t = Instant::now();
        let mut mode = if hurry { 1 } else { self.mode };
        while let Err(e) = self.models.get(self.gpu, shape, mode) {
            if mode == 1 {
                return Err(e);
            }
            mode -= 1;
            self.mode = mode;
            self.memory_short = true;
        }
        self.setup += t.elapsed();
        if self.mode != self.metered {
            self.metered = self.mode;
            self.meter = Meter::new();
        }

        // Encoder: reused while its stream description stays the same,
        // which also keeps the init segment identical between fragments.
        let duration = *self
            .durations
            .get(&frame.timestamp)
            .ok_or("the decoder returned a frame with an unknown timestamp")?;
        let fps = (self.track.timescale, duration.max(1));
        let rate = fps.0 as f64 / fps.1 as f64;
        let wanted = EncoderConfig {
            width: out.0,
            height: out.1,
            fps,
            dar: info.dar,
            gop: (rate.round() as u32).clamp(12, 120),
            max_bitrate: (out.0 as f64 * out.1 as f64 * rate.clamp(10.0, 60.0) * 0.09) as u32,
            full_range: info.full_range,
            primaries,
            transfer,
            matrix,
        };
        let reusable = self.encoder.as_ref().is_some_and(|e| {
            let c = e.config();
            // The frame rate only steers rate control; do not rebuild for it.
            EncoderConfig {
                fps: c.fps,
                gop: c.gop,
                max_bitrate: c.max_bitrate,
                ..wanted
            } == *c
        });
        if !reusable {
            if self.started {
                // The parts handed over so far go with the old description.
                return Err("the stream changes inside a fragment".into());
            }
            let t = Instant::now();
            *self.encoder = None;
            *self.encoder = Some(Encoder::new(
                self.gpu.nvenc.clone(),
                self.gpu.cuda.clone(),
                wanted,
            )?);
            self.setup += t.elapsed();
        }
        let encoder = self.encoder.as_mut().unwrap();

        if self.last_time.is_some_and(|t| frame.timestamp <= t) || frame.timestamp < 0 {
            return Err("frames did not come back in presentation order".into());
        }
        self.last_time = Some(frame.timestamp);
        if !self.started {
            self.started = true;
            let c = encoder.config();
            let init = mp4::write_init(&mp4::InitParams {
                width: c.width,
                height: c.height,
                dar: c.dar,
                timescale: self.track.timescale,
                sps: &encoder.sps,
                pps: &encoder.pps,
                primaries: c.primaries,
                transfer: c.transfer,
                matrix: c.matrix,
                full_range: c.full_range,
            });
            if !(self.emit)(Event::Start(Start {
                init,
                src,
                out,
                mode: self.mode,
            })) {
                self.cancelled = true;
                return Err("cancelled".into());
            }
        }

        // The model's work is only queued here. While the GPU does it, wait
        // for the encoder, which is hardware of its own, to finish the
        // previous picture; then for the GPU, and hand this one over.
        self.models.get(self.gpu, shape, mode)?.run(
            frame.nv12,
            encoder.input(),
            nvcv_colourspace(matrix, info.full_range),
        )?;
        let taken = self.take();
        self.gpu.cuda.synchronize()?;
        taken?;
        let key = std::mem::take(&mut self.fragment_start);
        self.encoder
            .as_mut()
            .unwrap()
            .submit(frame.timestamp as u64, key)?;
        self.pending = Some((frame.timestamp, duration, key));

        // What a frame takes is measured in the mode that was chosen, not
        // while hurrying.
        if mode != self.mode {
            self.meter.interrupt();
            return Ok(());
        }
        if let Some(ms) = self.meter.frame() {
            // Too slow to stay ahead of playback: the next frame is made
            // one mode down. A narrow miss has to happen twice first, it
            // may be something else that held the GPU up.
            let slow = ms > self.budget && mode > 1;
            let sure = ms > self.budget * RETRY_WITHIN || self.missed;
            self.missed = slow && !sure;
            if !self.missed {
                self.speeds.note(shape, mode, ms);
            }
            if slow && sure {
                self.mode = mode - 1;
            }
        }
        Ok(())
    }

    /// Waits for the picture the encoder is working on, if there is one, and
    /// hands over a part of the output if one is due.
    fn take(&mut self) -> Result<(), String> {
        let Some((time, duration, key)) = self.pending.take() else {
            return Ok(());
        };
        let encoder = self.encoder.as_mut().ok_or("no encoder")?;
        self.unit.clear();
        let keyframe = encoder
            .collect(&mut self.unit)?
            .ok_or("the encoder lost a frame")?;
        if key && !keyframe {
            return Err("a fragment did not start on a keyframe".into());
        }
        if self.samples.is_empty() {
            self.part_time = time;
        }
        let before = self.payload.len();
        h264::access_unit_to_sample(&self.unit, &mut self.payload);
        self.samples.push(mp4::OutSample {
            size: (self.payload.len() - before) as u32,
            duration,
            keyframe,
        });
        self.part_ticks += duration as u64;
        self.in_fragment += 1;
        self.done += 1;

        let last = self.done == self.total;
        let due = if self.parts == 0 {
            self.part_ticks as f64 >= FIRST_PART * self.track.timescale as f64
        } else {
            self.last_part.elapsed() >= PART_EVERY
        };
        if !last && !due {
            return Ok(());
        }
        *self.sequence += 1;
        let mut media = Vec::with_capacity(self.payload.len() + 12 * self.samples.len() + 128);
        mp4::write_fragment(
            &mut media,
            *self.sequence,
            self.part_time as u64,
            &self.samples,
            &self.payload,
        );
        self.samples.clear();
        self.payload.clear();
        self.part_ticks = 0;
        self.parts += 1;
        self.last_part = Instant::now();
        if !(self.emit)(Event::Part { media, last }) {
            self.cancelled = true;
            return Err("cancelled".into());
        }
        Ok(())
    }
}

/// Upscales what `request` holds, handing the output to `emit` piece by
/// piece; `emit` returns false if nobody is there to take it any more.
pub fn transform(
    gpu: &Gpu,
    session: &mut Session,
    models: &mut Models,
    speeds: &mut Speeds,
    request: &Request,
    emit: &mut dyn FnMut(Event) -> bool,
) -> Result<Outcome, String> {
    let (data, target) = (request.data, request.target);
    let top = mp4::boxes(data, 0, data.len());
    let Some(moov) = top.iter().find(|b| &b.kind == b"moov") else {
        return skip("no init segment", None);
    };
    let Some(track) = mp4::parse_init(data, moov)? else {
        return skip("no video track", None);
    };
    let declared = Some((track.width, track.height)).filter(|s| s.0 > 0 && s.1 > 0);
    if !track.is_h264() {
        let codec = String::from_utf8_lossy(&track.codec).into_owned();
        return skip(format!("not H.264 ({codec})"), declared);
    }
    if plan_output((track.width, track.height), target).is_none() {
        return skip(fills((track.width, track.height), target), declared);
    }
    let mut fragments = Vec::new();
    for (i, b) in top.iter().enumerate() {
        if &b.kind == b"moof" {
            match top[i + 1..]
                .iter()
                .find(|n| matches!(&n.kind, b"mdat" | b"moof"))
            {
                Some(mdat) if &mdat.kind == b"mdat" => {
                    fragments.push(read_fragment(data, b, mdat, &track)?)
                }
                _ => return Err("moof without mdat".into()),
            }
        }
    }
    if fragments.is_empty() {
        return skip("no media fragment", declared);
    }
    // How many frames there are and how fast they are played is what the
    // work is planned on.
    let total: usize = fragments.iter().map(|f| f.units.len()).sum();
    let ticks: u64 = fragments
        .iter()
        .flat_map(|f| f.durations.values())
        .map(|d| *d as u64)
        .sum();
    let media_seconds = ticks as f64 / track.timescale as f64;
    let budget = if media_seconds > 0.0 {
        media_seconds * 1000.0 / total as f64 / HEADROOM / request.rate.clamp(1.0, 16.0)
    } else {
        f64::INFINITY
    };

    let first_time = fragments
        .iter()
        .flat_map(|f| &f.units)
        .map(|u| u.1)
        .min()
        .unwrap_or(0);

    let started = Instant::now();
    let Session {
        decoder,
        encoder,
        sequence,
        ..
    } = session;
    let mut work = Work {
        gpu,
        request,
        track: &track,
        models,
        encoder,
        sequence,
        speeds,
        emit,
        total,
        budget,
        durations: &fragments[0].durations,
        fragment_start: true,
        shape: None,
        mode: request.quality.clamp(1, 4),
        memory_short: false,
        meter: Meter::new(),
        metered: 0,
        missed: false,
        first_time,
        short: false,
        setup: Duration::ZERO,
        pending: None,
        unit: Vec::new(),
        samples: Vec::new(),
        payload: Vec::new(),
        part_time: 0,
        part_ticks: 0,
        parts: 0,
        last_part: started,
        last_time: None,
        in_fragment: 0,
        done: 0,
        started: false,
        skipped: None,
        cancelled: false,
    };
    for fragment in &fragments {
        work.durations = &fragment.durations;
        work.fragment_start = true;
        work.in_fragment = 0;
        let decoded = decoder.decode(track.timescale, &fragment.units, &mut |info, frame| {
            work.frame(info, frame)
        });
        // Whatever happened, the encoder is not left with a picture in it.
        let taken = work.take();
        if work.cancelled {
            return Ok(Outcome::Cancelled);
        }
        if let Some((why, src)) = work.skipped.take() {
            return skip(why, Some(src));
        }
        decoded?;
        taken?;
        if work.in_fragment != fragment.units.len() {
            return Err(format!(
                "decoded {} of {} frames",
                work.in_fragment,
                fragment.units.len()
            ));
        }
    }

    let (shape, mode) = (work.shape.ok_or("no frames")?, work.mode);
    let (setup, memory_short) = (work.setup, work.memory_short);
    session.last_used = Instant::now();
    Ok(Outcome::Done(Summary {
        src: shape.0,
        out: shape.1,
        mode,
        slowed: mode < request.quality.clamp(1, 4) && !memory_short,
        frames: total as u32,
        media_seconds,
        work_seconds: started.elapsed().saturating_sub(setup).as_secs_f64(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_fits_the_target_box() {
        assert_eq!(plan_output((1280, 720), (2560, 1440)), Some((2560, 1440)));
        assert_eq!(plan_output((1280, 720), (1920, 1200)), Some((1920, 1080)));
        assert_eq!(plan_output((640, 360), (1920, 1080)), Some((1920, 1080)));
        assert_eq!(plan_output((640, 480), (1920, 1080)), Some((1440, 1080)));
        assert_eq!(plan_output((1920, 1080), (1920, 1200)), None);
        assert_eq!(plan_output((1920, 1080), (1920, 1080)), None);
        assert_eq!(plan_output((2560, 1440), (1920, 1080)), None);
        // the model stops at 4x
        assert_eq!(plan_output((256, 144), (3840, 2160)), Some((1024, 576)));
        assert_eq!(plan_output((0, 0), (1920, 1080)), None);
    }

    #[test]
    fn unspecified_colour_follows_the_source_size() {
        let mut info = StreamInfo {
            width: 640,
            height: 360,
            full_range: false,
            primaries: 2,
            transfer: 2,
            matrix: 2,
            dar: (16, 9),
        };
        assert_eq!(colour(&info), Ok((6, 6, 6)));
        info.height = 720;
        assert_eq!(colour(&info), Ok((1, 1, 1)));
        (info.primaries, info.transfer, info.matrix) = (9, 16, 9);
        assert!(colour(&info).is_err());
    }

    #[test]
    fn the_mode_is_the_best_one_that_keeps_up() {
        let shape = ((1920, 1080), (2560, 1440));
        let mut speeds = Speeds::default();
        // Nothing is known: the requested mode is what gets measured first.
        assert_eq!(speeds.choose(shape, 4, 14.5), 4);
        speeds.note(shape, 4, 26.2);
        assert_eq!(speeds.choose(shape, 4, 14.5), 3);
        speeds.note(shape, 3, 16.2);
        speeds.note(shape, 2, 7.7);
        // 60 fps: MEDIUM. 30 fps, the same stream otherwise: ULTRA.
        assert_eq!(speeds.choose(shape, 4, 14.5), 2);
        assert_eq!(speeds.choose(shape, 4, 29.0), 4);
        assert_eq!(speeds.choose(shape, 3, 29.0), 3);
        // Another shape has its own measurements.
        assert_eq!(speeds.choose(((1280, 720), (2560, 1440)), 4, 14.5), 4);
        // Nothing keeps up: LOW is still better than not upscaling.
        speeds.note(shape, 1, 5.1);
        assert_eq!(speeds.choose(shape, 4, 2.0), 1);
    }

    #[test]
    fn a_narrow_miss_gets_another_chance_later() {
        let (recent, old) = (Duration::from_secs(5), RETRY_AFTER);
        assert!(worth_trying(None, 14.5));
        assert!(worth_trying(Some((14.0, recent)), 14.5));
        assert!(!worth_trying(Some((16.2, recent)), 14.5));
        assert!(worth_trying(Some((16.2, old)), 14.5));
        // Twice too slow is not somebody else's doing.
        assert!(!worth_trying(Some((26.2, old)), 14.5));
    }

    #[test]
    fn frames_made_in_a_hurry_are_not_measured() {
        let mut meter = Meter::new();
        let mut results = Vec::new();
        for i in 0..WARM_UP as usize + 3 * (MEASURED + 2) {
            std::thread::sleep(Duration::from_millis(2));
            if i % 6 == 5 {
                // a long stretch with the other model, every few frames
                std::thread::sleep(Duration::from_millis(30));
                meter.interrupt();
            } else {
                results.extend(meter.frame());
            }
        }
        assert!(!results.is_empty());
        assert!(
            results.iter().all(|ms| (2.0..4.0).contains(ms)),
            "{results:?}"
        );
    }

    #[test]
    fn one_hiccup_does_not_count() {
        let mut meter = Meter::new();
        let mut results = Vec::new();
        for i in 0..WARM_UP as usize + 1 + 2 * MEASURED {
            std::thread::sleep(Duration::from_millis(if i == 7 { 40 } else { 2 }));
            results.extend(meter.frame());
        }
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|ms| (2.0..4.0).contains(ms)),
            "{results:?}"
        );
    }
}
