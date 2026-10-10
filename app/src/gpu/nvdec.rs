//! NVDEC through the driver's `libnvcuvid.so.1`: H.264 access units in,
//! NV12 pictures in CUDA memory out, in presentation order.
//!
//! The cuvid parser owns the bitstream logic (DPB, reordering) and calls back
//! on the calling thread. A parser cannot be reset, so each fragment gets a
//! fresh one; the decoder itself is kept for as long as the stream's format
//! stays the same.

use std::mem::zeroed;
use std::os::raw::{c_int, c_uint, c_ulonglong, c_void};
use std::rc::Rc;

use libloading::Library;

use super::cuda::{check, sym};
use super::sys::nvcodec as nv;
use super::vfx::Nv12;

#[allow(non_snake_case)]
pub struct Cuvid {
    _lib: Library,
    cuvidCreateVideoParser:
        unsafe extern "C" fn(*mut nv::CUvideoparser, *mut nv::CUVIDPARSERPARAMS) -> c_int,
    cuvidParseVideoData:
        unsafe extern "C" fn(nv::CUvideoparser, *mut nv::CUVIDSOURCEDATAPACKET) -> c_int,
    cuvidDestroyVideoParser: unsafe extern "C" fn(nv::CUvideoparser) -> c_int,
    cuvidCreateDecoder:
        unsafe extern "C" fn(*mut nv::CUvideodecoder, *mut nv::CUVIDDECODECREATEINFO) -> c_int,
    cuvidDestroyDecoder: unsafe extern "C" fn(nv::CUvideodecoder) -> c_int,
    cuvidDecodePicture: unsafe extern "C" fn(nv::CUvideodecoder, *mut nv::CUVIDPICPARAMS) -> c_int,
    cuvidMapVideoFrame64: unsafe extern "C" fn(
        nv::CUvideodecoder,
        c_int,
        *mut c_ulonglong,
        *mut c_uint,
        *mut nv::CUVIDPROCPARAMS,
    ) -> c_int,
    cuvidUnmapVideoFrame64: unsafe extern "C" fn(nv::CUvideodecoder, c_ulonglong) -> c_int,
}

impl Cuvid {
    pub fn load() -> Result<Self, String> {
        unsafe {
            let lib = Library::new("libnvcuvid.so.1")
                .map_err(|e| format!("NVDEC not available (libnvcuvid.so.1: {e})"))?;
            Ok(Cuvid {
                cuvidCreateVideoParser: sym!(lib, "cuvidCreateVideoParser"),
                cuvidParseVideoData: sym!(lib, "cuvidParseVideoData"),
                cuvidDestroyVideoParser: sym!(lib, "cuvidDestroyVideoParser"),
                cuvidCreateDecoder: sym!(lib, "cuvidCreateDecoder"),
                cuvidDestroyDecoder: sym!(lib, "cuvidDestroyDecoder"),
                cuvidDecodePicture: sym!(lib, "cuvidDecodePicture"),
                cuvidMapVideoFrame64: sym!(lib, "cuvidMapVideoFrame64"),
                cuvidUnmapVideoFrame64: sym!(lib, "cuvidUnmapVideoFrame64"),
                _lib: lib,
            })
        }
    }
}

/// What the bitstream says about the pictures being decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// Picture size after cropping, rounded down to even.
    pub width: u32,
    pub height: u32,
    pub full_range: bool,
    /// H.264 VUI code points; 2 means "unspecified".
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    /// Display aspect ratio.
    pub dar: (u32, u32),
}

pub struct DecodedFrame {
    pub nv12: Nv12,
    pub timestamp: i64,
}

pub type Sink<'a> = dyn FnMut(&StreamInfo, &DecodedFrame) -> Result<(), String> + 'a;

type DecoderKey = (u32, u32, [i32; 4], u32);

struct Inner {
    cuvid: Rc<Cuvid>,
    decoder: nv::CUvideodecoder,
    created_for: Option<DecoderKey>,
    info: Option<StreamInfo>,
    /// Only set while `Decoder::decode` runs.
    sink: Option<*mut Sink<'static>>,
    error: Option<String>,
}

pub struct Decoder {
    inner: Box<Inner>,
}

impl Decoder {
    pub fn new(cuvid: Rc<Cuvid>) -> Self {
        Decoder {
            inner: Box::new(Inner {
                cuvid,
                decoder: std::ptr::null_mut(),
                created_for: None,
                info: None,
                sink: None,
                error: None,
            }),
        }
    }

