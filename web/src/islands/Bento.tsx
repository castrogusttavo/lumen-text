import { useState, type ReactNode } from "react";
import facts from "../data/facts.json";
import themes from "../data/themes.json";
import { FileIcon, PaletteList, tokenize, useInView, useLoop, type PalItem } from "./ide";

const Card = ({ children, className = "" }: { children: ReactNode; className?: string }) => <div className={`ide not-prose ${className}`}>{children}</div>;

/* 1 · Command palette */
const CMDS: PalItem[] = [
  { label: "View: Split editor right", hint: "Ctrl+\\" }, { label: "View: Split editor down", hint: "Ctrl+K \\" }, { label: "Git: Open panel", hint: "Ctrl+Shift+G" },
  { label: "Code: Format document", hint: "Shift+Alt+F" }, { label: "Appearance: Choose theme…" }, { label: "Markdown: Open reader beside", hint: "Ctrl+Shift+V" },
  { label: "Preferences: Settings", hint: "Ctrl+," }, { label: "View: Toggle word wrap" },
];
export function PaletteMini() {
  const [ref, seen] = useInView<HTMLDivElement>();
  const [q, setQ] = useState("");
  useLoop(seen, async (sleep) => {
    setQ(""); await sleep(900);
    for (const w of ["t", "th", "the", "them"]) { setQ(w); await sleep(260); }
    await sleep(1700); setQ(""); await sleep(300);
    for (const w of ["s", "sp", "spl"]) { setQ(w); await sleep(260); }
    await sleep(1900);
  });
  return (
    <div ref={ref} className="grid h-full grid-cols-[minmax(0,1fr)] place-items-center p-4">
      <PaletteList query={q} items={CMDS} title="Commands" width={420} />
    </div>
  );
}

/* 2 · Split panes */
const LAYOUTS = [{ n: 1, cols: 1 }, { n: 2, cols: 2 }, { n: 3, cols: 3 }, { n: 4, cols: 4 }, { n: 6, cols: 3 }];
export function SplitMini() {
  const [n, setN] = useState(2);
  const l = LAYOUTS.find((x) => x.n === n)!;
  const names = ["queue.ts", "server.ts", "queue.test.ts", "README.md", "package.json", "tsconfig.json"];
  return (
    <div className="grid h-full grid-rows-[auto_1fr] gap-3 p-4">
      <div className="flex items-center justify-between">
        <span className="text-[11px] text-[var(--i-dim)]">Up to six panes</span>
        <div className="flex gap-1" role="group" aria-label="Pane layout">
          {LAYOUTS.map((x) => <button key={x.n} onClick={() => setN(x.n)} aria-pressed={x.n === n} className={`h-7 w-7 rounded-md border text-[12px] transition-colors ${x.n === n ? "border-[#6394bf] bg-[#16222c] text-white" : "border-[var(--i-line)] text-[var(--i-dim)] hover:text-white"}`}>{x.n}</button>)}
        </div>
      </div>
      <div className="grid gap-1.5" style={{ gridTemplateColumns: `repeat(${l.cols}, 1fr)` }}>
        {names.slice(0, n).map((f, i) => (
          <div key={f} className={`ide-pop flex min-h-[64px] flex-col rounded-lg border bg-[#0a0a0a] p-2 ${i === 0 ? "border-[#6394bf]/60" : "border-[var(--i-line)]"}`}>
            <div className="mb-1.5 flex items-center gap-1.5 text-[10.5px] text-[var(--i-dim)]"><FileIcon name={f} />{f}</div>
            {[78, 52, 66, 40].map((w, k) => <i key={k} className="mb-1 block h-[5px] rounded-full bg-[#1e1e1e]" style={{ width: `${w - i * 4}%` }} />)}
          </div>
        ))}
      </div>
    </div>
  );
}

