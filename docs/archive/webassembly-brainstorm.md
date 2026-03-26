> Status: superseded by `docs/current/future-wasm-plan.md` (reviewed, project-scoped plan).
> Keep this file only as original brainstorming context.

In future we want this application to be hosteable and the run on client side via webassembly.

You can build your Rust IFC→LBD converter to run **entirely in the user’s browser** via **WebAssembly (Wasm)**, so users pick a local file with an `<input type="file">` or drag-and-drop, the browser reads it locally, and your Rust/Wasm code processes it on their machine. The file is **not uploaded anywhere unless your page explicitly sends it over the network** with something like `fetch()` or XHR. The browser’s File API is specifically designed for local user-selected files. ([MDN Web Docs][1])

In practice, the architecture looks like this:

* **Rust core**: keep the IFC/LBD conversion logic in a pure Rust crate.
* **Wasm wrapper**: compile that crate to `wasm32-unknown-unknown` and expose a small API with `wasm-bindgen`.
* **Browser UI**: HTML/JS handles file selection, passes bytes into Wasm, then offers the converted result back as a download.
* **Static hosting**: you can host just the app shell (`.html`, `.js`, `.wasm`) on any static site/CDN. The server serves the app itself, but the actual conversion happens client-side. ([MDN Web Docs][1])

A very typical flow is:

1. User opens your webpage.
2. User chooses an IFC file from disk.
3. Browser reads the file locally into memory.
4. Wasm calls your Rust converter on those bytes.
5. Browser creates a downloadable Blob for the output.
   No backend processing is required for the conversion itself. ([MDN Web Docs][1])

A few caveats matter:

**1. Memory / file size**
Large IFC files can be heavy in the browser. Wasm runs locally, but the browser still has memory limits, and loading very large files fully into memory may be slower or fail on weak machines. Streams exist on the web platform, but your converter may need refactoring if it currently assumes full in-memory files. ([MDN Web Docs][2])

**2. Native Rust dependencies**
If your converter uses crates that depend on OS features, filesystem APIs, threads, or native C libraries, you may need to replace or adapt those for Wasm. Pure Rust logic usually ports best.

**3. Threads / performance**
If you want multithreading in Wasm, browser support exists, but it relies on shared memory features and usually requires special headers such as cross-origin isolation settings on your site. Single-threaded Wasm is much simpler to deploy. ([MDN Web Docs][3])

**4. Privacy claim**
You can honestly say “processed locally in your browser” **only if** your app does not send the file contents to a server. That is an implementation choice, not automatic magic from Wasm. The file picker itself does not upload anything. Network transfer only happens if your code requests it. ([MDN Web Docs][1])

A good way to structure the Rust side is:

* make a core function like `fn convert_ifc_to_lbd(input: &[u8]) -> Result<Vec<u8>, Error>`
* add a thin Wasm export like `pub fn convert(input: &[u8]) -> Vec<u8>`
  That keeps the converter reusable for both CLI and browser builds.

A minimal product stack could be:

* Rust + `wasm-bindgen`
* `wasm-pack` for bundling
* simple frontend in vanilla JS, React, or Svelte
* optional Web Worker so heavy conversion does not freeze the UI
* optional PWA/service worker if you want offline use after first load. ([MDN Web Docs][4])

So the answer is: **yes, this is a very reasonable design**, and for a privacy-friendly IFC converter it is often the best approach.

A few labels you can use publicly:

* “Runs locally in your browser”
* “No file upload required”
* “Client-side conversion via WebAssembly”

One important nuance:

* the **website assets** still come from a server/CDN the first time
* but the **user’s IFC file does not need to go to your server at all**

If you want, I can sketch the exact Rust/Wasm project structure for your converter, including a minimal `wasm-bindgen` interface and browser file-handling example.

[1]: https://developer.mozilla.org/en-US/docs/Web/API/File_API/Using_files_from_web_applications?utm_source=chatgpt.com "Using files from web applications - Web APIs - MDN Web Docs"
[2]: https://developer.mozilla.org/en-US/docs/Web/API/Streams_API?utm_source=chatgpt.com "Streams API - MDN Web Docs"
[3]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/SharedArrayBuffer?utm_source=chatgpt.com "SharedArrayBuffer - JavaScript - MDN Web Docs"
[4]: https://developer.mozilla.org/en-US/docs/Web/API/Service_Worker_API/Using_Service_Workers?utm_source=chatgpt.com "Using Service Workers - Web APIs | MDN"
