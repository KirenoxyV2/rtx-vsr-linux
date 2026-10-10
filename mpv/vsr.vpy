# vsr.vpy — RTX Video Super Resolution as a VapourSynth script for mpv.
#
# Works with any mpv built with vf_vapoursynth — including the stock Flathub
# build (io.mpv.Mpv), so no custom mpv is required.
#
# mpv usage:
#   mpv --vf=vapoursynth=<path>/vsr.vpy <file>
#   mpv --vf='vapoursynth=file=<path>/vsr.vpy:user-data="mode=3,maxh=1440"' <file>
#   # or bind a toggle key — see mpv/vsr.lua.
#
# Config: `key=value,...` in the vf's user-data option, else the matching
# VSR_<KEY> environment variable, else the default:
#   mode    1=LOW 2=MEDIUM 3=HIGH 4=ULTRA                      (default 2)
#   scale   upscale factor, or "fit" = fill the target box     (default fit)
#   maxw    target box; sources at/above it pass through untouched
#   maxh      (default: mpv's display resolution, else 3840x2160)
#   fmt     output pixel format: 420p8 | 420p10 | 444p10 | 444p16 (default 420p10)
#   gpu     CUDA device                                        (default 0)
#
# The script finds cupy/nvvfx by itself (VSR_SITE_PACKAGES, or a
# ~/vsr-venv*/ whose python version matches the interpreter mpv embeds), so
# mpv needs no PYTHONPATH / LD_LIBRARY_PATH setup.
#
# Pipeline per frame, all colour math on the GPU: YUV planes (as decoded) ->
# H2D -> YUV->RGB float CHW -> nvvfx VideoSuperRes.run -> RGB->YUV at the
# output size -> D2H straight into the output frame. mpv's bridge only accepts
# integer YUV, and shipping integer planes instead of float RGB also cuts the
# host<->GPU traffic ~4-8x.

import glob
import os
import site
import sys
import threading
import types

import vapoursynth as vs

core = vs.core


# ---------------------------------------------------------------- config
def _config():
    cfg = {}
    for item in str(globals().get("user_data") or "").split(","):
        k, sep, v = item.partition("=")
        if sep:
            cfg[k.strip().lower()] = v.strip()

    def opt(key, default):
        return cfg.get(key) or os.environ.get("VSR_" + key.upper()) or default

    return opt


_opt = _config()
MODE = int(_opt("mode", "2"))
SCALE = _opt("scale", "fit")
GPU = int(_opt("gpu", "0"))
OUT_FMT = _opt("fmt", "420p10").lower()

_FORMATS = {
    "420p8": vs.YUV420P8,
    "420p10": vs.YUV420P10,
    "444p10": vs.YUV444P10,
    "444p16": vs.YUV444P16,
}
if OUT_FMT not in _FORMATS:
    raise ValueError(f"vsr.vpy: fmt must be one of {sorted(_FORMATS)}, got {OUT_FMT!r}")

# `video_in` is injected by mpv's vapoursynth vf; `display_res` too, but it is
# [0, 0] when the VO doesn't exist yet.
clip = video_in  # noqa: F821
_disp = globals().get("display_res") or (0, 0)
MAX_W = int(_opt("maxw", _disp[0] or 3840))
MAX_H = int(_opt("maxh", _disp[1] or 2160))


def _even(n):
    return max(2, int(round(n / 2.0)) * 2)


# ---------------------------------------------------------------- policy
_fit = min(MAX_W / clip.width, MAX_H / clip.height, 4.0)   # model tops out at 4x
_factor = _fit if SCALE == "fit" else min(float(SCALE), _fit)
OUT_W = _even(clip.width * _factor)
OUT_H = _even(clip.height * _factor)
ACTIVE = _factor > 1.0 and (OUT_W > clip.width or OUT_H > clip.height)


