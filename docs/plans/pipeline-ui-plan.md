# WASM Pipeline UI — Design Plan v6

## Vision

A developer-grade conversion pipeline dashboard. Think Ableton Session View meets GitLab CI/CD pipeline — a light, grey-toned interface where the central panel shows a live DAG of conversion stages, each node pulses with pastel state colors, and clicking any plugin opens a detail sidebar with its options, telemetry, and failure state.

TU Eindhoven logo in white. No red. The palette is cool grey + muted pastel accents.

---

## Architecture

### Dual-Mode Layout

| Device | Layout | Description |
|---|---|---|
| **Desktop / iPad** | Full-width pipeline dashboard | DAG is the hero. Sidebars are contextual overlays. |
| **Phone** | Simple form (current v6) | File pick → Convert → Download. No DAG. |

Detection: `window.innerWidth < 900`. The simple view is the existing `main.js` worker architecture — it already works on phones.

### Top-Level Structure (Desktop)

The DAG gets the full width. Settings and detail are contextual slide-outs — they appear when needed and get out of the way.

```
┌──────────────────────────────────────────────────────────────────┐
│  TU/e logo    IFC2LBD-Neo Pipeline    [⚙] [Load] [Save] [▶ Run] │
├──────────────────────────────────────────────────────────────────┤
│                                                                  │
│                   P I P E L I N E   D A G                        │
│                                                                  │
│    ┌──────────┐      ┌──────────┐      ┌──────────┐             │
│    │ LBD Prod.│─────→│  Turtle  │─────→│   File   │             │
│    │   1.2s   │      │Serializer│      │  Export  │             │
│    └──────────┘      │   0.3s   │      │   0.01s  │             │
│    ┌──────────┐      └──────────┘      └──────────┘             │
│    │IfcOWL    │───┐                                          │
│    │Producer  │   │→ (merges into same serializer)             │
│    │   1.8s   │   │                                           │
│    └──────────┘   │                                           │
│    ┌──────────┐   │                                           │
│    │ Topo Lite│───┘                                           │
│    │ (skipped)│                                               │
│    └──────────┘                                               │
│                                                                  │
├──────────────────────────────────────────────────────────────────┤
│  ▾ LOG   runtime info, stage output, errors                      │
└──────────────────────────────────────────────────────────────────┘

         ↑ clicking any DAG node slides detail in from the right ↓

                              ┌─────────────────────┐
                              │  ← LBD Producer     │
                              │                     │
                              │  ● Produce stage    │
                              │  ✓ Success  1.23s   │
                              │  35.2 MB · 142K trp │
                              │                     │
                              │  OPTIONS            │
                              │  ─────────          │
                              │  (controls)         │
                              │                     │
                              │  TELEMETRY          │
                              │  ─────────          │
                              │  Batches: 47        │
                              │  Peak depth: 12     │
                              └─────────────────────┘
```

**[⚙] Settings panel** (left slide-out):
- Plugin toggles (checkboxes + radios)
- File input
- Base URI / output stem
- Per-plugin options

**Detail overlay** (right slide-out, on DAG node click):
- Plugin status, timing, options, telemetry
- Closes with ✕ or clicking outside

---

## Color Palette

Ableton-inspired light grey with cool pastel accents. No warm tones, no TU/e red.

| Token | Hex | Usage |
|---|---|---|
| `--bg` | `#E8E8E8` | Page background |
| `--surface` | `#F2F2F2` | Card / panel backgrounds |
| `--surface-raised` | `#FAFAFA` | Elevated panels, sidebar |
| `--border` | `#D0D0D0` | Borders, dividers |
| `--border-subtle` | `#DCDCDC` | Lighter borders |
| `--text` | `#2A2A2A` | Primary text |
| `--text-muted` | `#777` | Secondary text |
| `--accent` | `#4A9EDA` | Buttons, links, active focus |
| `--accent-hover` | `#3A8BC8` | Button hover |
| | | |
| `--stage-idle` | `#C8C8C8` | Pipeline node: not yet started |
| `--stage-running` | `#7EC8E3` | Pipeline node: actively processing (pulsing) |
| `--stage-success` | `#8BC9A0` | Pipeline node: completed OK |
| `--stage-failed` | `#E08B8B` | Pipeline node: error |
| `--stage-warning` | `#E8C872` | Pipeline node: optional failure |
| `--stage-skipped` | `#B8B8B8` | Pipeline node: not in current plan |
| | | |
| `--dag-line` | `#AAA` | Connection lines between nodes |
| `--dag-line-active` | `#7EC8E3` | Connection line: data flowing (animated dash) |

