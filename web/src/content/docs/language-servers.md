---
title: Language servers
description: Which language servers Lumen starts, how to add your own, how several servers share a file, and what they cost in RAM.
section: Using Lumen
order: 4
---

Lumen speaks the Language Server Protocol to as many servers as you like. TypeScript and JavaScript keep their dedicated, tuned path (`tsgo`, see [Language tooling](/docs/typescript)); everything else goes through one table of servers. A server is only started when **a matching file is opened and its program is found**; nothing is installed or started behind your back.

## Built-in servers

| Language | Servers, in order of preference | Project root is the nearest folder with |
| --- | --- | --- |
| Rust | `rust-analyzer` | `Cargo.toml`, `rust-project.json` |
| Python | `pyright-langserver`, `basedpyright-langserver`, `pylsp`; companion `ruff server` | `pyrightconfig.json`, `pyproject.toml`, `setup.py`, `.git` |
| Go | `gopls` | `go.work`, `go.mod` |
| C, C++ | `clangd` | `compile_commands.json`, `compile_flags.txt`, `.clangd` |
| Java | `jdtls` (Eclipse JDT LS, Java 25+) | `pom.xml`, `build.gradle(.kts)`, `settings.gradle(.kts)`, `.project` |
| C# | `csharp-ls`, `OmniSharp -lsp` | `*.sln`, `*.slnx`, `*.csproj` |
| Kotlin | `kotlin-lsp --stdio` (JetBrains), `kotlin-language-server` | `settings.gradle(.kts)`, `build.gradle(.kts)`, `pom.xml` |
| Swift, Objective-C | `sourcekit-lsp` | `Package.swift`, `*.xcodeproj` |
| Dart | `dart language-server` | `pubspec.yaml` |
| Elixir | `elixir-ls` | `mix.exs` |
| Haskell | `haskell-language-server-wrapper --lsp` | `stack.yaml`, `cabal.project`, `*.cabal` |
| Scala | `metals` | `build.sbt`, `build.sc`, `build.mill` |
| Terraform | `terraform-ls serve` | `.terraform` |
| Dockerfile | `docker-langserver --stdio` | `.git` |
| Lua | `lua-language-server` | `.luarc.json`, `.git` |
| TOML | `taplo lsp stdio` (schemas for `Cargo.toml`, `pyproject.toml`, `rustfmt.toml`...) | `taplo.toml`, `.git` |
| JSON | `vscode-json-language-server` (schemas for `package.json`, `tsconfig.json`, `composer.json`...) | `package.json`, `.git` |
| CSS, SCSS, Less | `vscode-css-language-server` (unknown at-rules such as Tailwind's `@apply` are not flagged) | `package.json`, `.git` |
| HTML | `vscode-html-language-server` | `package.json`, `.git` |
| YAML | `yaml-language-server` (SchemaStore: GitHub workflows, docker-compose, pnpm-workspace...) | `.git` |
| Shell | `bash-language-server start` | `.git` |
| Markdown | `marksman server` | `.marksman.toml`, `.git` |
| Zig | `zls` | `build.zig`, `.git` |
| Ruby | `ruby-lsp`, `solargraph stdio` | `Gemfile`, `.git` |
| PHP | `phpactor language-server`, `intelephense --stdio` | `composer.json`, `.git` |
| Svelte, Astro, Vue | `svelteserver`, `astro-ls`, `vue-language-server` | the framework's config file, `package.json` |
| GHP templates (`.ghp`) | `ghp-lsp` ([releases](https://github.com/GHP-GoLang-Framework/ghp-lsp/releases); Go expressions go through `gopls`) | `go.mod`, `.git` |
| TS / JS / CSS / HTML | companion `tailwindcss-language-server`, only in a project with a Tailwind config | `tailwind.config.*` |
| TS / JS | `deno lsp` replaces `tsgo`, only in a project with `deno.json` | `deno.json` |
| Prose (spell checking, off by default) | companions `harper-ls --stdio` and `typos-lsp`, only with `spell_check` on; see below | `.git` (typos-lsp: also `typos.toml`) |
| every file | companion `copilot-language-server --stdio` (GitHub Copilot inline completions), only with the setting `inline_completion` on | the project folder |

For a language the **first installed** server of the list is the *primary* one; the others of that language are ignored, so installing `pyright` is enough. A *companion* (`ruff`, `tailwindcss`) runs next to the primary and adds to it.

Executables are looked for in `<project>/node_modules/.bin` (and its parents), `PATH`, `~/.local/share/lumen-text/lsp/bin`, `~/.cargo/bin`, `~/.local/bin`, `~/go/bin`, `~/.dotnet/tools` and the Ruby user-gem folders (`~/.local/share/gem/ruby/*/bin`). A server runs with those folders, and the folder of its own executable, at the front of its `PATH`, so the helpers it calls (`bundle`, `zig`, `go`, `dotnet`, `node`) are found even when Lumen was started from the desktop menu. A file whose language has no installed server shows a one-time message with the install command.

The home folder is never a workspace root for a file below it (a stray `~/package.json` or `~/.git` would otherwise make a server index the whole home). Python servers are given the project's virtualenv so installed dependencies resolve: `.venv`, `venv`, `env` or any folder holding a `pyvenv.cfg` in the project or a parent folder (uv and `python -m venv` layouts), `$UV_PROJECT_ENVIRONMENT`, `$VIRTUAL_ENV`, then Poetry's cache folder (matched by project name). pyright and basedpyright get `python.pythonPath`, `python.venvPath` and `python.venv` (in `workspace/configuration` and the initialization options), every Python server runs with `VIRTUAL_ENV` set and the venv's `bin` first on its `PATH`, and a ruff or basedpyright installed in the venv is used instead of the one on the `PATH`. *Python: Select interpreter…* lists the venvs found and the system pythons and pins one per project (remembered in the state folder); the tooltip of the language item in the status bar shows the active interpreter. Your own `settings.python.pythonPath` always wins.

The table is data, not code: it is `src/lsp_servers.json` in the repository, embedded in the program.

### Inline completions (ghost text)

With the setting `inline_completion` on (Settings, or the palette: *Inline completion: Toggle on / off*), Lumen asks
the servers of the file for `textDocument/inlineCompletion` (LSP 3.18) after a 250 ms typing pause and shows the
first suggestion as faded text at the cursor; the lines after the first one appear as extra rows below the cursor
line. `Tab` accepts it (one undo step; a command that comes with it is then run with `workspace/executeCommand`),
`Ctrl+Right` accepts the next word, `Esc` dismisses it, `Alt+]` / `Alt+[` show the other suggestions and `Alt+\`
asks right away. Nothing is asked while the completion popup is open, with several cursors or with a selection;
moving the cursor or typing something else cancels the request in flight. A status bar item shows the state (on,
off, signed out, loading, no server); a click pauses it, or signs in when Copilot is signed out.

Any server that offers inline completions works. For **GitHub Copilot**, install its language server
(`npm i -g @github/copilot-language-server`) and run *Copilot: Sign in*: Lumen shows the device code (and copies
it), opens `github.com/login/device` in your browser and finishes when you confirm there. The login is stored by the
server itself; Lumen never sees a token or an API key. *Copilot: Sign out* signs out. Copilot only serves inline
completions in Lumen (its other features are not merged into hover, completion or code actions). The sign-in uses
Copilot's own requests (`checkStatus`, `signIn`, `signOut`); if a future server changes them, only the sign-in is
affected. Switch Copilot off while keeping other servers' suggestions with `"language_servers": {"copilot": {"enabled": false}}`.

## Your own servers

Add a `language_servers` object to `~/.config/lumen-text/config.json`:

```json
{
  "language_servers": {
    "rust-analyzer": { "args": ["--log-file", "/tmp/ra.log"], "env": { "RA_LOG": "info" } },
    "python": { "enabled": false },
    "go": { "command": "/opt/gopls/bin/gopls", "args": ["serve"] },
    "my-server": {
      "command": "my-lsp",
      "args": ["--stdio"],
      "languages": ["foo", "bar"],
      "root_markers": [".git", "foo.toml"],
      "initialization_options": { "mode": "fast" },
      "settings": { "foo": { "level": 3 } }
    }
  }
}
```

A key is resolved in this order:

1. **A server name** from the table (`rust-analyzer`, `ruff`, `tailwindcss`...) or one you defined: the fields you give replace the built-in ones.
2. **A language** (`rust`, `python`, `go`, `typescript`...): `"enabled": false` switches off the language's primary servers; a `"command"` replaces them with your own server for that language; other fields apply to those servers.
3. **Anything else** defines a new server and needs a `command`.

| Field | Meaning |
| --- | --- |
| `command` | Program name (looked up as above) or a path (`~/` and project-relative paths work). |
| `args` | Arguments. `${root}` is the workspace root. |
| `env` | Extra environment variables. |
| `languages` | Editor language ids **or file extensions** (without the dot) the server handles. |
| `extensions` | More file extensions. |
| `root_markers` | Files or folders that mark a workspace root; the nearest folder holding one is the root (else the file's own folder). `*.ext` matches any file with that extension (`*.csproj`). |
| `initialization_options` | Sent in `initialize`. `${root}` works inside strings. |
| `settings` | Answers `workspace/configuration` requests (by dotted section, such as `python.analysis`; a section you did not set is `{}`, a dotted key you did not set is `null`) and is pushed with `workspace/didChangeConfiguration` right after `initialized`, before any document is opened, and again when the server registers for configuration changes. |
| `enabled` | `false` switches the server off. |
| `role` | `"primary"` (default) or `"companion"`. |
| `requires` | Any-of list of files that must exist in the project (or a parent) for the server to apply. |
| `install` | Hint shown when the program is not found. |

A mistake never stops the editor: the offending field is ignored and the first problem is shown in the status bar (for example `language_servers.rust-analyzer.args must be an array of strings`); the rest of the file still applies.

## Spell checking

Two spell checkers are built in as *companions*, **off until you turn on `spell_check`** (Settings → *Spell checking*, or the palette *Language servers: Toggle spell checking*). Like every server they start only when installed:

| Server | What it checks | Install |
| --- | --- | --- |
| [harper-ls](https://github.com/Automattic/harper) | Spelling and grammar in comments and strings of code (Rust, TS/JS, Python, Go, C/C++, C#, Java, Kotlin, Swift, Dart, Scala, Haskell, Elixir, Lua, Ruby, PHP, shell, Zig, Nix, TOML, HTML), Markdown, LaTeX, Typst, plain text (`.txt`) and git commit messages | `cargo install harper-ls --locked`, or a release binary in `~/.local/share/lumen-text/lsp/bin` |
| [typos-lsp](https://github.com/tekumara/typos-lsp) | Common misspellings in identifiers and text (few false positives), the same files plus CSS, YAML, SQL, Vue, Svelte and Astro | a release binary on `PATH` or in `~/.local/share/lumen-text/lsp/bin` |

`tools/install-lsp-servers.sh spell` installs both (pinned versions, checksums verified).

What they find is never shown as an error: whatever severity a spell checker reports, its diagnostics are *info* and get a **dotted** underline (errors and warnings are wavy) and no error / warning count in the bottom bar. `Ctrl+.` on a flagged word lists the fixes ("Replace with: …", typos-lsp's correction) and harper-ls's "Add … to the user / workspace / file dictionary" and "Ignore" commands, which run on the server that offered them.

To keep one of them off while the other runs, switch it off by name: `"language_servers": {"typos-lsp": {"enabled": false}}` (a server's own `enabled` wins over `spell_check`, both ways). harper-ls reads its own options from `settings` under `harper-ls` (dialect, linters...), typos-lsp from a `typos.toml` in the project.

## Status, restart and log

*Settings* has a **Language servers** section listing each server with its state: *running*, *starting*, *installed* (starts when a matching file opens), *not installed*, *disabled* or *crashed*, the resolved command and a switch to turn it on or off. The palette has:

- **LSP: Restart language servers** (stops every server and forgets crash counters; they start again with the next file).
- **LSP: Show server log** (opens what each server printed on stderr and a one-line summary of the traffic; the log is a bounded ring, about 400 lines per server).

## One process per workspace

A server runs **once per (server, workspace root)**, started on the first matching file and kept while one of its files is open plus a 45 second grace period, so closing and reopening a file is instant. Disk changes under the root are forwarded as `workspace/didChangeWatchedFiles`.

If a server dies it is restarted after 0.5 s, then 1 s, 2 s, 4 s and 8 s. After the fifth failure in a row it is left off and the status bar says `<server> crashed`; *Restart language servers* tries again. A server that ran for a minute is forgiven its earlier crashes.

The bottom bar shows `RAM <editor> + <server> <MB>`. With one or two servers each is listed; with three or more it reads `+ 3 servers 210 MB` and the tooltip lists them one by one. Clicking the memory item opens the **Resources** panel: the editor, every server, terminal, AI agent, debug adapter, task and Build & Run job with PSS, RSS and uptime, and a Stop (servers also Restart) action per row; servers stop immediately and start again on next use, terminals with a running job and agents ask first. It measures only while open. Set `mem_budget_mb` (Settings, *Memory budget*; 0 = off) for a soft limit: over it the item reads `over budget`, a toast shows once, and the panel offers to stop idle servers. While a server reports work (indexing, loading a workspace) its `$/progress` title shows in the status line.

## Several servers on one file

A document can have a primary server and any number of companions (`tsgo` and Tailwind on a `.tsx`, `pyright` and `ruff` on a `.py`). All of them receive the same text.

| What | How it is resolved |
| --- | --- |
| Diagnostics | Every server's list is shown, each tagged with its source (`ruff: F401`). The same range and message from two servers appears once, the primary's copy wins. |
| Hover | The primary's section first, then each companion's after a rule. |
| Completion | The lists are joined, the primary's first. An item repeating a label and text is dropped; a companion's item resolves on its own server. |
| Code actions (`Ctrl+.`) | Every server's actions, duplicates by title removed; an action is resolved on the server that offered it. |
| Definition, references, rename, symbols, signature help, folding, inlay hints, semantic tokens | The primary only. |
| Formatting | The project's Biome or Prettier first, then the primary server, then the first companion that can format, then the built-in JSON re-indenter. |

A server that lacks a feature is simply not asked: the editor reads the capabilities from `initialize` and from dynamic registration (`client/registerCapability`).
