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

	// Conversion
	runConversion: (request) => ipcRenderer.invoke("conversion:run", request),

	// Progress events — renderer subscribes to these
	onConversionLog: (callback) =>
		ipcRenderer.on("conversion:log", (_event, line) => callback(line)),
	onStageEvent: (callback) =>
		ipcRenderer.on("conversion:stageEvent", (_event, data) => callback(data)),

	// File saving
	saveFiles: (directory, files) =>
		ipcRenderer.invoke("files:save", { directory, files }),
});
