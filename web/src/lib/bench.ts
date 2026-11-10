import data from "../data/benchmarks.json";

export type Source = "measured" | "external-reference" | "pending" | "unavailable";
export interface Cell { value: number | null; source: Source; note?: string; url?: string; reason?: string }
export const bench = data as unknown as {
  schemaVersion: number; measuredAt: string | null; methodology: string;
  machine: { label: string; cpu: string | null; ramGb: number | null; storage: string | null; os: string | null; note?: string };
  editors: { id: string; name: string; version: string | null; highlight?: boolean; note?: string }[];
  metrics: { id: string; required?: boolean; group: string; label: string; description: string; unit: string; lowerIsBetter: boolean; scaleMax: number; values: Record<string, Cell> }[];
  groups: { id: string; label: string }[];
  facts: typeof data.facts;
};

export const fmt = (v: number) => (Number.isInteger(v) ? v.toLocaleString("en-US") : v.toLocaleString("en-US", { maximumFractionDigits: 1 }));
export const pct = (v: number, max: number) => Math.max(2, Math.min(100, (v / max) * 100));

/** A cell with a number to draw. Unavailable and not-comparable cells never get a bar. */
export const hasValue = (c?: Cell): c is Cell & { value: number } => !!c && c.value !== null && c.value !== undefined;
export const reasonOf = (c?: Cell) => (c?.reason ?? "").replace(/^not-comparable:\s*/i, "");
export const notComparable = (c?: Cell) => /^not-comparable/i.test(c?.reason ?? "");

/** Rank of an editor among the cells that have a value (lower is better when `lowerIsBetter`). */
export function standings(m: (typeof bench)["metrics"][number]) {
  const rows = bench.editors.map((e) => ({ e, c: m.values[e.id] })).filter((r) => hasValue(r.c)) as { e: (typeof bench)["editors"][number]; c: Cell & { value: number } }[];
  rows.sort((a, b) => (m.lowerIsBetter ? a.c.value - b.c.value : b.c.value - a.c.value));
  return rows;
}
