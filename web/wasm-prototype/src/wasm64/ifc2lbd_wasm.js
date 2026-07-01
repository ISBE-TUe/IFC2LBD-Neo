// Stub — replaced by scripts/build_wasm_web.sh when the wasm64 build succeeds.
// If this stub is present at runtime, wasm64 is not available (wasm-bindgen-rayon
// does not yet support wasm64 threading). The worker (wasm-lowmem-worker.js)
// catches the error from default() and falls back to wasm32.
export default () => { throw new Error("wasm64 not available"); };
export const convertIfcToSink = () => { throw new Error("wasm64 not available"); };
export const initNeoThreadPool = () => { throw new Error("wasm64 not available"); };
export const listModules = () => { throw new Error("wasm64 not available"); };
export const resolvePlan = () => { throw new Error("wasm64 not available"); };
export const planExecution = () => { throw new Error("wasm64 not available"); };
export const benchmarkConvertIfc = () => { throw new Error("wasm64 not available"); };
