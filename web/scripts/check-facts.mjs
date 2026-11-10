// Keeps printed numbers truthful. Runs first in `pnpm build`.
//  - src/data/facts.json holds the non-benchmark facts; benchmarks.json holds every measurement.
//  - facts that can be derived from the repository are verified against it (themes, grammars, README memory goal).
//  - hard-coded numbers in components, pages and docs must agree with those sources.
// Docs lean-mode paragraphs (typescript.md, troubleshooting.md) are measured ranges, not claims checked here.
import { readFile, readdir } from "node:fs/promises";
import { join, extname } from "node:path";

const web = new URL("../", import.meta.url).pathname;
const read = (p) => readFile(join(web, p), "utf8");
const facts = JSON.parse(await read("src/data/facts.json"));
const bench = JSON.parse(await read("src/data/benchmarks.json"));
const problems = [];
const bad = (m) => problems.push(m);

// 1. Facts against the repository
const count = async (dir) => { try { return (await readdir(join(web, "..", dir))).length; } catch { return null; } };
const themes = await count("assets/themes");
if (themes !== null && themes !== facts.themes) bad(`facts.themes is ${facts.themes} but assets/themes has ${themes} files`);
const gram = await count("assets/grammars");
if (gram !== null && gram !== facts.grammars) bad(`facts.grammars is ${facts.grammars} but assets/grammars has ${gram} files`);
const themesJson = JSON.parse(await read("src/data/themes.json"));
if (themesJson.length !== facts.themes) bad(`facts.themes is ${facts.themes} but src/data/themes.json lists ${themesJson.length}`);
for (const k of ["tests", "themes", "grammars"]) if (typeof facts[k] !== "number" || !facts[k]) bad(`facts.${k} must be a positive number`);
try {
  const readme = await readFile(join(web, "..", "README.md"), "utf8");
  // The site promises no fixed total. If the README brings a total back, the copy must be revisited on purpose.
  if (/memory budget \([^)]*\*\*about \d+ MB/.test(readme) || /goal is \*\*about \d+ MB/.test(readme)) bad("README.md states a fixed memory goal again; update the site copy (Hero, Base) deliberately and this check");
} catch {}
// Legacy copies inside benchmarks.json must not disagree either
const bf = bench.facts ?? {};
for (const [k, v] of [["themes", facts.themes], ["grammars", facts.grammars], ["testsPassed", facts.tests]]) {
  // Unused by the site (components read facts.json); warn only, benchmarks.json has another owner.
  if (bf[k]?.value !== undefined && bf[k].value !== v) console.warn(`[facts] warning: benchmarks.json facts.${k} (${bf[k].value}) is stale; facts.json says ${v}`);
}

// 2. Hard-coded numbers in the sources
async function* walk(dir) {
  for (const e of await readdir(join(web, dir), { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) yield* walk(p);
    else if ([".astro", ".tsx", ".ts", ".md", ".mdx"].includes(extname(e.name))) yield p;
  }
}
const num = (s) => Number(s.replace(/,/g, ""));
const rules = [
  [/\b(\d[\d,]*)\s+(?:bundled\s+)?(?:VS Code\s+)?themes\b/gi, facts.themes, "themes"],
  [/\b(\d[\d,]*)\s+(?:TextMate\s+)?grammars\b/gi, facts.grammars, "grammars"],
  [/\b(\d[\d,]*)\s+(?:unit\s+|automated\s+)?tests?\b(?!\s+(?:file|run|runner))/gi, facts.tests, "tests"],
];
for await (const f of walk("src")) {
  const txt = await read(f);
  for (const [re, want, what] of rules) {
    for (const m of txt.matchAll(re)) if (num(m[1]) !== want) bad(`${f}: "${m[0]}" contradicts ${what} = ${want}`);
  }
  // The only memory totals allowed in prose are measured ones; a fixed "memory goal" or "in about N MB" promise is not.
  for (const m of txt.matchAll(/memory goal|(?:in|with) about\s+\d+\s*MB/gi)) if (!f.endsWith("check-facts.mjs")) bad(`${f}: "${m[0]}" is a fixed memory promise; reference the measured fields instead`);
  if (/tests?\s+passed|passing tests/i.test(txt) && /\b\d{3,}\b/.test(txt.match(/.{0,40}(tests?\s+passed|passing tests).{0,40}/i)?.[0] ?? "")) bad(`${f}: hard-coded passing-test count; read facts.json`);
}

if (!problems.length) { console.log(`[facts] ok (${facts.tests} tests, ${facts.themes} themes, ${facts.grammars} grammars)`); process.exit(0); }
console.error(`[facts] ${problems.length} problem(s):\n  - ${problems.join("\n  - ")}`);
process.exit(1);
