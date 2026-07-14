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
	const isStructuredImport = selectedPluginId === "neo-structured-data-import";

	const content = panelEl.querySelector("#detail-content");
	if (!content) return;

	if (!mod && !isParse && !isStructuredImport) {
		content.innerHTML =
			'<div class="detail-empty">Select a pipeline node to view details.</div>';
		return;
	}

	const displayName = isParse
		? "Parse IFC"
		: isStructuredImport
			? "Parse Structured Data"
			: mod.displayName;
	const stage = isParse || isStructuredImport ? "Import" : mod.stage;
	const rawOptionKeys = isParse || isStructuredImport ? [] : mod.optionKeys || [];
	// output_stem is controlled by the global "Stem" field in the left rail
	const optionKeys = rawOptionKeys.filter(
		(k) => !(mod?.id === "neo-file-export" && k === "output_stem"),
	);

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

    ${
			isStructuredImport
				? `
    <div class="detail-section">
      <div class="detail-section-title">INPUT FILES</div>
      <label class="rail-file-btn" id="structured-file-btn">
        <span class="rail-file-text" id="structured-file-text">Choose file(s)…</span>
        <input type="file" id="structured-file-input" accept=".json,.xml,.csv,.tsv" multiple />
      </label>
      <button class="rail-file-btn" id="btn-structured-dir" type="button" style="margin-top:6px;">
        <span class="rail-file-text" id="structured-dir-text">Choose directory…</span>
      </button>
      <div class="rail-file-meta" id="structured-file-meta"></div>
      <div class="rail-file-meta" id="structured-dir-unsupported" style="display:none">Directory selection is not supported in this browser.</div>
    </div>`
				: ""
		}

    ${
			optionKeys.length > 0
				? `
    <div class="detail-section">
      <div class="detail-section-title">OPTIONS</div>
      ${optionKeys.map((key) => optionControl(selectedPluginId, key)).join("")}
    </div>`
				: ""
		}

    ${
			!isParse && mod
				? `
    <div class="detail-section">
      <div class="detail-section-title">METADATA</div>
      <div class="detail-row"><span class="detail-label">ID</span><span class="detail-value detail-mono">${mod.id}</span></div>
      <div class="detail-row"><span class="detail-label">Failure policy</span><span class="detail-value">${mod.failurePolicy}</span></div>
      <div class="detail-row"><span class="detail-label">Parallelism</span><span class="detail-value">${mod.parallelism}</span></div>
      <div class="detail-row"><span class="detail-label">Inputs</span><span class="detail-value">${mod.inputs.join(", ") || "—"}</span></div>
      <div class="detail-row"><span class="detail-label">Outputs</span><span class="detail-value">${mod.outputs.join(", ") || "—"}</span></div>
      ${mod.requires.length ? `<div class="detail-row"><span class="detail-label">Requires</span><span class="detail-value">${mod.requires.join(", ")}</span></div>` : ""}
      ${mod.conflictsWith.length ? `<div class="detail-row"><span class="detail-label">Conflicts</span><span class="detail-value">${mod.conflictsWith.join(", ")}</span></div>` : ""}
    </div>`
				: ""
		}
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

	// Wire file-upload options (rml_mapping stores file text content, not a value)
	for (const key of optionKeys) {
		const input = content.querySelector(
			`[data-option-key="${key}"][type="file"]`,
		);
		if (!input) continue;
		input.addEventListener("change", async () => {
			const file = input.files?.[0];
			if (!file) return;
			const text = await file.text();
			const { moduleOptions } = getState();
			const opts = { ...moduleOptions };
			if (!opts[selectedPluginId]) opts[selectedPluginId] = {};
			opts[selectedPluginId][key] = text;
			update({ moduleOptions: opts });
		});
	}

	// Wire structured data file input (shown in detail panel for neo-structured-data-import)
	if (isStructuredImport) {
		content
			.querySelector("#structured-file-input")
			?.addEventListener("change", (e) => {
				const files = Array.from(e.target.files || []);
				if (!files.length) return;
				// Don't clear ifcFile — both IFC and structured data can coexist.
				update({
					structuredDataFiles: files,
				});
				const meta = content.querySelector("#structured-file-meta");
				if (meta)
					meta.textContent =
						files.length === 1
							? files[0].name
							: `${files.length} files selected`;
			});

		const dirBtn = content.querySelector("#btn-structured-dir");
		if (dirBtn) {
			if (typeof window.showDirectoryPicker === "function") {
				dirBtn.addEventListener("click", async () => {
					try {
						const dirHandle = await window.showDirectoryPicker();
						const files = [];
						for await (const entry of dirHandle.values()) {
							if (entry.kind === "file") {
								const file = await entry.getFile();
								if (/\.(json|xml|csv|tsv)$/i.test(file.name)) {
									files.push(file);
								}
							}
						}
						if (files.length) {
							update({
								structuredDataFiles: files,
								inputFormat: "structured-data",
								ifcFile: null,
							});
							const meta = content.querySelector("#structured-file-meta");
							if (meta)
								meta.textContent = `${files.length} files from ${dirHandle.name}`;
							document.querySelector("#rail-file-text").textContent =
								"Choose IFC file…";
						} else {
							const meta = content.querySelector("#structured-file-meta");
							if (meta)
								meta.textContent = "No JSON/CSV/XML files found in directory.";
						}
					} catch (err) {
						if (err.name !== "AbortError") {
							const meta = content.querySelector("#structured-file-meta");
							if (meta) meta.textContent = `Error: ${err.message}`;
						}
					}
				});
			} else {
				content.querySelector("#structured-dir-unsupported").style.display =
					"block";
				dirBtn.style.display = "none";
			}
		}
	}
}

