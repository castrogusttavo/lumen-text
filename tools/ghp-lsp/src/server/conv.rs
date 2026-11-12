//! JSON <-> domain conversions shared by the features.

use serde_json::{Value, json};

use crate::ast::Span;
use crate::diagnostics::{Diagnostic, Severity};
use crate::line_index::{LineIndex, Position, Range};

pub fn pos_json(p: Position) -> Value {
    json!({"line": p.line, "character": p.character})
}

pub fn range_json(r: Range) -> Value {
    json!({"start": pos_json(r.start), "end": pos_json(r.end)})
}

pub fn span_json(index: &LineIndex, s: Span) -> Value {
    range_json(index.range(s))
}

pub fn pos_from(v: &Value) -> Option<Position> {
    Some(Position {
        line: u32::try_from(v.get("line")?.as_u64()?).ok()?,
        character: u32::try_from(v.get("character")?.as_u64()?).ok()?,
    })
}

pub fn range_from(v: &Value) -> Option<Range> {
    Some(Range { start: pos_from(v.get("start")?)?, end: pos_from(v.get("end")?)? })
}

pub fn diag_json(d: &Diagnostic, index: &LineIndex) -> Value {
    json!({
        "range": span_json(index, d.span),
        "severity": match d.severity { Severity::Error => 1, Severity::Warning => 2 },
        "code": d.code,
        "source": "ghp",
        "message": d.message,
    })
}

/// Offset just after the last non-whitespace byte before `end` (0 when there is none).
pub fn trim_end(text: &str, end: usize) -> usize {
    text[..end.min(text.len())].trim_end().len()
}

/// The leading spaces/tabs of the line that contains `offset`.
pub fn line_indent<'a>(text: &'a str, index: &LineIndex, offset: usize) -> &'a str {
    let Some(ls) = index.line_span(index.line_of(offset)) else { return "" };
    let line = &text[ls.start..ls.end];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

pub fn uri_of(params: &Value) -> Option<&str> {
    params.get("textDocument")?.get("uri")?.as_str()
}
