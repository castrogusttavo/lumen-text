import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Fit, FileIcon, PaletteList, type PalItem, Sidebar, StatusBar, TabBar, TitleBar, Tokens, tokenize, useInView, useLoop } from "./ide";

const W = 1000, H = 450;
/** On phones each demo shows a 480 px window of the design (sidebar hidden): where the action is. */
const CROP_X: Record<string, number> = { edit: 0, navigate: 0, git: 520, preview: 520 };
const SERVER_HEAD = `import { JobQueue } from "./queue";

const queue = new JobQueue<{ url: string }>();

export async function start(port: number) {`;
const SERVER_LINE = `  queue.push({ url: "https://example.com" }, "`;
const QUEUE_LINES = `export class JobQueue<T> {
  private jobs: Job<T>[] = [];

  push(payload: T, priority: Priority = "normal"): Job<T> {
    const job = { id: randomUUID(), payload, attempts: 0 };
    this.jobs.push(job);
    return job;
  }
}`.split("\n");

function Lines({ lines, start = 1, hi }: { lines: string[]; start?: number; hi?: number }) {
  return (
    <>
      {lines.map((l, i) => (
        <div key={i} className={`flex ${hi === start + i ? "bg-[#1a1a1a]" : ""}`}>
          <span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{start + i}</span><span><Tokens toks={tokenize(l)} /></span>
        </div>
      ))}
    </>
  );
}

function Frame({ children, active, tabs, tab, status, side = true }: { children: ReactNode; active: string; tabs: string[]; tab: string; status?: ReactNode; side?: boolean }) {
  return (
    <div className="ide relative flex h-full flex-col shadow-[0_30px_80px_-20px_rgb(0_0_0/0.7)]">
      <TitleBar />
      <div className="flex min-h-0 flex-1">
        {side && <Sidebar active={active} width={190} />}
        <div className="relative flex min-w-0 flex-1 flex-col"><TabBar tabs={tabs} active={tab} />{children}</div>
      </div>
      {status ?? <StatusBar />}
    </div>
  );
}

function Key({ k }: { k: string | null }) {
  return k ? <div className="ide-pop absolute bottom-3 right-4 z-20 flex items-center gap-1.5 rounded-lg border border-[var(--i-line)] bg-[#141414]/90 px-2.5 py-1.5 text-[11px] text-[var(--i-dim)]">{k.split(" ").map((x) => <span key={x} className="i-kbd">{x}</span>)}</div> : null;
}
const FILES: PalItem[] = [
  { dir: "src", label: "server.ts" }, { dir: "src", label: "queue.ts" }, { dir: "src", label: "queue.test.ts" },
  { label: "tsconfig.json" }, { label: "README.md" }, { label: "package.json" },
];

/* ---------- Edit ---------- */
function Edit({ on }: { on: boolean }) {
  const [n, setN] = useState(SERVER_LINE.length);
  const [menu, setMenu] = useState(false);
  const [sel, setSel] = useState(0);
  const [done, setDone] = useState(true);
  const [key, setKey] = useState<string | null>(null);
  useLoop(on, async (sleep) => {
    setDone(false); setMenu(false); setN(0); setKey(null); await sleep(500);
    for (let i = 1; i <= SERVER_LINE.length; i++) { setN(i); await sleep(26); }
    setKey("Ctrl+Space"); setMenu(true); setSel(0); await sleep(900); setSel(1); await sleep(450); setSel(2); await sleep(450); setSel(0); await sleep(600);
    setKey("Enter"); setMenu(false); setDone(true); await sleep(900); setKey(null); await sleep(2400);
  }, []);
  const items = ['"high"', '"low"', '"normal"'];
  const line = SERVER_LINE.slice(0, n) + (done ? 'high");' : "");
  return (
    <Frame active="server.ts" tabs={["server.ts"]} tab="server.ts" status={<StatusBar pos="Ln 6, Col 48" />}>
      <div className="px-4 pt-1.5 text-[11.5px] text-[var(--i-dim)]">src &gt; server.ts &gt; <span className="text-[var(--i-fg)]">start</span></div>
      <div className="code relative flex-1 px-2 pt-1">
        <Lines lines={SERVER_HEAD.split("\n")} />
        <div className="flex"><span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">6</span><span><Tokens toks={tokenize(line)} />{!done && <span className="caret" />}</span></div>
        {done && <div className="flex"><span className="w-9 pr-4 text-right text-[var(--i-faint)]">7</span><span className="ide-pop"><Tokens toks={tokenize("  return { port, oldest: queue.oldest };")} /></span></div>}
        {done && <div className="flex"><span className="w-9 pr-4 text-right text-[var(--i-faint)]">8</span><span className="tk-d">{"}"}</span></div>}
        {menu && (
          <div className="i-pop ide-pop absolute left-[330px] top-[138px] max-sm:left-[250px] w-[170px] p-1.5 font-sans" style={{ whiteSpace: "normal" }}>
            {items.map((it, i) => <div key={it} className={`i-row ${i === sel ? "on" : ""}`}><span className="font-mono text-[12px]">{it}</span></div>)}
          </div>
        )}
        <Key k={key} />
      </div>
    </Frame>
  );
}