# ---------------------------------------------------------------- runtime
def _bootstrap():
    """Make cupy + nvvfx importable from inside mpv's embedded interpreter."""
    # Sandboxed players (Flatpak) mount $HOME read-only; cupy's default kernel
    # cache (~/.cupy) would fail there.
    if "CUPY_CACHE_DIR" not in os.environ and not os.access(os.path.expanduser("~"), os.W_OK):
        cache = os.environ.get("XDG_CACHE_HOME") or "/tmp"
        os.environ["CUPY_CACHE_DIR"] = os.path.join(cache, "cupy")
    try:
        import cupy  # noqa: F401
        import nvvfx  # noqa: F401
        return
    except ImportError:
        pass
    ver = "python%d.%d" % sys.version_info[:2]
    here = os.path.dirname(os.path.realpath(__file__)) if "__file__" in globals() else os.getcwd()
    cands = [os.environ.get("VSR_SITE_PACKAGES")]
    cands += sorted(glob.glob(os.path.expanduser(f"~/vsr-venv*/lib/{ver}/site-packages")))
    cands += glob.glob(os.path.join(here, "..", ".venv*", "lib", ver, "site-packages"))
    for sp in cands:
        if sp and os.path.isdir(os.path.join(sp, "nvvfx")):
            site.addsitedir(sp)
            # cuda-pathfinder locates the pip-shipped CUDA libs through
            # site.getsitepackages(), which an embedded interpreter doesn't
            # extend for added dirs.
            orig = site.getsitepackages
            site.getsitepackages = lambda prefixes=None: [sp] + orig(prefixes)
            return
    raise ImportError(
        f"vsr.vpy: no {ver} site-packages with nvvfx found. Create a venv for "
        f"this interpreter (see README) or set VSR_SITE_PACKAGES.")


if ACTIVE:
    _bootstrap()   # before numpy: the embedded interpreter may not ship it
    import numpy as np


_KERNEL_SRC = r"""
typedef PIX_T pix_t;

static __device__ __forceinline__ float sat(float v) {
    return fminf(fmaxf(v, 0.f), 1.f);
}

// One thread per luma pixel; chroma is sampled bilinearly at its true siting.
extern "C" __global__
void yuv2rgb(const pix_t* Y, const pix_t* U, const pix_t* V, float* out,
             int W, int H, int CW, int CH, int ssw, int ssh,
             float offx, float offy,
             float yoff, float yscale, float coff, float cscale,
             float kr, float kb)
{
    int x = blockIdx.x * blockDim.x + threadIdx.x;
    int y = blockIdx.y * blockDim.y + threadIdx.y;
    if (x >= W || y >= H) return;

    float cx = fminf(fmaxf(((float)x - offx) / (float)(1 << ssw), 0.f), (float)(CW - 1));
    float cy = fminf(fmaxf(((float)y - offy) / (float)(1 << ssh), 0.f), (float)(CH - 1));
    int x0 = (int)cx, y0 = (int)cy;
    int x1 = min(x0 + 1, CW - 1), y1 = min(y0 + 1, CH - 1);
    float fx = cx - (float)x0, fy = cy - (float)y0;
    float w00 = (1.f - fx) * (1.f - fy), w01 = fx * (1.f - fy);
    float w10 = (1.f - fx) * fy,         w11 = fx * fy;
    float u = w00 * U[y0 * CW + x0] + w01 * U[y0 * CW + x1]
            + w10 * U[y1 * CW + x0] + w11 * U[y1 * CW + x1];
    float v = w00 * V[y0 * CW + x0] + w01 * V[y0 * CW + x1]
            + w10 * V[y1 * CW + x0] + w11 * V[y1 * CW + x1];

    float yy = ((float)Y[y * W + x] - yoff) * yscale;
    float cb = (u - coff) * cscale, cr = (v - coff) * cscale;
    float r = yy + 2.f * (1.f - kr) * cr;
    float b = yy + 2.f * (1.f - kb) * cb;
    float g = (yy - kr * r - kb * b) / (1.f - kr - kb);

    size_t n = (size_t)W * H, i = (size_t)y * W + x;
    out[i] = sat(r);
    out[n + i] = sat(g);
    out[2 * n + i] = sat(b);
}

extern "C" __global__
void rgb2y(const float* in, pix_t* Y, int W, int H,
           float yoff, float ymul, float maxv, float kr, float kb)
{
    int x = blockIdx.x * blockDim.x + threadIdx.x;
    int y = blockIdx.y * blockDim.y + threadIdx.y;
    if (x >= W || y >= H) return;
    size_t n = (size_t)W * H, i = (size_t)y * W + x;
    float r = sat(in[i]), g = sat(in[n + i]), b = sat(in[2 * n + i]);
    float yy = kr * r + (1.f - kr - kb) * g + kb * b;
    Y[i] = (pix_t)fminf(fmaxf(yy * ymul + yoff + 0.5f, 0.f), maxv);
}

// One thread per chroma sample: filter RGB down to the chroma grid, then
// convert. left != 0 -> MPEG-2 siting (co-sited with even luma columns),
// else centred (box).
extern "C" __global__
void rgb2c(const float* in, pix_t* U, pix_t* V,
           int W, int H, int CW, int CH, int ssw, int ssh, int left,
           float coff, float cmul, float maxv, float kr, float kb)
{
    int i = blockIdx.x * blockDim.x + threadIdx.x;
    int j = blockIdx.y * blockDim.y + threadIdx.y;
    if (i >= CW || j >= CH) return;
    size_t n = (size_t)W * H;
    int sx = 1 << ssw, sy = 1 << ssh;
    float r = 0.f, g = 0.f, b = 0.f, wsum = 0.f;
    for (int dy = 0; dy < sy; ++dy) {
        int yy = min(j * sy + dy, H - 1);
        int xa = i * sx, xb = i * sx + sx - 1;
        if (left && sx == 2) { xa = i * 2 - 1; xb = i * 2 + 1; }
        for (int xx = xa; xx <= xb; ++xx) {
            float w = (left && sx == 2 && xx == i * 2) ? 2.f : 1.f;
            size_t k = (size_t)yy * W + min(max(xx, 0), W - 1);
            r += w * sat(in[k]); g += w * sat(in[n + k]); b += w * sat(in[2 * n + k]);
            wsum += w;
        }
    }
    r /= wsum; g /= wsum; b /= wsum;
    float yy = kr * r + (1.f - kr - kb) * g + kb * b;
    float cb = (b - yy) / (2.f * (1.f - kb));
    float cr = (r - yy) / (2.f * (1.f - kr));
    U[j * CW + i] = (pix_t)fminf(fmaxf(cb * cmul + coff + 0.5f, 0.f), maxv);
    V[j * CW + i] = (pix_t)fminf(fmaxf(cr * cmul + coff + 0.5f, 0.f), maxv);
}
"""

