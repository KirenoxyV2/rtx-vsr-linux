//! One MSE fragment through the GPU: demux, NVDEC, VSR, NVENC, remux.
//!
//! Input is what the browser extension sends: a stream's init segment
//! followed by one (or more) `moof`+`mdat`. Output is a new init segment
//! followed by fragments holding the same frames at the same times, upscaled.

use std::collections::HashMap;
use std::time::Instant;

use super::{h264, mp4};
use crate::gpu::nvdec::{Decoder, StreamInfo};
use crate::gpu::nvenc::{Encoder, EncoderConfig};
use crate::gpu::sys::nvcv;
use crate::gpu::vfx::Vsr;
use crate::gpu::Gpu;

/// The model does not go beyond 4x.
const MAX_FACTOR: f64 = 4.0;
/// Below this factor upscaling is not worth the GPU time.
const MIN_FACTOR: f64 = 1.05;

pub const MODE_NAMES: [&str; 5] = ["", "LOW", "MEDIUM", "HIGH", "ULTRA"];

#[derive(Debug)]
pub enum Outcome {
    /// Leave the original bytes alone. `why` is for the status display;
    /// `src` is the video's size, when the input got far enough to tell.
    Passthrough {
        why: String,
        src: Option<(u32, u32)>,
    },
    Done(Output),
}

fn skip(why: impl Into<String>, src: Option<(u32, u32)>) -> Result<Outcome, String> {
    Ok(Outcome::Passthrough {
        why: why.into(),
        src,
    })
}

