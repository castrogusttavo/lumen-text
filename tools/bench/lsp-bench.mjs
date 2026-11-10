#!/usr/bin/env node
// Language-server speed, measured through the protocol (no GUI): spawn the server the way the
// editor does, open src/broken.ts (one deliberate type error) and time spawn -> first diagnostics
// that contain the error. Then read the PSS of the server's process tree.
//
// Usage: lsp-bench.mjs --mode tsgo-lumen|tsgo-full|tsserver-vscode|vtsls-zed [--runs N] [--out file.json]
//   tsgo-lumen      tsgo --lsp --stdio inside Lumen's "narrow mirror" (src/lsp.rs write_mirror), pull diagnostics
//   tsgo-full       same binary on the real tsconfig (whole project)
//   tsserver-vscode the tsserver bundled with the VS Code tarball (run with its own Electron as node)
//   vtsls-zed       the vtsls Zed downloaded into the isolated profile (what Zed uses for TypeScript)
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i > 0 ? process.argv[i + 1] : d; };
const BENCH = process.env.LUMEN_BENCH_DIR || path.join(os.homedir(), ".cache/lumen-bench");
const ROOT = path.join(BENCH, "sample");
const FILE = path.join(ROOT, "src/broken.ts");
const mode = arg("mode"), runs = +arg("runs", 5), out = arg("out");
const TSGO = path.join(os.homedir(), ".local/share/lumen-text/tsgo/node_modules/@typescript/native-preview-linux-x64/lib/tsgo");
const VSC = path.join(BENCH, "vscode/VSCode-linux-x64");
const ZED = path.join(BENCH, "profiles/zed/data/zed");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const uri = (p) => "file://" + p;

function descendants(root) {
  const kids = new Map();
  for (const d of fs.readdirSync("/proc")) {
    if (!/^\d+$/.test(d)) continue;
    try {
      const st = fs.readFileSync(`/proc/${d}/stat`, "utf8");
      const pp = +st.slice(st.lastIndexOf(")") + 2).split(" ")[1];
      if (!kids.has(pp)) kids.set(pp, []);
      kids.get(pp).push(+d);
    } catch {}
  }
  const all = [root], q = [root];
  while (q.length) for (const c of kids.get(q.pop()) || []) { all.push(c); q.push(c); }
  return all;
}
function mem(root) {
  let pss = 0, rss = 0, n = 0;
  for (const p of descendants(root)) {
    try {
      for (const l of fs.readFileSync(`/proc/${p}/smaps_rollup`, "utf8").split("\n")) {
        if (l.startsWith("Pss:")) pss += +l.split(/\s+/)[1];
        else if (l.startsWith("Rss:")) rss += +l.split(/\s+/)[1];
      }
      n++;
    } catch {}
  }
  return { pss_mb: pss / 1024, rss_mb: rss / 1024, procs: n };
}

// --- LSP framing -----------------------------------------------------------------------------
function lspClient(child) {
  let buf = Buffer.alloc(0), id = 0;
  const waiters = new Map(), handlers = [];
  child.stdout.on("data", (d) => {
    buf = Buffer.concat([buf, d]);
    for (;;) {
      const he = buf.indexOf("\r\n\r\n");
      if (he < 0) return;
      const m = /Content-Length: (\d+)/i.exec(buf.slice(0, he).toString());
      const len = +m[1];
      if (buf.length < he + 4 + len) return;
      const msg = JSON.parse(buf.slice(he + 4, he + 4 + len).toString());
      buf = buf.slice(he + 4 + len);
      if (msg.id !== undefined && msg.method) { // server -> client request
        let result = null;
        if (msg.method === "workspace/configuration") result = (msg.params.items || []).map(() => null);
        send({ jsonrpc: "2.0", id: msg.id, result });
      } else if (msg.id !== undefined && waiters.has(msg.id)) { waiters.get(msg.id)(msg); waiters.delete(msg.id); }
      else for (const h of handlers) h(msg);
    }
  });
  const send = (o) => { const s = JSON.stringify(o); child.stdin.write(`Content-Length: ${Buffer.byteLength(s)}\r\n\r\n${s}`); };
  return {
    request: (method, params) => new Promise((res) => { const i = ++id; waiters.set(i, res); send({ jsonrpc: "2.0", id: i, method, params }); }),
    notify: (method, params) => send({ jsonrpc: "2.0", method, params }),
    on: (h) => handlers.push(h),
  };
}
const initParams = (root) => ({
  processId: process.pid, clientInfo: { name: "bench" }, rootUri: uri(root),
  workspaceFolders: [{ uri: uri(root), name: path.basename(root) }],
  capabilities: { workspace: { configuration: true, workspaceFolders: true },
    textDocument: { synchronization: {}, diagnostic: { dynamicRegistration: false, relatedDocumentSupport: true }, publishDiagnostics: {} } },
});

