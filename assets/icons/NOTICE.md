# Icons

## Bundled icons (included in this repository)

- `ui/`, `file/`, `file_types.json`: alpha masks rasterized from the icon set of
  [Zed](https://github.com/zed-industries/zed) (`assets/icons`; the file-type mapping follows
  `crates/theme/src/icon_theme.rs`). Several of these icons derive from [Lucide](https://lucide.dev) (ISC, see
  `ZED_ICONS_LICENSES.txt`). They are used here by decision of the project maintainer; the upstream icon
  artwork stays the property of its respective authors.
- If you are a rights holder and want an icon removed or credited differently, please open an issue and it will be
  handled promptly.

The masks are generated from SVG by `tools/gen-icons.py` and tinted at runtime, so one file works in every theme.

## HugeIcons Pro (optional, not included)

`assets/icons/hugeicons/` and `assets/icons/hugeicons_bulk/` are git-ignored on purpose: HugeIcons Pro is a
commercial set that may not be redistributed. A fresh clone works without them, because any missing
`hugeicons/<name>` or `hugeicons_bulk/<name>` icon falls back to the bundled `ui/<name>` icon.

If you own a HugeIcons Pro license you can generate the masks locally with your own npm token:

    HUGEICONS_TOKEN=<your token> tools/setup-hugeicons.sh

The generated files stay on your machine and must not be committed or redistributed.
