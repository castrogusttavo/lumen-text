/** Tiny TypeScript highlighter for the IDE replicas. Output classes map to .tk-* in global.css. */
export type Tok = [cls: string, text: string];

const KEYWORDS = new Set("import export from as const let var return async await if else while for of new class interface type extends implements private public readonly static get set try catch throw this void default function typeof in".split(" "));
const STORAGE = new Set(["const", "let", "var", "type", "interface", "class", "function", "private", "public", "readonly", "static", "void"]);
const PRIMS = new Set(["string", "number", "boolean", "undefined", "null", "never", "unknown", "any"]);

const RE = /(\/\/.*|\/\*.*?\*\/)|("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)|(\b\d[\d_.]*\b)|([A-Za-z_$][\w$]*)|(\s+)|([\s\S])/g;

export function tokenize(code: string): Tok[] {
  const out: Tok[] = [];
  const push = (c: string, t: string) => {
    const last = out[out.length - 1];
    if (last && last[0] === c) last[1] += t; else out.push([c, t]);
  };
  let m: RegExpExecArray | null;
  RE.lastIndex = 0;
  while ((m = RE.exec(code))) {
    const [, com, str, num, id, ws, ch] = m;
    if (com) push("c", com);
    else if (str) push("s", str);
    else if (num) push("n", num);
    else if (id) {
      const rest = code.slice(RE.lastIndex);
      if (STORAGE.has(id)) push("st", id);
      else if (KEYWORDS.has(id)) push("k", id);
      else if (PRIMS.has(id) || /^[A-Z]/.test(id)) push("t", id);
      else if (/^\(/.test(rest)) push("f", id);
      else if (/^\??:/.test(rest) && out.length && /[{,(]\s*$|^\s*$/.test(code.slice(0, m.index).split("\n").pop() ?? "") ) push("p", id);
      else push("v", id);
    } else if (ws) push("", ws);
    else if (ch) push(/[{}()[\];,.<>:=|&?!+\-*/]/.test(ch) ? "d" : "", ch);
  }
  return out;
}

/** First `n` characters of a token stream. */
export function sliceTokens(toks: Tok[], n: number): Tok[] {
  const out: Tok[] = [];
  let left = n;
  for (const [c, t] of toks) {
    if (left <= 0) break;
    out.push([c, t.length <= left ? t : t.slice(0, left)]);
    left -= t.length;
  }
  return out;
}

export function escapeHtml(s: string) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }
export function toHtml(toks: Tok[]) { return toks.map(([c, t]) => (c ? `<span class="tk-${c}">${escapeHtml(t)}</span>` : escapeHtml(t))).join(""); }
