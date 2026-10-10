/* RTX VSR — page-world hook.
 *
 * Registered by background.js only on enabled sites, in the MAIN world at
 * document_start, so it is in place before the player touches MSE.
 *
 * What it does to a `video/mp4; codecs="avc1…"` SourceBuffer:
 *   - collects the appended bytes into complete top-level boxes,
 *   - keeps the stream's init segment (ftyp+moov),
 *   - sends init + each complete fragment (moof+mdat) to the RTX VSR app via
 *     content.js -> background.js, and appends the upscaled H.264 fragment the
 *     app returns (preceded by the app's own init segment when it changes),
 *   - on any failure appends the original bytes, so playback never breaks.
 *
 * The page keeps seeing a normal SourceBuffer: `updating` is true and
 * `updateend` fires as usual while a fragment is away being transformed.
 *
 * Everything else (audio, WebM, other codecs) passes straight through. To get
 * H.264 from sites that prefer VP9/AV1, those are reported as unsupported.
 *
 * If the app's "indicator" setting is on, an "RTX VSR" mark is shown in the
 * top right corner of a video while the frames it shows are upscaled ones.
 */
(() => {
  "use strict";
  if (window.__vsr) return;
  const MS = window.MediaSource, SB = window.SourceBuffer;
  if (!MS || !SB) return;

  const TIMEOUT_MS = 30000;
  const MAX_FAILURES = 3;
  const stats = { transformed: 0, passthrough: 0, failures: 0, off: false, last: null, indicator: false };

  // ---- steer the page to H.264 -------------------------------------------
  const OTHER_CODECS = /vp8|vp9|vp09|av01|hev1|hvc1/i;
  const steer = (type) => /^video\//i.test(type || "") && OTHER_CODECS.test(type);
  const origSupported = MS.isTypeSupported;
  MS.isTypeSupported = function (type) {
    return steer(type) ? false : origSupported.call(this, type);
  };
  const origCanPlay = HTMLMediaElement.prototype.canPlayType;
  HTMLMediaElement.prototype.canPlayType = function (type) {
    return steer(type) ? "" : origCanPlay.call(this, type);
  };
  const mc = navigator.mediaCapabilities;
  if (mc && mc.decodingInfo) {
    const origInfo = mc.decodingInfo.bind(mc);
    mc.decodingInfo = (conf) =>
      conf && conf.video && steer(conf.video.contentType)
        ? Promise.resolve({ supported: false, smooth: false, powerEfficient: false })
        : origInfo(conf);
  }

  // ---- transport to the app (via the isolated-world content script) --------
  let nextId = 1;
  let markUrl = null;              // the indicator's picture, told by content.js
  const waiting = new Map();
  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (ev.source !== window || !m) return;
    if (m.__vsr === "cfgres") {
      if (m.config) stats.indicator = !!(m.config.indicator && m.config.enabled);
      if (typeof m.mark === "string") markUrl = m.mark;
      return;
    }
    if (m.__vsr !== "res") return;
    const w = waiting.get(m.id);
    if (!w) return;
    waiting.delete(m.id);
    clearTimeout(w.timer);
    w.resolve(m.ok ? { init: new Uint8Array(m.init), media: new Uint8Array(m.media), meta: m.meta }
                   : { failed: !m.passthrough, meta: m.meta });
  });

  function transform(bytes) {
    return new Promise((resolve) => {
      const id = nextId++;
      const timer = setTimeout(() => {
        waiting.delete(id);
        resolve({ failed: true });
      }, TIMEOUT_MS);
      waiting.set(id, { resolve, timer });
      window.postMessage({ __vsr: "req", id, screen: screenInfo(), body: bytes.buffer }, "*", [bytes.buffer]);
    });
  }

  // The screen this window is on, in CSS pixels: the app caps the output to
  // that display's resolution.
  const screenInfo = () => ({ w: screen.width, h: screen.height, dpr: window.devicePixelRatio || 1 });

  // ---- bytes helpers --------------------------------------------------------
  function toBytes(data) {   // always a private copy: the page may reuse its buffer
    if (ArrayBuffer.isView(data)) {
      return new Uint8Array(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength));
    }
    return new Uint8Array(data.slice(0));
  }
  function concat(parts) {
    let n = 0;
    for (const p of parts) n += p.length;
    const out = new Uint8Array(n);
    let o = 0;
    for (const p of parts) { out.set(p, o); o += p.length; }
    return out;
  }
  function sameBytes(a, b) {
    if (!a || !b || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
    return true;
  }
  /* Complete top-level box at `pos`, or null if its bytes haven't all arrived. */
  function boxAt(buf, pos) {
    if (pos + 8 > buf.length) return null;
    const dv = new DataView(buf.buffer, buf.byteOffset + pos);
    let size = dv.getUint32(0);
    if (size === 1) {
      if (pos + 16 > buf.length) return null;
      size = dv.getUint32(8) * 4294967296 + dv.getUint32(12);
    }
    if (size < 8) return { bad: true };
    if (pos + size > buf.length) return null;
    const type = String.fromCharCode(buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]);
    return { type, end: pos + size };
  }

  // ---- per-SourceBuffer state -----------------------------------------------
  const states = new WeakMap();
  const streams = new WeakMap();   // MediaSource -> states of its SourceBuffers
  const wanted = (type) => /^video\/mp4/i.test(type || "") && /avc[13]/i.test(type);

  function setType(sb, type) {
    let st = states.get(sb);
    if (!st) {
      st = { busy: false, gen: 0, sizes: new Set() };
      states.set(sb, st);
    }
    st.intercept = wanted(type);
    resetParser(st);
    st.init = null;        // the page's init segment (ftyp+moov)
    st.initParts = [];
    st.ours = null;        // app's init segment currently in effect, if any
    return st;
  }
  function resetParser(st) {
    st.acc = new Uint8Array(0);   // bytes not yet forming a complete box
    st.head = [];                 // styp/sidx/… in front of the next moof
    st.moof = null;
  }

  const origAdd = MS.prototype.addSourceBuffer;
  MS.prototype.addSourceBuffer = function (type) {
    const sb = origAdd.call(this, type);
    try {
      const st = setType(sb, type);
      if (!streams.has(this)) streams.set(this, []);
      streams.get(this).push(st);
    } catch (_) {}
    return sb;
  };
  const origChange = SB.prototype.changeType;
  if (origChange) {
    SB.prototype.changeType = function (type) {
      origChange.call(this, type);
      try { setType(this, type); } catch (_) {}
    };
  }

  /* Turn newly appended bytes into the bytes to really append (maybe none yet). */
  async function process(st, bytes) {
    const buf = st.acc.length ? concat([st.acc, bytes]) : bytes;
    const out = [];
    let pos = 0;
    for (;;) {
      const box = boxAt(buf, pos);
      if (!box) break;
      if (box.bad) {             // not ISO-BMFF after all: stop interfering
        st.intercept = false;
        out.push(buf.subarray(pos));
        pos = buf.length;
        break;
      }
      const data = buf.subarray(pos, box.end);
      pos = box.end;
      if (box.type === "ftyp" || box.type === "moov") {
        st.initParts.push(data);
        if (box.type === "moov") {
          st.init = concat(st.initParts);
          st.initParts = [];
          st.ours = null;
          out.push(st.init);
        }
      } else if (box.type === "moof") {
        st.moof = data;
      } else if (box.type === "mdat" && st.moof && st.init) {
        const fragment = concat([st.moof, data]);
        const original = concat([...st.head, fragment]);
        st.moof = null;
        st.head = [];
        const res = stats.off ? { failed: false } : await transform(concat([st.init, fragment]));
        if (res.media) {
          if (!sameBytes(st.ours, res.init)) {
            out.push(res.init);
            st.ours = res.init;
          }
          out.push(res.media);
          stats.transformed++;
          stats.failures = 0;
          stats.last = res.meta;
          if (res.meta) {
            // Frames of this size are upscaled ones (see the indicator).
            if (res.meta.out) st.sizes.add(res.meta.out);
            if ("indicator" in res.meta) stats.indicator = !!res.meta.indicator;
          }
          watchVideos();
        } else {
          if (res.failed && ++stats.failures >= MAX_FAILURES) {
            stats.off = true;
            console.warn("[vsr] the RTX VSR app keeps failing: passing video through until reload");
          }
          if (st.ours) {         // back to the page's own stream description
            out.push(st.init);
            st.ours = null;
          }
          out.push(original);
          // Originals of this size are in the buffer now: it no longer
          // tells upscaled frames apart (see the indicator).
          if (res.meta && res.meta.in) st.sizes.delete(res.meta.in);
          stats.passthrough++;
        }
      } else if (st.moof || !st.init) {
        out.push(data);          // unexpected order: don't hold anything back
      } else {
        st.head.push(data);      // styp / sidx / emsg … belong to the next fragment
      }
    }
    st.acc = buf.slice(pos);
    return concat(out);
  }

  // ---- the SourceBuffer facade ------------------------------------------------
  const nativeUpdating = Object.getOwnPropertyDescriptor(SB.prototype, "updating");
  const isBusy = (sb) => { const st = states.get(sb); return !!(st && st.busy); };
  const invalidState = (what) =>
    new DOMException(`Failed to execute '${what}' on 'SourceBuffer': This SourceBuffer is still processing an 'appendBuffer' or 'remove' operation.`, "InvalidStateError");
  const fire = (sb, names) => { for (const n of names) sb.dispatchEvent(new Event(n)); };

  Object.defineProperty(SB.prototype, "updating", {
    configurable: true, enumerable: true,
    get() { return isBusy(this) || nativeUpdating.get.call(this); },
  });

  const origAppend = SB.prototype.appendBuffer;

  /* The real append of what `process` produced. Upscaled video is several
   * times the bytes the player budgeted for, so the buffer can be full where
   * the original would have fitted: then the page keeps seeing `updating`
   * and the append is retried as playback frees space, which is the pace the
   * player would have settled on by itself. */
  function finishAppend(sb, st, gen, out) {
    if (st.gen !== gen) return;          // aborted meanwhile
    // The real append fires its own updatestart; the page already had ours.
    const swallow = (ev) => ev.stopImmediatePropagation();
    sb.addEventListener("updatestart", swallow, { capture: true, once: true });
    try {
      origAppend.call(sb, out);
      st.busy = false;
    } catch (e) {
      sb.removeEventListener("updatestart", swallow, { capture: true });
      if (e && e.name === "QuotaExceededError") {
        setTimeout(() => finishAppend(sb, st, gen, out), 1000);
        return;
      }
      st.busy = false;
      console.warn("[vsr] append failed:", e);
      fire(sb, ["error", "updateend"]);
    }
  }

  SB.prototype.appendBuffer = function (data) {
    const sb = this, st = states.get(sb);
    if (!st || !st.intercept) return origAppend.call(sb, data);
    if (sb.updating) throw invalidState("appendBuffer");
    const bytes = toBytes(data);
    const gen = st.gen;
    st.busy = true;
    queueMicrotask(() => { if (st.gen === gen) fire(sb, ["updatestart"]); });
    process(st, bytes).catch((e) => {
      console.warn("[vsr] passing through after error:", e);
      st.intercept = false;
      return bytes;
    }).then((out) => {
      if (st.gen !== gen) return;        // aborted meanwhile
      if (!out.length) {
        st.busy = false;
        return fire(sb, ["update", "updateend"]);
      }
      finishAppend(sb, st, gen, out);
    });
  };

  const origAbort = SB.prototype.abort;
  SB.prototype.abort = function () {
    const st = states.get(this);
    const wasBusy = isBusy(this);
    if (st) {
      st.gen++;
      st.busy = false;
      resetParser(st);
    }
    origAbort.call(this);
    if (wasBusy) fire(this, ["abort", "updateend"]);
  };

  const origRemove = SB.prototype.remove;
  SB.prototype.remove = function (start, end) {
    if (isBusy(this)) throw invalidState("remove");
    return origRemove.call(this, start, end);
  };

  for (const prop of ["timestampOffset", "appendWindowStart", "appendWindowEnd", "mode"]) {
    const d = Object.getOwnPropertyDescriptor(SB.prototype, prop);
    if (!d || !d.set) continue;
    Object.defineProperty(SB.prototype, prop, {
      configurable: true, enumerable: d.enumerable, get: d.get,
      set(v) {
        if (isBusy(this)) throw new DOMException(`Failed to set the '${prop}' property on 'SourceBuffer': This SourceBuffer is still processing an 'appendBuffer' or 'remove' operation.`, "InvalidStateError");
        d.set.call(this, v);
      },
    });
  }

  // ---- the "RTX VSR" indicator ---------------------------------------------------
  // A <video> plays a MediaSource through an object URL; remember which.
  const sources = new Map();       // object URL -> WeakRef(MediaSource)
  const origCreateURL = URL.createObjectURL;
  URL.createObjectURL = function (obj) {
    const url = origCreateURL.apply(this, arguments);
    try {
      if (obj instanceof MS) {
        for (const [u, ref] of sources) if (!ref.deref()) sources.delete(u);
        sources.set(url, new WeakRef(obj));
      }
    } catch (_) {}
    return url;
  };

  /* Is this element showing upscaled frames right now? The decoded size is
   * the proof: the app's output is larger than anything the page appended. */
  function upscaled(video) {
    if (!video.videoWidth || video.readyState < 2) return false;
    const ref = sources.get(video.currentSrc || video.src);
    const list = ref && ref.deref() && streams.get(ref.deref());
    const size = video.videoWidth + "x" + video.videoHeight;
    return !!list && list.some((st) => st.sizes.has(size));
  }

  const marks = new WeakMap();     // <video> -> its indicator element
  function makeMark() {
    // Styled through style properties only (pages may forbid inline style
    // attributes), and with everything a page's own rules for images could
    // add to it switched off.
    const el = document.createElement("img");
    el.setAttribute("data-rtx-vsr-indicator", "");
    el.setAttribute("aria-hidden", "true");
    el.alt = "";
    el.draggable = false;
    el.src = markUrl;
    Object.assign(el.style, {
      position: "absolute", zIndex: "2147483000", display: "none", pointerEvents: "none",
      opacity: "0.7", userSelect: "none", margin: "0", padding: "0", border: "0",
      borderRadius: "0", background: "none", boxShadow: "none", filter: "none",
      maxWidth: "none", maxHeight: "none", minWidth: "0", minHeight: "0",
      transform: "none", transition: "none",
    });
    return el;
  }

  function placeMark(video) {
    let el = marks.get(video);
    const host = video.offsetParent;   // null while the element is not laid out
    const show = stats.indicator && !!markUrl && !!host && video.clientWidth > 0 && upscaled(video);
    if (!show) {
      if (el) el.style.display = "none";
      return;
    }
    if (!el) marks.set(video, el = makeMark());
    if (el.parentNode !== host) host.appendChild(el);
    // The picture's box inside the element (letterboxed unless told otherwise).
    const ew = video.clientWidth, eh = video.clientHeight;
    let w = ew, h = eh;
    const fit = getComputedStyle(video).objectFit;
    if (fit !== "fill" && fit !== "cover") {
      const s = Math.min(ew / video.videoWidth, eh / video.videoHeight);
      w = video.videoWidth * s;
      h = video.videoHeight * s;
    }
    const side = Math.round(Math.max(26, Math.min(64, h * 0.075)));
    const gap = Math.round(side * 0.3);
    const left = video.offsetLeft + video.clientLeft + (ew - w) / 2 + w - side - gap;
    const top = video.offsetTop + video.clientTop + (eh - h) / 2 + gap;
    Object.assign(el.style, {
      display: "block", left: Math.round(left) + "px", top: Math.round(top) + "px",
      width: side + "px", height: side + "px",
    });
  }

  let watching = false, ticks = 0, wasPlaying = false;
  function watchVideos() {         // started by the first upscaled fragment
    if (watching) return;
    watching = true;
    setInterval(() => {
      let playing = false;
      for (const video of document.querySelectorAll("video")) {
        try {
          placeMark(video);
          playing = playing || (!document.hidden && !video.paused && !video.ended && upscaled(video));
        } catch (_) {}
      }
      // Ask for the app's settings now and then (they may change while
      // nothing is being appended), and tell it whether upscaled video is
      // playing here and on which screen: its window mentions a smaller
      // display only while something is being watched on one.
      if (ticks++ % 4 === 0 || playing !== wasPlaying) {
        window.postMessage({ __vsr: "cfg", playing, screen: screenInfo() }, "*");
      }
      wasPlaying = playing;
    }, 500);
  }

  window.__vsr = { status: () => Object.assign({}, stats) };
})();
