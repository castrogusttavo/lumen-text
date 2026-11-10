//! Icons as alpha masks (`assets/icons/<set>/<name>.a8`, made by `tools/gen-icons.py` from
//! SVGs), scaled to any size and tinted with a theme color. The file-type mapping follows Zed's
//! default icon theme (`assets/icons/file_types.json`).

use crate::{
    icon_pack::Pack,
    theme::Rgba,
    ui::{Canvas, Rect, sc},
};
use std::{collections::HashMap, path::PathBuf};

pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub a: Vec<u8>,
}

#[derive(Default)]
struct FileTypes {
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    icons: HashMap<String, String>,
}

/// Every `hugeicons/<name>` icon the app asks for (including the ones whose name is built at
/// runtime: settings tabs, section list, number steppers). Each must resolve without the
/// HugeIcons Pro masks: either `ui/<name>` exists (authored or from the Zed set) or `ui_alias`
/// maps it to one (checked by the `app_icons_resolve_without_hugeicons` test).
#[cfg(test)]
pub const APP_ICONS: &[&str] = &[
    "branch",
    "check",
    "chevron_down",
    "close",
    "code",
    "copy",
    "cpu",
    "download",
    "edit",
    "keyboard",
    "layout",
    "link",
    "minus",
    "palette",
    "plus",
    "reset",
    "search",
    "settings",
    "terminal",
    "trash",
];

/// Bundled `ui/` icon standing in for a HugeIcons name that the Zed set spells differently.
fn ui_alias(base: &str) -> Option<&'static str> {
    Some(match base {
        "search" => "magnifying_glass",
        "edit" => "pencil",
        "reset" => "rotate_ccw",
        "branch" => "git_branch",
        _ => return None,
    })
}

/// The HugeIcons Pro masks packed into the executable by `build.rs` (official release builds).
#[cfg(embedded_hugeicons)]
fn embedded_pack() -> Option<Pack> {
    Pack::parse(include_bytes!(concat!(env!("OUT_DIR"), "/hugeicons.pack")))
}

#[cfg(not(embedded_hugeicons))]
fn embedded_pack() -> Option<Pack> {
    None
}

pub struct Icons {
    dir: PathBuf,
    pack: Option<Pack>,
    cache: HashMap<String, Option<Mask>>,
    types: Option<FileTypes>,
}

fn parse_mask(bytes: &[u8]) -> Option<Mask> {
    let (&w, rest) = bytes.split_first()?;
    let (&h, a) = rest.split_first()?;
    (a.len() == w as usize * h as usize && w > 0 && h > 0).then(|| Mask { w: w as usize, h: h as usize, a: a.to_vec() })
}

/// Coverage (0..=255) of the mask under the destination pixel (dx, dy) of a `size` x `size` box,
/// averaged over a 3x3 grid of sub-samples (bilinear) so small sizes stay crisp.
pub fn sample(m: &Mask, dx: usize, dy: usize, size: usize) -> u8 {
    let mut acc = 0.0f32;
    for sy in 0..3 {
        for sx in 0..3 {
            let fx = ((dx as f32 + (sx as f32 + 0.5) / 3.0) / size as f32) * m.w as f32 - 0.5;
            let fy = ((dy as f32 + (sy as f32 + 0.5) / 3.0) / size as f32) * m.h as f32 - 0.5;
            let (x0, y0) = (fx.floor().max(0.0) as usize, fy.floor().max(0.0) as usize);
            let (x1, y1) = ((x0 + 1).min(m.w - 1), (y0 + 1).min(m.h - 1));
            let (tx, ty) = ((fx - x0 as f32).clamp(0.0, 1.0), (fy - y0 as f32).clamp(0.0, 1.0));
            let p = |x: usize, y: usize| m.a[y.min(m.h - 1) * m.w + x.min(m.w - 1)] as f32;
            let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
            let bot = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
            acc += top * (1.0 - ty) + bot * ty;
        }
    }
    (acc / 9.0).round().clamp(0.0, 255.0) as u8
}

