import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { sliceTokens, tokenize, type Tok } from "../lib/highlight";

export const FILES = [
  { name: "queue.test.ts", ts: true }, { name: "queue.ts", ts: true }, { name: "server.ts", ts: true, dot: true },
  { name: "package.json" }, { name: "README.md" }, { name: "tsconfig.json" },
] as const;

export function reducedMotion() {
  return typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/** Scales a fixed-size design to the width of its container. */
export function Fit({ w, h, children, className = "", max }: { w: number; h: number; children: ReactNode; className?: string; max?: number }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const apply = () => el.style.setProperty("--s", String(el.clientWidth / w));
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    return () => ro.disconnect();
  }, [w]);
  return (
    <div ref={ref} className={`fit ${className}`} style={{ aspectRatio: `${w} / ${h}`, maxWidth: max } as CSSProperties}>
      <div className="fit-in" style={{ width: w, height: h }}>{children}</div>
    </div>
  );
}

/** Runs an async script in a loop while `active`; the script gets a cancellable sleep. */
export function useLoop(active: boolean, run: (sleep: (ms: number) => Promise<void>) => Promise<void>, deps: unknown[] = []) {
  useEffect(() => {
    if (!active || reducedMotion()) return;
    let dead = false;
    const timers: number[] = [];
    const sleep = (ms: number) => new Promise<void>((res, rej) => {
      timers.push(window.setTimeout(() => (dead ? rej(new Error("stop")) : res()), ms));
    });
    (async () => {
      try { while (!dead) await run(sleep); } catch { /* cancelled */ }
    })();
    return () => { dead = true; timers.forEach(clearTimeout); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, ...deps]);
}

export function useInView<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [seen, setSeen] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver(([e]) => setSeen(!!e?.isIntersecting), { threshold: 0.25 });
    io.observe(el);
    return () => io.disconnect();
  }, []);
  return [ref, seen] as const;
}

export function Tokens({ toks, errorFrom, errorTo }: { toks: Tok[]; errorFrom?: number; errorTo?: number }) {
  if (errorFrom === undefined || errorTo === undefined) {
    return <>{toks.map(([c, t], i) => <span key={i} className={c ? `tk-${c}` : undefined}>{t}</span>)}</>;
  }
  // split tokens so the [errorFrom, errorTo) range gets a squiggle
  const out: ReactNode[] = [];
  let pos = 0;
  toks.forEach(([c, t], i) => {
    const s = pos, e = pos + t.length;
    pos = e;
    const a = Math.max(s, errorFrom), b = Math.min(e, errorTo);
    const cls = c ? `tk-${c}` : undefined;
    if (a >= b) { out.push(<span key={i} className={cls}>{t}</span>); return; }
    if (a > s) out.push(<span key={`${i}a`} className={cls}>{t.slice(0, a - s)}</span>);
    out.push(<span key={`${i}b`} className={`${cls ?? ""} squiggle`}>{t.slice(a - s, b - s)}</span>);
    if (b < e) out.push(<span key={`${i}c`} className={cls}>{t.slice(b - s)}</span>);
  });
  return <>{out}</>;
}

export function FileIcon({ name }: { name: string }) {
  const ts = name.endsWith(".ts");
  const md = name.endsWith(".md");
  return (
    <span aria-hidden className="grid size-[14px] shrink-0 place-items-center rounded-[3px] text-[7.5px] font-bold leading-none"
      style={{ background: ts ? "#4b8bbe" : "transparent", color: ts ? "#06121d" : "#8a8880", border: ts ? "none" : "1px solid #3a3a38" }}>
      {ts ? "TS" : md ? "M" : "{}"}
    </span>
  );
}

export function Sidebar({ active, onPick, width = 200 }: { active: string; onPick?: (n: string) => void; width?: number }) {
  return (
    <div className="shrink-0 border-r border-[var(--i-line)] bg-[var(--i-side)] p-2" style={{ width }}>
      <div className="px-2 pb-2 pt-1 text-[11px] font-medium text-[var(--i-dim)]">Files</div>
      <div className="i-row text-[var(--i-fg)]"><span aria-hidden className="text-[var(--i-dim)]">▾</span> src</div>
      <div className="pl-3">
        {FILES.slice(0, 3).map((f) => (
          <div key={f.name} className={`i-row ${active === f.name ? "on" : ""}`} onClick={() => onPick?.(f.name)} role={onPick ? "button" : undefined}>
            <FileIcon name={f.name} /><span className="flex-1 truncate">{f.name}</span>
            {"dot" in f && f.dot ? <i className="size-1.5 rounded-full bg-[#6394bf]" /> : null}
          </div>
        ))}
      </div>
      {FILES.slice(3).map((f) => (
        <div key={f.name} className={`i-row ${active === f.name ? "on" : ""}`}><FileIcon name={f.name} /><span className="truncate">{f.name}</span></div>
      ))}
    </div>
  );
}

