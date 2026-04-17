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
  await initNeoThreadPool(threads);
  threadPoolInitialized = true;
  threadPoolSize = threads;
  return { threads: threadPoolSize, reused: false };
};

self.addEventListener("message", async (event) => {
  const { id, type, payload } = event.data || {};
  if (!id || type !== "convert") return;

  try {
    await ensureWasm();
    const threadInfo = await ensureThreadPool(payload?.requestedThreads);
    self.postMessage({ id, type: "threadpool", threads: threadInfo.threads, reused: threadInfo.reused });

    const input = new Uint8Array(payload.inputBuffer);
    const request = payload.request || {};

    const sink = (sinkEvent) => {
      if (!sinkEvent || sinkEvent.type !== "fileChunk" || !sinkEvent.filename || !sinkEvent.chunk) return;
      const chunk = sinkEvent.chunk instanceof Uint8Array ? sinkEvent.chunk : new Uint8Array(sinkEvent.chunk);
      const chunkCopy = chunk.slice();
      self.postMessage(
        {
          id,
          type: "chunk",
          filename: sinkEvent.filename,
          chunk: chunkCopy
        },
        [chunkCopy.buffer]
      );
    };

    const streamResult = convertIfcToSink(input, request, sink);
    self.postMessage({ id, type: "done", streamResult });
  } catch (error) {
    self.postMessage({
      id,
      type: "error",
      error: error instanceof Error ? error.message : String(error)
    });
  }
});