#[derive(Debug)]
pub struct Output {
    pub bytes: Vec<u8>,
    pub init_len: usize,
    pub src: (u32, u32),
    pub out: (u32, u32),
    /// VSR mode used, which is lower than the requested one when VRAM is short.
    pub mode: u8,
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

struct Effect {
    vsr: Vsr,
    src: (u32, u32),
    out: (u32, u32),
    requested: u8,
    mode: u8,
}

/// Decoder, model and encoder for one stream shape; kept between fragments.
pub struct Session {
    decoder: Decoder,
    effect: Option<Effect>,
    encoder: Option<Encoder>,
    sequence: u32,
    pub last_used: Instant,
}

impl Session {
    pub fn new(gpu: &Gpu) -> Self {
        Session {
            decoder: Decoder::new(gpu.cuvid.clone()),
            effect: None,
            encoder: None,
            sequence: 0,
            last_used: Instant::now(),
        }
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

/// Transforms `data`; `quality` is the requested VSR mode (1..=4) and
/// `target` the box the output must fit in.
pub fn transform(
    gpu: &Gpu,
    session: &mut Session,
    data: &[u8],
    quality: u8,
    target: (u32, u32),
) -> Result<Outcome, String> {
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
    let fills = |src: (u32, u32)| {
        format!(
            "{}x{} already fills {}x{}",
            src.0, src.1, target.0, target.1
        )
    };
    if plan_output((track.width, track.height), target).is_none() {
        return skip(fills((track.width, track.height)), declared);
    }
    let mut fragments = Vec::new();
    for (i, b) in top.iter().enumerate() {
        if &b.kind == b"moof" {
            match top[i + 1..]
                .iter()
                .find(|n| matches!(&n.kind, b"mdat" | b"moof"))
            {
                Some(mdat) if &mdat.kind == b"mdat" => fragments.push((b, mdat)),
                _ => return Err("moof without mdat".into()),
            }
        }
    }
    if fragments.is_empty() {
        return skip("no media fragment", declared);
    }

    let started = Instant::now();
    let mut setup = std::time::Duration::ZERO;
    let mut media = Vec::new();
    let mut frames = 0u32;
    let mut ticks = 0u64;
    let mut shape = None;
    for (moof, mdat) in fragments {
        let fragment = read_fragment(data, moof, mdat, &track)?;
        let mut samples: Vec<mp4::OutSample> = Vec::with_capacity(fragment.units.len());
        let mut times: Vec<i64> = Vec::with_capacity(fragment.units.len());
        let mut payload = Vec::new();
        let mut unit = Vec::new();
        let mut skipped: Option<(String, (u32, u32))> = None;

        let Session {
            decoder,
            effect,
            encoder,
            ..
        } = session;
        let mut on_frame = |info: &StreamInfo,
                            frame: &crate::gpu::nvdec::DecodedFrame|
         -> Result<(), String> {
            if skipped.is_some() {
                return Ok(());
            }
            let src = (info.width, info.height);
            let Some(out) = plan_output(src, target) else {
                skipped = Some((fills(src), src));
                return Ok(());
            };
            let (primaries, transfer, matrix) = match colour(info) {
                Ok(c) => c,
                Err(why) => {
                    skipped = Some((why, src));
                    return Ok(());
                }
            };

            // Model: reused while the sizes and the requested mode stay put.
            if !effect
                .as_ref()
                .is_some_and(|e| e.src == src && e.out == out && e.requested == quality)
            {
                let t = Instant::now();
                *effect = None; // free the old one's memory first
                let free_mb = gpu.cuda.mem_info()?.0 as u64 >> 20;
                let mode = (1..=quality.clamp(1, 4))
                    .rev()
                    .find(|m| vram_estimate_mb(*m, out) <= free_mb)
                    .ok_or_else(|| {
                        format!(
                            "not enough free GPU memory: {free_mb} MB, even LOW needs about {} MB",
                            vram_estimate_mb(1, out)
                        )
                    })?;
                let vsr = Vsr::new(gpu.vfx.clone(), &gpu.cuda, src, out, mode as u32)?;
                *effect = Some(Effect {
                    vsr,
                    src,
                    out,
                    requested: quality,
                    mode,
                });
                setup += t.elapsed();
            }
            let effect = effect.as_mut().unwrap();

            // Encoder: reused while its stream description stays the same,
            // which also keeps the init segment identical between fragments.
            let first = *fragment.durations.get(&frame.timestamp).unwrap_or(&1);
            let fps = (track.timescale, first.max(1));
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
            let reusable = encoder.as_ref().is_some_and(|e| {
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
                let t = Instant::now();
                *encoder = None;
                *encoder = Some(Encoder::new(gpu.nvenc.clone(), gpu.cuda.clone(), wanted)?);
                setup += t.elapsed();
            }
            let encoder = encoder.as_mut().unwrap();

            effect.vsr.run(
                frame.nv12,
                encoder.input(),
                nvcv_colourspace(matrix, info.full_range),
            )?;
            gpu.cuda.synchronize()?;
            unit.clear();
            let keyframe = encoder.encode(frame.timestamp as u64, samples.is_empty(), &mut unit)?;
            let before = payload.len();
            h264::access_unit_to_sample(&unit, &mut payload);
            let duration = *fragment
                .durations
                .get(&frame.timestamp)
                .ok_or("the decoder returned a frame with an unknown timestamp")?;
            samples.push(mp4::OutSample {
                size: (payload.len() - before) as u32,
                duration,
                keyframe,
            });
            times.push(frame.timestamp);
            shape = Some((src, out, effect.mode));
            Ok(())
        };
        decoder.decode(track.timescale, &fragment.units, &mut on_frame)?;

        if let Some((why, src)) = skipped {
            return skip(why, Some(src));
        }
        if samples.len() != fragment.units.len() {
            return Err(format!(
                "decoded {} of {} frames",
                samples.len(),
                fragment.units.len()
            ));
        }
        if !samples[0].keyframe || times.windows(2).any(|w| w[1] <= w[0]) || times[0] < 0 {
            return Err("frames did not come back in presentation order".into());
        }
        session.sequence += 1;
        mp4::write_fragment(
            &mut media,
            session.sequence,
            times[0] as u64,
            &samples,
            &payload,
        );
        frames += samples.len() as u32;
        ticks += samples.iter().map(|s| s.duration as u64).sum::<u64>();
    }

    let (src, out, mode) = shape.ok_or("no frames")?;
    let encoder = session.encoder.as_ref().ok_or("no encoder")?;
    let c = encoder.config();
    let mut bytes = mp4::write_init(&mp4::InitParams {
        width: c.width,
        height: c.height,
        dar: c.dar,
        timescale: track.timescale,
        sps: &encoder.sps,
        pps: &encoder.pps,
        primaries: c.primaries,
        transfer: c.transfer,
        matrix: c.matrix,
        full_range: c.full_range,
    });
    let init_len = bytes.len();
    bytes.extend_from_slice(&media);
    session.last_used = Instant::now();
    Ok(Outcome::Done(Output {
        bytes,
        init_len,
        src,
        out,
        mode,
        frames,
        media_seconds: ticks as f64 / track.timescale as f64,
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
}
