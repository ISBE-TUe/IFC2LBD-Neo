import initWasm, { convertIfcToSink, initNeoThreadPool } from "./wasm/ifc2lbd_wasm.js";

let wasmReady = false;
let threadPoolInitialized = false;
let threadPoolSize = 0;

const ensureWasm = async () => {
  if (wasmReady) return;
  await initWasm();
  wasmReady = true;
};

const ensureThreadPool = async (requestedThreads) => {
  const threads = Math.max(2, Number(requestedThreads || 4));
  if (threadPoolInitialized) return { threads: threadPoolSize, reused: true };
  await withTimeout(initNeoThreadPool(threads), 8000, "initNeoThreadPool timeout");
  threadPoolInitialized = true;
  threadPoolSize = threads;
  return { threads: threadPoolSize, reused: false };
};

const withTimeout = (promise, ms, label) => {
  let timer = null;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(label)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer) clearTimeout(timer);
  });
};

self.addEventListener("message", async (event) => {
  const { id, type, payload } = event.data || {};
  if (!id || type !== "convert") return;

  try {
    self.postMessage({ id, type: "status", phase: "init-wasm", status: "start" });
    await ensureWasm();
    self.postMessage({ id, type: "status", phase: "init-wasm", status: "done" });
    self.postMessage({ id, type: "status", phase: "init-threadpool", status: "start" });
    const threadInfo = await ensureThreadPool(payload?.requestedThreads);
    self.postMessage({ id, type: "status", phase: "init-threadpool", status: "done" });
    self.postMessage({ id, type: "threadpool", threads: threadInfo.threads, reused: threadInfo.reused });

    const input = new Uint8Array(payload.inputBuffer);
    const request = payload.request || {};

    // Rust now emits real measured durations in stage events.
    // We just forward them — no JS-side timing needed.
    const sink = (sinkEvent) => {
      if (!sinkEvent || !sinkEvent.type) return;

      // Forward stage events — Rust provides real durationMs
      if (sinkEvent.type === "stageEvent") {
        self.postMessage({
          id,
          type: "stageEvent",
          pluginId: sinkEvent.pluginId,
          stage: sinkEvent.stage,
          status: sinkEvent.status,
          durationMs: sinkEvent.durationMs || 0,
          bytesOut: sinkEvent.bytesOut || 0,
          triplesOut: sinkEvent.triplesOut || 0,
          error: sinkEvent.error || null,
        });
        return;
      }

      // Forward file lifecycle events
      if (sinkEvent.type === "fileStart" || sinkEvent.type === "fileEnd") {
        self.postMessage({ id, ...sinkEvent });
        return;
      }

      // Forward file chunks with transfer
      if (sinkEvent.type === "fileChunk" && sinkEvent.filename && sinkEvent.chunk) {
        const chunk = sinkEvent.chunk instanceof Uint8Array ? sinkEvent.chunk : new Uint8Array(sinkEvent.chunk);
        const chunkCopy = chunk.slice();
        self.postMessage(
          { id, type: "chunk", filename: sinkEvent.filename, chunk: chunkCopy },
          [chunkCopy.buffer]
        );
        return;
      }
    };

    self.postMessage({ id, type: "status", phase: "convert", status: "start" });
    const streamResult = await withTimeout(
      Promise.resolve().then(() => convertIfcToSink(input, request, sink)),
      20000,
      "convertIfcToSink timeout"
    );
    self.postMessage({ id, type: "status", phase: "convert", status: "done" });
    self.postMessage({ id, type: "done", streamResult });
  } catch (error) {
    self.postMessage({
      id,
      type: "error",
      error: error instanceof Error ? error.message : String(error)
    });
  }
});
