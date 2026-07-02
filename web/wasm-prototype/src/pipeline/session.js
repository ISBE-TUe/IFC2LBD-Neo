// ---------------------------------------------------------------------------
// session.js — Ableton Session View pipeline grid with bezier connections
//
// Key features:
// - LBD before IfcOWL in Produce column, Turtle before N-Quads in Serialize
// - Clickable circles toggle modules (radio for serializers)
// - Bezier connections between active modules, only animate when running
// - Resize-aware: redraws connections on window resize
// - "IFC Input" file drop zone in Preprocess column
// - "Add module…" row for inactive producers
// ---------------------------------------------------------------------------

import { getState, subscribe, toggleModule, update } from "./state.js";

const ALL_STAGES = [
	{ key: "Import", label: "IMPORT" },
	{ key: "Preprocess", label: "PREPROCESS" },
	{ key: "Produce", label: "PRODUCE" },
	{ key: "Postprocess", label: "POSTPROCESS" },
	{ key: "Serialize", label: "SERIALIZE" },
	{ key: "Export", label: "EXPORT" },
];

// Module sort order: modular producers first, then others
const PRODUCE_ORDER = [
	"neo-bot-producer",
	"neo-beo-producer",
	"neo-props-opm",
	"neo-bsdd-producer",
	"neo-omg-fog",
	"neo-ifcowl-producer",
	"neo-ifc-topology-producer",
	"neo-bbox-enricher",
	"neo-rml-mapper",
];
const POSTPROCESS_ORDER = [
	"neo-ontology-mapper",
];
const SERIALIZE_ORDER = [
	"neo-turtle-serializer",
	"neo-nquads-serializer",
	"neo-nquads-chunked-serializer",
];

const STATUS_COLORS = {
	idle: "#B8B8B8",
	running: "#5BA8C8",
	success: "#00E676",
	failed: "#C87070",
	warning: "#B8A050",
};

const STATUS_ICONS = {
	idle: "○",
	running: "◉",
	success: "●",
	failed: "✕",
	warning: "▲",
};

let containerEl = null;
let resizeHandler = null;

export function initSession(container) {
	containerEl = container;
	subscribe("activeModules", render);
	subscribe("modules", render);
	subscribe("stageStatuses", render);
	subscribe("running", render);
	subscribe("selectedPluginId", render);
	subscribe("ifcFile", render);
	subscribe("showPreprocess", render);
	subscribe("showPostprocess", render);

	// Redraw connections on resize
	resizeHandler = () => requestAnimationFrame(() => drawConnections());
	window.addEventListener("resize", resizeHandler);

	render();
}

function sortModules(mods, stage) {
	const order =
		stage === "Produce"
			? PRODUCE_ORDER
			: stage === "Serialize"
				? SERIALIZE_ORDER
				: stage === "Postprocess"
					? POSTPROCESS_ORDER
					: [];
	return [...mods].sort((a, b) => {
		const ai = order.indexOf(a.id);
		const bi = order.indexOf(b.id);
		if (ai !== -1 && bi !== -1) return ai - bi;
		if (ai !== -1) return -1;
		if (bi !== -1) return 1;
		return 0;
	});
}

