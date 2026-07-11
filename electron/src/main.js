// main.js — Electron main process
//
// Responsibilities:
//   1. Create the browser window and load the renderer (web app)
//   2. Handle IPC: spawn the bundled ifc2lbd-neo CLI as a sidecar process
//   3. Parse CLI stderr (tracing output) for stage progress events
//   4. On completion, return file metadata (names, sizes) — NOT content
//   5. Copy output files from temp dir to user-chosen directory
//
// The renderer (web app) detects Electron via `window.electronAPI` and
// uses IPC instead of the WASM worker for conversions.

const { app, BrowserWindow, ipcMain, dialog, shell } = require("electron");
const { spawn } = require("node:child_process");
const {
	mkdirSync,
	rmSync,
	writeFileSync,
	readdirSync,
	statSync,
	copyFileSync,
} = require("node:fs");
const { join, basename, extname } = require("node:path");
const { tmpdir } = require("node:os");

// In CommonJS, __dirname is already defined by Node.js — no need to set it.

// ── CLI binary path ─────────────────────────────────────────────────────────

function getCliPath() {
	const exeName =
		process.platform === "win32" ? "ifc2lbd-neo.exe" : "ifc2lbd-neo";

	// Packaged: process.resourcesPath points to the Resources dir
	if (process.resourcesPath) {
		return join(process.resourcesPath, "bin", exeName);
	}

	// Development: relative to the electron/ directory
	return join(__dirname, "..", "resources", "bin", "macos", exeName);
}

// ── Window management ────────────────────────────────────────────────────────

let mainWindow = null;

function createWindow() {
	mainWindow = new BrowserWindow({
		width: 1400,
		height: 900,
		minWidth: 900,
		minHeight: 600,
		title: "IFC2LBD-Neo",
		webPreferences: {
			preload: join(__dirname, "preload.js"),
			contextIsolation: true,
			nodeIntegration: false,
		},
	});

	const isDev = !app.isPackaged;
	if (isDev) {
		mainWindow.loadURL("http://localhost:3031");
		mainWindow.webContents.openDevTools();
	} else {
		mainWindow.loadFile(join(__dirname, "..", "renderer", "index.html"));
	}

	mainWindow.on("closed", () => {
		mainWindow = null;
	});

	// Open all external links (https://) in the system browser, not in Electron
	mainWindow.webContents.setWindowOpenHandler(({ url }) => {
		if (url.startsWith("https://") || url.startsWith("http://")) {
			shell.openExternal(url);
			return { action: "deny" };
		}
		return { action: "allow" };
	});
}

app.whenReady().then(() => {
	createWindow();

	app.on("activate", () => {
		if (BrowserWindow.getAllWindows().length === 0) createWindow();
	});
});

app.on("window-all-closed", () => {
	if (process.platform !== "darwin") app.quit();
});

// ── IPC: Navigate to Viewer (same window) ────────────────────────────────────
// Navigates the main window to the viewer web app (no new window).
// The viewer needs SharedArrayBuffer for oxigraph, so COOP/COEP headers
// must be set on the main window's session.

let isViewerLoaded = false;

function setupCrossOriginIsolation(win) {
	win.webContents.session.webRequest.onHeadersReceived(
		(details, callback) => {
			callback({
				responseHeaders: {
					...details.responseHeaders,
					"Cross-Origin-Opener-Policy": ["same-origin"],
					"Cross-Origin-Embedder-Policy": ["require-corp"],
				},
			});
		},
	);
}

// Set up COOP/COEP on the main window from the start so the viewer
// gets crossOriginIsolated when loaded.
setupCrossOriginIsolation(mainWindow);

ipcMain.handle("viewer:open", async () => {
	const isDev = !app.isPackaged;
	isViewerLoaded = true;
	if (isDev) {
		mainWindow.loadURL("http://localhost:3004");
	} else {
		mainWindow.loadFile(
			join(__dirname, "..", "renderer", "viewer", "index.html"),
		);
	}
	mainWindow.title = "IFC2LBD-Neo Debug Viewer";
});

ipcMain.handle("viewer:navigateBack", async () => {
	const isDev = !app.isPackaged;
	isViewerLoaded = false;
	if (isDev) {
		mainWindow.loadURL("http://localhost:3031");
	} else {
		mainWindow.loadFile(join(__dirname, "..", "renderer", "index.html"));
	}
	mainWindow.title = "IFC2LBD-Neo";
});

// ── IPC: File dialog ─────────────────────────────────────────────────────────

ipcMain.handle("dialog:openFile", async () => {
	const result = await dialog.showOpenDialog(mainWindow, {
		title: "Select IFC file",
		filters: [
			{ name: "IFC files", extensions: ["ifc", "ifcxml", "step"] },
			{ name: "All files", extensions: ["*"] },
		],
		properties: ["openFile"],
	});
	if (result.canceled || result.filePaths.length === 0) return null;
	return result.filePaths[0];
});

ipcMain.handle("dialog:openDirectory", async () => {
	const result = await dialog.showOpenDialog(mainWindow, {
		title: "Select output directory",
		properties: ["openDirectory", "createDirectory"],
	});
	if (result.canceled || result.filePaths.length === 0) return null;
	return result.filePaths[0];
});

