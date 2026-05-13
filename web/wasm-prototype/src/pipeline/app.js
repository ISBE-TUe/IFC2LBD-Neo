// ---------------------------------------------------------------------------
// app.js — Pipeline dashboard (Ableton Session View style)
// ---------------------------------------------------------------------------

import initWasm, { listModules, resolvePlan, planExecution, convertIfcToSink, initNeoThreadPool } from "../wasm/ifc2lbd_wasm.js";
import "./pipeline.css";
import { getState, update, updateStageStatus, resetStageStatuses } from "./state.js";
import { initSession } from "./session.js";
import { initSidebar } from "./sidebar.js";
import { initLogPanel, log } from "./log-panel.js";
import { saveConfig, loadConfig } from "./config.js";

const RUNTIME_BUILD = "pipeline-v9-2026-05-13T14:00Z";

// ---------------------------------------------------------------------------
// Pipeline Templates
// ---------------------------------------------------------------------------

const LBD_MODULES = ["neo-bot-producer", "neo-beo-producer", "neo-props-opm", "neo-omg-fog"];

const TEMPLATES = [
  {
    id: "core-turtle-joined",
    label: "Core → Turtle (Joined)",
    desc: "BOT+BEO+Props+OMG into one grouped Turtle file",
    modules: [...LBD_MODULES, "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "sorted", layout: "joined" } },
  },
  {
    id: "core-turtle-separate",
    label: "Core → Turtle (Separate)",
    desc: "One Turtle file per active producer module",
    modules: [...LBD_MODULES, "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "sorted", layout: "separate" } },
  },
  {
    id: "core-ifcowl-turtle-joined",
    label: "Core+IfcOWL → Turtle (Joined)",
    desc: "All active producers merged into a single Turtle file",
    modules: [...LBD_MODULES, "neo-ifcowl-producer", "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "sorted", layout: "joined" } },
  },
  {
    id: "core-ifcowl-turtle-separate",
    label: "Core+IfcOWL → Turtle (Separate)",
    desc: "Per-module Turtle files including IfcOWL",
    modules: [...LBD_MODULES, "neo-ifcowl-producer", "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "sorted", layout: "separate" } },
  },
  {
    id: "core-ifcowl-nq",
    label: "Core+IfcOWL → N-Quads",
    desc: "Merged named-graph N-Quads export",
    modules: [...LBD_MODULES, "neo-ifcowl-producer", "neo-nquads-serializer", "neo-file-export"],
    options: {},
  },
  {
    id: "core-ifcowl-nq-chunked",
    label: "Core+IfcOWL → Chunked NQ",
    desc: "Chunked N-Quads parts plus manifest",
    modules: [...LBD_MODULES, "neo-ifcowl-producer", "neo-nquads-chunked-serializer", "neo-file-export"],
    options: { "neo-nquads-chunked-serializer": { chunking: "lines" } },
  },
  {
    id: "core-ifcowl-topology-turtle",
    label: "Core+IfcOWL+Topology → Turtle",
    desc: "Includes IfcTopology producer and grouped Turtle output",
    modules: [...LBD_MODULES, "neo-ifcowl-producer", "neo-ifc-topology-producer", "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "sorted", layout: "joined" } },
  },
  {
    id: "core-ifcowl-topology-bbox-turtle",
    label: "Core+IfcOWL+Topology+Bbox → Turtle",
    desc: "Topology plus bbox enrichment with joined Turtle output",
    modules: [...LBD_MODULES, "neo-ifcowl-producer", "neo-ifc-topology-producer", "neo-bbox-enricher", "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "sorted", layout: "joined" } },
  },
  {
    id: "core-turtle-streaming",
    label: "Core → Turtle (Streaming)",
    desc: "Low-memory incremental Turtle writer (no grouping)",
    modules: [...LBD_MODULES, "neo-turtle-serializer", "neo-file-export"],
    options: { "neo-turtle-serializer": { grouping: "streaming", layout: "joined" } },
  },
];

function applyTemplate(templateId) {
  const tpl = TEMPLATES.find((t) => t.id === templateId);
  if (!tpl) return;
  update({
    activeModules: new Set(tpl.modules),
    moduleOptions: { ...tpl.options },
  });
  log(`Template: ${tpl.label}`);
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

let conversionWorker = null;
let conversionRequestId = 0;
const pendingConversionRequests = new Map();
let threadPoolInitialized = false;
let threadPoolSize = 0;
let outputDirectoryHandle = null;
let outputDirectoryName = "";
const supportsOutputDirectoryPicker = typeof window.showDirectoryPicker === "function";
let asciiBgCanvas = null;
let asciiBgCtx = null;
let asciiBgAnimId = 0;

const detectFeasibilityBudgetMb = () => {
  const gb = Number(navigator.deviceMemory || 0);
  if (!Number.isFinite(gb) || gb <= 0) return undefined;
  return Math.max(512, Math.floor(gb * 1024 * 0.55));
};

function getConversionWorker() {
  if (conversionWorker) return conversionWorker;
  conversionWorker = new Worker(new URL("../wasm-lowmem-worker.js", import.meta.url), { type: "module" });
  conversionWorker.addEventListener("message", (event) => {
    const data = event.data || {};
    const pending = pendingConversionRequests.get(data.id);
    if (!pending) return;

    if (data.type === "threadpool") {
      pending.threadPoolSize = Number(data.threads) || pending.threadPoolSize;
      if (!pending.threadPoolLogged && !data.reused) {
        pending.threadPoolLogged = true;
        log(`ThreadPool: ${pending.threadPoolSize} threads`);
      }
      return;
    }

    if (data.type === "status") {
      log(`Worker ${data.phase}: ${data.status}`);
      return;
    }

    if (data.type === "stageEvent") {
      updateStageStatus(data);
      const icon = data.status === "success" ? "✓" : data.status === "failed" ? "✗" : "→";
      log(`${icon} ${data.pluginId}: ${data.status}${data.durationMs ? ` ${(data.durationMs / 1000).toFixed(2)}s` : ""}${data.triplesOut ? ` (${data.triplesOut.toLocaleString()} triples)` : ""}`);
      return;
    }

    if (data.type === "chunk") {
      if (!data.filename || data.chunk == null) return;
      const chunk = data.chunk instanceof Uint8Array ? data.chunk : new Uint8Array(data.chunk);
      const chunks = pending.memoryFiles.get(data.filename) || [];
      chunks.push(chunk);
      pending.memoryFiles.set(data.filename, chunks);
      return;
    }

    if (data.type === "done") {
      // Assemble all files that came through the sink.
      // For known files (single .nq, .ttl), use expectedFiles metadata.
      // For chunked output, the sink emitted fileStart/fileChunk/fileEnd with
      // dynamic filenames — we build the file list from memoryFiles directly.
      const renderedFiles = [];
      const knownFilenames = new Set(pending.expectedFiles.map(m => m.filename));
      // Known files first (with metadata from expectedFiles)
      for (const meta of pending.expectedFiles) {
        renderedFiles.push({
          filename: meta.filename, mimeType: meta.mimeType, role: meta.role,
          payloadParts: pending.memoryFiles.get(meta.filename) || []
        });
      }
      // Then any additional files from the sink (chunked .part-XXX.nq, manifest.json, etc.)
      for (const [filename, chunks] of pending.memoryFiles.entries()) {
        if (!knownFilenames.has(filename)) {
          const isManifest = filename.endsWith(".manifest.json");
          const isNq = filename.endsWith(".nq");
          renderedFiles.push({
            filename,
            mimeType: isManifest ? "application/json" : isNq ? "application/n-quads" : "application/octet-stream",
            role: isManifest ? "manifest" : isNq ? "chunk" : "other",
            payloadParts: chunks,
          });
        }
      }
      pendingConversionRequests.delete(data.id);
      pending.resolve({ streamResult: data.streamResult || {}, renderedFiles, threadPoolSize: pending.threadPoolSize });
      return;
    }

    if (data.type === "error") {
      pendingConversionRequests.delete(data.id);
      pending.reject(new Error(data.error || "Worker conversion failed."));
    }
  });
  return conversionWorker;
}

function runSinkConversionInWorker(input, requestPayload, expectedFiles, requestedThreads) {
  return new Promise((resolve, reject) => {
    const worker = getConversionWorker();
    const id = `conv-${++conversionRequestId}`;
    const inputCopy = input.slice();
    pendingConversionRequests.set(id, {
      resolve, reject, expectedFiles,
      memoryFiles: new Map(),
      threadPoolSize: requestedThreads,
      threadPoolLogged: false
    });
    worker.postMessage(
      { id, type: "convert", payload: { inputBuffer: inputCopy.buffer, request: requestPayload, requestedThreads } },
      [inputCopy.buffer]
    );
  });
}

async function runSinkConversionInMain(input, requestPayload, expectedFiles, requestedThreads) {
  if (!threadPoolInitialized) {
    await initNeoThreadPool(requestedThreads);
    threadPoolInitialized = true;
    threadPoolSize = requestedThreads;
    log(`ThreadPool: ${threadPoolSize} threads`);
  }
  const memoryFiles = new Map();
  const sink = (event) => {
    if (!event || !event.type) return;
    if (event.type === "stageEvent") {
      updateStageStatus(event);
      const icon = event.status === "success" ? "✓" : event.status === "failed" ? "✗" : "→";
      log(`${icon} ${event.pluginId}: ${event.status}${event.durationMs ? ` ${(event.durationMs / 1000).toFixed(2)}s` : ""}${event.triplesOut ? ` (${event.triplesOut.toLocaleString()} triples)` : ""}`);
      return;
    }
    if (event.type === "fileChunk" && event.filename && event.chunk != null) {
      const chunk = event.chunk instanceof Uint8Array ? event.chunk : new Uint8Array(event.chunk);
      const chunks = memoryFiles.get(event.filename) || [];
      chunks.push(chunk);
      memoryFiles.set(event.filename, chunks);
    }
  };
  const streamResult = convertIfcToSink(input, requestPayload, sink);
  const renderedFiles = [];
  const knownFilenames = new Set(expectedFiles.map((m) => m.filename));
  for (const meta of expectedFiles) {
    renderedFiles.push({
      filename: meta.filename,
      mimeType: meta.mimeType,
      role: meta.role,
      payloadParts: memoryFiles.get(meta.filename) || [],
    });
  }
  for (const [filename, chunks] of memoryFiles.entries()) {
    if (!knownFilenames.has(filename)) {
      const isManifest = filename.endsWith(".manifest.json");
      const isNq = filename.endsWith(".nq");
      renderedFiles.push({
        filename,
        mimeType: isManifest ? "application/json" : isNq ? "application/n-quads" : "application/octet-stream",
        role: isManifest ? "manifest" : isNq ? "chunk" : "other",
        payloadParts: chunks,
      });
    }
  }
  return { streamResult: streamResult || {}, renderedFiles, threadPoolSize };
}

// ---------------------------------------------------------------------------
// Module toggle wiring — now handled by grid circles in session.js
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

async function init() {
  if (!window.isSecureContext || !window.crossOriginIsolated) {
    throw new Error("Requires secure+isolated context. Open http://localhost:3031 or use HTTPS with COOP/COEP.");
  }

  await initWasm();
  const modules = listModules();
  update({ modules });

  initSession(document.querySelector("#session-area"));
  initSidebar();
  initLogPanel();
  initAsciiBackground();

  // Wire template picker (dropdown)
  const templatePicker = document.querySelector("#template-picker");
  if (templatePicker) {
    for (const tpl of TEMPLATES) {
      const opt = document.createElement("option");
      opt.value = tpl.id;
      opt.textContent = tpl.label;
      opt.title = tpl.desc;
      templatePicker.appendChild(opt);
    }
    templatePicker.addEventListener("change", () => {
      if (templatePicker.value) {
        applyTemplate(templatePicker.value);
        templatePicker.value = ""; // reset to placeholder
      }
    });
  }

  // Wire left rail: file input + settings
  document.querySelector("#file-input")?.addEventListener("change", (e) => {
    const file = e.target.files?.[0];
    if (!file) return;
    update({ ifcFile: file });
    const reader = new FileReader();
    reader.onload = () => update({ ifcFileBytes: new Uint8Array(reader.result) });
    reader.readAsArrayBuffer(file);
    // Update rail UI
    const btn = document.querySelector("#rail-file-btn");
    if (btn) btn.classList.add("has-file");
    const text = document.querySelector("#rail-file-text");
    if (text) text.textContent = file.name;
    const meta = document.querySelector("#rail-file-meta");
    if (meta) meta.textContent = bytesToHuman(file.size);
    log(`File: ${file.name} (${bytesToHuman(file.size)})`);
  });
  document.querySelector("#btn-output-dir")?.addEventListener("click", async () => {
    if (!supportsOutputDirectoryPicker) {
      log("Output directory picker is not supported in this browser.");
      return;
    }
    try {
      const dirHandle = await window.showDirectoryPicker({ mode: "readwrite" });
      outputDirectoryHandle = dirHandle;
      outputDirectoryName = dirHandle?.name || "(selected folder)";
      const text = document.querySelector("#output-dir-text");
      if (text) text.textContent = outputDirectoryName;
      const meta = document.querySelector("#output-dir-meta");
      if (meta) meta.textContent = "Outputs will be written to this folder.";
      const btn = document.querySelector("#btn-output-dir");
      if (btn) btn.classList.add("has-file");
      log(`Output directory: ${outputDirectoryName}`);
    } catch (error) {
      if (error?.name !== "AbortError") {
        log(`Output directory error: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  });
  document.querySelector("#btn-output-dir-clear")?.addEventListener("click", () => {
    outputDirectoryHandle = null;
    outputDirectoryName = "";
    const text = document.querySelector("#output-dir-text");
    if (text) text.textContent = "Choose output folder…";
    const meta = document.querySelector("#output-dir-meta");
    if (meta) meta.textContent = "";
    const btn = document.querySelector("#btn-output-dir");
    if (btn) btn.classList.remove("has-file");
    log("Output directory cleared.");
  });
  document.querySelector("#base-uri-input")?.addEventListener("change", (e) => update({ baseUri: e.target.value.trim() }));
  document.querySelector("#output-stem-input")?.addEventListener("change", (e) => update({ outputStem: e.target.value.trim() || "converted-model" }));
  document.querySelector("#toggle-preprocess")?.addEventListener("change", (e) => update({ showPreprocess: e.target.checked }));
  document.querySelector("#toggle-postprocess")?.addEventListener("change", (e) => update({ showPostprocess: e.target.checked }));
  document.querySelector("#btn-load")?.addEventListener("click", loadConfig);
  document.querySelector("#btn-save")?.addEventListener("click", saveConfig);
  document.querySelector("#btn-run")?.addEventListener("click", runConversion);
  setupOutputDirectoryUiSupport();

  log("WASM ready. Pipeline dashboard v9.");
  log(`Build: ${RUNTIME_BUILD}`);
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

async function runConversion() {
  const state = getState();
  if (state.running) return;
  if (!state.ifcFileBytes) { log("No file selected."); return; }

  update({ running: true });
  const runBtn = document.querySelector("#btn-run");
  if (runBtn) { runBtn.disabled = true; runBtn.textContent = "◉ RUNNING"; runBtn.classList.add("running"); }

  try {
    const { activeModules, moduleOptions, baseUri, outputStem, ifcFileBytes } = getState();
    const moduleIds = [...activeModules];
    const moduleOptionsArr = [];
    for (const [pluginId, opts] of Object.entries(moduleOptions)) {
      for (const [key, value] of Object.entries(opts)) {
        if (value) moduleOptionsArr.push(`${pluginId}.${key}=${value}`);
      }
    }

    const input = ifcFileBytes;
    const plan = resolvePlan(moduleIds, moduleOptionsArr);
    log(`Plan: ${plan.enabledIds.join(", ")}`);

    const requestPayload = { moduleIds, moduleOptions: moduleOptionsArr, baseUri, outputStem, executionMode: "auto" };
    const feasibilityMb = detectFeasibilityBudgetMb();
    if (feasibilityMb) requestPayload.memoryFeasibilityMb = feasibilityMb;
    const executionPlan = planExecution(input.byteLength, requestPayload);
    log(`Mode=${executionPlan.selectedMode} est=${executionPlan.estimatedPeakMb}MB`);

    const requestedThreads = Math.max(2, Number(navigator.hardwareConcurrency || 4));
    const hasNq = activeModules.has("neo-nquads-serializer") || activeModules.has("neo-nquads-chunked-serializer");
    const hasChunkedNq = activeModules.has("neo-nquads-chunked-serializer");
    const hasIfcowl = activeModules.has("neo-ifcowl-producer");
    const turtleLayout = moduleOptions["neo-turtle-serializer"]?.layout || "joined";
    // For chunked output, filenames are dynamic (out-lbd.part-000.nq, etc.)
    // so expectedFiles only lists the single-file outputs.
    // The done handler will pick up chunked files from memoryFiles automatically.
    const expectedFiles = hasChunkedNq
      ? []  // chunked files are discovered from sink events
      : hasNq
        ? [{ filename: `${outputStem}.nq`, mimeType: "application/n-quads", role: "merged" }]
        : turtleLayout === "separate"
          ? [
              ...(activeModules.has("neo-bot-producer") ? [{ filename: `${outputStem}_bot.ttl`, mimeType: "text/turtle", role: "bot" }] : []),
              ...(activeModules.has("neo-beo-producer") ? [{ filename: `${outputStem}_beo.ttl`, mimeType: "text/turtle", role: "beo" }] : []),
              ...(activeModules.has("neo-props-opm") ? [{ filename: `${outputStem}_props.ttl`, mimeType: "text/turtle", role: "props" }] : []),
              ...(activeModules.has("neo-omg-fog") ? [{ filename: `${outputStem}_omg.ttl`, mimeType: "text/turtle", role: "omg" }] : []),
              ...(activeModules.has("neo-ifcowl-producer") ? [{ filename: `${outputStem}_ifcowl.ttl`, mimeType: "text/turtle", role: "ifcowl" }] : []),
              ...(activeModules.has("neo-ifc-topology-producer") ? [{ filename: `${outputStem}_topology.ttl`, mimeType: "text/turtle", role: "topology" }] : []),
            ]
          : [{ filename: `${outputStem}.ttl`, mimeType: "text/turtle", role: "joined" }];

    const t0 = performance.now();
    resetStageStatuses();
    const result = await runSinkConversionInWorker(input, requestPayload, expectedFiles, requestedThreads);
    const elapsedMs = performance.now() - t0;

    const filesWithPayload = result.renderedFiles.filter(
      (f) => Array.isArray(f?.payloadParts) && f.payloadParts.length > 0
    );
    if (!filesWithPayload.length) {
      throw new Error("Conversion finished but produced no output payloads.");
    }

    if (outputDirectoryHandle) {
      try {
        const writable = await ensureOutputDirectoryWritable(outputDirectoryHandle);
        if (!writable) {
          throw new Error("Output directory permission denied.");
        }
        const { fileCount, totalBytes } = await writeRenderedFilesToDirectory(filesWithPayload, outputDirectoryHandle);
        renderDownloadsMessage(`Saved ${fileCount} file(s), ${bytesToHuman(totalBytes)}, to ${outputDirectoryName}.`);
        log(`Saved ${fileCount} file(s), ${bytesToHuman(totalBytes)}, to output directory.`);
      } catch (error) {
        log(`Output write failed, falling back to downloads: ${error instanceof Error ? error.message : String(error)}`);
        renderDownloads(filesWithPayload);
      }
    } else {
      renderDownloads(filesWithPayload);
    }
    const timeStr = `${(elapsedMs / 1000).toFixed(1)}s`;
    log(`Finished in ${timeStr}.`);

    const infoEl = document.querySelector("#runtime-info");
    if (infoEl) infoEl.innerHTML = `<span style="color:var(--status-success);font-weight:600">Finished in ${timeStr}</span>`;
  } catch (error) {
    log(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
    const { stageStatuses } = getState();
    const updated = { ...stageStatuses };
    for (const [id, s] of Object.entries(updated)) {
      if (s.status === "running") updated[id] = { ...s, status: "failed", error: error.message };
    }
    update({ stageStatuses: updated });
  } finally {
    update({ running: false });
    if (runBtn) { runBtn.disabled = false; runBtn.textContent = "▶ RUN"; runBtn.classList.remove("running"); }
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// readFileAsBytes removed — file reading happens in session.js grid file input

function renderDownloads(files) {
  const container = document.querySelector("#downloads");
  if (!container) return;
  container.innerHTML = "";
  if (!files.length) { container.innerHTML = '<span class="downloads-empty">No files.</span>'; return; }
  for (const file of files) {
    if (!Array.isArray(file.payloadParts)) continue;
    const blob = new Blob(file.payloadParts, { type: file.mimeType });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.className = "download-link";
    link.href = url;
    link.download = file.filename;
    link.textContent = `${file.filename} (${bytesToHuman(blob.size)})`;
    container.appendChild(link);
  }
}

function renderDownloadsMessage(message) {
  const container = document.querySelector("#downloads");
  if (!container) return;
  container.innerHTML = `<span class="downloads-empty">${escapeHtml(message)}</span>`;
}

async function writeRenderedFilesToDirectory(files, dirHandle) {
  let fileCount = 0;
  let totalBytes = 0;
  for (const file of files) {
    if (!file?.filename || !Array.isArray(file.payloadParts)) continue;
    const blob = new Blob(file.payloadParts, { type: file.mimeType || "application/octet-stream" });
    const fileHandle = await dirHandle.getFileHandle(file.filename, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(blob);
    await writable.close();
    fileCount += 1;
    totalBytes += blob.size;
  }
  return { fileCount, totalBytes };
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

async function ensureOutputDirectoryWritable(dirHandle) {
  if (!dirHandle) return false;
  if (typeof dirHandle.queryPermission !== "function" || typeof dirHandle.requestPermission !== "function") {
    return true;
  }
  let permission = await dirHandle.queryPermission({ mode: "readwrite" });
  if (permission === "granted") return true;
  permission = await dirHandle.requestPermission({ mode: "readwrite" });
  return permission === "granted";
}

function bytesToHuman(bytes) {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

function initAsciiBackground() {
  const area = document.querySelector("#session-area");
  if (!area) return;
  asciiBgCanvas?.remove();
  asciiBgCanvas = document.createElement("canvas");
  asciiBgCanvas.className = "ascii-bg";
  area.prepend(asciiBgCanvas);
  asciiBgCtx = asciiBgCanvas.getContext("2d", { alpha: true });
  resizeAsciiBackground();
  window.addEventListener("resize", resizeAsciiBackground);
  if (asciiBgAnimId) cancelAnimationFrame(asciiBgAnimId);
  const chars = " .,:;+*xo%#@";
  const step = 10;
  const draw = (t) => {
    if (!asciiBgCtx || !asciiBgCanvas) return;
    const w = asciiBgCanvas.width;
    const h = asciiBgCanvas.height;
    asciiBgCtx.clearRect(0, 0, w, h);
    asciiBgCtx.font = "8.5px JetBrains Mono, monospace";
    asciiBgCtx.textBaseline = "top";
    const tt = t * 0.00045;
    for (let y = 0; y < h; y += step) {
      for (let x = 0; x < w; x += step) {
        const v =
          Math.sin(x * 0.016 + tt) * 0.6 +
          Math.cos(y * 0.020 - tt * 1.05) * 0.4 +
          Math.sin((x + y) * 0.007 + tt * 0.55) * 0.5;
        const n = Math.max(0, Math.min(chars.length - 1, Math.floor(((v + 1.5) / 3) * chars.length)));
        const ch = chars[n];
        const alpha = 0.13 + (n / chars.length) * 0.18;
        asciiBgCtx.fillStyle = `rgba(46,46,46,${alpha.toFixed(3)})`;
        asciiBgCtx.fillText(ch, x, y);
      }
    }
    asciiBgAnimId = requestAnimationFrame(draw);
  };
  asciiBgAnimId = requestAnimationFrame(draw);
}

function resizeAsciiBackground() {
  if (!asciiBgCanvas) return;
  const area = document.querySelector("#session-area");
  if (!area) return;
  const dpr = Math.max(1, window.devicePixelRatio || 1);
  const w = Math.max(1, area.clientWidth);
  const h = Math.max(1, area.clientHeight);
  asciiBgCanvas.width = Math.floor(w * dpr);
  asciiBgCanvas.height = Math.floor(h * dpr);
  asciiBgCanvas.style.width = `${w}px`;
  asciiBgCanvas.style.height = `${h}px`;
  if (asciiBgCtx) asciiBgCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
}

function setupOutputDirectoryUiSupport() {
  if (supportsOutputDirectoryPicker) return;
  const pickBtn = document.querySelector("#btn-output-dir");
  if (pickBtn) pickBtn.disabled = true;
  const clearBtn = document.querySelector("#btn-output-dir-clear");
  if (clearBtn) clearBtn.disabled = true;
  const unsupported = document.querySelector("#output-dir-unsupported");
  if (unsupported) unsupported.style.display = "block";
  const meta = document.querySelector("#output-dir-meta");
  if (meta) meta.textContent = "";
}

init().catch((error) => log(`Startup: ${error instanceof Error ? error.message : String(error)}`));
