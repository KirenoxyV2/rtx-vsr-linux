//! Just enough ISO-BMFF for MSE segments: reading an init segment and the
//! `moof`/`mdat` fragments that follow it, and writing new ones.
//!
//! Timestamps follow what Chromium's MSE parser does with the same bytes: a
//! sample is presented at `decode time + composition offset - media_time of
//! the first edit`, so a fragment written with those times lands exactly
//! where the original would have.

#[derive(Clone, Copy, Debug)]
pub struct BoxRef {
    pub kind: [u8; 4],
    pub start: usize,
    /// First byte after the box header.
    pub body: usize,
    pub end: usize,
}

/// The complete boxes found in `data[start..end]`; stops at the first one
/// that is malformed or cut short.
pub fn boxes(data: &[u8], start: usize, end: usize) -> Vec<BoxRef> {
    let mut found = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        let mut size = be32(data, pos) as u64;
        let mut header = 8;
        if size == 1 {
            if pos + 16 > end {
                break;
            }
            size = be64(data, pos + 8);
            header = 16;
        } else if size == 0 {
            size = (end - pos) as u64;
        }
        if size < header as u64 || size > (end - pos) as u64 {
            break;
        }
        let kind = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
        found.push(BoxRef {
            kind,
            start: pos,
            body: pos + header,
            end: pos + size as usize,
        });
        pos += size as usize;
    }
    found
}

fn children(data: &[u8], parent: &BoxRef) -> Vec<BoxRef> {
    boxes(data, parent.body, parent.end)
}

fn child(data: &[u8], parent: &BoxRef, kind: &[u8; 4]) -> Option<BoxRef> {
    children(data, parent).into_iter().find(|b| &b.kind == kind)
}

fn be16(d: &[u8], p: usize) -> u16 {
    u16::from_be_bytes([d[p], d[p + 1]])
}
fn be32(d: &[u8], p: usize) -> u32 {
    u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}
fn be64(d: &[u8], p: usize) -> u64 {
    ((be32(d, p) as u64) << 32) | be32(d, p + 4) as u64
}

/// Bounds-checked reader over one box body.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], b: &BoxRef) -> Self {
        Reader {
            data,
            pos: b.body,
            end: b.end,
        }
    }
    fn need(&self, n: usize) -> Result<(), String> {
        if self.end - self.pos < n {
            Err("truncated box".into())
        } else {
            Ok(())
        }
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.need(n)?;
        self.pos += n;
        Ok(())
    }
    fn u8(&mut self) -> Result<u8, String> {
        self.need(1)?;
        self.pos += 1;
        Ok(self.data[self.pos - 1])
    }
    fn u16(&mut self) -> Result<u16, String> {
        self.need(2)?;
        self.pos += 2;
        Ok(be16(self.data, self.pos - 2))
    }
    fn u32(&mut self) -> Result<u32, String> {
        self.need(4)?;
        self.pos += 4;
        Ok(be32(self.data, self.pos - 4))
    }
    fn u64(&mut self) -> Result<u64, String> {
        self.need(8)?;
        self.pos += 8;
        Ok(be64(self.data, self.pos - 8))
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        self.need(n)?;
        self.pos += n;
        Ok(&self.data[self.pos - n..self.pos])
    }
}

/// The video track of an init segment.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub id: u32,
    pub timescale: u32,
    /// Sample entry type: `avc1`, `avc3`, `encv`, `vp09`, …
    pub codec: [u8; 4],
    pub width: u32,
    pub height: u32,
    pub nal_length_size: usize,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    /// Subtracted from every presentation time (the first edit's media time).
    pub edit_offset: i64,
    default_duration: u32,
    default_size: u32,
}

impl Track {
    pub fn is_h264(&self) -> bool {
        matches!(&self.codec, b"avc1" | b"avc3") && !self.sps.is_empty() && !self.pps.is_empty()
    }
}

