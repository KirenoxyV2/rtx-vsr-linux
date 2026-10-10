/* RTX VSR — toolbar popup.
 *
 * The site list lives in the service worker (one copy of the defaults and of
 * the host matching), so this only asks it for the state and sends changes
 * back. The site switch needs the page-world hook added or removed at
 * document_start, so it reloads the tab. Everything about how video is
 * upscaled is a setting of the app; the popup only reports it.
 */
const $ = (id) => document.getElementById(id);
const send = (msg) => chrome.runtime.sendMessage(msg);
const MODES = ["", "Low", "Medium", "High", "Ultra"];

async function showApp(url) {
  const el = $("appState");
  try {
    const [h, c] = await Promise.all(["/health", "/config"].map(
      async (path) => (await fetch(url + path, { cache: "no-store" })).json()));
    el.className = "box " + (h.ok ? "ok" : "bad");
    if (!h.ok) el.textContent = `The RTX VSR app is running, but: ${h.err}`;
    else if (!c.enabled) el.textContent = "Upscaling is switched off in the RTX VSR app.";
    else el.textContent = `Upscaling to ${c.target[0]}×${c.target[1]}, ${MODES[c.quality]} quality, on ${h.gpu}.`;
  } catch (_) {
    el.className = "box bad";
    el.textContent = "The RTX VSR app is not running. Start it to upscale video; until then video plays as usual.";
  }
}

// What the hook in this tab has done so far (inject.js: window.__vsr).
async function showPageState(tabId) {
  let st = null;
  try {
    const [r] = await chrome.scripting.executeScript({
      target: { tabId }, world: "MAIN",
      func: () => (window.__vsr ? window.__vsr.status() : null),
    });
    st = r && r.result;
  } catch (_) {}                    // a page extensions cannot script
  const el = $("pageState");
  el.hidden = false;
  if (!st) {
    el.textContent = "Not active in this tab yet. Reload the page.";
  } else if (st.off) {
    el.textContent = "Switched itself off in this tab after repeated failures. Reload to retry.";
  } else if (!st.transformed) {
    el.textContent = st.passthrough
      ? `Nothing upscaled yet (${st.passthrough} segments passed through).`
      : "Waiting for video…";
  } else {
    const last = st.last && st.last.in && st.last.out ? `, last ${st.last.in} → ${st.last.out}` : "";
    el.textContent = `${st.transformed} segments upscaled${last}.`;
  }
}

async function load() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const st = await send({ type: "state", url: tab && tab.url });
  showApp(st.cfg.proxy);

  if (!st.host) {
    $("site").textContent = "This page";
    $("siteNote").textContent = "Only http(s) pages can be upscaled.";
    $("enabled").disabled = true;
  } else {
    $("site").textContent = st.site;
    $("site").title = st.site;
    $("enabled").checked = st.enabled;
    $("siteNote").textContent = "Switching reloads the tab.";
    if (st.enabled) showPageState(tab.id);
  }

  $("enabled").addEventListener("change", async () => {
    $("enabled").disabled = true;
    await send({ type: "setSite", tabId: tab.id, url: tab.url, on: $("enabled").checked });
    window.close();
  });
}

$("options").addEventListener("click", (ev) => {
  ev.preventDefault();
  chrome.runtime.openOptionsPage();
});

load();
