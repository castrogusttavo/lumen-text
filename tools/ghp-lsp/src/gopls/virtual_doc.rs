//! Builds the virtual `.go` document gopls type-checks in place of a `.ghp` page.
//!
//! The shape mirrors what the GHP transpiler emits (`codegen.Assemble`): a package clause, the
//! imports (user `<go:import>` plus the managed `net/http`, `fmt`, `html`, `io`), one handler
//! function, and a statement per tag. Every Go region of the page is copied verbatim so the
//! [`SourceMap`] is a plain list of equal-length segments.

use super::sourcemap::{Segment, SourceMap};
use std::ops::Range;

/// What a region of the page is. Structural tags without Go text (`else`, `endif`, ...) are
/// regions with an empty range: they only contribute scaffolding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    /// `<go:import (...)/>`: the text after the tag name, e.g. `("fmt")` or `"fmt"`.
    Import,
    /// `<go ... />`: raw statements.
    Code,
    /// `<go= expr />`.
    Echo,
    If,
    ElseIf,
    Else,
    EndIf,
    For,
    EndFor,
    Switch,
    Case,
    Default,
    EndSwitch,
}

/// A Go region of the page, in byte offsets of the `.ghp` source. Local stand-in for the
/// parser's node type; adapt with `From` once the parser API settles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprRegion {
    pub start: usize,
    pub end: usize,
    pub kind: RegionKind,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Go package clause. Should match the sibling `.go` files of the page's directory.
    pub package: String,
    /// Handler function name. Keep it unique per page and distinct from the real generated one,
    /// so the virtual file never redeclares anything on disk.
    pub func_name: String,
}

impl Options {
    /// Options for a page named by its file stem (`home.ghp` -> `ghp_virtual_home`).
    pub fn for_page(file_stem: &str, package: &str) -> Self {
        let mut name = String::from("ghp_virtual_");
        for c in file_stem.chars() {
            name.push(if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' });
        }
        Options { package: package.to_string(), func_name: name }
    }
}

/// The generated document plus its mapping back to the page.
#[derive(Clone, Debug)]
pub struct VirtualDoc {
    pub text: String,
    pub map: SourceMap,
}

struct Writer<'a> {
    src: &'a str,
    text: String,
    segs: Vec<Segment>,
    /// Where the Go text of an empty tag would start (see [`SourceMap::with_slots`]).
    slots: Vec<Segment>,
}

impl Writer<'_> {
    fn raw(&mut self, s: &str) {
        self.text.push_str(s);
    }

    fn mapped(&mut self, r: Range<usize>) {
        self.segs.push(Segment { ghp_start: r.start, go_start: self.text.len(), len: r.end - r.start });
        self.text.push_str(&self.src[r]);
    }
}

