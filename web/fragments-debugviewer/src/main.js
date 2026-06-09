import "./style.css";

import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import * as FRAGS from "@thatopen/fragments";

const viewport = document.getElementById("viewport");
const fileInput = document.getElementById("frag-file");
const loadFileButton = document.getElementById("load-file");
const urlInput = document.getElementById("frag-url");
const loadUrlButton = document.getElementById("load-url");
const resetButton = document.getElementById("reset-viewer");
const status = document.getElementById("status");
const tree = document.getElementById("tree");
const metadata = document.getElementById("metadata");

const scene = new THREE.Scene();
scene.background = new THREE.Color("#f5f1e8");

const camera = new THREE.PerspectiveCamera(
  60,
  viewport.clientWidth / viewport.clientHeight,
  0.1,
  5000,
);
camera.up.set(0, 0, 1);
camera.position.set(16, 14, 16);

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(window.devicePixelRatio);
renderer.setSize(viewport.clientWidth, viewport.clientHeight);
viewport.append(renderer.domElement);

const controls = new OrbitControls(camera, renderer.domElement);
controls.enableDamping = true;
controls.target.set(0, 0, 4);

scene.add(new THREE.AmbientLight("#ffffff", 1.5));

const sun = new THREE.DirectionalLight("#fff6d6", 2.2);
sun.position.set(24, 36, 18);
scene.add(sun);

const grid = new THREE.GridHelper(120, 120, "#d97706", "#d6d3d1");
grid.rotation.x = Math.PI / 2;
scene.add(grid);
scene.add(new THREE.AxesHelper(5));

const setStatus = (message) => {
  status.textContent = message;
};

const setMetadata = (message) => {
  metadata.textContent = message;
};

const createFragmentsEngine = async () => {
  const fragments = new FRAGS.FragmentsModels("/thatopen-worker.mjs");
  fragments.settings.autoCoordinate = true;
  fragments.settings.graphicsQuality = 1;

  fragments.models.list.onItemSet.add(({ value: model }) => {
    model.useCamera(camera);
    scene.add(model.object);
    void fragments.update(true);
  });
  return fragments;
};

const resize = () => {
  const width = viewport.clientWidth;
  const height = viewport.clientHeight;
  camera.aspect = width / height;
  camera.updateProjectionMatrix();
  renderer.setSize(width, height);
};

window.addEventListener("resize", resize);
resize();

