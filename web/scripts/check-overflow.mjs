// Layout audit: nothing may leak outside its container, and the page may never scroll sideways.
//   pnpm run check:overflow           (builds into dist-check, serves it, drives Chromium)
// Env: CHROME_PATH=/path/to/chrome (default: Playwright's own), PAGES=/,/download  WIDTHS=360,390  SHOTS=dir
import { chromium } from "playwright-core";
import { createServer } from "node:http";
import { readFile, stat, mkdir } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const root = new URL("../dist-check/", import.meta.url).pathname;
const widths = (process.env.WIDTHS ?? "360,390,768,1024,1440,1920,2560").split(",").map(Number);
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".webp": "image/webp", ".avif": "image/avif", ".png": "image/png", ".woff2": "font/woff2", ".json": "application/json", ".xml": "text/xml", ".txt": "text/plain" };
const serve = async (req, res) => {
  let p = decodeURIComponent(new URL(req.url, "http://x").pathname);
  const tryFiles = [p, p + ".html", join(p, "index.html")];
  for (const f of tryFiles) {
    const full = normalize(join(root, f));
    if (!full.startsWith(root)) continue;
    try { if ((await stat(full)).isFile()) { res.writeHead(200, { "content-type": types[extname(full)] ?? "application/octet-stream" }); return res.end(await readFile(full)); } } catch {}
  }
  res.writeHead(404, { "content-type": "text/html" }); res.end(await readFile(join(root, "404.html")).catch(() => "404"));
};
const server = createServer(serve).listen(0);
const port = server.address().port;

const { readdirSync } = await import("node:fs");
const found = ["/", "/download", "/changelog", "/roadmap", "/404"];
try { for (const f of readdirSync(join(root, "docs"))) if (f.endsWith(".html")) found.push("/docs/" + f.replace(/\.html$/, "")); } catch {}
const pages = (process.env.PAGES ?? found.join(",")).split(",");

const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || undefined, args: ["--no-sandbox"] });
let bad = 0;
const shots = process.env.SHOTS;
if (shots) await mkdir(shots, { recursive: true });

/** Runs in the page: returns offenders. */
const audit = () => {
  const out = [];
  const vw = document.documentElement.clientWidth;
  if (document.documentElement.scrollWidth > window.innerWidth + 0.5) out.push(`page scrolls sideways: scrollWidth ${document.documentElement.scrollWidth} > ${window.innerWidth}`);
  const describe = (el) => `${el.tagName.toLowerCase()}${el.id ? "#" + el.id : ""}${el.className && typeof el.className === "string" ? "." + el.className.trim().split(/\s+/).slice(0, 3).join(".") : ""}`;
  const clips = (cs) => cs.overflowX !== "visible" || cs.overflowY !== "visible";
  const scrolls = (cs) => /auto|scroll/.test(cs.overflowX) || /auto|scroll/.test(cs.overflowY);
  const all = [...document.body.querySelectorAll("*")];
  for (const el of all) {
    const cs = getComputedStyle(el);
    if (cs.display === "none" || cs.display === "contents" || cs.visibility === "hidden" || cs.position === "fixed") continue;
    if (el.closest("svg") && el.tagName.toLowerCase() !== "svg") continue;
    if (el.closest("[hidden], [aria-hidden='true'][data-ignore-overflow]")) continue;
    if (el.closest(".fit-c") && matchMedia("(max-width: 639px)").matches) continue; // phone crop window: clipping the rest of the design is the point
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    if (el.matches(".sr-only, .sr-only *")) continue;
    // 1) the nearest clipping ancestor must contain the element (a scroll container may scroll its content)
    let a = el.parentElement, clipper = null;
    while (a && a !== document.documentElement) { if (clips(getComputedStyle(a))) { clipper = a; break; } a = a.parentElement; }
    if (clipper && !scrolls(getComputedStyle(clipper))) {
      const c = clipper.getBoundingClientRect();
      const abs = cs.position === "absolute";
      // Decorative backgrounds meant to bleed under a clipper are flagged only when sized by content, not 'inset' decor.
      const over = Math.max(c.left - r.left, r.right - c.right);
      if (over > 2 && !el.closest("[data-clip-ok]")) {
        // Elements that are *meant* to be clipped (hero glows, fit-in scaled replicas) opt in via data-clip-ok.
        out.push(`leaks its clipping ancestor by ${Math.round(over)}px: ${describe(el)} in ${describe(clipper)}${abs ? " (absolute)" : ""}`);
      }
    }
    // 2) no horizontal escape from the viewport unless a clipping ancestor or a scroll container hides it
    if (!clipper && (r.right > vw + 1 || r.left < -1)) out.push(`outside viewport (${Math.round(r.left)}..${Math.round(r.right)} of ${vw}): ${describe(el)}`);
    // 3) visual containers (border/background/shadow) that don't clip: children must stay inside
    const p = el.parentElement;
    if (p && p !== document.body && !el.matches("[data-clip-ok], [data-clip-ok] *")) {
      const ps = getComputedStyle(p);
      const visual = ps.borderTopWidth !== "0px" || ps.backgroundColor !== "rgba(0, 0, 0, 0)" || ps.backgroundImage !== "none";
      if (visual && !clips(ps) && cs.position !== "static" && cs.position !== "relative" && cs.position !== "sticky") {
        const pr = p.getBoundingClientRect();
        const over = Math.max(pr.left - r.left, r.right - pr.right, pr.top - r.top, r.bottom - pr.bottom);
        if (over > 2) out.push(`positioned child leaks its container by ${Math.round(over)}px: ${describe(el)} in ${describe(p)}`);
      }
    }
  }
  return [...new Set(out)];
};

for (const path of pages) {
  for (const w of widths) {
    const ctx = await browser.newContext({ viewport: { width: w, height: 900 }, reducedMotion: "no-preference" });
    const page = await ctx.newPage();
    await page.goto(`http://127.0.0.1:${port}${path}`, { waitUntil: "networkidle" });
    // let scroll-driven and scripted states happen: scroll through the page, then open every popover we can
    const h = await page.evaluate(() => document.documentElement.scrollHeight);
    for (let y = 0; y < h; y += 700) { await page.evaluate((y) => scrollTo(0, y), y); await page.waitForTimeout(120); }
    await page.evaluate(() => scrollTo(0, 0));
    await page.waitForTimeout(2500);
    const issues = await page.evaluate(audit);
    if (shots) await page.screenshot({ path: `${shots}/${(path === "/" ? "home" : path.slice(1).replace(/\//g, "-"))}-${w}.png`, fullPage: false });
    if (issues.length) { bad += issues.length; console.log(`\n${path} @ ${w}px: ${issues.length} issue(s)`); for (const i of issues.slice(0, 15)) console.log("  - " + i); }
    await ctx.close();
  }
}
await browser.close(); server.close();
console.log(bad ? `\n[overflow] ${bad} issue(s)` : `\n[overflow] clean: ${pages.length} pages x ${widths.length} widths`);
process.exit(bad ? 1 : 0);