/// Paths of the unaliased imports declared in an import region's text.
fn declared_unaliased(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches('(').trim_end_matches(')');
    inner
        .split(['\n', ';'])
        .filter_map(|spec| {
            let spec = spec.trim();
            if spec.starts_with('"') || spec.starts_with('`') {
                Some(spec.trim_matches(['"', '`']).to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Generate the virtual document. `regions` must be in source order and lie within `src`
/// on char boundaries; regions that violate this are skipped.
pub fn build(src: &str, regions: &[ExprRegion], opts: &Options) -> VirtualDoc {
    let valid = |r: &ExprRegion| {
        r.start <= r.end && r.end <= src.len() && src.is_char_boundary(r.start) && src.is_char_boundary(r.end)
    };
    let regions: Vec<&ExprRegion> = regions.iter().filter(|r| valid(r)).collect();

    // The transpiler imports these by their default name; a user import of the same path
    // (without alias) takes over, so we must not import it a second time.
    let mut managed = vec!["net/http", "fmt", "html", "io"];
    for r in regions.iter().filter(|r| r.kind == RegionKind::Import) {
        for p in declared_unaliased(&src[r.start..r.end]) {
            managed.retain(|m| *m != p);
        }
    }

    let mut w = Writer { src, text: String::new(), segs: Vec::new(), slots: Vec::new() };
    w.raw(&format!("package {}\n\n", opts.package));
    if !managed.is_empty() {
        w.raw("import (\n");
        for m in &managed {
            w.raw(&format!("\t\"{m}\"\n"));
        }
        w.raw(")\n");
    }
    for r in regions.iter().filter(|r| r.kind == RegionKind::Import) {
        w.raw("import ");
        w.mapped(r.start..r.end);
        w.raw("\n");
    }
    w.raw(&format!("\nfunc {}(w http.ResponseWriter, r *http.Request) {{\n\t_ = w\n\t_ = r\n", opts.func_name));

    for r in regions {
        use RegionKind::*;
        let range = r.start..r.end;
        let (before, after) = match r.kind {
            Import => continue,
            Code => ("", ""),
            Echo => ("io.WriteString(w, html.EscapeString(fmt.Sprint(", ")))"),
            If => ("if ", " {"),
            ElseIf => ("} else if ", " {"),
            Else => ("} else {", ""),
            EndIf | EndFor | EndSwitch => ("}", ""),
            For => ("for ", " {"),
            Switch => ("switch ", " {"),
            Case => ("case ", ":"),
            Default => ("default:", ""),
        };
        w.raw("\t");
        w.raw(before);
        if range.is_empty() && !matches!(r.kind, Else | EndIf | EndFor | EndSwitch | Default) {
            // `<go= | />`: the tag takes Go text but has none yet; keep a cursor slot.
            w.slots.push(Segment { ghp_start: r.start, go_start: w.text.len(), len: 0 });
        }
        w.mapped(range);
        w.raw(after);
        w.raw("\n");
    }
    w.raw("}\n\n");
    // Keep the managed imports used even on a page that never echoes, so only real problems
    // are reported (and anything about these lines is unmapped and dropped anyway).
    w.raw("var _ = http.StatusOK\nvar _ = fmt.Sprint\nvar _ = html.EscapeString\nvar _ = io.EOF\n");

    let map = SourceMap::new(w.segs, src, &w.text).with_slots(w.slots);
    VirtualDoc { text: w.text, map }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gopls::adapter::regions_of;

    fn golden(name: &str, src: &str, want: &str) {
        let regions = regions_of(src);
        let doc = build(src, &regions, &Options::for_page(name, "pages"));
        if std::env::var_os("GHP_UPDATE_GOLDEN").is_some() {
            let p = format!("{}/tests/golden/{name}.go", env!("CARGO_MANIFEST_DIR"));
            std::fs::write(p, &doc.text).unwrap();
            return;
        }
        assert_eq!(doc.text, want, "golden {name} differs (GHP_UPDATE_GOLDEN=1 to refresh)");
        // Every segment must copy identical text.
        for s in doc.map.segments() {
            assert_eq!(&src[s.ghp_start..s.ghp_start + s.len], &doc.text[s.go_start..s.go_start + s.len]);
        }
    }

    #[test]
    fn home_page() {
        golden("home", include_str!("../../tests/demo/pages/home.ghp"), include_str!("../../tests/golden/home.go"));
    }

    #[test]
    fn broken_page() {
        golden(
            "broken",
            include_str!("../../tests/demo/pages/broken.ghp"),
            include_str!("../../tests/golden/broken.go"),
        );
    }

    #[test]
    fn user_import_of_managed_package_is_not_duplicated() {
        let src = "<go:import (\"fmt\")/><go= fmt.Sprint(1) />";
        let doc = build(src, &regions_of(src), &Options::for_page("p", "main"));
        assert_eq!(doc.text.matches("\"fmt\"").count(), 1);
    }

    #[test]
    fn aliased_import_keeps_managed_copy() {
        let src = "<go:import (f \"fmt\")/>";
        let doc = build(src, &regions_of(src), &Options::for_page("p", "main"));
        assert_eq!(doc.text.matches("\"fmt\"").count(), 2);
    }

    #[test]
    fn invalid_regions_are_skipped() {
        let src = "héllo";
        let bad = [
            ExprRegion { start: 2, end: 3, kind: RegionKind::Code },
            ExprRegion { start: 0, end: 99, kind: RegionKind::Code },
        ];
        let doc = build(src, &bad, &Options::for_page("p", "main"));
        assert!(doc.map.segments().is_empty());
    }

    #[test]
    fn func_name_is_sanitised() {
        assert_eq!(Options::for_page("a-b.c", "x").func_name, "ghp_virtual_a_b_c");
    }
}