    /// Decodes one self-contained run of access units (Annex B, the first one
    /// carrying SPS/PPS and starting at an IDR) and hands every picture to
    /// `sink` in presentation order, with the timestamp of its access unit.
    /// `clock_rate` is the unit of the timestamps, in Hz.
    pub fn decode(
        &mut self,
        clock_rate: u32,
        units: &[(Vec<u8>, i64)],
        sink: &mut Sink<'_>,
    ) -> Result<(), String> {
        let inner = &mut *self.inner;
        let cuvid = inner.cuvid.clone();
        inner.error = None;
        // The callbacks run inside cuvidParseVideoData below, on this thread,
        // and the pointer is cleared before this function returns.
        inner.sink = Some(unsafe {
            std::mem::transmute::<*mut Sink<'_>, *mut Sink<'static>>(sink as *mut Sink<'_>)
        });

        let mut params: nv::CUVIDPARSERPARAMS = unsafe { zeroed() };
        params.CodecType = nv::cudaVideoCodec_H264;
        params.ulMaxNumDecodeSurfaces = 1; // the sequence callback returns the real count
        params.ulClockRate = clock_rate;
        params.ulMaxDisplayDelay = 2;
        params.pUserData = inner as *mut Inner as *mut c_void;
        params.pfnSequenceCallback = Some(on_sequence);
        params.pfnDecodePicture = Some(on_decode);
        params.pfnDisplayPicture = Some(on_display);

        let mut parser: nv::CUvideoparser = std::ptr::null_mut();
        let mut result = check(
            unsafe { (cuvid.cuvidCreateVideoParser)(&mut parser, &mut params) },
            "cuvidCreateVideoParser",
        );
        if result.is_ok() {
            let feed = |flags: nv::CUvideopacketflags, data: &[u8], timestamp: i64| {
                let mut packet: nv::CUVIDSOURCEDATAPACKET = unsafe { zeroed() };
                packet.flags = flags as _;
                packet.payload_size = data.len() as _;
                packet.payload = if data.is_empty() {
                    std::ptr::null()
                } else {
                    data.as_ptr()
                };
                packet.timestamp = timestamp;
                let rc = unsafe { (cuvid.cuvidParseVideoData)(parser, &mut packet) };
                let inner = unsafe { &mut *(params.pUserData as *mut Inner) };
                match inner.error.take() {
                    Some(e) => Err(e),
                    None => check(rc, "cuvidParseVideoData"),
                }
            };
            for (data, timestamp) in units {
                result = feed(nv::CUVID_PKT_TIMESTAMP, data, *timestamp);
                if result.is_err() {
                    break;
                }
            }
            if result.is_ok() {
                result = feed(nv::CUVID_PKT_ENDOFSTREAM, &[], 0);
            }
            unsafe { (cuvid.cuvidDestroyVideoParser)(parser) };
        }
        self.inner.sink = None;
        result
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if !self.inner.decoder.is_null() {
            unsafe { (self.inner.cuvid.cuvidDestroyDecoder)(self.inner.decoder) };
        }
    }
}

impl Inner {
    /// (Re)creates the decoder for `fmt` if needed; returns the number of
    /// decode surfaces the parser may use.
    fn configure(&mut self, fmt: &nv::CUVIDEOFORMAT) -> Result<u32, String> {
        if fmt.bit_depth_luma_minus8 != 0 || fmt.bit_depth_chroma_minus8 != 0 {
            return Err("more than 8 bits per sample".into());
        }
        if fmt.chroma_format != nv::cudaVideoChromaFormat_420 {
            return Err("chroma format is not 4:2:0".into());
        }
        if fmt.progressive_sequence == 0 {
            return Err("interlaced video".into());
        }
        let area = fmt.display_area;
        let rect = [area.left, area.top, area.right, area.bottom];
        let mut width = (area.right - area.left).max(0) as u32 & !1;
        let mut height = (area.bottom - area.top).max(0) as u32 & !1;
        if width == 0 || height == 0 {
            width = fmt.coded_width & !1;
            height = fmt.coded_height & !1;
        }
        let surfaces = (fmt.min_num_decode_surfaces as u32 + 4).max(8);
        let key: DecoderKey = (fmt.coded_width, fmt.coded_height, rect, surfaces);

        if self.created_for != Some(key) {
            if !self.decoder.is_null() {
                unsafe { (self.cuvid.cuvidDestroyDecoder)(self.decoder) };
                self.decoder = std::ptr::null_mut();
                self.created_for = None;
            }
            let mut ci: nv::CUVIDDECODECREATEINFO = unsafe { zeroed() };
            ci.ulWidth = fmt.coded_width as _;
            ci.ulHeight = fmt.coded_height as _;
            ci.ulNumDecodeSurfaces = surfaces as _;
            ci.CodecType = fmt.codec;
            ci.ChromaFormat = fmt.chroma_format;
            ci.ulCreationFlags = nv::cudaVideoCreate_PreferCUVID as _;
            ci.ulMaxWidth = fmt.coded_width as _;
            ci.ulMaxHeight = fmt.coded_height as _;
            ci.display_area.left = area.left as _;
            ci.display_area.top = area.top as _;
            ci.display_area.right = area.right as _;
            ci.display_area.bottom = area.bottom as _;
            ci.OutputFormat = nv::cudaVideoSurfaceFormat_NV12;
            ci.DeinterlaceMode = nv::cudaVideoDeinterlaceMode_Weave;
            ci.ulTargetWidth = width as _;
            ci.ulTargetHeight = height as _;
            ci.ulNumOutputSurfaces = 2;
            check(
                unsafe { (self.cuvid.cuvidCreateDecoder)(&mut self.decoder, &mut ci) },
                "cuvidCreateDecoder",
            )?;
            self.created_for = Some(key);
        }

        let signal = &fmt.video_signal_description;
        let dar = fmt.display_aspect_ratio;
        self.info = Some(StreamInfo {
            width,
            height,
            full_range: signal.video_full_range_flag() != 0,
            primaries: signal.color_primaries,
            transfer: signal.transfer_characteristics,
            matrix: signal.matrix_coefficients,
            dar: if dar.x > 0 && dar.y > 0 {
                (dar.x as u32, dar.y as u32)
            } else {
                (width, height)
            },
        });
        Ok(surfaces)
    }

