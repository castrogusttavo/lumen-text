// Extracts a compact palette (UI + 7 token colors) from every bundled theme in ../assets/themes.
import { readdir, readFile, writeFile } from "node:fs/promises";

const dir = new URL("../../assets/themes/", import.meta.url);
const strip = (c) => (c && c.length > 7 ? c.slice(0, 7) : c);
const picks = {
  comment: ["comment"], keyword: ["keyword", "storage.type", "storage"], string: ["string"],
  fn: ["entity.name.function", "support.function", "entity.name"], type: ["entity.name.type", "support.type", "support.class", "entity.name.class"],
  number: ["constant.numeric", "constant", "number"], prop: ["variable.other.property", "meta.object-literal.key", "variable"],
};
const out = [];
for (const f of (await readdir(dir)).filter((x) => x.endsWith(".json")).sort()) {
  const t = JSON.parse(await readFile(new URL(f, dir), "utf8"));
  const c = t.colors ?? {};
  const rules = (t.tokenColors ?? t.settings ?? []).map((r) => ({ s: [].concat(r.scope ?? []).flatMap((x) => String(x).split(",").map((y) => y.trim())), c: r.settings?.foreground }));
  const fg = strip(c["editor.foreground"]) ?? "#d4d4d4";
  const find = (names) => {
    for (const n of names) for (const r of rules) if (r.c && r.s.includes(n)) return strip(r.c);
    return null;
  };
  const p = {};
  for (const [k, names] of Object.entries(picks)) p[k] = find(names) ?? fg;
  out.push({
    id: f.replace(".json", ""), name: t.name ?? f, dark: (t.type ?? "dark") !== "light",
    bg: strip(c["editor.background"]) ?? "#1e1e1e", fg, ...p,
  });
}
await writeFile(new URL("../src/data/themes.json", import.meta.url), JSON.stringify(out));
console.log(out.length, "themes");
