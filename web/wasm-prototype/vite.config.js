import { defineConfig } from "vite";
import { readFileSync } from "fs";
import { execSync } from "child_process";

// Generate build version from git: short SHA + timestamp
function buildVersion() {
	try {
		const sha = execSync("git rev-parse --short HEAD").toString().trim();
		const date =
			new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19) + "Z";
		return `pipeline-${sha}-${date}`;
	} catch {
		return "pipeline-dev";
	}
}

const version = buildVersion();

export default defineConfig({
	base: "./",
	define: {
		__BUILD_VERSION__: JSON.stringify(version),
	},
	worker: {
		format: "es",
	},
	server: {
		headers: {
			"Cross-Origin-Opener-Policy": "same-origin",
			"Cross-Origin-Embedder-Policy": "require-corp",
			"Cross-Origin-Resource-Policy": "same-origin",
		},
	},
	preview: {
		headers: {
			"Cross-Origin-Opener-Policy": "same-origin",
			"Cross-Origin-Embedder-Policy": "require-corp",
			"Cross-Origin-Resource-Policy": "same-origin",
		},
	},
});
