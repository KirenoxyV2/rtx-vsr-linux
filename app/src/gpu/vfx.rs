//! NVIDIA VFX SDK: loading the libraries that ship in the `nvidia-vfx` wheel
//! and running the `VideoSuperRes` effect on GPU buffers.
//!
//! The effect takes and returns 8-bit BGRA images in CUDA memory; the SDK's
//! own `NvCVImage_Transfer*` calls convert from and to NV12 on the GPU, so a
//! frame never visits system memory.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};

use super::cuda::{sym, CUdeviceptr, Cuda};
use super::sys::nvcv::*;

type Handle = *mut c_void;
type Stream = *mut c_void;

/// Libraries to load, dependencies first. Their RPATHs do not cover each
/// other, so (like NVIDIA's own Python loader) each one is opened by full
/// path with RTLD_GLOBAL and the next finds it by SONAME.
const LOAD_ORDER: &[&str] = &[
    "libnppc.so.12",
    "libnppial.so.12",
    "libnppicc.so.12",
    "libnppidei.so.12",
    "libnppig.so.12",
    "libcudnn.so.9",
    "libNVCVImage.so",
    "libnvngxruntime.so",
    "libVideoFXLocal.so",
    "libVideoFX.so",
    "libnvVFXVideoSuperRes.so",
];
/// The model itself; its file name carries a version.
const MODEL_PREFIX: &str = "libnvidia-ngx-vsr.so";

/// Files that must be present for a directory to count as a usable runtime.
pub fn runtime_complete(libs_dir: &Path) -> bool {
    LOAD_ORDER.iter().all(|l| libs_dir.join(l).is_file()) && model_file(libs_dir).is_some()
}

fn model_file(libs_dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(libs_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(MODEL_PREFIX))
        })
        .collect();
    found.sort();
    found.pop()
}

#[allow(non_snake_case)]
pub struct Vfx {
    _libs: Vec<Library>,
    NvVFX_CreateEffect: unsafe extern "C" fn(*const c_char, *mut Handle) -> NvCVStatus,
    NvVFX_DestroyEffect: unsafe extern "C" fn(Handle),
    NvVFX_SetU32: unsafe extern "C" fn(Handle, *const c_char, c_uint) -> NvCVStatus,
    NvVFX_SetImage: unsafe extern "C" fn(Handle, *const c_char, *mut NvCVImage) -> NvCVStatus,
    NvVFX_SetCudaStream: unsafe extern "C" fn(Handle, *const c_char, Stream) -> NvCVStatus,
    NvVFX_Load: unsafe extern "C" fn(Handle) -> NvCVStatus,
    NvVFX_Run: unsafe extern "C" fn(Handle, c_int) -> NvCVStatus,
    NvVFX_CudaStreamCreate: unsafe extern "C" fn(*mut Stream) -> NvCVStatus,
    NvVFX_CudaStreamDestroy: unsafe extern "C" fn(Stream) -> NvCVStatus,
    NvCVImage_Alloc: unsafe extern "C" fn(
        *mut NvCVImage,
        c_uint,
        c_uint,
        c_int,
        c_int,
        c_uint,
        c_uint,
        c_uint,
    ) -> NvCVStatus,
    NvCVImage_Dealloc: unsafe extern "C" fn(*mut NvCVImage),
    #[allow(clippy::type_complexity)]
    NvCVImage_TransferFromYUV: unsafe extern "C" fn(
        *const c_void,
        c_int,
        c_int,
        *const c_void,
        *const c_void,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
        *mut NvCVImage,
        *const c_void,
        f32,
        Stream,
        *mut NvCVImage,
    ) -> NvCVStatus,
    #[allow(clippy::type_complexity)]
    NvCVImage_TransferToYUV: unsafe extern "C" fn(
        *const NvCVImage,
        *const c_void,
        *const c_void,
        c_int,
        c_int,
        *const c_void,
        *const c_void,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
        f32,
        Stream,
        *mut NvCVImage,
    ) -> NvCVStatus,
    NvCV_GetErrorStringFromCode: unsafe extern "C" fn(NvCVStatus) -> *const c_char,
}

