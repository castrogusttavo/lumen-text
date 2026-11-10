import { useState } from "react";
import { statusRam } from "../lib/ram";
import { Fit, PaletteList, Sidebar, StatusBar, TabBar, TitleBar, Tokens, tokenize, useInView, useLoop, type PalItem } from "./ide";

const HEAD = `import { randomUUID } from "node:crypto";

export type Priority = "low" | "normal" | "high";
export interface Job<T> { id: string; payload: T; attempts: number }

export class JobQueue<T> {
  private jobs: Job<T>[] = [];

  push(payload: T, priority: Priority = "normal"): Job<T> {
    const job: Job<T> = { id: randomUUID(), payload, attempts: 0 };
    this.jobs.push(job);
    return job;
  }
`.split("\n").slice(0, -1);
const TYPED = `  get oldest(): number {
    return this.jobs[0]?.attempts;
  }`;
const FIX = " ?? 0";
const SERVER = `import { JobQueue } from "./queue";

const queue = new JobQueue<{ url: string }>();

export async function start(port: number) {
  queue.push({ url: "https://example.com" }, "high");
  return { port, oldest: queue.oldest };
}`.split("\n");
const FILES: PalItem[] = [
  { dir: "src", label: "queue.ts" }, { dir: "src", label: "queue.test.ts" }, { dir: "src", label: "server.ts" },
  { label: "tsconfig.json" }, { label: "README.md" }, { label: "package.json" },
];
const MSG = "Type 'number | undefined' is not assignable to type 'number'.";
// Phone composition: the same story on a narrow excerpt of the same files (no sidebar), so type stays readable.
const HEAD_M = `export class JobQueue<T> {
  private jobs: Job<T>[] = [];

  push(payload: T): Job<T> {
    const job = createJob(payload);
    this.jobs.push(job);
    return job;
  }

`.split("\n").slice(0, -1);
const SERVER_M = `import { JobQueue } from "./queue";
const queue = new JobQueue<string>();

export function start(port: number) {
  queue.push("https://example.com");
  return { oldest: queue.oldest };
}`.split("\n");
const headM = HEAD_M.map((l) => tokenize(l));
const serverM = SERVER_M.map((l) => tokenize(l));
const STEPS = [
  { keys: null, text: "tsgo flags a type error" },
  { keys: null, text: "Fix it and the error clears" },
  { keys: "Ctrl+P", text: "Quick open server.ts" },
  { keys: "Ctrl+W", text: "Close it, back to queue.ts" },
] as const;
const headLines = HEAD.map((l) => tokenize(l));
const serverLines = SERVER.map((l) => tokenize(l));

