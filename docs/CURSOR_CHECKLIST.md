# Mouse cursor checklist

Manual pass for the final run. Start the app with `LUMEN_CURSOR_DEBUG=1`: the window title then
ends with `cursor=<Icon> region=<label>` while the pointer moves, so a wrong shape names the code
path that produced it (`region=tab` is a registry entry, `cascade:<branch>` a legacy branch).

Legend: **Hand** = Pointer, **Beam** = Text, **Arrow** = Default, **Col** / **Row** = resize,
**Grab** / **Grabbing** = drag handles and drags.

Rules: hand on anything that reacts to a click; beam on text inputs and selectable text;
resize arrows on every divider; arrow on dead areas; a modal hides everything below it
(the dimmed backdrop is always an arrow, never the hand of the covered UI); the wait cursor is
used only for blocking work (nothing in the IDE blocks today, so it never shows).

## Window chrome

| Where | Expected |
| --- | --- |
| Title-bar buttons and dropdown anchors (project, branch) | Hand |
| Empty title bar | Arrow |
| Activity rail buttons | Hand; the gaps between them: Arrow |
| Status bar buttons (git, problems, panel toggles) | Hand |
| Status bar read-outs (position, indentation, language, memory) | Arrow (tooltip only) |
| Quick-settings button at the end of the tab bar | Hand |

## Editor card

| Where | Expected |
| --- | --- |
| A tab, and its close button | Hand |
| While dragging a tab (anywhere, also over docks) | Grabbing |
| Empty tab-bar area | Arrow |
| Breadcrumb segments | Hand |
| Text area | Beam |
| Gutter (numbers, folds, breakpoints) | Arrow |
| Ctrl held over an inlay hint with a target | Hand |
| Minimap | Hand |
| Scrollbars (vertical, horizontal) | Arrow |
| Selecting text and leaving the editor with the button held | Beam |
| Split divider | Col (vertical split) / Row (horizontal split) |
| Diff hunk buttons, merge view buttons | Hand |
| Markdown preview links | Hand; text: Beam |
| Image viewer | Grab over the picture, Grabbing while panning |
| SQL / OpenAPI views | Hand on controls, Beam on inputs, Col on column dividers |
| Completion list rows | Hand |
| Hover popup: links/actions Hand, text Beam, rest Arrow | as listed |
| Find bar: field Beam, buttons Hand, rest Arrow | as listed |

## Welcome screen (no file open)

| Where | Expected |
| --- | --- |
| Command rows, recent projects | Hand |
| Everything else on the screen | Arrow (never Beam) |

## Sidebar

| Where | Expected |
| --- | --- |
| Mode tabs | Hand |
| Header controls (filter, new file / folder) | Hand; filter field Beam |
| Rows that exist | Hand |
| Empty tail below the rows | Arrow |
| Right edge (4 px each side) | Col; also while dragging it |
| While dragging a file row | Grabbing |

## Docks (Git | AI)

| Where | Expected |
| --- | --- |
| Left edge of the slot | Col; also while dragging it, wherever the pointer goes |
| Git-AI switch, header buttons, file rows, commit buttons | Hand |
| Commit message, filter | Beam |
| Rest of the Git dock | Arrow |
| AI terminal body | Beam |
| AI header dead area | Arrow |

## Bottom panel

| Where | Expected |
| --- | --- |
| Panel tabs, Clear / Rerun / Stop / Maximize / Close | Hand |
| Output and Terminal text | Beam |
| Tests / Build rows, chips, buttons | Hand; dead area Arrow |
| Debug panel controls | Hand; console and watch inputs Beam |
| Docker panel | Arrow in dead areas (rows and buttons still on the legacy Beam fallback, see below) |
| Terminal tab strip, its close buttons | Hand |

## Modals and menus (open each, then sweep the whole window)

| Where | Expected |
| --- | --- |
| Command palette, quick open, project search: rows | Hand |
| Their input line | Beam |
| Their backdrop and everything under it (rail, title bar, tabs, status bar, docks) | Arrow |
| Prompts (rename, new file ...): input Beam, buttons Hand | as listed |
| Context menu (explorer rows, tabs, commits): items Hand | Hand |
| Context menu: outside the card | Arrow |
| Top-bar dropdowns (project, branch): rows Hand, rest Arrow | as listed |
| Settings modal: toggles, sliders, tabs, selects Hand; text inputs Beam; slider drag Grabbing | as listed |
| Settings modal: everything outside it | Arrow |
| Font picker: rows Hand | Hand |
| Unsaved-changes card, "save as administrator" card: buttons Hand | Hand |

## Floating cards

| Where | Expected |
| --- | --- |
| Toasts | Hand |
| Update / About card: buttons and link Hand, body Arrow | as listed |
| Onboarding tour: its controls Hand, everything else (including all the chrome under it) Arrow | as listed |

## Known gaps (fall back to the legacy cascade; see the report of the cursor agent)

- Disabled controls give Hand like enabled ones: no NotAllowed anywhere yet.
- Docker panel rows and buttons are clickable but the panel's hit list is private, so they show
  Arrow (they used to show Beam).
- Surfaces not yet in the registry (still correct through their own `*_cursor` helper):
  sidebar rows and edge, git / AI docks, terminal, markdown, image, multibuffer, SQL, OpenAPI,
  tests / build / fswatch panels, panel buttons, diff and merge buttons, hover popup, find bar,
  update card, toasts, onboarding, settings.
