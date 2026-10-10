//! GPU name and memory through NVML, which (unlike CUDA) costs no GPU memory
//! and no context: used for the status the window shows.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::cuda::sym;

#[repr(C)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

#[allow(non_snake_case)]
struct Nvml {
    _lib: Library,
    nvmlDeviceGetHandleByIndex: unsafe extern "C" fn(c_uint, *mut *mut c_void) -> c_int,
    nvmlDeviceGetName: unsafe extern "C" fn(*mut c_void, *mut c_char, c_uint) -> c_int,
    nvmlDeviceGetMemoryInfo: unsafe extern "C" fn(*mut c_void, *mut Memory) -> c_int,
    nvmlSystemGetDriverVersion: unsafe extern "C" fn(*mut c_char, c_uint) -> c_int,
}

#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub name: String,
    pub driver: String,
    pub total_mb: u64,
    pub free_mb: u64,
}

fn nvml() -> Result<&'static Nvml, String> {
    static NVML: OnceLock<Result<Nvml, String>> = OnceLock::new();
    NVML.get_or_init(|| unsafe {
        let lib = Library::new("libnvidia-ml.so.1")
            .map_err(|_| "No NVIDIA driver found (libnvidia-ml.so.1 is missing).".to_string())?;
        let init: unsafe extern "C" fn() -> c_int = sym!(lib, "nvmlInit_v2");
        let nvml = Nvml {
            nvmlDeviceGetHandleByIndex: sym!(lib, "nvmlDeviceGetHandleByIndex_v2"),
            nvmlDeviceGetName: sym!(lib, "nvmlDeviceGetName"),
            nvmlDeviceGetMemoryInfo: sym!(lib, "nvmlDeviceGetMemoryInfo"),
            nvmlSystemGetDriverVersion: sym!(lib, "nvmlSystemGetDriverVersion"),
            _lib: lib,
        };
        match init() {
            0 => Ok(nvml),
            code => Err(format!(
                "The NVIDIA driver is not responding (NVML error {code})."
            )),
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub fn query(index: u32) -> Result<GpuInfo, String> {
    let nvml = nvml()?;
    let text =
        |buf: &[c_char]| unsafe { CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned() };
    unsafe {
        let mut device = std::ptr::null_mut();
        if (nvml.nvmlDeviceGetHandleByIndex)(index, &mut device) != 0 {
            return Err("No NVIDIA GPU found.".into());
        }
        let mut info = GpuInfo::default();
        let mut buf = [0 as c_char; 128];
        if (nvml.nvmlDeviceGetName)(device, buf.as_mut_ptr(), buf.len() as c_uint) == 0 {
            info.name = text(&buf);
        }
        let mut buf = [0 as c_char; 128];
        if (nvml.nvmlSystemGetDriverVersion)(buf.as_mut_ptr(), buf.len() as c_uint) == 0 {
            info.driver = text(&buf);
        }
        let mut mem = Memory {
            total: 0,
            free: 0,
            used: 0,
        };
        if (nvml.nvmlDeviceGetMemoryInfo)(device, &mut mem) == 0 {
            info.total_mb = mem.total >> 20;
            info.free_mb = mem.free >> 20;
        }
        Ok(info)
    }
}
