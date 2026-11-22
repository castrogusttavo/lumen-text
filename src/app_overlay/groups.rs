//! Group headers of the pickers: "Recent / Files" in quick open, the area of a command ("File",
//! "Edit"...), the file of a reference, the day of a local-history snapshot. Headers exist only
//! while the list is unfiltered (a ranked list is not contiguous per group), carry a count and the
//! one on top stays put (sticky) while its group scrolls. The geometry is pure so it is tested.

use super::*;
use crate::ds;
use std::path::Path;

/// Height of a header row.
pub const HDR_H: i32 = ds::S5;
/// Headers the list area reserves room for (more are clipped at the bottom, never move the panel).
pub const RESERVED: usize = 3;

/// A run of rows with the same group: `count` rows starting at filtered row `start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub start: usize,
    pub title: String,
    pub count: usize,
}

/// Runs of equal consecutive keys. Fewer than two runs means nothing is worth a header.
pub fn runs<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for (i, k) in keys.enumerate() {
        match out.last_mut() {
            Some(g) if g.title == k => g.count += 1,
            _ => out.push(Group { start: i, title: k.to_string(), count: 1 }),
        }
    }
    if out.len() < 2 { Vec::new() } else { out }
}

/// Extra height the list area keeps for `groups`.
pub fn reserve(groups: &[Group]) -> i32 {
    groups.len().min(RESERVED) as i32 * HDR_H
}

/// Headers at or before filtered row `k`.
fn before(groups: &[Group], k: usize) -> i32 {
    groups.iter().take_while(|g| g.start <= k).count() as i32
}

/// Top of row `k` in px from the top of the list area, with row `scroll` first. The first header
/// (or the sticky one) takes the top `HDR_H` px, so with groups every row sits that much lower.
pub fn row_y(groups: &[Group], scroll: usize, k: usize, row_h: i32) -> i32 {
    let abs = |i: usize| i as i32 * row_h + before(groups, i) * HDR_H;
    let lead = if groups.is_empty() { 0 } else { HDR_H };
    abs(k) - abs(scroll) + lead
}

/// Top of the highlight at the fractional row `f` (the glide between two rows).
pub fn glide_y(groups: &[Group], scroll: usize, f: f32, row_h: i32) -> i32 {
    let lo = f.max(0.0).floor() as usize;
    let t = (f.max(0.0) - lo as f32).clamp(0.0, 1.0);
    let (a, b) = (row_y(groups, scroll, lo, row_h), row_y(groups, scroll, lo + 1, row_h));
    a + ((b - a) as f32 * t).round() as i32
}

/// The header of the group row `k` belongs to.
pub fn group_of(groups: &[Group], k: usize) -> Option<&Group> {
    groups.iter().take_while(|g| g.start <= k).last()
}

/// Age bucket of a local-history snapshot (`age` in seconds), newest first.
pub fn age_bucket(age: i64) -> &'static str {
    match age {
        ..3600 => "Last hour",
        3600..86_400 => "Today",
        86_400..172_800 => "Yesterday",
        172_800..604_800 => "This week",
        _ => "Older",
    }
}

impl Picker {
    /// Headers of the list as shown now: `open` are the paths of the open documents.
    pub fn groups(&self, open: &[&Path]) -> Vec<Group> {
        if !self.field.text.trim().is_empty() || self.filtered.is_empty() {
            return Vec::new();
        }
        if let Some(by) = &self.group_by {
            return runs(self.filtered.iter().map(|(i, _)| by.get(*i).map_or("", String::as_str)));
        }
        match self.kind {
            PickKind::Commands => runs(
                self.filtered
                    .iter()
                    .map(|(i, _)| self.items[*i].label.split_once(": ").map_or("General", |(area, _)| area)),
            ),
            PickKind::QuickOpen if self.files.is_some() => {
                // The open documents lead the file list while nothing is typed.
                let lead = self
                    .filtered
                    .iter()
                    .take_while(
                        |(i, _)| matches!(&self.items[*i].action, Action::Open(p, _) if open.contains(&p.as_path())),
                    )
                    .count();
                runs((0..self.filtered.len()).map(|k| if k < lead { "Recent" } else { "Files" }))
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(keys: &[&str]) -> Vec<Group> {
        runs(keys.iter().copied())
    }

    #[test]
    fn runs_count_consecutive_keys() {
        let v = g(&["a", "a", "b", "a"]);
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].start, v[0].count), (0, 2));
        assert_eq!((v[1].title.as_str(), v[1].start), ("b", 2));
        assert_eq!((v[2].start, v[2].count), (3, 1));
    }

