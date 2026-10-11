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
 * top right corner of a video for as long as upscaled frames are playing in
 * it: it fades out when the video pauses, ends or waits for data, when it
 * comes to frames that were not upscaled, and when the app is switched off
 * or gone.
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
      // No answer: the app is not running, so nothing is being upscaled.
      stats.indicator = !!(m.config && m.config.indicator && m.config.enabled);
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

  // ---- when a fragment plays ---------------------------------------------------
  const u32 = (b, p) => ((b[p] << 24) | (b[p + 1] << 16) | (b[p + 2] << 8) | b[p + 3]) >>> 0;
  const u64 = (b, p) => u32(b, p) * 4294967296 + u32(b, p + 4);
  /* The boxes in buf[pos, end). */
  function boxesIn(buf, pos, end) {
    const out = [];
    while (pos + 8 <= end) {
      let size = u32(buf, pos), body = pos + 8;
      if (size === 1) { size = u64(buf, pos + 8); body = pos + 16; }
      if (size < body - pos || pos + size > end) break;
      out.push({ type: String.fromCharCode(buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]),
                 body, end: pos + size });
      pos += size;
    }
    return out;
  }
  const boxIn = (buf, parent, type) => boxesIn(buf, parent.body, parent.end).find((b) => b.type === type);

  /* What an init segment says about the times of its video track's fragments,
   * or null if it has no such track. */
  function clockOf(init) {
    const moov = boxesIn(init, 0, init.length).find((b) => b.type === "moov");
    for (const trak of boxesIn(init, moov.body, moov.end)) {
      const mdia = trak.type === "trak" && boxIn(init, trak, "mdia");
      const hdlr = mdia && boxIn(init, mdia, "hdlr");
      if (!hdlr || u32(init, hdlr.body + 8) !== 0x76696465) continue;   // "vide"
      const tkhd = boxIn(init, trak, "tkhd"), mdhd = boxIn(init, mdia, "mdhd");
      const clock = {
        track: u32(init, tkhd.body + (init[tkhd.body] === 1 ? 20 : 12)),
        scale: u32(init, mdhd.body + (init[mdhd.body] === 1 ? 20 : 12)),
        duration: 0,     // of a sample, unless the fragment says
        edit: 0,         // subtracted from every presentation time
      };
      const edts = boxIn(init, trak, "edts"), elst = edts && boxIn(init, edts, "elst");
      if (elst && u32(init, elst.body + 4) > 0) {
        const long = init[elst.body] === 1;
        const negative = init[elst.body + (long ? 16 : 12)] & 0x80;   // an empty edit
        if (!negative) clock.edit = long ? u64(init, elst.body + 16) : u32(init, elst.body + 12);
      }
      const mvex = boxIn(init, moov, "mvex");
      for (const trex of mvex ? boxesIn(init, mvex.body, mvex.end) : []) {
        if (trex.type === "trex" && u32(init, trex.body + 4) === clock.track) {
          clock.duration = u32(init, trex.body + 12);
        }
      }
      return clock.scale ? clock : null;
    }
    return null;
  }

  /* The stretch of the video's timeline, in seconds, that the samples of a
   * moof box are presented in. */
  function spanOf(clock, buf) {
    let from = Infinity, to = -Infinity;
    const moof = boxesIn(buf, 0, buf.length)[0];
    for (const traf of boxesIn(buf, moof.body, moof.end)) {
      const tfhd = traf.type === "traf" && boxIn(buf, traf, "tfhd");
      const tfdt = tfhd && boxIn(buf, traf, "tfdt");
      if (!tfdt || u32(buf, tfhd.body + 4) !== clock.track) continue;
      const has = u32(buf, tfhd.body) & 0xffffff;
      const fixed = has & 0x8 ? u32(buf, tfhd.body + 8 + (has & 0x1 ? 8 : 0) + (has & 0x2 ? 4 : 0))
                              : clock.duration;
      let dts = buf[tfdt.body] === 1 ? u64(buf, tfdt.body + 4) : u32(buf, tfdt.body + 4);
      for (const trun of boxesIn(buf, traf.body, traf.end)) {
        if (trun.type !== "trun") continue;
        const flags = u32(buf, trun.body) & 0xffffff, count = u32(buf, trun.body + 4);
        // Per sample, each if flagged: duration, size, flags, composition offset.
        const step = 4 * [0x100, 0x200, 0x400, 0x800].filter((f) => flags & f).length;
        let p = trun.body + 8 + (flags & 0x1 ? 4 : 0) + (flags & 0x4 ? 4 : 0);
        if (p + count * step > trun.end) return null;
        for (let i = 0; i < count; i++, p += step) {
          const duration = flags & 0x100 ? u32(buf, p) : fixed;
          let shift = flags & 0x800 ? u32(buf, p + step - 4) : 0;
          if (buf[trun.body] !== 0) shift |= 0;      // signed from version 1 on
          const pts = dts + shift - clock.edit;
          from = Math.min(from, pts);
          to = Math.max(to, pts + duration);
          dts += duration;
        }
      }
    }
    return to > from ? [from / clock.scale, to / clock.scale] : null;
  }

  // ---- per-SourceBuffer state -----------------------------------------------
  const states = new WeakMap();
  const streams = new WeakMap();   // MediaSource -> states of its SourceBuffers
  const wanted = (type) => /^video\/mp4/i.test(type || "") && /avc[13]/i.test(type);

  function setType(sb, type) {
    let st = states.get(sb);
    if (!st) {
      // `sizes` and `spans` are what the indicator goes by: the sizes of the
      // upscaled video appended here, and the stretches of the timeline it
      // was appended for.
      st = { busy: false, gen: 0, sizes: new Set(), spans: [] };
      states.set(sb, st);
    }
    st.intercept = wanted(type);
    if (!st.intercept) forget(st);
    resetParser(st);
    st.init = null;        // the page's init segment (ftyp+moov)
    st.clock = null;       // its timing, for `spanOf`
    st.initParts = [];
    st.ours = null;        // app's init segment currently in effect, if any
    return st;
  }
  /* From here on appends are not looked at, so what is in the buffer is not
   * known any more. */
  function forget(st) {
    st.sizes.clear();
    st.spans = [];
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

  /* Where on the timeline the fragment waiting in `st.moof` goes, or null if
   * that cannot be told. */
  function playsAt(sb, st) {
    try {
      if (sb.mode !== "segments") return null;    // "sequence": wherever the last one ended
      const span = st.clock && spanOf(st.clock, st.moof);
      return span && span.map((t) => t + sb.timestampOffset);
    } catch (_) { return null; }
  }

  /* Takes note of fragments that have been appended: { span, out, src }, with
   * `out` the size of an upscaled one and `src` that of an original. */
  function remember(st, notes) {
    for (const { span, out, src } of notes) {
      if (out) st.sizes.add(out);
      if (!span) st.spans = null;         // times unknown: sizes alone decide from now on
      if (!st.spans) {
        // Originals of this size are in the buffer now: it no longer tells
        // upscaled frames apart.
        if (!out && src) st.sizes.delete(src);
        continue;
      }
      // The fragment replaces whatever was there for its stretch of time.
      const [a, b] = span, spans = [];
      for (const [s, e] of st.spans) {
        if (s < a) spans.push([s, Math.min(e, a)]);
        if (e > b) spans.push([Math.max(s, b), e]);
      }
      if (out) spans.push([a, b]);
      spans.sort((x, y) => x[0] - y[0]);
      st.spans = [];
      for (const span of spans) {
        const last = st.spans[st.spans.length - 1];
        if (last && span[0] <= last[1] + 0.001) last[1] = Math.max(last[1], span[1]);
        else st.spans.push(span);
      }
    }
  }

  /* Turn newly appended bytes into the bytes to really append (maybe none
   * yet), with notes on the fragments among them (see `remember`). */
  async function process(sb, st, bytes) {
    const buf = st.acc.length ? concat([st.acc, bytes]) : bytes;
    const out = [], notes = [];
    let pos = 0;
    for (;;) {
      const box = boxAt(buf, pos);
      if (!box) break;
      if (box.bad) {             // not ISO-BMFF after all: stop interfering
        st.intercept = false;
        forget(st);
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
          try { st.clock = clockOf(st.init); } catch (_) { st.clock = null; }
          out.push(st.init);
        }
      } else if (box.type === "moof") {
        st.moof = data;
      } else if (box.type === "mdat" && st.moof && st.init) {
        const fragment = concat([st.moof, data]);
        const original = concat([...st.head, fragment]);
        const span = playsAt(sb, st);
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
          // Frames of this size, at these times, are upscaled ones.
          notes.push({ span, out: res.meta && res.meta.out });
          if (res.meta && "indicator" in res.meta) stats.indicator = !!res.meta.indicator;
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
          notes.push({ span, src: res.meta && res.meta.in });
          stats.passthrough++;
        }
      } else if (st.moof || !st.init) {
        out.push(data);          // unexpected order: don't hold anything back
      } else {
        st.head.push(data);      // styp / sidx / emsg … belong to the next fragment
      }
    }
    st.acc = buf.slice(pos);
    return { bytes: concat(out), notes };
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
  function finishAppend(sb, st, gen, out, notes) {
    if (st.gen !== gen) return;          // aborted meanwhile
    // The real append fires its own updatestart; the page already had ours.
    const swallow = (ev) => ev.stopImmediatePropagation();
    sb.addEventListener("updatestart", swallow, { capture: true, once: true });
    try {
      origAppend.call(sb, out);
      st.busy = false;
      remember(st, notes);
    } catch (e) {
      sb.removeEventListener("updatestart", swallow, { capture: true });
      if (e && e.name === "QuotaExceededError") {
        setTimeout(() => finishAppend(sb, st, gen, out, notes), 1000);
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
    process(sb, st, bytes).catch((e) => {
      console.warn("[vsr] passing through after error:", e);
      st.intercept = false;
      forget(st);
      return { bytes, notes: [] };
    }).then((out) => {
      if (st.gen !== gen) return;        // aborted meanwhile
      if (!out.bytes.length) {
        st.busy = false;
        return fire(sb, ["update", "updateend"]);
      }
      finishAppend(sb, st, gen, out.bytes, out.notes);
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
   * the proof: the app's output is larger than what the page appended for it.
   * A larger original can be of that size too, so the frames must also be
   * from a stretch of the timeline that upscaled video was appended for. */
  function upscaled(video) {
    if (!video.videoWidth || video.readyState < 2) return false;
    const ref = sources.get(video.currentSrc || video.src);
    const list = ref && ref.deref() && streams.get(ref.deref());
    const size = video.videoWidth + "x" + video.videoHeight, t = video.currentTime;
    return !!list && list.some((st) => st.sizes.has(size)
      && (!st.spans || st.spans.some(([from, to]) => t > from - 0.05 && t < to + 0.05)));
  }
  /* …and are they moving? Not while it is paused, at its end or waiting for
   * data (which is also what a seek looks like until it lands). */
  const live = (video) => !video.paused && !video.ended && video.readyState > 2 && upscaled(video);

  const FADE_MS = 250;
  const marks = new Map();         // <video> -> its indicator element
  function makeMark() {
    // Styled through style properties only (pages may forbid inline style
    // attributes), and with everything a page's own rules for images could
    // add to it switched off. It is in the page from its first appearance
    // on, shown and hidden by opacity so that both can be gradual;
    // `visibility` follows at the end of a fade out and the start of a
    // fade in.
    const el = document.createElement("img");
    el.setAttribute("data-rtx-vsr-indicator", "");
    el.setAttribute("aria-hidden", "true");
    el.alt = "";
    el.draggable = false;
    el.src = markUrl;
    Object.assign(el.style, {
      position: "absolute", zIndex: "2147483000", display: "block", pointerEvents: "none",
      opacity: "0", visibility: "hidden", userSelect: "none", margin: "0", padding: "0", border: "0",
      borderRadius: "0", background: "none", boxShadow: "none", filter: "none",
      maxWidth: "none", maxHeight: "none", minWidth: "0", minHeight: "0",
      transform: "none", animation: "none",
      transition: `opacity ${FADE_MS}ms ease, visibility ${FADE_MS}ms`,
    });
    return el;
  }
  function fade(el, shown) {
    el.style.opacity = shown ? "0.7" : "0";
    el.style.visibility = shown ? "visible" : "hidden";
  }

  function placeMark(video) {
    let el = marks.get(video);
    const host = video.offsetParent;   // null while the element is not laid out
    const show = stats.indicator && !!markUrl && !!host && video.clientWidth > 0 && live(video);
    if (!show) {
      if (el) fade(el, false);         // where it is
      return;
    }
    if (!el) marks.set(video, el = makeMark());
    const moved = el.parentNode !== host;
    if (moved) host.appendChild(el);
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
      left: Math.round(left) + "px", top: Math.round(top) + "px",
      width: side + "px", height: side + "px",
    });
    // A fade starts from what the browser last worked out for the element:
    // for one that has just been put in the page, that has to be asked for.
    if (moved) void el.offsetWidth;
    fade(el, true);
  }
  const tryPlace = (video) => { try { placeMark(video); } catch (_) {} };

  let watching = false, ticks = 0, wasPlaying = false;
  function watchVideos() {         // started by the first upscaled fragment
    if (watching) return;
    watching = true;
    // A video says when it stops and starts (these events do not bubble, but
    // they can be caught on their way down); the timer sees the rest, such
    // as playback coming to frames that are not upscaled.
    for (const name of ["playing", "pause", "waiting", "seeking", "ended", "emptied", "resize"]) {
      document.addEventListener(name, (ev) => {
        if (ev.target instanceof HTMLVideoElement) tryPlace(ev.target);
      }, true);
    }
    setInterval(() => {
      for (const [video, el] of marks) {
        if (video.isConnected) continue;
        el.remove();               // its video has left the page
        marks.delete(video);
      }
      let playing = false;
      for (const video of document.querySelectorAll("video")) {
        tryPlace(video);
        try { playing = playing || (!document.hidden && live(video)); } catch (_) {}
      }
      // Ask for the app's settings every second (they may change, and the
      // app may go, while nothing is being appended), and tell it whether
      // upscaled video is playing here and on which screen: its window
      // mentions a smaller display only while something is being watched
      // on one.
      if (ticks++ % 4 === 0 || playing !== wasPlaying) {
        window.postMessage({ __vsr: "cfg", playing, screen: screenInfo() }, "*");
      }
      wasPlaying = playing;
    }, 250);
  }

  window.__vsr = { status: () => Object.assign({}, stats) };
})();
