/* RTX VSR — service worker.
 *
 *  - keeps the content scripts registered for exactly the enabled sites
 *    (so the page-world hook is present from document_start, with no config
 *    race, and absent everywhere else),
 *  - holds the site list for the toolbar popup, which toggles the current
 *    tab's site,
 *  - relays segments to the RTX VSR app: requests from here are not subject
 *    to the page's CSP/CORS or local-network permission prompts.
 *
 * Quality, target resolution and the on-video indicator are settings of the
 * app, not of the extension; pages ask for them through "appConfig".
 */
const DEFAULTS = {
  proxy: "http://127.0.0.1:8730",
  sites: { "youtube.com": true },
};

async function getConfig() {
  const { vsrConfig } = await chrome.storage.local.get("vsrConfig");
  return Object.assign({}, DEFAULTS, vsrConfig);
}

const siteOf = (url) => {
  try {
    const u = new URL(url);
    return /^https?:$/.test(u.protocol) ? u.hostname.replace(/^www\./, "") : null;
  } catch (_) { return null; }
};
const enabledFor = (cfg, host) =>
  !!host && Object.entries(cfg.sites).some(([s, on]) => on && (host === s || host.endsWith("." + s)));

// Serialized: a toggle both writes storage (-> onChanged) and syncs directly,
// and two overlapping runs would register the same script ids twice.
let syncing = Promise.resolve();
function syncScripts() {
  syncing = syncing.then(doSync, doSync);
  return syncing;
}

async function doSync() {
  const cfg = await getConfig();
  const matches = Object.entries(cfg.sites).filter(([, on]) => on)
    .flatMap(([s]) => [`*://${s}/*`, `*://*.${s}/*`]);
  const ids = ["vsr-inject", "vsr-content"];
  const have = await chrome.scripting.getRegisteredContentScripts({ ids });
  if (have.length) await chrome.scripting.unregisterContentScripts({ ids: have.map((s) => s.id) });
  if (!matches.length) return;
  const common = { matches, runAt: "document_start", allFrames: true, persistAcrossSessions: true };
  await chrome.scripting.registerContentScripts([
    Object.assign({ id: "vsr-inject", js: ["inject.js"], world: "MAIN" }, common),
    Object.assign({ id: "vsr-content", js: ["content.js"] }, common),
  ]);
}

async function paint(tabId, url) {
  const on = enabledFor(await getConfig(), siteOf(url || ""));
  chrome.action.setBadgeText({ tabId, text: on ? "ON" : "" });
  chrome.action.setBadgeBackgroundColor({ tabId, color: "#76b900" });
}

chrome.runtime.onInstalled.addListener(syncScripts);
chrome.runtime.onStartup.addListener(syncScripts);
chrome.storage.onChanged.addListener((changes) => { if (changes.vsrConfig) syncScripts(); });
chrome.tabs.onUpdated.addListener((tabId, info, tab) => {
  if (info.status === "loading") paint(tabId, tab.url);
});

// The entry that covers this host, or the host itself if none does yet.
const siteKey = (cfg, host) =>
  Object.keys(cfg.sites).find((s) => host === s || host.endsWith("." + s)) || host;

// Serialized like syncScripts: two changes in quick succession would each
// read the old config and the second write would drop the first.
let writing = Promise.resolve();
function updateConfig(change) {
  const run = async () => {
    const cfg = await getConfig();
    change(cfg);
    await chrome.storage.local.set({ vsrConfig: cfg });
  };
  writing = writing.then(run, run);
  return writing;
}

// The screen a page is on, as the app's query parameters.
const screenQuery = (s) => (s && s.w && s.h ? `sw=${s.w}&sh=${s.h}&dpr=${s.dpr || 1}` : "");

// The app's own settings (GET /config), or null when it is not running. A
// page that has upscaled video playing says so along the way.
async function appConfig(msg) {
  const cfg = await getConfig();
  const where = msg.playing ? screenQuery(msg.screen) : "";
  try {
    const r = await fetch(cfg.proxy + "/config" + (where && "?playing=1&" + where), { cache: "no-store" });
    return r.ok ? await r.json() : null;
  } catch (_) { return null; }
}

