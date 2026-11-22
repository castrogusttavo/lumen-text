# lumen-text — design system ("UI redesign 9")

Goal: a calm, professional, developer-first interface. Every decision serves the person typing
code for eight hours: low noise, fast feedback, nothing jumps, nothing blocks. References live in
`examples/` (floating cards on a dark canvas, soft glow borders, a mode switch in the title bar,
a dock on the right, friendly empty states).

Hard rules (from the maintainer):
- 100% native Rust drawing (tiny-skia / cosmic-text / our `Canvas`). **No webview, no JS engine.**
- Icons: **HugeIcons only**, through `icons.rs` / `icon_pack.rs` (`tx.icon(...)`). No ad-hoc glyph art.
- The AI panel is the **real CLI terminal (PTY)**. No native chat, no composer of our own, no second mode.
  The redesign only restyles the chrome around it (header, tabs, empty/starting state, dock frame).
- Colors always come from `theme::Chrome` / the active theme (light and dark must both work, and
  every text/background pair must keep the contrast the theme code already enforces).
- Geometry always comes from `ds.rs`; motion always from `anim.rs`. No magic numbers.

## 1. Shell layout
The window is a canvas (`theme.background`, slightly darker than the cards) carrying **floating
cards** separated by `ds::GAP`, each with `R_CARD` corners and a hairline border:
`[ rail ][ sidebar card ][ editor card ][ dock card (Git | AI) ]`, title bar on top, status bar
below. The editor card is the hero: biggest, brightest, the only one with a subtle shadow
(`Elev::Card`). Popovers use `Elev::Pop`, the command palette and modals `Elev::Modal` with a
dimmed, softly faded backdrop. `CARD_M` (currently 0) becomes `ds::GAP`; zen mode removes the gaps.

## 2. Motion (see `anim.rs`)
- Anything that appears or disappears eases: dock open/close (`dur::SLOW`), modal in (fade + 8 px
  rise, `dur::SLOW`), menus/tooltips (`dur::BASE`), hover/press/focus (`Hot`, `dur::FAST`).
- Tab switch: indicator slides to the new tab. List selection: highlight glides between rows.
- Loading uses a shimmer/pulse, never a frozen UI. Toasts slide in, wait, fade.
- Everything is driven by `dt` and reported through the `busy` flag so an idle editor sleeps.
- Respect "reduce motion" (config `reduce_motion`, default follows the OS when known): `Anim::snap`.
- Never animate text being typed, the caret blink path, or scrolling of code (already tuned).

## 3. Components (reuse, do not fork)
- Buttons: primary (accent fill, `on_accent` text), secondary (widget bg + border), ghost (hover bg only),
  icon button (`HIT_MIN` square, `R_CTRL`). Every one has hover, pressed, focus-visible and disabled looks.
- Inputs: `input_bg`, `R_CTRL`, accent ring on focus (animated), placeholder in muted text.
- Rows: `ROW_LIST` tall, `R_ROW` highlight with 4 px inset, accent bar only for the *selected* row.
- Tooltips: 400 ms delay, name + shortcut chip. Keycaps drawn as small rounded chips.
- Badges/counters: pill, `R_CTRL`, 10–11 px text.
- Empty states: icon, one-line title, one-line hint, one primary action. Never a blank panel.

## 4. Developer-experience principles
1. **Keyboard first, mouse friendly.** Every clickable thing shows its shortcut in its tooltip; every
   new surface is reachable from the command palette.
2. **Show state, don't hide it.** Running/failed/dirty/ahead-behind/LSP-ready are visible at a glance in
   the status bar and tabs, with one click to the detail.
3. **Predictable.** No layout shift on hover or load; panels remember size and visibility per project.
4. **Forgiving.** Destructive actions confirm or offer Undo (toast with Undo), never silently.
5. **Fast.** A frame must stay well under 8 ms; skip work while nothing is animating.
6. **Right cursor everywhere.** Pointer on clickable, text on inputs, resize on drag edges, grab on drags.
7. **Accessible.** Contrast ≥ 4.5:1 for text, visible focus, hit targets ≥ 24 px.

## 5. Modules and what "done" looks like
1. **Onboarding & welcome** — 5-step full-window first run (welcome → theme with live preview → keymap
   → font/scale → pick a folder or clone), animated transitions between steps (slide + fade), progress
   dots, Skip always visible, fully keyboard driven; the welcome screen becomes a launchpad
   (recent projects with last-opened, quick actions with shortcuts, tips) with a staggered entrance.
2. **Shell** — title bar (project picker, menus, run/config cluster, window buttons), rail, status bar,
   floating-card layout, dock frame with animated open/close and tab pill slider (Git | AI).
3. **Explorer / sidebar** — tree rows, file icons, git decorations, hover/selection glide, inline rename,
   empty state, search field header, collapsible sections.
4. **Editor** — tabs (pill, slider indicator, dirty dot, close on hover, overflow menu), breadcrumbs,
   gutter, diagnostics summary chip (errors/warnings in the corner), find bar, minimap, inlay hints
   chips, zen mode, split view chrome, scrollbars that fade in/out.
5. **Git** — changes list with staged/unstaged groups, commit box, history/graph, diff chrome,
   branch picker, sync buttons with ahead/behind counters and progress, empty states.
6. **AI / agents** — dock tab, agent picker header, session tabs, "starting…" skeleton, empty state with
   suggested prompts that type into the PTY, status chip (running/idle), real terminal below untouched.
7. **Overlays & modals** — command palette, quick open, settings, shortcuts, menus, context menus, dialogs,
   toasts with actions, tooltips, confirmation sheets, update banner.
8. **Terminal / bottom panel, debug, tests, problems** — tab strip, toolbar, empty states.

## 6. Done criteria for each module PR
- Uses `ds` + `anim` + theme colors only; HugeIcons only; correct cursor/hover on every new hit region.
- Light and dark themes both checked via the headless screenshot runner (`--script`, PNG attached to the PR).
- Existing tests still pass; new logic (state machines, layout math) has unit tests.
- CI green on the PR (fmt, clippy -D warnings, build, tests). No local cargo build/test.
