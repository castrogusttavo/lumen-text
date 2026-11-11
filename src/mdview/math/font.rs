//! The embedded math font (STIX Two Math, SIL OFL 1.1): its OpenType MATH constants, glyph
//! metrics, size variants / assemblies of stretchy glyphs, and outlines for the rasterizer.
//!
//! The font file is deflated into the executable by `build.rs` and inflated on first use (only
//! documents with math pay for it).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use ttf_parser::{Face, GlyphId, math};

static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/math-font.z"));

fn data() -> Option<&'static [u8]> {
    static DATA: OnceLock<Option<&'static [u8]>> = OnceLock::new();
    *DATA.get_or_init(|| {
        let raw = miniz_oxide::inflate::decompress_to_vec(PACKED).ok()?;
        Some(&*Box::leak(raw.into_boxed_slice()))
    })
}

thread_local! {
    static FONT: Option<MFont> = data().and_then(MFont::new);
}

/// Run `f` with the math font (`None` when it cannot be loaded).
pub fn with_font<T>(f: impl FnOnce(&MFont) -> T) -> Option<T> {
    FONT.with(|m| m.as_ref().map(f))
}

/// The MATH table constants used by the typesetter, in font units (percentages as fractions).
#[derive(Clone, Debug)]
pub struct Consts {
    pub sp: f32,
    pub ssp: f32,
    pub display_op_min: f32,
    pub axis: f32,
    pub accent_base: f32,
    pub sub_down: f32,
    pub sub_top_max: f32,
    pub sub_drop_min: f32,
    pub sup_up: f32,
    pub sup_up_cramped: f32,
    pub sup_bottom_min: f32,
    pub sup_drop_max: f32,
    pub subsup_gap_min: f32,
    pub sup_bottom_max_with_sub: f32,
    pub space_after_script: f32,
    pub upper_gap_min: f32,
    pub upper_rise_min: f32,
    pub lower_gap_min: f32,
    pub lower_drop_min: f32,
    pub stack_up: f32,
    pub stack_up_d: f32,
    pub stack_down: f32,
    pub stack_down_d: f32,
    pub stack_gap: f32,
    pub stack_gap_d: f32,
    pub num_up: f32,
    pub num_up_d: f32,
    pub den_down: f32,
    pub den_down_d: f32,
    pub num_gap: f32,
    pub num_gap_d: f32,
    pub frac_rule: f32,
    pub den_gap: f32,
    pub den_gap_d: f32,
    pub overbar_gap: f32,
    pub overbar_rule: f32,
    pub overbar_extra: f32,
    pub underbar_gap: f32,
    pub underbar_rule: f32,
    pub underbar_extra: f32,
    pub rad_gap: f32,
    pub rad_gap_d: f32,
    pub rad_rule: f32,
    pub rad_extra: f32,
    pub rad_kern_before: f32,
    pub rad_kern_after: f32,
    pub rad_raise: f32,
}

impl Consts {
    fn read(c: &math::Constants) -> Consts {
        Consts {
            sp: c.script_percent_scale_down() as f32 / 100.0,
            ssp: c.script_script_percent_scale_down() as f32 / 100.0,
            display_op_min: c.display_operator_min_height() as f32,
            axis: c.axis_height().value as f32,
            accent_base: c.accent_base_height().value as f32,
            sub_down: c.subscript_shift_down().value as f32,
            sub_top_max: c.subscript_top_max().value as f32,
            sub_drop_min: c.subscript_baseline_drop_min().value as f32,
            sup_up: c.superscript_shift_up().value as f32,
            sup_up_cramped: c.superscript_shift_up_cramped().value as f32,
            sup_bottom_min: c.superscript_bottom_min().value as f32,
            sup_drop_max: c.superscript_baseline_drop_max().value as f32,
            subsup_gap_min: c.sub_superscript_gap_min().value as f32,
            sup_bottom_max_with_sub: c.superscript_bottom_max_with_subscript().value as f32,
            space_after_script: c.space_after_script().value as f32,
            upper_gap_min: c.upper_limit_gap_min().value as f32,
            upper_rise_min: c.upper_limit_baseline_rise_min().value as f32,
            lower_gap_min: c.lower_limit_gap_min().value as f32,
            lower_drop_min: c.lower_limit_baseline_drop_min().value as f32,
            stack_up: c.stack_top_shift_up().value as f32,
            stack_up_d: c.stack_top_display_style_shift_up().value as f32,
            stack_down: c.stack_bottom_shift_down().value as f32,
            stack_down_d: c.stack_bottom_display_style_shift_down().value as f32,
            stack_gap: c.stack_gap_min().value as f32,
            stack_gap_d: c.stack_display_style_gap_min().value as f32,
            num_up: c.fraction_numerator_shift_up().value as f32,
            num_up_d: c.fraction_numerator_display_style_shift_up().value as f32,
            den_down: c.fraction_denominator_shift_down().value as f32,
            den_down_d: c.fraction_denominator_display_style_shift_down().value as f32,
            num_gap: c.fraction_numerator_gap_min().value as f32,
            num_gap_d: c.fraction_num_display_style_gap_min().value as f32,
            frac_rule: c.fraction_rule_thickness().value as f32,
            den_gap: c.fraction_denominator_gap_min().value as f32,
            den_gap_d: c.fraction_denom_display_style_gap_min().value as f32,
            overbar_gap: c.overbar_vertical_gap().value as f32,
            overbar_rule: c.overbar_rule_thickness().value as f32,
            overbar_extra: c.overbar_extra_ascender().value as f32,
            underbar_gap: c.underbar_vertical_gap().value as f32,
            underbar_rule: c.underbar_rule_thickness().value as f32,
            underbar_extra: c.underbar_extra_descender().value as f32,
            rad_gap: c.radical_vertical_gap().value as f32,
            rad_gap_d: c.radical_display_style_vertical_gap().value as f32,
            rad_rule: c.radical_rule_thickness().value as f32,
            rad_extra: c.radical_extra_ascender().value as f32,
            rad_kern_before: c.radical_kern_before_degree().value as f32,
            rad_kern_after: c.radical_kern_after_degree().value as f32,
            rad_raise: c.radical_degree_bottom_raise_percent() as f32 / 100.0,
        }
    }
}

