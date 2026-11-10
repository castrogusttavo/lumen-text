---
title: Language tooling
description: tsgo, Biome, ESLint, tests and formatting, and how Lumen runs the TypeScript toolchain.
section: Using Lumen
order: 3
---

Servers for other languages (Rust, Python, Go, C/C++...) are described on [Language servers](/docs/language-servers); this page covers the TypeScript toolchain.

## tsgo

Lumen serves TypeScript with [`tsgo`](https://github.com/microsoft/typescript-go), the native TypeScript compiler, started on demand. You get diagnostics, rich Markdown hover, completion, signature help, definition, references, rename, organize imports, quick fixes (`Ctrl+.`), expand selection, call hierarchy and semantic tokens. Code lens and inlay hints are optional.

**Full project awareness** (the default) lets `tsgo` read the whole project from disk, like VS Code or Zed: imports resolve without opening the imported files, and creating, editing or deleting a file refreshes the diagnostics of the files that import it. On the 302-file sample project the language server uses about 76 MB; it grows with the project, to several hundred MB on very large ones. **Lean mode** (Settings, *Lean mode*, or the palette command *LSP: Toggle lean mode*) is opt-in for low-RAM machines: it only loads the open files and their imports, so errors and references elsewhere are not seen.

Which `tsgo` runs is the setting *tsgo source*: `auto` uses the project's own `@typescript/native-preview` when installed and a bundled one otherwise; `project` and `bundled` force one.

:::note
Auto import in completion costs about 3 seconds and hundreds of MB on a large project, so it is opt-in (*Auto import*). The `Ctrl+.` quick fixes already offer "add import" for unresolved names.
:::

## Formatting and linting

Format with `Shift+Alt+F`, or on save with *Format on save*. Lumen uses the project's Biome or Prettier and falls back to `tsgo`. Lint comes through `biome lsp-proxy` and ESLint; fixes appear in the `Ctrl+.` menu. *Code: Organize imports* is in the palette.

## Tests and tasks

- `Ctrl+K T` runs the test at the cursor, `Ctrl+K F` the current file, `Ctrl+K R` repeats the last run and `Ctrl+K X` stops it.
- Vitest and Jest runs put pass and fail marks in the gutter.
- *Tasks: Run…* lists the `package.json` scripts and `cargo` tasks; output opens in the bottom panel with ANSI colors (`Ctrl+J`).

## Inlay hints and code lens

*Inlay hints* shows inferred types and parameter names in gray inside the code. Double-click a type to insert the annotation; `Ctrl+click` goes to its definition. *Code lens* shows reference and implementation counts and costs some server CPU, so it is off by default.
