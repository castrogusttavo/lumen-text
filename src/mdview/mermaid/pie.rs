//! `pie`.

use super::*;

#[derive(Debug, Default)]
pub(crate) struct Pie {
    pub(crate) title: String,
    pub(crate) show_data: bool,
    pub(crate) slices: Vec<(String, f64)>,
}

pub(crate) fn parse(lines: &[String]) -> Pie {
    let mut p = Pie::default();
    for (i, line) in lines.iter().enumerate() {
        let mut l = line.trim();
        if i == 0 {
            l = l.strip_prefix("pie").unwrap_or(l).trim();
            if let Some(r) = l.strip_prefix("showData") {
                p.show_data = true;
                l = r.trim();
            }
        }
        if let Some(t) = l.strip_prefix("title") {
            p.title = t.trim().to_string();
            continue;
        }
        if l == "showData" {
            p.show_data = true;
            continue;
        }
        let Some((name, val)) = l.rsplit_once(':') else { continue };
        let Ok(v) = val.trim().parse::<f64>() else { continue };
        let name = name.trim().trim_matches('"').trim().to_string();
        if v.is_finite() && v > 0.0 && p.slices.len() < 64 {
            p.slices.push((name, v));
        }
    }
    p
}

/// A percentage as people write it: no decimals when whole, one otherwise.
pub(crate) fn pct(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if (r - r.round()).abs() < 1e-9 { format!("{}%", r.round() as i64) } else { format!("{r:.1}%") }
}

pub(super) fn build(lines: &[String], m: Meas) -> Result<Diagram, String> {
    let p = parse(lines);
    if p.slices.is_empty() {
        return Err("pie chart without slices".into());
    }
    let total: f64 = p.slices.iter().map(|s| s.1).sum();
    let r = 96.0f32;
    let mut d = Diagram::default();
    let title_h = if p.title.is_empty() { 0.0 } else { 34.0 };
    let c = (r + 4.0, title_h + r + 4.0);
    let mut a0 = -std::f32::consts::FRAC_PI_2;
    for (i, (_, v)) in p.slices.iter().enumerate() {
        let frac = (*v / total) as f32;
        let a1 = a0 + frac * std::f32::consts::TAU;
        let mut path = vec![P::M(c.0, c.1), P::L(c.0 + r * a0.cos(), c.1 + r * a0.sin())];
        arc(&mut path, c, r, a0, a1);
        path.push(P::Z);
        d.shape(path, Some(Paint::Series(i as u8)), Some(Paint::LabelBg), 1.5);
        if frac > 0.06 {
            let mid = (a0 + a1) / 2.0;
            let (tx, ty) = (c.0 + r * 0.64 * mid.cos(), c.1 + r * 0.64 * mid.sin());
            d.text(tx, ty, &pct(frac as f64 * 100.0), FS_SMALL, true, 0, Paint::OnSeries(i as u8));
        }
        a0 = a1;
    }
    // Legend: swatch, name (and value with showData), percentage.
    let lx = c.0 + r + 30.0;
    let row = 24.0;
    let names: Vec<String> =
        p.slices.iter().map(|(n, v)| if p.show_data { format!("{n} [{}]", trim_num(*v)) } else { n.clone() }).collect();
    let name_w = names.iter().map(|n| m(n, false, FS)).fold(0.0f32, f32::max);
    let pct_w = p.slices.iter().map(|(_, v)| m(&pct(v / total * 100.0), false, FS)).fold(0.0f32, f32::max);
    let leg_h = row * p.slices.len() as f32;
    let ly0 = c.1 - leg_h / 2.0 + row / 2.0;
    for (i, ((_, v), name)) in p.slices.iter().zip(&names).enumerate() {
        let y = ly0 + i as f32 * row;
        d.rect(lx, y - 6.0, 12.0, 12.0, 3.0, Some(Paint::Series(i as u8)), None);
        d.text(lx + 20.0, y, name, FS, false, -1, Paint::Text);
        d.text(lx + 20.0 + name_w + 14.0 + pct_w, y, &pct(v / total * 100.0), FS, false, 1, Paint::Muted);
    }
    let w = lx + 20.0 + name_w + 14.0 + pct_w + 6.0;
    let title_w = if p.title.is_empty() { 0.0 } else { m(&p.title, true, FS_TITLE) + 8.0 };
    d.w = w.max(title_w);
    if !p.title.is_empty() {
        d.text(d.w / 2.0, 14.0, &p.title, FS_TITLE, true, 0, Paint::Text);
    }
    d.h = (c.1 + r + 6.0).max(ly0 + leg_h);
    Ok(d)
}

fn trim_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 { format!("{}", v.round() as i64) } else { format!("{v}") }
}
