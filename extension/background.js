/* RTX VSR Local Upscaler — service worker.
 *
 *  - keeps the content scripts registered for exactly the enabled sites
 *    (so the page-world hook is present from document_start, with no config
 *    race, and absent everywhere else),
 *  - toolbar button toggles the current tab's site,
 *  - relays segments to the local proxy: requests from here are not subject
 *    to the page's CSP/CORS or local-network permission prompts.
 */
const DEFAULTS = {
  proxy: "http://127.0.0.1:8730",
  mode: 2,                          // 1 LOW  2 MEDIUM  3 HIGH  4 ULTRA
  scale: 2,
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

chrome.action.onClicked.addListener(async (tab) => {
  const host = siteOf(tab.url || "");
  if (!host) return;
  const cfg = await getConfig();
  // Toggle the entry that covers this host, or add the host itself.
  const key = Object.keys(cfg.sites).find((s) => host === s || host.endsWith("." + s)) || host;
  cfg.sites = Object.assign({}, cfg.sites, { [key]: !cfg.sites[key] });
  await chrome.storage.local.set({ vsrConfig: cfg });
  await syncScripts();
  chrome.tabs.reload(tab.id);       // hooks attach at document_start
});

async function transform(msg) {
  const cfg = await getConfig();
  const body = Uint8Array.fromBase64 ? Uint8Array.fromBase64(msg.body)
    : Uint8Array.from(atob(msg.body), (c) => c.charCodeAt(0));
  const url = `${cfg.proxy}/transform?mse=1&mode=${cfg.mode}&scale=${cfg.scale}` +
              (msg.maxh ? `&maxh=${msg.maxh}` : "");
  const r = await fetch(url, { method: "POST", body });
  if (r.status === 204) return { ok: false, passthrough: true };
  if (!r.ok) throw new Error("proxy HTTP " + r.status);
  const out = new Uint8Array(await r.arrayBuffer());
  let b64;
  if (out.toBase64) b64 = out.toBase64();
  else {
    let s = "";
    for (let i = 0; i < out.length; i += 0x8000) s += String.fromCharCode.apply(null, out.subarray(i, i + 0x8000));
    b64 = btoa(s);
  }
  let meta = null;
  try { meta = JSON.parse(r.headers.get("X-VSR-Meta")); } catch (_) {}
  return { ok: true, body: b64, initLen: Number(r.headers.get("X-VSR-Init-Length")), meta };
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (!msg || msg.type !== "transform") return;
  transform(msg).then(sendResponse, (e) => {
    console.warn("[vsr] transform failed:", e.message);
    sendResponse({ ok: false });
  });
  return true;                      // async response
});