impl Vfx {
    /// `libs_dir` is the wheel's `nvvfx/libs` directory (its sibling
    /// `nvidia_vfx.libs` holds the CUDA runtime the libraries' RPATH points at).
    pub fn load(libs_dir: &Path) -> Result<Self, String> {
        let open = |path: &Path| unsafe {
            Library::open(Some(path), RTLD_NOW | RTLD_GLOBAL)
                .map_err(|e| format!("cannot load {}: {e}", path.display()))
        };
        let mut libs = Vec::new();
        for name in LOAD_ORDER {
            libs.push(open(&libs_dir.join(name))?);
        }
        let model = model_file(libs_dir)
            .ok_or_else(|| format!("no {MODEL_PREFIX}* in {}", libs_dir.display()))?;
        libs.push(open(&model)?);

        let cv = &libs[LOAD_ORDER
            .iter()
            .position(|l| *l == "libNVCVImage.so")
            .unwrap()];
        let fx = &libs[LOAD_ORDER
            .iter()
            .position(|l| *l == "libVideoFX.so")
            .unwrap()];
        unsafe {
            Ok(Vfx {
                NvVFX_CreateEffect: sym!(fx, "NvVFX_CreateEffect"),
                NvVFX_DestroyEffect: sym!(fx, "NvVFX_DestroyEffect"),
                NvVFX_SetU32: sym!(fx, "NvVFX_SetU32"),
                NvVFX_SetImage: sym!(fx, "NvVFX_SetImage"),
                NvVFX_SetCudaStream: sym!(fx, "NvVFX_SetCudaStream"),
                NvVFX_Load: sym!(fx, "NvVFX_Load"),
                NvVFX_Run: sym!(fx, "NvVFX_Run"),
                NvVFX_CudaStreamCreate: sym!(fx, "NvVFX_CudaStreamCreate"),
                NvVFX_CudaStreamDestroy: sym!(fx, "NvVFX_CudaStreamDestroy"),
                NvCVImage_Alloc: sym!(cv, "NvCVImage_Alloc"),
                NvCVImage_Dealloc: sym!(cv, "NvCVImage_Dealloc"),
                NvCVImage_TransferFromYUV: sym!(cv, "NvCVImage_TransferFromYUV"),
                NvCVImage_TransferToYUV: sym!(cv, "NvCVImage_TransferToYUV"),
                NvCV_GetErrorStringFromCode: sym!(cv, "NvCV_GetErrorStringFromCode"),
                _libs: libs,
            })
        }
    }