# Kr, Kb by VS/H.273 matrix code.
_MATRIX = {1: (0.2126, 0.0722), 5: (0.299, 0.114), 6: (0.299, 0.114),
           9: (0.2627, 0.0593)}
_BLOCK = (32, 8)


def _colorimetry(props, height):
    """(kr, kb, limited, left_sited) from frame props, guessing when unset."""
    m = props.get("_Matrix", props.get("_ColorSpace", 2))
    if m not in _MATRIX:
        m = 1 if height > 576 else 6
    kr, kb = _MATRIX[m]
    # VS R74+ renamed _ColorRange (1 = limited) to _Range (0 = limited) and
    # aliases reads of the old name to the new *value*, so go by the real keys.
    keys = set(props)
    if "_Range" in keys:
        limited = int(props["_Range"]) == 0
    elif "_ColorRange" in keys:
        limited = int(props["_ColorRange"]) != 0
    else:
        limited = True
    left = props.get("_ChromaLocation", 0) != 1
    return kr, kb, limited, left


def _levels(bits, limited):
    """(y offset, y range, chroma offset, chroma range, max code value)."""
    peak = (1 << bits) - 1
    if limited:
        s = 1 << (bits - 8)
        return 16.0 * s, 219.0 * s, 128.0 * s, 224.0 * s, float(peak)
    return 0.0, float(peak), float(1 << (bits - 1)), float(peak), float(peak)


