---
title: GHP pages
description: Editing and debugging GHP (.ghp) templates in Lumen - setup, language features, the debug workflow and troubleshooting.
section: Using Lumen
order: 4.2
---

[GHP](https://github.com/GHP-GoLang-Framework/ghp) is a Go framework whose pages are `.ghp` templates: HTML with `<go:if>`, `<go:for>`, `<go:switch>` tags and Go expressions. Lumen edits them with their own highlighting, a bundled language server (`ghp-lsp`) and a debugger that stops inside the `.ghp` file, not in the generated Go.

## Setup

Lumen ships `ghp-lsp` next to the editor (in `bin/`, next to the `.exe` on Windows, inside the app bundle on macOS). The rest is yours to install; nothing is downloaded or started behind your back.

| Tool | What for | Install |
| --- | --- | --- |
| `ghp` | `ghp build --debug` before a debug session | a release from [GHP-GoLang-Framework/ghp](https://github.com/GHP-GoLang-Framework/ghp/releases) |
| `dlv` (Delve) | stepping and breakpoints | `go install github.com/go-delve/delve/cmd/dlv@latest` |
| `gopls` | completion, hover and diagnostics for the Go expressions inside a page | `go install golang.org/x/tools/gopls@latest` |

Tools are looked for in `PATH`, `~/go/bin`, `~/.local/bin` and the other folders listed on [Language servers](/docs/language-servers). Open the project folder (the one with `go.mod`, or `.git`) and open a `.ghp` file: the server starts for that project.

## Editing

- **Highlighting**: HTML, the `go:` tags and the Go inside `{ ... }` expressions are coloured with their own grammars.
- **Indentation**: a `<go:if ... />`, `<go:for ... />`, `<go:switch ... />` or `<go:case ... />` indents its body; `<go:else/>`, `<go:elif/>`, `<go:endif/>`, `<go:endfor/>`, `<go:endswitch/>`, `case` and `default` snap back to the opener as you type the closing `>`. `<go:...>` tags are statements, so no closing element is inserted; plain HTML tags still close themselves.
- **Language server**: diagnostics for broken templates (unterminated or unknown tags, unbalanced or unclosed `if` / `for` / `switch` blocks, `else` or `case` outside their block, empty conditions), completion, hover, go to definition, document symbols, folding, selection ranges, document highlights, formatting (whole file and range), quick fixes (`Ctrl+.`) and semantic colours. Go expressions are forwarded to `gopls`, with positions mapped back to the `.ghp` file; without `gopls` the template features still work.
- **Server log**: *LSP: Show server log* in the palette shows what `ghp-lsp` said.

## Debugging a page

1. Put breakpoints in the `.ghp` file (`F9` or a click in the gutter). Conditional breakpoints and logpoints work as for any language ([Debugging](/docs/debugging)).
2. With a `.ghp` file or a GHP project open, press `F5`. If several launch configurations apply (a Go `main` and GHP), pick one with *Debug: Select launch configuration…* or the toolbar chip.
3. Lumen runs `ghp build --debug` **in the background**. The toolbar shows *Building…*, the window stays usable, and *Stop* cancels. When the build is done, Delve starts the program and the session begins. If the build fails, its output (the last lines of `ghp`) appears in the **Debug Console** and the panel opens.
4. Execution stops in the `.ghp` file, on the line of the tag or expression. Locals, Watch, the call stack and hover-to-evaluate work as in Go. A structured value in Watch (a struct, slice or map) has a chevron: click it to see its fields, like in Variables.

The debug binary is written to `.lumen/ghp-debug/app` (`app.exe` on Windows) in the project. That folder contains its own `.gitignore` (`*`), so nothing needs adding to your repository's. Delete the folder at any time; the next `F5` rebuilds it.

Breakpoints follow your edits: lines you insert or delete above a breakpoint move it, and a running session is told the new lines. The program being debugged is the one that was built when you pressed `F5`; to debug your changes, restart (the toolbar button) so it is rebuilt.

### Overriding the launch: `.lumen/launch.json`

For a page that needs arguments, an environment or another binary, create `.lumen/launch.json` in the project (whole-line `//` comments are allowed):

```json
{
  "configurations": [
    {
      "config": "ghp",
      "args": ["-addr=127.0.0.1:8081"],
      "env": { "APP_ENV": "dev" },
      "cwd": "site"
    }
  ]
}
```

| Key | Meaning |
| --- | --- |
| `config` | Which launch configuration it applies to: `ghp` or `delve` (Go). Defaults to `adapter`. |
| `adapter` | Debug with another adapter of the table (`ghp` or `delve`) instead of the configuration's own. |
| `program` | Program to debug. Relative paths start at the project root. For `ghp` the build writes its output there too. |
| `args` | Program arguments. |
| `env` | Environment variables for the program. |
| `cwd` | Working directory. Relative paths start at the project root. |

`${root}` (the project), `${file}` and `${dir}` (the file being debugged) are replaced in every value. An invalid file is reported in the status bar and ignored. Starting a session runs what the file names, like building the project does: only keep `launch.json` files of projects you trust.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| No colours or diagnostics in a `.ghp` file | Open *LSP: Show server log*. `ghp-lsp` must be next to the editor binary or on `PATH`; a build from source needs `cargo install --path tools/ghp-lsp`. |
| Go expressions have no completion or errors | `gopls` is not installed or does not find `go.mod`. Install it (table above) and reopen the file. |
| *ghp not found (install: ...)* in the status bar | `ghp` is not on `PATH`. Install a release and restart Lumen if it was started from the desktop menu. |
| *dlv not found* | `go install github.com/go-delve/delve/cmd/dlv@latest`; `~/go/bin` is searched. |
| *debug build failed* | The Debug Console has the output of `ghp build --debug`; fix the template or Go error it names and press `F5` again. |
| Breakpoints are hollow and never hit | The adapter could not place them (Breakpoints list: *not placed by the adapter*). Check that the file is part of a page that is built, and that you restarted after editing. |
| *bind: address already in use* on a new session | Stop and Restart end the program with the session (and wait for it to be gone). If it still happens, something outside Lumen holds the port, for example a server started from a terminal; the Debug Console shows the program's own output, including this error. |
| *execution finished* with no output | The Debug Console shows what the program printed (stdout, and stderr in red) and *Program exited with code N*; the status bar repeats the code and the last line. |
| Windows: the program is not found | The binary is `.lumen\ghp-debug\app.exe`; if you set `program` in `launch.json`, include `.exe`. |
