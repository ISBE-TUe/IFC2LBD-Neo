// ---------------------------------------------------------------------------
// app.js — Pipeline dashboard (Ableton Session View style)
// ---------------------------------------------------------------------------

// WASM imports are dynamic (lazy) so they don't crash in Electron's file://
// protocol. In Electron mode, these are never loaded at all.
let initWasm,
	listModules,
	resolvePlan,
	planExecution,
	convertIfcToSink,
	initNeoThreadPool;

async function loadWasmModule() {
	const mod = await import("../wasm/ifc2lbd_wasm.js");
	initWasm = mod.default;
	listModules = mod.listModules;
	resolvePlan = mod.resolvePlan;
	planExecution = mod.planExecution;
	convertIfcToSink = mod.convertIfcToSink;
	initNeoThreadPool = mod.initNeoThreadPool;
}

import {
	MODULES as FALLBACK_MODULES,
	resolvePlanStatic,
} from "./module-metadata.js";
import "./pipeline.css";
import {
	getState,
	getBackendModuleIds,
	update,
	updateStageStatus,
	resetStageStatuses,
	subscribe,
} from "./state.js";
import { initSession } from "./session.js";
import { initSidebar } from "./sidebar.js";
import { initLogPanel, log } from "./log-panel.js";
import { saveConfig, loadConfig } from "./config.js";
import { showCliCommand } from "./cli-command.js";

const RUNTIME_BUILD = __BUILD_VERSION__;

// Detect browser support for WASM memory64 (> 4 GiB linear memory).
// Requires: Chrome 133+, Firefox 134+, Edge 133+. Safari: not supported.
const detectWasm64Support = () => {
	try {
		// Test creating a shared i64 memory — requires cross-origin isolation
		// (COOP/COEP headers, already configured in vite.config.js).
		new WebAssembly.Memory({
			address: "i64",
			initial: 1n,
			maximum: 256n,
			shared: true,
		});
		return true;
	} catch {
		return false;
	}
};

const WASM64_SUPPORTED = detectWasm64Support();
// Env-var gate: VITE_ENABLE_WASM64=true opts into the wasm64 build.
// Default (unset / anything other than "true") = wasm64 disabled — the build
// script skips the slow wasm64 compilation and the UI shows a warning popup
// for large files instead of silently using wasm64.
const WASM64_ENABLED = import.meta.env.VITE_ENABLE_WASM64 === "true";
const WASM64_AVAILABLE = WASM64_ENABLED && WASM64_SUPPORTED;
const WASM32_HARD_CAP_MB = 4096;
const WASM64_HARD_CAP_MB = 14336; // 14 GiB, leaving ~2 GiB headroom under the 16 GiB browser limit

// ---------------------------------------------------------------------------
// Pipeline Templates
// ---------------------------------------------------------------------------

const PREPROCESS_MODULES = ["neo-cleanup-preprocess", "neo-qto-preprocess"];
const LBD_MODULES = [
	"neo-bot-producer",
	"neo-beo-producer",
	"neo-bsdd-producer",
	"neo-omg-fog",
];
const GEO_MODULES = ["neo-geometry-preprocess", "neo-geometry-producer"];

const TURTLE_OPTS = {
	"neo-turtle-serializer": { grouping: "streaming", layout: "joined" },
	"neo-file-export": { compress: "gzip" },
	"neo-bsdd-producer": { compact: "true", dedup_properties: "true" },
};

const TEMPLATES = [
	{
		id: "default",
		label: "Default",
		desc: "Standard producers, Turtle output",
		modules: [
			...PREPROCESS_MODULES,
			...LBD_MODULES,
			"neo-turtle-serializer",
			"neo-file-export",
		],
		options: { ...TURTLE_OPTS },
	},
	{
		id: "default-ifcowl",
		label: "Default + IfcOWL",
		desc: "Standard producers + IfcOWL, Turtle output",
		modules: [
			...PREPROCESS_MODULES,
			...LBD_MODULES,
			"neo-ifcowl-producer",
			"neo-turtle-serializer",
			"neo-file-export",
		],
		options: { ...TURTLE_OPTS },
	},
	{
		id: "default-geometry",
		label: "Default + Geometry",
		desc: "Default + geometry preprocess & producer, Turtle output",
		modules: [
			...PREPROCESS_MODULES,
			...GEO_MODULES,
			...LBD_MODULES,
			"neo-turtle-serializer",
			"neo-file-export",
		],
		options: { ...TURTLE_OPTS },
	},
];

/** Resolve the output stem: use the user-set value, or derive from the IFC filename. */
function resolveOutputStem() {
	const { outputStem, ifcFile } = getState();
	if (outputStem) return outputStem;
	if (ifcFile) return ifcFile.name.replace(/\.[^.]+$/, "");
	return "converted-model";
}

function applyTemplate(templateId) {
	const tpl = TEMPLATES.find((t) => t.id === templateId);
	if (!tpl) return;
	update({
		activeModules: new Set(tpl.modules),
		moduleOptions: { ...tpl.options },
	});
	log(`Template: ${tpl.label}`);
	// Keep the dropdown showing the selected preset
	const picker = document.querySelector("#template-picker");
	if (picker) picker.value = templateId;
}

/** Sync the template dropdown to reflect the current module selection.
 *  If active modules match a template, show it; otherwise show Custom. */
function syncTemplatePicker() {
	const picker = document.querySelector("#template-picker");
	if (!picker) return;
	const { activeModules } = getState();
	const match = TEMPLATES.find(
		(t) =>
			t.modules.length === activeModules.size &&
			t.modules.every((m) => activeModules.has(m)),
	);
	picker.value = match ? match.id : "";
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
const supportsOutputDirectoryPicker =
	typeof window.showDirectoryPicker === "function";
let asciiBgCanvas = null;
let asciiBgCtx = null;
let asciiBgAnimId = 0;
let asciiBgResizeObserver = null;
let asciiBgHealthTimer = 0;
let asciiGraphNodes = [];
let asciiMouseX = -1;
let asciiMouseY = -1;
let asciiToneState = new Float32Array(0);
let asciiToneCols = 0;
let asciiToneRows = 0;
const asciiCfg = {
	speed: 30,
	charSpeed: 60,
	scale: 8,
	opacity: 50,
	darkness: 100,
	nodeCount: 50,
	range: 500,
	dot: 8.0,
	lineWidth: 4.0,
	mouseFactor: 0.9,
	worldPadPct: 25.0,
	step: 6,
	smoothing: 1.0,
	holdBand: 4.0,
	threshold: 0.0,
	thresholdFade: 0.16,
	thresholdGray: 90,
	invert: false,
};

const detectFeasibilityBudgetMb = () => {
	const gb = Number(navigator.deviceMemory || 0);
	if (!Number.isFinite(gb) || gb <= 0) return undefined;
	return Math.max(512, Math.floor(gb * 1024 * 0.55));
};

function getConversionWorker() {
	if (conversionWorker) return conversionWorker;
	conversionWorker = new Worker(
		new URL("../wasm-lowmem-worker.js", import.meta.url),
		{ type: "module" },
	);
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
			const icon =
				data.status === "success" ? "✓" : data.status === "failed" ? "✗" : "→";
			log(
				`${icon} ${data.pluginId}: ${data.status}${data.durationMs ? ` ${(data.durationMs / 1000).toFixed(2)}s` : ""}${data.triplesOut ? ` (${data.triplesOut.toLocaleString()} triples)` : ""}`,
			);
			return;
		}

		if (data.type === "chunk") {
			if (!data.filename || data.chunk == null) return;
			const chunk =
				data.chunk instanceof Uint8Array
					? data.chunk
					: new Uint8Array(data.chunk);
			accumulateChunk(pending.memoryFiles, data.filename, chunk);
			return;
		}

		if (data.type === "done") {
			const renderedFiles = [];
			const knownFilenames = new Set(
				pending.expectedFiles.map((m) => m.filename),
			);
			for (const meta of pending.expectedFiles) {
				renderedFiles.push({
					filename: meta.filename,
					mimeType: meta.mimeType,
					role: meta.role,
					payloadParts: finalizeChunks(pending.memoryFiles, meta.filename),
				});
			}
			for (const [filename] of pending.memoryFiles.entries()) {
				if (!knownFilenames.has(filename)) {
					const isManifest = filename.endsWith(".manifest.json");
					const isNq = filename.endsWith(".nq");
					renderedFiles.push({
						filename,
						mimeType: isManifest
							? "application/json"
							: isNq
								? "application/n-quads"
								: "application/octet-stream",
						role: isManifest ? "manifest" : isNq ? "chunk" : "other",
						payloadParts: finalizeChunks(pending.memoryFiles, filename),
					});
				}
			}
			pendingConversionRequests.delete(data.id);
			pending.resolve({
				streamResult: data.streamResult || {},
				renderedFiles,
				threadPoolSize: pending.threadPoolSize,
			});
			return;
		}

		if (data.type === "error") {
			pendingConversionRequests.delete(data.id);
			pending.reject(new Error(data.error || "Worker conversion failed."));
		}
	});
	return conversionWorker;
}

