// ---------------------------------------------------------------------------
// state.js — Minimal reactive state for the pipeline dashboard
// ---------------------------------------------------------------------------

const listeners = new Map();

const state = {
  // Pipeline configuration
  activeModules: new Set([
    "neo-lbd-producer",
    "neo-ifcowl-producer",
    "neo-turtle-serializer",
    "neo-file-export",
  ]),
  moduleOptions: {},
  baseUri: "https://lbd.example.com/",
  outputStem: "converted-model",
  ifcFile: null,
  ifcFileBytes: null,

  // Available modules (from listModules())
  modules: [],

  // Pipeline execution state
  running: false,
  stageStatuses: {},

  // UI state
  selectedPluginId: null,
  detailOpen: false,
};

export function getState() { return state; }

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

export function toggleModule(id) {
  const mods = new Set(state.activeModules);
  mods.has(id) ? mods.delete(id) : mods.add(id);
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
