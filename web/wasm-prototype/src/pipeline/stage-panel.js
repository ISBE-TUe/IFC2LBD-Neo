// ---------------------------------------------------------------------------
// stage-panel.js — Left slide-out settings panel
// ---------------------------------------------------------------------------

import { getState, update, toggleModule, subscribe } from "./state.js";

let panelEl = null;
let backdropEl = null;

export function initStagePanel() {
	panelEl = document.querySelector("#settings-panel");
	backdropEl = document.querySelector("#settings-backdrop");

	// Close on backdrop click
	backdropEl?.addEventListener("click", () => {
		update({ settingsOpen: false });
	});

	// Close button
	panelEl?.querySelector(".panel-close")?.addEventListener("click", () => {
		update({ settingsOpen: false });
	});

	subscribe("settingsOpen", renderOpenState);
	subscribe("activeModules", renderToggles);
	subscribe("modules", renderToggles);
	subscribe("ifcFile", renderFileInfo);

	renderOpenState();
	renderToggles();
}

function renderOpenState() {
	const { settingsOpen } = getState();
	if (panelEl) panelEl.classList.toggle("open", settingsOpen);
	if (backdropEl) backdropEl.classList.toggle("open", settingsOpen);
}

function renderToggles() {
	const { activeModules, modules } = getState();
	const container = panelEl?.querySelector("#plugin-toggles");
	if (!container) return;

	container.innerHTML = "";

	// Group by stage
	const stages = ["Produce", "Postprocess", "Serialize", "Export"];
	for (const stage of stages) {
		const stageMods = modules.filter((m) => m.stage === stage);
		if (stageMods.length === 0) continue;

		// Stage header
		const header = document.createElement("div");
		header.className = "stage-header";
		header.textContent = stage.toUpperCase();
		container.appendChild(header);

		for (const mod of stageMods) {
			const isActive = activeModules.has(mod.id);
			const isRequired = mod.id === "neo-file-export";
			const isSerializer = stage === "Serialize";

			const row = document.createElement("label");
			row.className = "plugin-row";

			if (isSerializer) {
				// Radio button for serializers (mutually exclusive)
				const radio = document.createElement("input");
				radio.type = "radio";
				radio.name = "serializer";
				radio.value = mod.id;
				radio.checked = isActive;
				radio.disabled = isRequired;
				radio.addEventListener("change", () => {
					// Deactivate other serializers, activate this one
					const mods = new Set(activeModules);
					for (const sm of modules.filter((m) => m.stage === "Serialize")) {
						mods.delete(sm.id);
					}
					mods.add(mod.id);
					update({ activeModules: mods });
				});
				row.appendChild(radio);
			} else {
				// Checkbox for producers/enrichers
				const checkbox = document.createElement("input");
				checkbox.type = "checkbox";
				checkbox.checked = isActive;
				checkbox.disabled = isRequired;
				checkbox.addEventListener("change", () => toggleModule(mod.id));
				row.appendChild(checkbox);
			}

			const label = document.createElement("span");
			label.className = "plugin-label";
			label.textContent = mod.displayName;
			row.appendChild(label);

			if (isRequired) {
				const badge = document.createElement("span");
				badge.className = "required-badge";
				badge.textContent = "required";
				row.appendChild(badge);
			}

			if (!mod.wasmCompatible) {
				const badge = document.createElement("span");
				badge.className = "incompatible-badge";
				badge.textContent = "not in browser";
				row.appendChild(badge);
			}

			container.appendChild(row);
		}
	}
}

function renderFileInfo() {
	const { ifcFile } = getState();
	const fileLabel = panelEl?.querySelector("#file-name");
	if (fileLabel) {
		fileLabel.textContent = ifcFile ? ifcFile.name : "No file selected";
	}
}