---

## Panel Details

### 1. Top Bar

- TU/e logo (white PNG on grey bar), app title "IFC2LBD-Neo Pipeline"
- **[Load]** button: import a saved pipeline config JSON
- **[Save]** button: export current config (active modules + options) as JSON
- **[Run]** button: primary CTA, triggers conversion. Shows ▶ icon. While running: ⏸ icon + elapsed timer.
- File input is in the left panel, not the top bar.

### 2. ⚙ Settings Panel (Left Slide-Out, ~300px)

Opens when ⚙ button is clicked. Slides in from the left, pushing the DAG content right (or overlaying with a backdrop). Contains:

```
▼ PRODUCE
  ☑ LBD Producer          ← always on (required)
  ☑ IfcOWL Producer       ← toggled
  ☐ Topology Lite         ← toggled (optional)
  ☐ Bbox Enricher         ← toggled (optional)

▼ SERIALIZE
  ○ Turtle                ← radio (mutually exclusive with N-Quads)
  ○ N-Quads

▼ EXPORT
  ☑ File Export           ← always on (required)

─────

IFC FILE
  [📁 Drop or browse]

BASE URI
  [https://lbd.example.com/]

OUTPUT STEM
  [converted-model     ]
```

- Checkboxes: producers + enrichers (can run in parallel)
- Radio buttons: serializers (mutually exclusive — conflict constraint)
- Required plugins are checked + disabled (cannot uncheck)
- Toggling a plugin instantly updates the DAG
- The `requires` and `conflicts_with` constraints are enforced: toggling IfcOWL off when Bbox requires it shows a warning or auto-toggles Bbox off.
- File input is here, not in the DAG area

### 3. Pipeline DAG (Full-Width Center)

The hero of the interface. Occupies the full viewport width between top bar and log panel. A horizontal DAG showing stage columns with plugin nodes.

**Node Design:**

```
 ┌──────────────────┐
 │  ● LBD Producer  │
 │  1.23s · 35MB    │
 └──────────────────┘
```

- The colored dot `●` uses the stage-* color
- During `running` state, the dot pulses (CSS animation: `@keyframes pulse { 0%,100% { opacity:1; } 50% { opacity:0.5; } }`)
- The timing and size appear after the stage completes
- Nodes in the same stage column are stacked vertically
- Connection lines go from output of one stage to inputs of the next

**Data Flow Animation:**

When running, the connection lines between stages show an animated dashed line flowing in the direction of data flow. CSS `stroke-dashoffset` animation on SVG paths.

**Layout:**

Stages flow left → right: `Produce → Serialize → Export`

Within each stage column, plugins are stacked:

```
  Produce          Serialize         Export
  ┌─────────┐     ┌──────────┐     ┌──────────┐
  │ LBD     │────→│ Turtle   │────→│ File     │
  │ Producer│     │Serializer│     │ Export   │
  └─────────┘     └──────────┘     └──────────┘
  ┌─────────┐
  │ IfcOWL  │─── ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─→ (joins LBD into Turtle)
  │Producer │
  └─────────┘
```

Parallel producers connect to the same serializer node. IfcOWL output merges into the same serializer stream (this matches the actual channel architecture).

**Interactivity:**

- Click a node → opens its detail in the right sidebar
- Hover a node → shows tooltip with stage, status, timing
- Drag to pan (if DAG is larger than viewport)
- Mouse wheel to zoom (subtle)

### 4. Plugin Detail Sidebar (Right Slide-Out, ~300px)

Appears when a DAG node is clicked. Slides in from the right with a semi-transparent backdrop. Shows:

