//! Everything that talks to the GPU: CUDA, NVDEC, NVENC and the VFX SDK.
//! All of it is used from the engine's single worker thread.

pub mod cuda;
pub mod nvdec;
pub mod nvenc;
pub mod nvml;
pub mod sys;
pub mod vfx;

use std::path::Path;
use std::rc::Rc;

pub struct Gpu {
    pub cuda: Rc<cuda::Cuda>,
    pub vfx: Rc<vfx::Vfx>,
    pub cuvid: Rc<nvdec::Cuvid>,
    pub nvenc: Rc<nvenc::Nvenc>,
    pub name: String,
}

impl Gpu {
    /// Creates the CUDA context on `device` and loads the decoder, encoder and
    /// VFX libraries. Must be called on the thread that will use them.
    pub fn open(vfx_libs: &Path, device: i32) -> Result<Self, String> {
        let cuda = Rc::new(cuda::Cuda::new(device)?);
        let name = cuda.device_name();
        Ok(Gpu {
            vfx: Rc::new(vfx::Vfx::load(vfx_libs)?),
            cuvid: Rc::new(nvdec::Cuvid::load()?),
            nvenc: Rc::new(nvenc::Nvenc::load()?),
            cuda,
            name,
        })
    }
}
