const DEFAULTS = {
  proxy: "http://127.0.0.1:8730",
  sites: { "youtube.com": true },
};
const $ = (id) => document.getElementById(id);

async function load() {
  const { vsrConfig } = await chrome.storage.local.get("vsrConfig");
  const cfg = Object.assign({}, DEFAULTS, vsrConfig);
  $("proxy").value = cfg.proxy;
  $("sites").value = Object.keys(cfg.sites).filter((s) => cfg.sites[s]).join("\n");
}

$("save").addEventListener("click", async () => {
  const sites = {};
  for (const line of $("sites").value.split("\n")) {
    const s = line.trim().replace(/^https?:\/\//, "").replace(/^www\./, "").replace(/[/:].*$/, "");
    if (s) sites[s] = true;
  }
  const cfg = {
    proxy: $("proxy").value.trim().replace(/\/+$/, "") || DEFAULTS.proxy,
    sites,
  };
  await chrome.storage.local.set({ vsrConfig: cfg });
  $("status").textContent = "Saved. Reload open tabs to apply.";
});

load();
