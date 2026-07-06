# IFC2LBD-Neo Desktop (Electron)

Wraps the web UI in an Electron app that uses the native CLI binary as a
sidecar process instead of WebAssembly. This gives full native threading
(rayon), no memory limits, and no browser memory caps.

## Architecture

```
Renderer (web UI)
  │  window.electronAPI detected → IPC path
  ▼
Preload (contextBridge)
  │
  ▼
Main process
  └─ child_process.spawn(ifc2lbd-neo, [...args])
       └─ rayon thread pool (native OS threads)
       └─ writes output to temp dir
       └─ stderr → parsed for stage events → IPC → UI
```

The renderer code in `web/wasm-prototype/src/pipeline/app.js` detects
Electron via `window.electronAPI.isElectron` and switches from the WASM
worker path to the IPC/sidecar path. Module metadata is provided statically
by `module-metadata.js` instead of from WASM bindings.

## Development

```bash
# Terminal 1: start the Vite dev server
cd web/wasm-prototype && npm run dev

# Terminal 2: start Electron
cd electron && npm install && npm run dev
```

Electron loads `http://localhost:3031` in development. The web app detects
`window.electronAPI` and switches to the native CLI path automatically.

## Building

The desktop apps are built by `.github/workflows/build-desktop.yml`, which
runs after `build-cli.yml` has published CLI binaries to a GitHub Release.
The CLI binaries are downloaded from the release and bundled into the
Electron app via `electron-builder`'s `extraResources` config.

Manual build (requires CLI binary in `resources/bin/<platform>/`):

```bash
cd electron
mkdir -p resources/bin/macos resources/bin/win32
cp /path/to/ifc2lbd-neo resources/bin/macos/ifc2lbd-neo
cp /path/to/ifc2lbd-neo.exe resources/bin/win32/ifc2lbd-neo.exe
npm install
npm run build:mac    # → release/*.dmg
npm run build:win    # → release/*.exe
```

## Files

| File | Purpose |
|------|---------|
| `src/main.js` | Electron main process — window creation, IPC, CLI sidecar |
| `src/preload.js` | Secure contextBridge — exposes `window.electronAPI` |
| `package.json` | Electron + electron-builder config |
| `resources/bin/` | CLI binaries per platform (gitignored, populated by CI) |