    fn check(&self, status: NvCVStatus, what: &str) -> Result<(), String> {
        if status == NVCV_SUCCESS {
            return Ok(());
        }
        let text = unsafe {
            let p = (self.NvCV_GetErrorStringFromCode)(status);
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        Err(format!("{what}: {text} ({status})"))
    }
}

/// An NV12 picture in CUDA memory: luma at `ptr`, interleaved chroma at
/// `ptr + uv_offset`, both with the same row stride.
#[derive(Clone, Copy)]
pub struct Nv12 {
    pub ptr: CUdeviceptr,
    pub pitch: u32,
    pub uv_offset: u64,
}

/// A loaded `VideoSuperRes` effect for one input size, output size and mode.
pub struct Vsr {
    vfx: Rc<Vfx>,
    handle: Handle,
    stream: Stream,
    // The effect keeps pointers to these descriptors, so they must not move.
    src: Box<NvCVImage>,
    dst: Box<NvCVImage>,
}

impl Vsr {
    /// `quality` is the SDK's QualityLevel: 1 LOW … 4 ULTRA.
    pub fn new(
        vfx: Rc<Vfx>,
        cuda: &Cuda,
        (in_w, in_h): (u32, u32),
        (out_w, out_h): (u32, u32),
        quality: u32,
    ) -> Result<Self, String> {
        let mut vsr = Vsr {
            vfx,
            handle: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            src: Box::new(NvCVImage::empty()),
            dst: Box::new(NvCVImage::empty()),
        };
        let v = vsr.vfx.clone();
        let name = |s: &str| CString::new(s).unwrap();
        unsafe {
            v.check(
                (v.NvVFX_CreateEffect)(name("VideoSuperRes").as_ptr(), &mut vsr.handle),
                "creating the VideoSuperRes effect",
            )?;
            v.check(
                (v.NvVFX_CudaStreamCreate)(&mut vsr.stream),
                "creating a CUDA stream",
            )?;
            v.check(
                (v.NvVFX_SetCudaStream)(vsr.handle, name("CudaStream").as_ptr(), vsr.stream),
                "setting the CUDA stream",
            )?;
            v.check(
                (v.NvVFX_SetU32)(vsr.handle, name("QualityLevel").as_ptr(), quality),
                "setting the quality level",
            )?;
            for (img, w, h) in [(&mut *vsr.src, in_w, in_h), (&mut *vsr.dst, out_w, out_h)] {
                v.check(
                    (v.NvCVImage_Alloc)(img, w, h, NVCV_BGRA, NVCV_U8, NVCV_CHUNKY, NVCV_GPU, 0),
                    "allocating a GPU image",
                )?;
                // Conversions from YUV leave alpha alone: make it opaque once.
                cuda.memset(
                    img.pixels as CUdeviceptr,
                    0xff,
                    img.pitch as usize * h as usize,
                )?;
            }
            v.check(
                (v.NvVFX_SetImage)(vsr.handle, name("SrcImage0").as_ptr(), &mut *vsr.src),
                "setting the input image",
            )?;
            v.check(
                (v.NvVFX_SetImage)(vsr.handle, name("DstImage0").as_ptr(), &mut *vsr.dst),
                "setting the output image",
            )?;
            v.check((v.NvVFX_Load)(vsr.handle), "loading the VSR model")?;
        }
        Ok(vsr)
    }

    /// Upscales `src` into `dst`. `colorspace` is the NvCV colourspace word
    /// (matrix | range | chroma siting) used for both conversions, so the
    /// output carries the same YUV encoding as the input.
    pub fn run(&mut self, src: Nv12, dst: Nv12, colorspace: c_uint) -> Result<(), String> {
        let v = &self.vfx;
        let (y, uv) = (
            src.ptr as *const c_void,
            (src.ptr + src.uv_offset) as *const c_void,
        );
        let (oy, ouv) = (
            dst.ptr as *const c_void,
            (dst.ptr + dst.uv_offset) as *const c_void,
        );
        unsafe {
            v.check(
                (v.NvCVImage_TransferFromYUV)(
                    y,
                    1,
                    src.pitch as c_int,
                    uv,
                    uv.byte_add(1),
                    2,
                    src.pitch as c_int,
                    NVCV_YUV420,
                    NVCV_U8,
                    colorspace,
                    NVCV_GPU,
                    &mut *self.src,
                    std::ptr::null(),
                    1.0,
                    self.stream,
                    std::ptr::null_mut(),
                ),
                "converting the decoded frame",
            )?;
            v.check((v.NvVFX_Run)(self.handle, 0), "running the VSR model")?;
            v.check(
                (v.NvCVImage_TransferToYUV)(
                    &*self.dst,
                    std::ptr::null(),
                    oy,
                    1,
                    dst.pitch as c_int,
                    ouv,
                    ouv.byte_add(1),
                    2,
                    dst.pitch as c_int,
                    NVCV_YUV420,
                    NVCV_U8,
                    colorspace,
                    NVCV_GPU,
                    1.0,
                    self.stream,
                    std::ptr::null_mut(),
                ),
                "converting the upscaled frame",
            )
        }
    }
}

impl Drop for Vsr {
    fn drop(&mut self) {
        let v = &self.vfx;
        unsafe {
            if !self.handle.is_null() {
                (v.NvVFX_DestroyEffect)(self.handle);
            }
            (v.NvCVImage_Dealloc)(&mut *self.src);
            (v.NvCVImage_Dealloc)(&mut *self.dst);
            if !self.stream.is_null() {
                (v.NvVFX_CudaStreamDestroy)(self.stream);
            }
        }
    }
}