const requests = {
  // from the toolbar popup (popup.js)
  async state(msg) {
    const cfg = await getConfig(), host = siteOf(msg.url || "");
    return { cfg, host, site: host && siteKey(cfg, host), enabled: enabledFor(cfg, host) };
  },
  async setSite(msg) {
    const host = siteOf(msg.url || "");
    if (!host) return;
    await updateConfig((cfg) => {
      cfg.sites = Object.assign({}, cfg.sites, { [siteKey(cfg, host)]: !!msg.on });
    });
    await syncScripts();
    chrome.tabs.reload(msg.tabId);  // hooks attach at document_start
  },
  // from pages (content.js)
  appConfig,
};

// The app hands an upscaled segment back in parts, so that the first frames
// can play while the last are still being made: the answer to a segment is
// its first part, and names the segment (X-VSR-Next) if more is to come.
async function transform(msg) {
  const cfg = await getConfig();
  // Where the player is: how much of the segment the playhead is past
  // already, and how much video it has buffered in front of the playhead.
  const where = [msg.behind > 0 ? "behind=" + (+msg.behind).toFixed(3) : "",
                 typeof msg.lead === "number" && msg.lead >= 0 ? "lead=" + msg.lead.toFixed(3) : ""];
  let r;
  if (msg.next) {
    const what = msg.cancel ? "cancel" : "next";
    const query = ["job=" + encodeURIComponent(msg.next), ...where].filter(Boolean).join("&");
    r = await fetch(`${cfg.proxy}/transform/${what}?${query}`, { method: "POST" });
    if (msg.cancel) return { ok: false };
  } else {
    const body = Uint8Array.fromBase64 ? Uint8Array.fromBase64(msg.body)
      : Uint8Array.from(atob(msg.body), (c) => c.charCodeAt(0));
    // The screen the page is on: the app never upscales past what it can
    // show. And the speed the video is played at, if it is not the usual.
    const query = [screenQuery(msg.screen), msg.parts ? "parts=1" : "", ...where,
                   msg.rate > 0 && msg.rate !== 1 ? "rate=" + (+msg.rate).toFixed(3) : ""]
      .filter(Boolean).join("&");
    r = await fetch(`${cfg.proxy}/transform${query && "?" + query}`, { method: "POST", body });
  }
  let meta = null;
  try { meta = JSON.parse(r.headers.get("X-VSR-Meta")); } catch (_) {}
  if (r.status === 204) return { ok: false, passthrough: true, meta };
  if (!r.ok) throw new Error("app answered HTTP " + r.status);
  const out = new Uint8Array(await r.arrayBuffer());
  // Whatever answers on that port must be the app: its reply to a segment
  // says where the init segment ends. Anything else is not something to
  // append.
  const initLen = Number(r.headers.get("X-VSR-Init-Length")) || 0;
  if (!(msg.next ? out.length > 8 : initLen > 0 && initLen < out.length)) {
    throw new Error("the answer is not from the RTX VSR app");
  }
  let b64;
  if (out.toBase64) b64 = out.toBase64();
  else {
    let str = "";
    for (let i = 0; i < out.length; i += 0x8000) str += String.fromCharCode.apply(null, out.subarray(i, i + 0x8000));
    b64 = btoa(str);
  }
  return { ok: true, body: b64, initLen, meta, next: r.headers.get("X-VSR-Next") || null };
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg && Object.hasOwn(requests, msg.type)) {
    requests[msg.type](msg).then(sendResponse, (e) => {
      console.warn("[vsr] " + msg.type + " failed:", e.message);
      sendResponse(null);
    });
    return true;
  }
  if (!msg || msg.type !== "transform") return;
  transform(msg).then(sendResponse, (e) => {
    console.warn("[vsr] transform failed:", e.message);
    sendResponse({ ok: false });
  });
  return true;                      // async response
});
