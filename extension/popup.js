/* RTX VSR Local Upscaler — toolbar popup.
 *
 * The config lives in the service worker (one copy of the defaults and of the
 * host matching), so this only asks it for the state and sends changes back.
 * Quality and factor apply to the next segment; the site switch needs the
 * page-world hook added or removed at document_start, so it reloads the tab.
 */
const $ = (id) => document.getElementById(id);
const send = (msg) => chrome.runtime.sendMessage(msg);

async function checkProxy(url) {
  try {
    const h = await (await fetch(url + "/health")).json();
    $("proxyState").textContent = h.ok
      ? `Proxy running on ${h.gpu} (${(h.free_mb / 1024).toFixed(1)} GB free).`
      : `Proxy is up but the GPU is not ready: ${h.err}`;
  } catch (_) {
    $("proxyState").textContent =
      `No proxy at ${url}. Start it with: python proxy/vsr_proxy.py`;
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
    el.textContent = "Not active in this tab yet — reload the page.";
  } else if (st.off) {
    el.textContent = "Switched itself off in this tab after repeated proxy failures — reload to retry.";
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
  $("mode").value = st.cfg.mode;
  $("scale").value = st.cfg.scale;
  checkProxy(st.cfg.proxy);

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

$("mode").addEventListener("change", () => send({ type: "set", mode: Number($("mode").value) }));
$("scale").addEventListener("change", () => {
  const scale = Number($("scale").value);
  if (scale >= 1.1 && scale <= 4) send({ type: "set", scale });
});
$("options").addEventListener("click", (ev) => {
  ev.preventDefault();
  chrome.runtime.openOptionsPage();
});

load();