/// One piece of a glyph assembly (bottom to top / left to right).
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub g: u16,
    pub adv: f32,
    pub ext: bool,
}

pub struct MFont {
    face: Face<'static>,
    math: math::Table<'static>,
    pub upem: f32,
    pub c: Consts,
    pub min_overlap: f32,
    /// Ink boxes (`x0, y0, x1, y1`, y up): CFF outlines are walked to get them, so they are cached.
    boxes: RefCell<HashMap<u16, [f32; 4]>>,
}

/// Collects a glyph outline into a tiny-skia path.
struct Sink(tiny_skia::PathBuilder);

impl ttf_parser::OutlineBuilder for Sink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

impl MFont {
    fn new(data: &'static [u8]) -> Option<MFont> {
        let face = Face::parse(data, 0).ok()?;
        let math = face.tables().math?;
        let c = Consts::read(&math.constants?);
        let min_overlap = math.variants.map_or(0.0, |v| v.min_connector_overlap as f32);
        let upem = face.units_per_em() as f32;
        Some(MFont { face, math, upem, c, min_overlap, boxes: RefCell::new(HashMap::new()) })
    }

    pub fn glyph(&self, c: char) -> Option<u16> {
        self.face.glyph_index(c).map(|g| g.0).filter(|&g| g != 0)
    }

    pub fn adv(&self, g: u16) -> f32 {
        self.face.glyph_hor_advance(GlyphId(g)).unwrap_or(0) as f32
    }

    /// Ink box `(x0, y0, x1, y1)` in font units, y up (zeros for blank glyphs).
    pub fn bbox(&self, g: u16) -> (f32, f32, f32, f32) {
        if let Some(b) = self.boxes.borrow().get(&g) {
            return (b[0], b[1], b[2], b[3]);
        }
        let b = self
            .face
            .glyph_bounding_box(GlyphId(g))
            .map_or([0.0; 4], |r| [r.x_min as f32, r.y_min as f32, r.x_max as f32, r.y_max as f32]);
        self.boxes.borrow_mut().insert(g, b);
        (b[0], b[1], b[2], b[3])
    }

    pub fn italic(&self, g: u16) -> f32 {
        self.math
            .glyph_info
            .and_then(|i| i.italic_corrections)
            .and_then(|v| v.get(GlyphId(g)))
            .map_or(0.0, |v| v.value as f32)
    }

    pub fn top_accent(&self, g: u16) -> Option<f32> {
        self.math
            .glyph_info
            .and_then(|i| i.top_accent_attachments)
            .and_then(|v| v.get(GlyphId(g)))
            .map(|v| v.value as f32)
    }

    fn construction(&self, g: u16, vertical: bool) -> Option<math::GlyphConstruction<'static>> {
        let v = self.math.variants?;
        let set = if vertical { v.vertical_constructions } else { v.horizontal_constructions };
        set.get(GlyphId(g))
    }

    /// Size variants of `g` (smallest first) with their extent along the stretch direction.
    pub fn variants(&self, g: u16, vertical: bool) -> Vec<(u16, f32)> {
        self.construction(g, vertical)
            .map(|c| c.variants.into_iter().map(|v| (v.variant_glyph.0, v.advance_measurement as f32)).collect())
            .unwrap_or_default()
    }

    /// Assembly recipe of `g`, when it has one.
    pub fn assembly(&self, g: u16, vertical: bool) -> Option<Vec<Part>> {
        let a = self.construction(g, vertical)?.assembly?;
        let parts: Vec<Part> = a
            .parts
            .into_iter()
            .map(|p| Part { g: p.glyph_id.0, adv: p.full_advance as f32, ext: p.part_flags.extender() })
            .collect();
        (!parts.is_empty()).then_some(parts)
    }

    /// Outline of `g` in font units (y up), `None` for blank glyphs.
    pub fn outline(&self, g: u16) -> Option<tiny_skia::Path> {
        let mut s = Sink(tiny_skia::PathBuilder::new());
        self.face.outline_glyph(GlyphId(g), &mut s)?;
        s.0.finish()
    }
}
