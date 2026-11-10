# Lumen web

The landing page, download page, changelog and docs for [Lumen](https://github.com/castrogusttavo/lumen-text).
Astro (static output) with React islands only where something is interactive, Tailwind CSS v4 (CSS-first, `@theme`), self-hosted fonts.

```
web/
  src/
    components/   Astro sections (Hero, Comparison, Metrics, Bento, FAQ, ...) and components/docs/
    islands/      React islands: HeroIde, DemoTabs, Bento (palette, split, themes, git, hover, settings), ide.tsx (shared IDE replica)
    layouts/      Base.astro (head, meta, JSON-LD, theme), Docs.astro (sidebar, TOC, search box)
    content/docs/ the docs, one Markdown file per page (content collection, see src/content.config.ts)
    data/         benchmarks.json, release.json, releases.json, themes.json, keymap.json, commands.json, settings.json, faq.ts
    pages/        index, download, changelog, 404, docs/[...slug], docs/search.json, robots.txt
    styles/       global.css (tokens, IDE replica palette, docs typography)
  scripts/        fetch-release, extract-themes, extract-docs-data, make-images, check-bench, check-overflow
  public/         favicon, logo marks, og.png, img/ (AVIF/WebP, generated)
```

## Develop, build, deploy

```sh
cd web
pnpm install
pnpm dev                     # http://localhost:4321/lumen-text (base path, see below)
BASE_PATH=/ pnpm dev         # serve at the root instead
pnpm build:preview           # local build that tolerates empty benchmarks (never publish this)
pnpm build                   # production build: FAILS while benchmarks.json has placeholders
pnpm preview
```

- **Base and site**: `SITE_URL` (default `https://castrogusttavo.github.io`) and `BASE_PATH` (default `/lumen-text`). For a custom domain build with `SITE_URL=https://example.com BASE_PATH=/`.
- **Deploy**: `.github/workflows/web.yml` builds with pnpm and publishes with `upload-pages-artifact` + `deploy-pages` on pushes touching `web/**` (or the Rust files the docs are generated from) and on every published release. In the repository settings, set Pages source to "GitHub Actions". Repository variables `SITE_URL` and `BASE_PATH` override the defaults.
- **Release data**: `pnpm build` runs `scripts/fetch-release.mjs`, which resolves the latest GitHub release into `src/data/release.json` and the version list into `src/data/releases.json` (the committed copies are the offline fallback). The `/download` page also refreshes itself in the browser from the GitHub API.
- **Docs data**: `pnpm extract` regenerates `keymap.json`, `commands.json`, `settings.json` (from `src/keymap.rs`, `src/app_overlay/commands.rs`, `src/settingsui/model.rs`, `src/config.rs`) and `themes.json` (from `assets/themes`). CI checks they are up to date.
- **Images**: `SHOTS_DIR=<dir with editor.png, light.png> pnpm images` regenerates the aurora and grain backgrounds, the screenshots (AVIF + WebP) and the OG image. The screenshots come from the editor's headless mode (`lumen-text --snap`, see the docs, "Build from source").
- **Layout audit**: `pnpm run check:overflow` builds into `dist-check`, serves it and drives Chromium over every page at 360, 390, 768, 1024, 1440, 1920 and 2560 px. It fails on horizontal page scroll, elements that escape a clipping ancestor and positioned children that leak their container. Set `CHROME_PATH` to use a specific Chromium, `PAGES=/,/download` and `WIDTHS=390,1440` to narrow it, `SHOTS=dir` to save a screenshot per run.

## Benchmarks: `src/data/benchmarks.json`

The comparison section renders entirely from this file. It never contains invented numbers: a cell is either a real value with its source, or `null` (shown as "measuring..." in dev).

**The production build fails if any required metric is `null`** (`scripts/check-bench.mjs`, run first by `pnpm build`). Metrics are required unless they set `"required": false`. `ALLOW_PENDING_BENCH=1` bypasses the check for local previews.

```jsonc
{
  "schemaVersion": 1,
  "measuredAt": "2026-11-10",                // ISO date the competitor numbers were taken; null = not measured
  "machine": {                               // the machine every number was taken on
    "label": "string", "cpu": "string|null", "ramGb": 8, "storage": "string|null", "os": "string|null", "note": "string?"
  },
  "methodology": "string",                   // shown verbatim in the footnote; describe scenario, tools, PSS vs RSS
  "editors": [                               // columns, in display order; the first is Lumen
    { "id": "lumen|vscode|zed|sublime|openidea", "name": "string", "version": "string|null", "highlight": true, "note": "string?" }
  ],
  "groups": [ { "id": "memory", "label": "Memory" } ],   // metric groups (card eyebrow)
  "metrics": [
    {
      "id": "ram-idle", "group": "memory", "label": "string", "description": "string",
      "unit": "MB|ms", "lowerIsBetter": true,
      "scaleMax": 1024,                      // bar scale: a bar is value / scaleMax; choose it once, per metric
      "required": true,                      // default true; false = may stay null without failing the build
      "values": {
        "<editor id>": {
          "value": 16,                       // number | null
          "source": "measured | external-reference | pending",
          "url": "https://...",              // required when source is external-reference
          "note": "string?"                  // tooltip: test name, peak value, caveat
        }
      }
    }
  ],
  "facts": { ... }                           // used by "Metrics active": tests, themes, grammars, budgets, memoryScenarios
}
```

Rules for whoever fills it (`tools/bench-compare.sh` is meant to write this file):

- Use the same machine, the same scenario and the same file for every editor; record the editor versions in `editors[].version`.
- Memory is PSS (proportional set size), summed over the editor's whole process tree (Electron helpers and language servers count for the editor; `lsp-memory` isolates the TypeScript server).
- `source: "measured"` for numbers taken by the script; `"external-reference"` only with a public URL.
- Windsurf and Devin are VS Code forks and are covered by the `vscode` column (the section says so).
- Keep `facts.budgets` and `facts.memoryScenarios` in sync with `src/perf.rs` and the README.

Required metric ids (each needs a value for every editor): `ram-idle`, `ram-open-100k`, `disk`, `startup`, `open-100k`, `scroll-frame`, `keystroke`, `lsp-first-diagnostics`, `lsp-memory`.

## Performance notes

JavaScript ships only for the islands (`client:idle` hero, `client:visible` everything else) plus a small vanilla script for reveal, counters, theme toggle and OS detection. Fonts are Latin subsets of Geist and Geist Mono, preloaded; images are AVIF/WebP with explicit width and height and `loading="lazy"` below the fold; motion respects `prefers-reduced-motion`.

## Methodology (how the comparison numbers are taken)

`tools/bench-compare.sh` produces every competitor number in `src/data/benchmarks.json` and merges it in (run it again to refresh; `--only zed`, `--runs N`, `--skip gui|lsp|cli|disk`, `--no-merge`; raw per-run results are archived in `tools/bench/results/`).

- **Workspace**: `tools/bench/gen-sample.mjs` generates, under `~/.cache/lumen-bench/sample`, 302 TypeScript files with cross imports, `big/big.ts` (100,000 lines, about 5.8 MB) and `src/broken.ts` (one deliberate type error). Nothing else on the machine is opened.
- **Profiles**: each editor runs with its own `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` under `~/.cache/lumen-bench/profiles/<editor>` (VS Code also gets its own `HOME`, `--user-data-dir` and `--extensions-dir`, so it has no extensions). One warm-up launch per editor is discarded (first-run setup, Zed downloading vtsls), then N runs (default 5); cells hold the median, the note holds min/max.
- **Memory**: PSS from `/proc/<pid>/smaps_rollup`, summed over every process of the editor's tree (found by a `BENCH_TAG` environment variable plus descendants), so Electron helpers, GPU processes and language servers count. Idle = the workspace and `README.md` open (no language server starts), 20 s after the window settled. "100k lines" = launched into `big/big.ts`, 10 s after settling.
- **Startup / open**: wall time from `spawn` until the window is mapped (Hyprland `hyprctl clients`) and then until its pixels stop changing (`grim` at 10% scale every 100 ms; a frame is a change when more than 1% of the pixels differ; stable for 1.5 s). The same method for every editor, but it is a visual "settled" time, not an internal "ready" event. Windows open briefly on the current workspace, one at a time.
- **TypeScript**: measured at the language-server protocol level (`tools/bench/lsp-bench.mjs`): spawn the server the editor ships, open `src/broken.ts`, time to the first diagnostics, then PSS of the server tree. Lumen: tsgo on the whole project as Lumen starts it by default (`GOMEMLIMIT=512MiB GOGC=50`), with the opt-in narrow mirror (`lsp_narrow`, as `src/lsp.rs` builds it) in the note; VS Code: its bundled tsserver (TypeScript 6.0.3); Zed: vtsls. `facts.cliTypecheck` adds `tsc --noEmit` (TypeScript 6.0.3, JavaScript), `tsc` 7.0.2 (native) and Lumen's bundled tsgo on the whole project.
- **Disk**: sum of file sizes of the install (Lumen: the version directory; VS Code: the extracted official tarball; Zed and Sublime: the pacman package files; openidea: the extracted AppImage).

Schema additions (backwards compatible): a cell may have `"source": "unavailable"` with `"value": null` and a `"reason"` string (shown instead of a bar; `scripts/check-bench.mjs` should accept it); `facts.cliTypecheck` holds the CLI reference. Scroll-frame and keystroke are `"required": false` because only Lumen's own test numbers exist (the other editors have no comparable counter).
