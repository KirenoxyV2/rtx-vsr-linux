/* RTX VSR Local Upscaler — isolated-world bridge.
 *
 * inject.js lives in the page's world and cannot use extension APIs; the
 * page's own network rules (CSP, CORS, local-network prompts) would also apply
 * to a fetch made from there. So it hands each segment to this script with
 * window.postMessage, and this script passes it on to the service worker,
 * which talks to the proxy. Extension messages are JSON, hence base64.
 */
(() => {
  "use strict";

  const toB64 = (bytes) => {
    if (bytes.toBase64) return bytes.toBase64();
    let s = "";
    for (let i = 0; i < bytes.length; i += 0x8000) {
      s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return btoa(s);
  };
  const fromB64 = (b64) => {
    if (Uint8Array.fromBase64) return Uint8Array.fromBase64(b64);
    const s = atob(b64);
    const out = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
    return out;
  };

  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (ev.source !== window || !m || m.__vsr !== "req") return;
    const reply = (res, transfer) =>
      window.postMessage(Object.assign({ __vsr: "res", id: m.id }, res), "*", transfer || []);
    try {
      chrome.runtime.sendMessage(
        { type: "transform", maxh: m.maxh, body: toB64(new Uint8Array(m.body)) },
        (res) => {
          if (chrome.runtime.lastError || !res || !res.ok) {
            return reply({ ok: false, passthrough: !!(res && res.passthrough) });
          }
          const all = fromB64(res.body);
          const init = all.slice(0, res.initLen), media = all.slice(res.initLen);
          reply({ ok: true, init: init.buffer, media: media.buffer, meta: res.meta },
                [init.buffer, media.buffer]);
        });
    } catch (_) {   // extension reloaded under a live page
      reply({ ok: false });
    }
  });
})();
