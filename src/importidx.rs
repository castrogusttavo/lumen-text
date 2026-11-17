//! The editor's own index of importable names: every `export` of the project's sources and of the
//! type entry points of the packages listed in `package.json`.
//!
//! tsgo builds the same kind of index for its auto-imports, but only on demand, and that build costs
//! 4-12 s and ~1.5 GB of peak memory on a large app (measured on two Next.js projects). This one is
//! built on a worker thread right after the project opens, in a few MB, and answers completion
//! ("exok" -> `expectOk`) and the "Add import" quick fix without the server.
//!
//! It is syntactic: exports are found by scanning top-level `export` statements, and re-exports are
//! followed only through `export * from` / `/// <reference path>` inside a package. The `import`
//! statement is written by the editor (see [`import_edit`]).
//!
//! The index keeps what it was made from, piece by piece (one per project file, one per package,
//! see [`Parts`]): after a save only the files that changed are read again ([`build_from`]).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

const MAX_PROJECT_FILES: usize = 30_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Generated declaration files are big: the Prisma client of a large schema is 4-70 MB (they are
/// read line by line, never loaded whole).
const MAX_DECL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PKG_FILES: usize = 300;
const MAX_SUBPATHS: usize = 80;
const MAX_DEPTH: usize = 6;

/// Prefix of the `ambient` tag of names declared inside `declare namespace X { ... }`.
const NS_TAG: char = '\0';

const TYPE: u8 = 1;
const DEFAULT: u8 = 2;

#[derive(Clone, Copy)]
struct Entry {
    name_off: u32,
    name_len: u16,
    flags: u8,
    /// Index into `sources`.
    source: u32,
}

/// Where a name is imported from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// A file of the project (absolute path).
    File(PathBuf),
    /// A module specifier: `next`, `next/link`, `node:fs`, `bun:test`.
    Module(String),
}

/// One importable name.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub name: String,
    pub source: Source,
    pub is_type: bool,
    pub is_default: bool,
    pub score: i32,
}

#[derive(Default)]
pub struct Index {
    /// All names, back to back.
    names: String,
    /// Sorted by ASCII-lowercased name.
    entries: Vec<Entry>,
    sources: Vec<Source>,
    /// Per source: how many import statements of the project name that module (0 for files).
    usage: Vec<u32>,
    /// `compilerOptions.paths` of the root tsconfig as (alias prefix, absolute directory).
    aliases: Vec<(String, PathBuf)>,
    /// Files and packages read to make this index (an update reads only what changed).
    pub files_scanned: usize,
    pub packages_scanned: usize,
    /// What the index was assembled from, kept so the next build can reuse what did not change.
    parts: Parts,
}

impl Index {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn name(&self, e: &Entry) -> &str {
        &self.names[e.name_off as usize..e.name_off as usize + e.name_len as usize]
    }

    /// Tie-breaker between sources of one name: the module the project imports most, then the
    /// shortest specifier (`vitest` before `zod` for `describe`, `zod` before `zod/v4-mini`).
    fn source_rank(&self, source: u32) -> (std::cmp::Reverse<u32>, usize) {
        match &self.sources[source as usize] {
            Source::File(_) => (std::cmp::Reverse(u32::MAX), 0),
            Source::Module(m) => (std::cmp::Reverse(self.usage[source as usize]), m.len()),
        }
    }

    fn hit(&self, e: &Entry, score: i32) -> Hit {
        Hit {
            name: self.name(e).to_string(),
            source: self.sources[e.source as usize].clone(),
            is_type: e.flags & TYPE != 0,
            is_default: e.flags & DEFAULT != 0,
            score,
        }
    }

    /// Entries whose name starts with the byte `first` (case-insensitive).
    fn bucket(&self, first: u8) -> &[Entry] {
        let key = first.to_ascii_lowercase();
        let lo = self.entries.partition_point(|e| lower_first(self.name(e)) < key);
        let hi = self.entries.partition_point(|e| lower_first(self.name(e)) <= key);
        &self.entries[lo..hi]
    }

    /// The best `limit` names for what was typed: same first letter, the rest as a subsequence
    /// (`exok` finds `expectOk`). Prefix matches come first.
    pub fn search(&self, typed: &str, limit: usize) -> Vec<Hit> {
        let Some(&first) = typed.as_bytes().first() else { return Vec::new() };
        let lower: Vec<u8> = typed.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let query = crate::fuzzy::Query::new(typed);
        let mut scored: Vec<(i32, &Entry)> = Vec::new();
        for e in self.bucket(first) {
            let name = self.name(e);
            if !subsequence(&lower, name.as_bytes()) {
                continue;
            }
            let Some(mut s) = query.score(name) else { continue };
            if name.len() >= lower.len() && name.as_bytes()[..lower.len()].eq_ignore_ascii_case(&lower) {
                s += 40;
            }
            if name == typed {
                s += 40;
            }
            // Shorter names first among equals (`Link` before `LinkProps`), the project's own first.
            let own = matches!(self.sources[e.source as usize], Source::File(_)) as i32;
            scored.push((s * 4 - name.len().min(60) as i32 / 8 + own, e));
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| self.name(a.1).cmp(self.name(b.1)))
                .then_with(|| self.source_rank(a.1.source).cmp(&self.source_rank(b.1.source)))
                .then_with(|| (a.1.flags & DEFAULT).cmp(&(b.1.flags & DEFAULT)))
        });
        scored.truncate(limit);
        scored.into_iter().map(|(s, e)| self.hit(e, s)).collect()
    }

    /// Every source that exports exactly `name`.
    pub fn exact(&self, name: &str) -> Vec<Hit> {
        let Some(&first) = name.as_bytes().first() else { return Vec::new() };
        let mut found: Vec<&Entry> = self.bucket(first).iter().filter(|e| self.name(e) == name).collect();
        // The project's own files first, then the modules it already imports most.
        found.sort_by_key(|e| self.source_rank(e.source));
        found.into_iter().map(|e| self.hit(e, 0)).collect()
    }

    /// The module specifier to import `source` with from the file `from`.
    pub fn specifier(&self, source: &Source, from: &Path) -> String {
        match source {
            Source::Module(m) => m.clone(),
            Source::File(f) => file_specifier(f, from, &self.aliases),
        }
    }
}

fn lower_first(name: &str) -> u8 {
    name.as_bytes().first().map_or(0, u8::to_ascii_lowercase)
}

/// Is `needle` (already lowercase) a case-insensitive subsequence of `hay`?
fn subsequence(needle: &[u8], hay: &[u8]) -> bool {
    let mut i = 0;
    for &b in hay {
        if i < needle.len() && b.to_ascii_lowercase() == needle[i] {
            i += 1;
        }
    }
    i == needle.len()
}

// ───────────── scanning ─────────────

