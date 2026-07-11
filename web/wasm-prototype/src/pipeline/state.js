// ---------------------------------------------------------------------------
// state.js — Minimal reactive state for the pipeline dashboard
// ---------------------------------------------------------------------------

const listeners = new Map();

const state = {
	// Pipeline configuration
	activeModules: new Set([
		"neo-cleanup-preprocess",
		"neo-qto-preprocess",
		"neo-bot-producer",
		"neo-beo-producer",
		"neo-bsdd-producer",
		"neo-omg-fog",
		"neo-turtle-serializer",
		"neo-file-export",
	]),
	moduleOptions: {
		"neo-turtle-serializer": { grouping: "streaming", layout: "joined" },
		"neo-bsdd-producer": {
			compact: "true",
			include_standard_attrs: "true",
			dedup_properties: "true",
		},
		"neo-file-export": {
			compress: "gzip",
		},
	},
	baseUri: "https://lbd.org/",
	outputStem: "",
	ifcFile: null,
	ifcFileBytes: null,
	structuredDataFiles: null, // File[] or null (array for directory mode)
	structuredDataBytes: null, // Uint8Array[] or null (read at run time)
	inputFormat: "ifc", // "ifc" or "structured-data"

	// Available modules (from listModules())
	modules: [],

	// Pipeline execution state
	running: false,
	stageStatuses: {},

	// UI state
	selectedPluginId: null,
	detailOpen: false,
	showPreprocess: true,
	showPostprocess: false,
};

export function getState() {
	return state;
}

export function update(patch) {
	const prev = { ...state };
	Object.assign(state, patch);
	notify(prev);
}

export function subscribe(key, fn) {
	if (!listeners.has(key)) listeners.set(key, new Set());
	listeners.get(key).add(fn);
	return () => listeners.get(key)?.delete(fn);
}

export function subscribeAll(fn) {
	if (!listeners.has("*")) listeners.set("*", new Set());
	listeners.get("*").add(fn);
	return () => listeners.get("*").delete(fn);
}

function notify(prev) {
	for (const [key, fns] of listeners) {
		if (key === "*" || state[key] !== prev[key]) {
			for (const fn of fns) fn(state, prev);
		}
	}
}

const SERIALIZERS = [
	"neo-turtle-serializer",
	"neo-nquads-serializer",
	"neo-nquads-chunked-serializer",
];

// Preprocess is an internal dependency of the producer — always enabled together.
const GEO_PREPROCESS = "neo-geometry-preprocess";
const GEO_PRODUCER = "neo-geometry-producer";

export function toggleModule(id) {
	const mods = new Set(state.activeModules);
	if (mods.has(id)) {
		mods.delete(id);
		// Turning off producer also removes its dependency
		if (id === GEO_PRODUCER) mods.delete(GEO_PREPROCESS);
		// Turning off preprocess also removes its dependent producer
		if (id === GEO_PREPROCESS) mods.delete(GEO_PRODUCER);
	} else {
		if (SERIALIZERS.includes(id)) {
			SERIALIZERS.forEach((s) => mods.delete(s));
		}
		mods.add(id);
		// Enabling producer always enables its preprocess dependency too
		if (id === GEO_PRODUCER) mods.add(GEO_PREPROCESS);
		// Enabling preprocess also enables its natural consumer
		if (id === GEO_PREPROCESS) mods.add(GEO_PRODUCER);
	}
	update({ activeModules: mods });
}

export function updateStageStatus(event) {
	const statuses = { ...state.stageStatuses };
	statuses[event.pluginId] = {
		status: event.status,
		durationMs: event.durationMs || 0,
		bytesOut: event.bytesOut || 0,
		triplesOut: event.triplesOut || 0,
		error: event.error || null,
	};
	update({ stageStatuses: statuses });
}

export function resetStageStatuses() {
	update({ stageStatuses: {} });
}
