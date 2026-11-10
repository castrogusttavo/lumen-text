import { useState } from "react";
import { Fit, PaletteList, Sidebar, StatusBar, TabBar, TitleBar, Tokens, tokenize, useInView, useLoop } from "./ide";

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
`;
const TYPED = `  get oldest(): number {
    return this.jobs[0]?.attempts;
  }`;
const FIX = " ?? 0";
const MSG = "Type 'number | undefined' is not assignable to type 'number'.";
const FILES = ["queue.ts", "queue.test.ts", "server.ts", "package.json", "README.md", "tsconfig.json"].map((label) => ({ label }));

const headLines = HEAD.split("\n").slice(0, -1).map((l) => tokenize(l));

export default function HeroIde() {
  const [ref, seen] = useInView<HTMLDivElement>();
  const [typed, setTyped] = useState(TYPED.length); // SSR shows the finished, still-erroring state
  const [fix, setFix] = useState(0);
  const [diag, setDiag] = useState(true);
  const [hover, setHover] = useState(true);
  const [pal, setPal] = useState<string | null>(null);

  useLoop(seen, async (sleep) => {
    const type = async (n: number, set: (v: number) => void, ms: number) => { for (let i = 1; i <= n; i++) { set(i); await sleep(ms); } };
    // 1) fix the error (the static state starts here)
    await sleep(1800);
    setHover(false); await sleep(500);
    await type(FIX.length, setFix, 120);
    setDiag(false); await sleep(1600);
    // 2) quick open
    setPal(""); await sleep(600);
    for (const q of ["s", "se", "ser"]) { setPal(q); await sleep(280); }
    await sleep(900); setPal(null); await sleep(700);
    // 3) erase and type again
    setTyped(0); setFix(0); await sleep(700);
    await type(TYPED.length, setTyped, 34);
    await sleep(700); setDiag(true); await sleep(900); setHover(true);
  });

  const typedLines = TYPED.slice(0, typed).split("\n");
  const fixed = FIX.slice(0, fix);
  const lineNo0 = headLines.length + 1;
  const errLine = lineNo0 + 1;
  const total = Math.max(typedLines.length, 1);

  return (
    <div ref={ref}>
      <Fit w={1000} h={500} max={1200}>
        <div className="ide relative flex h-full flex-col shadow-[0_40px_120px_-30px_rgb(0_0_0/0.9)]" role="img" aria-label="Lumen editing a TypeScript file: a type error appears, is fixed, and quick open searches for server.ts">
          <TitleBar />
          <div className="flex min-h-0 flex-1">
            <Sidebar active="queue.ts" />
            <div className="relative flex min-w-0 flex-1 flex-col">
              <TabBar tabs={["queue.ts", "server.ts"]} active="queue.ts" />
              <div className="px-4 pt-1.5 text-[11.5px] text-[var(--i-dim)]">src &gt; queue.ts &gt; <span className="text-[var(--i-fg)]">JobQueue</span></div>
              <div className="code relative flex-1 overflow-hidden px-2 pt-1">
                {headLines.map((toks, i) => (
                  <div key={i} className="flex"><span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{i + 1}</span><span><Tokens toks={toks} /></span></div>
                ))}
                {Array.from({ length: total }).map((_, i) => {
                  const text = typedLines[i] ?? "";
                  const isErr = lineNo0 + i === errLine;
                  const base = tokenize(text + (isErr ? fixed : ""));
                  const eFrom = isErr ? text.indexOf("this") : undefined;
                  const eTo = isErr ? text.length : undefined;
                  const show = isErr && diag && fix === 0 && eFrom !== undefined && eFrom >= 0;
                  const lastLine = i === typedLines.length - 1;
                  return (
                    <div key={i} className="relative flex">
                      <span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{lineNo0 + i}</span>
                      {show && <i className="absolute -left-1 top-1.5 h-[9px] w-[3px] rounded-sm bg-[#cb7676]" />}
                      <span>{show ? <Tokens toks={base} errorFrom={eFrom} errorTo={eTo} /> : <Tokens toks={base} />}{lastLine && typed > 0 && typed < TYPED.length ? <span className="caret" /> : null}</span>
                      {show && hover && (
                        <div className="i-pop ide-pop absolute left-40 top-6 z-10 w-[330px] p-3 font-sans text-[12px] leading-snug text-[var(--i-fg)]" style={{ whiteSpace: "normal" }}>
                          <div className="mb-1 font-medium text-[#cb7676]">Error</div>{MSG}<div className="mt-1.5 text-[11px] text-[var(--i-dim)]">ts(2322) · tsgo</div>
                          <div className="mt-2.5 flex gap-2 border-t border-[var(--i-line)] pt-2 text-[11px] text-[var(--i-dim)]"><span className="i-kbd">Ctrl+.</span> Quick fix</div>
                        </div>
                      )}
                    </div>
                  );
                })}
                <div className="flex"><span className="w-9 select-none pr-4 text-right text-[var(--i-faint)]">{lineNo0 + total}</span><span className="tk-d">{"}"}</span></div>
              </div>
              {pal !== null && <div className="absolute left-1/2 top-12 z-20 -translate-x-1/2"><PaletteList query={pal} items={FILES} /></div>}
            </div>
          </div>
          <StatusBar errors={diag && fix === 0 && typed === TYPED.length ? 1 : 0} />
        </div>
      </Fit>
    </div>
  );
}