def _grid(w, h):
    return (-(-w // _BLOCK[0]), -(-h // _BLOCK[1]))


def _load(in_fmt, in_w, in_h):
    """Loaded model + GPU buffers for one (mode, in size, out size, fmt).

    Returns plain data only: mpv re-evaluates this script on every seek and
    VSScript wipes the old evaluation's globals, so nothing cached across
    evaluations may hold code (or VS objects) from this module.
    """
    import cupy as cp
    from nvvfx import VideoSuperRes

    out_fmt = core.get_video_format(_FORMATS[OUT_FMT])
    st = types.SimpleNamespace(
        cp=cp, lock=threading.Lock(), dev=cp.cuda.Device(GPU),
        in_w=in_w, in_h=in_h, in_bits=in_fmt.bits_per_sample,
        in_ssw=in_fmt.subsampling_w, in_ssh=in_fmt.subsampling_h,
        out_bits=out_fmt.bits_per_sample,
        out_ssw=out_fmt.subsampling_w, out_ssh=out_fmt.subsampling_h,
        out_cw=OUT_W >> out_fmt.subsampling_w, out_ch=OUT_H >> out_fmt.subsampling_h)

    def kernels(bits):
        pix = "unsigned char" if bits == 8 else "unsigned short"
        mod = cp.RawModule(code=_KERNEL_SRC.replace("PIX_T", pix))
        return {n: mod.get_function(n) for n in ("yuv2rgb", "rgb2y", "rgb2c")}

    with st.dev:
        st.k_in, st.k_out = kernels(st.in_bits), kernels(st.out_bits)
        st.rgb = cp.empty((3, in_h, in_w), dtype=cp.float32)
        dt = np.uint8 if st.out_bits == 8 else np.uint16
        st.out = [cp.empty((OUT_H, OUT_W), dtype=dt),
                  cp.empty((st.out_ch, st.out_cw), dtype=dt),
                  cp.empty((st.out_ch, st.out_cw), dtype=dt)]

        st.sr = VideoSuperRes(quality=MODE, device=GPU)
        st.sr.output_width = OUT_W
        st.sr.output_height = OUT_H
        st.sr.load()
        # Warmup: the first run allocates the engine's working memory.
        st.rgb[...] = cp.random.rand(3, in_h, in_w, dtype=cp.float32)
        res = st.sr.run(st.rgb)
        st.dev.synchronize()
        del res
    return st


def _state(in_fmt, in_w, in_h):
    # The interpreter outlives script evaluations, so park the loaded engine
    # in sys.modules: seeks then cost nothing.
    cache = sys.modules.setdefault("_vsr_engines", types.ModuleType("_vsr_engines")).__dict__
    key = ("engine", MODE, GPU, OUT_FMT, in_fmt.id, in_w, in_h, OUT_W, OUT_H)
    if key not in cache:
        for k in [k for k in cache if isinstance(k, tuple)]:   # one engine at a time
            del cache[k]
        cache[key] = _load(in_fmt, in_w, in_h)
    return cache[key]


def _process(st, src, dst):
    cp, f32, i32 = st.cp, np.float32, np.int32
    kr, kb, limited, left = _colorimetry(src.props, st.in_h)
    with st.lock, st.dev:
        y, u, v = (cp.asarray(np.ascontiguousarray(np.asarray(src[p])))
                   for p in range(3))
        yoff, yrange, coff, crange, _ = _levels(st.in_bits, limited)
        sx, sy = 1 << st.in_ssw, 1 << st.in_ssh
        st.k_in["yuv2rgb"](
            _grid(st.in_w, st.in_h), _BLOCK,
            (y, u, v, st.rgb,
             i32(st.in_w), i32(st.in_h), i32(u.shape[1]), i32(u.shape[0]),
             i32(st.in_ssw), i32(st.in_ssh),
             f32(0.0 if left else (sx - 1) / 2.0), f32((sy - 1) / 2.0),
             f32(yoff), f32(1.0 / yrange), f32(coff), f32(1.0 / crange),
             f32(kr), f32(kb)))

        # The result's image is a view into effect-owned memory: keep `res`
        # alive until every read of it has completed (DESIGN §9a).
        res = st.sr.run(st.rgb)
        up = cp.from_dlpack(res.image)
        yoff, yrange, coff, crange, peak = _levels(st.out_bits, limited)
        st.k_out["rgb2y"](
            _grid(OUT_W, OUT_H), _BLOCK,
            (up, st.out[0], i32(OUT_W), i32(OUT_H),
             f32(yoff), f32(yrange), f32(peak), f32(kr), f32(kb)))
        st.k_out["rgb2c"](
            _grid(st.out_cw, st.out_ch), _BLOCK,
            (up, st.out[1], st.out[2],
             i32(OUT_W), i32(OUT_H), i32(st.out_cw), i32(st.out_ch),
             i32(st.out_ssw), i32(st.out_ssh), i32(1 if left else 0),
             f32(coff), f32(crange), f32(peak), f32(kr), f32(kb)))
        st.dev.synchronize()
        del up, res

        for p in range(3):
            host = np.asarray(dst[p])
            if host.flags.c_contiguous:
                st.out[p].get(out=host)
            else:
                np.copyto(host, st.out[p].get())


def _build(clip):
    fmt = clip.format
    if not (fmt.color_family == vs.YUV and fmt.sample_type == vs.INTEGER
            and fmt.bytes_per_sample <= 2):
        clip = core.resize.Bicubic(clip, format=vs.YUV444P16, matrix_s="709")
    st = _state(clip.format, clip.width, clip.height)
    out_fmt = core.get_video_format(_FORMATS[OUT_FMT])
    template = core.std.BlankClip(clip=clip, width=OUT_W, height=OUT_H,
                                  format=out_fmt.id, keep=True)

    def sel(n, f):
        dst = core.create_video_frame(out_fmt, OUT_W, OUT_H)
        _process(st, f, dst)
        # mpv derives timestamps from the _Duration* props; keep them all.
        dst.props.update(f.props)
        return dst

    return core.std.ModifyFrame(clip=template, clips=[clip], selector=sel)


# Sources already at/above the target box get zero AI work.
res_clip = _build(clip) if ACTIVE else clip
res_clip.set_output()