    fn display(&mut self, disp: &nv::CUVIDPARSERDISPINFO) -> Result<(), String> {
        let info = self.info.ok_or("picture before any sequence header")?;
        let sink = self.sink.ok_or("no frame sink")?;
        let mut vpp: nv::CUVIDPROCPARAMS = unsafe { zeroed() };
        vpp.progressive_frame = disp.progressive_frame;
        vpp.second_field = disp.repeat_first_field + 1;
        vpp.top_field_first = disp.top_field_first;
        vpp.unpaired_field = (disp.repeat_first_field < 0) as c_int;
        let (mut ptr, mut pitch) = (0 as c_ulonglong, 0 as c_uint);
        check(
            unsafe {
                (self.cuvid.cuvidMapVideoFrame64)(
                    self.decoder,
                    disp.picture_index,
                    &mut ptr,
                    &mut pitch,
                    &mut vpp,
                )
            },
            "cuvidMapVideoFrame",
        )?;
        let frame = DecodedFrame {
            nv12: Nv12 {
                ptr,
                pitch,
                uv_offset: pitch as u64 * info.height as u64,
            },
            timestamp: disp.timestamp,
        };
        let result = unsafe { (*sink)(&info, &frame) };
        unsafe { (self.cuvid.cuvidUnmapVideoFrame64)(self.decoder, ptr) };
        result
    }
}

// The parser treats a zero return as "stop"; the reason is kept in
// `Inner::error` and reported by `Decoder::decode`. None of these may unwind.

unsafe extern "C" fn on_sequence(user: *mut c_void, fmt: *mut nv::CUVIDEOFORMAT) -> c_int {
    let inner = &mut *(user as *mut Inner);
    match inner.configure(&*fmt) {
        Ok(surfaces) => surfaces as c_int,
        Err(e) => {
            inner.error.get_or_insert(e);
            0
        }
    }
}

unsafe extern "C" fn on_decode(user: *mut c_void, pic: *mut nv::CUVIDPICPARAMS) -> c_int {
    let inner = &mut *(user as *mut Inner);
    if inner.decoder.is_null() {
        inner
            .error
            .get_or_insert("picture before any sequence header".into());
        return 0;
    }
    match check(
        (inner.cuvid.cuvidDecodePicture)(inner.decoder, pic),
        "cuvidDecodePicture",
    ) {
        Ok(()) => 1,
        Err(e) => {
            inner.error.get_or_insert(e);
            0
        }
    }
}

unsafe extern "C" fn on_display(user: *mut c_void, disp: *mut nv::CUVIDPARSERDISPINFO) -> c_int {
    let inner = &mut *(user as *mut Inner);
    if disp.is_null() || inner.error.is_some() {
        return 1; // end of stream, or already failing: nothing more to show
    }
    match inner.display(&*disp) {
        Ok(()) => 1,
        Err(e) => {
            inner.error.get_or_insert(e);
            0
        }
    }
}
