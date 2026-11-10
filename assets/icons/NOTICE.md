# Icons

- `ui/`, `file/`, `file_types.json`: rasterized alpha masks of Zed's icons (zed-industries/zed, `assets/icons`; the mapping
  comes from `crates/theme/src/icon_theme.rs`). See `ZED_ICONS_LICENSES.txt` (parts derive from Lucide, ISC). The rest of the
  Zed icons follow the Zed repository license — check compatibility before publishing this project.
- `hugeicons/` (solid-rounded, the default for the platform UI: high contrast at 14-16 px) and `hugeicons_bulk/` (bulk-rounded, two-tone, for large decorative icons >= 20 px): masks derived from **HugeIcons Pro**, which this project's owner licenses. The Pro license
  does not allow redistributing the icon set: do NOT publish these files in a public repository or package without
  checking the license (regenerate them locally with `tools/hugeicons-to-svg.mjs` + `tools/gen-icons.py`).

## HugeIcons Pro (not included in this repository)

`assets/icons/hugeicons/` and `assets/icons/hugeicons_bulk/` are git-ignored on purpose: HugeIcons Pro is a
commercial set that may not be redistributed. A fresh clone works without them, because any missing
`hugeicons/<name>` or `hugeicons_bulk/<name>` icon falls back to the bundled `ui/<name>` icon.

If you own a HugeIcons Pro license you can generate the masks locally with your own npm token:

    HUGEICONS_TOKEN=<your token> tools/setup-hugeicons.sh
