# Contributing to lumen-text

Thanks for helping. lumen-text is a small, fast, memory-conscious editor; contributions that keep it that way are the most welcome.
By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Contributions are licensed under the [MIT License](LICENSE).

## Development setup

1. Install the Rust stable toolchain (edition 2024) with [rustup](https://rustup.rs).
2. Install the system packages CI uses (Debian/Ubuntu names):

   ```sh
   sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev \
     libfontconfig1-dev libclang-dev libonig-dev
   ```

3. Optional: `tsgo` for the TypeScript tests and features
   (`npm i --prefix ~/.local/share/lumen-text/tsgo @typescript/native-preview`); tests that need it skip themselves when it is absent.
4. Clone, then build and run:

   ```sh
   cargo build --release
   cargo run --release -- path/to/project path/to/file.ts:10:3
   ```

`config::assets_dir()` finds `assets/` through `$LUMEN_ASSETS`, then next to the binary, then `~/.local/share/lumen-text/assets`;
when running from the repository set `LUMEN_ASSETS=$PWD/assets` if the editor cannot find its grammars.

## Build and test

```sh
cargo build --release
cargo test --release                          # ~500 tests (includes an end-to-end test against the real tsgo if installed)
cargo test --release perf -- --nocapture --test-threads=1   # performance regression tests (CI: informational)
cargo test --release name_of_test             # one test: much cheaper while iterating
```

Use `--release`: debug builds of the tokenizer and renderer are far too slow for the tests.

### Icons and HugeIcons Pro

You do **not** need any key to build, test or send a pull request: without HugeIcons Pro the app uses the bundled
Zed-based icons (`assets/icons/ui`, `file`). Never commit `assets/icons/hugeicons*` (git-ignored; a test fails if any
such file is tracked). If you own your own HugeIcons Pro licence you can generate the masks locally with
`HUGEICONS_TOKEN=<your npm token> tools/setup-hugeicons.sh`; `build.rs` then embeds them in the binary you build.
Official release builds in CI do the same with the maintainer's licence (secret `HUGEICONS_TOKEN`); on pull requests
from forks the secret is absent and the step is skipped. The masks are embedded in the executable and packages never
contain them as loose files (see `assets/icons/NOTICE.md`).

## Headless testing

The headless mode runs the real key-handling and render code without a window and writes PNGs, so UI work can be verified and
regression-tested without a display:

```sh
lumen-text --size 900x500 --snap out/ \
  --keys "ctrl+p;type:app;enter;wait:800;f12;wait:500;snap:after" project/ project/src/a.ts
```

Always point it at a throwaway configuration so your own settings and session are untouched:

```sh
XDG_CONFIG_HOME=$HOME/.cache/lumen-test/cfg XDG_STATE_HOME=$HOME/.cache/lumen-test/state lumen-text --keys "..." ...
```

Flags: `--keys`, `--snap <dir>` (default `/tmp/lumen-snap`), `--size WxH` (logical pixels), `--scale <factor>`. Tokens in `--keys` are separated by `;` and run in order (`src/script.rs`):

| Token | Effect |
|---|---|
| `ctrl+shift+p`, `alt+left`, `f12`, `shift+a` | A key combo (`ctrl`, `shift`, `alt` modifiers). Named keys: `enter esc tab backspace delete up down left right home end pageup pagedown space f2 f3 f5 f8 f9 f10 f11 f12`; any single character; a literal plus is `ctrl++` |
| `type:text` | Type text character by character (spaces included; `;` cannot be typed) |
| `wait:ms` | Let background work (tokenizer, LSP, watcher) run for `ms` and render a frame |
| `snap:name` | Render until settled and write `<snap dir>/name.png` (also prints memory) |
| `full` | Render until nothing is pending and print the frame count and time |
| `click:x,y` / `hover:x,y` | Left click / move the pointer at logical pixel `x,y` |
| `rclick:x,y` | Right click (context menu) |
| `down:x,y`, `move:x,y`, `up` | Press and drag with the left button, move the pointer, release |
| `drag:x,y,x2,y2` | Press at one point, move to another, release (scrollbars, dividers) |
| `wheel:dx,dy[,x,y]` / `pwheel:dx,dy[,x,y]` | Wheel in notches / in pixels (positive = down/right) |
| `wheel-end` | End a wheel gesture |
| `mods:ctrl+shift` | Keep modifiers held for the next mouse/wheel tokens |
| `still` / `frames:N` | Render until animations settle / N frames; prints frame count and scroll state |
| `state` | Print cursor, selection and scroll state |
| `sh:command` | Run a shell command (for example to edit a file or run `git` mid-script) |

Tests in `src/` use the same building blocks (`App`, `Editor`, `Buffer`) directly; prefer a unit test for logic and a headless script for layout and visuals.
Tests must not write to `/tmp` or your real config: use `std::env::temp_dir()` subfolders that they clean up, or `CARGO_TARGET_TMPDIR`.

## Code style

- `cargo fmt`-style formatting and warning-free `cargo build --release`; follow the surrounding code (the project uses long lines in places, do not reformat unrelated code).
- Keep logic pure and testable (see `editor.rs`, `rowmap.rs`, `config.rs`); keep drawing and input glue in `app*.rs`.
- Be careful with memory: no per-frame allocations in hot paths, no caching whole files or LSP responses without a bound, no new heavy dependencies (check the binary size and `RAM` readout before and after).
- New dependencies need a clear reason; use `default-features = false` and enable only what is needed. Update `THIRD_PARTY_LICENSES.md` when dependencies change.
- Comments explain *why*; doc comments (`//!`, `///`) describe modules and non-obvious behavior.
- User-facing strings are Portuguese today; an English UI is planned. Keep strings in one place per feature so they are easy to translate.

## How to add things

**A command.** Add a variant to `enum Cmd` and a row `("Label", "shortcut text", Cmd::X)` to `COMMANDS` in `src/app_overlay.rs`, then handle it in `App::run_cmd`.
The palette, fuzzy search and the shortcut modal pick it up automatically.

**A shortcut.** Add `("ctrl+shift+x", "VariantName")` to `DEFAULTS` in `src/keymap.rs` (the first row for a command is its main default and must match the palette text; later rows are aliases).
A test checks that ids exist, combos are unique and the palette text agrees. Chords use a space (`ctrl+k r`); explorer-only keys use `explorer:`.

**A setting.** Add a field to `Config` in `src/config.rs` with its default in `Default`, parse it in `load_from` (validate and clamp), write it in `save_to`, extend the round-trip test, and add a row in `src/settingsui.rs` so it shows in the settings modal. Document it in the README table.

**A theme.** Bundled themes are VS Code JSON in `assets/themes/` and listed through `assets/language-manifest.json`; the test suite loads every theme and checks WCAG AA text pairs, so run `cargo test --release theme`.
Users can add themes without code in `~/.config/lumen-text/themes/` (see README). Zed conversion lives in `src/zedtheme.rs`.

## Pull request checklist

- [ ] `cargo build --release` has no new warnings and `cargo test --release` passes
- [ ] New behavior has a test (unit or headless) and, for UI changes, a headless screenshot you looked at (light and dark if colors are involved)
- [ ] Memory and speed checked for anything on the hot path (typing, scrolling, tokenizing, LSP)
- [ ] README, `ROADMAP.md` and `THIRD_PARTY_LICENSES.md` updated when relevant
- [ ] No generated files, secrets, HugeIcons Pro assets or large binaries committed
- [ ] One focused change per PR with a clear description

## Commit messages

Short imperative summary (about 70 characters), no trailing period, optional body explaining why. Examples from the history:
`Add LineTrailingText: ghost text after a line`, `Make ghost text after a long line scrollable`, `Skip the color-emoji test when no color emoji font is installed`.

## Releases

Maintainers do not tag by hand. Every push to `main` that passes `.github/workflows/ci.yml` triggers:
`test` (build + tests) -> `version` (CalVer `YYYY.M.D.N`: today's UTC date plus the number of that day's tags + 1) -> `build-linux` (binary stamped with `LUMEN_VERSION`, `.tar.gz`, `.deb`, `.rpm`, AppImage, Arch package) -> `release` (`SHA256SUMS`, GitHub Release `vYYYY.M.D.N` with generated notes).
`Cargo.toml`'s `version` is only the fallback for local builds. Packaging details: [packaging/README.md](packaging/README.md). Pull requests only run build, tests and the perf job.

### Auto-update: signing, secrets and `latest.json`

The in-app updater (`src/update.rs`, `src/app_update.rs`; user docs in the README) trusts exactly one thing: a [minisign](https://jedisct1.github.io/minisign/) signature made with the release key.

- **Public key**: `src/update_key.pub`, embedded in the binary with `include_str!`. It may contain several keys (concatenated `.pub` files); a signature is accepted when it was made by any of them.
- **Secret key**: only in the GitHub Actions secret `MINISIGN_SECRET_KEY` (repository secret, the content of the key file, generated without a password). Never commit it (`*.key` is git-ignored) and never keep a copy on disk.
- The `release` job installs `minisign`, signs every `*.AppImage`, `*.tar.gz`, Windows `*.zip` and macOS `*.dmg` with the trusted comment `lumen-text <version> <file name>` (the client requires exactly that, so an old signed file can't be replayed as a new release), verifies each signature against `src/update_key.pub` (a wrong secret fails the release instead of bricking updates), then writes and uploads `latest.json` plus the `.minisig` files next to the other release files. The release notes are generated once (`gh api .../releases/generate-notes`) and used both for the GitHub release and, truncated, in `latest.json`.
- The app reads `https://github.com/castrogusttavo/lumen-text/releases/latest/download/latest.json`.

`latest.json` (modeled on Tauri's updater manifest):

```json
{
  "version": "2026.11.10.1",
  "notes": "## What's Changed\n* ...",
  "pub_date": "2026-11-10T12:00:00Z",
  "platforms": {
    "linux-x86_64-appimage": { "url": "https://github.com/castrogusttavo/lumen-text/releases/download/v2026.11.10.1/lumen-text-2026.11.10.1-x86_64.AppImage", "signature": "<base64 of the .minisig file>" },
    "linux-x86_64-tarball":  { "url": ".../lumen-text-2026.11.10.1-linux-x86_64.tar.gz", "signature": "<base64 of the .minisig file>" },
    "linux-x86_64-deb":      { "url": ".../lumen-text_2026.11.10.1-1_amd64.deb", "signature": "" },
    "linux-x86_64-rpm":      { "url": ".../lumen-text-2026.11.10.1-1.x86_64.rpm", "signature": "" },
    "linux-x86_64-arch":     { "url": ".../lumen-text-bin-2026.11.10.1-1-x86_64.pkg.tar.zst", "signature": "" },
    "windows-x86_64-zip":    { "url": ".../lumen-text-2026.11.10.1-windows-x86_64.zip", "signature": "<base64 of the .minisig file>" },
    "macos-aarch64-dmg":     { "url": ".../lumen-text-2026.11.10.1-macos-aarch64.dmg", "signature": "<base64 of the .minisig file>" },
    "macos-x86_64-dmg":      { "url": ".../lumen-text-2026.11.10.1-macos-x86_64.dmg", "signature": "<base64 of the .minisig file>" }
  }
}
```

`version` is the CalVer without `v`. `appimage` and `tarball` entries (also `linux-aarch64-*` when those artifacts exist) are the installable, signed ones; the `windows-*`/`macos-*` entries are signed but the app only announces them (link to the release page; no in-place update yet); `deb`/`rpm`/`arch` entries are informational (the app shows the install command, it never modifies a system-managed install).

**Rotating the key** (compromised key, or routine). The app only trusts the keys compiled into it, so rotate in two releases:

1. Generate a new pair: `minisign -G -W -p new.pub -s new.key` (any machine with minisign, or `docker run --rm -v "$PWD:/k" ubuntu:24.04 bash -c 'apt-get update -qq && apt-get install -y -qq minisign && minisign -G -W -p /k/new.pub -s /k/new.key'`).
2. Append the new public key to `src/update_key.pub` (keep the old one) and merge. This release is still signed with the **old** secret, so installed apps accept it, and from now on they also trust the new key.
3. After enough users updated (wait a few weeks), run `gh secret set MINISIGN_SECRET_KEY -R castrogusttavo/lumen-text < new.key`, then shred `new.key` (`shred -u new.key`). The next release is signed with the new key; apps that skipped step 2 can't verify it and must be reinstalled manually.
4. Later, drop the old public key from `src/update_key.pub`.

If the old key leaked, do steps 2 and 3 immediately and say in the release notes that older builds need a manual reinstall.

Testing the updater needs no real release: `tools/update-e2e.sh [binary]` starts a local `python3 -m http.server`, signs fake releases with a throwaway key (minisign or docker) and drives `--check-update`, `--update`, `--install-user` and the headless UI (auto/notify/off, card, restart) against it, under `~/.cache/lumen-e2e`. Unit tests cover versions, schedule, manifest parsing, signatures and the install steps (`cargo test --release update`).

## Resource rules for contributors

The editor targets about 100 MB of RAM, and so should your workflow on small machines:

- Do not spam cargo: build once, run targeted tests (`cargo test --release <name>`), and avoid parallel `cargo` invocations. Use `CARGO_BUILD_JOBS=2` on low-memory machines.
- Keep scratch files (screenshots, sample projects, test configs) out of RAM-backed `/tmp`; use a folder under `~/.cache`.
- Do not leave editor, `tsgo` or terminal processes running after a headless session.
- Watch the status bar `RAM` readout (or `[mem]` lines from headless runs) when you touch rendering, tokenizing or the LSP client.
