const fs = require("fs");
const os = require("os");
const { performance } = require("perf_hooks");

const wasm = require("../artifacts/wasm-node/ifc2lbd_wasm.js");

const INPUT_PATH = process.argv[2] || "DigitalHub_FM-ARC_v2.ifc";
const RUNS = Number(process.argv[3] || 3);

const request = {
  moduleIds: [
    "neo-lbd-producer",
    "neo-ifcowl-producer",
    "neo-nquads-serializer",
    "neo-file-export",
  ],
  moduleOptions: ["neo-nquads-serializer.chunking=none"],
  baseUri: "https://example.test/base/",
  outputStem: "digitalhub-wasm",
};

async function main() {
  const inputBytes = fs.readFileSync(INPUT_PATH);
  const threads = Math.max(2, os.cpus().length);
  await wasm.initThreadPool(threads);

  const timesMs = [];
  for (let i = 0; i < RUNS; i += 1) {
    const t0 = performance.now();
    const bundle = wasm.benchmarkConvertIfc(inputBytes, request);
    const t1 = performance.now();
    timesMs.push(t1 - t0);

    if (!bundle.outputFiles || bundle.outputFiles.length !== 1) {
      throw new Error("expected exactly one exported file in N-Quads mode");
    }
    console.log(
      `wasm run ${i + 1}: ${((t1 - t0) / 1000).toFixed(3)}s, bytes=${bundle.totalOutputBytes}`
    );
  }

  const avgMs = timesMs.reduce((acc, value) => acc + value, 0) / timesMs.length;
  const minMs = Math.min(...timesMs);
  const maxMs = Math.max(...timesMs);
  console.log(
    `wasm summary: avg=${(avgMs / 1000).toFixed(3)}s min=${(minMs / 1000).toFixed(
      3
    )}s max=${(maxMs / 1000).toFixed(3)}s threads=${threads}`
  );
  process.exit(0);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
