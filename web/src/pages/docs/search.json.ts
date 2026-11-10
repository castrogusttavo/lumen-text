import type { APIRoute } from "astro";
import { allDocs, docUrl } from "../../lib/docs";
import commands from "../../data/commands.json";
import settings from "../../data/settings.json";
import keymap from "../../data/keymap.json";

const plain = (md: string) => md.replace(/^---[\s\S]*?---/, "").replace(/```[\s\S]*?```/g, (m) => m.replace(/```\w*/g, " ")).replace(/[#>*_|`:\[\]()]/g, " ").replace(/\s+/g, " ").trim().toLowerCase();

export const GET: APIRoute = async () => {
  const docs = await allDocs();
  const index = docs.map((d) => {
    const body = d.body ?? "";
    const heads = [...body.matchAll(/^#{2,3}\s+(.+)$/gm)].map((m) => m[1]!.replace(/[`*]/g, ""));
    let extra = "";
    if (d.data.generated === "commands") extra = commands.map((c) => `${c.label} ${c.shortcut} ${c.id}`).join(" ");
    if (d.data.generated === "shortcuts") extra = keymap.map((k) => `${k.combo} ${k.id}`).join(" ");
    if (d.data.generated === "settings") extra = settings.map((s) => `${s.label} ${s.key} ${s.help}`).join(" ");
    return { t: d.data.title, u: docUrl(d), s: d.data.section, h: heads, x: plain(`${d.data.description} ${body}`) + " " + extra.toLowerCase() };
  });
  return new Response(JSON.stringify(index), { headers: { "content-type": "application/json" } });
};