```
┌─────────────────────────┐
│  ← LBD Producer         │
│                          │
│  Stage: Produce          │
│  Status: ✓ Success       │
│  Duration: 1.23s         │
│  Output: 35.2 MB         │
│  Triples: 142,847        │
│                          │
│  OPTIONS                 │
│  ────────                │
│  (plugin-specific        │
│   option controls)       │
│                          │
│  TELEMETRY               │
│  ────────                │
│  Batch count: 47         │
│  Avg batch size: 3,041   │
│  Peak channel depth: 12  │
│                          │
│  FAILURE POLICY          │
│  ────────                │
│  Required                │
│                          │
│  DEPENDENCIES            │
│  ────────                │
│  Requires: (none)        │
│  Conflicts: (none)       │
└─────────────────────────┘
```

Plugin-specific options are rendered from the `option_keys` array in the manifest. Each key maps to an input control:

| Key | Control |
|---|---|
| `grouping` | Dropdown: Sorted / Streaming |
| `chunking` | Dropdown: none / lines / bytes |
| `chunk_size_lines` | Number input |
| `chunk_size_bytes` | Number input |
| `chunk_prefix` | Text input |
| `chunk_min_count` | Number input |
| `chunk_core_count` | Number input |
| `lbd_graph_iri` | Text input |
| `ifcowl_graph_iri` | Text input |
| `output_stem` | Text input |

### 5. Log Panel (Bottom, collapsible)

A slim log panel at the bottom. Shows:
- WASM initialization messages
- Pipeline stage start/stop
- Warnings from the runner
- Error messages
- Telemetry summaries

Can be collapsed to a single line showing the latest log entry.

---

## Pipeline Config Save/Load

The **Save** button exports:

```json
{
  "version": 1,
  "moduleIds": ["neo-lbd-producer", "neo-ifcowl-producer", "neo-turtle-serializer", "neo-file-export"],
  "moduleOptions": ["neo-turtle-serializer.grouping=sorted"],
  "baseUri": "https://lbd.example.com/",
  "outputStem": "converted-model"
}
```

The **Load** button imports this JSON and:
1. Restores the checkbox/radio state in the Stage Panel
2. Sets all option values in the Detail Sidebar
3. Updates the DAG

---

## Per-Stage Telemetry (Rust Changes Required)

### New Types

```rust
/// Per-stage telemetry for a single plugin execution.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageTelemetry {
    pub plugin_id: String,
    pub stage: String,         // "Produce" | "Serialize" | "Export"
    pub status: String,        // "running" | "success" | "failed" | "skipped"
    pub duration_ms: u64,      // 0 until complete
    pub bytes_out: u64,        // bytes produced (0 for producers, bytes written for serializers)
    pub triples_out: u64,      // triples emitted (0 for exporters)
    pub error: Option<String>, // set if status == "failed"
}
```

### Where to Instrument

The `PipelineRunner` methods (`run_to_sink`, `run_benchmark`) need to wrap each stage with timing:

1. **Parsing** (not a plugin, but a stage): `parse_step_bytes` + `build_model` — track as `parse` stage
2. **Produce**: The `stream_step_and_model` call — track as producer stage(s)
3. **Serialize**: The `serialize_*` call — track as serializer stage
4. **Export**: The `SinkChunkWriter::finish()` call — track as export stage

The `StageTelemetry` vec is included in `StreamConversionBundle` and `BenchmarkBundle`.

### Real-Time Emission

For live DAG updates, the Rust side should emit stage events through the same `sink` callback. Add a new event type:

```js
// sink event from Rust
{ type: "stageEvent", pluginId: "neo-lbd-producer", stage: "Produce", status: "running" }
{ type: "stageEvent", pluginId: "neo-lbd-producer", stage: "Produce", status: "success", durationMs: 1230 }
```

This way the worker can forward these to the main thread, and the DAG updates in real-time.

### Implementation Plan

1. Add `StageTelemetry` struct to `types.rs`
2. Add `stage_telemetry: Vec<StageTelemetry>` to `StreamConversionBundle` and `BenchmarkBundle`
3. In `export_browser_files_to_sink_streaming`, emit `stageEvent` sink events at stage boundaries
4. In `wasm-lowmem-worker.js`, forward `stageEvent` messages to main thread
5. In main thread, update DAG node states reactively

---

## DAG Rendering Strategy

### SVG (Recommended)

