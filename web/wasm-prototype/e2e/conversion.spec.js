// @ts-check
import { test, expect } from "@playwright/test";
import path from "path";
import fs from "fs";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PUBLIC = path.join(__dirname, "../public");
const WORKSPACE_ROOT = path.join(__dirname, "../../../");

// Large real-world models kept outside public/ (never committed)
const CX_MODEL = path.join(
  WORKSPACE_ROOT,
  "CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc"
);

// Template IDs from TEMPLATES array in app.js
const TPL_TURTLE_JOINED = "core-turtle-joined";
const TPL_NQUADS = "core-ifcowl-nq";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function waitForWasm(page) {
  // Pipeline columns are rendered only after initWasm() + initSession() complete.
  await page.waitForSelector(".session-column", { timeout: 60_000 });
}

async function applyTemplate(page, templateId) {
  const picker = page.locator("#template-picker");
  await picker.selectOption(templateId);
  // Picker resets to "" after applying — just wait a tick
  await page.waitForTimeout(100);
}

async function uploadFile(page, filename) {
  const input = page.locator("#file-input");
  await input.setInputFiles(path.join(PUBLIC, filename));
}

// For files too large to transfer over CDP, fetch them inside the browser
// from the already-served public/ directory (Docker volume-mounts public/).
async function uploadFileViaFetch(page, publicFilename) {
  await page.evaluate(async (filename) => {
    const res = await fetch(`/${filename}`);
    if (!res.ok) throw new Error(`fetch failed: ${res.status} ${filename}`);
    const buf = await res.arrayBuffer();
    const file = new File([buf], filename, { type: "application/octet-stream" });
    const dt = new DataTransfer();
    dt.items.add(file);
    const input = document.querySelector("#file-input");
    input.files = dt.files;
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }, publicFilename);
}

async function waitForConversion(page, timeoutMs = 2 * 60 * 1000) {
  await expect(page.locator("#runtime-info")).toContainText("Finished in", {
    timeout: timeoutMs,
  });
}

async function downloadFileAsText(page, selector) {
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    page.locator(selector).click(),
  ]);
  const stream = await download.createReadStream();
  const chunks = [];
  for await (const chunk of stream) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf-8");
}

// ---------------------------------------------------------------------------
// 1. Smoke test — tiny file, no JS errors
// ---------------------------------------------------------------------------

test("smoke: test.ifc converts without JS errors", async ({ page }) => {
  const jsErrors = [];
  page.on("pageerror", (err) => jsErrors.push(err.message));

  await page.goto("/");
  await waitForWasm(page);
  await applyTemplate(page, TPL_TURTLE_JOINED);
  await uploadFile(page, "test.ifc");
  await page.locator("#btn-run").click();
  await waitForConversion(page);

  expect(jsErrors).toHaveLength(0);

  // At least one download link appeared
  const links = page.locator(".download-link");
  await expect(links.first()).toBeVisible();
});

// ---------------------------------------------------------------------------
// 2. NQuads named graphs — per-producer IRIs, no legacy /lbd graph
// ---------------------------------------------------------------------------

test("nquads: per-producer named graph IRIs are correct (DigitalHub.ifc)", async ({
  page,
}) => {
  const jsErrors = [];
  page.on("pageerror", (err) => jsErrors.push(err.message));

  await page.goto("/");
  await waitForWasm(page);
  await applyTemplate(page, TPL_NQUADS);
  await uploadFile(page, "DigitalHub.ifc");
  await page.locator("#btn-run").click();
  await waitForConversion(page, 3 * 60 * 1000);

  expect(jsErrors).toHaveLength(0);

  const nqLink = page.locator('.download-link[download$=".nq"]').first();
  await expect(nqLink).toBeVisible({ timeout: 10_000 });

  const nqContent = await downloadFileAsText(
    page,
    '.download-link[download$=".nq"]'
  );

  // Must contain per-producer graph IRIs
  expect(nqContent).toContain("/bot>");
  expect(nqContent).toContain("/beo>");

  // Must NOT contain legacy merged /lbd graph
  expect(nqContent).not.toContain("/lbd>");
});

// ---------------------------------------------------------------------------
// 3. Triple counts — non-zero quads in NQ output
// ---------------------------------------------------------------------------

