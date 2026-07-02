// ---------------------------------------------------------------------------
// dag.js — SVG DAG renderer for the pipeline visualization
// ---------------------------------------------------------------------------

import { getState, subscribe } from "./state.js";

const STAGE_ORDER = ["Preprocess", "Produce", "Postprocess", "Serialize", "Export"];
const STAGE_COLORS = {
  idle: "#C8C8C8",
  running: "#7EC8E3",
  success: "#8BC9A0",
  failed: "#E08B8B",
  warning: "#E8C872",
  skipped: "#B8B8B8",
};

let svgEl = null;
let containerEl = null;

export function initDAG(container) {
  containerEl = container;
  svgEl = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svgEl.setAttribute("width", "100%");
  svgEl.setAttribute("height", "100%");
  svgEl.style.overflow = "visible";
  container.appendChild(svgEl);

  subscribe("activeModules", render);
  subscribe("modules", render);
  subscribe("stageStatuses", render);
  subscribe("running", render);
  subscribe("selectedPluginId", render);

  render();
}

function getStageForModule(mod, activeModules) {
  // Map the module stage to the display stage
  if (mod.stage === "Produce") return "Produce";
  if (mod.stage === "Postprocess") return "Postprocess";
  if (mod.stage === "Serialize") return "Serialize";
  if (mod.stage === "Export") return "Export";
  return mod.stage;
}

function render() {
  if (!svgEl) return;
  const state = getState();
  const { activeModules, modules, stageStatuses, running, selectedPluginId } = state;

  // Group modules by display stage
  const stageGroups = {};
  for (const stage of STAGE_ORDER) {
    stageGroups[stage] = [];
  }

  for (const mod of modules) {
    const stage = getStageForModule(mod, activeModules);
    if (!stageGroups[stage]) stageGroups[stage] = [];
    stageGroups[stage].push(mod);
  }

  // Filter to only show active modules + parse
  const activeMods = modules.filter(
    (m) => activeModules.has(m.id) || m.id === "parse"
  );

  // Rebuild stage groups with only active modules
  const activeGroups = {};
  for (const stage of STAGE_ORDER) {
    activeGroups[stage] = [];
  }
  // Add parse pseudo-module
  activeGroups["Preprocess"].push({
    id: "parse",
    displayName: "Parse IFC",
    stage: "Preprocess",
    failurePolicy: "Required",
  });
  for (const mod of activeMods) {
    const stage = getStageForModule(mod, activeModules);
    if (activeGroups[stage]) {
      activeGroups[stage].push(mod);
    }
  }

  // Clear SVG
  while (svgEl.firstChild) svgEl.removeChild(svgEl.firstChild);

  // Layout constants
  const nodeW = 160;
  const nodeH = 56;
  const gapX = 80;
  const gapY = 16;
  const padX = 40;
  const padY = 40;

  // Calculate positions
  const positions = new Map(); // id -> { x, y, w, h }
  let maxX = 0;
  let maxY = 0;

  for (let si = 0; si < STAGE_ORDER.length; si++) {
    const stage = STAGE_ORDER[si];
    const nodes = activeGroups[stage];
    if (!nodes || nodes.length === 0) continue;

    const x = padX + si * (nodeW + gapX);
    const totalH = nodes.length * nodeH + (nodes.length - 1) * gapY;
    const startY = padY + Math.max(0, (200 - totalH) / 2);

    for (let ni = 0; ni < nodes.length; ni++) {
      const y = startY + ni * (nodeH + gapY);
      positions.set(nodes[ni].id, { x, y, w: nodeW, h: nodeH });
      maxX = Math.max(maxX, x + nodeW);
      maxY = Math.max(maxY, y + nodeH);
    }
  }

  // Set viewBox
  svgEl.setAttribute("viewBox", `0 0 ${maxX + padX} ${maxY + padY}`);

  // Draw stage labels
  for (let si = 0; si < STAGE_ORDER.length; si++) {
    const stage = STAGE_ORDER[si];
    const nodes = activeGroups[stage];
    if (!nodes || nodes.length === 0) continue;

    const x = padX + si * (nodeW + gapX);
    const label = document.createElementNS("http://www.w3.org/2000/svg", "text");
    label.setAttribute("x", x + nodeW / 2);
    label.setAttribute("y", 20);
    label.setAttribute("text-anchor", "middle");
    label.setAttribute("fill", "#999");
    label.setAttribute("font-size", "11");
    label.setAttribute("font-family", "Nunito, sans-serif");
    label.setAttribute("font-weight", "700");
    label.setAttribute("letter-spacing", "1.5");
    label.textContent = stage.toUpperCase();
    svgEl.appendChild(label);
  }

  // Draw connections between stages
  const stageIds = STAGE_ORDER.map((stage) =>
    (activeGroups[stage] || []).map((n) => n.id)
  );

  // Connect each stage's nodes to the next stage's nodes
  for (let si = 0; si < stageIds.length - 1; si++) {
    const fromIds = stageIds[si];
    const toIds = stageIds[si + 1];
    if (!fromIds.length || !toIds.length) continue;

    for (const fromId of fromIds) {
      const fromPos = positions.get(fromId);
      if (!fromPos) continue;

      for (const toId of toIds) {
        const toPos = positions.get(toId);
        if (!toPos) continue;

        // Special case: IfcOWL and TopoLite don't connect directly to Export
        // They connect to the serializer
        const fromMod = modules.find((m) => m.id === fromId) || { stage: "Preprocess" };
        const toMod = modules.find((m) => m.id === toId) || { stage: "Export" };
        if (fromMod.stage === "Produce" && toMod.stage === "Export") continue;

        drawConnection(fromPos, toPos, running && !stageStatuses[fromId]?.status);
      }
    }
  }

  // Draw nodes
  for (const [id, pos] of positions) {
    const status = stageStatuses[id]?.status || "idle";
    const isSelected = selectedPluginId === id;
    drawNode(id, pos, status, isSelected, running);
  }
}

