// Fails the build when the comparison data still has placeholders.
// Bypass for local previews only: ALLOW_PENDING_BENCH=1 pnpm build
import { readFile } from "node:fs/promises";

const data = JSON.parse(await readFile(new URL("../src/data/benchmarks.json", import.meta.url), "utf8"));
const problems = [];
if (!data.measuredAt) problems.push("measuredAt is null");
if (!data.machine?.cpu || !data.machine?.storage) problems.push("machine.cpu and machine.storage must be filled in");
for (const e of data.editors) if (!e.version) problems.push(`editors[${e.id}].version is null`);
for (const m of data.metrics) {
  if (m.required === false) { for (const [k, c] of Object.entries(m.values ?? {})) if (c?.source === "unavailable" && !String(c.reason ?? "").trim()) problems.push(`${m.id} / ${k}: unavailable needs a non-empty reason`); continue; }
  for (const e of data.editors) {
    const c = m.values?.[e.id];
    if (!c) problems.push(`${m.id} / ${e.id}: missing cell`);
    else if (c.source === "unavailable") { if (c.value !== null && c.value !== undefined) problems.push(`${m.id} / ${e.id}: unavailable cell must have value null`); if (typeof c.reason !== "string" || !c.reason.trim()) problems.push(`${m.id} / ${e.id}: unavailable needs a non-empty reason`); }
    else if (c.value === null || c.value === undefined) problems.push(`${m.id} / ${e.id}: value is null (use source "unavailable" with a reason if it cannot be measured)`);
    else if (!["measured", "external-reference"].includes(c.source)) problems.push(`${m.id} / ${e.id}: source must be "measured", "external-reference" or "unavailable"`);
    else if (c.source === "external-reference" && !/^https?:\/\//.test(c.url ?? "")) problems.push(`${m.id} / ${e.id}: external-reference needs a url`);
  }
}
if (problems.length === 0) { console.log("[bench] all required metrics are filled"); process.exit(0); }
const msg = `[bench] ${problems.length} placeholder(s) in src/data/benchmarks.json:\n  - ${problems.slice(0, 40).join("\n  - ")}${problems.length > 40 ? `\n  ... and ${problems.length - 40} more` : ""}`;
if (process.env.ALLOW_PENDING_BENCH === "1") { console.warn(msg + "\n[bench] ALLOW_PENDING_BENCH=1: continuing (do NOT publish this build)"); process.exit(0); }
console.error(msg + "\n\nFill the file with tools/bench-compare.sh (see web/README.md) or set ALLOW_PENDING_BENCH=1 for a local preview.");
process.exit(1);
