// ---------------------------------------------------------------------------
// config.js — Save/load pipeline configuration as JSON
// ---------------------------------------------------------------------------

import { getState, update } from "./state.js";
import { log } from "./log-panel.js";

export function saveConfig() {
  const { activeModules, moduleOptions, baseUri, outputStem } = getState();

  // Build moduleOptions array from the structured options
  const moduleOptionsArr = [];
  for (const [pluginId, opts] of Object.entries(moduleOptions)) {
    for (const [key, value] of Object.entries(opts)) {
      if (value) moduleOptionsArr.push(`${pluginId}.${key}=${value}`);
    }
  }

  const config = {
    version: 1,
    moduleIds: [...activeModules],
    moduleOptions: moduleOptionsArr,
    baseUri,
    outputStem,
  };

  const blob = new Blob([JSON.stringify(config, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "pipeline-config.json";
  a.click();
  URL.revokeObjectURL(url);
  log("Config saved.");
}

export function loadConfig() {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".json";
  input.addEventListener("change", async () => {
    const file = input.files?.[0];
    if (!file) return;
    try {
      const text = await file.text();
      const config = JSON.parse(text);
      if (config.version !== 1) throw new Error(`Unsupported config version: ${config.version}`);

      const activeModules = new Set(config.moduleIds || []);
      const moduleOptions = {};

      // Parse moduleOptions array into structured format
      for (const opt of config.moduleOptions || []) {
        const dotIdx = opt.indexOf(".");
        const eqIdx = opt.indexOf("=", dotIdx + 1);
        if (dotIdx < 0 || eqIdx < 0) continue;
        const pluginId = opt.slice(0, dotIdx);
        const key = opt.slice(dotIdx + 1, eqIdx);
        const value = opt.slice(eqIdx + 1);
        if (!moduleOptions[pluginId]) moduleOptions[pluginId] = {};
        moduleOptions[pluginId][key] = value;
      }

      update({
        activeModules,
        moduleOptions,
        baseUri: config.baseUri || "https://lbd.org/",
        outputStem: config.outputStem || "",
      });
      log(`Config loaded: ${activeModules.size} modules, ${config.moduleOptions?.length || 0} options.`);
    } catch (err) {
      log(`Config load error: ${err.message}`);
    }
  });
  input.click();
}
