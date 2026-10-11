/* RTX VSR — isolated-world bridge.
 *
 * inject.js lives in the page's world and cannot use extension APIs; the
 * page's own network rules (CSP, CORS, local-network prompts) would also apply
 * to a fetch made from there. So it hands each segment to this script with
 * window.postMessage, and this script passes it on to the service worker,
 * which talks to the app. Extension messages are JSON, hence base64.
 *
 * The app's answer comes in parts: the first with the segment's request, each
 * further one when the hook asks for it by the name the part before gave it.
 */
(() => {
  "use strict";

  // Two copies of the extension (say, one loaded by hand and one loaded by
  // the app) would each relay every segment: only the first bridge answers.
  const root = document.documentElement;
  if (root) {
    if (root.hasAttribute("data-rtx-vsr-bridge")) return;
    root.setAttribute("data-rtx-vsr-bridge", "");
  }

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
    if (ev.source !== window || !m) return;
    if (m.__vsr === "cfg") {
      // The app's settings and the picture, for the on-video indicator.
      try {
        chrome.runtime.sendMessage({ type: "appConfig", playing: !!m.playing, screen: m.screen }, (res) => {
          void chrome.runtime.lastError;
          window.postMessage({ __vsr: "cfgres", config: res || null,
                               mark: chrome.runtime.getURL("icons/indicator.png") }, "*");
        });
      } catch (_) {}
      return;
    }
    if (m.__vsr !== "req") return;
    const reply = (res, transfer) =>
      window.postMessage(Object.assign({ __vsr: "res", id: m.id }, res), "*", transfer || []);
    // Either a segment, or the name of one that is on its way back: for its
    // next part, or to say that the rest is not wanted.
    // `behind`, `lead` and `rate` say what the player is doing (inject.js).
    const msg = m.next ? { type: "transform", next: m.next, cancel: !!m.cancel, behind: m.behind, lead: m.lead }
                       : { type: "transform", screen: m.screen, parts: !!m.parts,
                           behind: m.behind, lead: m.lead, rate: m.rate,
                           body: toB64(new Uint8Array(m.body)) };
    try {
      chrome.runtime.sendMessage(msg, (res) => {
        if (chrome.runtime.lastError || !res || !res.ok) {
          return reply({ ok: false, passthrough: !!(res && res.passthrough), meta: res && res.meta });
        }
        const all = fromB64(res.body);
        const init = all.slice(0, res.initLen || 0), media = all.slice(res.initLen || 0);
        reply({ ok: true, init: init.buffer, media: media.buffer, meta: res.meta, next: res.next || null },
              [init.buffer, media.buffer]);
      });
    } catch (_) {   // extension reloaded under a live page
      reply({ ok: false });
    }
  });
})();