function optionControl(pluginId, key) {
	const { moduleOptions } = getState();
	const current = moduleOptions[pluginId]?.[key] || "";

	if (key === "grouping") {
		const groupingVal = current || "streaming";
		return `
      <div class="detail-row">
        <span class="detail-label">${key}</span>
        <select data-option-key="${key}" class="detail-select">
          <option value="streaming" ${groupingVal === "streaming" ? "selected" : ""}>Streaming</option>
          <option value="sorted" ${groupingVal === "sorted" ? "selected" : ""}>Sorted (grouped)</option>
        </select>
      </div>`;
	}
	if (key === "layout") {
		const layoutVal = current || "joined";
		return `
      <div class="detail-row">
        <span class="detail-label">${key}</span>
        <select data-option-key="${key}" class="detail-select">
          <option value="joined" ${layoutVal === "joined" ? "selected" : ""}>Joined Turtle file</option>
          <option value="separate" ${layoutVal === "separate" ? "selected" : ""}>Separate files per producer</option>
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
	if (key === "graph_naming") {
		const namingVal = current || "producers";
		return `
      <div class="detail-row">
        <span class="detail-label">graph naming</span>
        <select data-option-key="graph_naming" class="detail-select">
          <option value="producers" ${namingVal === "producers" ? "selected" : ""}>Per producer graphs</option>
          <option value="filename" ${namingVal === "filename" ? "selected" : ""}>Single graph from filename</option>
        </select>
      </div>`;
	}
	if (key === "mode") {
		const modeVal = current || "full";
		return `
      <div class="detail-row">
        <span class="detail-label">mode</span>
        <select data-option-key="mode" class="detail-select">
          <option value="full" ${modeVal === "full" ? "selected" : ""}>Full (standard ifcOWL)</option>
          <option value="projected" ${modeVal === "projected" ? "selected" : ""}>Projected (compact, ~58% fewer triples)</option>
        </select>
      </div>`;
	}
	if (key === "profile") {
		const profileVal = current || "base";
		return `
      <div class="detail-row">
        <span class="detail-label">profile</span>
        <select data-option-key="profile" class="detail-select">
          <option value="base" ${profileVal === "base" ? "selected" : ""}>base (universal IFC aliases)</option>
          <option value="revit-dach" ${profileVal === "revit-dach" ? "selected" : ""}>revit-dach (Revit, German)</option>
          <option value="allplan-de" ${profileVal === "allplan-de" ? "selected" : ""}>allplan-de (Allplan, German)</option>
          <option value="tekla-en" ${profileVal === "tekla-en" ? "selected" : ""}>tekla-en (Tekla, English)</option>
        </select>
      </div>`;
	}
	if (key === "compact") {
		const compactVal = current || "false";
		return `
      <div class="detail-row">
        <span class="detail-label">compact</span>
        <select data-option-key="compact" class="detail-select">
          <option value="false" ${compactVal === "false" ? "selected" : ""}>Off — full provenance metadata</option>
          <option value="true" ${compactVal === "true" ? "selected" : ""}>On — skip mapping metadata triples</option>
        </select>
      </div>`;
	}
	if (key === "include_standard_attrs") {
		const attrsVal = current || "true";
		return `
      <div class="detail-row">
        <span class="detail-label">standard attrs</span>
        <select data-option-key="include_standard_attrs" class="detail-select">
          <option value="true" ${attrsVal === "true" ? "selected" : ""}>On — emit GlobalId, Name, etc.</option>
          <option value="false" ${attrsVal === "false" ? "selected" : ""}>Off — bSDD types and props only</option>
        </select>
      </div>`;
	}
	if (key === "dedup_properties") {
		const dedupVal = current || "false";
		return `
      <div class="detail-row">
        <span class="detail-label">dedup properties</span>
        <select data-option-key="dedup_properties" class="detail-select">
          <option value="false" ${dedupVal === "false" ? "selected" : ""}>Off — one instance per element</option>
          <option value="true" ${dedupVal === "true" ? "selected" : ""}>On — share instances with equal values</option>
        </select>
      </div>`;
	}
	if (key === "format") {
		const formatVal = current || "fragments";
		return `
      <div class="detail-row">
        <span class="detail-label">format</span>
        <select data-option-key="format" class="detail-select">
          <option value="fragments" ${formatVal === "fragments" ? "selected" : ""}>Fragments (.frag) — ThatOpen viewer</option>
          <option value="parquet" ${formatVal === "parquet" ? "selected" : ""}>Parquet ZIP (.bos) — analytics / ifc-lite schema</option>
          <option value="gltf" ${formatVal === "gltf" ? "selected" : ""}>glTF binary (.glb) — 3D viewers</option>
        </select>
      </div>`;
	}
	if (key === "metadata") {
		const metaVal = current || "full";
		return `
      <div class="detail-row">
        <span class="detail-label">metadata</span>
        <select data-option-key="metadata" class="detail-select">
          <option value="full" ${metaVal === "full" ? "selected" : ""}>Full — all properties, relations, GUIDs</option>
          <option value="stripped" ${metaVal === "stripped" ? "selected" : ""}>Stripped — geometry + GUIDs only (faster, smaller)</option>
        </select>
      </div>`;
	}
	if (key === "compress") {
		const compressVal = current || "none";
		return `
      <div class="detail-row">
        <span class="detail-label">compress</span>
        <select data-option-key="compress" class="detail-select">
          <option value="none" ${compressVal === "none" ? "selected" : ""}>None — plain file</option>
          <option value="gzip" ${compressVal === "gzip" ? "selected" : ""}>gzip — smaller file</option>
        </select>
      </div>`;
	}
	if (key === "rml_mapping") {
		const filename = current ? current.split("\n")[0].slice(0, 40) : "";
		return `
      <div class="detail-row">
        <span class="detail-label">RML mapping</span>
        <div style="display:flex;flex-direction:column;gap:4px;">
          <input type="file" data-option-key="rml_mapping" accept=".ttl,.turtle,.n3" class="detail-input" style="padding:2px;" />
          ${filename ? `<span style="font-size:10px;color:var(--text-dim);">${filename}</span>` : ""}
        </div>
      </div>`;
	}
	if (key === "alignment_file") {
		return `
      <div class="detail-row">
        <span class="detail-label">Alignment file</span>
        <div style="display:flex;flex-direction:column;gap:4px;">
          <input type="file" data-option-key="alignment_file" accept=".ttl,.turtle,.n3,.rdf,.xml,.owl" class="detail-input" style="padding:2px;" />
          ${current ? `<span style="font-size:10px;color:var(--text-dim);">loaded (${current.length} chars)</span>` : ""}
        </div>
      </div>`;
	}
	if (key === "ontology_file") {
		return `
      <div class="detail-row">
        <span class="detail-label">Ontology file</span>
        <div style="display:flex;flex-direction:column;gap:4px;">
          <input type="file" data-option-key="ontology_file" accept=".ttl,.turtle,.n3,.rdf,.xml,.owl" class="detail-input" style="padding:2px;" />
          ${current ? `<span style="font-size:10px;color:var(--text-dim);">loaded (${current.length} chars)</span>` : ""}
        </div>
      </div>`;
	}
	// Default: text input with contextual placeholder
	const placeholders = {
		chunk_size_lines: "2000000 (2M lines)",
		chunk_size_bytes: "268435456 (256MB)",
		chunk_prefix: "out",
		graph_naming: "producers | filename",
		inflation_threshold: "0.1",
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
	const labels = {
		idle: "Idle",
		running: "Running…",
		success: "Success",
		failed: "Failed",
		warning: "Warning",
	};
	return labels[status.status] || status.status;
}

function bytesToHuman(bytes) {
	if (!bytes) return "—";
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