// --- Lumen narrow mirror (same layout as src/lsp.rs write_mirror) ----------------------------------
function makeMirror() {
  const mirror = path.join(BENCH, "lsp-mirror");
  fs.rmSync(mirror, { recursive: true, force: true });
  fs.mkdirSync(mirror, { recursive: true });
  for (const n of fs.readdirSync(ROOT)) {
    if (n.startsWith("tsconfig") || n.startsWith("jsconfig") || n === ".git") continue;
    fs.symlinkSync(path.join(ROOT, n), path.join(mirror, n));
  }
  fs.writeFileSync(path.join(mirror, "tsconfig.json"), JSON.stringify({
    include: ["src/broken.ts"], compilerOptions: { incremental: false, noEmit: true }, extends: path.join(ROOT, "tsconfig.json") }, null, 2));
  return mirror;
}

async function runTsgo(lumen) {
  const root = lumen ? makeMirror() : ROOT;
  const file = path.join(root, "src/broken.ts"), u = uri(file);
  const text = fs.readFileSync(FILE, "utf8");
  const t0 = performance.now();
  const child = spawn(TSGO, ["--lsp", "--stdio"], { cwd: root, stdio: ["pipe", "pipe", "ignore"],
    env: { ...process.env, ...(lumen ? { GOMEMLIMIT: "512MiB", GOGC: "50" } : {}) } });
  const c = lspClient(child);
  await c.request("initialize", initParams(root));
  c.notify("initialized", {});
  c.notify("textDocument/didOpen", { textDocument: { uri: u, languageId: "typescript", version: 1, text } });
  const r = await c.request("textDocument/diagnostic", { textDocument: { uri: u } });
  const ms = performance.now() - t0;
  const n = r.result?.items?.length ?? 0;
  await sleep(1500);
  const m = mem(child.pid);
  child.kill("SIGKILL");
  return { ms, diagnostics: n, ...m };
}

