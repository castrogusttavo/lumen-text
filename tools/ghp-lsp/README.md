# ghp-lsp

Language server for [GHP](https://github.com/GHP-GoLang-Framework/ghp) templates (`.ghp`): HTML pages with
Go islands (`<go= expr />`, `<go:if cond/>`, `<go:for .../>`, `<go ... />`, `<go:import .../>`).

It speaks LSP over stdio, so any editor with a generic LSP client can use it. Lumen ships the binary next
to the editor and starts it for `.ghp` files (`lsp_servers.json`).

* The **GHP side** (tags, blocks, structure) is answered by the server itself, from an error-tolerant parser:
  it always produces a tree, whatever the input, and every problem becomes a diagnostic.
* The **Go side** (everything inside the tags) is answered by a real `gopls`, through a virtual `.go`
  document that mirrors what the GHP compiler generates. While gopls is missing, still starting or
  crashed, a built-in scope analysis answers for the page's local names.

## Capabilities

| LSP feature | What it does |
| --- | --- |
| `textDocument/publishDiagnostics` | Parser errors (unclosed `if`/`for`/`switch`, stray closers, empty conditions, bad imports, ...) plus gopls' diagnostics for the Go text, mapped back onto the page. Debounced (150 ms) after edits; **republished on its own when gopls reports late** (it is asynchronous), and when gopls starts, restarts or crashes. |
| `textDocument/completion` | Tag completion (`<go:if` ... with snippets that close their block) in markup; Go completion inside tags, including an **empty tag** (`<go= | />`) and **attribute values** (`<a href="<go= u| />">`). Trigger characters `<` `:` `=` `.`. |
| `textDocument/hover` | Tag documentation in markup; gopls (or scope) hover inside Go text. |
| `textDocument/definition` | Go definitions (gopls, or local names via scope analysis), mapped to page positions. |
| `textDocument/documentSymbol` | Blocks, branches and imports as an outline. |
| `textDocument/foldingRange` | Blocks, branches, multi-line tags. |
| `textDocument/selectionRange` | Word -> Go expression -> tag -> enclosing blocks -> document. |
| `textDocument/formatting`, `rangeFormatting` | Re-indents block structure only (never touches `<pre>`/`<textarea>`, leaves templates with errors alone, idempotent). Go code is not reformatted. |
| `textDocument/documentHighlight` | Pairs a tag with the other tags of its block. |
| `textDocument/codeAction` | Quick fixes: insert the missing closer, remove a stray tag or an unexpected expression, close an unterminated tag. |
| `textDocument/semanticTokens/full`, `full/delta`, `range` | GHP syntax only (tag heads, `/>`, import aliases/paths). Go tokens are left to the editor. |
| `$/cancelRequest` | Interrupts a request that is waiting on gopls (answers `RequestCancelled`, -32800). |

Positions are UTF-16 (the LSP default); text sync is incremental. `\n`, `\r\n` and lone `\r` line breaks are understood.

Not provided (yet): references, signature help, rename, workspace symbols, inlay hints. The router already has
references/signature help for gopls; they are not exposed by the server.

## Architecture

```
stdin/stdout ── server::rpc (framing) ── server::Server ──┬── parser / lexer / ast  (GHP structure, diagnostics)
                                                          ├── server::{completion,hover,symbols,folding,format,...}
                                                          └── router::Router ──┬── gopls::Bridge ── gopls (one per Go module)
                                                                               └── scope  (fallback: local names, unused imports)
```

* `lexer`, `parser`, `ast`, `diagnostics`, `line_index`: the tolerant front end. Linear time on any input.
* `gopls/virtual_doc` + `gopls/sourcemap`: builds the virtual `.go` file (package clause from sibling `.go` files,
  the handler function, one statement per tag) and maps positions both ways. Empty tags keep a zero-length
  *slot* so a cursor in `<go= | />` maps.
* `gopls/proxy`: the gopls child processes. One per module root (nearest `go.mod`/`go.work`), started **on a
  background thread**, restarted after a crash with the open documents replayed, abandoned (with one status
  message) after 3 crashes in 60 s. The page is opened in gopls as an overlay `page.ghp.go`; nothing is written to disk.
* `router`: decides whether a position is Go; delegates to gopls, or to `scope` when gopls cannot answer.
* `server`: LSP handlers, debouncing, cancellation, the event loop. A panic inside a handler is caught and answered
  with `InternalError`; malformed params answer `InvalidParams`; requests before `initialize` answer `ServerNotInitialized`.

The event loop is single-threaded for requests (a reader thread frames messages and handles `$/cancelRequest`
immediately; a forwarder thread feeds gopls events in). A Go request waits for gopls at most 10 s.

## Running and debugging

```sh
cargo build --release --locked          # target/release/ghp-lsp (ghp-lsp.exe on Windows)
```

The server logs to **stderr** only (stdout is the protocol). Environment:

| Variable | Effect |
| --- | --- |
| `GHP_GOPLS` | Path of the `gopls` to use. Otherwise: `PATH`, `$GOPATH/bin`, `~/go/bin`. |
| `GHP_LSP_NO_GOPLS=1` | Never start gopls: Go text is answered by the scope analysis only. |

Poke it by hand:

```sh
printf 'Content-Length: 58\r\n\r\n{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | ./target/release/ghp-lsp
```

or drive it from a script (see `tests/common/mod.rs` for a small Rust client, `tests/fake_gopls.rs` for a scripted gopls
stand-in that misbehaves on cue).

## Editors

Any client that can launch a stdio server for `*.ghp`:

* **Lumen**: built in.
* **Neovim** (`vim.lsp`):
  ```lua
  vim.filetype.add({ extension = { ghp = "ghp" } })
  vim.lsp.config("ghp", { cmd = { "ghp-lsp" }, filetypes = { "ghp" }, root_markers = { "go.mod" } })
  vim.lsp.enable("ghp")
  ```
* **Helix** (`languages.toml`):
  ```toml
  [language-server.ghp-lsp]
  command = "ghp-lsp"
  [[language]]
  name = "ghp"
  scope = "text.html.ghp"
  file-types = ["ghp"]
  language-servers = ["ghp-lsp"]
  ```
* **Emacs** (eglot): `(add-to-list 'eglot-server-programs '(ghp-mode "ghp-lsp"))`.
* **VS Code**: use a generic LSP-client extension configured with `ghp-lsp` over stdio for the `ghp` language id.

## Go inside attributes

`<go= ... />` and the block tags are recognized **anywhere in the text**, so Go in attribute values works like Go
anywhere else: hover, completion, definition and diagnostics map correctly.

```html
<a href="<go= url />" class="<go= cls />">x</a>
<input <go:if disabled/>disabled<go:endif/> value="<go= v />">
```

Limits:

* The server has no HTML model: it does not complete attribute names, validate HTML, or know that a position is inside
  an attribute (it only knows tags, Go regions and text).
* A tag ends at the first `>` outside `()`, `[]` and quotes. Parenthesize conditions that contain `>`:
  `<go:if (a > b)/>`.
* A Go string literal inside a quoted attribute value is fine for GHP (`href="<go= "/x" + id />"`) but your editor's HTML
  highlighter may not agree; prefer backticks or single-quoted attributes.
* A semantic-token or hover request on the attribute text *around* a tag is plain markup: no answer.

## Limits

* Every change re-parses the whole document (linear; a 5 MB page parses in well under a second in release builds).
  Diagnostics publishing is debounced. Pages over a few MB work but are not what this is tuned for.
* The bridge locates positions with `\n` line breaks only (CRLF is fine; a file using lone `\r` as line break gets
  scope-based answers but wrong gopls positions).
* One gopls per module root: very large modules make the first Go answer slow (see Troubleshooting).
* gopls `additionalTextEdits` (auto-imports) are dropped: they would edit the virtual file, not the page.

## Troubleshooting

* **No Go completion/hover, only local names**: gopls was not found (the editor shows a one-time message; install with
  `go install golang.org/x/tools/gopls@latest` or set `GHP_GOPLS`), is still loading the module (the first answers come
  from the scope analysis; gopls takes over by itself), or gave up after crashing. Check stderr and `gopls version`.
* **No Go diagnostics**: gopls reports them after it loads the package; they arrive on their own (no edit needed). A page
  must live under a `go.mod`. A parser error on the same tag hides gopls' fallout on it by design.
* **Wrong package errors**: the package clause of the virtual file is taken from the first non-test `.go` file next to
  the page; add one (or fix its `package`) if the page's directory has none.
* **Server not starting**: run `ghp-lsp` in a terminal; it should wait silently for input. On Windows the binary is
  `ghp-lsp.exe`; file URIs with drive letters (`file:///c%3A/...`) are handled.
* **Hangs**: a stuck gopls delays a Go request by at most 10 s and `$/cancelRequest` interrupts it; `GHP_LSP_NO_GOPLS=1`
  rules gopls out.

## Tests

```sh
cargo test --locked                      # unit, parser fuzz, stdio robustness/fuzz, scripted gopls, 5 MB perf guards
cargo test --locked -- --ignored         # with gopls on PATH: real-gopls bridge, router and server tests
```

CI (`.github/workflows/ghp-lsp.yml`): fmt, clippy `-D warnings`, tests on Linux and Windows, and the real-gopls job
(gopls pinned). The Lumen side is covered by `lsp_ghp` in the editor (`.github/workflows/ghp-editor.yml`).
`Cargo.lock` is committed; all builds use `--locked`.