/// Reads the first video track of `moov`; `None` if there is none (audio).
pub fn parse_init(data: &[u8], moov: &BoxRef) -> Result<Option<Track>, String> {
    let mut track = None;
    for trak in children(data, moov).iter().filter(|b| &b.kind == b"trak") {
        let Some(mdia) = child(data, trak, b"mdia") else {
            continue;
        };
        let Some(hdlr) = child(data, &mdia, b"hdlr") else {
            continue;
        };
        let mut r = Reader::new(data, &hdlr);
        r.skip(8)?;
        if r.bytes(4)? != b"vide" {
            continue;
        }
        let mut t = Track::default();

        let tkhd = child(data, trak, b"tkhd").ok_or("no tkhd")?;
        let mut r = Reader::new(data, &tkhd);
        let version = r.u8()?;
        r.skip(3 + if version == 1 { 16 } else { 8 })?;
        t.id = r.u32()?;

        let mdhd = child(data, &mdia, b"mdhd").ok_or("no mdhd")?;
        let mut r = Reader::new(data, &mdhd);
        let version = r.u8()?;
        r.skip(3 + if version == 1 { 16 } else { 8 })?;
        t.timescale = r.u32()?;
        if t.timescale == 0 {
            return Err("track timescale is 0".into());
        }

        let stsd = child(data, &mdia, b"minf")
            .and_then(|minf| child(data, &minf, b"stbl"))
            .and_then(|stbl| child(data, &stbl, b"stsd"))
            .ok_or("no stsd")?;
        let entry = boxes(data, stsd.body + 8, stsd.end)
            .into_iter()
            .next()
            .ok_or("empty stsd")?;
        t.codec = entry.kind;
        // VisualSampleEntry: 78 bytes of fixed fields, then child boxes.
        if entry.end - entry.body >= 78 {
            t.width = be16(data, entry.body + 24) as u32;
            t.height = be16(data, entry.body + 26) as u32;
            let inner = BoxRef {
                body: entry.body + 78,
                ..entry
            };
            if let Some(avcc) = child(data, &inner, b"avcC") {
                let mut r = Reader::new(data, &avcc);
                r.skip(4)?;
                t.nal_length_size = (r.u8()? & 3) as usize + 1;
                for _ in 0..r.u8()? & 0x1f {
                    let len = r.u16()? as usize;
                    t.sps.push(r.bytes(len)?.to_vec());
                }
                for _ in 0..r.u8()? {
                    let len = r.u16()? as usize;
                    t.pps.push(r.bytes(len)?.to_vec());
                }
            }
        }

        if let Some(elst) = child(data, trak, b"edts").and_then(|e| child(data, &e, b"elst")) {
            let mut r = Reader::new(data, &elst);
            let version = r.u8()?;
            r.skip(3)?;
            if r.u32()? > 0 {
                let media_time = if version == 1 {
                    r.skip(8)?;
                    r.u64()? as i64
                } else {
                    r.skip(4)?;
                    r.u32()? as i32 as i64
                };
                t.edit_offset = media_time.max(0);
            }
        }
        track = Some(t);
        break;
    }
    let Some(mut t) = track else { return Ok(None) };

    if let Some(mvex) = child(data, moov, b"mvex") {
        for trex in children(data, &mvex).iter().filter(|b| &b.kind == b"trex") {
            let mut r = Reader::new(data, trex);
            r.skip(4)?;
            if r.u32()? == t.id {
                r.skip(4)?;
                t.default_duration = r.u32()?;
                t.default_size = r.u32()?;
            }
        }
    }
    Ok(Some(t))
}

/// One access unit of a fragment, in decode order.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub offset: usize,
    pub size: usize,
    /// Presentation time in the track's timescale, edit list applied.
    pub pts: i64,
    pub duration: u32,
}

