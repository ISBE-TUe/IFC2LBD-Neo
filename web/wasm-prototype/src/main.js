import initWasm, {
  initNeoThreadPool,
  listModules,
  resolvePlan,
  convertIfc,
  convertIfcToSink,
  planExecution
} from "./wasm/ifc2lbd_wasm.js";
import "./styles.css";

const modulesEl = document.querySelector("#modules");
const runtimeEl = document.querySelector("#runtime");
const downloadsEl = document.querySelector("#downloads");
const logEl = document.querySelector("#log");
const formEl = document.querySelector("#convert-form");
const convertBtn = document.querySelector("#convert-btn");
let threadPoolInitialized = false;
let threadPoolSize = 0;
const RUNTIME_BUILD = "retry-v4-2026-04-14T18:44Z";

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
  log("WASM initialized. Thread pool will be configured per execution mode on first conversion.");
  log(`Runtime build: ${RUNTIME_BUILD}`);
};

const supportsFileSave = () => typeof window.showSaveFilePicker === "function";
const shouldRetryLowmem = (error, serializer, currentMode) => {
  return serializer === "ttl" && currentMode !== "lowmem";
};

const detectFeasibilityBudgetMb = () => {
  const deviceMemoryGb = Number(navigator.deviceMemory || 0);
  if (!Number.isFinite(deviceMemoryGb) || deviceMemoryGb <= 0) return undefined;
  return Math.max(512, Math.floor(deviceMemoryGb * 1024 * 0.55));
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
    if (!threadPoolInitialized) {
      const requestedThreads = Math.max(2, Number(navigator.hardwareConcurrency || 4));
      await initNeoThreadPool(requestedThreads);
      threadPoolInitialized = true;
      threadPoolSize = requestedThreads;
      log(`Thread pool initialized: ${threadPoolSize} threads`);
    }
    const t0 = performance.now();
    let exportedFileCount = 0;
    const shouldUseFileSink = supportsFileSave();
    const expectedFiles =
      serializer === "nq"
        ? [{ filename: `${outputStem}.nq`, mimeType: "application/n-quads" }]
        : emitIfcowl
          ? [
              { filename: `${outputStem}.ttl`, mimeType: "text/turtle;charset=utf-8" },
              { filename: `${outputStem}_ifcowl.ttl`, mimeType: "text/turtle;charset=utf-8" }
            ]
          : [{ filename: `${outputStem}.ttl`, mimeType: "text/turtle;charset=utf-8" }];
    const runSinkConversion = async (payload) => {
      const writers = new Map();
      const memoryFiles = new Map();
      try {
        if (shouldUseFileSink) {
          for (const fileMeta of expectedFiles) {
            const handle = await window.showSaveFilePicker({
              suggestedName: fileMeta.filename,
              types: [
                {
                  description: fileMeta.mimeType.includes("n-quads") ? "N-Quads" : "Turtle",
                  accept: {
                    [fileMeta.mimeType]: [fileMeta.filename.endsWith(".nq") ? ".nq" : ".ttl"]
                  }
                }
              ]
            });
            const writable = await handle.createWritable();
            writers.set(fileMeta.filename, { writable, pendingWrite: Promise.resolve(), pendingBytes: 0 });
          }
        }
        const sink = (event) => {
          if (!event || !event.type) return;
          if (event.type === "fileChunk" && event.chunk && event.filename) {
            const chunk = event.chunk instanceof Uint8Array ? event.chunk : new Uint8Array(event.chunk);
            if (shouldUseFileSink) {
              const state = writers.get(event.filename);
              if (!state) throw new Error(`Missing sink target for ${event.filename}`);
              state.pendingBytes += chunk.byteLength;
              state.pendingWrite = state.pendingWrite
                .then(() => state.writable.write(chunk))
                .then(() => {
                  state.pendingBytes -= chunk.byteLength;
                });
            } else {
              const chunks = memoryFiles.get(event.filename) || [];
              chunks.push(chunk);
              memoryFiles.set(event.filename, chunks);
            }
          }
        };
        const streamResult = convertIfcToSink(input, payload, sink);
        if (shouldUseFileSink) {
          await Promise.all([...writers.values()].map((s) => s.pendingWrite));
          await Promise.all([...writers.values()].map((s) => s.writable.close()));
          return { streamResult, renderedFiles: [] };
        }
        const renderedFiles = expectedFiles.map((meta) => {
          const parts = memoryFiles.get(meta.filename) || [];
          return {
            filename: meta.filename,
            mimeType: meta.mimeType,
            role: meta.filename.endsWith("_ifcowl.ttl")
              ? "ifcowl"
              : meta.filename.endsWith(".ttl")
                ? "lbd"
                : "merged",
            payloadParts: parts
          };
        });
        return { streamResult, renderedFiles };
      } catch (error) {
        if (shouldUseFileSink) {
          await Promise.all(
            [...writers.values()].map(async (s) => {
              try {
                await s.writable.abort();
              } catch (_) {}
            })
          );
        }
        throw error;
      }
    };
    let streamResult;
    let renderedFiles = [];
    try {
      const result = await runSinkConversion(requestPayload);
      streamResult = result.streamResult;
      renderedFiles = result.renderedFiles || [];
    } catch (error) {
      const mode = (requestPayload.executionMode || "auto").toLowerCase();
      if (!shouldRetryLowmem(error, serializer, mode)) throw error;
      log(`Fast/auto Turtle run failed (${error instanceof Error ? error.message : String(error)}). Retrying with lowmem mode.`);
      const retryResult = await runSinkConversion({ ...requestPayload, executionMode: "lowmem" });
      streamResult = retryResult.streamResult;
      renderedFiles = retryResult.renderedFiles || [];
    }
    exportedFileCount = streamResult.outputFileCount || expectedFiles.length;
    if (shouldUseFileSink) {
      downloadsEl.innerHTML = "";
      const msg = document.createElement("div");
      msg.textContent = `Saved streamed output files: ${expectedFiles.map((f) => f.filename).join(", ")}`;
      downloadsEl.appendChild(msg);
    } else {
      renderDownloads(renderedFiles);
    }
    if (streamResult.telemetry) log(`Telemetry: ${JSON.stringify(streamResult.telemetry)}`);
    const elapsedMs = performance.now() - t0;
    setRuntimeMeta([
      `Thread pool initialized: ${threadPoolInitialized ? threadPoolSize : "pending"} threads`,
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
