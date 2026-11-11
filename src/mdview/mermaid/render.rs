//! Rasterize the shapes of a [`Diagram`] (its labels are drawn by the preview as text).

use super::{Diagram, P, Paint};
use crate::mdimg::Pix;
use crate::theme::Rgba;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

fn path_of(cmds: &[P]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for c in cmds {
        match *c {
            P::M(x, y) => pb.move_to(x, y),
            P::L(x, y) => pb.line_to(x, y),
            P::Q(a, b, x, y) => pb.quad_to(a, b, x, y),
            P::C(a, b, c, d, x, y) => pb.cubic_to(a, b, c, d, x, y),
            P::Z => pb.close(),
        }
    }
    pb.finish()
}

fn paint(c: Rgba) -> tiny_skia::Paint<'static> {
    let mut p = tiny_skia::Paint::default();
    p.set_color(Color::from_rgba8(c.0, c.1, c.2, c.3));
    p.anti_alias = true;
    p
}

/// The diagram's shapes scaled to `w` x `h` device pixels, colors from `col`.
pub(crate) fn raster(d: &Diagram, w: u32, h: u32, col: &dyn Fn(Paint) -> Rgba) -> Option<Pix> {
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return None;
    }
    let mut pm = Pixmap::new(w, h)?;
    let ts = Transform::from_scale(w as f32 / d.w.max(1.0), h as f32 / d.h.max(1.0));
    for s in &d.shapes {
        let Some(path) = path_of(&s.path) else { continue };
        if let Some(f) = s.fill {
            pm.fill_path(&path, &paint(col(f)), FillRule::Winding, ts, None);
        }
        if let Some(c) = s.stroke
            && s.width > 0.0
        {
            let stroke = Stroke {
                width: s.width,
                line_cap: if s.dash { LineCap::Butt } else { LineCap::Round },
                line_join: LineJoin::Round,
                dash: if s.dash { StrokeDash::new(vec![5.0, 4.0], 0.0) } else { None },
                ..Default::default()
            };
            pm.stroke_path(&path, &paint(col(c)), &stroke, ts, None);
        }
    }
    let px = pm
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = p[3] as u32;
            if a == 0 {
                return 0;
            }
            let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255);
            (a << 24) | (un(p[0]) << 16) | (un(p[1]) << 8) | un(p[2])
        })
        .collect();
    Some(Pix { w, h, px })
}
