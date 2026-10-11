//! NVENC through the driver's `libnvidia-encode.so.1`: NV12 pictures in CUDA
//! memory in, H.264 access units out.
//!
//! The stream is shaped for MSE: no B-frames (so PTS == DTS), an IDR whenever
//! the caller asks for one, and the source's colour description in the VUI.

use std::mem::zeroed;
use std::os::raw::{c_char, c_void};
use std::rc::Rc;

use libloading::Library;

use super::cuda::{sym, CUdeviceptr, Cuda};
use super::sys::nvcodec as nv;
use super::vfx::Nv12;

// What bindgen cannot translate from nvEncodeAPI.h (SDK 13.0).
const NVENCAPI_VERSION: u32 = nv::NVENCAPI_MAJOR_VERSION | (nv::NVENCAPI_MINOR_VERSION << 24);
const fn struct_version(v: u32) -> u32 {
    NVENCAPI_VERSION | (v << 16) | (0x7 << 28)
}
const FUNCTION_LIST_VER: u32 = struct_version(2);
const OPEN_SESSION_VER: u32 = struct_version(1);
const INITIALIZE_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
const CONFIG_VER: u32 = struct_version(9) | (1 << 31);
const PRESET_CONFIG_VER: u32 = struct_version(5) | (1 << 31);
const PIC_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
const LOCK_BITSTREAM_VER: u32 = struct_version(2) | (1 << 31);
const CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
const REGISTER_RESOURCE_VER: u32 = struct_version(5);
const MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
const SEQUENCE_PARAM_PAYLOAD_VER: u32 = struct_version(1);

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> nv::GUID {
    nv::GUID {
        Data1: d1,
        Data2: d2,
        Data3: d3,
        Data4: d4,
    }
}
const CODEC_H264: nv::GUID = guid(
    0x6bc82762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
const PROFILE_HIGH: nv::GUID = guid(
    0xe7cbc309,
    0x4f7a,
    0x4b89,
    [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
);
const PRESET_P4: nv::GUID = guid(
    0x90a7b826,
    0xdf06,
    0x4862,
    [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
);

pub struct Nvenc {
    _lib: Library,
    api: nv::NV_ENCODE_API_FUNCTION_LIST,
}

impl Nvenc {
    pub fn load() -> Result<Self, String> {
        unsafe {
            let lib = Library::new("libnvidia-encode.so.1")
                .map_err(|e| format!("NVENC not available (libnvidia-encode.so.1: {e})"))?;
            let max_version: unsafe extern "C" fn(*mut u32) -> nv::NVENCSTATUS =
                sym!(lib, "NvEncodeAPIGetMaxSupportedVersion");
            let create: unsafe extern "C" fn(
                *mut nv::NV_ENCODE_API_FUNCTION_LIST,
            ) -> nv::NVENCSTATUS = sym!(lib, "NvEncodeAPICreateInstance");
            let mut supported = 0u32;
            max_version(&mut supported);
            let wanted = (nv::NVENCAPI_MAJOR_VERSION << 4) | nv::NVENCAPI_MINOR_VERSION;
            if supported < wanted {
                return Err(format!(
                    "the NVIDIA driver is too old for NVENC API {}.{} (driver 570 or newer is needed)",
                    nv::NVENCAPI_MAJOR_VERSION,
                    nv::NVENCAPI_MINOR_VERSION
                ));
            }
            let mut api: nv::NV_ENCODE_API_FUNCTION_LIST = zeroed();
            api.version = FUNCTION_LIST_VER;
            let status = create(&mut api);
            if status != nv::NV_ENC_SUCCESS {
                return Err(format!("NvEncodeAPICreateInstance failed ({status})"));
            }
            Ok(Nvenc { _lib: lib, api })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    /// Frame rate as a fraction; used for rate control only.
    pub fps: (u32, u32),
    pub dar: (u32, u32),
    /// Frames between forced keyframes inside a fragment.
    pub gop: u32,
    pub max_bitrate: u32,
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

/// One picture's worth of encoder buffers: where the caller writes it and
/// where its bitstream comes out.
struct Slot {
    input: CUdeviceptr,
    registered: *mut c_void,
    /// Set while the encoder reads the picture.
    mapped: *mut c_void,
    bitstream: *mut c_void,
}

pub struct Encoder {
    nvenc: Rc<Nvenc>,
    cuda: Rc<Cuda>,
    session: *mut c_void,
    pitch: u32,
    /// Two, so that the next picture can be prepared while this one encodes.
    slots: [Slot; 2],
    /// The slot the next `submit` reads.
    next: usize,
    /// A picture is being encoded (in the other slot).
    busy: bool,
    config: EncoderConfig,
    /// SPS and PPS NAL units (without start codes).
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

/// Constant-quality target for the VBR rate control (lower is better).
const TARGET_QUALITY: u8 = 21;

impl Encoder {
    pub fn new(nvenc: Rc<Nvenc>, cuda: Rc<Cuda>, config: EncoderConfig) -> Result<Self, String> {
        let mut enc = Encoder {
            nvenc,
            cuda,
            session: std::ptr::null_mut(),
            pitch: config.width.next_multiple_of(64),
            slots: std::array::from_fn(|_| Slot {
                input: 0,
                registered: std::ptr::null_mut(),
                mapped: std::ptr::null_mut(),
                bitstream: std::ptr::null_mut(),
            }),
            next: 0,
            busy: false,
            config,
            sps: Vec::new(),
            pps: Vec::new(),
        };
        // On error `enc` is dropped, which releases whatever was created.
        unsafe { enc.open()? };
        Ok(enc)
    }

    unsafe fn open(&mut self) -> Result<(), String> {
        let nvenc = self.nvenc.clone();
        let api = &nvenc.api;
        let c = self.config;

        let mut open: nv::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = zeroed();
        open.version = OPEN_SESSION_VER;
        open.deviceType = nv::NV_ENC_DEVICE_TYPE_CUDA;
        open.device = self.cuda.ctx;
        open.apiVersion = NVENCAPI_VERSION;
        let status = api.nvEncOpenEncodeSessionEx.ok_or("NVENC API incomplete")?(
            &mut open,
            &mut self.session,
        );
        if status != nv::NV_ENC_SUCCESS {
            self.session = std::ptr::null_mut();
            return Err(format!("cannot open an NVENC session ({status})"));
        }

        let mut preset: Box<nv::NV_ENC_PRESET_CONFIG> = Box::new(zeroed());
        preset.version = PRESET_CONFIG_VER;
        preset.presetCfg.version = CONFIG_VER;
        self.check(
            api.nvEncGetEncodePresetConfigEx
                .ok_or("NVENC API incomplete")?(
                self.session,
                CODEC_H264,
                PRESET_P4,
                nv::NV_ENC_TUNING_INFO_HIGH_QUALITY,
                &mut *preset,
            ),
            "reading the NVENC preset",
        )?;

        let mut cfg: Box<nv::NV_ENC_CONFIG> = Box::new(preset.presetCfg);
        cfg.version = CONFIG_VER;
        cfg.profileGUID = PROFILE_HIGH;
        cfg.gopLength = c.gop;
        cfg.frameIntervalP = 1; // I and P only
        let rc = &mut cfg.rcParams;
        rc.rateControlMode = nv::NV_ENC_PARAMS_RC_VBR;
        rc.targetQuality = TARGET_QUALITY;
        rc.averageBitRate = 0;
        rc.maxBitRate = c.max_bitrate;
        rc.vbvBufferSize = c.max_bitrate;
        rc.set_enableAQ(1);
        rc.set_enableLookahead(0);
        rc.set_zeroReorderDelay(1);
        let h264 = &mut cfg.encodeCodecConfig.h264Config;
        h264.idrPeriod = c.gop;
        h264.set_repeatSPSPPS(0);
        h264.set_outputAUD(0);
        h264.level = nv::NV_ENC_LEVEL_AUTOSELECT;
        h264.chromaFormatIDC = 1;
        h264.entropyCodingMode = nv::NV_ENC_H264_ENTROPY_CODING_MODE_CABAC;
        let vui = &mut h264.h264VUIParameters;
        vui.videoSignalTypePresentFlag = 1;
        vui.videoFormat = 5; // unspecified
        vui.videoFullRangeFlag = c.full_range as u32;
        vui.colourDescriptionPresentFlag = 1;
        vui.colourPrimaries = c.primaries as _;
        vui.transferCharacteristics = c.transfer as _;
        vui.colourMatrix = c.matrix as _;
        vui.bitstreamRestrictionFlag = 1;

        let mut init: Box<nv::NV_ENC_INITIALIZE_PARAMS> = Box::new(zeroed());
        init.version = INITIALIZE_PARAMS_VER;
        init.encodeGUID = CODEC_H264;
        init.presetGUID = PRESET_P4;
        init.encodeWidth = c.width;
        init.encodeHeight = c.height;
        init.darWidth = c.dar.0;
        init.darHeight = c.dar.1;
        init.frameRateNum = c.fps.0;
        init.frameRateDen = c.fps.1;
        init.enablePTD = 1;
        init.encodeConfig = &mut *cfg;
        init.maxEncodeWidth = c.width;
        init.maxEncodeHeight = c.height;
        init.tuningInfo = nv::NV_ENC_TUNING_INFO_HIGH_QUALITY;
        self.check(
            api.nvEncInitializeEncoder.ok_or("NVENC API incomplete")?(self.session, &mut *init),
            "initialising the encoder",
        )?;

        let bytes = self.pitch as usize * c.height as usize * 3 / 2;
        for i in 0..self.slots.len() {
            let mut bs: nv::NV_ENC_CREATE_BITSTREAM_BUFFER = zeroed();
            bs.version = CREATE_BITSTREAM_BUFFER_VER;
            self.check(
                api.nvEncCreateBitstreamBuffer
                    .ok_or("NVENC API incomplete")?(self.session, &mut bs),
                "creating the output buffer",
            )?;
            self.slots[i].bitstream = bs.bitstreamBuffer;

            self.slots[i].input = self.cuda.alloc(bytes)?;
            let mut reg: Box<nv::NV_ENC_REGISTER_RESOURCE> = Box::new(zeroed());
            reg.version = REGISTER_RESOURCE_VER;
            reg.resourceType = nv::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR;
            reg.width = c.width;
            reg.height = c.height;
            reg.pitch = self.pitch;
            reg.resourceToRegister = self.slots[i].input as *mut c_void;
            reg.bufferFormat = nv::NV_ENC_BUFFER_FORMAT_NV12;
            reg.bufferUsage = nv::NV_ENC_INPUT_IMAGE;
            self.check(
                api.nvEncRegisterResource.ok_or("NVENC API incomplete")?(self.session, &mut *reg),
                "registering the input buffer",
            )?;
            self.slots[i].registered = reg.registeredResource;
        }

        let mut buf = [0u8; 1024];
        let mut size = 0u32;
        let mut seq: nv::NV_ENC_SEQUENCE_PARAM_PAYLOAD = zeroed();
        seq.version = SEQUENCE_PARAM_PAYLOAD_VER;
        seq.inBufferSize = buf.len() as u32;
        seq.spsppsBuffer = buf.as_mut_ptr() as *mut c_void;
        seq.outSPSPPSPayloadSize = &mut size;
        self.check(
            api.nvEncGetSequenceParams.ok_or("NVENC API incomplete")?(self.session, &mut seq),
            "reading SPS/PPS",
        )?;
        for nal in crate::engine::h264::split_annex_b(&buf[..size as usize]) {
            match nal.first().map(|b| b & 0x1f) {
                Some(7) => self.sps = nal.to_vec(),
                Some(8) => self.pps = nal.to_vec(),
                _ => {}
            }
        }
        if self.sps.is_empty() || self.pps.is_empty() {
            return Err("the encoder returned no SPS/PPS".into());
        }
        Ok(())
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// The picture the next `submit` call will read.
    pub fn input(&self) -> Nv12 {
        Nv12 {
            ptr: self.slots[self.next].input,
            pitch: self.pitch,
            uv_offset: self.pitch as u64 * self.config.height as u64,
        }
    }

    /// Starts encoding the input picture and returns without waiting for it:
    /// the encoder is hardware of its own, so the next picture can be worked
    /// out (into `input`, which moves on to the other buffer) meanwhile.
    /// `collect` must be called before the next `submit`.
    pub fn submit(&mut self, pts: u64, force_idr: bool) -> Result<(), String> {
        if self.busy {
            return Err("NVENC: the previous picture was not collected".into());
        }
        let nvenc = self.nvenc.clone();
        let api = &nvenc.api;
        let slot = self.next;
        unsafe {
            let mut map: Box<nv::NV_ENC_MAP_INPUT_RESOURCE> = Box::new(zeroed());
            map.version = MAP_INPUT_RESOURCE_VER;
            map.registeredResource = self.slots[slot].registered;
            self.check(
                api.nvEncMapInputResource.ok_or("NVENC API incomplete")?(self.session, &mut *map),
                "mapping the input buffer",
            )?;
            self.slots[slot].mapped = map.mappedResource;

            let mut pic: Box<nv::NV_ENC_PIC_PARAMS> = Box::new(zeroed());
            pic.version = PIC_PARAMS_VER;
            pic.inputWidth = self.config.width;
            pic.inputHeight = self.config.height;
            pic.inputPitch = self.pitch;
            pic.inputTimeStamp = pts;
            pic.inputBuffer = map.mappedResource;
            pic.outputBitstream = self.slots[slot].bitstream;
            pic.bufferFmt = map.mappedBufferFmt;
            pic.pictureStruct = nv::NV_ENC_PIC_STRUCT_FRAME;
            if force_idr {
                pic.encodePicFlags =
                    nv::NV_ENC_PIC_FLAG_FORCEIDR | nv::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
            }
            let result = self.check(
                api.nvEncEncodePicture.ok_or("NVENC API incomplete")?(self.session, &mut *pic),
                "encoding a frame",
            );
            if result.is_err() {
                self.unmap(slot);
                return result;
            }
        }
        self.busy = true;
        self.next = 1 - slot;
        Ok(())
    }

    /// Waits for the picture that `submit` started; appends its access unit
    /// (Annex B) to `out` and returns whether it is an IDR picture. `None`
    /// if no picture is being encoded.
    pub fn collect(&mut self, out: &mut Vec<u8>) -> Result<Option<bool>, String> {
        if !self.busy {
            return Ok(None);
        }
        self.busy = false;
        let nvenc = self.nvenc.clone();
        let api = &nvenc.api;
        let slot = 1 - self.next;
        unsafe {
            let mut lock: Box<nv::NV_ENC_LOCK_BITSTREAM> = Box::new(zeroed());
            lock.version = LOCK_BITSTREAM_VER;
            lock.outputBitstream = self.slots[slot].bitstream;
            let result = self.check(
                api.nvEncLockBitstream.ok_or("NVENC API incomplete")?(self.session, &mut *lock),
                "reading the encoded frame",
            );
            let mut keyframe = false;
            if result.is_ok() {
                out.extend_from_slice(std::slice::from_raw_parts(
                    lock.bitstreamBufferPtr as *const u8,
                    lock.bitstreamSizeInBytes as usize,
                ));
                keyframe = lock.pictureType == nv::NV_ENC_PIC_TYPE_IDR;
                api.nvEncUnlockBitstream.ok_or("NVENC API incomplete")?(
                    self.session,
                    self.slots[slot].bitstream,
                );
            }
            self.unmap(slot);
            result.map(|()| Some(keyframe))
        }
    }

    fn unmap(&mut self, slot: usize) {
        let mapped = std::mem::replace(&mut self.slots[slot].mapped, std::ptr::null_mut());
        if !mapped.is_null() {
            if let Some(f) = self.nvenc.api.nvEncUnmapInputResource {
                unsafe { f(self.session, mapped) };
            }
        }
    }

    fn check(&self, status: nv::NVENCSTATUS, what: &str) -> Result<(), String> {
        if status == nv::NV_ENC_SUCCESS {
            return Ok(());
        }
        let detail = unsafe {
            match self.nvenc.api.nvEncGetLastErrorString {
                Some(f) if !self.session.is_null() => {
                    let p: *const c_char = f(self.session);
                    if p.is_null() {
                        String::new()
                    } else {
                        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                    }
                }
                _ => String::new(),
            }
        };
        let detail = detail.trim();
        Err(if detail.is_empty() {
            format!("NVENC: {what} failed ({status})")
        } else {
            format!("NVENC: {what} failed ({status}: {detail})")
        })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // A picture still being encoded has to be let through first.
        let _ = self.collect(&mut Vec::new());
        let api = &self.nvenc.api;
        for slot in &self.slots {
            unsafe {
                if !self.session.is_null() {
                    if !slot.registered.is_null() {
                        if let Some(f) = api.nvEncUnregisterResource {
                            f(self.session, slot.registered);
                        }
                    }
                    if !slot.bitstream.is_null() {
                        if let Some(f) = api.nvEncDestroyBitstreamBuffer {
                            f(self.session, slot.bitstream);
                        }
                    }
                }
            }
        }
        if !self.session.is_null() {
            if let Some(f) = api.nvEncDestroyEncoder {
                unsafe { f(self.session) };
            }
        }
        for slot in &self.slots {
            self.cuda.free(slot.input);
        }
    }
}
