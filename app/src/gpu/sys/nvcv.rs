//! `NvCVImage` and the constants that go with it, as declared in the VFX
//! SDK's public headers (nvCVImage.h, nvCVStatus.h). The enum values were
//! checked against the library shipped in the nvidia-vfx 0.2.0.0 wheel.

use std::os::raw::{c_int, c_uint, c_void};

pub type NvCVStatus = c_int;
pub const NVCV_SUCCESS: NvCVStatus = 0;

// NvCVImage_PixelFormat
pub const NVCV_BGRA: c_int = 7;
pub const NVCV_YUV420: c_int = 10;

// NvCVImage_ComponentType
pub const NVCV_U8: c_int = 1;

// layout
pub const NVCV_CHUNKY: c_uint = 0;

// memory space
pub const NVCV_GPU: c_uint = 1;

// colorspace: an OR of matrix, range and chroma siting
pub const NVCV_601: c_uint = 0x00;
pub const NVCV_709: c_uint = 0x01;
pub const NVCV_2020: c_uint = 0x02;
pub const NVCV_VIDEO_RANGE: c_uint = 0x00;
pub const NVCV_FULL_RANGE: c_uint = 0x04;
pub const NVCV_CHROMA_COSITED: c_uint = 0x00;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvCVImage {
    pub width: c_uint,
    pub height: c_uint,
    pub pitch: c_int,
    pub pixel_format: c_int,
    pub component_type: c_int,
    pub pixel_bytes: u8,
    pub component_bytes: u8,
    pub num_components: u8,
    pub planar: u8,
    pub gpu_mem: u8,
    pub colorspace: u8,
    pub reserved: [u8; 2],
    pub pixels: *mut c_void,
    pub delete_ptr: *mut c_void,
    pub delete_proc: Option<unsafe extern "C" fn(*mut c_void)>,
    pub buffer_bytes: u64,
}

impl NvCVImage {
    pub const fn empty() -> Self {
        NvCVImage {
            width: 0,
            height: 0,
            pitch: 0,
            pixel_format: 0,
            component_type: 0,
            pixel_bytes: 0,
            component_bytes: 0,
            num_components: 0,
            planar: 0,
            gpu_mem: 0,
            colorspace: 0,
            reserved: [0; 2],
            pixels: std::ptr::null_mut(),
            delete_ptr: std::ptr::null_mut(),
            delete_proc: None,
            buffer_bytes: 0,
        }
    }
}