test("nquads: triple counts are non-zero for active producers", async ({
  page,
}) => {
  await page.goto("/");
  await waitForWasm(page);
  await applyTemplate(page, TPL_NQUADS);
  await uploadFile(page, "test.ifc");
  await page.locator("#btn-run").click();
  await waitForConversion(page);

  const nqLink = page.locator('.download-link[download$=".nq"]').first();
  await expect(nqLink).toBeVisible({ timeout: 10_000 });

  const nqContent = await downloadFileAsText(
    page,
    '.download-link[download$=".nq"]'
  );

  const quads = nqContent
    .split("\n")
    .filter((l) => l.trim().length > 0 && !l.startsWith("#"));
  expect(quads.length).toBeGreaterThan(0);
});

// ---------------------------------------------------------------------------
// 4. Turtle output — basic smoke
// ---------------------------------------------------------------------------

test("turtle: test.ifc produces non-empty .ttl file", async ({ page }) => {
  const jsErrors = [];
  page.on("pageerror", (err) => jsErrors.push(err.message));

  await page.goto("/");
  await waitForWasm(page);
  await applyTemplate(page, TPL_TURTLE_JOINED);
  await uploadFile(page, "test.ifc");
  await page.locator("#btn-run").click();
  await waitForConversion(page);

  expect(jsErrors).toHaveLength(0);

  const ttlLink = page.locator('.download-link[download$=".ttl"]').first();
  await expect(ttlLink).toBeVisible({ timeout: 10_000 });

  const ttlContent = await downloadFileAsText(
    page,
    '.download-link[download$=".ttl"]'
  );
  expect(ttlContent.length).toBeGreaterThan(100);
  expect(ttlContent).toContain("@prefix");
});

// ---------------------------------------------------------------------------
// 5. Large model — CX 163 MB (skipped if file not present)
// ---------------------------------------------------------------------------

// Copy large model into public/ so Docker can serve it, then remove after test.
// This avoids sending 163MB over the Chrome DevTools Protocol (CDP has size limits).
const CX_PUBLIC_NAME = "cx-model-test.ifc";
const CX_PUBLIC_PATH = path.join(PUBLIC, CX_PUBLIC_NAME);

test.beforeAll(() => {
  if (fs.existsSync(CX_MODEL)) {
    fs.copyFileSync(CX_MODEL, CX_PUBLIC_PATH);
  }
});

test.afterAll(() => {
  if (fs.existsSync(CX_PUBLIC_PATH)) {
    fs.unlinkSync(CX_PUBLIC_PATH);
  }
});

test("large: CX model converts to Turtle without JS errors", async ({
  page,
}) => {
  test.skip(!fs.existsSync(CX_MODEL), "CX model not found — skipping");
  test.setTimeout(12 * 60 * 1000);

  const jsErrors = [];
  page.on("pageerror", (err) => jsErrors.push(err.message));

  await page.goto("/");
  await waitForWasm(page);
  await applyTemplate(page, TPL_TURTLE_JOINED);
  await uploadFileViaFetch(page, CX_PUBLIC_NAME);
  await page.locator("#btn-run").click();

  await waitForConversion(page, 10 * 60 * 1000);

  if (jsErrors.length) console.log("JS errors:", jsErrors);
  expect(jsErrors).toHaveLength(0);

  const ttlLink = page.locator('.download-link[download$=".ttl"]').first();
  await expect(ttlLink).toBeVisible({ timeout: 10_000 });
});

test("large: CX model NQuads named graphs are per-producer", async ({
  page,
}) => {
  test.skip(!fs.existsSync(CX_MODEL), "CX model not found — skipping");
  test.setTimeout(12 * 60 * 1000);

  const jsErrors = [];
  page.on("pageerror", (err) => jsErrors.push(err.message));

  await page.goto("/");
  await waitForWasm(page);
  await applyTemplate(page, TPL_NQUADS);
  await uploadFileViaFetch(page, CX_PUBLIC_NAME);
  await page.locator("#btn-run").click();

  await waitForConversion(page, 10 * 60 * 1000);

  if (jsErrors.length) console.log("JS errors:", jsErrors);
  expect(jsErrors).toHaveLength(0);

  const nqLink = page.locator('.download-link[download$=".nq"]').first();
  await expect(nqLink).toBeVisible({ timeout: 10_000 });

  const nqContent = await downloadFileAsText(
    page,
    '.download-link[download$=".nq"]'
  );

  expect(nqContent).toContain("/bot>");
  expect(nqContent).not.toContain("/lbd>");

  const quads = nqContent
    .split("\n")
    .filter((l) => l.trim().length > 0 && !l.startsWith("#"));
  expect(quads.length).toBeGreaterThan(1000);
  console.log(`CX model produced ${quads.length.toLocaleString()} quads`);
});
