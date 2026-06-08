// ---------------------------------------------------------------------------
// cli-command.js — Generate an ifc2lbd-neo CLI command from current UI state
// ---------------------------------------------------------------------------

import { getState } from "./state.js";

// Default option values per module, matching the Rust-side defaults.
// Only options that differ from these will appear in the command.
const MODULE_DEFAULTS = {
  "neo-turtle-serializer": {
    grouping: "streaming",
    layout: "joined",
  },
  "neo-nquads-chunked-serializer": {
    chunking: "lines",
    chunk_size_lines: "2000000",
    chunk_size_bytes: "268435456",
    chunk_prefix: "out",
    graph_naming: "producers",
  },
  "neo-nquads-serializer": {
    graph_naming: "producers",
  },
  "neo-bsdd-producer": {
    profile: "base",
    compact: "false",
    include_standard_attrs: "true",
    dedup_properties: "false",
  },
  "neo-geometry-preprocess": {
    metadata: "full",
  },
  "neo-geometry-producer": {
    format: "fragments",
  },
  "neo-ifcowl-producer": {
    mode: "full",
  },
  "neo-file-export": {
    compress: "none",
  },
};

function outputExtension(activeModules) {
  if (activeModules.has("neo-nquads-serializer") || activeModules.has("neo-nquads-chunked-serializer")) {
    return ".nq";
  }
  return ".ttl";
}

export function generateCliCommand() {
  const { activeModules, moduleOptions, baseUri, outputStem, ifcFile } = getState();

  const args = ["ifc2lbd-neo"];

  // positional input
  args.push(shellQuote(ifcFile?.name ?? "input.ifc"));

  // --output
  args.push("--output", shellQuote(`${outputStem}${outputExtension(activeModules)}`));

  // --base-uri
  args.push("--base-uri", shellQuote(baseUri));

  // --module for every active module
  for (const id of [...activeModules].sort()) {
    args.push("--module", id);
  }

  // --module-opt: emit all known options, using user value if set else the default
  for (const id of [...activeModules].sort()) {
    const defaults = MODULE_DEFAULTS[id];
    if (!defaults) continue;
    const userOpts = moduleOptions[id] ?? {};
    for (const [key, def] of Object.entries(defaults)) {
      const value = (userOpts[key] !== undefined && userOpts[key] !== "") ? userOpts[key] : def;
      args.push("--module-opt", `${id}.${key}=${value}`);
    }
  }

  return args.join(" ");
}

function shellQuote(str) {
  if (/^[a-zA-Z0-9_./:@=-]+$/.test(str)) return str;
  return `'${str.replace(/'/g, "'\\''")}'`;
}

// ---------------------------------------------------------------------------
// Modal UI — markup lives in index.html, we just wire and show it
// ---------------------------------------------------------------------------

let wired = false;

function wireModal() {
  if (wired) return;
  wired = true;
  document.querySelector(".cli-modal-backdrop")?.addEventListener("click", closeModal);
  document.querySelector("#cli-modal-close")?.addEventListener("click", closeModal);
  document.querySelector("#cli-modal-copy")?.addEventListener("click", () => {
    const text = document.querySelector("#cli-modal-pre")?.textContent ?? "";
    navigator.clipboard.writeText(text).then(() => {
      const btn = document.querySelector("#cli-modal-copy");
      if (btn) {
        btn.textContent = "Copied!";
        setTimeout(() => { btn.textContent = "Copy"; }, 1500);
      }
    });
  });
}

function closeModal() {
  document.querySelector("#cli-modal")?.classList.remove("open");
}

export function showCliCommand() {
  wireModal();
  const pre = document.querySelector("#cli-modal-pre");
  if (pre) pre.textContent = generateCliCommand();
  document.querySelector("#cli-modal")?.classList.add("open");
}
