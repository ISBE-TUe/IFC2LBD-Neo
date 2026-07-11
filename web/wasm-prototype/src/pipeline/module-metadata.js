// module-metadata.js — Static module list for Electron mode
//
// In the browser, the module list comes from the WASM bindings (listModules()).
// In Electron, we don't load WASM — the native CLI does the conversion.
// This file provides the same metadata so the UI grid renders identically.
//
// Keep in sync with crates/ifc2lbd-wasm/src/plugins.rs browser_registry().

function mod(
	id,
	displayName,
	stage,
	description,
	failurePolicy,
	optionKeys = [],
	extra = {},
) {
	return {
		id,
		displayName,
		stage,
		description,
		inputs: extra.inputs || [],
		outputs: extra.outputs || [],
		requires: extra.requires || [],
		conflictsWith: extra.conflictsWith || [],
		failurePolicy,
		parallelism: extra.parallelism || "Parallel",
		wasmCompatible: true,
		optionKeys,
	};
}

export const MODULES = [
	// Preprocess
	mod(
		"neo-cleanup-preprocess",
		"ASCII Repair",
		"Preprocess",
		"Deduplicates IFC property occurrences and normalizes property payload quality",
		"Optional",
	),
	mod(
		"neo-bsdd-match-preprocess",
		"bSDD Matcher",
		"Preprocess",
		"Precomputes bSDD fuzzy/exact match cache shared by producers",
		"Optional",
	),
	mod(
		"neo-qto-preprocess",
		"QTO Rebuild",
		"Preprocess",
		"Detects missing IFC quantity sets and computes them from STEP geometry",
		"Optional",
	),
	mod(
		"neo-geometry-preprocess",
		"Geometry preprocessor",
		"Preprocess",
		"Tessellates IFC geometry using ifc-lite and stores TessellatedModel in context",
		"Optional",
		["metadata"],
	),

	// Produce
	mod(
		"neo-bot-producer",
		"BOT",
		"Produce",
		"Building Topology Ontology — spatial structure and zones",
		"Required",
		[],
		{ outputs: ["bot"] },
	),
	mod(
		"neo-beo-producer",
		"BEO",
		"Produce",
		"Building Element Ontology — element classification",
		"Required",
		[],
		{ outputs: ["beo"] },
	),
	mod(
		"neo-bsdd-producer",
		"bSDD",
		"Produce",
		"buildingSMART Data Dictionary properties",
		"Required",
		["profile", "compact", "include_standard_attrs", "dedup_properties"],
		{ outputs: ["props"] },
	),
	mod(
		"neo-props-opm",
		"Props-OPM",
		"Produce",
		"OPM property set modeling",
		"Required",
		[],
		{ outputs: ["props"] },
	),
	mod(
		"neo-omg-fog",
		"OMG-FOG",
		"Produce",
		"Ontology for Managing Geometry / Fog features",
		"Required",
		[],
		{ outputs: ["omg"] },
	),
	mod(
		"neo-ifcowl-producer",
		"IfcOWL",
		"Produce",
		"Full IfcOWL RDF representation",
		"Required",
		["mode"],
		{ outputs: ["ifcowl"] },
	),
	mod(
		"neo-geometry-producer",
		"Geometry producer",
		"Produce",
		"Serializes tessellated geometry to fragments, glTF, Parquet or IFC5",
		"Optional",
		["format"],
	),
	mod(
		"neo-rml-mapper",
		"RML Mapper",
		"Produce",
		"RML mapping for structured data",
		"Optional",
		["rml_mapping"],
	),

	// Postprocess
	mod(
		"neo-ontology-mapper",
		"Ontology Mapper",
		"Postprocess",
		"Align output with external ontologies",
		"Optional",
		["alignment_file", "ontology_file"],
	),

	// Serialize
	mod(
		"neo-turtle-serializer",
		"Turtle serializer",
		"Serialize",
		"Turtle RDF serialization",
		"Required",
		["grouping", "layout"],
	),
	mod(
		"neo-nquads-serializer",
		"N-Quads serializer",
		"Serialize",
		"N-Quads serialization with named graphs",
		"Required",
		["graph_naming"],
	),
	mod(
		"neo-nquads-chunked-serializer",
		"N-Quads chunked serializer",
		"Serialize",
		"Chunked N-Quads for large datasets",
		"Required",
		[
			"chunking",
			"chunk_size_lines",
			"chunk_size_bytes",
			"chunk_prefix",
			"graph_naming",
		],
	),

	// Export
	mod(
		"neo-file-export",
		"File exporter",
		"Export",
		"Write output files to disk",
		"Required",
		["output_stem", "compress"],
	),
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