ipcMain.handle("dialog:showSaveDialog", async (_event, { defaultFileName }) => {
	const result = await dialog.showSaveDialog(mainWindow, {
		title: "Save output file",
		defaultPath: defaultFileName || "output.ttl",
	});
	if (result.canceled || !result.filePath) return null;
	return result.filePath;
});

ipcMain.handle("shell:openExternal", async (_event, { url }) => {
	await shell.openExternal(url);
});

// ── IPC: Run conversion ──────────────────────────────────────────────────────
//
// The renderer sends: { fileName, fileData (ArrayBuffer), modules, moduleOptions, ... }
// The main process writes the input to a temp dir, spawns the CLI, parses
// stderr for stage events, and returns file METADATA only (no content).
// The renderer then asks the user where to save, and the main process copies
// files from the temp dir to the target directory.

let activeConversion = null;

ipcMain.handle("conversion:run", async (_event, request) => {
	if (activeConversion) {
		throw new Error("A conversion is already running.");
	}

	const tempDir = join(tmpdir(), `ifc2lbd-neo-${Date.now()}`);
	mkdirSync(tempDir, { recursive: true });

	// Write input file to temp directory
	const inputFile = join(tempDir, request.fileName || "input.ifc");
	if (request.fileData) {
		writeFileSync(inputFile, Buffer.from(request.fileData));
	}

	// Output path — extension is determined by the active serializer
	// (Turtle → .ttl, N-Quads → .nq). The renderer sends the correct extension.
	const outputExt = request.outputExt || ".ttl";
	const outputFile = join(
		tempDir,
		`${request.outputStem || "converted-model"}${outputExt}`,
	);

	// Build CLI arguments
	const args = [
		inputFile,
		"--output",
		outputFile,
		"--base-uri",
		request.baseUri || "https://lbd.org/",
	];

	if (request.inputFormat === "structured-data") {
		args.push("--input-format", "structured-data");
	}

	for (const mod of request.modules || []) {
		args.push("--module", mod);
	}

	for (const opt of request.moduleOptions || []) {
		args.push("--module-opt", opt);
	}

	return new Promise((resolve, reject) => {
		const cliPath = getCliPath();
		const child = spawn(cliPath, args, {
			stdio: ["ignore", "pipe", "pipe"],
			env: { ...process.env, RUST_LOG: "info" },
		});

		activeConversion = { child, tempDir };

		let stderrBuffer = "";

		child.stderr.on("data", (chunk) => {
			stderrBuffer += chunk.toString();
			const lines = stderrBuffer.split("\n");
			stderrBuffer = lines.pop() || "";

			for (const line of lines) {
				if (!line.trim()) continue;
				mainWindow?.webContents.send("conversion:log", line.trim());

				// Parse stage events: "module <id>: running/success/failed"
				const stageMatch = line.match(
					/module\s+(\S+):\s+(success|failed|running)/,
				);
				if (stageMatch) {
					const [, pluginId, status] = stageMatch;
					let durationMs = 0;
					let triplesOut = 0;

					const durMatch = line.match(/([\d.]+)s/);
					if (durMatch) durationMs = Math.round(parseFloat(durMatch[1]) * 1000);

					const tripMatch = line.match(/\((\d+)\s+triples\)/);
					if (tripMatch) triplesOut = parseInt(tripMatch[1], 10);

					mainWindow?.webContents.send("conversion:stageEvent", {
						pluginId,
						status,
						durationMs,
						triplesOut,
					});
				}
			}
		});

		child.on("error", (err) => {
			activeConversion = null;
			rmSync(tempDir, { recursive: true, force: true });
			reject(new Error(`Failed to start CLI: ${err.message}`));
		});

		child.on("close", (code) => {
			activeConversion = null;

			if (code !== 0) {
				rmSync(tempDir, { recursive: true, force: true });
				reject(new Error(`CLI exited with code ${code}`));
				return;
			}

			// Return file METADATA only — no content through IPC
			const fileMetadata = [];
			try {
				const entries = readdirSync(tempDir);
				for (const entry of entries) {
					// Skip the input file
					if (entry === request.fileName) continue;
					const fullPath = join(tempDir, entry);
					if (!statSync(fullPath).isFile()) continue;
					const ext = extname(entry);
					let mimeType = "application/octet-stream";
					if (ext === ".ttl") mimeType = "text/turtle";
					else if (ext === ".nq") mimeType = "application/n-quads";
					else if (ext === ".json") mimeType = "application/json";

					fileMetadata.push({
						filename: entry,
						mimeType,
						size: statSync(fullPath).size,
					});
				}
			} catch (readErr) {
				reject(new Error(`Failed to read output: ${readErr.message}`));
				return;
			}

			// Return tempDir so the renderer can request a copy later
			resolve({ tempDir, files: fileMetadata });
		});
	});
});

// ── IPC: Copy output files from temp dir to user-chosen directory ───────────

ipcMain.handle(
	"files:copyFile",
	async (_event, { tempDir, fileName, targetPath }) => {
		const srcPath = join(tempDir, fileName);
		copyFileSync(srcPath, targetPath);
		return { saved: 1, totalBytes: statSync(srcPath).size };
	},
);