function runSinkConversionInWorker(
	input,
	requestPayload,
	expectedFiles,
	requestedThreads,
	wasmVariant = "wasm32",
) {
	return new Promise((resolve, reject) => {
		const worker = getConversionWorker();
		const id = `conv-${++conversionRequestId}`;
		const inputCopy = input.slice();
		pendingConversionRequests.set(id, {
			resolve,
			reject,
			expectedFiles,
			memoryFiles: new Map(),
			threadPoolSize: requestedThreads,
			threadPoolLogged: false,
		});
		worker.postMessage(
			{
				id,
				type: "convert",
				payload: {
					inputBuffer: inputCopy.buffer,
					request: requestPayload,
					requestedThreads,
					wasmVariant,
					inputFormat: requestPayload.inputFormat,
					structuredDataFiles: requestPayload.structuredDataFiles,
				},
			},
			[inputCopy.buffer],
		);
	});
}

async function runSinkConversionInMain(
	input,
	requestPayload,
	expectedFiles,
	requestedThreads,
) {
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
			const icon =
				event.status === "success"
					? "✓"
					: event.status === "failed"
						? "✗"
						: "→";
			log(
				`${icon} ${event.pluginId}: ${event.status}${event.durationMs ? ` ${(event.durationMs / 1000).toFixed(2)}s` : ""}${event.triplesOut ? ` (${event.triplesOut.toLocaleString()} triples)` : ""}`,
			);
			return;
		}
		if (event.type === "fileChunk" && event.filename && event.chunk != null) {
			const chunk =
				event.chunk instanceof Uint8Array
					? event.chunk
					: new Uint8Array(event.chunk);
			accumulateChunk(memoryFiles, event.filename, chunk);
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
			payloadParts: finalizeChunks(memoryFiles, meta.filename),
		});
	}
	for (const [filename] of memoryFiles.entries()) {
		if (!knownFilenames.has(filename)) {
			const isManifest = filename.endsWith(".manifest.json");
			const isNq = filename.endsWith(".nq");
			renderedFiles.push({
				filename,
				mimeType: isManifest
					? "application/json"
					: isNq
						? "application/n-quads"
						: "application/octet-stream",
				role: isManifest ? "manifest" : isNq ? "chunk" : "other",
				payloadParts: finalizeChunks(memoryFiles, filename),
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
	// In Electron, skip WASM loading — the native CLI is used instead.
	// We still need the module list (from the WASM bindings) for the UI grid.
	// The module metadata is hardcoded as a fallback.
	const isElectron = window.electronAPI?.isElectron;

	if (!isElectron) {
		if (!window.isSecureContext || !window.crossOriginIsolated) {
			document.querySelector("#wasm-loading")?.remove();
			document
				.querySelector(".pipeline-app")
				?.style.setProperty("visibility", "visible");
			throw new Error(
				"Requires secure+isolated context. Open http://localhost:3031 or use HTTPS with COOP/COEP.",
			);
		}

		await loadWasmModule();
		await initWasm();
	}

	const HIDDEN_MODULES = new Set([
		"neo-ifc-topology-producer",
		"neo-bbox-enricher",
		"neo-topology-full-producer",
	]);

	// In Electron mode, use the static module list instead of WASM.
	const rawModules = isElectron ? FALLBACK_MODULES : listModules();
	const modules = rawModules.filter((m) => !HIDDEN_MODULES.has(m.id));
	update({ modules });

	// WASM and modules ready — remove spinner, reveal app
	document.querySelector("#wasm-loading")?.remove();
	document
		.querySelector(".pipeline-app")
		?.style.setProperty("visibility", "visible");
	requestAnimationFrame(() =>
		requestAnimationFrame(() => document.body.classList.remove("preload")),
	);

	const sessionArea = document.querySelector("#session-area");
	if (!sessionArea) throw new Error("Missing #session-area");
	let sessionMount = sessionArea.querySelector("#session-content");
	if (!sessionMount) {
		sessionMount = document.createElement("div");
		sessionMount.id = "session-content";
		sessionArea.appendChild(sessionMount);
	}
	initSession(sessionMount);
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
			}
		});
	}

	// Sync template dropdown when modules change manually (toggle circles)
	subscribe("activeModules", () => syncTemplatePicker());
	syncTemplatePicker(); // initial sync — default state matches a preset

	// Wire left rail: file input + settings
	document.querySelector("#file-input")?.addEventListener("change", (e) => {
		const file = e.target.files?.[0];
		if (!file) return;
		update({
			ifcFile: file,
			ifcFileBytes: null,
			inputFormat: "ifc",
			structuredDataFiles: null,
		});
		// Update rail UI
		const btn = document.querySelector("#rail-file-btn");
		if (btn) btn.classList.add("has-file");
		const text = document.querySelector("#rail-file-text");
		if (text) text.textContent = file.name;
		const meta = document.querySelector("#rail-file-meta");
		if (meta) meta.textContent = bytesToHuman(file.size);
		// Update Model Name placeholder to show stripped filename
		const stemInput = document.querySelector("#output-stem-input");
		if (stemInput) {
			const baseName = file.name.replace(/\.[^.]+$/, "");
			stemInput.placeholder = `(auto: ${baseName})`;
		}
		log(`File: ${file.name} (${bytesToHuman(file.size)})`);
	});

	document
		.querySelector("#btn-output-dir")
		?.addEventListener("click", async () => {
			if (!supportsOutputDirectoryPicker) {
				log("Output directory picker is not supported in this browser.");
				return;
			}
			try {
				const dirHandle = await window.showDirectoryPicker({
					mode: "readwrite",
				});
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
					log(
						`Output directory error: ${error instanceof Error ? error.message : String(error)}`,
					);
				}
			}
		});
	document
		.querySelector("#btn-output-dir-clear")
		?.addEventListener("click", () => {
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
	document
		.querySelector("#base-uri-input")
		?.addEventListener("change", (e) =>
			update({ baseUri: e.target.value.trim() }),
		);
	document
		.querySelector("#output-stem-input")
		?.addEventListener("change", (e) =>
			update({ outputStem: e.target.value.trim() }),
		);
	document
		.querySelector("#toggle-preprocess")
		?.addEventListener("change", (e) =>
			update({ showPreprocess: e.target.checked }),
		);
	document
		.querySelector("#toggle-postprocess")
		?.addEventListener("change", (e) =>
			update({ showPostprocess: e.target.checked }),
		);

	// Compressed toggle — layers bSDD compact+dedup and gzip export on top of current state
	document
		.querySelector("#toggle-compressed")
		?.addEventListener("change", (e) => {
			const { moduleOptions } = getState();
			const opts = JSON.parse(JSON.stringify(moduleOptions));
			if (e.target.checked) {
				opts["neo-bsdd-producer"] = {
					...(opts["neo-bsdd-producer"] || {}),
					compact: "true",
					dedup_properties: "true",
				};
				opts["neo-file-export"] = {
					...(opts["neo-file-export"] || {}),
					compress: "gzip",
				};
			} else {
				opts["neo-bsdd-producer"] = {
					...(opts["neo-bsdd-producer"] || {}),
					compact: "false",
					dedup_properties: "false",
				};
				opts["neo-file-export"] = {
					...(opts["neo-file-export"] || {}),
					compress: "none",
				};
			}
			update({ moduleOptions: opts });
			log(
				e.target.checked
					? "Compressed mode: bSDD compact+dedup, gzip export"
					: "Compressed mode off",
			);
		});
	const { showPreprocess, showPostprocess } = getState();
	const preEl = document.querySelector("#toggle-preprocess");
	const postEl = document.querySelector("#toggle-postprocess");
	if (preEl) preEl.checked = showPreprocess;
	if (postEl) postEl.checked = showPostprocess;
	document.querySelector("#btn-load")?.addEventListener("click", loadConfig);
	document.querySelector("#btn-save")?.addEventListener("click", saveConfig);
	document
		.querySelector("#btn-cli-cmd")
		?.addEventListener("click", showCliCommand);
	document.querySelector("#btn-viewer")?.addEventListener("click", () => {
		if (window.electronAPI?.isElectron) {
			window.electronAPI.openViewer();
		} else {
			window.open(
				"https://viewer-ifc2lbd-neo.pages.dev/",
				"_blank",
				"noopener,noreferrer",
			);
		}
	});
	document.querySelector("#btn-run")?.addEventListener("click", runConversion);
	initMusic();
	setupOutputDirectoryUiSupport();
	initCiteWidget();

	if (isElectron) {
		log(`Native CLI ready. Pipeline dashboard (Electron mode).`);
		log(`Build: ${RUNTIME_BUILD}`);
	} else {
		log(`WASM ready. Pipeline dashboard.`);
		log(`Build: ${RUNTIME_BUILD}`);
		log(
			`WASM: ${WASM64_AVAILABLE ? "wasm64 (16 GiB limit) + wasm32 (4 GiB limit)" : "wasm32 (4 GiB limit)"}${WASM64_AVAILABLE ? "" : WASM64_ENABLED ? " — wasm64 not supported in this browser" : " — wasm64 disabled (VITE_ENABLE_WASM64 not set)"}`,
		);
	}
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/**
 * Run conversion via the Electron sidecar (native CLI).
 *
 * Instead of loading WASM and running in a Web Worker, this path:
 *   1. Asks the main process to open a file dialog (if no file is loaded)
 *   2. Sends the module config to the main process via IPC
 *   3. The main process spawns ifc2lbd-neo, writes output to a temp dir
 *   4. Stage events arrive via IPC and update the UI (same as WASM path)
 *   5. On completion, output files are read back and rendered as downloads
 *
 * The CLI runs with full native threading (rayon) and no memory limits.
 */
async function runConversionElectron() {
	const state = getState();

	// Check for input
	if (state.inputFormat === "structured-data") {
		if (!state.structuredDataFiles || !state.structuredDataFiles.length) {
			log("No structured data files selected.");
			return;
		}
	} else {
		if (!state.ifcFile) {
			// In Electron, prompt the user to select a file via the native dialog
			const result = await window.electronAPI.openFile();
			if (!result) {
				log("No file selected.");
				return;
			}
			// Create a File-like object from the native file data
			const file = new File([result.content], result.name, {
				type: "application/octet-stream",
			});
			update({
				ifcFile: file,
				ifcFileBytes: null,
				inputFormat: "ifc",
			});
			const btn = document.querySelector("#rail-file-btn");
			if (btn) btn.classList.add("has-file");
			const text = document.querySelector("#rail-file-text");
			if (text) text.textContent = result.name;
			const meta = document.querySelector("#rail-file-meta");
			if (meta) meta.textContent = bytesToHuman(result.size);
			// Update Model Name placeholder to show stripped filename
			const stemInput = document.querySelector("#output-stem-input");
			if (stemInput) {
				const baseName = result.name.replace(/\.[^.]+$/, "");
				stemInput.placeholder = `(auto: ${baseName})`;
			}
			log(`File: ${result.name} (${bytesToHuman(result.size)})`);
		}
	}

	update({ running: true });
	const runBtn = document.querySelector("#btn-run");
	if (runBtn) {
		runBtn.disabled = true;
		runBtn.textContent = "◉ RUNNING";
		runBtn.classList.add("running");
	}
	const infoElReset = document.querySelector("#runtime-info");
	if (infoElReset) infoElReset.innerHTML = "";

	try {
		const { activeModules, moduleOptions, baseUri, ifcFile } = getState();
		const outputStem = resolveOutputStem();

		// Build module options array (same format as CLI --module-opt)
		const moduleOptionsArr = [];
		for (const [pluginId, opts] of Object.entries(moduleOptions)) {
			if (!activeModules.has(pluginId)) continue;
			for (const [key, value] of Object.entries(opts)) {
				if (value) moduleOptionsArr.push(`${pluginId}.${key}=${value}`);
			}
		}

		// For the CLI, we need the file on disk. In Electron's sandboxed
		// renderer, File.path is not available, so we send the file bytes
		// via IPC and the main process writes them to a temp file.
		const fileData = await ifcFile.arrayBuffer();

		const plan = resolvePlanStatic(getBackendModuleIds(), moduleOptionsArr);
		log(`Plan: ${plan.enabledIds.join(", ")}`);
		log(`Running via native CLI (Electron sidecar)...`);

		resetStageStatuses();

		// Subscribe to progress events from the main process
		const logUnsub = window.electronAPI.onConversionLog((line) => {
			log(line);
		});
		const stageUnsub = window.electronAPI.onStageEvent((event) => {
			updateStageStatus(event);
			const icon =
				event.status === "success"
					? "✓"
					: event.status === "failed"
						? "✗"
						: "→";
			log(
				`${icon} ${event.pluginId}: ${event.status}${event.durationMs ? ` ${(event.durationMs / 1000).toFixed(2)}s` : ""}${event.triplesOut ? ` (${event.triplesOut.toLocaleString()} triples)` : ""}`,
			);
		});

		const t0 = performance.now();

		// Determine output extension from active serializer
		const hasNquads =
			activeModules.has("neo-nquads-serializer") ||
			activeModules.has("neo-nquads-chunked-serializer");
		const outputExt = hasNquads ? ".nq" : ".ttl";

		// Read structured data file bytes if present (both IFC + structured data can coexist)
		const structuredDataFiles = [];
		if (state.structuredDataFiles?.length) {
			for (const file of state.structureduredDataFiles) {
				const data = await file.arrayBuffer();
				structuredDataFiles.push({ name: file.name, data });
			}
		}

		const result = await window.electronAPI.runConversion({
			fileName: ifcFile.name,
			fileData,
			modules: getBackendModuleIds(),
			moduleOptions: moduleOptionsArr,
			baseUri,
			outputStem,
			outputExt,
			inputFormat: state.inputFormat,
			structuredDataFiles,
		});

		// Unsubscribe from progress events
		logUnsub();
		stageUnsub();

		// result contains { tempDir, files: [{ filename, mimeType, size }] }
		// Show output files as clickable links in the downloads bar — same
		// styling as the web app. Clicking opens a native Save File dialog.
		const container = document.querySelector("#downloads");
		if (container) {
			container.innerHTML = "";
			for (const file of result.files || []) {
				const link = document.createElement("a");
				link.className = "download-link";
				link.textContent = "";
				const icon = document.createElement("span");
				icon.className = "download-icon";
				icon.textContent = "▼";
				link.appendChild(icon);
				const name = document.createElement("span");
				name.textContent = file.filename;
				link.appendChild(name);
				const sizeBadge = document.createElement("span");
				sizeBadge.className = "download-size";
				sizeBadge.textContent = bytesToHuman(file.size);
				link.appendChild(sizeBadge);
				link.href = "#";
				link.addEventListener("click", async (e) => {
					e.preventDefault();
					const savePath = await window.electronAPI.showSaveDialog(
						file.filename,
					);
					if (!savePath) return;
					await window.electronAPI.saveOutputFile(
						result.tempDir,
						file.filename,
						savePath,
					);
					log(`Saved ${file.filename} to ${savePath}`);
				});
				container.appendChild(link);
			}
		}
		log(
			`Output: ${(result.files || []).map((f) => `${f.filename} (${bytesToHuman(f.size)})`).join(", ")}`,
		);

		const elapsedMs = performance.now() - t0;
		const timeStr = `${(elapsedMs / 1000).toFixed(1)}s`;
		log(`Finished in ${timeStr}.`);

		const infoEl = document.querySelector("#runtime-info");
		if (infoEl)
			infoEl.innerHTML = `<span style="color:var(--status-success);font-weight:600">Finished in ${timeStr}</span>`;
	} catch (error) {
		log(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
		const { stageStatuses } = getState();
		const updated = { ...stageStatuses };
		for (const [id, s] of Object.entries(updated)) {
			if (s.status === "running")
				updated[id] = { ...s, status: "failed", error: error.message };
		}
		update({ stageStatuses: updated });
		const infoEl = document.querySelector("#runtime-info");
		if (infoEl)
			infoEl.innerHTML = `<span style="color:var(--status-failed);font-weight:600">Failed: ${escapeHtml(error instanceof Error ? error.message : String(error))}</span>`;
	} finally {
		update({ running: false });
		const runBtn = document.querySelector("#btn-run");
		if (runBtn) {
			runBtn.disabled = false;
			runBtn.textContent = "RUN";
			runBtn.classList.remove("running");
		}
	}
}

async function runConversion() {
	const state = getState();
	if (state.running) return;

	// ── Electron sidecar path ──────────────────────────────────────────────
	// When running inside Electron, use the native CLI instead of WASM.
	// The CLI runs as a separate process with full native threading (rayon)
	// and no memory limits.
	if (window.electronAPI?.isElectron) {
		return runConversionElectron();
	}

	// ── Browser WASM path (existing) ─────────────────────────────────────────

	// Check for input
	if (state.inputFormat === "structured-data") {
		if (!state.structuredDataFiles || !state.structuredDataFiles.length) {
			log("No structured data files selected.");
			return;
		}
	} else {
		if (!state.ifcFile) {
			log("No file selected.");
			return;
		}
	}

	update({ running: true });
	const runBtn = document.querySelector("#btn-run");
	if (runBtn) {
		runBtn.disabled = true;
		runBtn.textContent = "◉ RUNNING";
		runBtn.classList.add("running");
	}
	const infoElReset = document.querySelector("#runtime-info");
	if (infoElReset) infoElReset.innerHTML = "";

	try {
		const { activeModules, moduleOptions, baseUri } = getState();
		const outputStem = resolveOutputStem();
		const moduleIds = getBackendModuleIds();
		const moduleOptionsArr = [];
		for (const [pluginId, opts] of Object.entries(moduleOptions)) {
			if (!activeModules.has(pluginId)) continue;
			for (const [key, value] of Object.entries(opts)) {
				if (value) moduleOptionsArr.push(`${pluginId}.${key}=${value}`);
			}
		}

		let input;
		let requestPayload;

		if (state.inputFormat === "structured-data") {
			// Read all structured data files
			const files = getState().structuredDataFiles;
			const fileBuffers = [];
			for (const file of files) {
				const bytes = new Uint8Array(await file.arrayBuffer());
				fileBuffers.push({ name: file.name, data: bytes });
			}
			// For planExecution: use total size
			const totalBytes = fileBuffers.reduce(
				(sum, f) => sum + f.data.byteLength,
				0,
			);
			input = fileBuffers[0].data; // first buffer for compatibility
			requestPayload = {
				moduleIds: getBackendModuleIds(),
				moduleOptions: moduleOptionsArr,
				baseUri,
				outputStem,
				executionMode: "auto",
				inputFormat: "structured-data",
				structuredDataFiles: fileBuffers.map((f) => ({
					name: f.name,
					size: f.data.byteLength,
				})),
			};
			// Note: actual file bytes will be sent separately via transferables
		} else {
			// Existing IFC path
			const ifcFile = getState().ifcFile;
			const ifcFileBytes = new Uint8Array(await ifcFile.arrayBuffer());
			input = ifcFileBytes;
			requestPayload = {
				moduleIds: getBackendModuleIds(),
				moduleOptions: moduleOptionsArr,
				baseUri,
				outputStem,
				executionMode: "auto",
			};
		}
		const plan = resolvePlan(moduleIds, moduleOptionsArr);
		log(`Plan: ${plan.enabledIds.join(", ")}`);

		const feasibilityMb = detectFeasibilityBudgetMb();
		if (feasibilityMb) requestPayload.memoryFeasibilityMb = feasibilityMb;
		const executionPlan = planExecution(input.byteLength, requestPayload);
		log(
			`Mode=${executionPlan.selectedMode} est=${executionPlan.estimatedPeakMb}MB`,
		);

		// Pre-flight memory check: choose wasm32 (fast) or wasm64 (large files).
		const needsWasm64 = executionPlan.estimatedPeakMb > WASM32_HARD_CAP_MB;
		const useWasm64 = needsWasm64 && WASM64_AVAILABLE;

		if (needsWasm64) {
			if (!WASM64_ENABLED) {
				// wasm64 is disabled (env var not set) — warn the user that the
				// browser may crash and let them choose to abort or proceed.
				log(
					`Estimated peak memory (${executionPlan.estimatedPeakMb} MB) exceeds the wasm32 limit (${WASM32_HARD_CAP_MB} MB). wasm64 is disabled — showing warning.`,
				);
				const proceed = await showMemoryWarning();
				if (!proceed) {
					log("Conversion aborted by user (large file warning).");
					return;
				}
				// User chose "Continue anyway" — proceed with wasm32 (may crash).
				log("User chose to continue with wasm32 despite memory risk.");
			} else if (!WASM64_SUPPORTED) {
				throw new Error(
					`Estimated peak memory (${executionPlan.estimatedPeakMb} MB) exceeds the wasm32 limit (${WASM32_HARD_CAP_MB} MB). This browser does not support wasm64 (memory64). Use the CLI for files this large.`,
				);
			} else if (executionPlan.estimatedPeakMb > WASM64_HARD_CAP_MB) {
				throw new Error(
					`Estimated peak memory (${executionPlan.estimatedPeakMb} MB) exceeds even the wasm64 limit (${WASM64_HARD_CAP_MB} MB). Use the CLI for files this large.`,
				);
			}
		}

		if (useWasm64) {
			log(
				`Using wasm64 (memory64) for large file: ${executionPlan.estimatedPeakMb} MB estimated peak`,
			);
		}

		const requestedThreads = Math.max(
			2,
			Number(navigator.hardwareConcurrency || 4),
		);
		const hasNq =
			activeModules.has("neo-nquads-serializer") ||
			activeModules.has("neo-nquads-chunked-serializer");
		const hasChunkedNq = activeModules.has("neo-nquads-chunked-serializer");
		const hasIfcowl = activeModules.has("neo-ifcowl-producer");
		const turtleLayout =
			moduleOptions["neo-turtle-serializer"]?.layout || "joined";
		// For chunked output, filenames are dynamic (out-lbd.part-000.nq, etc.)
		// so expectedFiles only lists the single-file outputs.
		// The done handler will pick up chunked files from memoryFiles automatically.
		const expectedFiles = hasChunkedNq
			? [] // chunked files are discovered from sink events
			: hasNq
				? [
						{
							filename: `${outputStem}.nq`,
							mimeType: "application/n-quads",
							role: "merged",
						},
					]
				: turtleLayout === "separate"
					? [
							...(activeModules.has("neo-bot-producer")
								? [
										{
											filename: `${outputStem}_bot.ttl`,
											mimeType: "text/turtle",
											role: "bot",
										},
									]
								: []),
							...(activeModules.has("neo-beo-producer")
								? [
										{
											filename: `${outputStem}_beo.ttl`,
											mimeType: "text/turtle",
											role: "beo",
										},
									]
								: []),
							...(activeModules.has("neo-props-opm")
								? [
										{
											filename: `${outputStem}_props.ttl`,
											mimeType: "text/turtle",
											role: "props",
										},
									]
								: []),
							...(activeModules.has("neo-bsdd-producer")
								? [
										{
											filename: `${outputStem}_bsdd.ttl`,
											mimeType: "text/turtle",
											role: "bsdd",
										},
									]
								: []),
							...(activeModules.has("neo-omg-fog")
								? [
										{
											filename: `${outputStem}_omg.ttl`,
											mimeType: "text/turtle",
											role: "omg",
										},
									]
								: []),
							...(activeModules.has("neo-ifcowl-producer")
								? [
										{
											filename: `${outputStem}_ifcowl.ttl`,
											mimeType: "text/turtle",
											role: "ifcowl",
										},
									]
								: []),
							...(activeModules.has("neo-ifc-topology-producer")
								? [
										{
											filename: `${outputStem}_topology.ttl`,
											mimeType: "text/turtle",
											role: "topology",
										},
									]
								: []),
						]
					: [
							{
								filename: `${outputStem}.ttl`,
								mimeType: "text/turtle",
								role: "joined",
							},
						];

		const t0 = performance.now();
		resetStageStatuses();
		const result = await runSinkConversionInWorker(
			input,
			requestPayload,
			expectedFiles,
			requestedThreads,
			useWasm64 ? "wasm64" : "wasm32",
		);

		// Geometry sidecar files (fragments/parquet/gltf) — returned as base64 in bundle
		const geoFiles = result.streamResult?.geometryFiles || [];
		const geoRendered = geoFiles.map((gf) => {
			const binaryStr = atob(gf.dataBase64);
			const bytes = new Uint8Array(binaryStr.length);
			for (let i = 0; i < binaryStr.length; i++)
				bytes[i] = binaryStr.charCodeAt(i);
			return {
				filename: gf.filename,
				mimeType: gf.mimeType,
				role: "geometry",
				payloadParts: [bytes],
			};
		});
		if (geoFiles.length > 0) {
			log(
				`Geometry output: ${geoFiles.map((g) => `${g.filename} (${(g.bytes / 1024).toFixed(0)} KB)`).join(", ")}`,
			);
		}

		const filesWithPayload = [...result.renderedFiles, ...geoRendered].filter(
			(f) => Array.isArray(f?.payloadParts) && f.payloadParts.length > 0,
		);
		if (!filesWithPayload.length) {
			throw new Error("Conversion finished but produced no output payloads.");
		}

		if (outputDirectoryHandle) {
			try {
				const writable = await ensureOutputDirectoryWritable(
					outputDirectoryHandle,
				);
				if (!writable) {
					throw new Error("Output directory permission denied.");
				}
				log(`→ neo-file-export: writing files...`);
				const { fileCount, totalBytes } = await writeRenderedFilesToDirectory(
					filesWithPayload,
					outputDirectoryHandle,
				);
				renderDownloadsMessage(
					`Saved ${fileCount} file(s), ${bytesToHuman(totalBytes)}, to ${outputDirectoryName}.`,
				);
				log(
					`Saved ${fileCount} file(s), ${bytesToHuman(totalBytes)}, to output directory.`,
				);
			} catch (error) {
				log(
					`Output write failed, falling back to downloads: ${error instanceof Error ? error.message : String(error)}`,
				);
				renderDownloads(filesWithPayload);
			}
		} else {
			renderDownloads(filesWithPayload);
		}
		const elapsedMs = performance.now() - t0;
		const timeStr = `${(elapsedMs / 1000).toFixed(1)}s`;
		log(`Finished in ${timeStr}.`);

		const infoEl = document.querySelector("#runtime-info");
		if (infoEl)
			infoEl.innerHTML = `<span style="color:var(--status-success);font-weight:600">Finished in ${timeStr}</span>`;
	} catch (error) {
		log(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
		const { stageStatuses } = getState();
		const updated = { ...stageStatuses };
		for (const [id, s] of Object.entries(updated)) {
			if (s.status === "running")
				updated[id] = { ...s, status: "failed", error: error.message };
		}
		update({ stageStatuses: updated });
	} finally {
		update({ running: false });
		if (runBtn) {
			runBtn.disabled = false;
			runBtn.textContent = "▶ RUN";
			runBtn.classList.remove("running");
		}
	}
}

// ---------------------------------------------------------------------------
// Memory warning modal — shown when a large file exceeds the wasm32 memory
// cap and wasm64 is disabled (VITE_ENABLE_WASM64 not set).  Lets the user
// abort or proceed with wasm32 despite the crash risk.
// ---------------------------------------------------------------------------

let memoryWarningWired = false;

function wireMemoryWarning() {
	if (memoryWarningWired) return;
	memoryWarningWired = true;
	const modal = document.querySelector("#memory-warning");
	if (!modal) return;
	modal
		.querySelector(".mem-warn-backdrop")
		?.addEventListener("click", () => resolveMemoryWarning(false));
	modal
		.querySelector("#mem-warn-close")
		?.addEventListener("click", () => resolveMemoryWarning(false));
	modal
		.querySelector("#mem-warn-abort")
		?.addEventListener("click", () => resolveMemoryWarning(false));
	modal
		.querySelector("#mem-warn-continue")
		?.addEventListener("click", () => resolveMemoryWarning(true));
}

let memoryWarningResolver = null;

function resolveMemoryWarning(result) {
	const modal = document.querySelector("#memory-warning");
	modal?.classList.remove("open");
	if (memoryWarningResolver) {
		const resolve = memoryWarningResolver;
		memoryWarningResolver = null;
		resolve(result);
	}
}

function showMemoryWarning() {
	wireMemoryWarning();
	const modal = document.querySelector("#memory-warning");
	if (!modal) {
		// Fallback: if the modal markup is missing, default to abort (safe).
		console.warn("Memory warning modal not found in DOM.");
		return Promise.resolve(false);
	}
	return new Promise((resolve) => {
		memoryWarningResolver = resolve;
		modal.classList.add("open");
	});
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// readFileAsBytes removed — file reading happens in session.js grid file input

function renderDownloads(files) {
	const container = document.querySelector("#downloads");
	if (!container) return;
	container.innerHTML = "";
	if (!files.length) {
		container.innerHTML = '<span class="downloads-empty">No files.</span>';
		return;
	}
	for (const file of files) {
		if (!Array.isArray(file.payloadParts)) continue;
		const blob = new Blob(file.payloadParts, { type: file.mimeType });
		const url = URL.createObjectURL(blob);
		const link = document.createElement("a");
		link.className = "download-link";
		link.href = url;
		link.download = file.filename;
		link.textContent = "";
		const icon = document.createElement("span");
		icon.className = "download-icon";
		icon.textContent = "▼";
		link.appendChild(icon);
		const name = document.createElement("span");
		name.textContent = file.filename;
		link.appendChild(name);
		const sizeBadge = document.createElement("span");
		sizeBadge.className = "download-size";
		sizeBadge.textContent = bytesToHuman(blob.size);
		link.appendChild(sizeBadge);
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
		const blob = new Blob(file.payloadParts, {
			type: file.mimeType || "application/octet-stream",
		});
		const fileHandle = await dirHandle.getFileHandle(file.filename, {
			create: true,
		});
		const writable = await fileHandle.createWritable();
		await writable.write(blob);
		await writable.close();
		fileCount += 1;
		totalBytes += blob.size;
	}
	return { fileCount, totalBytes };
}

// Coalesce every N raw Uint8Array chunks into a Blob as they arrive so that the
// final Blob construction at download time is cheap (Blob-from-Blobs is O(1) refs,
// not O(size) copying like Blob-from-Uint8Arrays).
const CHUNK_COALESCE_COUNT = 32;

function accumulateChunk(map, filename, chunk) {
	let entry = map.get(filename);
	if (!entry) {
		entry = { pending: [], blobs: [] };
		map.set(filename, entry);
	}
	entry.pending.push(chunk);
	if (entry.pending.length >= CHUNK_COALESCE_COUNT) {
		entry.blobs.push(new Blob(entry.pending));
		entry.pending = [];
	}
}

function finalizeChunks(map, filename) {
	const entry = map.get(filename);
	if (!entry) return [];
	if (Array.isArray(entry)) return entry;
	if (entry.pending.length > 0) {
		entry.blobs.push(new Blob(entry.pending));
		entry.pending = [];
	}
	return entry.blobs;
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
	if (
		typeof dirHandle.queryPermission !== "function" ||
		typeof dirHandle.requestPermission !== "function"
	) {
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
	asciiBgResizeObserver?.disconnect();
	asciiBgCanvas?.remove();
	asciiBgCanvas = document.createElement("canvas");
	asciiBgCanvas.className = "ascii-bg";
	area.prepend(asciiBgCanvas);
	asciiBgCtx = asciiBgCanvas.getContext("2d", { alpha: true });
	area.onmousemove = (ev) => {
		const rect = area.getBoundingClientRect();
		asciiMouseX = ev.clientX - rect.left;
		asciiMouseY = ev.clientY - rect.top;
	};
	area.onmouseleave = () => {
		asciiMouseX = -1;
		asciiMouseY = -1;
	};
	resizeAsciiBackground();
	window.addEventListener("resize", resizeAsciiBackground);
	if (typeof ResizeObserver !== "undefined") {
		asciiBgResizeObserver = new ResizeObserver(() => resizeAsciiBackground());
		asciiBgResizeObserver.observe(area);
	}
	// Defensive re-attach in case DOM updates remove/recreate session contents.
	if (asciiBgHealthTimer) clearInterval(asciiBgHealthTimer);
	asciiBgHealthTimer = setInterval(() => {
		const currentArea = document.querySelector("#session-area");
		if (!currentArea) return;
		if (!asciiBgCanvas || asciiBgCanvas.parentElement !== currentArea) {
			initAsciiBackground();
		}
	}, 1200);
	if (asciiBgAnimId) cancelAnimationFrame(asciiBgAnimId);
	const charsNormal =
		"   ..''`^,:;Il!i~+_-?][}{1)(|\\/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$";
	const charsInverted =
		"$@B%8&WM#*oahkbdpqwmZO0QLCJUYXzcvunxrjft/\\|)(1}{][?-_+~i!lI;:,^`''..   ";
	const graphCanvas = document.createElement("canvas");
	const graphCtx = graphCanvas.getContext("2d", { alpha: true });

	const resetGraph = () => {
		if (!asciiBgCanvas) return;
		const w = Math.max(
			1,
			Math.floor(asciiBgCanvas.width / (window.devicePixelRatio || 1)),
		);
		const h = Math.max(
			1,
			Math.floor(asciiBgCanvas.height / (window.devicePixelRatio || 1)),
		);
		graphCanvas.width = w;
		graphCanvas.height = h;
		const worldPadFrac = Math.max(0, asciiCfg.worldPadPct / 100);
		const worldPadX = w * worldPadFrac;
		const worldPadY = h * worldPadFrac;
		const worldMinX = -worldPadX;
		const worldMaxX = w + worldPadX;
		const worldMinY = -worldPadY;
		const worldMaxY = h + worldPadY;
		// Keep graph density stable; scale slider controls ASCII sampling only.
		const desired = Math.max(
			8,
			Math.round(asciiCfg.nodeCount * ((w * h) / (1280 * 720))),
		);
		asciiGraphNodes = [];
		for (let i = 0; i < desired; i += 1) {
			const dir = Math.random() * Math.PI * 2;
			const mag = 0.25 + Math.random() * 0.95;
			asciiGraphNodes.push({
				x: worldMinX + Math.random() * (worldMaxX - worldMinX),
				y: worldMinY + Math.random() * (worldMaxY - worldMinY),
				vx: Math.sin(dir) * mag,
				vy: Math.cos(dir) * mag,
			});
		}
	};
	resetGraph();
	const draw = (t) => {
		if (!asciiBgCtx || !asciiBgCanvas || !graphCtx) return;
		const w = Math.max(
			1,
			Math.floor(asciiBgCanvas.width / (window.devicePixelRatio || 1)),
		);
		const h = Math.max(
			1,
			Math.floor(asciiBgCanvas.height / (window.devicePixelRatio || 1)),
		);
		if (graphCanvas.width !== w || graphCanvas.height !== h) resetGraph();

		const baseOpacity = Math.max(0, Math.min(1, asciiCfg.opacity / 100));
		const darknessNorm = Math.max(0, Math.min(1, asciiCfg.darkness / 100));
		const speedMul = 0.03 + (asciiCfg.speed / 100) * 1.05;
		const dot = Math.max(0.1, asciiCfg.dot);
		const range = Math.max(1, asciiCfg.range);
		const mouseRange = Math.max(1, range * asciiCfg.mouseFactor);
		const worldPadFrac = Math.max(0, asciiCfg.worldPadPct / 100);
		const worldPadX = w * worldPadFrac;
		const worldPadY = h * worldPadFrac;
		const worldMinX = -worldPadX;
		const worldMaxX = w + worldPadX;
		const worldMinY = -worldPadY;
		const worldMaxY = h + worldPadY;

		// Update particle positions.
		for (let i = 0; i < asciiGraphNodes.length; i += 1) {
			const p = asciiGraphNodes[i];
			p.x += p.vx * speedMul;
			p.y += p.vy * speedMul;
			if (p.x > worldMaxX - dot / 2) {
				p.x = worldMaxX - dot / 2;
				p.vx = -Math.abs(p.vx);
			} else if (p.x < worldMinX + dot / 2) {
				p.x = worldMinX + dot / 2;
				p.vx = Math.abs(p.vx);
			}
			if (p.y > worldMaxY - dot / 2) {
				p.y = worldMaxY - dot / 2;
				p.vy = -Math.abs(p.vy);
			} else if (p.y < worldMinY + dot / 2) {
				p.y = worldMinY + dot / 2;
				p.vy = Math.abs(p.vy);
			}
		}

		// Render graph into an offscreen grayscale buffer.
		graphCtx.clearRect(0, 0, w, h);
		graphCtx.fillStyle = "#000";
		graphCtx.fillRect(0, 0, w, h);
		for (let i = 0; i < asciiGraphNodes.length; i += 1) {
			const a = asciiGraphNodes[i];
			for (let j = i + 1; j < asciiGraphNodes.length; j += 1) {
				const b = asciiGraphNodes[j];
				const dx = a.x - b.x;
				const dy = a.y - b.y;
				const d = Math.hypot(dx, dy);
				if (d >= range) continue;
				const m = 1 - d / range;
				const alpha = Math.max(0.05, m * m * (0.45 + baseOpacity * 0.95));
				graphCtx.strokeStyle = `rgba(255,255,255,${alpha.toFixed(3)})`;
				graphCtx.lineWidth = Math.max(0.05, asciiCfg.lineWidth);
				graphCtx.beginPath();
				graphCtx.moveTo(a.x, a.y);
				graphCtx.lineTo(b.x, b.y);
				graphCtx.stroke();
			}
			if (asciiMouseX >= 0 && asciiMouseY >= 0) {
				const md = Math.hypot(a.x - asciiMouseX, a.y - asciiMouseY);
				if (md < mouseRange) {
					const m = 1 - md / mouseRange;
					const alpha = Math.max(0.08, m * (0.35 + baseOpacity * 0.9));
					graphCtx.strokeStyle = `rgba(180,255,205,${alpha.toFixed(3)})`;
					graphCtx.lineWidth = Math.max(0.05, asciiCfg.lineWidth);
					graphCtx.beginPath();
					graphCtx.moveTo(a.x, a.y);
					graphCtx.lineTo(asciiMouseX, asciiMouseY);
					graphCtx.stroke();
				}
			}
			graphCtx.fillStyle = `rgba(255,255,255,${Math.max(0.22, 0.44 + baseOpacity * 0.4).toFixed(3)})`;
			graphCtx.beginPath();
			graphCtx.arc(a.x, a.y, dot, 0, Math.PI * 2);
			graphCtx.fill();
		}

		// ASCII-map the graph buffer onto visible canvas.
		asciiBgCtx.clearRect(0, 0, w, h);
		const step = Math.max(1, Math.round(asciiCfg.step));
		const fontPx = Math.max(3.4, 3 + asciiCfg.scale * 0.42);
		const chars = asciiCfg.invert ? charsInverted : charsNormal;
		const cols = Math.max(1, Math.ceil(w / step));
		const rows = Math.max(1, Math.ceil(h / step));
		if (
			cols !== asciiToneCols ||
			rows !== asciiToneRows ||
			asciiToneState.length !== cols * rows
		) {
			asciiToneCols = cols;
			asciiToneRows = rows;
			asciiToneState = new Float32Array(cols * rows);
		}
		asciiBgCtx.font = `${fontPx}px JetBrains Mono, monospace`;
		asciiBgCtx.textBaseline = "top";
		const img = graphCtx.getImageData(0, 0, w, h).data;
		const channel = 96;
		const charSpeedNorm = Math.max(0, Math.min(1, asciiCfg.charSpeed / 100));
		const smoothing =
			Math.max(0, Math.min(1, asciiCfg.smoothing)) *
			(0.2 + charSpeedNorm * 2.2);
		const holdBand = Math.max(0, asciiCfg.holdBand);
		let cell = 0;
		for (let y = 0; y < h; y += step) {
			for (let x = 0; x < w; x += step) {
				const idx = (y * w + x) * 4;
				const intensity = img[idx] / 255;
				const target = intensity * (chars.length - 1);
				const prev = asciiToneState[cell] || 0;
				const mixed = prev + (target - prev) * smoothing;
				let stable = mixed;
				if (Math.abs(target - prev) < holdBand) stable = prev;
				asciiToneState[cell] = stable;
				const n = Math.max(0, Math.min(chars.length - 1, Math.round(stable)));
				const ch = chars[n];
				const mainAlpha = Math.min(1, 0.72 + intensity * 0.28);
				if (intensity >= asciiCfg.threshold) {
					asciiBgCtx.fillStyle = `rgba(${channel},${channel},${channel},${mainAlpha.toFixed(3)})`;
					asciiBgCtx.fillText(ch, x, y);
				} else {
					const fadeAlpha = Math.max(
						0,
						Math.min(1, mainAlpha * asciiCfg.thresholdFade),
					);
					const gray = Math.max(
						0,
						Math.min(255, Math.round(asciiCfg.thresholdGray)),
					);
					if (fadeAlpha > 0.001) {
						asciiBgCtx.fillStyle = `rgba(${gray},${gray},${gray},${fadeAlpha.toFixed(3)})`;
						asciiBgCtx.fillText(ch, x, y);
					}
				}
				cell += 1;
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

function bindAsciiDebugControls() {
	const speed = document.querySelector("#ascii-speed");
	const speedNum = document.querySelector("#ascii-speed-num");
	const charSpeed = document.querySelector("#ascii-char-speed");
	const charSpeedNum = document.querySelector("#ascii-char-speed-num");
	const scale = document.querySelector("#ascii-scale");
	const scaleNum = document.querySelector("#ascii-scale-num");
	const opacity = document.querySelector("#ascii-opacity");
	const opacityNum = document.querySelector("#ascii-opacity-num");
	const darkness = document.querySelector("#ascii-darkness");
	const darknessNum = document.querySelector("#ascii-darkness-num");
	const nodeCount = document.querySelector("#ascii-node-count");
	const nodeCountNum = document.querySelector("#ascii-node-count-num");
	const range = document.querySelector("#ascii-range");
	const rangeNum = document.querySelector("#ascii-range-num");
	const dot = document.querySelector("#ascii-dot");
	const dotNum = document.querySelector("#ascii-dot-num");
	const lineWidth = document.querySelector("#ascii-line-width");
	const lineWidthNum = document.querySelector("#ascii-line-width-num");
	const mouseFactor = document.querySelector("#ascii-mouse-factor");
	const mouseFactorNum = document.querySelector("#ascii-mouse-factor-num");
	const worldPad = document.querySelector("#ascii-world-pad");
	const worldPadNum = document.querySelector("#ascii-world-pad-num");
	const step = document.querySelector("#ascii-step");
	const stepNum = document.querySelector("#ascii-step-num");
	const smoothing = document.querySelector("#ascii-smoothing");
	const smoothingNum = document.querySelector("#ascii-smoothing-num");
	const holdBand = document.querySelector("#ascii-hold-band");
	const holdBandNum = document.querySelector("#ascii-hold-band-num");
	const threshold = document.querySelector("#ascii-threshold");
	const thresholdNum = document.querySelector("#ascii-threshold-num");
	const thresholdFade = document.querySelector("#ascii-threshold-fade");
	const thresholdFadeNum = document.querySelector("#ascii-threshold-fade-num");
	const thresholdGray = document.querySelector("#ascii-threshold-gray");
	const thresholdGrayNum = document.querySelector("#ascii-threshold-gray-num");
	const invert = document.querySelector("#ascii-invert");
	const speedVal = document.querySelector("#ascii-speed-val");
	const charSpeedVal = document.querySelector("#ascii-char-speed-val");
	const scaleVal = document.querySelector("#ascii-scale-val");
	const opacityVal = document.querySelector("#ascii-opacity-val");
	const darknessVal = document.querySelector("#ascii-darkness-val");
	const nodeCountVal = document.querySelector("#ascii-node-count-val");
	const rangeVal = document.querySelector("#ascii-range-val");
	const dotVal = document.querySelector("#ascii-dot-val");
	const lineWidthVal = document.querySelector("#ascii-line-width-val");
	const mouseFactorVal = document.querySelector("#ascii-mouse-factor-val");
	const worldPadVal = document.querySelector("#ascii-world-pad-val");
	const stepVal = document.querySelector("#ascii-step-val");
	const smoothingVal = document.querySelector("#ascii-smoothing-val");
	const holdBandVal = document.querySelector("#ascii-hold-band-val");
	const thresholdVal = document.querySelector("#ascii-threshold-val");
	const thresholdFadeVal = document.querySelector("#ascii-threshold-fade-val");
	const thresholdGrayVal = document.querySelector("#ascii-threshold-gray-val");
	const preset = document.querySelector("#ascii-preset");
	const copyBtn = document.querySelector("#btn-copy-ascii");

	const bindPair = (rangeEl, numberEl, key, digits = 0) => {
		const set = (raw) => {
			const min = Number(
				rangeEl?.min ?? numberEl?.min ?? Number.NEGATIVE_INFINITY,
			);
			const max = Number(
				rangeEl?.max ?? numberEl?.max ?? Number.POSITIVE_INFINITY,
			);
			const normalized = typeof raw === "string" ? raw.replace(",", ".") : raw;
			let value = Number(normalized);
			if (!Number.isFinite(value)) return;
			value = Math.max(min, Math.min(max, value));
			asciiCfg[key] = value;
			const text =
				digits > 0 ? value.toFixed(digits) : String(Math.round(value));
			if (rangeEl) rangeEl.value = String(value);
			if (numberEl) numberEl.value = text;
			refresh();
		};
		if (rangeEl) rangeEl.addEventListener("input", () => set(rangeEl.value));
		if (numberEl) {
			numberEl.addEventListener("input", () => set(numberEl.value));
			numberEl.addEventListener("change", () => set(numberEl.value));
		}
		set(asciiCfg[key]);
	};

	const refresh = () => {
		if (speedVal) speedVal.textContent = String(asciiCfg.speed);
		if (charSpeedVal) charSpeedVal.textContent = String(asciiCfg.charSpeed);
		if (scaleVal) scaleVal.textContent = String(asciiCfg.scale);
		if (opacityVal) opacityVal.textContent = String(asciiCfg.opacity);
		if (darknessVal) darknessVal.textContent = String(asciiCfg.darkness);
		if (nodeCountVal) nodeCountVal.textContent = String(asciiCfg.nodeCount);
		if (rangeVal) rangeVal.textContent = String(asciiCfg.range);
		if (dotVal) dotVal.textContent = Number(asciiCfg.dot).toFixed(1);
		if (lineWidthVal)
			lineWidthVal.textContent = Number(asciiCfg.lineWidth).toFixed(1);
		if (mouseFactorVal)
			mouseFactorVal.textContent = Number(asciiCfg.mouseFactor).toFixed(2);
		if (worldPadVal)
			worldPadVal.textContent = Number(asciiCfg.worldPadPct).toFixed(1);
		if (stepVal) stepVal.textContent = String(asciiCfg.step);
		if (smoothingVal)
			smoothingVal.textContent = Number(asciiCfg.smoothing).toFixed(2);
		if (holdBandVal)
			holdBandVal.textContent = Number(asciiCfg.holdBand).toFixed(2);
		if (thresholdVal)
			thresholdVal.textContent = Number(asciiCfg.threshold).toFixed(2);
		if (thresholdFadeVal)
			thresholdFadeVal.textContent = Number(asciiCfg.thresholdFade).toFixed(2);
		if (thresholdGrayVal)
			thresholdGrayVal.textContent = String(Math.round(asciiCfg.thresholdGray));
		const str =
			`speed=${asciiCfg.speed};scale=${asciiCfg.scale};opacity=${asciiCfg.opacity};darkness=${asciiCfg.darkness};` +
			`charSpeed=${asciiCfg.charSpeed};` +
			`nodeCount=${asciiCfg.nodeCount};range=${asciiCfg.range};dot=${asciiCfg.dot.toFixed(1)};lineWidth=${asciiCfg.lineWidth.toFixed(1)};` +
			`mouseFactor=${asciiCfg.mouseFactor.toFixed(2)};worldPadPct=${asciiCfg.worldPadPct.toFixed(1)};step=${asciiCfg.step};` +
			`smoothing=${asciiCfg.smoothing.toFixed(2)};holdBand=${asciiCfg.holdBand.toFixed(2)};threshold=${asciiCfg.threshold.toFixed(2)};` +
			`thresholdFade=${asciiCfg.thresholdFade.toFixed(2)};thresholdGray=${Math.round(asciiCfg.thresholdGray)};` +
			`invert=${asciiCfg.invert ? 1 : 0}`;
		if (preset) preset.value = str;
	};

	bindPair(speed, speedNum, "speed", 0);
	bindPair(charSpeed, charSpeedNum, "charSpeed", 0);
	bindPair(scale, scaleNum, "scale", 0);
	bindPair(opacity, opacityNum, "opacity", 0);
	bindPair(darkness, darknessNum, "darkness", 0);
	bindPair(nodeCount, nodeCountNum, "nodeCount", 0);
	bindPair(range, rangeNum, "range", 0);
	bindPair(dot, dotNum, "dot", 1);
	bindPair(lineWidth, lineWidthNum, "lineWidth", 1);
	bindPair(mouseFactor, mouseFactorNum, "mouseFactor", 2);
	bindPair(worldPad, worldPadNum, "worldPadPct", 1);
	bindPair(step, stepNum, "step", 0);
	bindPair(smoothing, smoothingNum, "smoothing", 2);
	bindPair(holdBand, holdBandNum, "holdBand", 2);
	bindPair(threshold, thresholdNum, "threshold", 2);
	bindPair(thresholdFade, thresholdFadeNum, "thresholdFade", 2);
	bindPair(thresholdGray, thresholdGrayNum, "thresholdGray", 0);
	if (invert)
		invert.addEventListener("change", () => {
			asciiCfg.invert = !!invert.checked;
			refresh();
		});
	if (copyBtn) {
		copyBtn.addEventListener("click", async () => {
			const text = preset?.value || "";
			try {
				await navigator.clipboard.writeText(text);
				log("ASCII preset copied.");
			} catch {
				if (preset) {
					preset.focus();
					preset.select();
				}
			}
		});
	}
	refresh();
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

function initMusic() {
	const btn = document.querySelector("#btn-music");
	const slider = document.querySelector("#music-volume");
	const widget = document.querySelector("#music-widget");
	if (!btn || !slider || !widget) return;

	const audio = new Audio("./soundtrack/lbd2neo.mp3");
	audio.preload = "none";
	audio.loop = true;
	audio.volume = slider.value / 100;

	let playing = false;

	btn.addEventListener("click", () => {
		if (playing) {
			audio.pause();
			playing = false;
			btn.classList.remove("music-btn--on");
			widget.classList.remove("music-widget--on");
		} else {
			audio.play();
			playing = true;
			btn.classList.add("music-btn--on");
			widget.classList.add("music-widget--on");
		}
	});

	slider.addEventListener("input", () => {
		audio.volume = slider.value / 100;
	});
}

// ---- Floating citation widget ----

const PAPER_URL =
	"https://research.tue.nl/nl/publications/ifc2lbd-neo-a-rust-based-converter-for-efficient-linked-building-";
const BIBTEX_ENTRY = `@inproceedings{6dfbd65779994b9cb40a5e5990afed5f,
  title     = "IFC2LBD-Neo: A Rust-Based Converter for Efficient Linked Building Data Generation from IFC",
  author    = "Lukas Kirner and Jyrki Oraskari and A.J.A. Donkers and Ekaterina Petrova and Pieter Pauwels and Jakob Beetz",
  booktitle = "Proceedings of the 33rd EG-ICE International Workshop on Intelligent Computing in Engineering",
  year      = "2026",
  url       = "https://www.dicea.unipd.it/eg-ice-2026-padua",
}`;

function initCiteWidget() {
	document.querySelector("#cite-paper-btn")?.addEventListener("click", () => {
		window.open(PAPER_URL, "_blank", "noopener");
	});

	document.querySelector("#cite-bibtex-btn")?.addEventListener("click", () => {
		navigator.clipboard.writeText(BIBTEX_ENTRY).then(() => {
			const btn = document.querySelector("#cite-bibtex-btn");
			if (btn) {
				const orig = btn.innerHTML;
				btn.innerHTML = "✓ Copied!";
				setTimeout(() => {
					btn.innerHTML = orig;
				}, 1500);
			}
		});
	});
}

init().catch((error) =>
	log(`Startup: ${error instanceof Error ? error.message : String(error)}`),
);
