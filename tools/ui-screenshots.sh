#!/usr/bin/env bash
# Render the key screens of the UI to PNG with the headless script runner (no display, no
# secrets, nothing from the network), once in a dark theme and once in a light one.
#
#   tools/ui-screenshots.sh <out-dir> [path-to-lumen-text]
#
# Every shot is its own process with a throwaway HOME / config / state, so one failing state never
# hides the others; the script reports the ones that did not come out and still exits 0 (the CI
# job is informative only). The headless PNG writer stores raw pixels: run `optipng` on the files
# before committing any of them.
set -u

out="${1:?usage: ui-screenshots.sh <out-dir> [binary]}"
bin="${2:-target/release/lumen-text}"
bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

SIZE=1200x760
THEMES=("dark:dracula" "light:github-light-default")

# ── a small project: Rust, TypeScript, Markdown, an OpenAPI spec and a conflicted file ────────
proj="$work/demo"
mkdir -p "$proj/src" "$proj/web"
cat > "$proj/src/main.rs" <<'EOF'
use std::collections::HashMap;

/// A tiny inventory: item name to count.
struct Stock {
    items: HashMap<String, u32>,
}

impl Stock {
    fn new() -> Self {
        Self { items: HashMap::new() }
    }

    fn add(&mut self, name: &str, n: u32) -> u32 {
        let slot = self.items.entry(name.to_string()).or_insert(0);
        *slot += n;
        *slot
    }
}

fn main() {
    let mut stock = Stock::new();
    println!("{}", stock.add("bolt", 12));
    let missing: u32 = "oops";
}
EOF
cat > "$proj/web/todo.ts" <<'EOF'
export interface Todo {
  id: number;
  title: string;
  done: boolean;
}

export function toggle(list: Todo[], id: number): Todo[] {
  return list.map((t) => (t.id === id ? { ...t, done: !t.done } : t));
}

const sample: Todo[] = [
  { id: 1, title: "Write the docs", done: false },
  { id: 2, title: "Ship it", done: true },
];

console.log(toggle(sample, 1));
EOF
cat > "$proj/README.md" <<'EOF'
# Demo project

A short **Markdown** file for the reader.

- [x] Rust
- [x] TypeScript
- [ ] More

| Tool | Use |
| --- | --- |
| `cargo` | build |
| `tsc` | check |

```rust
fn main() { println!("hello"); }
```

See the [docs](https://example.com/docs).
EOF
cat > "$proj/openapi.yaml" <<'EOF'
openapi: 3.0.3
info:
  title: Pet shop
  version: 1.0.0
servers:
  - url: https://api.example.com/v1
paths:
  /pets:
    get:
      summary: List pets
      operationId: listPets
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
      responses:
        "200":
          description: A list of pets
  /pets/{id}:
    get:
      summary: Get one pet
      operationId: getPet
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      responses:
        "200":
          description: The pet
        "404":
          description: Not found
EOF
printf 'one\ntwo\nthree\nfour\n' > "$proj/notes.txt"

g() { git -C "$proj" -c user.name=Demo -c user.email=demo@example.com -c commit.gpgsign=false "$@" >/dev/null 2>&1; }
g init -q -b main
g add -A
g commit -q -m "Initial commit"
g checkout -q -b feature
printf 'one\nTWO from feature\nthree\nfour\n' > "$proj/notes.txt"
g commit -q -am "Feature change"
g checkout -q main
printf 'one\nTWO from main\nthree\nfour\n' > "$proj/notes.txt"
g commit -q -am "Main change"
g merge feature || true      # leaves notes.txt conflicted
printf 'one\nchanged line\nthree\nfour\n' > "$proj/web/extra.txt"

failed=()

# shot <theme-name> <label> <first-run?> <keys> <files...>: one process, one throwaway profile.
shot() {
  local theme="$1" label="$2" first="$3" keys="$4"
  shift 4
  local home="$work/home-$label"
  rm -rf "$home"
  mkdir -p "$home/c/lumen-text" "$home/s" "$home/d"
  if [ "$first" != "first" ]; then
    printf '{"theme":"%s","updates":"off"}\n' "$theme" > "$home/c/lumen-text/config.json"
  fi
  (
    cd "$proj" || exit 1
    HOME="$home" XDG_CONFIG_HOME="$home/c" XDG_STATE_HOME="$home/s" XDG_DATA_HOME="$home/d" \
      LUMEN_REDUCE_MOTION=1 timeout 120 "$bin" --size "$SIZE" --snap "$out" --keys "$keys" "$@" \
      > "$work/log-$label.txt" 2>&1
  )
  if [ ! -s "$out/$label.png" ]; then
    failed+=("$label")
    echo "::warning::no screenshot for $label"
    tail -n 5 "$work/log-$label.txt" || true
  fi
}

for entry in "${THEMES[@]}"; do
  mode="${entry%%:*}"
  theme="${entry#*:}"
  p="$mode"

  shot "$theme" "$p-welcome" "" "wait:900;snap:$p-welcome" .
  # No config file yet: the tour opens by itself; "right" goes to step 2.
  shot "$theme" "$p-onboarding-2" first "wait:900;right;wait:700;snap:$p-onboarding-2" .
  shot "$theme" "$p-editor-rust" "" "wait:1500;snap:$p-editor-rust" src/main.rs
  shot "$theme" "$p-editor-typescript" "" "wait:1500;snap:$p-editor-typescript" web/todo.ts
  shot "$theme" "$p-sidebar" "" "wait:900;ctrl+shift+e;wait:400;snap:$p-sidebar" src/main.rs web/todo.ts
  shot "$theme" "$p-git-dock" "" "wait:900;ctrl+shift+g;wait:1500;snap:$p-git-dock" src/main.rs
  shot "$theme" "$p-ai-dock" "" "wait:900;ctrl+shift+a;wait:900;snap:$p-ai-dock" src/main.rs
  shot "$theme" "$p-panel-problems" "" \
    "wait:1200;ctrl+shift+p;wait:300;type:View: Problems;wait:300;enter;wait:900;snap:$p-panel-problems" src/main.rs
  shot "$theme" "$p-panel-output" "" "wait:900;ctrl+j;wait:700;snap:$p-panel-output" src/main.rs
  shot "$theme" "$p-palette" "" "wait:900;ctrl+shift+p;wait:300;type:git;wait:500;snap:$p-palette" src/main.rs
  shot "$theme" "$p-settings" "" "wait:900;ctrl+,;wait:700;snap:$p-settings" src/main.rs
  shot "$theme" "$p-context-menu" "" "wait:1200;rclick:120,130;wait:500;snap:$p-context-menu" src/main.rs web/todo.ts
  shot "$theme" "$p-openapi" "" \
    "wait:900;ctrl+shift+p;wait:300;type:OpenAPI: Open as API explorer;wait:300;enter;wait:1200;snap:$p-openapi" openapi.yaml
  shot "$theme" "$p-markdown-preview" "" "wait:900;ctrl+shift+v;wait:900;snap:$p-markdown-preview" README.md
  shot "$theme" "$p-merge" "" \
    "wait:900;ctrl+shift+p;wait:300;type:Git: Open merge editor;wait:300;enter;wait:1200;snap:$p-merge" notes.txt
done

count="$(find "$out" -name '*.png' | wc -l)"
echo "ui-screenshots: $count PNG files in $out"
if [ "${#failed[@]}" -gt 0 ]; then
  echo "missing: ${failed[*]}"
fi
exit 0