/* 3 · Theme picker with live preview */
const PICK = ["vitesse-black", "github-dark-default", "tokyo-night", "catppuccin-mocha", "dracula", "nord", "rose-pine-dawn", "github-light-default"];
const list = PICK.map((id) => themes.find((t) => t.id === id)!).filter(Boolean);
const SNIP = `export class JobQueue<T> {
  private jobs: Job<T>[] = [];
  // newest first
  push(item: T, retries = 3) {
    return this.jobs.unshift({ item, retries });
  }
}`;
export function ThemeMini() {
  const [ref, seen] = useInView<HTMLDivElement>();
  const [i, setI] = useState(0);
  const [touched, setTouched] = useState(false);
  useLoop(seen && !touched, async (sleep) => { await sleep(1700); setI((v) => (v + 1) % list.length); });
  const t = list[i]!;
  const toks = SNIP.split("\n").map((l) => tokenize(l));
  const col = (c: string) => ({ k: t.keyword, st: t.keyword, s: t.string, f: t.fn, t: t.type, n: t.number, p: t.prop, v: t.fg, c: t.comment, d: t.fg, "": t.fg }[c] ?? t.fg);
  return (
    <div ref={ref} className="grid h-full grid-cols-[156px_1fr] gap-3 p-4">
      <div className="space-y-0.5 text-[12px]" role="listbox" aria-label="Choose theme (arrow keys to preview)">
        {list.map((x, k) => (
          <div key={x.id} role="none">
            <button role="option" aria-selected={k === i} onMouseEnter={() => { setTouched(true); setI(k); }} onFocus={() => { setTouched(true); setI(k); }} onClick={() => { setTouched(true); setI(k); }}
              className={`i-row w-full text-left ${k === i ? "on" : ""}`}><i className="size-2.5 shrink-0 rounded-full ring-1 ring-white/20" style={{ background: x.bg }} /><span className="truncate">{x.name}</span></button>
          </div>
        ))}
        <div className="px-2.5 pt-1.5 text-[11px] text-[var(--i-dim)]">+ {facts.themes - list.length} more · Zed import</div>
      </div>
      <div className="code min-w-0 overflow-x-auto rounded-lg border border-white/10 p-3 transition-colors duration-300" style={{ background: t.bg, color: t.fg, fontSize: 11.5 }}>
        {toks.map((row, r) => (
          <div key={r} className="flex"><span className="w-6 shrink-0 pr-1 opacity-40">{r + 1}</span><span>{row.map(([c, s], k) => <span key={k} style={{ color: col(c) }}>{s}</span>)}</span></div>
        ))}
      </div>
    </div>
  );
}

/* 4 · Git */
export function GitMini() {
  const [staged, setStaged] = useState<Record<string, boolean>>({ "queue.ts": true });
  const files = [{ f: "queue.ts", d: "src" }, { f: "server.ts", d: "src" }, { f: "README.md", d: "" }];
  const row = ({ f, d }: { f: string; d: string }) => (
    <button key={f} onClick={() => setStaged((s) => ({ ...s, [f]: !s[f] }))} aria-pressed={!!staged[f]} aria-label={`${staged[f] ? "Unstage" : "Stage"} ${f}`} className="i-row w-full text-left hover:bg-[#151515]">
      <span className={`grid size-4 place-items-center rounded border text-[10px] ${staged[f] ? "border-[#6394bf] bg-[#6394bf] text-black" : "border-[#4a4a4a]"}`}>{staged[f] ? "✓" : ""}</span>
      <FileIcon name={f} /><span>{f}</span><span className="text-[11px] text-[var(--i-dim)]">{d}</span><span className="ml-auto font-semibold text-[#6394bf]">M</span>
    </button>
  );
  const st = files.filter((x) => staged[x.f]), un = files.filter((x) => !staged[x.f]);
  const sec = (l: string, n: number, a: string) => <div className="flex items-center justify-between px-2 pb-0.5 pt-2 text-[12px]"><span><b className="font-semibold">{l}</b> <span className="text-[var(--i-dim)]">{n}</span></span><span className="text-[var(--i-dim)]">{a}</span></div>;
  return (
    <div className="flex h-full flex-col p-4 font-sans text-[12.5px]">
      <div className="flex items-center justify-between text-[12px] text-[var(--i-dim)]"><span>⑂ main</span><span>↓ Pull &nbsp; ↑ Push</span></div>
      {st.length > 0 && <>{sec("Staged", st.length, "Remove all")}{st.map(row)}</>}
      {sec("Unstaged", un.length, "Add all")}{un.map(row)}
      <div className="mt-auto flex items-center justify-between pt-3"><span className="text-[11px] text-[var(--i-dim)]">Commit message (Ctrl+Enter to commit)</span><span className="grid h-8 w-20 place-items-center rounded-lg bg-[#1d1d1d] font-semibold">Commit</span></div>
    </div>
  );
}

