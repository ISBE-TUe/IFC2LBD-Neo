// main.js — Electron main process
//
// Responsibilities:
//   1. Create the browser window and load the renderer (web app)
//   2. Handle IPC: spawn the bundled ifc2lbd-neo CLI as a sidecar process
//   3. Parse CLI stderr (tracing output) for stage progress events
//   4. Read output files from the temp directory and send them back
//   5. Clean up temp files on completion
//
// The renderer (web app) detects Electron via `window.electronAPI` and
// uses IPC instead of the WASM worker for conversions.

const { app, BrowserWindow, ipcMain, dialog } = require("electron");
const { spawn } = require("node:child_process");
const {
  mkdirSync,
  rmSync,
  writeFileSync,
  readFileSync,
  readdirSync,
  statSync,
} = require("node:fs");
const { join, dirname, basename, extname } = require("node:path");
const { tmpdir } = require("node:os");

const __dirname = __dirname;

// ── CLI binary path ─────────────────────────────────────────────────────────
//
// In development, the CLI is at ../resources/bin/<os>/ifc2lbd-neo.
// In production (packaged), it's in the extraResources/bin/ directory.
//
// electron-builder copies extraResources to <app>/Resources/bin/.

function getCliPath() {
	const platform = process.platform === "win32" ? "win32" : "macos";
	const exeName =
		process.platform === "win32" ? "ifc2lbd-neo.exe" : "ifc2lbd-neo";

	// Packaged: process.resourcesPath points to the Resources dir
	if (process.resourcesPath) {
		return join(process.resourcesPath, "bin", exeName);
	}

	// Development: relative to the electron/ directory
	return join(__dirname, "..", "resources", "bin", platform, exeName);
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
			// Electron doesn't need cross-origin isolation (no SharedArrayBuffer)
			// because we use IPC instead of WASM workers.
		},
	});

	// In development, load the Vite dev server. In production, load the built files.
	const isDev = !app.isPackaged;
	if (isDev) {
		// Vite dev server — start with `npm run dev` in web/wasm-prototype/
		mainWindow.loadURL("http://localhost:3031");
		mainWindow.webContents.openDevTools();
	} else {
		// Load the bundled renderer from the web app's dist/
		mainWindow.loadFile(join(__dirname, "..", "renderer", "index.html"));
	}

	mainWindow.on("closed", () => {
		mainWindow = null;
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
	const filePath = result.filePaths[0];
	const content = readFileSync(filePath);
	return {
		name: basename(filePath),
		path: filePath,
		size: content.byteLength,
		content: content.buffer.slice(
			content.byteOffset,
			content.byteOffset + content.byteLength,
		),
	};
});

ipcMain.handle("dialog:openDirectory", async () => {
	const result = await dialog.showOpenDialog(mainWindow, {
		title: "Select output directory",
		properties: ["openDirectory", "createDirectory"],
	});
	if (result.canceled || result.filePaths.length === 0) return null;
	return result.filePaths[0];
});

// ── IPC: Run conversion ──────────────────────────────────────────────────────
//
// The renderer sends a conversion request with:
//   { inputPath, modules, moduleOptions, baseUri, outputStem, inputFormat }
//
// The main process:
//   1. Creates a temp output directory
//   2. Builds the CLI command from the request
//   3. Spawns the CLI binary
//   4. Parses stderr for stage events and forwards them via IPC
//   5. On completion, reads output files and returns them

let activeConversion = null;

ipcMain.handle("conversion:run", async (event, request) => {
	if (activeConversion) {
		throw new Error("A conversion is already running.");
	}

	const tempDir = join(tmpdir(), `ifc2lbd-neo-${Date.now()}`);
	mkdirSync(tempDir, { recursive: true });

	const outputFile = join(
		tempDir,
		`${request.outputStem || "converted-model"}.ttl`,
	);

	// Build CLI arguments
	const args = [
		request.inputPath,
		"--output",
		outputFile,
		"--base-uri",
		request.baseUri || "https://lbd.example.com/",
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

		// Parse stderr for stage events
		// The CLI uses tracing::info! which outputs lines like:
		//   2024-01-15T10:30:00.123Z INFO ifc2lbd_cli: phase input_parsing completed in 0.123s
		//   2024-01-15T10:30:00.456Z INFO ifc2lbd_cli: module neo-bot-producer: success (1234 triples)
		child.stderr.on("data", (chunk) => {
			stderrBuffer += chunk.toString();
			const lines = stderrBuffer.split("\n");
			stderrBuffer = lines.pop() || "";

			for (const line of lines) {
				if (!line.trim()) continue;
				// Forward all log lines to the renderer
				mainWindow?.webContents.send("conversion:log", line.trim());

				// Try to parse stage events
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

			// Read output files from temp directory
			const outputFiles = [];
			try {
				const entries = readdirSync(tempDir);
				for (const entry of entries) {
					const fullPath = join(tempDir, entry);
					if (!statSync(fullPath).isFile()) continue;
					const content = readFileSync(fullPath);
					const ext = extname(entry);
					let mimeType = "application/octet-stream";
					if (ext === ".ttl") mimeType = "text/turtle";
					else if (ext === ".nq") mimeType = "application/n-quads";
					else if (ext === ".json") mimeType = "application/json";

					outputFiles.push({
						filename: entry,
						mimeType,
						content: content.buffer.slice(
							content.byteOffset,
							content.byteOffset + content.byteLength,
						),
					});
				}
			} catch (readErr) {
				rmSync(tempDir, { recursive: true, force: true });
				reject(new Error(`Failed to read output: ${readErr.message}`));
				return;
			}

			rmSync(tempDir, { recursive: true, force: true });
			resolve({ files: outputFiles });
		});
	});
});

// ── IPC: Save output files ──────────────────────────────────────────────────

ipcMain.handle("files:save", async (event, { directory, files }) => {
	let saved = 0;
	let totalBytes = 0;
	for (const file of files) {
		const filePath = join(directory, file.filename);
		writeFileSync(filePath, Buffer.from(file.content));
		saved++;
		totalBytes += file.content.byteLength;
	}
	return { saved, totalBytes };
});
