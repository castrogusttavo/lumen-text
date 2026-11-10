// The status-bar RAM readout of the IDE replicas, built from measured fields in benchmarks.json
// (editor and tsgo separately). Falls back to no readout when a field is missing.
import { metrics } from "../data/benchmarks.json";

const lumen = (id: string) => (metrics as { id: string; values: Record<string, { value: number | null }> }[]).find((m) => m.id === id)?.values.lumen?.value ?? null;
const editor = lumen("ram-idle");
const tsgo = lumen("lsp-memory");
const mb = (v: number) => `${Math.round(v)} MB`;

export const statusRam = editor != null && tsgo != null ? `RAM ${mb(editor)} + tsgo ${mb(tsgo)}` : editor != null ? `RAM ${mb(editor)}` : "";
