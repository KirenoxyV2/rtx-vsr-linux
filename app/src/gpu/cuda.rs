//! The handful of CUDA driver-API calls the pipeline needs, loaded from the
//! driver's `libcuda.so.1` at run time.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};

use libloading::Library;

pub type CUdeviceptr = u64;
pub type CUcontext = *mut c_void;

/// Copies a symbol's address out of `$lib`; the caller keeps the library alive.
macro_rules! sym {
    ($lib:expr, $name:literal) => {
        *$lib
            .get(concat!($name, "\0").as_bytes())
            .map_err(|e| format!("{}: {e}", $name))?
    };
}
pub(crate) use sym;

#[allow(non_snake_case)]
pub struct Cuda {
    _lib: Library,
    cuDeviceGetName: unsafe extern "C" fn(*mut c_char, c_int, c_int) -> c_int,
    cuCtxSetCurrent: unsafe extern "C" fn(CUcontext) -> c_int,
    cuMemAlloc: unsafe extern "C" fn(*mut CUdeviceptr, usize) -> c_int,
    cuMemFree: unsafe extern "C" fn(CUdeviceptr) -> c_int,
    cuMemGetInfo: unsafe extern "C" fn(*mut usize, *mut usize) -> c_int,
    cuMemsetD8: unsafe extern "C" fn(CUdeviceptr, u8, usize) -> c_int,
    cuCtxSynchronize: unsafe extern "C" fn() -> c_int,
    pub ctx: CUcontext,
    pub device: c_int,
}

impl Cuda {
    /// Loads the driver library, retains the primary context of `device` (the
    /// one the VFX SDK's CUDA runtime uses too) and makes it current on the
    /// calling thread.
    pub fn new(device: c_int) -> Result<Self, String> {
        unsafe {
            let lib = Library::new("libcuda.so.1")
                .map_err(|e| format!("NVIDIA driver not found (libcuda.so.1: {e})"))?;
            let init: unsafe extern "C" fn(c_uint) -> c_int = sym!(lib, "cuInit");
            let device_get: unsafe extern "C" fn(*mut c_int, c_int) -> c_int =
                sym!(lib, "cuDeviceGet");
            let retain: unsafe extern "C" fn(*mut CUcontext, c_int) -> c_int =
                sym!(lib, "cuDevicePrimaryCtxRetain");
            let mut cuda = Cuda {
                cuDeviceGetName: sym!(lib, "cuDeviceGetName"),
                cuCtxSetCurrent: sym!(lib, "cuCtxSetCurrent"),
                cuMemAlloc: sym!(lib, "cuMemAlloc_v2"),
                cuMemFree: sym!(lib, "cuMemFree_v2"),
                cuMemGetInfo: sym!(lib, "cuMemGetInfo_v2"),
                cuMemsetD8: sym!(lib, "cuMemsetD8_v2"),
                cuCtxSynchronize: sym!(lib, "cuCtxSynchronize"),
                _lib: lib,
                ctx: std::ptr::null_mut(),
                device: 0,
            };
            check(init(0), "cuInit")?;
            check(device_get(&mut cuda.device, device), "cuDeviceGet")?;
            check(
                retain(&mut cuda.ctx, cuda.device),
                "cuDevicePrimaryCtxRetain",
            )?;
            check((cuda.cuCtxSetCurrent)(cuda.ctx), "cuCtxSetCurrent")?;
            Ok(cuda)
        }
    }

    pub fn device_name(&self) -> String {
        let mut buf = [0 as c_char; 128];
        unsafe {
            if (self.cuDeviceGetName)(buf.as_mut_ptr(), buf.len() as c_int, self.device) != 0 {
                return "NVIDIA GPU".into();
            }
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        }
    }

    /// (free, total) bytes on the device.
    pub fn mem_info(&self) -> Result<(usize, usize), String> {
        let (mut free, mut total) = (0usize, 0usize);
        check(
            unsafe { (self.cuMemGetInfo)(&mut free, &mut total) },
            "cuMemGetInfo",
        )?;
        Ok((free, total))
    }

    pub fn alloc(&self, bytes: usize) -> Result<CUdeviceptr, String> {
        let mut ptr = 0;
        check(unsafe { (self.cuMemAlloc)(&mut ptr, bytes) }, "cuMemAlloc")?;
        Ok(ptr)
    }

    pub fn free(&self, ptr: CUdeviceptr) {
        if ptr != 0 {
            unsafe { (self.cuMemFree)(ptr) };
        }
    }

    pub fn memset(&self, ptr: CUdeviceptr, value: u8, bytes: usize) -> Result<(), String> {
        check(
            unsafe { (self.cuMemsetD8)(ptr, value, bytes) },
            "cuMemsetD8",
        )
    }

    pub fn synchronize(&self) -> Result<(), String> {
        check(unsafe { (self.cuCtxSynchronize)() }, "cuCtxSynchronize")
    }
}

pub fn check(code: c_int, what: &str) -> Result<(), String> {
    match code {
        0 => Ok(()),
        2 => Err(format!("{what}: out of GPU memory")),
        _ => Err(format!("{what} failed (CUDA error {code})")),
    }
}