#[derive(Default, Debug, PartialEq)]
struct Scan {
    /// (name, flags, ambient module it was declared in).
    exports: Vec<(String, u8, Option<String>)>,
    /// `export * from "x"` specifiers.
    stars: Vec<String>,
    /// `/// <reference path="x" />` paths.
    refs: Vec<String>,
    /// `export = X;`: the members of `namespace X` are the module's exports (React's typings).
    export_eq: Option<String>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn ident(s: &str) -> Option<&str> {
    let end = s.char_indices().find(|&(_, c)| !is_ident(c)).map_or(s.len(), |(i, _)| i);
    let id = &s[..end];
    (!id.is_empty() && !id.starts_with(|c: char| c.is_ascii_digit())).then_some(id)
}

/// The text between the first pair of quotes of `s`.
fn quoted(s: &str) -> Option<&str> {
    let start = s.find(['"', '\''])?;
    let q = s.as_bytes()[start] as char;
    let rest = &s[start + 1..];
    Some(&rest[..rest.find(q)?])
}

/// Names of an export list body (`a, b as c, type D`), without the braces.
fn brace_names(body: &str, all_types: bool, ambient: &Option<String>, out: &mut Scan) {
    for item in body.split(',') {
        let item = item.trim();
        let (item, is_type) = match item.strip_prefix("type ") {
            Some(rest) => (rest.trim(), true),
            None => (item, all_types),
        };
        let (local, exported) = match item.split_once(" as ") {
            Some((l, e)) => (l.trim(), e.trim()),
            None => (item, item),
        };
        let ty = if is_type { TYPE } else { 0 };
        if exported == "default" {
            // `export { Foo as default }`: a default export with a usable name.
            if let Some(id) = ident(local).filter(|id| *id != "default" && id.len() == local.len()) {
                out.exports.push((id.to_string(), ty | DEFAULT, ambient.clone()));
            }
        } else if let Some(id) = ident(exported).filter(|id| id.len() == exported.len()) {
            out.exports.push((id.to_string(), ty, ambient.clone()));
        }
    }
}

/// A declaration after its `export` (and modifiers): `function f`, `const x`, `interface I`...
fn declaration(rest: &str) -> Option<(&str, u8)> {
    let mut rest = rest;
    loop {
        let before = rest;
        for m in ["declare ", "async ", "abstract "] {
            rest = rest.strip_prefix(m).unwrap_or(rest).trim_start();
        }
        if rest == before {
            break;
        }
    }
    let (after, flags) = if let Some(r) = rest.strip_prefix("function") {
        (r.trim_start_matches('*').trim_start().trim_start_matches('*').trim_start(), 0)
    } else if let Some(r) = rest.strip_prefix("const enum ") {
        (r, 0)
    } else if let Some(r) =
        ["const ", "let ", "var ", "class ", "enum ", "namespace ", "module "].iter().find_map(|k| rest.strip_prefix(k))
    {
        (r, 0)
    } else if let Some(r) = rest.strip_prefix("interface ") {
        (r, TYPE)
    } else {
        (rest.strip_prefix("type ")?, TYPE)
    };
    ident(after.trim_start()).map(|id| (id, flags))
}

/// Exports of one source file. Only statements that start a line are seen (and, inside
/// `declare module "x" { ... }`, statements one indentation level in).
fn scan(text: &str) -> Scan {
    scan_lines(text.lines())
}

/// [`scan`] over any source of lines (a big file is streamed instead of read into memory).
fn scan_lines<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> Scan {
    let mut out = Scan::default();
    let mut ambient: Option<String> = None;
    // Indentation of the statements directly inside the ambient module (its first line decides).
    let mut ambient_indent: Option<usize> = None;
    // A multi-line `export { ... }` being collected: (body so far, `export type {`).
    let mut open: Option<(String, bool)> = None;
    for raw in lines {
        let raw = raw.as_ref();
        if let Some((body, all_types)) = &mut open {
            match raw.find('}') {
                Some(end) => {
                    body.push_str(&raw[..end]);
                    let (body, all_types) = (std::mem::take(body), *all_types);
                    brace_names(&body, all_types, &ambient, &mut out);
                    open = None;
                }
                None => {
                    // Line comments inside the list would otherwise swallow the next names.
                    body.push_str(raw.split("//").next().unwrap_or(""));
                    body.push(' ');
                }
            }
            continue;
        }
        let line = raw.trim_start();
        let indent = raw.len() - line.len();
        if indent == 0 {
            if let Some(rest) = line.strip_prefix("/// <reference path=") {
                if let Some(p) = quoted(rest) {
                    out.refs.push(p.to_string());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("declare module ") {
                if line.trim_end().ends_with('{') {
                    ambient = quoted(rest).filter(|m| !m.contains('*')).map(str::to_string);
                    ambient_indent = None;
                }
                continue;
            }
            let ns = line.strip_prefix("declare namespace ").or_else(|| line.strip_prefix("export declare namespace "));
            if let Some(id) = ns.and_then(ident).filter(|_| line.trim_end().ends_with('{')) {
                ambient = Some(format!("{NS_TAG}{id}"));
                ambient_indent = None;
                continue;
            }
            if let Some(id) = line.strip_prefix("export = ").and_then(ident) {
                out.export_eq = Some(id.to_string());
                continue;
            }
            if ambient.is_some() && line.starts_with('}') {
                ambient = None;
                continue;
            }
        }
        if ambient.is_some() && ambient_indent.is_none() && !line.is_empty() && indent > 0 {
            ambient_indent = Some(indent);
        }
        let level_ok = if ambient.is_some() { ambient_indent == Some(indent) } else { indent == 0 };
        if !level_ok {
            continue;
        }
        let Some(rest) = line.strip_prefix("export ") else {
            // Inside an ambient module every declaration is exported, with or without the keyword.
            if ambient.is_some()
                && let Some((id, flags)) = declaration(line)
            {
                out.exports.push((id.to_string(), flags, ambient.clone()));
            }
            continue;
        };
        let rest = rest.trim_start();
        if let Some(star) = rest.strip_prefix('*') {
            let star = star.trim_start();
            if let Some(ns) = star.strip_prefix("as ") {
                if let Some(id) = ident(ns.trim_start()) {
                    out.exports.push((id.to_string(), 0, ambient.clone()));
                }
            } else if let Some(spec) = quoted(star) {
                out.stars.push(spec.to_string());
            }
            continue;
        }
        let (list, all_types) = match rest.strip_prefix("type") {
            Some(r) if r.trim_start().starts_with('{') => (Some(r.trim_start()), true),
            _ if rest.starts_with('{') => (Some(rest), false),
            _ => (None, false),
        };
        if let Some(list) = list {
            let body = &list[1..];
            match body.find('}') {
                Some(end) => brace_names(&body[..end], all_types, &ambient, &mut out),
                None => open = Some((format!("{} ", body.split("//").next().unwrap_or("")), all_types)),
            }
            continue;
        }
        if let Some(def) = rest.strip_prefix("default ") {
            let def = def.trim_start();
            let named = declaration(def).or_else(|| {
                // `export default Foo;`
                let id = ident(def)?;
                let tail = def[id.len()..].trim();
                (tail.is_empty() || tail == ";").then_some((id, 0))
            });
            if let Some((id, flags)) = named.filter(|(id, _)| !matches!(*id, "function" | "class" | "async")) {
                out.exports.push((id.to_string(), flags | DEFAULT, ambient.clone()));
            }
            continue;
        }
        if let Some((id, flags)) = declaration(rest) {
            out.exports.push((id.to_string(), flags, ambient.clone()));
        }
    }
    out
}

// ───────────── building ─────────────

#[derive(Default)]
struct Builder {
    names: String,
    entries: Vec<Entry>,
    sources: Vec<Source>,
    source_ids: std::collections::HashMap<Source, u32>,
    seen: HashSet<(u32, String, u8)>,
    /// Module specifier -> import statements of the project that name it.
    imported: std::collections::HashMap<String, u32>,
}

impl Builder {
    fn source(&mut self, s: Source) -> u32 {
        if let Some(&i) = self.source_ids.get(&s) {
            return i;
        }
        let i = self.sources.len() as u32;
        self.sources.push(s.clone());
        self.source_ids.insert(s, i);
        i
    }

    fn add(&mut self, name: &str, flags: u8, source: u32) {
        if name.len() > 200 || self.names.len() > u32::MAX as usize - 256 {
            return;
        }
        if !self.seen.insert((source, name.to_string(), flags & DEFAULT)) {
            return;
        }
        let name_off = self.names.len() as u32;
        self.names.push_str(name);
        self.entries.push(Entry { name_off, name_len: name.len() as u16, flags, source });
    }

    fn finish(mut self, aliases: Vec<(String, PathBuf)>) -> Index {
        let names = std::mem::take(&mut self.names);
        let key = |e: &Entry| &names[e.name_off as usize..e.name_off as usize + e.name_len as usize];
        self.entries.sort_by(|a, b| {
            let (x, y) = (key(a), key(b));
            x.bytes().map(|c| c.to_ascii_lowercase()).cmp(y.bytes().map(|c| c.to_ascii_lowercase())).then(x.cmp(y))
        });
        let usage = self
            .sources
            .iter()
            .map(|s| match s {
                Source::Module(m) => self.imported.get(m).copied().unwrap_or(0),
                Source::File(_) => 0,
            })
            .collect();
        Index {
            names,
            entries: self.entries,
            sources: self.sources,
            usage,
            aliases,
            files_scanned: 0,
            packages_scanned: 0,
            parts: Parts::default(),
        }
    }
}

/// What tells a changed file from an unchanged one: (modification time in ns, length).
type Stamp = (u64, u64);

fn stamp_of(meta: &std::fs::Metadata) -> Option<Stamp> {
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((mtime.as_nanos() as u64, meta.len()))
}

/// The stamp of a path, `None` when nothing is there.
fn stamp(p: &Path) -> Option<Stamp> {
    stamp_of(&std::fs::metadata(p).ok()?)
}

/// Export names back to back, each with its flags and a group (the module, in a package).
#[derive(Clone, Default, Debug, PartialEq)]
struct Packed {
    text: String,
    /// (offset in `text`, length, flags, group).
    items: Vec<(u32, u16, u8, u16)>,
}

impl Packed {
    fn push(&mut self, name: &str, flags: u8, group: u16) {
        if name.len() > 200 || self.text.len() > u32::MAX as usize - 256 {
            return;
        }
        self.items.push((self.text.len() as u32, name.len() as u16, flags, group));
        self.text.push_str(name);
    }

    fn iter(&self) -> impl Iterator<Item = (&str, u8, u16)> {
        self.items
            .iter()
            .map(|&(off, len, flags, group)| (&self.text[off as usize..off as usize + len as usize], flags, group))
    }
}

/// What one project file gives the index.
#[derive(Default)]
struct FilePart {
    stamp: Stamp,
    exports: Packed,
    /// The module specifiers its import statements name, one per line (relative ones left out:
    /// they only rank packages).
    imports: String,
}

/// What one package of `package.json` gives the index, through all its entry points.
#[derive(Default)]
struct PkgPart {
    dep: String,
    /// Files and folders that change without the version changing, as they were when it was
    /// scanned (`None`: not there): declaration files outside the package's folder (the generated
    /// `.prisma/client` that `@prisma/client` re-exports), the package another one re-exports
    /// that was not found, and every file of a package that is not a real install (a linked
    /// workspace package, one without a version).
    stamps: Vec<(PathBuf, Option<Stamp>)>,
    /// Module specifiers (`next`, `next/link`): the groups of `exports`.
    modules: Vec<String>,
    exports: Packed,
    /// Declaration files read.
    files: usize,
}

impl PkgPart {
    fn stamps_current(&self) -> bool {
        self.stamps.iter().all(|(p, s)| stamp(p) == *s)
    }
}

/// Everything an [`Index`] is assembled from.
#[derive(Default)]
struct Parts {
    root: PathBuf,
    /// Every source file of the project, by path.
    files: BTreeMap<PathBuf, Arc<FilePart>>,
    pkgs: Vec<Arc<PkgPart>>,
    /// Stamps of what decides the package list and their versions (see [`deps_stamps`]).
    deps: Vec<Option<Stamp>>,
}

/// `package.json`, the lockfiles and `node_modules`: when none of them moved, the packages are the
/// ones already indexed.
fn deps_stamps(root: &Path) -> Vec<Option<Stamp>> {
    [
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "node_modules",
        "node_modules/.package-lock.json",
        "node_modules/.modules.yaml",
        "node_modules/.yarn-integrity",
    ]
    .iter()
    .map(|f| stamp(&root.join(f)))
    .collect()
}

fn is_source(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs")
        && !name.ends_with(".d.ts")
        && !name.ends_with(".d.mts")
        && !name.ends_with(".d.cts")
}

/// Build the index of the project at `root` (blocking: call it from a worker thread).
pub fn build(root: &Path) -> Index {
    build_from(root, None)
}

/// [`build`], reading only what changed since `old` (an earlier index of the same project) was
/// made: project files with another modification time or size, and packages whose generated
/// files moved. A change of `package.json`, a lockfile or `node_modules` scans every package again.
pub fn build_from(root: &Path, old: Option<&Index>) -> Index {
    let old = old.map(|i| &i.parts).filter(|p| p.root == root);
    let deps = deps_stamps(root);
    let mut files_scanned = 0;
    let files = scan_project(root, old.map(|p| &p.files), &mut files_scanned);
    let mut packages_scanned = 0;
    let mut fresh = |dep: &str| {
        let part = scan_package(root, dep);
        packages_scanned += 1;
        files_scanned += part.files;
        Arc::new(part)
    };
    let pkgs: Vec<Arc<PkgPart>> = match old.filter(|p| p.deps == deps) {
        Some(p) => {
            p.pkgs.iter().map(|pkg| if pkg.stamps_current() { pkg.clone() } else { fresh(pkg.dep.as_str()) }).collect()
        }
        None => dependency_names(root).iter().map(|dep| fresh(dep.as_str())).collect(),
    };
    let mut idx = assemble(Parts { root: root.to_path_buf(), files, pkgs, deps }, read_aliases(root));
    (idx.files_scanned, idx.packages_scanned) = (files_scanned, packages_scanned);
    idx
}

/// The searchable index of `parts`.
fn assemble(parts: Parts, aliases: Vec<(String, PathBuf)>) -> Index {
    let mut b = Builder::default();
    for (path, file) in &parts.files {
        for spec in file.imports.lines() {
            match b.imported.get_mut(spec) {
                Some(n) => *n += 1,
                None => {
                    b.imported.insert(spec.to_string(), 1);
                }
            }
        }
        if file.exports.items.is_empty() {
            continue;
        }
        let src = b.source(Source::File(path.clone()));
        for (name, flags, _) in file.exports.iter() {
            b.add(name, flags, src);
        }
    }
    for pkg in &parts.pkgs {
        let ids: Vec<u32> = pkg.modules.iter().map(|m| b.source(Source::Module(m.clone()))).collect();
        for (name, flags, module) in pkg.exports.iter() {
            if let Some(&src) = ids.get(module as usize) {
                b.add(name, flags, src);
            }
        }
    }
    let mut idx = b.finish(aliases);
    idx.parts = parts;
    idx
}

/// What `text`, a project source file, exports and imports.
fn file_part(text: &str, stamp: Stamp) -> FilePart {
    let mut part = FilePart { stamp, ..Default::default() };
    for line in text.lines().filter(|l| l.ends_with(['"', '\'', ';'])) {
        if let Some(spec) = line.rfind(" from ").and_then(|i| quoted(&line[i..]))
            && !spec.starts_with('.')
        {
            part.imports.push_str(spec);
            part.imports.push('\n');
        }
    }
    for (name, flags, ambient) in &scan(text).exports {
        if ambient.is_none() {
            part.exports.push(name, *flags, 0);
        }
    }
    part
}

/// Every source file of the project. A file with the stamp it has in `old` is taken from there;
/// the others are read (`scanned` counts them).
fn scan_project(
    root: &Path,
    old: Option<&BTreeMap<PathBuf, Arc<FilePart>>>,
    scanned: &mut usize,
) -> BTreeMap<PathBuf, Arc<FilePart>> {
    let mut files = BTreeMap::new();
    for e in ignore::WalkBuilder::new(root).build().flatten() {
        if files.len() >= MAX_PROJECT_FILES {
            break;
        }
        let p = e.path();
        if !is_source(p) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(p) else { continue };
        let Some(stamp) = stamp_of(&meta).filter(|_| meta.is_file() && meta.len() <= MAX_FILE_BYTES) else { continue };
        if let Some(part) = old.and_then(|o| o.get(p)).filter(|part| part.stamp == stamp) {
            files.insert(p.to_path_buf(), part.clone());
            continue;
        }
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        *scanned += 1;
        files.insert(p.to_path_buf(), Arc::new(file_part(&text, stamp)));
    }
    files
}

fn dependency_names(root: &Path) -> Vec<String> {
    let Some(v) = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return Vec::new();
    };
    let mut names: Vec<String> = ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]
        .iter()
        .filter_map(|k| v[*k].as_object())
        .flat_map(|o| o.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The first `types` entry inside a `package.json` `exports` value.
fn types_of(v: &serde_json::Value, depth: usize) -> Option<&str> {
    match v {
        serde_json::Value::String(s) if s.ends_with(".d.ts") || s.ends_with(".d.mts") || s.ends_with(".d.cts") => {
            Some(s)
        }
        serde_json::Value::Object(o) if depth < 4 => {
            o.get("types").and_then(|t| types_of(t, depth + 1).or(t.as_str())).or_else(|| {
                ["import", "default", "require", "node", "browser"].iter().find_map(|k| types_of(o.get(*k)?, depth + 1))
            })
        }
        _ => None,
    }
}

/// `@types/scope__pkg` -> `@scope/pkg`, `@types/node` -> `node`.
fn typed_module(dep: &str) -> Option<String> {
    let n = dep.strip_prefix("@types/")?;
    Some(match n.split_once("__") {
        Some((scope, pkg)) => format!("@{scope}/{pkg}"),
        None => n.to_string(),
    })
}

/// (module specifier, declaration file) pairs a package offers.
fn package_entries(root: &Path, dep: &str) -> Vec<(String, PathBuf)> {
    let nm = root.join("node_modules");
    let (module, dir) = match typed_module(dep) {
        Some(m) => (m, nm.join(dep)),
        None => (dep.to_string(), nm.join(dep)),
    };
    let Some(pkg) = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let exports = &pkg["exports"];
    let main = types_of(exports.get(".").unwrap_or(exports), 0)
        .filter(|_| !exports.is_null())
        .or_else(|| pkg["types"].as_str())
        .or_else(|| pkg["typings"].as_str())
        .map(|t| dir.join(t))
        .or_else(|| Some(dir.join("index.d.ts")).filter(|p| p.is_file()));
    let main = main.map(|p| if p.is_dir() { p.join("index.d.ts") } else { p }).filter(|p| p.is_file()).or_else(|| {
        // No types in the package itself: its DefinitelyTyped sibling (unless it is listed too).
        let mangled = dep.strip_prefix('@').map(|s| s.replace('/', "__")).unwrap_or_else(|| dep.to_string());
        let tdir = nm.join("@types").join(mangled);
        let tp = std::fs::read_to_string(tdir.join("package.json")).ok()?;
        let tv: serde_json::Value = serde_json::from_str(&tp).ok()?;
        let entry = tv["types"].as_str().or_else(|| tv["typings"].as_str()).unwrap_or("index.d.ts");
        Some(tdir.join(entry)).filter(|p| p.is_file())
    });
    if let Some(m) = &main {
        out.push((module.clone(), m.clone()));
    }
    if let Some(map) = exports.as_object() {
        for (key, v) in map {
            if out.len() > MAX_SUBPATHS {
                break;
            }
            let Some(sub) = key.strip_prefix("./") else { continue };
            if sub.contains('*') || sub == "package.json" {
                continue;
            }
            if let Some(file) = types_of(v, 0).map(|t| dir.join(t)).filter(|p| p.is_file()) {
                out.push((format!("{module}/{sub}"), file));
            }
        }
    } else if typed_module(dep).is_none()
        && let Ok(rd) = std::fs::read_dir(&dir)
    {
        // No exports map: `next/link`, `next/navigation`... are declaration files at the package root.
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files {
            if out.len() > MAX_SUBPATHS {
                break;
            }
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(stem) = name.strip_suffix(".d.ts") else { continue };
            if stem == "index" || main.as_deref() == Some(f.as_path()) || !f.is_file() {
                continue;
            }
            out.push((format!("{module}/{stem}"), f));
        }
    }
    out
}

/// The declaration file a relative specifier of a `.d.ts` points at.
fn resolve_dts(dir: &Path, spec: &str) -> Option<PathBuf> {
    let base = dir.join(spec);
    let s = base.to_string_lossy().into_owned();
    let mut tries: Vec<PathBuf> = Vec::new();
    for (js, dts) in [(".js", ".d.ts"), (".mjs", ".d.mts"), (".cjs", ".d.cts")] {
        if let Some(stem) = s.strip_suffix(js) {
            tries.push(PathBuf::from(format!("{stem}{dts}")));
        }
    }
    if s.ends_with(".d.ts") || s.ends_with(".d.mts") || s.ends_with(".d.cts") {
        tries.push(base.clone());
    }
    for ext in [".d.ts", ".d.mts", ".d.cts", ".ts"] {
        tries.push(PathBuf::from(format!("{s}{ext}")));
    }
    tries.push(base.join("index.d.ts"));
    tries.into_iter().find(|p| p.is_file())
}

/// `@scope/pkg/sub/path` -> (`@scope/pkg`, `sub/path`).
fn split_bare(spec: &str) -> Option<(String, Option<&str>)> {
    let mut parts = spec.splitn(if spec.starts_with('@') { 3 } else { 2 }, '/');
    let pkg =
        if spec.starts_with('@') { format!("{}/{}", parts.next()?, parts.next()?) } else { parts.next()?.to_string() };
    Some((pkg, parts.next()))
}

/// The declaration file of a bare specifier re-exported by a file in `dir` (`export * from
/// ".prisma/client/default"` in `@prisma/client`): node resolution from the file's real location.
fn resolve_bare(dir: &Path, spec: &str) -> Option<PathBuf> {
    let real = std::fs::canonicalize(dir).ok()?;
    let (pkg, sub) = split_bare(spec)?;
    for anc in real.ancestors() {
        let nm = anc.join("node_modules");
        let pkg_dir = nm.join(&pkg);
        if !pkg_dir.is_dir() {
            continue;
        }
        let json = std::fs::read_to_string(pkg_dir.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .unwrap_or(serde_json::Value::Null);
        let exports = &json["exports"];
        let from_map = |key: &str| types_of(exports.get(key)?, 0).map(|t| pkg_dir.join(t)).filter(|p| p.is_file());
        return match sub {
            Some(sub) => from_map(&format!("./{sub}")).or_else(|| resolve_dts(&pkg_dir, sub)),
            None => from_map(".")
                .or_else(|| json["types"].as_str().or_else(|| json["typings"].as_str()).map(|t| pkg_dir.join(t)))
                .filter(|p| p.is_file())
                .or_else(|| Some(pkg_dir.join("index.d.ts")).filter(|p| p.is_file())),
        };
    }
    None
}

/// A package being scanned.
struct PkgScan {
    part: PkgPart,
    module_ids: HashMap<String, u16>,
    seen: HashSet<(u16, String, u8)>,
    /// The package's folder and the project's `node_modules`.
    dir: PathBuf,
    node_modules: PathBuf,
    /// Not a real install: every file read is stamped (see `PkgPart::stamps`).
    volatile: bool,
    /// Declaration files it may still read.
    budget: usize,
    /// Each file is parsed once per package, however many entry points reach it.
    parsed: HashMap<PathBuf, Rc<Scan>>,
}

impl PkgScan {
    fn add(&mut self, module: &str, name: &str, flags: u8) {
        let id = match self.module_ids.get(module) {
            Some(&id) => id,
            None => {
                let Ok(id) = u16::try_from(self.part.modules.len()) else { return };
                self.part.modules.push(module.to_string());
                self.module_ids.insert(module.to_string(), id);
                id
            }
        };
        if self.seen.insert((id, name.to_string(), flags & DEFAULT)) {
            self.part.exports.push(name, flags, id);
        }
    }

    fn note(&mut self, path: PathBuf) {
        if !self.part.stamps.iter().any(|(p, _)| *p == path) {
            let stamp = stamp(&path);
            self.part.stamps.push((path, stamp));
        }
    }

    /// The scan of a declaration file, `None` when it cannot be read or the budget is spent.
    fn parse(&mut self, file: &Path) -> Option<Rc<Scan>> {
        if let Some(found) = self.parsed.get(file) {
            return Some(found.clone());
        }
        if self.budget == 0 {
            return None;
        }
        let found = Rc::new(parse_declaration(file)?);
        self.budget -= 1;
        self.part.files += 1;
        if self.volatile || !file.starts_with(&self.dir) {
            self.note(file.to_path_buf());
        }
        self.parsed.insert(file.to_path_buf(), found.clone());
        Some(found)
    }
}

/// The `version` of a `package.json`.
fn package_version(manifest: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(manifest).ok()?).ok()?;
    v["version"].as_str().map(str::to_string)
}

/// Everything the dependency `dep` of the project at `root` exports, through all its entry points.
fn scan_package(root: &Path, dep: &str) -> PkgPart {
    let node_modules = root.join("node_modules");
    let dir = node_modules.join(dep);
    let manifest = dir.join("package.json");
    let version = package_version(&manifest);
    // A real install sits in a `node_modules` folder (through the links of pnpm too) and has a
    // version; a linked workspace package is rebuilt in place under the same version.
    let installed =
        std::fs::canonicalize(&dir).is_ok_and(|real| real.components().any(|c| c.as_os_str() == "node_modules"));
    let mut s = PkgScan {
        part: PkgPart { dep: dep.to_string(), ..Default::default() },
        module_ids: HashMap::new(),
        seen: HashSet::new(),
        dir,
        node_modules,
        volatile: version.is_none() || !installed,
        budget: MAX_PKG_FILES,
        parsed: HashMap::new(),
    };
    if s.volatile {
        s.note(manifest);
    }
    // The main entry first (it gets the budget), and a fresh `visited` per entry: a file both
    // `next` and `next/types` reach is importable from either.
    let mut entries = package_entries(root, dep);
    entries.sort_by_key(|(module, _)| module.len());
    for (module, file) in entries {
        let mut visited: HashSet<PathBuf> = HashSet::new();
        scan_declaration(&file, Some(&module), 0, &mut visited, &mut s);
    }
    s.part
}

fn parse_declaration(file: &Path) -> Option<Scan> {
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > MAX_DECL_BYTES {
        return None;
    }
    if meta.len() <= MAX_FILE_BYTES {
        return Some(scan(&std::fs::read_to_string(file).ok()?));
    }
    use std::io::BufRead;
    let f = std::fs::File::open(file).ok()?;
    Some(scan_lines(std::io::BufReader::with_capacity(1 << 16, f).lines().map_while(Result::ok)))
}

/// `module` is `None` for a file reached through `/// <reference path>`: it is part of the
/// compilation but not of the module, so only its `declare module "x"` blocks count.
fn scan_declaration(file: &Path, module: Option<&str>, depth: usize, visited: &mut HashSet<PathBuf>, s: &mut PkgScan) {
    if depth > MAX_DEPTH || !visited.insert(file.to_path_buf()) {
        return;
    }
    let Some(found) = s.parse(file) else { return };
    for (name, flags, ambient) in &found.exports {
        let target = match ambient.as_deref() {
            None => module,
            // A namespace member is an export only of the `export = Namespace` module.
            Some(tag) if tag.starts_with(NS_TAG) => {
                module.filter(|_| found.export_eq.as_deref() == Some(&tag[NS_TAG.len_utf8()..]))
            }
            Some(named) => Some(named),
        };
        if let Some(target) = target {
            s.add(target, name, *flags);
        }
    }
    let dir = file.parent().unwrap_or(Path::new("."));
    for spec in &found.stars {
        // `./x` is a file next to this one; anything else is another package (`.prisma/client`).
        let relative = spec.starts_with("./") || spec.starts_with("../");
        let next = if relative { resolve_dts(dir, spec) } else { resolve_bare(dir, spec) };
        match next {
            Some(next) => scan_declaration(&next, module, depth + 1, visited, s),
            // Not there (yet): when that package appears, this one is scanned again.
            None if !relative => {
                if let Some((pkg, _)) = split_bare(spec) {
                    s.note(s.node_modules.join(pkg));
                }
            }
            None => {}
        }
    }
    for spec in &found.refs {
        if let Some(next) = resolve_dts(dir, spec) {
            scan_declaration(&next, None, depth + 1, visited, s);
        }
    }
}

// ───────────── module specifiers ─────────────

/// `compilerOptions.paths` entries of the form `"prefix/*": ["dir/*"]`, longest directory first.
fn read_aliases(root: &Path) -> Vec<(String, PathBuf)> {
    let Some(v) = std::fs::read_to_string(root.join("tsconfig.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&crate::workspace::strip_jsonc(&t)).ok())
    else {
        return Vec::new();
    };
    let co = &v["compilerOptions"];
    let base = root.join(co["baseUrl"].as_str().unwrap_or("."));
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (pattern, targets) in co["paths"].as_object().into_iter().flatten() {
        let (Some(prefix), Some(target)) =
            (pattern.strip_suffix('*'), targets.get(0).and_then(|t| t.as_str()).and_then(|t| t.strip_suffix('*')))
        else {
            continue;
        };
        out.push((prefix.to_string(), crate::paths::simplify(base.join(target))));
    }
    out.sort_by_key(|(_, dir)| std::cmp::Reverse(dir.as_os_str().len()));
    out
}

fn strip_source_ext(p: &str) -> &str {
    for ext in [".tsx", ".ts", ".jsx", ".js", ".mts", ".cts", ".mjs", ".cjs"] {
        if let Some(s) = p.strip_suffix(ext) {
            return s.strip_suffix("/index").unwrap_or(s);
        }
    }
    p
}

fn file_specifier(target: &Path, from: &Path, aliases: &[(String, PathBuf)]) -> String {
    let from_dir = from.parent().unwrap_or(Path::new("/"));
    // Same folder or below: a short relative path reads best.
    if let Ok(rel) = target.strip_prefix(from_dir) {
        return format!("./{}", strip_source_ext(&rel.to_string_lossy().replace('\\', "/")));
    }
    for (prefix, dir) in aliases {
        if let Ok(rel) = target.strip_prefix(dir) {
            return format!("{prefix}{}", strip_source_ext(&rel.to_string_lossy().replace('\\', "/")));
        }
    }
    // Climb from the importing folder to the common ancestor.
    let mut ups = String::new();
    let mut base = from_dir;
    loop {
        if let Ok(rel) = target.strip_prefix(base) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            return format!("{}{}", if ups.is_empty() { "./" } else { &ups }, strip_source_ext(&rel));
        }
        match base.parent() {
            Some(p) => {
                ups.push_str("../");
                base = p;
            }
            None => return strip_source_ext(&target.to_string_lossy().replace('\\', "/")).to_string(),
        }
    }
}

// ───────────── writing the import ─────────────

/// A text insertion: line, UTF-16 column, text.
#[derive(Debug, PartialEq)]
pub struct Insert {
    pub line: usize,
    pub col: usize,
    pub text: String,
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

fn has_word(hay: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(i) = hay[from..].find(word) {
        let (a, b) = (from + i, from + i + word.len());
        let before = hay[..a].chars().next_back().is_none_or(|c| !is_ident(c));
        let after = hay[b..].chars().next().is_none_or(|c| !is_ident(c));
        if before && after {
            return true;
        }
        from = b;
    }
    false
}

/// The edit that makes `name` importable from `spec` in a file whose text is `text`: the name is
/// added to an existing `import { ... } from "spec"`, or a new statement goes after the last
/// import. `None` when it is already imported from there.
pub fn import_edit(text: &str, name: &str, spec: &str, is_type: bool, is_default: bool) -> Option<Insert> {
    let lines: Vec<&str> = text.lines().collect();
    // Import statements as (first line, last line).
    let mut imports: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.starts_with("import ") || l.starts_with("import{") || l.starts_with("import\"") || l.starts_with("import'")
        {
            let start = i;
            // The statement ends on the line that carries its module string.
            while i < lines.len() && quoted(lines[i]).is_none() && i - start < 400 {
                i += 1;
            }
            imports.push((start, i.min(lines.len() - 1)));
        }
        i += 1;
    }
    let (mut quote, mut semi) = ('"', true);
    if let Some(&(_, end)) = imports.first() {
        let l = lines[end].trim_end();
        quote = if l.contains('\'') && !l.contains('"') { '\'' } else { '"' };
        semi = l.ends_with(';');
    }
    for &(start, end) in &imports {
        if quoted(lines[end]) != Some(spec) {
            continue;
        }
        let stmt = lines[start..=end].join("\n");
        let head = &stmt[..stmt.rfind(" from ").or_else(|| stmt.rfind("from")).unwrap_or(stmt.len())];
        if has_word(head.strip_prefix("import").unwrap_or(head), name) {
            return None;
        }
        let type_only = head.trim_start().starts_with("import type ");
        if is_default || head.contains("* as ") || (type_only && !is_type) {
            continue;
        }
        // Merge into the braces of this statement.
        let Some(close_line) = (start..=end).rev().find(|&k| lines[k].contains('}')) else { continue };
        let close_col = lines[close_line].rfind('}')?;
        let item = if is_type && !type_only { format!("type {name}") } else { name.to_string() };
        // The last thing before the closing brace decides the separator.
        let (mut k, mut before) = (close_line, lines[close_line][..close_col].trim_end());
        while before.is_empty() && k > start {
            k -= 1;
            before = lines[k].trim_end();
        }
        if before.ends_with('{') {
            return Some(Insert { line: k, col: utf16_len(before), text: format!(" {item}") });
        }
        let multi = k != close_line;
        let indent: String = lines[k].chars().take_while(|c| c.is_whitespace()).collect();
        let text = match (before.ends_with(','), multi) {
            (true, true) => format!("\n{indent}{item},"),
            (true, false) => format!(" {item},"),
            (false, true) => format!(",\n{indent}{item}"),
            (false, false) => format!(", {item}"),
        };
        return Some(Insert { line: k, col: utf16_len(before), text });
    }
    let end = if semi { ";" } else { "" };
    let stmt = match (is_default, is_type) {
        (true, true) => format!("import type {name} from {quote}{spec}{quote}{end}\n"),
        (true, false) => format!("import {name} from {quote}{spec}{quote}{end}\n"),
        (false, true) => format!("import type {{ {name} }} from {quote}{spec}{quote}{end}\n"),
        (false, false) => format!("import {{ {name} }} from {quote}{spec}{quote}{end}\n"),
    };
    if let Some(&(_, last)) = imports.last() {
        return Some(Insert { line: last + 1, col: 0, text: stmt });
    }
    // No imports yet: below a shebang and the directives ("use client").
    let mut line = 0;
    while line < lines.len() {
        let l = lines[line].trim();
        let directive = (l.starts_with('"') || l.starts_with('\'')) && l.len() < 40;
        if (line == 0 && l.starts_with("#!")) || directive {
            line += 1;
        } else {
            break;
        }
    }
    let blank_after = lines.get(line).is_some_and(|l| !l.trim().is_empty());
    let lead = if line > 0 && lines.get(line).is_some_and(|l| !l.trim().is_empty()) { "\n" } else { "" };
    Some(Insert { line, col: 0, text: format!("{lead}{stmt}{}", if blank_after { "\n" } else { "" }) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &Scan) -> Vec<(&str, u8)> {
        s.exports.iter().map(|(n, f, _)| (n.as_str(), *f)).collect()
    }

    #[test]
    fn scans_declarations_lists_and_defaults() {
        let s = scan(
            "export function expectOk<T>(r: R<T>): T {}\nexport async function* gen() {}\nexport const a = 1, b = 2;\n\
             export declare const enum E {}\nexport interface I {}\nexport type T = 1;\nexport abstract class C {}\n\
             export { x, y as z, type W };\nexport type { M } from './m';\nexport default function Page() {}\n\
             const local = 1;\n  export const nested = 1;\nexport * from './all';\nexport * as ns from './ns';\n\
             export const { d1, d2 } = obj;\n",
        );
        assert_eq!(
            names(&s),
            vec![
                ("expectOk", 0),
                ("gen", 0),
                ("a", 0),
                ("E", 0),
                ("I", TYPE),
                ("T", TYPE),
                ("C", 0),
                ("x", 0),
                ("z", 0),
                ("W", TYPE),
                ("M", TYPE),
                ("Page", DEFAULT),
                ("ns", 0),
            ]
        );
        assert_eq!(s.stars, vec!["./all"]);
    }

    #[test]
    fn scans_multi_line_lists_default_aliases_and_ambient_modules() {
        let s = scan(
            "/// <reference path=\"./test.d.ts\" />\nexport {\n  a, // first\n  b as c,\n  Foo as default,\n};\n\
             export default Bar;\nexport default {\n};\ndeclare module \"bun:test\" {\n  export function describe(): void;\n  \
             const it: number;\n      const deep: number;\n}\ndeclare module \"*.svg\" {\n  export const x: string;\n}\n",
        );
        assert_eq!(
            names(&s),
            vec![("a", 0), ("c", 0), ("Foo", DEFAULT), ("Bar", DEFAULT), ("describe", 0), ("it", 0),]
        );
        assert_eq!(s.exports[4].2.as_deref(), Some("bun:test"));
        assert_eq!(s.refs, vec!["./test.d.ts"]);
    }

    fn index_of(list: &[(&str, u8, Source)]) -> Index {
        let mut b = Builder::default();
        for (n, f, s) in list {
            let id = b.source(s.clone());
            b.add(n, *f, id);
        }
        b.finish(Vec::new())
    }

    #[test]
    fn search_matches_abbreviations_and_ranks_prefixes_first() {
        let m = |s: &str| Source::Module(s.to_string());
        let idx = index_of(&[
            ("expectOk", 0, m("a")),
            ("expectErr", 0, m("a")),
            ("Export", 0, m("b")),
            ("exit", 0, m("c")),
            ("Metadata", TYPE, m("next")),
            ("metadataBase", 0, m("x")),
            ("expectOk", 0, m("a")),
        ]);
        assert_eq!(idx.len(), 6, "the duplicate is dropped");
        let hits: Vec<String> = idx.search("exok", 10).into_iter().map(|h| h.name).collect();
        assert_eq!(hits, vec!["expectOk"]);
        let hits: Vec<String> = idx.search("Meta", 10).into_iter().map(|h| h.name).collect();
        assert_eq!(hits, vec!["Metadata", "metadataBase"]);
        assert!(idx.search("zz", 10).is_empty());
        let exact = idx.exact("Metadata");
        assert_eq!(exact.len(), 1);
        assert!(exact[0].is_type && exact[0].source == m("next"));
        assert!(idx.exact("metadata").is_empty());
    }

    #[test]
    fn specifiers_prefer_short_relative_then_alias() {
        let aliases = vec![("@/".to_string(), PathBuf::from("/p"))];
        let spec = |t: &str, f: &str| file_specifier(Path::new(t), Path::new(f), &aliases);
        assert_eq!(spec("/p/src/a/b.ts", "/p/src/a/c.ts"), "./b");
        assert_eq!(spec("/p/src/a/sub/index.tsx", "/p/src/a/c.ts"), "./sub");
        assert_eq!(spec("/p/src/helpers/result.helpers.ts", "/p/app/page.tsx"), "@/src/helpers/result.helpers");
        assert_eq!(file_specifier(Path::new("/p/lib/x.ts"), Path::new("/p/app/deep/page.tsx"), &[]), "../../lib/x");
    }

    #[test]
    fn import_edit_adds_a_statement_after_the_last_import() {
        let text = "\"use client\";\nimport a from 'a'\nimport {\n  b,\n} from 'b'\n\nconst x = 1\n";
        assert_eq!(
            import_edit(text, "Metadata", "next", true, false),
            Some(Insert { line: 5, col: 0, text: "import type { Metadata } from 'next'\n".into() })
        );
        assert_eq!(
            import_edit(text, "Link", "next/link", false, true),
            Some(Insert { line: 5, col: 0, text: "import Link from 'next/link'\n".into() })
        );
    }

    #[test]
    fn import_edit_merges_into_an_existing_statement() {
        let one = "import { a, b } from \"m\";\n";
        assert_eq!(import_edit(one, "c", "m", false, false), Some(Insert { line: 0, col: 13, text: ", c".into() }));
        assert_eq!(import_edit(one, "T", "m", true, false), Some(Insert { line: 0, col: 13, text: ", type T".into() }));
        assert_eq!(import_edit(one, "b", "m", false, false), None, "already imported");
        let multi = "import {\n  a,\n  b,\n} from \"m\";\n";
        assert_eq!(import_edit(multi, "c", "m", false, false), Some(Insert { line: 2, col: 4, text: "\n  c,".into() }));
        let no_comma = "import {\n  a,\n  b\n} from \"m\";\n";
        assert_eq!(
            import_edit(no_comma, "c", "m", false, false),
            Some(Insert { line: 2, col: 3, text: ",\n  c".into() })
        );
        let types = "import type { A } from \"m\";\n";
        assert_eq!(
            import_edit(types, "v", "m", false, false),
            Some(Insert { line: 1, col: 0, text: "import { v } from \"m\";\n".into() }),
            "a value does not go into `import type`"
        );
        assert_eq!(import_edit(types, "B", "m", true, false), Some(Insert { line: 0, col: 15, text: ", B".into() }));
    }

    #[test]
    fn import_edit_without_imports_goes_below_directives() {
        assert_eq!(
            import_edit("const x = 1\n", "a", "m", false, false),
            Some(Insert { line: 0, col: 0, text: "import { a } from \"m\";\n\n".into() })
        );
        assert_eq!(
            import_edit("'use client'\n\nconst x = 1\n", "a", "m", false, false),
            Some(Insert { line: 1, col: 0, text: "import { a } from \"m\";\n".into() })
        );
        assert_eq!(
            import_edit("", "a", "m", false, false),
            Some(Insert { line: 0, col: 0, text: "import { a } from \"m\";\n".into() })
        );
    }

    /// Not a test: prints what the index finds in a real project.
    /// `LUMEN_IDX_ROOT=/path cargo test importidx::tests::real_project -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_project() {
        let root = PathBuf::from(std::env::var("LUMEN_IDX_ROOT").expect("LUMEN_IDX_ROOT"));
        let t = std::time::Instant::now();
        let idx = build(&root);
        let took = t.elapsed();
        let t = std::time::Instant::now();
        let again = build_from(&root, Some(&idx));
        println!("update with nothing changed: {:?}, {} files read", t.elapsed(), again.files_scanned);
        let bytes = idx.names.len() + idx.entries.len() * std::mem::size_of::<Entry>();
        println!(
            "{}: {} names, {} sources, {} files, {took:?}, ~{} KB",
            root.display(),
            idx.len(),
            idx.sources.len(),
            idx.files_scanned,
            bytes / 1024
        );
        let from = root.join("app/page.tsx");
        for name in std::env::var("LUMEN_IDX_NAMES").unwrap_or_default().split(',').filter(|n| !n.is_empty()) {
            let t = std::time::Instant::now();
            let hits = idx.exact(name);
            let specs: Vec<String> = hits.iter().take(5).map(|h| idx.specifier(&h.source, &from)).collect();
            println!("  exact {name:<14} {:>3} in {:?}: {specs:?}", hits.len(), t.elapsed());
        }
        for q in std::env::var("LUMEN_IDX_QUERIES").unwrap_or_default().split(',').filter(|n| !n.is_empty()) {
            let t = std::time::Instant::now();
            let hits = idx.search(q, 6);
            let took = t.elapsed();
            let shown: Vec<String> =
                hits.iter().map(|h| format!("{} <{}>", h.name, idx.specifier(&h.source, &from))).collect();
            println!("  search {q:<8} in {took:?}: {shown:?}");
        }
    }

    /// Every entry of an index, in its order.
    fn dump(idx: &Index) -> Vec<(String, Source, u8)> {
        idx.entries.iter().map(|e| (idx.name(e).to_string(), idx.sources[e.source as usize].clone(), e.flags)).collect()
    }

    /// A project on disk: two source files, and a package that re-exports a generated one.
    fn fixture(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-importidx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let nm = d.join("node_modules");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(nm.join("pkg")).unwrap();
        std::fs::create_dir_all(nm.join(".gen/client")).unwrap();
        std::fs::write(d.join("package.json"), r#"{"dependencies":{"pkg":"1"}}"#).unwrap();
        std::fs::write(d.join("src/a.ts"), "import { pkgThing } from 'pkg'\nexport const alpha = 1;\n").unwrap();
        std::fs::write(d.join("src/b.ts"), "export function beta() {}\n").unwrap();
        std::fs::write(nm.join("pkg/package.json"), r#"{"version":"1.0.0","types":"index.d.ts"}"#).unwrap();
        std::fs::write(
            nm.join("pkg/index.d.ts"),
            "export declare function pkgThing(): void;\nexport * from \".gen/client\";\nexport * from \"absent\";\n",
        )
        .unwrap();
        std::fs::write(nm.join(".gen/client/index.d.ts"), "export class GenOne {}\n").unwrap();
        d
    }

    #[test]
    fn an_update_reads_only_what_changed() {
        let d = fixture("update");
        let nm = d.join("node_modules");
        let idx = build(&d);
        assert_eq!((idx.files_scanned, idx.packages_scanned), (4, 1), "two sources, two declaration files");
        let found = |i: &Index, n: &str| !i.exact(n).is_empty();
        assert!(found(&idx, "alpha") && found(&idx, "beta") && found(&idx, "pkgThing") && found(&idx, "GenOne"));

        // Nothing changed: nothing is read again.
        let same = build_from(&d, Some(&idx));
        assert_eq!((same.files_scanned, same.packages_scanned), (0, 0));
        assert_eq!(dump(&same), dump(&idx));

        // One file edited, one created, one removed: two files are read, no package.
        std::fs::write(d.join("src/a.ts"), "export const alphaRenamed = 1;\n").unwrap();
        std::fs::write(d.join("src/c.ts"), "export type Gamma = 1;\n").unwrap();
        std::fs::remove_file(d.join("src/b.ts")).unwrap();
        let next = build_from(&d, Some(&same));
        assert_eq!((next.files_scanned, next.packages_scanned), (2, 0));
        assert!(!found(&next, "alpha") && !found(&next, "beta"));
        assert!(found(&next, "alphaRenamed") && found(&next, "Gamma") && found(&next, "pkgThing"));
        assert_eq!(dump(&next), dump(&build(&d)), "an updated index is what a fresh build makes");

        // A generated file a package re-exports changes under the same version: that package again.
        std::fs::write(nm.join(".gen/client/index.d.ts"), "export class GenOne {}\nexport class GenTwo {}\n").unwrap();
        let regen = build_from(&d, Some(&next));
        assert_eq!((regen.files_scanned, regen.packages_scanned), (2, 1));
        assert!(found(&regen, "GenTwo"));

        // A package that was re-exported but missing appears: again.
        std::fs::create_dir_all(nm.join("absent")).unwrap();
        std::fs::write(nm.join("absent/index.d.ts"), "export declare const wasAbsent: number;\n").unwrap();
        // (`node_modules` itself changed here, so this is the full path.)
        let appeared = build_from(&d, Some(&regen));
        assert!(found(&appeared, "wasAbsent"));

        // `package.json` lists another package: every package is scanned again, no project file.
        std::fs::create_dir_all(nm.join("other")).unwrap();
        std::fs::write(nm.join("other/package.json"), r#"{"version":"2.0.0","types":"index.d.ts"}"#).unwrap();
        std::fs::write(nm.join("other/index.d.ts"), "export declare const otherThing: number;\n").unwrap();
        std::fs::write(d.join("package.json"), r#"{"dependencies":{"pkg":"1","other":"2"}}"#).unwrap();
        let full = build_from(&d, Some(&appeared));
        assert_eq!((full.files_scanned, full.packages_scanned), (4, 2));
        assert!(found(&full, "otherThing") && found(&full, "pkgThing") && found(&full, "alphaRenamed"));
        assert_eq!(dump(&full), dump(&build(&d)));

        // An index of another project is no base for this one.
        let other_root = fixture("update-other");
        assert_eq!(build_from(&other_root, Some(&full)).packages_scanned, 1);
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&other_root).ok();
    }

    #[test]
    fn packed_names_keep_flags_and_groups() {
        let mut p = Packed::default();
        p.push("alpha", TYPE, 0);
        p.push("b", DEFAULT, 3);
        p.push(&"x".repeat(201), 0, 0);
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![("alpha", TYPE, 0), ("b", DEFAULT, 3)]);
    }

    #[test]
    fn builds_from_a_project_on_disk() {
        let d = std::env::temp_dir().join(format!("lumen-importidx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let nm = d.join("node_modules");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(nm.join("pkg/dist")).unwrap();
        std::fs::create_dir_all(nm.join("@types/bun")).unwrap();
        std::fs::write(d.join("package.json"), r#"{"dependencies":{"pkg":"1"},"devDependencies":{"@types/bun":"1"}}"#)
            .unwrap();
        std::fs::write(
            d.join("tsconfig.json"),
            "{ // comment\n \"compilerOptions\": {\"paths\": {\"@/*\": [\"./*\"],}} }",
        )
        .unwrap();
        std::fs::write(d.join("src/h.ts"), "export function expectOk() {}\n").unwrap();
        std::fs::write(d.join("src/types.d.ts"), "export type Ignored = 1;\n").unwrap();
        std::fs::write(
            nm.join("pkg/package.json"),
            r#"{"exports":{".":{"types":"./dist/index.d.ts"},"./sub":{"import":{"types":"./dist/sub.d.ts"}}}}"#,
        )
        .unwrap();
        std::fs::write(
            nm.join("pkg/dist/index.d.ts"),
            "export type { Metadata } from './meta.js';\nexport * from './more.js';\n",
        )
        .unwrap();
        std::fs::write(
            nm.join("pkg/dist/more.d.ts"),
            "export declare function more(): void;\nexport * from \".gen/client/default\";\n",
        )
        .unwrap();
        std::fs::create_dir_all(nm.join(".gen/client")).unwrap();
        std::fs::write(nm.join(".gen/client/default.d.ts"), "export * from \"./index\"\n").unwrap();
        std::fs::write(nm.join(".gen/client/index.d.ts"), "export class GenClient {}\n").unwrap();
        std::fs::write(nm.join("pkg/dist/sub.d.ts"), "export declare const sub: number;\n").unwrap();
        std::fs::write(nm.join("@types/bun/package.json"), r#"{"types":"index.d.ts"}"#).unwrap();
        std::fs::write(nm.join("@types/bun/index.d.ts"), "/// <reference path=\"./test.d.ts\" />\nexport = Bun;\ndeclare namespace Bun {\n    function useThing(): void;\n}\ndeclare namespace Other {\n    function hidden(): void;\n}\n").unwrap();
        std::fs::write(
            nm.join("@types/bun/test.d.ts"),
            "declare module \"bun:test\" {\n  export function describe(): void;\n}\nexport declare const notOfBun: number;\n",
        )
        .unwrap();
        let idx = build(&d);
        let src = |n: &str| idx.exact(n).into_iter().map(|h| h.source).collect::<Vec<_>>();
        assert_eq!(src("expectOk"), vec![Source::File(d.join("src/h.ts"))]);
        assert_eq!(src("Metadata"), vec![Source::Module("pkg".into())]);
        assert_eq!(src("more"), vec![Source::Module("pkg".into())]);
        assert_eq!(src("GenClient"), vec![Source::Module("pkg".into())], "a re-exported generated package");
        assert_eq!(src("sub"), vec![Source::Module("pkg/sub".into())]);
        assert_eq!(src("describe"), vec![Source::Module("bun:test".into())]);
        assert_eq!(src("useThing"), vec![Source::Module("bun".into())], "members of the `export =` namespace");
        assert!(src("hidden").is_empty(), "another namespace is not exported");
        assert!(src("notOfBun").is_empty(), "a referenced file is not part of the module");
        assert!(src("Ignored").is_empty(), "project .d.ts files are not import sources");
        assert_eq!(idx.specifier(&Source::File(d.join("src/h.ts")), &d.join("app/page.tsx")), "@/src/h");
        std::fs::remove_dir_all(&d).ok();
    }
}
