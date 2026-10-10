const DEFAULTS = {
  proxy: "http://127.0.0.1:8730",
  mode: 2,
  scale: 2,
  sites: { "youtube.com": true },
};
const $ = (id) => document.getElementById(id);

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

async function load() {
  const { vsrConfig } = await chrome.storage.local.get("vsrConfig");
  const cfg = Object.assign({}, DEFAULTS, vsrConfig);
  $("mode").value = cfg.mode;
  $("scale").value = cfg.scale;
  $("proxy").value = cfg.proxy;
  $("sites").value = Object.keys(cfg.sites).filter((s) => cfg.sites[s]).join("\n");
  checkProxy(cfg.proxy);
}

$("save").addEventListener("click", async () => {
  const sites = {};
  for (const line of $("sites").value.split("\n")) {
    const s = line.trim().replace(/^https?:\/\//, "").replace(/^www\./, "").replace(/[/:].*$/, "");
    if (s) sites[s] = true;
  }
  const cfg = {
    proxy: $("proxy").value.trim().replace(/\/+$/, "") || DEFAULTS.proxy,
    mode: Number($("mode").value),
    scale: Number($("scale").value) || DEFAULTS.scale,
    sites,
  };
  await chrome.storage.local.set({ vsrConfig: cfg });
  $("status").textContent = "Saved — reload open tabs to apply.";
  checkProxy(cfg.proxy);
});

load();
