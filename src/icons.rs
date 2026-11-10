//! Icons as alpha masks (`assets/icons/<set>/<name>.a8`, made by `tools/gen-icons.py` from
//! SVGs), scaled to any size and tinted with a theme color. The file-type mapping follows Zed's
//! default icon theme (`assets/icons/file_types.json`).

use crate::{
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

pub struct Icons {
    dir: PathBuf,
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
        Self { dir: crate::config::assets_dir().join("icons"), cache: HashMap::new(), types: None }
    }

    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir, cache: HashMap::new(), types: None }
    }

    fn mask(&mut self, name: &str) -> Option<&Mask> {
        if !self.cache.contains_key(name) {
            let load = |n: &str| std::fs::read(self.dir.join(format!("{n}.a8"))).ok().and_then(|b| parse_mask(&b));
            // HugeIcons Pro masks are not redistributable, so a fresh clone does not have them:
            // fall back to the same-named icon of the bundled `ui/` set.
            let m = load(name).or_else(|| {
                let base = name.strip_prefix("hugeicons_bulk/").or_else(|| name.strip_prefix("hugeicons/"))?;
                load(&format!("ui/{base}"))
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
            if let Ok(text) = std::fs::read_to_string(self.dir.join("file_types.json")) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let load = |k: &str| -> HashMap<String, String> {
                        v[k].as_object().map(|o| o.iter().filter_map(|(a, b)| b.as_str().map(|s| (a.clone(), s.to_string()))).collect()).unwrap_or_default()
                    };
                    t = FileTypes { stems: load("stems"), suffixes: load("suffixes"), icons: load("icons") };
                }
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
}
