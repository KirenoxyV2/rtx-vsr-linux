#!/usr/bin/env python3
"""probe_youtube.py — what does YouTube actually serve? (not one mp4)

Fetches a watch page, extracts ytInitialPlayerResponse, and lists the real
formats/segments. Confirms the DASH structure the proxy must handle:
  - adaptiveFormats: separate video-only & audio-only fMP4/CMAF segments
  - formats: legacy combined progressive MP4 (rare, low res)
  - segment URLs look like googlevideo.com/videoplayback?...&clen=...

Usage: .venv/bin/python spike/probe_youtube.py [VIDEO_ID]
"""
import json
import re
import sys
import urllib.request

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")


def main():
    vid = sys.argv[1] if len(sys.argv) > 1 else "dQw4w9WgXcQ"
    url = f"https://www.youtube.com/watch?v={vid}"
    req = urllib.request.Request(url, headers={
        "User-Agent": UA,
        "Accept-Language": "en-US,en;q=0.9",
    })
    html = urllib.request.urlopen(req, timeout=25).read().decode("utf-8", "replace")

    idx = html.find("ytInitialPlayerResponse")
    if idx < 0:
        print(f"no ytInitialPlayerResponse in page (len={len(html)}) — likely bot wall")
        print(html[:300].replace("\n", " "))
        return 1
    start = html.find("{", idx)
    pr, _ = json.JSONDecoder().raw_decode(html[start:])

    sd = pr.get("streamingData") or {}
    title = (pr.get("videoDetails") or {}).get("title", "?")
    print(f"# video: {title}  ({vid})")
    print(f"# formats: {len(sd.get('formats', []))} combined, "
          f"{len(sd.get('adaptiveFormats', []))} adaptive (DASH)")

    def show(tag, f):
        w = f.get("width", "")
        h = f.get("height", "")
        res = f"{w}x{h}" if w else "-"
        clen = f.get("clen", "?")
        print(f"  itag {f.get('itag'):>4}  {f.get('container','?'):<6} "
              f"v={str(f.get('videoCodec','-')):<5} a={str(f.get('audioCodec','-')):<6} "
              f"{res:<9} {str(f.get('qualityLabel','')):<10} "
              f"~{int(clen)/1e6 if str(clen).isdigit() else '?'} MB")

    print("\n# combined (progressive single file — legacy):")
    for f in sd.get("formats", []):
        show("fmt", f)
    print("\n# adaptive (DASH segments — what modern browsers use):")
    for f in sd.get("adaptiveFormats", []):
        show("adp", f)

    # show one segment URL shape (video-only, H.264 if available)
    ad = [f for f in sd.get("adaptiveFormats", [])
          if f.get("videoCodec") == "avc1" and f.get("width")]
    pick = ad[0] if ad else (sd.get("adaptiveFormats") or [{}])[0]
    u = pick.get("url", "")
    if u:
        print("\n# example segment URL (truncated):")
        print("  " + u[:160] + " ...")
        m = re.search(r"clen=(\d+)", u)
        if m:
            print(f"  clen={m.group(1)} bytes total, delivered as many small segments")
    return 0


if __name__ == "__main__":
    sys.exit(main())
