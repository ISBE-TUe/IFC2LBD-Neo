const { chromium } = require("playwright");
const http = require("http");
const fs = require("fs");
const path = require("path");

const IFC_FILE = process.env.IFC_FILE || "/app/test-file.ifc";
const MODULES = (process.env.MODULES || "").split(",").filter(Boolean);
const OPTS = (process.env.OPTS || "").split(",").filter(Boolean);

async function main() {
    const distDir = "/app/web/wasm-prototype/dist";
    fs.copyFileSync(IFC_FILE, path.join(distDir, "test-file.ifc"));

    const server = http.createServer((req, res) => {
        let fp = path.join(distDir, req.url === "/" ? "/index.html" : req.url);
        if (req.url.includes("?")) fp = fp.split("?")[0];
        res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
        res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
        res.setHeader("Cross-Origin-Resource-Policy", "same-origin");
        if (!fs.existsSync(fp)) { res.statusCode = 404; res.end("404"); return; }
        const types = { ".html": "text/html", ".js": "application/javascript", ".wasm": "application/wasm", ".ifc": "application/octet-stream" };
        res.setHeader("Content-Type", types[path.extname(fp)] || "application/octet-stream");
        fs.createReadStream(fp).pipe(res);
    });
    await new Promise(r => server.listen(3080, r));

    const browser = await chromium.launch({ args: ["--no-sandbox", "--enable-experimental-webassembly-features"] });
    const page = await browser.newPage();
    page.on("pageerror", err => console.log(`[PAGE ERROR] ${err.message}`));
    page.on("console", msg => { if (msg.type() !== "warning") console.log(`[console.${msg.type()}] ${msg.text()}`); });
    await page.goto("http://localhost:3080/", { waitUntil: "networkidle", timeout: 15000 });

    const result = await page.evaluate(async (params) => {
        try {
            const resp = await fetch("/test-file.ifc");
            const bytes = new Uint8Array(await resp.arrayBuffer());

            const wc = `let ready=false,api=null;self.onmessage=async(e)=>{const{id,type,payload}=e.data||{};if(!id||type!=="convert")return;try{if(!ready){api=await import("http://localhost:3080/wasm64/ifc2lbd_wasm.js");await api.default();await api.initNeoThreadPool(2);ready=true}const input=new Uint8Array(payload.inputBuffer);const sink=(ev)=>{if(ev?.type==="stageEvent")self.postMessage({id,type:"stage",p:ev.pluginId,s:ev.status,e:ev.error})};await api.convertIfcToSink(input,payload.request,sink);self.postMessage({id,type:"done"})}catch(err){self.postMessage({id,type:"error",error:err.message||String(err)})}};`;
            const blob = new Blob([wc], {type:"application/javascript"});
            const wurl = URL.createObjectURL(blob);
            const worker = new Worker(wurl, {type:"module"});

            const result = await new Promise((resolve,reject)=>{
                worker.addEventListener("message",(e)=>{
                    const d=e.data;
                    if(d.type==="stage")console.log(`[w] ${d.p}: ${d.s}${d.e?" ERR: "+d.e:""}`);
                    else if(d.type==="error")reject(new Error(d.error));
                    else if(d.type==="done")resolve({success:true});
                });
                const copy=bytes.slice();
                worker.postMessage({id:"c",type:"convert",payload:{
                    inputBuffer:copy.buffer,
                    request:{moduleIds:params.modules,moduleOptions:params.opts,baseUri:"https://lbd.example.com/",outputStem:"out",executionMode:"lowmem"}
                }},[copy.buffer]);
            }).catch(e=>({success:false,error:e.message}));

            worker.terminate();
            URL.revokeObjectURL(wurl);
            return result;
        } catch (err) { return {success:false,error:err.message}; }
    }, { modules: MODULES, opts: OPTS });

    console.log("Result:", JSON.stringify(result, null, 2));
    await browser.close();
    server.close();
    process.exit(result.success ? 0 : 1);
}

main().catch(e => { console.error(e); process.exit(1); });
