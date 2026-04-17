import initWasm, { listModules, resolvePlan, planExecution } from "./wasm/ifc2lbd_wasm.js";
import "./styles.css";

const modulesEl = document.querySelector("#modules");
const runtimeEl = document.querySelector("#runtime");
const downloadsEl = document.querySelector("#downloads");
const logEl = document.querySelector("#log");
const formEl = document.querySelector("#convert-form");
const convertBtn = document.querySelector("#convert-btn");

let conversionWorker = null;
let conversionRequestId = 0;
const pendingConversionRequests = new Map();
const RUNTIME_BUILD = "worker-v6-2026-04-17Z";

const log = (line) => {
  logEl.textContent += `${new Date().toISOString()} ${line}\n`;
  logEl.scrollTop = logEl.scrollHeight;
};

const bytesToHuman = (bytes) => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
};

const setRuntimeMeta = (items) => {
  runtimeEl.innerHTML = "";
  for (const text of items) {
    const li = document.createElement("li");
    li.textContent = text;
    runtimeEl.appendChild(li);
  }
};

const setModuleList = (modules) => {
  modulesEl.innerHTML = "";
  for (const module of modules) {
    const li = document.createElement("li");
    li.textContent = `${module.id} (${module.stage})`;
    modulesEl.appendChild(li);
  }
};

const renderDownloads = (files) => {
  downloadsEl.innerHTML = "";
  if (!files.length) {
    downloadsEl.textContent = "No files exported.";
    return;
  }
  for (const file of files) {
    let blob;
    let sizeBytes = 0;
    if (file.payloadBlob instanceof Blob) {
      blob = file.payloadBlob;
      sizeBytes = blob.size;
    } else if (Array.isArray(file.payloadParts)) {
      blob = new Blob(file.payloadParts, { type: file.mimeType });
      sizeBytes = blob.size;
    } else {
      const payload = file.payload instanceof Uint8Array ? file.payload : new Uint8Array(file.payload);
      blob = new Blob([payload], { type: file.mimeType });
      sizeBytes = payload.byteLength;
    }
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.className = "download-link";
    link.href = url;
    link.download = file.filename;
    link.textContent = `${file.filename} (${bytesToHuman(sizeBytes)})`;
    downloadsEl.appendChild(link);
  }
};

const readFileAsBytes = (file) =>
  new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error || new Error("Unable to read file."));
    reader.onload = () => resolve(new Uint8Array(reader.result));
    reader.readAsArrayBuffer(file);
  });

const detectFeasibilityBudgetMb = () => {
  const deviceMemoryGb = Number(navigator.deviceMemory || 0);
  if (!Number.isFinite(deviceMemoryGb) || deviceMemoryGb <= 0) return undefined;
  return Math.max(512, Math.floor(deviceMemoryGb * 1024 * 0.55));
};

const getConversionWorker = () => {
  if (conversionWorker) return conversionWorker;
  conversionWorker = new Worker(new URL("./wasm-lowmem-worker.js", import.meta.url), { type: "module" });
  conversionWorker.addEventListener("message", (event) => {
    const data = event.data || {};
    const pending = pendingConversionRequests.get(data.id);
    if (!pending) return;

    if (data.type === "threadpool") {
      pending.threadPoolSize = Number(data.threads) || pending.threadPoolSize;
      if (!pending.threadPoolLogged && !data.reused) {
        pending.threadPoolLogged = true;
        log(`Thread pool initialized: ${pending.threadPoolSize} threads`);
      }
      return;
    }

    if (data.type === "chunk") {
      const filename = data.filename;
      if (!filename || data.chunk == null) return;
      const chunk = data.chunk instanceof Uint8Array ? data.chunk : new Uint8Array(data.chunk);
      const chunks = pending.memoryFiles.get(filename) || [];
      chunks.push(chunk);
      pending.memoryFiles.set(filename, chunks);
      return;
    }

    if (data.type === "done") {
      const renderedFiles = pending.expectedFiles.map((meta) => ({
        filename: meta.filename,
        mimeType: meta.mimeType,
        role: meta.role,
        payloadParts: pending.memoryFiles.get(meta.filename) || []
      }));
      pendingConversionRequests.delete(data.id);
      pending.resolve({
        streamResult: data.streamResult || {},
        renderedFiles,
        threadPoolSize: pending.threadPoolSize
      });
      return;
    }

    if (data.type === "error") {
      pendingConversionRequests.delete(data.id);
      pending.reject(new Error(data.error || "Worker conversion failed."));
    }
  });
  return conversionWorker;
};