/* ---------- Navigate ---------- */
function Navigate({ on }: { on: boolean }) {
  const [pal, setPal] = useState<string | null>(null);
  const [file, setFile] = useState("queue.ts");
  const [open2, setOpen2] = useState(false);
  const [hi, setHi] = useState<number | undefined>();
  const [key, setKey] = useState<string | null>(null);
  useLoop(on, async (sleep) => {
    setFile("queue.ts"); setOpen2(false); setPal(null); setHi(undefined); setKey(null); await sleep(1100);
    setKey("Ctrl+P"); setPal(""); await sleep(600);
    for (const q of ["s", "sr", "srv"]) { setPal(q); await sleep(300); }
    await sleep(600); setKey("Enter"); setPal(null); setFile("server.ts"); setOpen2(true); await sleep(1500);
    setKey("F12"); setFile("queue.ts"); setHi(6); await sleep(2600); setKey(null);
  }, []);
  const isServer = file === "server.ts";
  return (
    <Frame active={file} tabs={open2 ? ["queue.ts", "server.ts"] : ["queue.ts"]} tab={file} status={<StatusBar pos={isServer ? "Ln 1, Col 1" : hi ? "Ln 6, Col 14" : "Ln 1, Col 1"} />}>
      <div className="px-4 pt-1.5 text-[11.5px] text-[var(--i-dim)]">src &gt; {file}</div>
      <div className="code relative flex-1 px-2 pt-1">
        {isServer ? <Lines lines={[...SERVER_HEAD.split("\n"), '  queue.push({ url: "https://example.com" }, "high");', "  return { port, oldest: queue.oldest };", "}"]} /> : <Lines lines={QUEUE_LINES} start={6} hi={hi} />}
        {pal !== null && <div className="absolute inset-x-4 top-0 flex justify-center"><PaletteList query={pal} items={FILES} title="Open file" /></div>}
        <Key k={key} />
      </div>
    </Frame>
  );
}