impl Icons {
    pub fn new() -> Self {
        Self {
            dir: crate::config::assets_dir().join("icons"),
            pack: embedded_pack(),
            cache: HashMap::new(),
            types: None,
        }
    }

    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir, pack: None, cache: HashMap::new(), types: None }
    }

    #[cfg(test)]
    pub fn with_dir_and_pack(dir: PathBuf, pack: Option<Pack>) -> Self {
        Self { pack, ..Self::with_dir(dir) }
    }

    fn mask(&mut self, name: &str) -> Option<&Mask> {
        if !self.cache.contains_key(name) {
            let pack = self.pack.as_ref();
            let loose = |n: &str| std::fs::read(self.dir.join(format!("{n}.a8"))).ok().and_then(|b| parse_mask(&b));
            // Lookup order: the masks embedded in the executable (official builds), loose files in
            // the assets dir (development), then, for HugeIcons names only, the bundled `ui/` set:
            // the Pro masks are not redistributable, so a fresh clone does not have them.
            let m = pack.and_then(|p| parse_mask(p.get(name)?)).or_else(|| loose(name)).or_else(|| {
                let base = name.strip_prefix("hugeicons_bulk/").or_else(|| name.strip_prefix("hugeicons/"))?;
                loose(&format!("ui/{base}")).or_else(|| loose(&format!("ui/{}", ui_alias(base)?)))
            });
            self.cache.insert(name.to_string(), m);
        }
        self.cache.get(name).and_then(|m| m.as_ref())
    }

    /// Draw icon `name` (e.g. "ui/chevron_right", "file/rust") in a `size` px box at (x, y).
    pub fn draw(&mut self, cv: &mut Canvas, name: &str, x: i32, y: i32, size: i32, color: Rgba, clip: Rect) {
        let Some(m) = self.mask(name) else { return };
        // Sampled straight at device resolution (the mask is resampled anyway), so icons stay crisp.
        let (x, y, size) = (sc(x), sc(y), sc(size));
        let clip = cv.to_dev(clip.intersect(&cv.bounds())).intersect(&Rect::new(0, 0, cv.w, cv.h));
        for dy in 0..size {
            let py = y + dy;
            if py < clip.y || py >= clip.bottom() {
                continue;
            }
            for dx in 0..size {
                let px = x + dx;
                if px < clip.x || px >= clip.right() {
                    continue;
                }
                let a = sample(m, dx as usize, dy as usize, size as usize) as u32;
                if a > 0 {
                    cv.dot_dev(px, py, Rgba(color.0, color.1, color.2, (color.3 as u32 * a / 255) as u8));
                }
            }
        }
    }

    fn file_types(&mut self) -> &FileTypes {
        if self.types.is_none() {
            let mut t = FileTypes::default();
            if let Ok(text) = std::fs::read_to_string(self.dir.join("file_types.json"))
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
            {
                let load = |k: &str| -> HashMap<String, String> {
                    v[k].as_object()
                        .map(|o| o.iter().filter_map(|(a, b)| b.as_str().map(|s| (a.clone(), s.to_string()))).collect())
                        .unwrap_or_default()
                };
                t = FileTypes { stems: load("stems"), suffixes: load("suffixes"), icons: load("icons") };
            }
            self.types = Some(t);
        }
        self.types.as_ref().unwrap()
    }

    /// Icon asset for a file name, like Zed: exact stem first, then the longest dotted suffix.
    pub fn file_icon(&mut self, name: &str) -> String {
        let t = self.file_types();
        let key = t.stems.get(name).or_else(|| {
            let mut rest = name;
            while let Some(i) = rest.find('.') {
                rest = &rest[i + 1..];
                if let Some(k) = t.suffixes.get(rest) {
                    return Some(k);
                }
            }
            None
        });
        key.and_then(|k| t.icons.get(k)).cloned().unwrap_or_else(|| "file/file".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without the (git-ignored) HugeIcons directories, every icon name the app uses must still
    /// draw something from the committed `ui/` and `file/` sets.
    #[test]
    fn app_icons_resolve_without_hugeicons() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = std::env::temp_dir().join(format!("lumen-icons-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for set in ["ui", "file"] {
            std::fs::create_dir_all(dir.join(set)).unwrap();
            for e in std::fs::read_dir(root.join("assets/icons").join(set)).unwrap().flatten() {
                std::fs::copy(e.path(), dir.join(set).join(e.file_name())).unwrap();
            }
        }
        let mut ic = Icons::with_dir(dir.clone());
        let mut names: Vec<String> =
            APP_ICONS.iter().flat_map(|n| [format!("hugeicons/{n}"), format!("hugeicons_bulk/{n}")]).collect();
        // Literal names in the sources must be listed above (so the list cannot drift).
        for e in std::fs::read_dir(root.join("src")).unwrap().flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "rs") || path.ends_with("icons.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for pre in ["hugeicons_bulk/", "hugeicons/", "\"ui/", "\"file/"] {
                for (i, _) in text.match_indices(pre) {
                    let rest = &text[i + pre.len()..];
                    let n: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                        .collect();
                    if n.is_empty() || rest[n.len()..].starts_with('{') {
                        continue;
                    }
                    if pre.starts_with("hugeicons") {
                        assert!(APP_ICONS.contains(&n.as_str()), "{}: `{pre}{n}` is not in APP_ICONS", path.display());
                    } else {
                        names.push(format!("{}{n}", pre.trim_start_matches('"')));
                    }
                }
            }
        }
        let missing: Vec<&String> = names.iter().filter(|n| ic.mask(n).is_none()).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(missing.is_empty(), "icons without a bundled mask: {missing:?}");
    }

    #[test]
    fn mask_parsing_and_sampling() {
        assert!(parse_mask(&[2, 2, 1, 2, 3, 4]).is_some());
        assert!(parse_mask(&[2, 2, 1, 2, 3]).is_none(), "length must match w*h");
        let full = Mask { w: 4, h: 4, a: vec![255; 16] };
        assert_eq!(sample(&full, 3, 3, 4), 255);
        assert_eq!(sample(&full, 0, 0, 16), 255, "upscaling keeps full coverage");
        let half = Mask { w: 2, h: 1, a: vec![255, 0] };
        let (l, r) = (sample(&half, 0, 0, 2), sample(&half, 1, 0, 2));
        assert!(l > 200 && r < 60, "{l} {r}");
    }

    #[test]
    fn zed_mapping_and_drawing_with_real_assets() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons");
        let mut ic = Icons::with_dir(dir);
        assert_eq!(ic.file_icon("app.ts"), "file/typescript");
        assert_eq!(ic.file_icon("Button.tsx"), "file/react");
        assert_eq!(ic.file_icon("lib.rs"), "file/rust");
        assert_eq!(ic.file_icon("config.foo.json"), "file/code", "falls back to the longest known suffix");
        assert_eq!(ic.file_icon("whatever.zzzz"), "file/file");
        let mut px = vec![0u32; 32 * 32];
        let mut cv = Canvas { px: &mut px, w: 32, h: 32 };
        ic.draw(&mut cv, "ui/chevron_right", 8, 8, 16, Rgba(255, 255, 255, 255), Rect::new(0, 0, 32, 32));
        assert!(px.iter().any(|p| *p != 0), "something was drawn");
        assert_eq!(px[0], 0, "nothing outside the icon box");
    }

    #[test]
    fn missing_hugeicons_mask_falls_back_to_ui_set() {
        let dir = std::env::temp_dir().join(format!("lumen-icons-fb-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        // 1x1 fully opaque mask: header (w, h as u16 LE style is irrelevant here: reuse a real one).
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/ui/check.a8");
        std::fs::copy(&real, dir.join("ui/check.a8")).unwrap();
        let mut ic = Icons::with_dir(dir.clone());
        assert!(ic.mask("hugeicons/check").is_some(), "falls back to ui/check");
        assert!(ic.mask("hugeicons/does_not_exist").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tiny_pack() -> Pack {
        // 2x2 masks: hugeicons/check is fully opaque, hugeicons_bulk/only_here is half covered.
        let entries = vec![
            ("hugeicons/check".to_string(), vec![2, 2, 255, 255, 255, 255]),
            ("hugeicons_bulk/only_here".to_string(), vec![2, 2, 255, 0, 255, 0]),
        ];
        let blob = crate::icon_pack::pack(&entries);
        let p = Pack::parse(&blob).expect("blob parses");
        assert_eq!(p.len(), 2);
        assert!(Pack::parse(&blob[..blob.len() / 2]).is_none(), "truncated blob is rejected");
        p
    }

    #[test]
    fn embedded_pack_roundtrip_and_lookup_order() {
        let dir = std::env::temp_dir().join(format!("lumen-icons-emb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("hugeicons")).unwrap();
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/ui/check.a8");
        std::fs::copy(&real, dir.join("ui/check.a8")).unwrap();
        std::fs::copy(&real, dir.join("ui/plus.a8")).unwrap();
        // A loose file for a name that is also embedded: the embedded one must win.
        std::fs::write(dir.join("hugeicons/check.a8"), [1u8, 1, 7]).unwrap();
        // A loose file for a name that is not embedded: used (development builds).
        std::fs::write(dir.join("hugeicons/loose.a8"), [1u8, 1, 9]).unwrap();
        let mut ic = Icons::with_dir_and_pack(dir.clone(), Some(tiny_pack()));
        let m = ic.mask("hugeicons/check").unwrap();
        assert_eq!((m.w, m.h, m.a.as_slice()), (2, 2, &[255u8, 255, 255, 255][..]), "embedded beats loose");
        assert_eq!(ic.mask("hugeicons_bulk/only_here").unwrap().a, vec![255, 0, 255, 0]);
        assert_eq!(ic.mask("hugeicons/loose").unwrap().a, vec![9], "loose files come next");
        let ui = ic.mask("hugeicons/plus").unwrap();
        assert!(ui.w > 1, "then the bundled ui/ icon");
        assert!(ic.mask("hugeicons/nope").is_none());
        // Without a pack the loose file is used again.
        let mut ic = Icons::with_dir_and_pack(dir.clone(), None);
        assert_eq!(ic.mask("hugeicons/check").unwrap().a, vec![7]);
        assert!(ic.mask("hugeicons_bulk/only_here").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The licensed masks must never be tracked by git (CI safe: skipped outside a git checkout).
    #[test]
    fn repo_tracks_no_hugeicons_masks() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(out) = std::process::Command::new("git").arg("-C").arg(root).args(["ls-files", "assets/icons"]).output()
        else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let list = String::from_utf8_lossy(&out.stdout);
        let bad: Vec<&str> = list.lines().filter(|l| l.contains("/hugeicons")).collect();
        assert!(bad.is_empty(), "HugeIcons Pro masks must not be committed: {bad:?}");
        assert!(list.lines().any(|l| l.starts_with("assets/icons/ui/")), "sanity: ui icons are tracked");
    }
}