const runSinkConversionInWorker = (input, requestPayload, expectedFiles, requestedThreads) =>
  new Promise((resolve, reject) => {
    const worker = getConversionWorker();
    const id = `conv-${++conversionRequestId}`;
    const inputCopy = input.slice();
    pendingConversionRequests.set(id, {
      resolve,
      reject,
      expectedFiles,
      memoryFiles: new Map(),
      threadPoolSize: requestedThreads,
      threadPoolLogged: false
    });
    worker.postMessage(
      {
        id,
        type: "convert",
        payload: {
          inputBuffer: inputCopy.buffer,
          request: requestPayload,
          requestedThreads
        }
      },
      [inputCopy.buffer]
    );
  });

const init = async () => {
  const secureContext = window.isSecureContext;
  const crossOriginIsolated = window.crossOriginIsolated;
  if (!secureContext || !crossOriginIsolated) {
    throw new Error(
      `Threaded WASM requires secure+isolated context (secure=${String(
        secureContext
      )}, isolated=${String(
        crossOriginIsolated
      )}). Open http://localhost:3031 (not a LAN IP), or use HTTPS with COOP/COEP headers.`
    );
  }
  await initWasm();
  const modules = listModules();
  setModuleList(modules);
  setRuntimeMeta([
    "Thread pool initialized: pending",
    `Cross-origin isolated: ${String(window.crossOriginIsolated)}`
  ]);
  log("WASM initialized. Conversion executes in a dedicated worker.");
  log(`Runtime build: ${RUNTIME_BUILD}`);
};

formEl.addEventListener("submit", async (event) => {
  event.preventDefault();
  convertBtn.disabled = true;
  downloadsEl.textContent = "Converting...";

  try {
    const file = document.querySelector("#ifc-file").files?.[0];
    if (!file) throw new Error("No IFC file selected.");

    const serializer = document.querySelector('input[name="serializer"]:checked')?.value || "ttl";
    const emitIfcowl = document.querySelector("#emit-ifcowl").checked;
    const baseUri = document.querySelector("#base-uri").value.trim();
    const outputStem = document.querySelector("#output-stem").value.trim() || "converted-model";

    const moduleIds = ["neo-lbd-producer", "neo-file-export"];
    if (emitIfcowl) moduleIds.push("neo-ifcowl-producer");
    moduleIds.push(serializer === "nq" ? "neo-nquads-serializer" : "neo-turtle-serializer");

    const moduleOptions = [];
    if (serializer === "nq") moduleOptions.push("neo-nquads-serializer.chunking=none");

    const input = await readFileAsBytes(file);
    const plan = resolvePlan(moduleIds, moduleOptions);
    log(`Resolved plan: ${plan.enabledIds.join(", ")}`);

    const requestPayload = {
      moduleIds,
      moduleOptions,
      baseUri,
      outputStem,
      executionMode: "auto"
    };
    const feasibilityMb = detectFeasibilityBudgetMb();
    if (feasibilityMb) requestPayload.memoryFeasibilityMb = feasibilityMb;
    const executionPlan = planExecution(input.byteLength, requestPayload);
    log(
      `Execution plan: mode=${executionPlan.selectedMode} est=${executionPlan.estimatedPeakMb}MB feasibility=${executionPlan.feasibilityCheckMb}MB`
    );

    const requestedThreads = Math.max(2, Number(navigator.hardwareConcurrency || 4));
    const expectedFiles =
      serializer === "nq"
        ? [{ filename: `${outputStem}.nq`, mimeType: "application/n-quads", role: "merged" }]
        : emitIfcowl
          ? [
              { filename: `${outputStem}.ttl`, mimeType: "text/turtle", role: "lbd" },
              { filename: `${outputStem}_ifcowl.ttl`, mimeType: "text/turtle", role: "ifcowl" }
            ]
          : [{ filename: `${outputStem}.ttl`, mimeType: "text/turtle", role: "lbd" }];

    const t0 = performance.now();
    const result = await runSinkConversionInWorker(input, requestPayload, expectedFiles, requestedThreads);
    const streamResult = result.streamResult || {};
    const renderedFiles = result.renderedFiles || [];

    renderDownloads(renderedFiles);
    if (streamResult.telemetry) log(`Telemetry: ${JSON.stringify(streamResult.telemetry)}`);

    const elapsedMs = performance.now() - t0;
    const exportedFileCount = streamResult.outputFileCount || expectedFiles.length;
    setRuntimeMeta([
      `Thread pool initialized: ${result.threadPoolSize || requestedThreads} threads`,
      `Cross-origin isolated: ${String(window.crossOriginIsolated)}`,
      `Conversion duration: ${(elapsedMs / 1000).toFixed(3)} s`,
      `Input size: ${bytesToHuman(input.byteLength)}`,
      `Exported files: ${exportedFileCount}`
    ]);
    log(`Conversion finished in ${(elapsedMs / 1000).toFixed(3)}s.`);
  } catch (error) {
    downloadsEl.textContent = "Conversion failed. Check log.";
    log(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    convertBtn.disabled = false;
  }
});

init().catch((error) => {
  log(`Startup failure: ${error instanceof Error ? error.message : String(error)}`);
  convertBtn.disabled = true;
});