/* 5 · Inlay hints + hover */
export function HoverMini() {
  const [hints, setHints] = useState(true);
  const [hover, setHover] = useState(true);
  const Hint = ({ t }: { t: string }) => hints ? <span className="mx-1 rounded bg-[#1a1a1a] px-1 text-[10px] text-[var(--i-dim)]">{t}</span> : null;
  return (
    <div className="flex h-full flex-col gap-3 p-4">
      <label className="flex items-center justify-between text-[11px] text-[var(--i-dim)]">Inlay hints
        <button role="switch" aria-checked={hints} onClick={() => setHints(!hints)} className={`h-[18px] w-8 rounded-full p-0.5 transition-colors ${hints ? "bg-[#6394bf]" : "bg-[#2a2a2a]"}`}><i className={`block size-[14px] rounded-full bg-white transition-transform ${hints ? "translate-x-[14px]" : ""}`} /></button>
      </label>
      <div className="code overflow-x-auto rounded-lg border border-[var(--i-line)] bg-[#0a0a0a] p-3">
        <div><span className="tk-st">const</span> total<Hint t=": number" /> <span className="tk-d">=</span> <span className="tk-v">sum</span><span className="tk-d">(</span><Hint t="a:" /><span className="tk-n">2</span><span className="tk-d">,</span> <Hint t="b:" /><span className="tk-n">3</span><span className="tk-d">);</span></div>
        <div>
          <span className="tk-st">const</span>{" "}
          <button onMouseEnter={() => setHover(true)} onMouseLeave={() => setHover(false)} onFocus={() => setHover(true)} onBlur={() => setHover(false)} className="rounded px-0.5 text-[var(--i-fg)] underline decoration-dotted underline-offset-4 hover:bg-[#1c1c1c]">first</button>
          <Hint t=": Job<string>" /> <span className="tk-d">=</span> queue<span className="tk-d">.</span><span className="tk-f">peek</span><span className="tk-d">();</span>
        </div>
        {hover && <div className="i-pop ide-pop w-full max-w-[250px] p-2.5 font-sans text-[11.5px] leading-snug" style={{ whiteSpace: "normal" }}><code className="font-mono text-[#e9b27e]">const first: Job&lt;string&gt;</code><p className="mt-1 text-[var(--i-dim)]">Rest the mouse on a symbol, or press <span className="i-kbd">Ctrl+K I</span>.</p></div>}
      </div>
    </div>
  );
}

/* 6 · Settings */
export function SettingsMini() {
  const [s, setS] = useState({ minimap: true, wrap: false, sticky: true, format: false });
  const [upd, setUpd] = useState("auto");
  const rows: [keyof typeof s, string][] = [["format", "Format on save"], ["minimap", "Minimap"], ["wrap", "Word wrap"], ["sticky", "Sticky scroll"]];
  return (
    <div className="grid h-full grid-cols-[minmax(0,1fr)] place-items-center p-4">
      <div className="i-pop w-full max-w-[320px] p-3 font-sans text-[12.5px]">
        <div className="mb-1 flex items-center justify-between px-2 pb-2 pt-1 font-medium"><span>Settings</span><span className="i-kbd">Ctrl+,</span></div>
        {rows.map(([k, l]) => (
          <div key={k} className="flex h-9 items-center justify-between rounded-lg px-2 hover:bg-[#191919]">
            <span>{l}</span>
            <button role="switch" aria-checked={s[k]} aria-label={l} onClick={() => setS({ ...s, [k]: !s[k] })} className={`h-[18px] w-8 rounded-full p-0.5 transition-colors ${s[k] ? "bg-[#6394bf]" : "bg-[#2a2a2a]"}`}><i className={`block size-[14px] rounded-full bg-white transition-transform ${s[k] ? "translate-x-[14px]" : ""}`} /></button>
          </div>
        ))}
        <div className="flex h-9 items-center justify-between px-2"><span>Updates</span>
          <div className="flex rounded-lg bg-[#1a1a1a] p-0.5" role="radiogroup" aria-label="Updates">
            {["auto", "notify", "off"].map((o) => <button key={o} role="radio" aria-checked={upd === o} onClick={() => setUpd(o)} className={`rounded-md px-2.5 py-1 text-[11px] ${upd === o ? "bg-[#333] text-white" : "text-[var(--i-dim)]"}`}>{o}</button>)}
          </div>
        </div>
      </div>
    </div>
  );
}

export { Card };
