// Docs: copy buttons on code blocks + a tiny client-side search over /docs/search.json (loaded on first focus).
document.querySelectorAll<HTMLPreElement>(".prose-docs pre").forEach((pre) => {
  const wrap = document.createElement("div");
  wrap.className = "code-wrap";
  pre.replaceWith(wrap);
  wrap.append(pre);
  const b = document.createElement("button");
  b.type = "button"; b.className = "copy-btn"; b.textContent = "Copy"; b.setAttribute("aria-label", "Copy code");
  b.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(pre.innerText.replace(/\n$/, "")); b.textContent = "Copied"; } catch { b.textContent = "Press Ctrl+C"; }
    setTimeout(() => (b.textContent = "Copy"), 1500);
  });
  wrap.append(b);
});

type Entry = { t: string; u: string; s: string; h: string[]; x: string };
const root = document.querySelector<HTMLElement>("[data-docs]");
const input = document.querySelector<HTMLInputElement>("[data-docs-search]");
const list = document.querySelector<HTMLUListElement>("[data-docs-results]");
let index: Entry[] | null = null;
let sel = -1;

async function load() {
  if (index || !root) return;
  try { index = await (await fetch(root.dataset.searchUrl!)).json(); } catch { index = []; }
}
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
function search(q: string) {
  const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length || !index) return [];
  return index.map((e) => {
    let score = 0;
    for (const t of terms) {
      if (e.t.toLowerCase().includes(t)) score += 10;
      if (e.h.some((h) => h.toLowerCase().includes(t))) score += 4;
      if (e.x.includes(t)) score += 1; else if (!e.t.toLowerCase().includes(t) && !e.h.some((h) => h.toLowerCase().includes(t))) return { e, score: 0 };
    }
    return { e, score };
  }).filter((r) => r.score > 0).sort((a, b) => b.score - a.score).slice(0, 8);
}
function render() {
  if (!input || !list) return;
  const q = input.value.trim();
  if (!q) { list.hidden = true; return; }
  const res = search(q);
  const first = q.toLowerCase().split(/\s+/)[0]!;
  list.innerHTML = res.length ? res.map((r, i) => {
    const at = r.e.x.indexOf(first);
    const snip = at >= 0 ? "…" + r.e.x.slice(Math.max(0, at - 40), at + 90) + "…" : r.e.x.slice(0, 100);
    return `<li role="option" aria-selected="${i === sel}"><a href="${r.e.u}" class="block rounded-lg px-3 py-2 text-sm ${i === sel ? "bg-card" : "hover:bg-card"}"><span class="font-medium">${esc(r.e.t)}</span> <span class="text-faint text-xs">${esc(r.e.s)}</span><span class="mt-0.5 block truncate text-xs text-muted">${esc(snip)}</span></a></li>`;
  }).join("") : `<li class="px-3 py-3 text-sm text-muted">No results for "${esc(q)}"</li>`;
  list.hidden = false;
}
input?.addEventListener("focus", () => void load());
input?.addEventListener("input", () => { sel = -1; void load().then(render); });
input?.addEventListener("keydown", (e) => {
  const links = list ? [...list.querySelectorAll<HTMLAnchorElement>("a")] : [];
  if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); sel = (sel + (e.key === "ArrowDown" ? 1 : -1) + links.length) % Math.max(links.length, 1); render(); }
  else if (e.key === "Enter" && links[Math.max(sel, 0)]) location.href = links[Math.max(sel, 0)]!.href;
  else if (e.key === "Escape") { input.value = ""; if (list) list.hidden = true; input.blur(); }
});
document.addEventListener("keydown", (e) => {
  if (e.key === "/" && document.activeElement !== input && !/INPUT|TEXTAREA/.test((document.activeElement as HTMLElement)?.tagName ?? "")) { e.preventDefault(); input?.focus(); }
});
document.addEventListener("click", (e) => { if (list && !list.contains(e.target as Node) && e.target !== input) list.hidden = true; });