export function TitleBar({ branch = "main", project = "orbit-api" }: { branch?: string; project?: string }) {
  return (
    <div className="flex h-9 items-center gap-5 border-b border-[var(--i-line)] px-4 text-[12.5px] text-[var(--i-fg)]">
      <span className="flex items-center gap-1.5 font-medium"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>{project}<span className="text-[var(--i-dim)]">⌄</span></span>
      <span className="flex items-center gap-1.5 text-[var(--i-dim)]"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden><circle cx="6" cy="5" r="2" /><circle cx="6" cy="19" r="2" /><circle cx="18" cy="9" r="2" /><path d="M6 7v10M18 11c0 4-6 3-12 6" /></svg>{branch}<span>⌄</span></span>
    </div>
  );
}

export function TabBar({ tabs, active }: { tabs: string[]; active: string }) {
  return (
    <div className="flex h-9 items-end gap-1 border-b border-[var(--i-line)] px-2 pb-0">
      {tabs.map((t) => (
        <div key={t} className={`mb-1 flex h-7 items-center gap-2 rounded-lg px-3 text-[12px] ${t === active ? "bg-[var(--i-hover)] font-medium text-[var(--i-fg)]" : "text-[var(--i-dim)]"}`}>
          <FileIcon name={t} />{t}{t === active ? <span className="text-[var(--i-dim)]">×</span> : null}
        </div>
      ))}
    </div>
  );
}

export function StatusBar({ errors = 0, warnings = 0, msg, ram = "RAM 37 MB + tsgo 46 MB", extra }: { errors?: number; warnings?: number; msg?: string; ram?: string; extra?: ReactNode }) {
  return (
    <div className="flex h-7 items-center justify-between border-t border-[var(--i-line)] px-3 text-[11.5px] text-[#9c9a92]">
      <div className="flex items-center gap-4">
        <span className={errors ? "text-[#cb7676]" : ""}>⊗ {errors}</span><span>△ {warnings}</span><span>{msg ?? "tsgo ready"}</span>{extra}
      </div>
      <div className="flex items-center gap-4"><span>Ln 15, Col 35</span><span>Spaces: 2</span><span>typescript</span><span>{ram}</span></div>
    </div>
  );
}

export function fuzzy(query: string, text: string): ReactNode[] | null {
  const q = query.toLowerCase();
  const out: ReactNode[] = [];
  let qi = 0;
  for (let i = 0; i < text.length; i++) {
    const hit = qi < q.length && text[i]!.toLowerCase() === q[qi];
    if (hit) qi++;
    out.push(hit ? <mark key={i} className="bg-transparent text-[#e9b27e]">{text[i]}</mark> : text[i]);
  }
  return qi === q.length ? out : null;
}

export function PaletteList({ query, items, sel = 0, placeholder = "Search files by name", width = 480 }: { query: string; items: { label: string; hint?: string; icon?: boolean }[]; sel?: number; placeholder?: string; width?: number }) {
  const rows = items.map((it) => ({ it, m: fuzzy(query, it.label) })).filter((r) => r.m);
  return (
    <div className="i-pop ide-pop overflow-hidden" style={{ width }} role="dialog" aria-label="Command palette">
      <div className="flex h-10 items-center gap-2 border-b border-[var(--i-line)] px-3 text-[13px]">
        <span className="text-[var(--i-dim)]" aria-hidden>⌕</span>
        {query ? <span>{query}<span className="caret" /></span> : <span className="text-[var(--i-faint)]">{placeholder}<span className="caret" /></span>}
      </div>
      <div className="p-1.5">
        {rows.slice(0, 6).map(({ it, m }, i) => (
          <div key={it.label} className={`i-row ${i === Math.min(sel, rows.length - 1) ? "on" : ""}`}>
            {it.icon !== false && <FileIcon name={it.label} />}
            <span className="flex-1 truncate">{m}</span>{it.hint && <span className="i-kbd">{it.hint}</span>}
          </div>
        ))}
        {rows.length === 0 && <div className="px-3 py-3 text-[var(--i-dim)]">No results</div>}
      </div>
    </div>
  );
}

export function useTokens(code: string) { return tokenize(code); }
export { sliceTokens, tokenize };