function render() {
	if (!containerEl) return;
	const {
		activeModules,
		modules,
		stageStatuses,
		selectedPluginId,
		inputFormat,
		showPreprocess,
		showPostprocess,
	} = getState();

	const stages = ALL_STAGES.filter(
		(s) =>
			(s.key !== "Preprocess" || showPreprocess) &&
			(s.key !== "Postprocess" || showPostprocess),
	);

	const parseMod = {
		id: "parse",
		displayName: "Parse IFC",
		stage: "Import",
		failurePolicy: "Required",
		wasmCompatible: true,
		optionKeys: [],
		description: "Parse IFC STEP file and build typed model",
	};

	const parseStructuredMod = {
		id: "neo-structured-data-import",
		displayName: "Parse Structured Data",
		stage: "Import",
		failurePolicy: "Required",
		wasmCompatible: true,
		optionKeys: [],
		description:
			"Parse JSON/CSV/XML structured data for RML and ontology mapping",
	};

	const columns = stages.map((s) => ({
		...s,
		modules:
			s.key === "Import"
				? [parseMod, parseStructuredMod]
				: sortModules(
						modules.filter((m) => m.stage === s.key),
						s.key,
					),
	}));

	let html = '<div class="session-grid">';

	// Column headers
	html += '<div class="session-headers">';
	for (let i = 0; i < columns.length; i++) {
		html += `<div class="session-col-header"><span class="col-label">${columns[i].label}</span></div>`;
		if (i < columns.length - 1)
			html += '<div class="session-col-header-sep"></div>';
	}
	html += "</div>";

	// Body
	html += '<div class="session-body" id="session-body">';

	for (let ci = 0; ci < columns.length; ci++) {
		const col = columns[ci];
		html += `<div class="session-column" data-stage="${col.key}">`;
		for (const mod of col.modules) {
			const isStructured = mod.id === "neo-structured-data-import";
			const isActive =
				mod.id === "parse" ||
				(isStructured && inputFormat === "structured-data") ||
				activeModules.has(mod.id);
			const isRequired =
				mod.id === "parse" ||
				mod.id === "neo-structured-data-import" ||
				mod.id === "neo-file-export";
			const status = stageStatuses[mod.id];
			const statusStr = status?.status || "idle";
			const isSelected = selectedPluginId === mod.id;
			const statusColor = STATUS_COLORS[statusStr] || STATUS_COLORS.idle;
			const statusIcon = STATUS_ICONS[statusStr] || STATUS_ICONS.idle;
			const isRunning = statusStr === "running";
			const isFailed = statusStr === "failed";
			const durationMs = status?.durationMs || 0;

			const isSucceeded = statusStr === "success";
			html += `<div class="session-cell ${isActive ? "active" : "inactive"} ${isSelected ? "selected" : ""} ${isRunning ? "running" : ""} ${isSucceeded ? "succeeded" : ""}" data-plugin-id="${mod.id}" id="cell-${mod.id}">`;

			// Clickable circle (not for parse/required)
			if (mod.id !== "parse" && !isRequired) {
				html += `<button class="cell-circle ${isActive ? "on" : "off"} ${isRunning ? "pulse" : ""}" 
          style="color:${isActive ? statusColor : "#CCC"}" 
          data-toggle-id="${mod.id}">${statusIcon}</button>`;
			} else {
				html += `<span class="cell-circle fixed ${isRunning ? "pulse" : ""}" style="color:${statusColor}">${statusIcon}</span>`;
			}

			// Name
			html += `<span class="cell-name">${shortName(mod.displayName)}</span>`;

			// Timing — show for completed stages
			if (statusStr === "success" || statusStr === "failed") {
				html += `<span class="cell-timing">${formatDuration(durationMs)}</span>`;
			}
			const triplesOut = status?.triplesOut || 0;
			const showTriples = triplesOut > 0;
			if (showTriples) {
				html += `<span class="cell-triples">${formatTriples(status.triplesOut)}</span>`;
			}
			if (status?.bytesOut) {
				html += `<span class="cell-bytes">${bytesToHuman(status.bytesOut)}</span>`;
			}
			if (isFailed && status?.error) {
				html += `<span class="cell-error" title="${escapeHtml(status.error)}">!</span>`;
			}

			html += "</div>";
		}

		// "Add module…" button for adding new modules not yet in this column
		if (col.key !== "Import" && col.key !== "Preprocess") {
			html += `<div class="session-cell add-plugin" data-add-stage="${col.key}">
        <span class="cell-circle off" style="color:#CCC">+</span>
        <span class="cell-name" style="color:var(--text-dim)">Add module…</span>
      </div>`;
		}

		html += "</div>";

		// Connector column
		if (ci < columns.length - 1) {
			html += '<div class="session-connector-col"></div>';
		}
	}

	html += "</div></div>";

	containerEl.innerHTML = html;

	// Draw SVG bezier connections after layout
	requestAnimationFrame(() => drawConnections());

	// Wire circle toggles
	containerEl
		.querySelectorAll(".cell-circle[data-toggle-id]")
		.forEach((btn) => {
			btn.addEventListener("click", (e) => {
				e.stopPropagation();
				const id = btn.getAttribute("data-toggle-id");
				toggleModule(id);
			});
		});

	// Wire cell clicks for detail panel
	containerEl
		.querySelectorAll(".session-cell[data-plugin-id]")
		.forEach((cell) => {
			cell.addEventListener("click", () => {
				const id = cell.getAttribute("data-plugin-id");
				update({ selectedPluginId: id, detailOpen: true });
			});
		});

	// Wire "Add module" buttons — show a dropdown picker of available modules
	containerEl.querySelectorAll(".add-plugin[data-add-stage]").forEach((btn) => {
		btn.addEventListener("click", (e) => {
			e.stopPropagation();
			const stage = btn.getAttribute("data-add-stage");
			// Remove any existing picker
			document.querySelector(".module-picker")?.remove();

			const picker = document.createElement("div");
			picker.className = "module-picker";
			picker.style.position = "fixed";
			const rect = btn.getBoundingClientRect();
			picker.style.left = rect.left + "px";
			picker.style.top = rect.bottom + 4 + "px";
			picker.style.zIndex = "9999";

			// Available modules not yet in the registry
			// In the future this will include user-uploaded WASM plugins
			// For now, show a message that this is for future extensions
			const availableModules = getAvailableUserModules(stage);

			if (availableModules.length === 0) {
				picker.innerHTML = `
          <div class="module-picker-header">Available modules</div>
          <div class="module-picker-empty">
            <div>No additional modules available</div>
            <div style="font-size:10px;color:var(--text-dim);margin-top:4px">Upload a WASM plugin to add custom modules</div>
          </div>`;
			} else {
				let list = `<div class="module-picker-header">Add module</div>`;
				for (const mod of availableModules) {
					list += `<div class="module-picker-item" data-module-id="${mod.id}" data-module-stage="${stage}">
            <span class="picker-item-name">${mod.displayName}</span>
            <span class="picker-item-desc">${mod.description || ""}</span>
          </div>`;
				}
				picker.innerHTML = list;
			}

			document.body.appendChild(picker);

			// Wire picker items
			picker.querySelectorAll(".module-picker-item").forEach((item) => {
				item.addEventListener("click", () => {
					const modId = item.getAttribute("data-module-id");
					const stage = item.getAttribute("data-module-stage");
					// Add the module to the registry and activate it
					addUserModule(modId, stage);
					picker.remove();
				});
			});

			// Close on outside click
			const closePicker = (ev) => {
				if (!picker.contains(ev.target)) {
					picker.remove();
					document.removeEventListener("click", closePicker);
				}
			};
			setTimeout(() => document.addEventListener("click", closePicker), 0);
		});
	});
}

