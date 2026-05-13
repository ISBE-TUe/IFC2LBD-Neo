// ---------------------------------------------------------------------------
// sidebar.js — Right slide-out detail panel for a selected plugin
// ---------------------------------------------------------------------------

import { getState, update, subscribe } from "./state.js";

let panelEl = null;
let backdropEl = null;

export function initSidebar() {
  panelEl = document.querySelector("#detail-panel");
  backdropEl = document.querySelector("#detail-backdrop");

  backdropEl?.addEventListener("click", () => {
    update({ detailOpen: false, selectedPluginId: null });
  });

  panelEl?.querySelector("#detail-close")?.addEventListener("click", () => {
    update({ detailOpen: false, selectedPluginId: null });
  });

  subscribe("selectedPluginId", render);
  subscribe("stageStatuses", render);
  subscribe("detailOpen", renderOpenState);
  subscribe("modules", render);

  renderOpenState();
}

function renderOpenState() {
  const { detailOpen } = getState();
  if (panelEl) panelEl.classList.toggle("open", detailOpen);
  if (backdropEl) backdropEl.classList.toggle("open", detailOpen);
}

function render() {
  const { selectedPluginId, modules, stageStatuses } = getState();
  if (!panelEl) return;

  // Find module info
  const mod = modules.find((m) => m.id === selectedPluginId);
  const status = stageStatuses[selectedPluginId];
  const isParse = selectedPluginId === "parse";

  const content = panelEl.querySelector("#detail-content");
  if (!content) return;

  if (!mod && !isParse) {
    content.innerHTML = '<div class="detail-empty">Select a pipeline node to view details.</div>';
    return;
  }

  const displayName = isParse ? "Parse IFC" : mod.displayName;
  const stage = isParse ? "Import" : mod.stage;
  const rawOptionKeys = isParse ? [] : mod.optionKeys || [];
  // output_stem is controlled by the global "Stem" field in the left rail
  const optionKeys = rawOptionKeys.filter((k) => !(mod?.id === "neo-file-export" && k === "output_stem"));

  content.innerHTML = `
    <div class="detail-header-row">
      <button class="detail-back" id="detail-back-btn">←</button>
      <h3 class="detail-title">${displayName}</h3>
    </div>

    <div class="detail-section">
      <div class="detail-row">
        <span class="detail-label">Stage</span>
        <span class="detail-value">${stage}</span>
      </div>
      <div class="detail-row">
        <span class="detail-label">Status</span>
        <span class="detail-value detail-status-${status?.status || "idle"}">${statusLabel(status)}</span>
      </div>
      ${status?.durationMs ? `<div class="detail-row"><span class="detail-label">Duration</span><span class="detail-value">${(status.durationMs / 1000).toFixed(3)}s</span></div>` : ""}
      ${status?.bytesOut ? `<div class="detail-row"><span class="detail-label">Output</span><span class="detail-value">${bytesToHuman(status.bytesOut)}</span></div>` : ""}
      ${status?.triplesOut ? `<div class="detail-row"><span class="detail-label">Triples</span><span class="detail-value">${status.triplesOut.toLocaleString()}</span></div>` : ""}
      ${status?.error ? `<div class="detail-row"><span class="detail-label">Error</span><span class="detail-value detail-error">${status.error}</span></div>` : ""}
    </div>

    ${optionKeys.length > 0 ? `
    <div class="detail-section">
      <div class="detail-section-title">OPTIONS</div>
      ${optionKeys.map((key) => optionControl(selectedPluginId, key)).join("")}
    </div>` : ""}

    ${!isParse ? `
    <div class="detail-section">
      <div class="detail-section-title">METADATA</div>
      <div class="detail-row"><span class="detail-label">ID</span><span class="detail-value detail-mono">${mod.id}</span></div>
      <div class="detail-row"><span class="detail-label">Failure policy</span><span class="detail-value">${mod.failurePolicy}</span></div>
      <div class="detail-row"><span class="detail-label">Parallelism</span><span class="detail-value">${mod.parallelism}</span></div>
      <div class="detail-row"><span class="detail-label">Inputs</span><span class="detail-value">${mod.inputs.join(", ") || "—"}</span></div>
      <div class="detail-row"><span class="detail-label">Outputs</span><span class="detail-value">${mod.outputs.join(", ") || "—"}</span></div>
      ${mod.requires.length ? `<div class="detail-row"><span class="detail-label">Requires</span><span class="detail-value">${mod.requires.join(", ")}</span></div>` : ""}
      ${mod.conflictsWith.length ? `<div class="detail-row"><span class="detail-label">Conflicts</span><span class="detail-value">${mod.conflictsWith.join(", ")}</span></div>` : ""}
    </div>` : ""}
  `;

  // Wire up back button
  content.querySelector("#detail-back-btn")?.addEventListener("click", () => {
    update({ detailOpen: false, selectedPluginId: null });
  });

  // Wire up option controls
  for (const key of optionKeys) {
    const input = content.querySelector(`[data-option-key="${key}"]`);
    if (!input) continue;
    input.addEventListener("change", () => {
      const { moduleOptions } = getState();
      const opts = { ...moduleOptions };
      if (!opts[selectedPluginId]) opts[selectedPluginId] = {};
      opts[selectedPluginId][key] = input.value;
      update({ moduleOptions: opts });
    });
  }
}

function optionControl(pluginId, key) {
  const { moduleOptions } = getState();
  const current = moduleOptions[pluginId]?.[key] || "";

  if (key === "grouping") {
    return `
      <div class="detail-row">
        <span class="detail-label">${key}</span>
        <select data-option-key="${key}" class="detail-select">
          <option value="sorted" ${current === "sorted" ? "selected" : ""}>Sorted (grouped)</option>
          <option value="streaming" ${current === "streaming" ? "selected" : ""}>Streaming</option>
        </select>
      </div>`;
  }
  if (key === "chunking") {
    // Default to "lines" for chunked serializer (matches Rust default)
    const chunkingVal = current || "lines";
    return `
      <div class="detail-row">
        <span class="detail-label">${key}</span>
        <select data-option-key="${key}" class="detail-select">
          <option value="none" ${chunkingVal === "none" ? "selected" : ""}>None</option>
          <option value="lines" ${chunkingVal === "lines" ? "selected" : ""}>Lines</option>
          <option value="bytes" ${chunkingVal === "bytes" ? "selected" : ""}>Bytes</option>
        </select>
      </div>`;
  }
  // Default: text input with contextual placeholder
  const placeholders = {
    chunk_size_lines: "2000000 (2M lines)",
    chunk_size_bytes: "268435456 (256MB)",
    chunk_prefix: "out",
    inflation_threshold: "0.1",
    lbd_graph_iri: "https://example.com/lbd",
    ifcowl_graph_iri: "https://example.com/ifcowl",
    output_stem: "converted-model",
  };
  const placeholder = placeholders[key] || key;
  return `
    <div class="detail-row">
      <span class="detail-label">${key}</span>
      <input data-option-key="${key}" class="detail-input" type="text" value="${current}" placeholder="${placeholder}" />
    </div>`;
}

function statusLabel(status) {
  if (!status) return "Idle";
  const labels = { idle: "Idle", running: "Running…", success: "Success", failed: "Failed", warning: "Warning" };
  return labels[status.status] || status.status;
}

function bytesToHuman(bytes) {
  if (!bytes) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