/// The samples `moof` describes for `track`. `mdat` is the box holding their
/// bytes; offsets are checked against `data`.
pub fn parse_fragment(
    data: &[u8],
    moof: &BoxRef,
    mdat: &BoxRef,
    track: &Track,
) -> Result<Vec<Sample>, String> {
    let mut samples = Vec::new();
    for traf in children(data, moof).iter().filter(|b| &b.kind == b"traf") {
        let tfhd = child(data, traf, b"tfhd").ok_or("no tfhd")?;
        let mut r = Reader::new(data, &tfhd);
        let flags = r.u32()? & 0xff_ffff;
        if r.u32()? != track.id {
            continue;
        }
        // An explicit base offset counts from the start of the original
        // stream, which is not what we were given; the data is in `mdat`.
        let explicit_base = flags & 0x1 != 0;
        if explicit_base {
            r.skip(8)?;
        }
        if flags & 0x2 != 0 {
            r.skip(4)?;
        }
        let default_duration = if flags & 0x8 != 0 {
            r.u32()?
        } else {
            track.default_duration
        };
        let default_size = if flags & 0x10 != 0 {
            r.u32()?
        } else {
            track.default_size
        };

        let mut dts = match child(data, traf, b"tfdt") {
            Some(tfdt) => {
                let mut r = Reader::new(data, &tfdt);
                let version = r.u8()?;
                r.skip(3)?;
                if version == 1 {
                    r.u64()? as i64
                } else {
                    r.u32()? as i64
                }
            }
            None => return Err("no tfdt".into()),
        };

        let mut pos = mdat.body;
        for trun in children(data, traf).iter().filter(|b| &b.kind == b"trun") {
            let mut r = Reader::new(data, trun);
            let version = r.u8()?;
            r.skip(1)?;
            let flags = r.u16()? as u32;
            let count = r.u32()? as usize;
            if flags & 0x1 != 0 {
                let offset = r.u32()? as i32 as i64;
                if !explicit_base {
                    pos = usize::try_from(moof.start as i64 + offset)
                        .map_err(|_| "negative data offset")?;
                }
            }
            if flags & 0x4 != 0 {
                r.skip(4)?;
            }
            if count > (trun.end - r.pos) {
                return Err("implausible sample count".into());
            }
            for _ in 0..count {
                let duration = if flags & 0x100 != 0 {
                    r.u32()?
                } else {
                    default_duration
                };
                let size = if flags & 0x200 != 0 {
                    r.u32()?
                } else {
                    default_size
                } as usize;
                if flags & 0x400 != 0 {
                    r.skip(4)?;
                }
                let cto = if flags & 0x800 != 0 {
                    let raw = r.u32()?;
                    if version == 0 {
                        raw as i64
                    } else {
                        raw as i32 as i64
                    }
                } else {
                    0
                };
                if size == 0 || pos < mdat.body || size > mdat.end.saturating_sub(pos) {
                    return Err("sample lies outside its mdat".into());
                }
                samples.push(Sample {
                    offset: pos,
                    size,
                    pts: dts + cto - track.edit_offset,
                    duration,
                });
                pos += size;
                dts += duration as i64;
            }
        }
    }
    Ok(samples)
}

// ---- writing ----------------------------------------------------------------

fn write_box(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(kind);
    body(out);
    let size = (out.len() - start) as u32;
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

fn full_box(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Vec<u8>),
) {
    write_box(out, kind, |o| {
        o.extend_from_slice(&((version as u32) << 24 | flags).to_be_bytes());
        body(o);
    });
}

fn put16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn put32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn put64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn zeros(o: &mut Vec<u8>, n: usize) {
    o.resize(o.len() + n, 0);
}

const MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

pub struct InitParams<'a> {
    pub width: u32,
    pub height: u32,
    /// Display aspect ratio; non-square pixels get a `pasp` box.
    pub dar: (u32, u32),
    pub timescale: u32,
    pub sps: &'a [u8],
    pub pps: &'a [u8],
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

