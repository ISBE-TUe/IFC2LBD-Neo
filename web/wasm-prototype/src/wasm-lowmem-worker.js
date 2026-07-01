// wasm-lowmem-worker.js — Low-memory WASM conversion worker
//
// Dynamically loads the wasm32 or wasm64 module based on the `wasmVariant`
// field in the convert payload.  wasm32 is the default (fast, no bounds
// checks on 64-bit systems).  wasm64 is used for large files that exceed
// the 4 GiB wasm32 memory cap (slower due to bounds checks, but 16 GiB cap).

let wasmReady = false;
const wasmVariantLoaded = null;
let threadPoolInitialized = false;
let threadPoolSize = 0;
let wasmApi = null;

// Use explicit if/else with static string literals so Vite/Rollup can
// statically analyze and rewrite each import to the correct production
// chunk URL.
//
// wasm32: bundled by Vite (import("./wasm/ifc2lbd_wasm.js"))
// wasm64: loaded from /wasm64/ (public dir, not bundled — 24 MB binary
//         only fetched when a file exceeds the 4 GB wasm32 cap)
//         Uses a variable + @vite-ignore so Vite doesn't try to resolve
//         the path at build time (the file only exists in public/ at
//         runtime after CI deploys it).
const WASM64_URL = "/wasm64/ifc2lbd_wasm.js";

const ensureWasm = async (variant) => {
	// Reload if variant changed (e.g. first file small → wasm32, second
	// file large → wasm64). The thread pool must also be re-initialized
	// for the new module.
	if (wasmReady && wasmApi && wasmVariantLoaded === variant) return;
	if (wasmReady && wasmVariantLoaded !== variant) {
		// Different variant — reset state for re-initialization
		wasmReady = false;
		threadPoolInitialized = false;
		wasmApi = null;
	}
	try {
		if (variant === "wasm64") {
			wasmApi = await import(/* @vite-ignore */ WASM64_URL);
		} else {
			wasmApi = await import("./wasm/ifc2lbd_wasm.js");
		}
		await wasmApi.default();
	} catch (err) {
		// wasm64 module may not be available (build failed or browser
		// doesn't support memory64). Fall back to wasm32.
		if (variant === "wasm64") {
			console.error("[worker] wasm64 load failed, falling back to wasm32:", err);
			variant = "wasm32";
			wasmApi = await import("./wasm/ifc2lbd_wasm.js");
			await wasmApi.default();
		} else {
			throw err;
		}
	}
	wasmReady = true;
	wasmVariantLoaded = variant;
};

const ensureThreadPool = async (requestedThreads) => {
	const threads = Math.max(2, Number(requestedThreads || 4));
	if (threadPoolInitialized) return { threads: threadPoolSize, reused: true };
	await withTimeout(
		wasmApi.initNeoThreadPool(threads),
		8000,
		"initNeoThreadPool timeout",
	);
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
		const variant = payload?.wasmVariant || "wasm32";

		self.postMessage({
			id,
			type: "status",
			phase: "init-wasm",
			status: "start",
		});
		await ensureWasm(variant);
		self.postMessage({
			id,
			type: "status",
			phase: "init-wasm",
			status: "done",
		});
		self.postMessage({
			id,
			type: "status",
			phase: "init-threadpool",
			status: "start",
		});
		const threadInfo = await ensureThreadPool(payload?.requestedThreads);
		self.postMessage({
			id,
			type: "status",
			phase: "init-threadpool",
			status: "done",
		});
		self.postMessage({
			id,
			type: "threadpool",
			threads: threadInfo.threads,
			reused: threadInfo.reused,
		});

		const input = new Uint8Array(payload.inputBuffer);
		const request = payload.request || {};

		// Pass inputFormat and structured data file metadata to the WASM converter
		// so the runner knows whether to parse IFC or structured data.
		if (payload.inputFormat) {
			request.inputFormat = payload.inputFormat;
		}
		if (payload.structuredDataFiles) {
			request.structuredDataFiles = payload.structuredDataFiles;
		}

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
			if (
				sinkEvent.type === "fileChunk" &&
				sinkEvent.filename &&
				sinkEvent.chunk
			) {
				const chunk =
					sinkEvent.chunk instanceof Uint8Array
						? sinkEvent.chunk
						: new Uint8Array(sinkEvent.chunk);
				const chunkCopy = chunk.slice();
				self.postMessage(
					{ id, type: "chunk", filename: sinkEvent.filename, chunk: chunkCopy },
					[chunkCopy.buffer],
				);
				return;
			}
		};

		self.postMessage({ id, type: "status", phase: "convert", status: "start" });
		const streamResult = await withTimeout(
			Promise.resolve().then(() =>
				wasmApi.convertIfcToSink(input, request, sink),
			),
			20000,
			"convertIfcToSink timeout",
		);
		self.postMessage({ id, type: "status", phase: "convert", status: "done" });
		self.postMessage({ id, type: "done", streamResult });
	} catch (error) {
		self.postMessage({
			id,
			type: "error",
			error: error instanceof Error ? error.message : String(error),
		});
	}
});
