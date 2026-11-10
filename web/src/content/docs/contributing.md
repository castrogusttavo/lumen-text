---
title: Build from source
description: Build Lumen with Rust, run the tests and use the headless mode.
section: Project
order: 1
---

## Build

Rust stable (edition 2024) and a few system libraries are needed. On Debian or Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev \
  libfontconfig1-dev libclang-dev libonig-dev
./install.sh      # cargo build --release + install into ~/.local
```

## Test

```sh
cargo test
cargo test --release perf -- --nocapture   # the performance budgets
```

## Headless mode

The same input and render code runs without a window and writes PNGs, which is how the editor is tested and how the screenshots on this site were made. Point it at a throwaway configuration so your own settings stay untouched.

```sh
XDG_CONFIG_HOME=~/.cache/lumen-test/cfg XDG_STATE_HOME=~/.cache/lumen-test/state \
  lumen-text --size 900x500 --snap out/ \
  --keys "ctrl+p;type:app;enter;wait:800;f12;wait:500;snap:after" project/ project/src/a.ts
```

Flags: `--keys "<tokens separated by ;>"`, `--snap <dir>`, `--size WxH` (logical pixels) and `--scale <f>`. The full token list is in [CONTRIBUTING.md](https://github.com/castrogusttavo/lumen-text/blob/main/CONTRIBUTING.md#headless-testing).

## Contributing

Issues and pull requests are welcome on [GitHub](https://github.com/castrogusttavo/lumen-text). Every push to `main` that passes CI publishes a release.
