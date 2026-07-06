// ---------------------------------------------------------------------------
// cli-command.js — Generate an ifc2lbd-neo CLI command from current UI state
// ---------------------------------------------------------------------------

import { getState } from "./state.js";

// Default option values per module, matching the Rust-side defaults.
// Only options that differ from these will appear in the command.
const MODULE_DEFAULTS = {
	"neo-turtle-serializer": {
		grouping: "streaming",
		layout: "joined",
	},
	"neo-nquads-chunked-serializer": {
		chunking: "lines",
		chunk_size_lines: "2000000",
		chunk_size_bytes: "268435456",
		chunk_prefix: "out",
		graph_naming: "producers",
	},
	"neo-nquads-serializer": {
		graph_naming: "producers",
	},
	"neo-bsdd-producer": {
		profile: "base",
		compact: "false",
		include_standard_attrs: "true",
		dedup_properties: "false",
	},
	"neo-geometry-preprocess": {
		metadata: "full",
	},
	"neo-geometry-producer": {
		format: "fragments",
	},
	"neo-ifcowl-producer": {
		mode: "full",
	},
	"neo-file-export": {
		compress: "none",
	},
};

function outputExtension(activeModules) {
	if (
		activeModules.has("neo-nquads-serializer") ||
		activeModules.has("neo-nquads-chunked-serializer")
	) {
		return ".nq";
	}
	return ".ttl";
}

export function generateCliCommand() {
	const { activeModules, moduleOptions, baseUri, outputStem, ifcFile } =
		getState();

	const lines = [];
	const inputFile = shellQuote(ifcFile?.name ?? "input.ifc");

	// --output
	const outputFile = shellQuote(
		`${outputStem}${outputExtension(activeModules)}`,
	);

	// --base-uri
	const baseUriQ = shellQuote(baseUri);

	// Build --module flags
	const moduleFlags = [];
	for (const id of [...activeModules].sort()) {
		moduleFlags.push("--module", id);
	}

	// Build --module-opt flags
	const moduleOptFlags = [];
	for (const id of [...activeModules].sort()) {
		const defaults = MODULE_DEFAULTS[id];
		if (!defaults) continue;
		const userOpts = moduleOptions[id] ?? {};
		for (const [key, def] of Object.entries(defaults)) {
			const value =
				userOpts[key] !== undefined && userOpts[key] !== ""
					? userOpts[key]
					: def;
			moduleOptFlags.push("--module-opt", `${id}.${key}=${value}`);
		}
	}

	// Detect platform for the correct binary name
	const cliBin = detectPlatform() === "windows" ? "ifc2lbd-neo-cli-windows.exe" : `ifc2lbd-neo-cli-${detectPlatform() || "linux"}`;

	// Multiline with backslash continuation
	lines.push(`${cliBin} \\`);
	lines.push(`  ${inputFile} \\`);
	lines.push(`  --output ${outputFile} \\`);
	lines.push(
		`  --base-uri ${baseUriQ}` +
			(moduleFlags.length || moduleOptFlags.length ? " \\" : ""),
	);

	for (let i = 0; i < moduleFlags.length; i += 2) {
		const isLast = i + 2 >= moduleFlags.length && moduleOptFlags.length === 0;
		lines.push(
			`  ${moduleFlags[i]} ${moduleFlags[i + 1]}` + (isLast ? "" : " \\"),
		);
	}

	for (let i = 0; i < moduleOptFlags.length; i += 2) {
		const isLast = i + 2 >= moduleOptFlags.length;
		lines.push(
			`  ${moduleOptFlags[i]} ${moduleOptFlags[i + 1]}` + (isLast ? "" : " \\"),
		);
	}

	return lines.join("\n");
}

function shellQuote(str) {
	if (/^[a-zA-Z0-9_./:@=-]+$/.test(str)) return str;
	return `'${str.replace(/'/g, "'\\''")}'`;
}

// ---------------------------------------------------------------------------
// Modal UI — markup lives in index.html, we just wire and show it
// ---------------------------------------------------------------------------

let wired = false;

function wireModal() {
	if (wired) return;
	wired = true;
	document
		.querySelector(".cli-modal-backdrop")
		?.addEventListener("click", closeModal);
	document
		.querySelector("#cli-modal-close")
		?.addEventListener("click", closeModal);
	document.querySelector("#cli-modal-copy")?.addEventListener("click", () => {
		const text = document.querySelector("#cli-modal-pre")?.textContent ?? "";
		navigator.clipboard.writeText(text).then(() => {
			const btn = document.querySelector("#cli-modal-copy");
			if (btn) {
				btn.textContent = "Copied!";
				setTimeout(() => {
					btn.textContent = "Copy";
				}, 1500);
			}
		});
	});

	// Download bin buttons — detect OS and highlight
	const binLinks = document.querySelectorAll(".cli-modal-bin-link");
	if (binLinks.length) {
		const platform = detectPlatform();
		binLinks.forEach((link) => {
			const target = link.dataset.platform;
			if (target === platform) {
				link.classList.add("recommended");
			}
			// In Electron, <a href="https://..."> doesn't open in an external
			// browser by default. Intercept clicks and open via the shell.
			if (window.electronAPI?.isElectron) {
				link.addEventListener("click", (e) => {
					e.preventDefault();
					window.electronAPI.openExternal(link.href);
				});
			}
		});
	}
}

function closeModal() {
	document.querySelector("#cli-modal")?.classList.remove("open");
}

function detectPlatform() {
	const ua = navigator.userAgent.toLowerCase();
	if (ua.includes("win")) return "windows";
	if (ua.includes("mac")) return "macos";
	if (ua.includes("linux")) return "linux";
	return null;
}

export function showCliCommand() {
	wireModal();
	const pre = document.querySelector("#cli-modal-pre");
	if (pre) pre.textContent = generateCliCommand();
	document.querySelector("#cli-modal")?.classList.add("open");
}