- SVG elements for nodes and connection paths
- CSS animations for pulse, dash-offset, color transitions
- Lightweight, no dependency
- Easy to make responsive

### Layout Algorithm

- Fixed 3-column layout: Produce | Serialize | Export
- Within each column, nodes are stacked with 16px gap
- SVG `<path>` elements for connections, routed with simple orthogonal segments
- No need for a full DAG layout library — the structure is known and small (≤7 nodes)

### Mobile Adaptation

On phones (`width < 900px`), the full dashboard is hidden and the current simple form is shown. This is a separate `index-simple.html` or a CSS-hidden section.

---

## Implementation Order

### Phase A: Rust Telemetry ( prerequisite)
1. Add `StageTelemetry` type
2. Add `stageEvent` emission through sink callback
3. Include `stage_telemetry` vec in result bundles
4. Update worker to forward stage events

### Phase B: UI Shell
1. Create new `index-pipeline.html` + `pipeline.js` + `pipeline.css`
2. Top bar with logo, title, Load/Save/Run buttons
3. Left sidebar with plugin toggles
4. Right sidebar with plugin detail (empty state)
5. Bottom log panel

### Phase C: DAG Visualization
1. SVG DAG renderer
2. Node rendering from `listModules()` + current activation state
3. Connection path routing
4. Click-to-select → sidebar update
5. State transitions: idle → running → success/failed

### Phase D: Live Pipeline
1. Wire Run button → worker conversion
2. Forward `stageEvent` from worker → DAG state updates
3. Animate node states and connection flow
4. Show timing after completion
5. Error handling: failed node styling, log output

### Phase E: Plugin Options
1. Render option controls from manifest `option_keys`
2. Map option values to `moduleOptions` format
3. Validate on change (e.g., grouping → streaming for lowmem)

### Phase F: Config Management
1. Save config JSON from current state
2. Load config JSON → restore state
3. File download/upload via Blob API

### Phase G: Mobile Fallback
1. Detect mobile viewport
2. Show simple form (current v6 UI) on phones
3. Show pipeline dashboard on desktop/iPad

---

## File Structure

```
web/wasm-prototype/
├── index.html                  ← router: detects viewport, loads appropriate view
├── src/
│   ├── main.js                 ← mobile/simple view (current v6 worker code)
│   ├── styles.css              ← mobile/simple styles
│   ├── wasm-lowmem-worker.js   ← shared worker
│   ├── pipeline/
│   │   ├── app.js              ← desktop app entry point
│   │   ├── dag.js              ← SVG DAG renderer
│   │   ├── sidebar.js          ← plugin detail sidebar
│   │   ├── stage-panel.js      ← left sidebar (toggles, file input)
│   │   ├── log-panel.js        ← bottom log
│   │   ├── config.js           ← save/load pipeline configs
│   │   ├── state.js            ← reactive state management
│   │   └── pipeline.css        ← desktop dashboard styles
│   └── wasm/
│       └── ...                  ← generated WASM artifacts
├── public/
│   ├── logo-horizontal.png     ← TU/e logo
│   └── logo-vertical.png
└── vite.config.js
```

---

## Key Design Decisions

1. **No framework.** Vanilla JS + SVG. The DAG is small (≤7 nodes), no virtual DOM needed. CSS custom properties for theming. Keeps the WASM prototype lightweight.

2. **Reactive state via simple pub/sub.** A `state.js` module holds the pipeline state (active plugins, options, stage statuses). Components subscribe to changes. No proxy/Observable overhead.

3. **Stage events through existing sink channel.** The `convertIfcToSink` sink callback is already a function that receives events. Adding `{ type: "stageEvent", ... }` events requires zero API changes — the worker and main thread already handle discriminated event types.

4. **SVG over Canvas.** SVG nodes are DOM elements — clickable, hoverable, CSS-animatable. Canvas would require a hit-testing layer. The DAG is small enough that SVG performance is fine.

5. **Config as flat JSON.** No nesting, directly maps to `ConversionRequest.moduleIds` and `moduleOptions`. Easy to share, diff, version-control.

6. **Phone = simple view.** No responsive breakpoints for the DAG. Phones get the working v6 form. iPads in landscape get the dashboard (width ≥ 900px).