/**
 * Draw bezier curve connections between active modules in adjacent columns.
 * A connection is only visible when the upstream stage has completed or is running.
 * It only animates (flow dash) when the downstream stage is actively running.
 */
function drawConnections() {
	document.querySelector("#connections-svg")?.remove();

	const body = document.querySelector("#session-body");
	if (!body) return;

	const { stageStatuses, showPreprocess, showPostprocess } = getState();

	const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
	svg.id = "connections-svg";
	svg.style.position = "absolute";
	svg.style.top = "0";
	svg.style.left = "0";
	svg.style.width = "100%";
	svg.style.height = "100%";
	svg.style.pointerEvents = "none";
	svg.style.zIndex = "0";

	const bodyRect = body.getBoundingClientRect();
	const colEls = body.querySelectorAll(".session-column");
	const cols = Array.from(colEls);

	// Determine which stages have completed or are running
	const stageKeys = ALL_STAGES.filter(
		(s) =>
			(s.key !== "Preprocess" || showPreprocess) &&
			(s.key !== "Postprocess" || showPostprocess),
	).map((s) => s.key);
	const stageReached = new Set(); // stages that have completed or are running
	for (const key of stageKeys) {
		// Check if any module in this stage has succeeded or is running
		const mods =
			cols[stageKeys.indexOf(key)]?.querySelectorAll(".session-cell") || [];
		let reached = false;
		for (const cell of mods) {
			const pid = cell.getAttribute("data-plugin-id");
			const s = stageStatuses[pid];
			if (s && (s.status === "success" || s.status === "running")) {
				reached = true;
				break;
			}
		}
		if (reached) stageReached.add(key);
	}

	for (let ci = 0; ci < cols.length - 1; ci++) {
		const fromStageKey = stageKeys[ci];
		const toStageKey = stageKeys[ci + 1];

		// Only draw this connection group if the upstream stage has been reached
		const upstreamDone = stageReached.has(fromStageKey);
		if (!upstreamDone) continue;

		const fromCells = cols[ci].querySelectorAll(".session-cell.active");
		const toCells = cols[ci + 1].querySelectorAll(".session-cell.active");

		// Is the downstream stage currently running?
		const downstreamRunning =
			stageReached.has(toStageKey) &&
			[...toCells].some((cell) => {
				const pid = cell.getAttribute("data-plugin-id");
				return stageStatuses[pid]?.status === "running";
			});

		// Has the downstream stage completed?
		const downstreamDone =
			stageReached.has(toStageKey) &&
			[...toCells].some((cell) => {
				const pid = cell.getAttribute("data-plugin-id");
				return stageStatuses[pid]?.status === "success";
			});

		for (const fromEl of fromCells) {
			const fromId = fromEl.getAttribute("data-plugin-id");
			const fromRect = fromEl.getBoundingClientRect();
			const x1 = fromRect.right - bodyRect.left;
			const y1 = fromRect.top + fromRect.height / 2 - bodyRect.top;

			for (const toEl of toCells) {
				const toId = toEl.getAttribute("data-plugin-id");

				// Geometry preprocess feeds only geometry producer — skip all other targets
				if (
					fromId === "neo-geometry-preprocess" &&
					toId !== "neo-geometry-producer"
				)
					continue;
				// Non-geometry preprocess does not feed geometry producer
				if (
					fromId !== "neo-geometry-preprocess" &&
					toId === "neo-geometry-producer"
				)
					continue;
				// Geometry producer outputs files directly — skip serializer connections
				if (fromId === "neo-geometry-producer") continue;

				const toRect = toEl.getBoundingClientRect();
				const x2 = toRect.left - bodyRect.left;
				const y2 = toRect.top + toRect.height / 2 - bodyRect.top;

				const dx = x2 - x1;
				const cpOffset = Math.max(dx * 0.4, 12);

				const path = document.createElementNS(
					"http://www.w3.org/2000/svg",
					"path",
				);
				path.setAttribute(
					"d",
					`M${x1},${y1} C${x1 + cpOffset},${y1} ${x2 - cpOffset},${y2} ${x2},${y2}`,
				);
				path.setAttribute("fill", "none");

				if (downstreamRunning) {
					// Active flow — animated dashed
					path.setAttribute("stroke", "#5BA8C8");
					path.setAttribute("stroke-width", "1.5");
					path.setAttribute("stroke-dasharray", "5 3");
					path.style.animation = "flow-dash 0.6s linear infinite";
				} else if (downstreamDone) {
					// Completed — solid green
					path.setAttribute("stroke", "#00E676");
					path.setAttribute("stroke-width", "1.2");
				} else {
					// Upstream done, waiting for downstream — grey, visible
					path.setAttribute("stroke", "#A0A0A0");
					path.setAttribute("stroke-width", "1.2");
				}

				svg.appendChild(path);
			}
		}
	}

	// ── Geometry skip connection: producer → waypoint dot in Serialize column → file exporter ──
	// Only drawn when geometry producer is active. The dot sits in the Serialize column at the
	// same vertical height as the geometry producer cell, making the bypass visually obvious.
	const geoProducerEl = body.querySelector(
		'.session-cell.active[data-plugin-id="neo-geometry-producer"]',
	);
	const fileExporterEl = body.querySelector(
		'.session-cell.active[data-plugin-id="neo-file-export"]',
	);
	const serializeColIdx = stageKeys.indexOf("Serialize");
	const serializeColEl = serializeColIdx >= 0 ? cols[serializeColIdx] : null;

	if (geoProducerEl && fileExporterEl && serializeColEl) {
		const geoRect = geoProducerEl.getBoundingClientRect();
		const expRect = fileExporterEl.getBoundingClientRect();
		const serRect = serializeColEl.getBoundingClientRect();

		const geoY = geoRect.top + geoRect.height / 2 - bodyRect.top;
		const dotX = serRect.right - bodyRect.left;
		const dotY = geoY;

		const x1 = geoRect.right - bodyRect.left;
		const x2 = expRect.left - bodyRect.left;
		const expY = expRect.top + expRect.height / 2 - bodyRect.top;

		// Same color/animation rules as normal adjacent connections:
		// upstream (Produce) must be reached; color driven by export stage state.
		const upstreamReached = stageReached.has("Produce");
		if (!upstreamReached) {
			body.style.position = "relative";
			body.appendChild(svg);
			return;
		}

		const exportCells =
			cols[stageKeys.indexOf("Export")]?.querySelectorAll(
				".session-cell.active",
			) || [];
		const skipDownstreamRunning =
			stageReached.has("Export") &&
			[...exportCells].some(
				(c) =>
					stageStatuses[c.getAttribute("data-plugin-id")]?.status === "running",
			);
		const skipDownstreamDone =
			stageReached.has("Export") &&
			[...exportCells].some(
				(c) =>
					stageStatuses[c.getAttribute("data-plugin-id")]?.status === "success",
			);

		const stroke = skipDownstreamDone
			? "#00E676"
			: skipDownstreamRunning
				? "#5BA8C8"
				: "#A0A0A0";
		const animated = skipDownstreamRunning;

		function makePath(d) {
			const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
			p.setAttribute("d", d);
			p.setAttribute("fill", "none");
			p.setAttribute("stroke", stroke);
			p.setAttribute("stroke-width", "1.2");
			if (animated) {
				p.setAttribute("stroke-dasharray", "5 3");
				p.style.animation = "flow-dash 0.6s linear infinite";
			}
			return p;
		}

		// Segment 1: geo producer → waypoint dot
		const cp1 = Math.max((dotX - x1) * 0.4, 12);
		svg.appendChild(
			makePath(
				`M${x1},${geoY} C${x1 + cp1},${geoY} ${dotX - cp1},${dotY} ${dotX},${dotY}`,
			),
		);

		// Segment 2: waypoint dot → file exporter
		const cp2 = Math.max((x2 - dotX) * 0.4, 12);
		svg.appendChild(
			makePath(
				`M${dotX},${dotY} C${dotX + cp2},${dotY} ${x2 - cp2},${expY} ${x2},${expY}`,
			),
		);

		// Waypoint dot
		const circle = document.createElementNS(
			"http://www.w3.org/2000/svg",
			"circle",
		);
		circle.setAttribute("cx", dotX);
		circle.setAttribute("cy", dotY);
		circle.setAttribute("r", "4");
		circle.setAttribute("fill", stroke);
		svg.appendChild(circle);
	}

	body.style.position = "relative";
	body.appendChild(svg);
}

