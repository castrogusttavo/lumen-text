const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const inline = (s: string) =>
  esc(s)
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a href="$2" rel="noopener">$1</a>')
    .replace(/(^|[\s(])(https?:\/\/[^\s<)]+)/g, '$1<a href="$2" rel="noopener">$2</a>')
    .replace(/(^|\s)@([\w-]+)/g, '$1<a href="https://github.com/$2" rel="noopener">@$2</a>');
/** Minimal Markdown (headings, bullets, paragraphs) for GitHub-generated release notes. */
export function renderNotes(md: string): string {
  const out: string[] = [];
  let list = false;
  for (const raw of md.replace(/\r/g, "").split("\n")) {
    const l = raw.trimEnd();
    const li = /^\s*[*-]\s+(.*)/.exec(l);
    if (li) { if (!list) { out.push("<ul>"); list = true; } out.push(`<li>${inline(li[1]!)}</li>`); continue; }
    if (list) { out.push("</ul>"); list = false; }
    const h = /^(#{1,4})\s+(.*)/.exec(l);
    if (h) out.push(`<h4>${inline(h[2]!)}</h4>`);
    else if (l) out.push(`<p>${inline(l)}</p>`);
  }
  if (list) out.push("</ul>");
  return out.join("\n");
}
