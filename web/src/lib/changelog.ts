// Builds the changelog page from the GitHub releases alone (no hand-written file needed).
// Release notes are commit subjects (the release job writes "- subject (`sha`)"); older releases carry
// GitHub's generated "* subject by @user in <pr url>". Subjects are classified by their Conventional
// Commit prefix and, for free-form ones, by their first word, then grouped per day.
// Keep the heuristics in sync with the `type` mapping in .github/workflows/ci.yml (release notes).

export type Kind = "feature" | "fix" | "perf" | "docs" | "ci" | "other";

export const KIND_LABEL: Record<Kind, string> = {
  feature: "Features",
  fix: "Bug fixes",
  perf: "Performance",
  docs: "Docs",
  ci: "Build and CI",
  other: "Other changes",
};
/** Order of the groups on the page; the first two are open, the rest folded. */
export const KIND_ORDER: Kind[] = ["feature", "fix", "perf", "other", "docs", "ci"];

export type Item = { text: string; kind: Kind; sha?: string; pr?: string; tag: string };
export type Release = { tag: string; publishedAt: string; url: string; notes: string };
export type Day = { date: string; first: string; last: string; releases: { tag: string; url: string }[]; groups: { kind: Kind; items: Item[] }[]; count: number };

const CONVENTIONAL = /^(feat|feature|fix|perf|docs|ci|build|chore|refactor|test|style|revert)(\([^)]*\))?(!)?:\s*/i;

export function classify(subject: string): { kind: Kind; text: string } {
  const m = CONVENTIONAL.exec(subject);
  if (m) {
    const t = m[1]!.toLowerCase();
    const text = subject.slice(m[0].length);
    const kind: Kind = t === "feat" || t === "feature" ? "feature" : t === "fix" ? "fix" : t === "perf" ? "perf" : t === "docs" ? "docs" : t === "ci" || t === "build" ? "ci" : "other";
    return { kind, text: text.charAt(0).toUpperCase() + text.slice(1) };
  }
  const s = subject.trim();
  if (/^(fix(es|ed)?|hotfix|bug|repair|correct|patch|stop|no more|don'?t|never)\b/i.test(s) || /\b(bug|crash|orphan|regression|leak)\b/i.test(s) && !/^(add|new)\b/i.test(s)) return { kind: "fix", text: s };
  if (/^(perf|faster|speed|optimi[sz]e|reduce)\b/i.test(s)) return { kind: "perf", text: s };
  if (/^(docs?|readme|changelog|roadmap|document)\b/i.test(s)) return { kind: "docs", text: s };
  if (/^(ci|release|packaging|build|windows: link|bump|update dependenc|deps)\b/i.test(s) || /\b(workflow|dry run)\b/i.test(s)) return { kind: "ci", text: s };
  if (/^(add|new|support|implement|introduce|ghp|lsp|dap|debug|editor|show|allow|enable|make|let)\b/i.test(s)) return { kind: "feature", text: s };
  return { kind: "other", text: s };
}

/** Commit-subject lines of one release's notes. Lines that are not bullets (headings, "Full diff") are ignored. */
export function parseNotes(notes: string, tag: string): Item[] {
  const items: Item[] = [];
  for (const raw of notes.replace(/\r/g, "").split("\n")) {
    const b = /^\s*[*-]\s+(.*)$/.exec(raw);
    if (!b) continue;
    let line = b[1]!.trim();
    let sha: string | undefined;
    let pr: string | undefined;
    const s = /\s*\(`([0-9a-f]{7,40})`\)\s*$/.exec(line);
    if (s) { sha = s[1]; line = line.slice(0, s.index).trim(); }
    const g = /\s+by\s+@[\w-]+\s+in\s+(https?:\S+\/pull\/(\d+))\s*$/.exec(line);
    if (g) { pr = g[1]; line = line.slice(0, g.index).trim(); }
    const n = /\s*\(#(\d+)\)\s*$/.exec(line);
    if (n) { pr = pr ?? `#${n[1]}`; line = line.slice(0, n.index).trim(); }
    if (!line) continue;
    const { kind, text } = classify(line);
    items.push({ text, kind, sha, pr, tag });
  }
  return items;
}

/** One entry per UTC day: all releases of that day merged, duplicates removed, grouped by kind. */
export function byDay(releases: Release[]): Day[] {
  const days = new Map<string, { releases: Release[]; items: Item[] }>();
  for (const r of releases) {
    const date = r.publishedAt.slice(0, 10);
    const d = days.get(date) ?? { releases: [], items: [] };
    d.releases.push(r);
    d.items.push(...parseNotes(r.notes, r.tag));
    days.set(date, d);
  }
  return [...days.entries()]
    .sort((a, b) => (a[0] < b[0] ? 1 : -1))
    .map(([date, d]) => {
      const seen = new Set<string>();
      const items = d.items.filter((i) => {
        const key = `${i.kind}:${i.text.toLowerCase()}`;
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });
      const sorted = [...d.releases].sort((a, b) => (a.publishedAt < b.publishedAt ? -1 : 1));
      return {
        date,
        first: sorted[0]!.tag,
        last: sorted[sorted.length - 1]!.tag,
        releases: sorted.map((r) => ({ tag: r.tag, url: r.url })),
        groups: KIND_ORDER.map((kind) => ({ kind, items: items.filter((i) => i.kind === kind) })).filter((g) => g.items.length > 0),
        count: items.length,
      };
    });
}
