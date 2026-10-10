//! Finding, and if need be fetching, the NVIDIA libraries the model runs on.
//!
//! NVIDIA publishes them as the `nvidia-vfx` Python wheel. They are not
//! redistributable, so the app either uses a copy that pip already put on the
//! machine or downloads the wheel from NVIDIA's own index and unpacks the
//! libraries it needs — no Python involved.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::gpu::vfx::runtime_complete;
use crate::state::{data_dir, home, Runtime, Shared};

const WHEEL: &str = "nvidia_vfx-0.2.0.0-cp312-abi3-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl";
const WHEEL_URL: &str = "https://pypi.nvidia.com/nvidia-vfx/";
const WHEEL_SHA256: &str = "0252866a7b798a6f92c8543443d88f716e8c5bbc6a7ddd77a2754728dada9fa8";

/// Parts of the wheel worth keeping: the libraries the VSR effect loads, the
/// CUDA runtime they link to, and NVIDIA's licence texts.
fn wanted(name: &str) -> bool {
    const SKIP: [&str; 7] = [
        "FrameGeneration",
        "TrueHDR",
        "truehdr",
        "libnppif",
        "libnppim",
        "libnppist",
        "libnppitc",
    ];
    if let Some(lib) = name.strip_prefix("nvvfx/libs/") {
        return !SKIP.iter().any(|s| lib.contains(s));
    }
    name.starts_with("nvidia_vfx.libs/") || name.contains(".dist-info/licenses/")
}

fn managed_root() -> PathBuf {
    data_dir().join("runtime")
}

/// The `nvvfx/libs` directory of a usable runtime, if there is one: the app's
/// own download first, then copies installed with pip.
pub fn find() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("RTX_VSR_RUNTIME") {
        candidates.push(PathBuf::from(&dir));
        candidates.push(PathBuf::from(&dir).join("nvvfx/libs"));
    }
    candidates.push(managed_root().join("nvvfx/libs"));
    let home = home();
    for venv in ["vsr-venv", "vsr-venv-py313", ".local"] {
        let Ok(entries) = fs::read_dir(home.join(venv).join("lib")) else {
            continue;
        };
        let mut pythons: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        pythons.sort();
        for python in pythons {
            candidates.push(python.join("site-packages/nvvfx/libs"));
        }
    }
    candidates.into_iter().find(|dir| runtime_complete(dir))
}

/// Downloads and unpacks the runtime, reporting progress in `shared.runtime`.
/// Leaves it `Ready` or `Failed`.
pub fn install(shared: &Shared) {
    let set = |state: Runtime| *shared.runtime.lock().unwrap() = state;
    set(Runtime::Downloading { done: 0, total: 0 });
    let result =
        download(&|done, total| set(Runtime::Downloading { done, total })).and_then(|wheel| {
            set(Runtime::Unpacking);
            let result = unpack(&wheel);
            let _ = fs::remove_file(&wheel);
            result
        });
    set(match result {
        Ok(libs) => Runtime::Ready(libs),
        Err(e) => Runtime::Failed(e),
    });
}

fn download(progress: &dyn Fn(u64, u64)) -> Result<PathBuf, String> {
    let dir = data_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("runtime.download");
    let url = format!("{WHEEL_URL}{WHEEL}");
    let mut response = ureq::get(&url)
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let total = response.body().content_length().unwrap_or(0);
    let mut reader = response.body_mut().as_reader();
    let mut file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != WHEEL_SHA256 {
        let _ = fs::remove_file(&path);
        return Err("The downloaded file does not match NVIDIA's published checksum.".into());
    }
    Ok(path)
}

fn unpack(wheel: &Path) -> Result<PathBuf, String> {
    let root = managed_root();
    let staging = data_dir().join("runtime.partial");
    let _ = fs::remove_dir_all(&staging);
    let io = |e: std::io::Error| format!("Unpacking failed: {e}");
    let file = File::open(wheel).map_err(io)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Unpacking failed: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Unpacking failed: {e}"))?;
        // `enclosed_name` rejects paths that would escape the destination.
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        if entry.is_dir() || !wanted(&relative.to_string_lossy()) {
            continue;
        }
        let dest = staging.join(&relative);
        fs::create_dir_all(dest.parent().unwrap()).map_err(io)?;
        let mut out = File::create(&dest).map_err(io)?;
        std::io::copy(&mut entry, &mut out).map_err(io)?;
    }
    let libs = staging.join("nvvfx/libs");
    if !runtime_complete(&libs) {
        let _ = fs::remove_dir_all(&staging);
        return Err("The download does not contain the expected libraries.".into());
    }
    let _ = fs::remove_dir_all(&root);
    fs::rename(&staging, &root).map_err(io)?;
    Ok(root.join("nvvfx/libs"))
}

#[cfg(test)]
mod tests {
    use super::wanted;

    #[test]
    fn only_what_vsr_needs_is_unpacked() {
        for keep in [
            "nvvfx/libs/libVideoFX.so",
            "nvvfx/libs/libnvidia-ngx-vsr.so.1.8.3",
            "nvvfx/libs/libnppig.so.12",
            "nvidia_vfx.libs/libcudart-256e6409.so.12.9.79",
            "nvidia_vfx-0.2.0.0.dist-info/licenses/packaging/THIRD_PARTY.md",
        ] {
            assert!(wanted(keep), "{keep}");
        }
        for skip in [
            "nvvfx/libs/libnvVFXVideoFrameGeneration.so",
            "nvvfx/libs/libnvidia-ngx-truehdr.so.1.2.3",
            "nvvfx/libs/libnppif.so.12",
            "nvvfx/__init__.py",
            "nvvfx/_ext.abi3.so",
        ] {
            assert!(!wanted(skip), "{skip}");
        }
    }
}
