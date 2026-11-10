// Generates docs reference data from the Rust sources so the docs cannot drift from the app:
//   src/keymap.rs      DEFAULTS    -> keymap.json
//   src/app_overlay/commands.rs COMMANDS    -> commands.json (palette labels, shortcuts)
//   src/settingsui/model.rs + config.rs  -> settings.json (labels, help, kind, defaults)
import { readFile, writeFile } from "node:fs/promises";

const src = (f) => readFile(new URL(`../../src/${f}`, import.meta.url), "utf8");
const out = (f, d) => writeFile(new URL(`../src/data/${f}`, import.meta.url), JSON.stringify(d, null, 1) + "\n");

// --- keymap
const km = await src("keymap.rs");
const block = km.slice(km.indexOf("pub const DEFAULTS"), km.indexOf("];", km.indexOf("pub const DEFAULTS")));
const keymap = [...block.matchAll(/\("([^"]+)",\s*"([^"]+)"\)/g)].map(([, combo, id]) => ({ combo: combo.replace(/\\\\/g, "\\"), id }));
await out("keymap.json", keymap);

// --- commands (palette)
const ov = await src("app_overlay/commands.rs");
const cstart = ov.indexOf("pub const COMMANDS");
const cblock = ov.slice(cstart, ov.indexOf("\n];", cstart));
const commands = [...cblock.matchAll(/\("([^"]+)",\s*"([^"]*)",\s*Cmd::(\w+)\)/g)].map(([, label, shortcut, id]) => ({ label, shortcut: shortcut.replace(/\\\\/g, "\\"), id }));
await out("commands.json", commands);

// --- settings
const ui = await src("settingsui/model.rs");
const sstart = ui.indexOf("pub fn default_settings");
const ext = await src("settings_ext.rs");
const estart = ext.indexOf("pub fn defs");
// The font / editor-display rows live in settings_ext.rs and are appended to the main list.
const sblock =
  ui.slice(sstart, ui.indexOf("\n    ]", sstart)) + "\n" + ext.slice(estart, ext.indexOf("\n    ]\n}", estart));
const defs = [];
const re = /d\(\s*"(\w+)",\s*"([^"]+)",\s*"([^"]+)",\s*"((?:[^"\\]|\\.)*)",\s*([^;]*?)\),?\s*(?=\n\s*(?:d\(|\/\/|$))/gs;
for (const m of sblock.matchAll(re)) {
  const kind = m[5].trim();
  const choice = /Choice\(&\[([^\]]*)\]\)/.exec(kind);
  const num = /Number\s*\{\s*min:\s*([\d.]+),\s*max:\s*([\d.]+)/.exec(kind);
  defs.push({
    key: m[1], section: m[2], label: m[3], help: m[4].replace(/\\"/g, '"').replace(/\\n/g, " "),
    kind: choice ? "choice" : num ? "number" : kind.startsWith("Bool") ? "bool" : "text",
    options: choice ? [...choice[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]) : undefined,
    range: num ? [Number(num[1]), Number(num[2])] : undefined,
  });
}
const cfg = await src("config.rs");
const dstart = cfg.indexOf("impl Default for Config");
const dblock = cfg.slice(dstart, cfg.indexOf("\n}\n", dstart));
const defaults = {};
for (const m of dblock.matchAll(/^\s+(\w+):\s*(.+?),\s*$/gm)) {
  let v = m[2].trim();
  if (/^".*"\.into\(\)$/.test(v)) v = v.slice(1, v.indexOf('".into'));
  else if (v === "String::new()") v = "";
  else if (v === "true" || v === "false") v = v === "true";
  else if (/^-?[\d.]+$/.test(v)) v = Number(v);
  else v = null;
  defaults[m[1]] = v;
}
for (const d of defs) d.default = d.key in defaults ? defaults[d.key] : d.key.startsWith("status_") && !d.key.startsWith("status_pos_") ? true : null;
await out("settings.json", defs);
console.log(`keymap ${keymap.length}, commands ${commands.length}, settings ${defs.length}`);
