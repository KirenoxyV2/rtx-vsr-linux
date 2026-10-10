#!/bin/bash
# Does PyAV's bundled ffmpeg expose nvenc encoders?
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import av
print("PyAV:", av.__version__, "| FFmpeg:", av.ffmpeg_version)
names = [e.name for e in av.codecs_available if "nvenc" in e.name] if hasattr(av, "codecs_available") else None
if names is None:
    # try opening directly
    try:
        import io
        c = av.open(io.BytesIO(), "w", format="mp4")
        s = c.add_stream("h264_nvenc", rate=30)
        print("h264_nvenc OPENED OK, default options:", s.options)
        c.close()
    except Exception as e:
        print("h264_nvenc failed:", type(e).__name__, str(e)[:200])
else:
    print([n for n in names if "nvenc" in n])
PYEOF
