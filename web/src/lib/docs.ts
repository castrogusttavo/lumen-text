import { getCollection, type CollectionEntry } from "astro:content";
import { url } from "./url";

export const SECTIONS = ["Start", "Using Lumen", "Customize", "Project"] as const;
export type Doc = CollectionEntry<"docs">;

export async function allDocs(): Promise<Doc[]> {
  const docs = await getCollection("docs");
  return docs.sort((a, b) => SECTIONS.indexOf(a.data.section) - SECTIONS.indexOf(b.data.section) || a.data.order - b.data.order);
}
export const docUrl = (d: Doc) => url(`/docs/${d.id}`);

export function prettyCombo(combo: string): string {
  if (combo.startsWith("explorer:")) return `${prettyCombo(combo.slice(9))} (explorer)`;
  return combo.split(" ").map((stroke) => stroke.split("+").map((k) => (k.length === 1 ? k.toUpperCase() : k[0]!.toUpperCase() + k.slice(1))).join("+")).join(" ");
}
