import { access, copyFile, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

const root = process.cwd();
const publicDir = path.join(root, "public");
const target = path.join(publicDir, "thatopen-worker.mjs");

const candidates = [
  path.join(root, "node_modules", "@thatopen", "fragments", "dist", "Worker", "worker.mjs"),
  path.join(root, "node_modules", "@thatopen", "fragments", "dist", "worker.mjs"),
  path.join(root, "node_modules", "@thatopen", "fragments", "resources", "worker.mjs"),
];

const ensureDir = async () => {
  await mkdir(publicDir, { recursive: true });
};

const pathExists = async (candidate) => {
  try {
    await access(candidate);
    return true;
  } catch {
    return false;
  }
};

const copyWorkerFromNodeModules = async () => {
  for (const candidate of candidates) {
    if (await pathExists(candidate)) {
      await copyFile(candidate, target);
      console.log(`Copied worker from ${candidate}`);
      return true;
    }
  }

  return false;
};

const downloadWorker = async () => {
  const response = await fetch("https://thatopen.github.io/engine_fragment/resources/worker.mjs");
  if (!response.ok) {
    throw new Error(`Failed to download worker: HTTP ${response.status}`);
  }

  const source = await response.text();
  await writeFile(target, source, "utf8");
  console.log("Downloaded worker from That Open docs resources");
};

await ensureDir();

if (!(await copyWorkerFromNodeModules())) {
  await downloadWorker();
}
