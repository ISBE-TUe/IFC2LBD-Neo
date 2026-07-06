// preload.js — Secure bridge between renderer and main process
//
// Exposes a minimal, typed API on `window.electronAPI` via contextBridge.
// The renderer checks `window.electronAPI` to detect Electron and switch
// from the WASM worker path to the IPC/sidecar path.

const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("electronAPI", {
	// Platform detection
	platform: process.platform,
	isElectron: true,

	// File dialogs
	openFile: () => ipcRenderer.invoke("dialog:openFile"),
	openDirectory: () => ipcRenderer.invoke("dialog:openDirectory"),

	// Conversion — sends file bytes, main process writes to temp + spawns CLI
	// Returns file metadata only (names, sizes, mime types) — NOT content
	runConversion: (request) => ipcRenderer.invoke("conversion:run", request),

	// Progress events — renderer subscribes, returns an unsubscribe function
	onConversionLog: (callback) => {
		const handler = (_event, line) => callback(line);
		ipcRenderer.on("conversion:log", handler);
		return () => ipcRenderer.removeListener("conversion:log", handler);
	},
	onStageEvent: (callback) => {
		const handler = (_event, data) => callback(data);
		ipcRenderer.on("conversion:stageEvent", handler);
		return () => ipcRenderer.removeListener("conversion:stageEvent", handler);
	},

	// Open URL in external browser (for download links that don't work in file://)
	openExternal: (url) => ipcRenderer.invoke("shell:openExternal", { url }),

	// File saving — main process copies a single file from temp dir to
	// the user's chosen path via a native Save File dialog.
	showSaveDialog: (defaultFileName) =>
		ipcRenderer.invoke("dialog:showSaveDialog", { defaultFileName }),
	saveOutputFile: (tempDir, fileName, targetPath) =>
		ipcRenderer.invoke("files:copyFile", { tempDir, fileName, targetPath }),
});