async function runTsserver() {
  const js = path.join(VSC, "resources/app/extensions/node_modules/typescript/lib/tsserver.js");
  const t0 = performance.now();
  const child = spawn(path.join(VSC, "code"), [js, "--useInferredProjectPerProjectRoot", "--disableAutomaticTypingAcquisition"],
    { cwd: ROOT, stdio: ["pipe", "pipe", "ignore"], env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" } });
  let seq = 0, done, ms = null, n = 0;
  const p = new Promise((r) => (done = r));
  let buf = "";
  child.stdout.on("data", (d) => {
    buf += d;
    for (const line of buf.split("\n")) {
      if (!line.startsWith("{")) continue;
      try {
        const m = JSON.parse(line);
        if (m.type === "event" && m.event === "semanticDiag" && m.body?.file === FILE && m.body.diagnostics.length) {
          ms = performance.now() - t0; n = m.body.diagnostics.length; done();
        }
      } catch {}
    }
    buf = buf.slice(buf.lastIndexOf("\n") + 1);
  });
  const send = (command, args) => child.stdin.write(JSON.stringify({ seq: ++seq, type: "request", command, arguments: args }) + "\n");
  send("open", { file: FILE, projectRootPath: ROOT });
  send("geterr", { files: [FILE], delay: 0 });
  await Promise.race([p, sleep(60000)]);
  await sleep(1500);
  const m = mem(child.pid);
  child.kill("SIGKILL");
  if (ms === null) throw new Error("no semanticDiag within 60 s");
  return { ms, diagnostics: n, ...m };
}

async function runVtsls() {
  const bin = path.join(ZED, "languages/vtsls/node_modules/@vtsls/language-server/bin/vtsls.js");
  const nodeDir = path.join(ZED, "node");
  let node = process.execPath;
  if (fs.existsSync(nodeDir)) {
    const f = fs.readdirSync(nodeDir).find((d) => d.startsWith("node-"));
    if (f && fs.existsSync(path.join(nodeDir, f, "bin/node"))) node = path.join(nodeDir, f, "bin/node");
  }
  if (!fs.existsSync(bin)) throw new Error("vtsls not found in the Zed profile (run the Zed warm-up first)");
  const t0 = performance.now();
  const child = spawn(node, [bin, "--stdio"], { cwd: ROOT, stdio: ["pipe", "pipe", "ignore"] });
  const c = lspClient(child);
  let ms = null, n = 0, done;
  const p = new Promise((r) => (done = r));
  c.on((m) => {
    if (m.method === "textDocument/publishDiagnostics" && m.params.uri === uri(FILE) && m.params.diagnostics.length) {
      ms = performance.now() - t0; n = m.params.diagnostics.length; done();
    }
  });
  await c.request("initialize", initParams(ROOT));
  c.notify("initialized", {});
  c.notify("textDocument/didOpen", { textDocument: { uri: uri(FILE), languageId: "typescript", version: 1, text: fs.readFileSync(FILE, "utf8") } });
  await Promise.race([p, sleep(90000)]);
  await sleep(1500);
  const m = mem(child.pid);
  child.kill("SIGKILL");
  if (ms === null) throw new Error("no publishDiagnostics within 90 s");
  return { ms, diagnostics: n, ...m, node, vtsls: JSON.parse(fs.readFileSync(path.join(path.dirname(bin), "../package.json"), "utf8")).version };
}

const fn = { "tsgo-lumen": () => runTsgo(true), "tsgo-full": () => runTsgo(false), "tsserver-vscode": runTsserver, "vtsls-zed": runVtsls }[mode];
if (!fn) { console.error("usage: lsp-bench.mjs --mode tsgo-lumen|tsgo-full|tsserver-vscode|vtsls-zed [--runs N] [--out f]"); process.exit(2); }
const results = [];
for (let i = 0; i < runs + 1; i++) { // first run is a warm-up (disk cache), reported separately
  try {
    const r = await fn();
    console.error(`[${mode}] ${i === 0 ? "warm-up" : "run " + i}:`, JSON.stringify(r));
    if (i > 0) results.push(r);
  } catch (e) { console.error(`[${mode}] failed:`, e.message); results.push({ error: e.message }); }
  await sleep(1000);
}
const ok = results.filter((r) => !r.error);
const med = (k) => { const v = ok.map((r) => r[k]).sort((a, b) => a - b); return v.length ? (v.length % 2 ? v[(v.length - 1) / 2] : (v[v.length / 2 - 1] + v[v.length / 2]) / 2) : null; };
const st = (k) => ok.length ? { median: med(k), min: Math.min(...ok.map((r) => r[k])), max: Math.max(...ok.map((r) => r[k])), n: ok.length } : null;
const summary = { mode, runs: results, stats: { ms: st("ms"), pss_mb: st("pss_mb"), rss_mb: st("rss_mb"), procs: st("procs") }, extra: ok[0] ? { node: ok[0].node, vtsls: ok[0].vtsls } : {} };
if (out) fs.writeFileSync(out, JSON.stringify(summary, null, 2));
else console.log(JSON.stringify(summary, null, 2));