/// `ftyp` + `moov` for a single H.264 track with id 1 and no edit list.
pub fn write_init(p: &InitParams) -> Vec<u8> {
    // Pixel aspect ratio = DAR / (width / height), reduced.
    let (mut par_w, mut par_h) = (
        p.dar.0 as u64 * p.height as u64,
        p.dar.1 as u64 * p.width as u64,
    );
    let g = gcd(par_w, par_h).max(1);
    par_w /= g;
    par_h /= g;
    let square = par_w == par_h || par_w == 0 || par_h == 0 || par_w.max(par_h) > u32::MAX as u64;
    let display_width = if square {
        p.width
    } else {
        (p.width as u64 * par_w / par_h) as u32
    };

    let mut out = Vec::with_capacity(1024);
    write_box(&mut out, b"ftyp", |o| {
        o.extend_from_slice(b"iso5");
        put32(o, 512);
        o.extend_from_slice(b"iso5iso6mp41avc1");
    });
    write_box(&mut out, b"moov", |o| {
        full_box(o, b"mvhd", 0, 0, |o| {
            put32(o, 0);
            put32(o, 0);
            put32(o, 1000);
            put32(o, 0);
            put32(o, 0x10000); // rate
            put16(o, 0x100); // volume
            zeros(o, 10);
            MATRIX.iter().for_each(|v| put32(o, *v));
            zeros(o, 24);
            put32(o, 2); // next track id
        });
        write_box(o, b"trak", |o| {
            full_box(o, b"tkhd", 0, 3, |o| {
                put32(o, 0);
                put32(o, 0);
                put32(o, 1); // track id
                put32(o, 0);
                put32(o, 0); // duration
                zeros(o, 8);
                put16(o, 0); // layer
                put16(o, 0); // alternate group
                put16(o, 0); // volume
                put16(o, 0);
                MATRIX.iter().for_each(|v| put32(o, *v));
                put32(o, display_width << 16);
                put32(o, p.height << 16);
            });
            write_box(o, b"mdia", |o| {
                full_box(o, b"mdhd", 0, 0, |o| {
                    put32(o, 0);
                    put32(o, 0);
                    put32(o, p.timescale);
                    put32(o, 0);
                    put16(o, 0x55c4); // "und"
                    put16(o, 0);
                });
                full_box(o, b"hdlr", 0, 0, |o| {
                    put32(o, 0);
                    o.extend_from_slice(b"vide");
                    zeros(o, 12);
                    o.extend_from_slice(b"VideoHandler\0");
                });
                write_box(o, b"minf", |o| {
                    full_box(o, b"vmhd", 0, 1, |o| zeros(o, 8));
                    write_box(o, b"dinf", |o| {
                        full_box(o, b"dref", 0, 0, |o| {
                            put32(o, 1);
                            full_box(o, b"url ", 0, 1, |_| {});
                        });
                    });
                    write_box(o, b"stbl", |o| {
                        full_box(o, b"stsd", 0, 0, |o| {
                            put32(o, 1);
                            write_box(o, b"avc1", |o| {
                                zeros(o, 6);
                                put16(o, 1); // data reference index
                                zeros(o, 16);
                                put16(o, p.width as u16);
                                put16(o, p.height as u16);
                                put32(o, 0x48_0000); // 72 dpi
                                put32(o, 0x48_0000);
                                put32(o, 0);
                                put16(o, 1); // frames per sample
                                zeros(o, 32); // compressor name
                                put16(o, 0x18); // depth
                                put16(o, 0xffff);
                                write_box(o, b"avcC", |o| {
                                    o.push(1);
                                    o.extend_from_slice(&p.sps[1..4]); // profile, compat, level
                                    o.push(0xff); // 4-byte NAL lengths
                                    o.push(0xe1);
                                    put16(o, p.sps.len() as u16);
                                    o.extend_from_slice(p.sps);
                                    o.push(1);
                                    put16(o, p.pps.len() as u16);
                                    o.extend_from_slice(p.pps);
                                    if matches!(p.sps[1], 100 | 110 | 122 | 144) {
                                        // 4:2:0, 8-bit luma and chroma, no SPS extensions
                                        o.extend_from_slice(&[0xfd, 0xf8, 0xf8, 0]);
                                    }
                                });
                                write_box(o, b"colr", |o| {
                                    o.extend_from_slice(b"nclx");
                                    put16(o, p.primaries as u16);
                                    put16(o, p.transfer as u16);
                                    put16(o, p.matrix as u16);
                                    o.push((p.full_range as u8) << 7);
                                });
                                if !square {
                                    write_box(o, b"pasp", |o| {
                                        put32(o, par_w as u32);
                                        put32(o, par_h as u32);
                                    });
                                }
                            });
                        });
                        full_box(o, b"stts", 0, 0, |o| put32(o, 0));
                        full_box(o, b"stsc", 0, 0, |o| put32(o, 0));
                        full_box(o, b"stsz", 0, 0, |o| zeros(o, 8));
                        full_box(o, b"stco", 0, 0, |o| put32(o, 0));
                    });
                });
            });
        });
        write_box(o, b"mvex", |o| {
            full_box(o, b"trex", 0, 0, |o| {
                put32(o, 1); // track id
                put32(o, 1); // sample description index
                zeros(o, 12);
            });
        });
    });
    out
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

pub struct OutSample {
    pub size: u32,
    pub duration: u32,
    pub keyframe: bool,
}

/// Appends one `moof` + `mdat` for track 1 to `out`. There are no
/// composition offsets: samples are in presentation order and `base_time`
/// is the first one's decode and presentation time.
pub fn write_fragment(
    out: &mut Vec<u8>,
    sequence: u32,
    base_time: u64,
    samples: &[OutSample],
    payload: &[u8],
) {
    let moof_start = out.len();
    let mut data_offset_at = 0;
    write_box(out, b"moof", |o| {
        full_box(o, b"mfhd", 0, 0, |o| put32(o, sequence));
        write_box(o, b"traf", |o| {
            full_box(o, b"tfhd", 0, 0x02_0000, |o| put32(o, 1)); // default-base-is-moof
            full_box(o, b"tfdt", 1, 0, |o| put64(o, base_time));
            // data offset, per-sample duration, size and flags
            full_box(o, b"trun", 0, 0x1 | 0x100 | 0x200 | 0x400, |o| {
                put32(o, samples.len() as u32);
                data_offset_at = o.len();
                put32(o, 0);
                for s in samples {
                    put32(o, s.duration);
                    put32(o, s.size);
                    // depends on no other sample / depends on others and is not a sync sample
                    put32(o, if s.keyframe { 0x0200_0000 } else { 0x0101_0000 });
                }
            });
        });
    });
    let data_offset = (out.len() - moof_start + 8) as u32;
    out[data_offset_at..data_offset_at + 4].copy_from_slice(&data_offset.to_be_bytes());
    write_box(out, b"mdat", |o| o.extend_from_slice(payload));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_init() -> Vec<u8> {
        write_init(&InitParams {
            width: 1280,
            height: 720,
            dar: (16, 9),
            timescale: 15360,
            sps: &[0x67, 100, 0, 31, 0xac],
            pps: &[0x68, 0xee],
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
        })
    }

    #[test]
    fn init_segment_reads_back() {
        let init = sample_init();
        let top = boxes(&init, 0, init.len());
        assert_eq!(
            top.iter().map(|b| &b.kind).collect::<Vec<_>>(),
            [b"ftyp", b"moov"]
        );
        let track = parse_init(&init, &top[1]).unwrap().unwrap();
        assert!(track.is_h264());
        assert_eq!(
            (track.id, track.timescale, track.width, track.height),
            (1, 15360, 1280, 720)
        );
        assert_eq!((track.nal_length_size, track.edit_offset), (4, 0));
        assert_eq!(track.sps, [vec![0x67, 100, 0, 31, 0xac]]);
        assert_eq!(track.pps, [vec![0x68, 0xee]]);
    }

    #[test]
    fn fragment_reads_back() {
        let mut data = sample_init();
        let payload = [1u8, 2, 3, 4, 5, 6, 7];
        let samples = [
            OutSample {
                size: 3,
                duration: 512,
                keyframe: true,
            },
            OutSample {
                size: 4,
                duration: 256,
                keyframe: false,
            },
        ];
        write_fragment(&mut data, 7, 30720, &samples, &payload);
        let top = boxes(&data, 0, data.len());
        assert_eq!(
            top.iter().map(|b| &b.kind).collect::<Vec<_>>(),
            [b"ftyp", b"moov", b"moof", b"mdat"]
        );
        let track = parse_init(&data, &top[1]).unwrap().unwrap();
        let read = parse_fragment(&data, &top[2], &top[3], &track).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(
            (read[0].pts, read[0].size, read[0].duration),
            (30720, 3, 512)
        );
        assert_eq!(
            (read[1].pts, read[1].size, read[1].duration),
            (31232, 4, 256)
        );
        assert_eq!(&data[read[1].offset..read[1].offset + 4], &[4, 5, 6, 7]);
    }

    #[test]
    fn damaged_input_is_an_error_not_a_panic() {
        let mut data = sample_init();
        write_fragment(
            &mut data,
            1,
            0,
            &[OutSample {
                size: 9,
                duration: 1,
                keyframe: true,
            }],
            &[0; 4],
        );
        let top = boxes(&data, 0, data.len());
        let track = parse_init(&data, &top[1]).unwrap().unwrap();
        assert!(parse_fragment(&data, &top[2], &top[3], &track).is_err());
        for cut in 0..data.len() {
            let part = &data[..cut];
            let top = boxes(part, 0, part.len());
            if let Some(moov) = top.iter().find(|b| &b.kind == b"moov") {
                let _ = parse_init(part, moov);
            }
        }
    }
}