function shortName(name) {
	return name
		.replace(/^Built-in /, "")
		.replace(" producer", "")
		.replace(" serializer", "")
		.replace(" enricher", "")
		.replace("Neo ", "");
}

function formatDuration(ms) {
	if (ms < 1000) return `${ms}ms`;
	return `${(ms / 1000).toFixed(2)}s`;
}

function formatTriples(count) {
	if (count < 1000) return `${count} triples`;
	if (count < 1_000_000) return `${(count / 1000).toFixed(1)}K triples`;
	return `${(count / 1_000_000).toFixed(1)}M triples`;
}

function bytesToHuman(bytes) {
	if (bytes < 1024) return `${bytes}B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)}KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

function escapeHtml(s) {
	return s
		.replace(/&/g, "&amp;")
		.replace(/</g, "&lt;")
		.replace(/>/g, "&gt;")
		.replace(/"/g, "&quot;");
}

/**
 * Get available user modules that can be added to a stage.
 * Currently returns empty — in the future this will include
 * user-uploaded WASM plugins from a collection or registry.
 */
function getAvailableUserModules(stage) {
	// Future: fetch from a plugin registry or local collection
	// For now, return empty — built-in modules are already in the grid
	return [];
}

/**
 * Add a user module to the session and activate it.
 * In the future this will register the module's WASM plugin,
 * add it to the module list, and activate it.
 */
function addUserModule(modId, stage) {
	// Future: register WASM plugin, add to state.modules, then activate
	// For now, just toggle if it's already known
	toggleModule(modId);
}
