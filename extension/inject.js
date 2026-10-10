/* RTX VSR Local Upscaler — page-world hook.
 *
 * Registered by background.js only on enabled sites, in the MAIN world at
 * document_start, so it is in place before the player touches MSE.
 *
 * What it does to a `video/mp4; codecs="avc1…"` SourceBuffer:
 *   - collects the appended bytes into complete top-level boxes,
 *   - keeps the stream's init segment (ftyp+moov),
 *   - sends init + each complete fragment (moof+mdat) to the local proxy via
 *     content.js -> background.js, and appends the upscaled H.264 fragment the
 *     proxy returns (preceded by the proxy's own init segment when it changes),
 *   - on any failure appends the original bytes, so playback never breaks.
 *
 * The page keeps seeing a normal SourceBuffer: `updating` is true and
 * `updateend` fires as usual while a fragment is away being transformed.
 *
 * Everything else (audio, WebM, other codecs) passes straight through. To get
 * H.264 from sites that prefer VP9/AV1, those are reported as unsupported.
 */
(() => {
  "use strict";
  if (window.__vsr) return;
  const MS = window.MediaSource, SB = window.SourceBuffer;
  if (!MS || !SB) return;

  const TIMEOUT_MS = 30000;
  const MAX_FAILURES = 3;
  const stats = { transformed: 0, passthrough: 0, failures: 0, off: false, last: null };

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

  // ---- transport to the proxy (via the isolated-world content script) ------
  let nextId = 1;
  const waiting = new Map();
  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (ev.source !== window || !m || m.__vsr !== "res") return;
    const w = waiting.get(m.id);
    if (!w) return;
    waiting.delete(m.id);
    clearTimeout(w.timer);
    w.resolve(m.ok ? { init: new Uint8Array(m.init), media: new Uint8Array(m.media), meta: m.meta }
                   : { failed: !m.passthrough });
  });

  function transform(bytes) {
    return new Promise((resolve) => {
      const id = nextId++;
      const timer = setTimeout(() => {
        waiting.delete(id);
        resolve({ failed: true });
      }, TIMEOUT_MS);
      waiting.set(id, { resolve, timer });
      const maxh = Math.round(screen.height * (window.devicePixelRatio || 1));
      window.postMessage({ __vsr: "req", id, maxh, body: bytes.buffer }, "*", [bytes.buffer]);
    });
  }

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
  const wanted = (type) => /^video\/mp4/i.test(type || "") && /avc[13]/i.test(type);

  function setType(sb, type) {
    let st = states.get(sb);
    if (!st) {
      st = { busy: false, gen: 0 };
      states.set(sb, st);
    }
    st.intercept = wanted(type);
    resetParser(st);
    st.init = null;        // the page's init segment (ftyp+moov)
    st.initParts = [];
    st.ours = null;        // proxy init segment currently in effect, if any
  }
  function resetParser(st) {
    st.acc = new Uint8Array(0);   // bytes not yet forming a complete box
    st.head = [];                 // styp/sidx/… in front of the next moof
    st.moof = null;
  }

  const origAdd = MS.prototype.addSourceBuffer;
  MS.prototype.addSourceBuffer = function (type) {
    const sb = origAdd.call(this, type);
    try { setType(sb, type); } catch (_) {}
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
        } else {
          if (res.failed && ++stats.failures >= MAX_FAILURES) {
            stats.off = true;
            console.warn("[vsr] proxy keeps failing — passing video through until reload");
          }
          if (st.ours) {         // back to the page's own stream description
            out.push(st.init);
            st.ours = null;
          }
          out.push(original);
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
      st.busy = false;
      if (!out.length) return fire(sb, ["update", "updateend"]);
      // The real append fires its own updatestart; the page already had ours.
      const swallow = (ev) => ev.stopImmediatePropagation();
      sb.addEventListener("updatestart", swallow, { capture: true, once: true });
      try {
        origAppend.call(sb, out);
      } catch (e) {
        sb.removeEventListener("updatestart", swallow, { capture: true });
        console.warn("[vsr] append failed:", e);
        fire(sb, ["error", "updateend"]);
      }
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

  window.__vsr = { status: () => Object.assign({}, stats) };
})();
