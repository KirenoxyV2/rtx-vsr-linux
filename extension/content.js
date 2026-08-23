/* RTX VSR Local Upscaler — content script (runs at document_start, all frames).
 *
 * Strategy: patch SourceBuffer.prototype.appendBuffer BEFORE any page script
 * runs. Video segments flow through appendBuffer regardless of how the page
 * fetched them (fetch API, MSE pipeline, Server ABR) — so this hook is immune
 * to YouTube protocol changes. For each video segment we POST the raw bytes to
 * the local proxy's /transform endpoint and re-append the returned H.264.
 * Audio SourceBuffers pass through untouched. If the proxy is down or errors,
 * we append the ORIGINAL bytes so playback never breaks (circuit breaker).
 */
(() => {
  "use strict";

  // ---- config (fast in-memory; background keeps it fresh) -----------------
  const DEFAULTS = {
    proxy: "http://127.0.0.1:8730",
    mode: 2,          // 1 LOW 2 MEDIUM 3 HIGH 4 ULTRA (proxy may downshift on VRAM)
    scale: 1.5,
    sites: { "youtube.com": true },  // safe default: YouTube only
  };
  let cfg = Object.assign({}, DEFAULTS);

  if (typeof chrome !== "undefined" && chrome.storage && chrome.storage.local) {
    try {
      chrome.storage.local.get("vsrConfig", (res) => {
        if (res && res.vsrConfig) cfg = Object.assign({}, DEFAULTS, res.vsrConfig);
      });
    } catch (_) {}
  }

  // ---- circuit breaker -----------------------------------------------------
  let failures = 0;
  const MAX_FAILURES = 3;
  let disabledUntilReload = false;

  function siteEnabled() {
    if (disabledUntilReload) return false;
    try {
      const host = location.hostname;
      for (const [site, on] of Object.entries(cfg.sites || {})) {
        if (host === site || host.endsWith("." + site)) return !!on;
      }
      return false;
    } catch (_) { return false; }
  }

  // ---- per-SourceBuffer ordering chain (segments must append in order) -----
  const chains = new WeakMap();

  function transform(buf, sb) {
    const url = `${cfg.proxy}/transform?mode=${cfg.mode}&scale=${cfg.scale}`;
    return fetch(url, { method: "POST", body: buf })
      .then((r) => {
        if (!r.ok) throw new Error("proxy HTTP " + r.status);
        failures = 0;
        return r.arrayBuffer();
      })
      .then((out) => out) // transformed bytes
      .catch((e) => {
        failures++;
        if (failures >= MAX_FAILURES) disabledUntilReload = true;
        console.warn("[vsr] transform failed, passing through:", e.message);
        return buf; // original bytes — never break playback
      });
  }

  const SB = window.SourceBuffer;
  if (!SB || !SB.prototype) return;

  const origAppend = SB.prototype.appendBuffer;
  SB.prototype.appendBuffer = function (data) {
    const sb = this;
    // Only touch video tracks; audio + unknown pass straight through.
    let isVideo = false;
    try { isVideo = String(sb.type || "").startsWith("video/"); } catch (_) {}
    if (!isVideo || !siteEnabled()) return origAppend.call(sb, data);

    // Serialize per-SourceBuffer so completion order == submission order.
    const prev = chains.get(sb) || Promise.resolve();
    const step = prev.then(() => dataToBuffer(data)).then((buf) => transform(buf, sb));
    chains.set(sb, step.catch(() => {}));
    // NOTE: we deliberately do NOT call origAppend now. The real append happens
    // when the transform resolves (below), which is what fires updatestart/
    // updateend for the player's queue. Until then the player simply waits —
    // the same wait it has for any slow segment, absorbed by its buffer.
    step.then((out) => {
      try { origAppend.call(sb, out); }
      catch (e) { console.warn("[vsr] re-append failed:", e.message); }
    });
  };

  function dataToBuffer(data) {
    if (data instanceof ArrayBuffer || (typeof SharedArrayBuffer !== "undefined" && data instanceof SharedArrayBuffer)) {
      return Promise.resolve(data);
    }
    const blob = (data instanceof Blob) ? data : new Blob([data]);
    return blob.arrayBuffer();
  }

  // ---- expose a tiny debug handle ------------------------------------------
  window.__vsr = {
    status: () => ({ enabled: siteEnabled(), failures, disabled: disabledUntilReload, cfg }),
  };
})();
