import data from "../data/benchmarks.json";

export type Source = "measured" | "external-reference" | "pending";
export interface Cell { value: number | null; source: Source; note?: string; url?: string }
export const bench = data as unknown as {
  schemaVersion: number; measuredAt: string | null; methodology: string;
  machine: { label: string; cpu: string | null; ramGb: number | null; storage: string | null; os: string | null; note?: string };
  editors: { id: string; name: string; version: string | null; highlight?: boolean; note?: string }[];
  metrics: { id: string; group: string; label: string; description: string; unit: string; lowerIsBetter: boolean; scaleMax: number; values: Record<string, Cell> }[];
  groups: { id: string; label: string }[];
  facts: typeof data.facts;
};

export const fmt = (v: number) => (Number.isInteger(v) ? v.toLocaleString("en-US") : v.toLocaleString("en-US", { maximumFractionDigits: 1 }));
export const pct = (v: number, max: number) => Math.max(2, Math.min(100, (v / max) * 100));
