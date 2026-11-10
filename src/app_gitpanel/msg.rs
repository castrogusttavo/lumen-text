use super::*;

// ── multi-line editing on top of the single-line TextField ──

pub(super) fn byte_at(f: &TextField, ci: usize) -> usize {
    f.text.char_indices().nth(ci).map_or(f.text.len(), |(b, _)| b)
}

/// Insert text keeping line breaks (CRLF / CR become LF, tabs four spaces, other controls dropped).
pub fn ml_insert(f: &mut TextField, s: &str) {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let s: String = s
        .chars()
        .flat_map(|c| if c == '\t' { vec![' '; 4] } else { vec![c] })
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let b = byte_at(f, f.cursor);
    f.text.insert_str(b, &s);
    f.cursor += s.chars().count();
}

/// (line, column) of the cursor, both 0-based, in chars.
pub fn ml_pos(f: &TextField) -> (usize, usize) {
    let before: String = f.text.chars().take(f.cursor).collect();
    let line = before.matches('\n').count();
    (line, before.rsplit('\n').next().map_or(0, |l| l.chars().count()))
}

pub(super) fn line_lens(f: &TextField) -> Vec<usize> {
    f.text.split('\n').map(|l| l.chars().count()).collect()
}

/// Char index of (line, col), col clamped to the line.
pub fn ml_index(f: &TextField, line: usize, col: usize) -> usize {
    let lens = line_lens(f);
    let line = line.min(lens.len() - 1);
    lens[..line].iter().map(|n| n + 1).sum::<usize>() + col.min(lens[line])
}

pub fn ml_vertical(f: &mut TextField, dir: isize) {
    let (l, c) = ml_pos(f);
    let nl = (l as isize + dir).clamp(0, line_lens(f).len() as isize - 1) as usize;
    f.cursor = if nl == l { if dir < 0 { 0 } else { f.len() } } else { ml_index(f, nl, c) };
}

pub fn ml_home(f: &mut TextField) {
    let (l, _) = ml_pos(f);
    f.cursor = ml_index(f, l, 0);
}

pub fn ml_end(f: &mut TextField) {
    let (l, _) = ml_pos(f);
    f.cursor = ml_index(f, l, usize::MAX);
}