function drawNode(id, pos, status, isSelected, isRunning) {
  const state = getState();
  const mod = state.modules.find((m) => m.id === id);
  const displayName = mod ? mod.displayName : id === "parse" ? "Parse IFC" : id;
  const statusData = state.stageStatuses[id];

  const g = document.createElementNS("http://www.w3.org/2000/svg", "g");
  g.setAttribute("class", "dag-node");
  g.setAttribute("data-id", id);
  g.style.cursor = "pointer";

  // Background rect
  const rect = document.createElementNS("http://www.w3.org/2000/svg", "rect");
  rect.setAttribute("x", pos.x);
  rect.setAttribute("y", pos.y);
  rect.setAttribute("width", pos.w);
  rect.setAttribute("height", pos.h);
  rect.setAttribute("rx", 8);
  rect.setAttribute("fill", "#FAFAFA");
  rect.setAttribute("stroke", isSelected ? "#4A9EDA" : STAGE_COLORS[status] || STAGE_COLORS.idle);
  rect.setAttribute("stroke-width", isSelected ? 2.5 : 1.5);
  if (status === "running") {
    rect.style.animation = "dag-pulse 1.5s ease-in-out infinite";
  }
  g.appendChild(rect);

  // Status dot
  const dot = document.createElementNS("http://www.w3.org/2000/svg", "circle");
  dot.setAttribute("cx", pos.x + 14);
  dot.setAttribute("cy", pos.y + pos.h / 2);
  dot.setAttribute("r", 4);
  dot.setAttribute("fill", STAGE_COLORS[status] || STAGE_COLORS.idle);
  if (status === "running") {
    dot.style.animation = "dag-pulse 1.5s ease-in-out infinite";
  }
  g.appendChild(dot);

  // Name text
  const name = document.createElementNS("http://www.w3.org/2000/svg", "text");
  name.setAttribute("x", pos.x + 26);
  name.setAttribute("y", pos.y + (statusData?.durationMs ? pos.h / 2 - 6 : pos.h / 2 + 4));
  name.setAttribute("fill", "#2A2A2A");
  name.setAttribute("font-size", "12");
  name.setAttribute("font-family", "Roboto, sans-serif");
  name.setAttribute("font-weight", "500");
  name.textContent = displayName;
  g.appendChild(name);

  // Timing text (if completed)
  if (statusData?.durationMs) {
    const timing = document.createElementNS("http://www.w3.org/2000/svg", "text");
    timing.setAttribute("x", pos.x + 26);
    timing.setAttribute("y", pos.y + pos.h / 2 + 10);
    timing.setAttribute("fill", "#888");
    timing.setAttribute("font-size", "10");
    timing.setAttribute("font-family", "Roboto, sans-serif");
    timing.textContent = `${(statusData.durationMs / 1000).toFixed(2)}s${statusData.bytesOut ? ` · ${bytesToHuman(statusData.bytesOut)}` : ""}`;
    g.appendChild(timing);
  }

  // Error indicator
  if (status === "failed") {
    const errText = document.createElementNS("http://www.w3.org/2000/svg", "text");
    errText.setAttribute("x", pos.x + pos.w - 10);
    errText.setAttribute("y", pos.y + pos.h / 2 + 4);
    errText.setAttribute("text-anchor", "end");
    errText.setAttribute("fill", "#E08B8B");
    errText.setAttribute("font-size", "11");
    errText.setAttribute("font-family", "Roboto, sans-serif");
    errText.textContent = "✗";
    g.appendChild(errText);
  } else if (status === "success") {
    const okText = document.createElementNS("http://www.w3.org/2000/svg", "text");
    okText.setAttribute("x", pos.x + pos.w - 10);
    okText.setAttribute("y", pos.y + pos.h / 2 + 4);
    okText.setAttribute("text-anchor", "end");
    okText.setAttribute("fill", "#8BC9A0");
    okText.setAttribute("font-size", "11");
    okText.setAttribute("font-family", "Roboto, sans-serif");
    okText.textContent = "✓";
    g.appendChild(okText);
  }

  // Click handler
  g.addEventListener("click", () => {
    import("./state.js").then(({ update }) => {
      update({ selectedPluginId: id, detailOpen: true });
    });
  });

  svgEl.appendChild(g);
}

function drawConnection(fromPos, toPos, animated) {
  const x1 = fromPos.x + fromPos.w;
  const y1 = fromPos.y + fromPos.h / 2;
  const x2 = toPos.x;
  const y2 = toPos.y + toPos.h / 2;
  const midX = (x1 + x2) / 2;

  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute(
    "d",
    `M${x1},${y1} C${midX},${y1} ${midX},${y2} ${x2},${y2}`
  );
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", animated ? "#7EC8E3" : "#CCC");
  path.setAttribute("stroke-width", animated ? 2 : 1.2);
  if (animated) {
    path.setAttribute("stroke-dasharray", "6 4");
    path.style.animation = "dag-flow 0.8s linear infinite";
  }
  svgEl.appendChild(path);
}

function bytesToHuman(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
