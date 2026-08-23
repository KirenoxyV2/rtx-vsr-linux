#!/usr/bin/env python3
"""End-to-end: pull a REAL YouTube DASH video track and run it through /proxy.
Proves the remote path (fetch -> policy -> VSR -> H.264) on actual YouTube bytes."""
import json, re, sys, time, urllib.request

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
BASE = "http://127.0.0.1:8730"

def get_yt_url(vid, want_h):
    req = urllib.request.Request(f"https://www.youtube.com/watch?v={vid}",
                                 headers={"User-Agent": UA, "Accept-Language": "en-US,en;q=0.9"})
    html = urllib.request.urlopen(req, timeout=25).read().decode("utf-8", "replace")
    idx = html.find("ytInitialPlayerResponse")
    if idx < 0:
        raise SystemExit("no ytInitialPlayerResponse (bot wall)")
    start = html.find("{", idx)
    pr, _ = json.JSONDecoder().raw_decode(html[start:])
    sd = pr.get("streamingData") or {}
    adp = [f for f in sd.get("adaptiveFormats", [])
           if f.get("url") and f.get("width") and f.get("height")]
    def score(f):
        codec_rank = {"avc1": 0, "vp9": 1}.get(f.get("videoCodec"), 2)
        return (0 if f["height"] <= want_h else 1, abs(f["height"] - want_h), codec_rank)
    adp.sort(key=score)
    pick = adp[0] if adp else None
    if not pick:  # fall back to legacy combined progressive formats
        comb = [f for f in sd.get("formats", []) if f.get("url") and f.get("height")]
        comb.sort(key=lambda f: abs(f["height"] - want_h))
        pick = comb[0]
    return pick

def main():
    vid = sys.argv[1] if len(sys.argv) > 1 else "aqz-KE-bpKQ"   # Big Buck Bunny (CC, non-DRM)
    want = int(sys.argv[2]) if len(sys.argv) > 2 else 360
    f = get_yt_url(vid, want)
    seg_url = f["url"]
    print(f"# picked itag={f['itag']} {f['width']}x{f['height']} avc1 "
          f"~{int(f.get('clen',0))/1e6:.1f} MB  (want ~{want}p)")
    # run through the proxy: upscale to ~1.5x (policy caps at 1440)
    url = f"{BASE}/proxy?url={urllib.parse.quote(seg_url, safe='')}&mode=2&scale=1.5"
    t0 = time.perf_counter()
    with urllib.request.urlopen(url, timeout=600) as r:
        meta = json.loads(r.headers.get("X-VSR-Meta", "{}"))
        body = r.read()
        code = r.status
    dt = time.perf_counter() - t0
    print(f"# /proxy -> http={code} in {dt:.1f}s")
    print("# meta:", json.dumps(meta, indent=2))
    # sanity: output should be valid mp4 and larger height than source
    open("/tmp/vsr_yt_test.mp4", "wb").write(body)
    print(f"# wrote /tmp/vsr_yt_test.mp4 ({len(body)/1e6:.2f} MB)")

if __name__ == "__main__":
    import urllib.parse
    main()