export default function HeroIde() {
  const [ref, seen] = useInView<HTMLDivElement>();
  // SSR/initial state = where every loop ends: queue.ts open and active, the error showing.
  const [typed, setTyped] = useState(TYPED.length);
  const [fix, setFix] = useState(0);
  const [diag, setDiag] = useState(true);
  const [hover, setHover] = useState(true);
  const [pal, setPal] = useState<string | null>(null);
  const [serverOpen, setServerOpen] = useState(false);
  const [view, setView] = useState<"queue" | "server">("queue");
  const [step, setStep] = useState(0);

  useLoop(seen, async (sleep) => {
    const type = async (n: number, set: (v: number) => void, ms: number) => { for (let i = 1; i <= n; i++) { set(i); await sleep(ms); } };
    await sleep(2200);                                   // 1) read the error, then fix it
    setHover(false); setStep(1); await sleep(500);
    await type(FIX.length, setFix, 120);
    setDiag(false); await sleep(1500);
    setPal(""); setStep(2); await sleep(700);                        // 2) Ctrl+P, type, Enter: server.ts opens
    for (const q of ["s", "se", "ser"]) { setPal(q); await sleep(300); }
    await sleep(700); setPal(null); setServerOpen(true); setView("server"); await sleep(2200);
    setServerOpen(false); setView("queue"); setStep(3); await sleep(1600); // 3) Ctrl+W closes it, queue.ts is active again
    setTyped(0); setFix(0); setStep(0); await sleep(700);            // 4) start over: type the getter, the error comes back
    await type(TYPED.length, setTyped, 34);
    await sleep(700); setDiag(true); await sleep(900); setHover(true);
  });

  const typedLines = TYPED.slice(0, typed).split("\n");
  const lineNo0 = headLines.length + 1;
  const errLine = lineNo0 + 1;
  const total = Math.max(typedLines.length, 1);
  const isServer = view === "server";
  const fixed = FIX.slice(0, fix);
  const errors = diag && fix === 0 && typed === TYPED.length ? 1 : 0;

  const errM = HEAD_M.length + 2; // line number of `return this.jobs[0]?.attempts;` in the phone excerpt
  const gut = "w-6 select-none pr-2.5 text-right text-[var(--i-faint)]";
  const label = "Lumen editing a TypeScript file: tsgo reports a type error, it is fixed, quick open finds server.ts, and closing it returns to queue.ts";
  const st = STEPS[step]!;

  const phone = (
    <div className="pb-5 sm:hidden">
      <Fit w={344} h={410} max={480} className="fit-m mx-auto">
        <div className="ide relative flex h-full flex-col shadow-[0_30px_80px_-24px_rgb(0_0_0/0.9)]" role="img" aria-label={label}>
          <TitleBar />
          <TabBar tabs={serverOpen ? ["queue.ts", "server.ts"] : ["queue.ts"]} active={isServer ? "server.ts" : "queue.ts"} />
          <div className="px-3 pt-1.5 text-[11.5px] text-[var(--i-dim)]">src &gt; {isServer ? <>server.ts &gt; <span className="text-[var(--i-fg)]">start</span></> : <>queue.ts &gt; <span className="text-[var(--i-fg)]">JobQueue</span></>}</div>
          <div className="code relative flex-1 overflow-hidden px-1.5 pt-1" style={{ fontSize: 12.5 }}>
            {isServer ? serverM.map((toks, i) => (
              <div key={i} className="flex"><span className={gut}>{i + 1}</span><span><Tokens toks={toks} />{i === 0 && <span className="caret" />}</span></div>
            )) : (
              <>
                {headM.map((toks, i) => (
                  <div key={i} className="flex"><span className={gut}>{i + 1}</span><span><Tokens toks={toks} /></span></div>
                ))}
                {Array.from({ length: total }).map((_, i) => {
                  const text = typedLines[i] ?? "";
                  const isErr = HEAD_M.length + 1 + i === errM;
                  const base = tokenize(isErr ? text.replace(/;$/, fixed + ";") : text);
                  const eFrom = isErr ? text.indexOf("this") : undefined;
                  const show = isErr && diag && fix === 0 && eFrom !== undefined && eFrom >= 0 && text.endsWith(";");
                  const lastLine = i === typedLines.length - 1;
                  return (
                    <div key={i} className="relative flex">
                      <span className={gut}>{HEAD_M.length + 1 + i}</span>
                      {show && <i className="absolute -left-1 top-1.5 h-[9px] w-[3px] rounded-sm bg-[#cb7676]" />}
                      <span>{show ? <Tokens toks={base} errorFrom={eFrom} errorTo={text.length - 1} /> : <Tokens toks={base} />}{lastLine && typed > 0 && typed < TYPED.length ? <span className="caret" /> : null}</span>
                    </div>
                  );
                })}
                <div className="flex"><span className={gut}>{HEAD_M.length + 1 + total}</span><span className="tk-d">{"}"}</span></div>
                {diag && fix === 0 && hover && typed === TYPED.length && (
                  <div className="i-pop ide-pop absolute left-2 right-2 z-10 p-3 font-sans text-[12.5px] leading-snug text-[var(--i-fg)]" style={{ top: 4 + (errM - 1) * 20, transform: "translateY(calc(-100% - 4px))", whiteSpace: "normal" }}>
                    {MSG}<div className="mt-1 text-[11.5px] text-[var(--i-dim)]">ts(2322)</div>
                    <div className="mt-2 border-t border-[var(--i-line)] pt-2 text-[12px] text-[var(--i-dim)]">Quick actions (Ctrl+.)</div>
                  </div>
                )}
              </>
            )}
          </div>
          {pal !== null && <div className="absolute inset-x-3 top-[78px] z-20"><PaletteList query={pal} items={FILES} title="Open file" width={320} /></div>}
          <div className="flex h-8 items-center justify-between border-t border-[var(--i-line)] px-3 text-[12px] text-[#9c9a92]">
            <div className="flex items-center gap-3"><span className={!isServer && errors ? "text-[#cb7676]" : ""}>⊗ {isServer ? 0 : errors}</span><span>△ 0</span></div>
            <div className="flex items-center gap-3"><span>{isServer ? "Ln 1, Col 1" : `Ln ${errM}, Col 35`}</span>{statusRam ? <span>{statusRam}</span> : null}</div>
          </div>
        </div>
      </Fit>
      <div className="mt-4 flex h-6 items-center justify-center gap-2.5 text-[13.5px] text-muted" aria-hidden="true">
        {st.keys ? <kbd className="rounded-md border border-line2 bg-card px-1.5 py-0.5 font-mono text-[12px] text-fg">{st.keys}</kbd> : null}
        <span>{st.text}</span>
      </div>
      <div className="mt-2 flex justify-center gap-1.5" aria-hidden="true">
        {STEPS.map((_, i) => <i key={i} className={`h-1 rounded-full transition-all ${i === step ? "w-5 bg-accent" : "w-1.5 bg-line2"}`} />)}
      </div>
    </div>
  );

  return (
    <div ref={ref}>
      {phone}
      <div className="hidden sm:block">
      <Fit w={1000} h={500} max={1200}>
        <div className="ide relative flex h-full flex-col shadow-[0_40px_120px_-30px_rgb(0_0_0/0.9)]" role="img" aria-label={label}>
          <TitleBar />
          <div className="flex min-h-0 flex-1">
            <Sidebar active={isServer ? "server.ts" : "queue.ts"} />
            <div className="relative flex min-w-0 flex-1 flex-col">
              <TabBar tabs={serverOpen ? ["queue.ts", "server.ts"] : ["queue.ts"]} active={isServer ? "server.ts" : "queue.ts"} />
              <div className="px-4 pt-1.5 text-[11.5px] text-[var(--i-dim)]">src &gt; {isServer ? <>server.ts &gt; <span className="text-[var(--i-fg)]">start</span></> : <>queue.ts &gt; <span className="text-[var(--i-fg)]">JobQueue</span></>}</div>
              <div className="code relative flex-1 overflow-hidden px-2 pt-1">
                {isServer ? serverLines.map((toks, i) => (
                  <div key={i} className="flex"><span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{i + 1}</span><span><Tokens toks={toks} />{i === 0 && <span className="caret" />}</span></div>
                )) : (
                  <>
                    {headLines.map((toks, i) => (
                      <div key={i} className="flex"><span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{i + 1}</span><span><Tokens toks={toks} /></span></div>
                    ))}
                    {Array.from({ length: total }).map((_, i) => {
                      const text = typedLines[i] ?? "";
                      const isErr = lineNo0 + i === errLine;
                      const base = tokenize(isErr ? text.replace(/;$/, fixed + ";") : text);
                      const eFrom = isErr ? text.indexOf("this") : undefined;
                      const show = isErr && diag && fix === 0 && eFrom !== undefined && eFrom >= 0 && text.endsWith(";");
                      const lastLine = i === typedLines.length - 1;
                      return (
                        <div key={i} className="relative flex">
                          <span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{lineNo0 + i}</span>
                          {show && <i className="absolute -left-1 top-1.5 h-[9px] w-[3px] rounded-sm bg-[#cb7676]" />}
                          <span>{show ? <Tokens toks={base} errorFrom={eFrom} errorTo={text.length - 1} /> : <Tokens toks={base} />}{lastLine && typed > 0 && typed < TYPED.length ? <span className="caret" /> : null}</span>
                          {show && hover && (
                            <div className="i-pop ide-pop absolute bottom-[26px] left-40 z-10 w-[330px] p-3 font-sans text-[12px] leading-snug text-[var(--i-fg)]" style={{ whiteSpace: "normal" }}>
                              {MSG}<div className="mt-1 text-[11px] text-[var(--i-dim)]">ts(2322)</div>
                              <div className="mt-2.5 border-t border-[var(--i-line)] pt-2 text-[11.5px] text-[var(--i-dim)]">Quick actions (Ctrl+.)</div>
                            </div>
                          )}
                        </div>
                      );
                    })}
                    <div className="flex"><span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{lineNo0 + total}</span><span className="tk-d">{"}"}</span></div>
                  </>
                )}
              </div>
              {pal !== null && <div className="absolute left-4 right-4 top-9 z-20 flex justify-center"><PaletteList query={pal} items={FILES} title="Open file" /></div>}
            </div>
          </div>
          <StatusBar errors={isServer ? 0 : errors} pos={isServer ? "Ln 1, Col 1" : "Ln 15, Col 35"} />
        </div>
      </Fit>
      </div>
    </div>
  );
}
