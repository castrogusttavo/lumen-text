//! GitHub alerts (`> [!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]`, `[!CAUTION]`): a colored
//! left bar, an icon and a title over the quoted blocks, on a faint tint of the same color.

use super::*;

/// Title of alert kind `k` (0 note .. 4 caution).
pub(super) fn title(k: u8) -> &'static str {
    match k {
        0 => "Note",
        1 => "Tip",
        2 => "Important",
        3 => "Warning",
        _ => "Caution",
    }
}

/// Icon of alert kind `k` (from the bundled `ui` set).
pub(super) fn icon(k: u8) -> &'static str {
    match k {
        0 => "ui/info",
        1 => "ui/sparkle",
        2 => "ui/chat",
        3 => "ui/warning",
        _ => "ui/stop",
    }
}

/// GitHub's alert colors: tuned for dark pages, and darker ones for light pages; both are then
/// checked for contrast against the page.
pub(super) fn color(k: u8, bg: Rgba) -> Rgba {
    let dark = bg.luminance() < 0.4;
    let c = match (k, dark) {
        (0, true) => Rgba(0x44, 0x93, 0xf8, 255),
        (1, true) => Rgba(0x3f, 0xb9, 0x50, 255),
        (2, true) => Rgba(0xab, 0x7d, 0xf8, 255),
        (3, true) => Rgba(0xd2, 0x99, 0x22, 255),
        (_, true) => Rgba(0xf8, 0x51, 0x49, 255),
        (0, false) => Rgba(0x09, 0x69, 0xda, 255),
        (1, false) => Rgba(0x1a, 0x7f, 0x37, 255),
        (2, false) => Rgba(0x82, 0x50, 0xdf, 255),
        (3, false) => Rgba(0x9a, 0x67, 0x00, 255),
        (_, false) => Rgba(0xd1, 0x24, 0x2f, 255),
    };
    if c.contrast(bg) >= 3.0 { c } else { c.readable_on(&[bg], 3.0) }
}

impl Lay<'_> {
    /// An alert: bar, icon + title line, then the blocks.
    pub(super) fn alert(&mut self, k: u8, bl: &[Block], x: i32, w: i32, y: i32, st: St) -> i32 {
        let bg = self.ops.len();
        self.op(y, y, Op::Fill { r: Rect::new(x, y, w, 0), col: Col::AlertBg(k), rad: 6 });
        self.op(y, y, Op::Fill { r: Rect::new(x, y, QUOTE_BAR, 0), col: Col::Alert(k), rad: QUOTE_BAR / 2 });
        let (pad_l, pad_r, pad_v) = (QUOTE_INDENT, 14, 10);
        let ty = y + pad_v;
        let lh = LH[BODY];
        self.op(
            ty,
            ty + lh,
            Op::Icon { x: x + pad_l, y: ty + (lh - 16) / 2, size: 16, name: icon(k), col: Col::Alert(k) },
        );
        let t = title(k);
        let tw = self.m.w(self.tx, t, true, BODY).ceil() as i32;
        let lx = x + pad_l + 24;
        self.op(
            ty,
            ty + lh,
            Op::Label { x: lx, w: tw, text: t.into(), col: Col::Alert(k), si: BODY as u8, lh, bold: true },
        );
        let mut y1 = ty + lh;
        if !bl.is_empty() {
            y1 = self.blocks(bl, x + pad_l, w - pad_l - pad_r, y1 + 4, st);
        }
        let y1 = y1 + pad_v;
        for o in self.ops[bg..bg + 2].iter_mut() {
            o.y1 = y1;
            if let Op::Fill { r, .. } = &mut o.op {
                r.h = y1 - y;
            }
        }
        y1
    }
}