    #[test]
    fn one_group_gets_no_header() {
        assert!(g(&["a", "a", "a"]).is_empty());
        assert!(g(&[]).is_empty());
    }

    #[test]
    fn rows_sit_below_their_headers() {
        let v = g(&["a", "a", "b", "b", "b"]);
        // Scrolled to the top: header, rows 0 and 1, header, rows 2..
        assert_eq!(row_y(&v, 0, 0, 34), HDR_H);
        assert_eq!(row_y(&v, 0, 1, 34), HDR_H + 34);
        assert_eq!(row_y(&v, 0, 2, 34), HDR_H + 2 * 34 + HDR_H);
        // Scrolled onto the first row of a group: its header is on top.
        assert_eq!(row_y(&v, 2, 2, 34), HDR_H);
        // Scrolled into the middle of a group: the sticky header takes the top.
        assert_eq!(row_y(&v, 3, 3, 34), HDR_H);
        assert_eq!(row_y(&v, 3, 4, 34), HDR_H + 34);
    }

    #[test]
    fn no_groups_means_plain_rows() {
        assert_eq!(row_y(&[], 3, 5, 34), 68);
        assert_eq!(reserve(&[]), 0);
    }

    #[test]
    fn glide_is_between_the_two_rows() {
        let v = g(&["a", "b"]);
        let (a, b) = (row_y(&v, 0, 0, 34), row_y(&v, 0, 1, 34));
        assert_eq!(glide_y(&v, 0, 0.0, 34), a);
        assert_eq!(glide_y(&v, 0, 1.0, 34), b);
        let mid = glide_y(&v, 0, 0.5, 34);
        assert!(mid > a && mid < b);
    }

    #[test]
    fn reserve_is_capped() {
        let v = g(&["a", "b", "c", "d", "e"]);
        assert_eq!(reserve(&v), RESERVED as i32 * HDR_H);
        assert_eq!(group_of(&v, 3).map(|x| x.title.as_str()), Some("d"));
    }

    #[test]
    fn buckets_by_age() {
        assert_eq!(age_bucket(30), "Last hour");
        assert_eq!(age_bucket(7200), "Today");
        assert_eq!(age_bucket(100_000), "Yesterday");
        assert_eq!(age_bucket(300_000), "This week");
        assert_eq!(age_bucket(9_999_999), "Older");
    }

    fn picker(kind: PickKind, labels: &[&str]) -> Picker {
        let items = labels
            .iter()
            .map(|l| Item { label: l.to_string(), detail: String::new(), action: Action::Run(l.to_string()) })
            .collect();
        Picker::new(kind, "t", items)
    }

    #[test]
    fn commands_group_by_area_only_while_unfiltered() {
        let mut p = picker(PickKind::Commands, &["File: Save", "File: Close", "Edit: Undo", "Go to line"]);
        let v = p.groups(&[]);
        assert_eq!(
            v.iter().map(|x| (x.title.as_str(), x.count)).collect::<Vec<_>>(),
            [("File", 2), ("Edit", 1), ("General", 1)]
        );
        p.field = TextField::with("sa");
        p.refilter();
        assert!(p.groups(&[]).is_empty(), "a ranked list has no headers");
    }

    #[test]
    fn explicit_groups_win() {
        let mut p = picker(PickKind::Locations, &["x", "y", "z"]);
        p.group_by = Some(vec!["a.rs".into(), "a.rs".into(), "b.rs".into()]);
        let v = p.groups(&[]);
        assert_eq!((v.len(), v[0].count, v[1].title.as_str()), (2, 2, "b.rs"));
    }
}
