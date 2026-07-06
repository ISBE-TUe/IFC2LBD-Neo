// module-metadata.js — Static module list for Electron mode
//
// In the browser, the module list comes from the WASM bindings (listModules()).
// In Electron, we don't load WASM — the native CLI does the conversion.
// This file provides the same metadata so the UI grid renders identically.
//
// Keep in sync with crates/ifc2lbd-wasm/src/plugins.rs browser_registry().

export const MODULES = [
  // Preprocess
  {
    id: "neo-cleanup-preprocess",
    displayName: "Cleanup",
    stage: "Preprocess",
    description: "ASCII repair and property deduplication",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-bsdd-match-preprocess",
    displayName: "bSDD Match",
    stage: "Preprocess",
    description: "Match properties to bSDD dictionaries",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-qto-preprocess",
    displayName: "QTO",
    stage: "Preprocess",
    description: "QTO reconstruction from geometry",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-geometry-preprocess",
    displayName: "Geometry",
    stage: "Preprocess",
    description: "Tessellate and prepare geometry for streaming",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: ["metadata"],
  },

  // Produce
  {
    id: "neo-bot-producer",
    displayName: "BOT",
    stage: "Produce",
    description: "Building Topology Ontology — spatial structure and zones",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-beo-producer",
    displayName: "BEO",
    stage: "Produce",
    description: "Building Element Ontology — element classification",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-bsdd-producer",
    displayName: "bSDD",
    stage: "Produce",
    description: "buildingSMART Data Dictionary properties",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: ["profile", "compact", "include_standard_attrs", "dedup_properties"],
  },
  {
    id: "neo-props-opm",
    displayName: "Props OPM",
    stage: "Produce",
    description: "OPM property set modeling",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-omg-fog",
    displayName: "OMG/FOG",
    stage: "Produce",
    description: "Ontology for Managing Geometry / Fog features",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: [],
  },
  {
    id: "neo-ifcowl-producer",
    displayName: "IfcOWL",
    stage: "Produce",
    description: "Full IfcOWL RDF representation",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: ["mode"],
  },
  {
    id: "neo-geometry-producer",
    displayName: "Geo Sidecar",
    stage: "Produce",
    description: "Geometry sidecar files (Fragments/glTF/Parquet)",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: ["format"],
  },
  {
    id: "neo-rml-mapper",
    displayName: "RML Mapper",
    stage: "Produce",
    description: "RML mapping for structured data",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: ["rml_mapping"],
  },

  // Postprocess
  {
    id: "neo-ontology-mapper",
    displayName: "Ontology Mapper",
    stage: "Postprocess",
    description: "Align output with external ontologies",
    failurePolicy: "Optional",
    wasmCompatible: true,
    optionKeys: ["alignment_file", "ontology_file"],
  },

  // Serialize
  {
    id: "neo-turtle-serializer",
    displayName: "Turtle",
    stage: "Serialize",
    description: "Turtle RDF serialization",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: ["grouping", "layout"],
  },
  {
    id: "neo-nquads-serializer",
    displayName: "N-Quads",
    stage: "Serialize",
    description: "N-Quads serialization with named graphs",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: ["graph_naming"],
  },
  {
    id: "neo-nquads-chunked-serializer",
    displayName: "N-Quads Chunked",
    stage: "Serialize",
    description: "Chunked N-Quads for large datasets",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: ["chunking", "chunk_size_lines", "chunk_size_bytes", "chunk_prefix", "graph_naming"],
  },

  // Export
  {
    id: "neo-file-export",
    displayName: "File Export",
    stage: "Export",
    description: "Write output files to disk",
    failurePolicy: "Required",
    wasmCompatible: true,
    optionKeys: ["output_stem", "compress"],
  },
];

// Resolve a module activation plan — mirrors the Rust resolvePlan() logic.
// In Electron mode we use a simplified version that just returns the
// requested modules plus any required dependencies.
export function resolvePlanStatic(requestedModules, _moduleOptions) {
  const requested = new Set(requestedModules);

  // File export is always required
  requested.add("neo-file-export");

  // Exactly one serializer is required
  const serializers = [
    "neo-turtle-serializer",
    "neo-nquads-serializer",
    "neo-nquads-chunked-serializer",
  ];
  const activeSerializers = serializers.filter((s) => requested.has(s));
  if (activeSerializers.length === 0) {
    requested.add("neo-turtle-serializer");
  }

  return {
    enabledIds: [...requested].sort(),
  };
}