const init = async () => {
  const fragments = await createFragmentsEngine();

  const renderLoop = () => {
    controls.update();
    renderer.render(scene, camera);
  };

  renderer.setAnimationLoop(renderLoop);
  controls.addEventListener("change", () => {
    void fragments.update();
  });
  controls.addEventListener("end", () => {
    void fragments.update(true);
  });

  let loadCount = 0;
  let currentModel = null;
  let selectedLocalId = null;
  let categoryByLocalId = new Map();

  const nextFrame = () =>
    new Promise((resolve) => {
      requestAnimationFrame(() => resolve());
    });

  const wait = (ms) =>
    new Promise((resolve) => {
      window.setTimeout(resolve, ms);
    });

  const fitCameraToBox = (box) => {
    if (box.isEmpty()) {
      return null;
    }

    const size = box.getSize(new THREE.Vector3());
    const center = box.getCenter(new THREE.Vector3());
    const maxSize = Math.max(size.x, size.y, size.z);
    const distance = Math.max(maxSize * 1.6, 10);

    controls.target.copy(center);
    camera.position.set(center.x + distance, center.y + distance, center.z + distance * 0.6);
    camera.lookAt(center);
    controls.update();

    return {
      center,
      size,
    };
  };

  const formatValue = (value) => {
    if (value === null || value === undefined) {
      return String(value);
    }
    if (typeof value === "object") {
      return JSON.stringify(value, null, 2);
    }
    return String(value);
  };

  const selectionMaterial = {
    color: new THREE.Color("#ef4444"),
    opacity: 1,
    transparent: false,
    renderedFaces: FRAGS.RenderedFaces.TWO,
  };

  const clearSelectionUi = () => {
    for (const node of tree.querySelectorAll(".tree-node.selected")) {
      node.classList.remove("selected");
    }
  };

  const markSelectedTreeNode = (localId) => {
    clearSelectionUi();
    const button = tree.querySelector(`[data-local-id="${localId}"]`);
    if (button) {
      button.classList.add("selected");
      button.scrollIntoView({ block: "nearest" });
    }
  };

  const selectItem = async (model, localId, source = "selection") => {
    if (!model || localId === null || localId === undefined) {
      return;
    }

    try {
      if (selectedLocalId !== null && currentModel) {
        await currentModel.resetHighlight([selectedLocalId]);
      }
    } catch {}

    currentModel = model;
    selectedLocalId = localId;

    try {
      await model.highlight([localId], selectionMaterial);
    } catch {}

    markSelectedTreeNode(localId);

    const item = model.getItem(localId);

    const read = async (fn, fallback = null) => {
      try {
        return await fn();
      } catch {
        return fallback;
      }
    };

    const [guid, attrs, box] = await Promise.all([
      read(() => item.getGuid()),
      read(() => item.getAttributes()),
      read(() => model.getMergedBox([localId])),
    ]);
    const category =
      categoryByLocalId.get(localId) ??
      (await read(() => item.getCategory(), null));

    // Refresh the highlight without moving the camera. (Auto zoom/centering
    // on selection was removed — selecting an element no longer reframes.)
    await fragments.update(true);

    const attrObject = attrs ? attrs.object : {};
    const lines = [
      `source=${source}`,
      `localId=${localId}`,
      `guid=${guid ?? "null"}`,
      `category=${category ?? "null"}`,
      "",
      "attributes:",
    ];

    const entries = Object.entries(attrObject);
    if (entries.length === 0) {
      lines.push("(none)");
    } else {
      for (const [key, value] of entries) {
        lines.push(`${key}: ${formatValue(value)}`);
      }
    }

    setMetadata(lines.join("\n"));
  };

  const createTreeNode = (model, item, depth = 0) => {
    const hasChildren = Array.isArray(item.children) && item.children.length > 0;
    const label = item.category ?? "Uncategorized";
    const idSuffix = item.localId === null ? "" : ` #${item.localId}`;

    if (item.localId !== null && item.category) {
      categoryByLocalId.set(item.localId, item.category);
    }

    if (!hasChildren) {
      const wrapper = document.createElement("div");
      const button = document.createElement("button");
      button.type = "button";
      button.className = "tree-node";
      button.dataset.localId = item.localId ?? "";
      button.textContent = `${label}${idSuffix}`;
      if (item.localId !== null) {
        button.addEventListener("click", () => {
          void selectItem(model, item.localId, "tree");
        });
      } else {
        button.disabled = true;
      }
      wrapper.append(button);
      return wrapper;
    }

    const details = document.createElement("details");
    details.open = depth < 2;

    const summary = document.createElement("summary");
    summary.textContent = `${label}${idSuffix}`;
    details.append(summary);

    if (item.localId !== null) {
      const selfButton = document.createElement("button");
      selfButton.type = "button";
      selfButton.className = "tree-node";
      selfButton.dataset.localId = item.localId;
      selfButton.textContent = `Select ${label}${idSuffix}`;
      selfButton.addEventListener("click", () => {
        void selectItem(model, item.localId, "tree");
      });
      details.append(selfButton);
    }

    for (const child of item.children) {
      details.append(createTreeNode(model, child, depth + 1));
    }

    return details;
  };

  const renderTree = async (model) => {
    tree.textContent = "Loading spatial tree…";
    try {
      categoryByLocalId = new Map();
      const spatial = await model.getSpatialStructure();
      tree.replaceChildren(createTreeNode(model, spatial));
    } catch (error) {
      tree.textContent = `Tree load failed: ${error instanceof Error ? error.message : String(error)}`;
    }
  };

  renderer.domElement.addEventListener("click", async (event) => {
    if (!currentModel) {
      return;
    }

    try {
      const result = await currentModel.raycast({
        camera,
        mouse: new THREE.Vector2(event.clientX, event.clientY),
        dom: renderer.domElement,
      });
      if (!result) {
        return;
      }
      await selectItem(result.fragments, result.localId, "pick");
    } catch (error) {
      setMetadata(`Pick failed: ${error instanceof Error ? error.message : String(error)}`);
    }
  });

  const loadBuffer = async (buffer, sourceName) => {
    loadCount += 1;
    const modelId = `debug-model-${loadCount}`;
    const sourceBytes = buffer.byteLength;
    setStatus(`Loading ${sourceName} (${sourceBytes.toLocaleString()} bytes)…`);
    const model = await fragments.load(buffer, { modelId, camera });
    currentModel = model;
    selectedLocalId = null;
    setMetadata("No item selected.");
    tree.textContent = "Loading spatial tree…";
    const frame = fitCameraToBox(model.box.clone());

    for (let i = 0; i < 12; i += 1) {
      await fragments.update(true);
      await nextFrame();
      if (model.object.children.length > 0 && !model.isBusy) {
        break;
      }
      await wait(50);
    }

    const lines = [
      `Loaded ${sourceName}`,
      `modelId=${modelId}`,
      `size=${sourceBytes.toLocaleString()} bytes`,
      `rootChildren=${model.object.children.length}`,
      `isBusy=${model.isBusy}`,
    ];

    if (frame) {
      lines.push(
        `bbox=${frame.size.x.toFixed(2)} x ${frame.size.y.toFixed(2)} x ${frame.size.z.toFixed(2)}`,
      );
    } else {
      lines.push("bbox=empty");
    }

    await renderTree(model);

    setStatus(
      lines.join("\n"),
    );
  };

  loadFileButton.addEventListener("click", async () => {
    const [file] = fileInput.files ?? [];
    if (!file) {
      setStatus("Choose a .frag file first.");
      return;
    }

    try {
      const buffer = await file.arrayBuffer();
      await loadBuffer(buffer, file.name);
    } catch (error) {
      setStatus(`File load failed: ${error instanceof Error ? error.message : String(error)}`);
    }
  });

  loadUrlButton.addEventListener("click", async () => {
    const url = urlInput.value.trim();
    if (!url) {
      setStatus("Enter a URL or public path first.");
      return;
    }

    try {
      setStatus(`Fetching ${url}…`);
      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      const buffer = await response.arrayBuffer();
      await loadBuffer(buffer, url);
    } catch (error) {
      setStatus(`URL load failed: ${error instanceof Error ? error.message : String(error)}`);
    }
  });

  resetButton.addEventListener("click", () => {
    window.location.reload();
  });

  setStatus(
    "Viewer ready.\nLoad a local .frag file or place one in public/models and load it via /models/your-file.frag",
  );
  setMetadata("No item selected.");
};

init().catch((error) => {
  setStatus(`Viewer boot failed: ${error instanceof Error ? error.message : String(error)}`);
});