/* ---------- Git ---------- */
function Git({ on }: { on: boolean }) {
  const [staged, setStaged] = useState<string[]>([]);
  const [committed, setCommitted] = useState(false);
  const [msg, setMsg] = useState("");
  const [key, setKey] = useState<string | null>(null);
  const text = "Retry failed jobs up to three times";
  useLoop(on, async (sleep) => {
    setStaged([]); setCommitted(false); setMsg(""); setKey(null); await sleep(1300);
    setStaged(["server.ts"]); await sleep(900); setStaged(["server.ts", "queue.ts"]); await sleep(800);
    for (let i = 1; i <= text.length; i++) { setMsg(text.slice(0, i)); await sleep(32); }
    await sleep(500); setKey("Ctrl+Enter"); await sleep(500); setCommitted(true); setStaged([]); setMsg(""); await sleep(2800); setKey(null);
  }, []);
  const all = ["server.ts", "queue.ts"];
  const unstaged = committed ? [] : all.filter((f) => !staged.includes(f));
  const row = (f: string, st: boolean) => (
    <div key={f} className="i-row"><span className={`grid size-4 place-items-center rounded border text-[10px] ${st ? "border-[#6394bf] bg-[#6394bf] text-black" : "border-[#4a4a4a]"}`}>{st ? "✓" : ""}</span><FileIcon name={f} /><span>{f}</span><span className="text-[11px] text-[var(--i-dim)]">src</span><span className="ml-auto font-semibold text-[#6394bf]">M</span></div>
  );
  const sec = (label: string, n: number, act: string) => <div className="flex items-center justify-between px-2 pb-1 pt-3 text-[12px]"><span><b className="font-semibold">{label}</b> <span className="text-[var(--i-dim)]">{n}</span></span><span className="text-[var(--i-dim)]">{act}</span></div>;
  return (
    <Frame active="server.ts" tabs={["server.ts"]} tab="server.ts" status={<StatusBar pos="Ln 6, Col 1" />}>
      <div className="flex min-h-0 flex-1">
        <div className="code relative min-w-0 flex-1 px-2 pt-1">
          <div className="mb-1 px-2 font-sans text-[11.5px] text-[var(--i-dim)]">src &gt; server.ts</div>
          <Lines lines={[...SERVER_HEAD.split("\n"), '  queue.push({ url: "https://example.com" }, "high");', "  return { port, oldest: queue.oldest };", "}"]} />
          <i className="absolute left-0 top-[100px] h-5 w-[3px] bg-[#4d9375]" />
        </div>
        <div className="w-[300px] shrink-0 overflow-hidden border-l border-[var(--i-line)] bg-[var(--i-side)] p-3 font-sans">
          <div className="flex items-center justify-between text-[12.5px]"><span className="rounded-lg bg-[#1a1a1a] px-3 py-1.5 font-medium">Changes <span className="text-[var(--i-dim)]">{unstaged.length + staged.length}</span></span><span className="mr-auto pl-4 text-[var(--i-dim)]">History</span><span className="text-[var(--i-dim)]">⟳ ✕</span></div>
          <div className="mt-3 flex items-center justify-between px-2 text-[12px] text-[var(--i-dim)]"><span>⑂ main</span><span>↓ Pull &nbsp; ↑ Push</span></div>
          {staged.length > 0 && <>{sec("Staged", staged.length, "Remove all")}{staged.map((f) => row(f, true))}</>}
          {sec("Unstaged", unstaged.length, "Add all")}{unstaged.map((f) => row(f, false))}
          <div className="mt-2 h-[44px] rounded-xl border border-[var(--i-line)] bg-[#0b0b0b] p-2.5 text-[12px]">{msg ? <span>{msg}<span className="caret" /></span> : <span className="text-[var(--i-dim)]">Commit message (Ctrl+Enter to commit)</span>}</div>
          <div className="mt-2 flex items-center gap-2 px-1 text-[12px] text-[var(--i-dim)]"><span className="size-3.5 rounded border border-[#4a4a4a]" />Amend last commit</div>
          <div className="mt-2 flex items-center justify-between px-1 text-[12px] text-[var(--i-dim)]"><span className="flex items-center gap-2"><span className="grid size-3.5 place-items-center rounded bg-[#555] text-[9px] text-white">✓</span>Include everything if nothing is staged</span></div>
          <div className="ml-auto mt-1.5 grid h-8 w-20 place-items-center rounded-lg bg-[#1d1d1d] text-[12.5px] font-semibold">Commit</div>
        </div>
      </div>
      <Key k={key} />
    </Frame>
  );
}

