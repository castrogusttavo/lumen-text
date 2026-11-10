// Generates the benchmark workspace: ~300 TS files with cross imports, a
// 100,000-line generated file (big/big.ts, outside tsconfig) and one file with
// a deliberate type error (src/broken.ts). Usage: node gen-sample.mjs <dir>
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
const root = process.argv[2];
if (!root) { console.error("usage: gen-sample.mjs <dir>"); process.exit(2); }
rmSync(root, { recursive: true, force: true });
const MODS = 300, DIRS = 10;
mkdirSync(join(root, "big"), { recursive: true });
const file = (i) => `src/d${i % DIRS}/mod${i}.ts`;
for (let d = 0; d < DIRS; d++) mkdirSync(join(root, `src/d${d}`), { recursive: true });
for (let i = 0; i < MODS; i++) {
  const dep = i > 0 ? [Math.floor(i / 2), Math.max(0, i - 7)].filter((v, k, a) => v < i && a.indexOf(v) === k) : [];
  let s = "";
  for (const d of dep) s += `import { Entity${d}, make${d}, type Options${d} } from "../${"d" + (d % DIRS)}/mod${d}";\n`;
  s += `\nexport interface Options${i} { id: number; name: string; tags: string[]; parent?: Entity${dep[0] ?? i} | null }\n`;
  s += `export class Entity${i} {\n  constructor(public readonly opts: Options${i}) {}\n`;
  for (let m = 0; m < 6; m++) s += `  method${m}(x: number): number {\n    const arr = [1, 2, 3].map((v) => v * x + ${m});\n    return arr.reduce((a, b) => a + b, this.opts.id);\n  }\n`;
  s += `}\n\nexport function make${i}(id: number): Entity${i} {\n  return new Entity${i}({ id, name: \`entity-\${id}\`, tags: ["a", "b"], parent: null });\n}\n`;
  for (const d of dep) s += `export const use${d}_in${i} = (o: Options${d}): string => make${d}(o.id).opts.name + String(new Entity${d}(o).method0(${i}));\n`;
  writeFileSync(join(root, file(i)), s);
}
writeFileSync(join(root, "src/broken.ts"),
`import { make1 } from "./d1/mod1";

export const value: number = make1(1).opts.name; // type error: string is not assignable to number
export function oops(): string {
  return 42;
}
`);
writeFileSync(join(root, "src/index.ts"), `export * from "./d0/mod0";\n`);
writeFileSync(join(root, "tsconfig.json"), JSON.stringify({ compilerOptions: { target: "es2022", module: "esnext", moduleResolution: "bundler", strict: true, noEmit: true, skipLibCheck: true }, include: ["src"] }, null, 2));
writeFileSync(join(root, "README.md"), "# bench-sample\n\nGenerated TypeScript workspace used by tools/bench-compare.sh.\n");
writeFileSync(join(root, "package.json"), JSON.stringify({ name: "bench-sample", private: true, type: "module" }, null, 2));
// 100,000 lines, ~80 bytes per line.
const lines = [];
lines.push("// generated 100k-line TypeScript file");
let n = 1;
while (lines.length < 100000) {
  lines.push(`export function fn${n}(a: number, b: number): number {`);
  lines.push(`  const sum = a + b * ${n}; // computes a value for case number ${n}; padding text to reach roughly eight megabytes in total for the file, with some more filler words`);
  lines.push(`  const label: string = "item-" + String(sum) + "-" + String(${n});`);
  lines.push(`  return label.length > 12 ? sum : sum + label.length;`);
  lines.push(`}`);
  lines.push("");
  n++;
}
lines.length = 100000;
writeFileSync(join(root, "big/big.ts"), lines.join("\n") + "\n");