/* ---------- Preview ---------- */
const MD_BASE = ["# Orbit API", "", "A tiny **priority job queue** written in TypeScript.", "", "- Priorities: `low`, `normal`, `high`"];
const MD_NEW = "- Retries up to three attempts";
function inline(s: string): ReactNode[] {
  return s.split(/(\*\*[^*]+\*\*|`[^`]+`)/g).map((p, i) => p.startsWith("**") ? <strong key={i} className="font-semibold">{p.slice(2, -2)}</strong> : p.startsWith("`") ? <code key={i} className="rounded bg-[#1b1b1b] px-1.5 py-0.5 font-mono text-[12px]">{p.slice(1, -1)}</code> : p);
}
function Preview({ on }: { on: boolean }) {
  const [n, setN] = useState(MD_NEW.length);
  const [key, setKey] = useState<string | null>(null);
  useLoop(on, async (sleep) => {
    setN(0); setKey("Ctrl+Shift+V"); await sleep(1400); setKey(null);
    for (let i = 1; i <= MD_NEW.length; i++) { setN(i); await sleep(55); }
    await sleep(3200);
  }, []);
  const lines = [...MD_BASE, MD_NEW.slice(0, n)];
  const srcLine = (l: string) => l.startsWith("#") ? <span className="tk-k font-semibold">{l}</span> : l.startsWith("-") ? <><span className="text-[#d4976c]">-</span>{l.slice(1)}</> : l;
  return (
    <Frame active="README.md" tabs={["queue.ts", "README.md"]} tab="README.md" status={<StatusBar pos="Ln 1, Col 1" lang="markdown" />}>
      <div className="px-4 pt-1.5 text-[11.5px] text-[var(--i-fg)]">README.md</div>
      <div className="flex min-h-0 flex-1">
        <div className="code flex-1 border-r border-[var(--i-line)] px-2 pt-1">
          {lines.map((l, i) => <div key={i} className="flex"><span className="w-9 pr-4 text-right text-[var(--i-faint)]">{i + 1}</span><span>{srcLine(l)}{i === lines.length - 1 && n < MD_NEW.length && <span className="caret" />}</span></div>)}
        </div>
        <div className="relative flex-1 overflow-hidden p-7 font-sans text-[14px] leading-relaxed">
          <h3 className="border-b border-[var(--i-line)] pb-2 text-[28px] font-semibold tracking-tight">Orbit API</h3>
          <p className="mt-4 text-[#c9c6bb]">{inline(MD_BASE[2]!)}</p>
          <ul className="mt-3 list-disc space-y-1.5 pl-5 text-[#c9c6bb]">
            <li>{inline(MD_BASE[4]!.slice(2))}</li>
            {n > 0 && <li>{MD_NEW.slice(2, n)}</li>}
          </ul>
        </div>
      </div>
      <Key k={key} />
    </Frame>
  );
}

const TABS = [
  { id: "edit", label: "Edit", C: Edit, text: "Completion, diagnostics and hover come from the TypeScript language server, tsgo." },
  { id: "navigate", label: "Navigate", C: Navigate, text: "Open files with Ctrl+P, jump to a definition with F12, go back with Alt+Left." },
  { id: "git", label: "Git", C: Git, text: "Stage files or single hunks, commit with Ctrl+Enter, pull and push from the Git panel (Ctrl+Shift+G)." },
  { id: "preview", label: "Preview", C: Preview, text: "The Markdown reader (Ctrl+Shift+V) updates as you type and scrolls in sync with the source." },
];

export default function DemoTabs() {
  const [ref, seen] = useInView<HTMLDivElement>();
  const [tab, setTab] = useState(0);
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent) => {
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const next = e.key === "Home" ? 0 : e.key === "End" ? TABS.length - 1 : (tab + d + TABS.length) % TABS.length;
    setTab(next); refs.current[next]?.focus();
  };
  return (
    <div ref={ref}>
      <div role="tabpanel" id={`demo-panel-${TABS[tab]!.id}`} aria-labelledby={`demo-tab-${TABS[tab]!.id}`} tabIndex={0}>
        {TABS.map(({ id, C }, i) => (
          <div key={id} hidden={i !== tab}><Fit w={W} h={H} max={1040} crop={{ x: CROP_X[id] ?? 0, w: 480 }}><C on={seen && i === tab} /></Fit></div>
        ))}
      </div>
      <p className="mt-8 text-center text-sm text-[#2c2c33]" aria-live="polite">{TABS[tab]!.text}</p>
      <div role="tablist" aria-label="Lumen workflow" onKeyDown={onKey} className="mx-auto mt-5 flex w-fit max-w-full gap-0.5 rounded-full border border-black/10 bg-white/75 p-1.5 shadow-lg backdrop-blur">
        {TABS.map((t, i) => (
          <button key={t.id} ref={(el) => { refs.current[i] = el; }} role="tab" id={`demo-tab-${t.id}`} aria-selected={i === tab} aria-controls={`demo-panel-${t.id}`} tabIndex={i === tab ? 0 : -1}
            onClick={() => setTab(i)}
            className={`rounded-full px-3 py-2 text-[13px] font-medium transition-colors sm:px-5 sm:text-sm ${i === tab ? "bg-[#111] text-white" : "text-[#333] hover:bg-black/5"}`}>{t.label}</button>
        ))}
      </div>
    </div>
  );
}
