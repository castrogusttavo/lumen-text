//! TextMate grammar engine: a Rust port of the core of vscode-textmate.
//!
//! Not implemented yet: grammar `injections` / `injectionSelector`, and
//! rule-scoped `repository` shadowing (nested repositories are merged into the
//! grammar-level one).

use onig::{Region, Regex, RegexOptions, SearchOptions, Syntax};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI64, Ordering::Relaxed},
        Arc, OnceLock,
    },
};

// Thread-safety note: the whole engine is `Send` (asserted at the bottom of this file) so one
// `Tokenizer` can live behind an `Arc<Mutex<..>>` and be driven by a background worker as well as
// by the UI thread. Shared ownership therefore uses `Arc`; the two per-stack counters are
// relaxed atomics (they are only ever touched while the tokenizer mutex is held, the atomics just
// make the type `Sync`); the lazily compiled regex cache is a `OnceLock` (lock-free reads).

pub type RuleId = usize;
type Caps = Vec<Option<(usize, usize)>>;

// ───────────────────────────── scopes ─────────────────────────────

pub struct ScopeNode {
    pub parent: Scopes,
    pub id: u32,
    pub hash: u64,
}
pub type Scopes = Option<Arc<ScopeNode>>;

// ───────────────────────────── regex sources ─────────────────────────────

pub struct Source {
    pat: String,
    has_a: bool,
    has_g: bool,
    backref: bool,
    cache: [OnceLock<Option<Regex>>; 4],
}

impl Source {
    pub fn new(pat: &str) -> Arc<Source> {
        let b = pat.as_bytes();
        let (mut has_a, mut has_g, mut backref) = (false, false, false);
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 1 < b.len() {
                match b[i + 1] {
                    b'A' => has_a = true,
                    b'G' => has_g = true,
                    c if c.is_ascii_digit() => backref = true,
                    _ => {}
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        Arc::new(Source { pat: pat.to_string(), has_a, has_g, backref, cache: Default::default() })
    }

    fn regex(&self, allow_a: bool, allow_g: bool) -> Option<&Regex> {
        let a = !self.has_a || allow_a;
        let g = !self.has_g || allow_g;
        let idx = ((a as usize) << 1) | g as usize;
        self.cache[idx]
            .get_or_init(|| {
                let pat = if a && g { self.pat.clone() } else { resolve_anchors(&self.pat, a, g) };
                Regex::with_options(&pat, RegexOptions::REGEX_OPTION_CAPTURE_GROUP, Syntax::default()).ok()
            })
            .as_ref()
    }
}

fn resolve_anchors(pat: &str, allow_a: bool, allow_g: bool) -> String {
    let mut out = String::with_capacity(pat.len());
    let mut it = pat.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('A') if !allow_a => out.push('\u{FFFF}'),
                Some('G') if !allow_g => out.push('\u{FFFF}'),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn resolve_backrefs(pat: &str, line: &str, caps: &Caps) -> String {
    let mut out = String::with_capacity(pat.len());
    let b: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\\' && i + 1 < b.len() {
            if let Some(d) = b[i + 1].to_digit(10) {
                let text = caps.get(d as usize).copied().flatten().map(|(s, e)| &line[s..e]).unwrap_or("");
                for ch in text.chars() {
                    if "\\|(){}[]^$+*?.-#,".contains(ch) {
                        out.push('\\');
                    }
                    out.push(ch);
                }
                i += 2;
                continue;
            }
            out.push('\\');
            out.push(b[i + 1]);
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

// ───────────────────────────── rules ─────────────────────────────

enum Inc {
    SelfG,
    Base,
    Repo(String),
    Ext(String, Option<String>),
}

enum Kind {
    Match(Arc<Source>),
    BeginEnd { begin: Arc<Source>, end: Arc<Source>, apply_end_last: bool },
    BeginWhile { begin: Arc<Source>, while_: Arc<Source> },
    Container,
    Include(Inc),
}

#[derive(Clone, Default)]
struct CapRule {
    name: Option<String>,
    content_name: Option<String>,
    retok: Option<RuleId>,
}
type CapList = Vec<Option<CapRule>>;

struct Rule {
    grammar: usize,
    name: Option<String>,
    content_name: Option<String>,
    kind: Kind,
    patterns: Vec<RuleId>,
    caps_a: CapList, // match captures / begin captures
    caps_b: CapList, // end captures / while captures
}

struct Grammar {
    scope: String,
    root: RuleId,
    repo: HashMap<String, RuleId>,
}

pub struct Registry {
    rules: Vec<Arc<Rule>>,
    grammars: Vec<Grammar>,
    by_scope: HashMap<String, usize>,
    scope_paths: HashMap<String, PathBuf>,
    id_scope: HashMap<String, String>,
    flat: HashMap<(RuleId, usize), Arc<Vec<RuleId>>>,
    dyn_src: HashMap<String, Arc<Source>>,
    scope_names: Vec<Arc<str>>,
    scope_ids: HashMap<Arc<str>, u32>,
    root_dir: PathBuf,
}

impl Registry {
    pub fn new(assets: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(assets.join("language-manifest.json")).map_err(|e| e.to_string())?;
        let m: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let mut scope_paths = HashMap::new();
        let mut id_scope = HashMap::new();
        for l in m["languages"].as_array().into_iter().flatten() {
            let (Some(scope), Some(res), Some(id)) =
                (l["scopeName"].as_str(), l["resource"].as_str(), l["id"].as_str())
            else {
                continue;
            };
            scope_paths.insert(scope.to_string(), assets.join(res));
            id_scope.insert(id.to_string(), scope.to_string());
        }
        Ok(Self {
            rules: Vec::new(),
            grammars: Vec::new(),
            by_scope: HashMap::new(),
            scope_paths,
            id_scope,
            flat: HashMap::new(),
            dyn_src: HashMap::new(),
            scope_names: Vec::new(),
            scope_ids: HashMap::new(),
            root_dir: assets.to_path_buf(),
        })
    }

    pub fn scope_for_language(&self, id: &str) -> Option<String> {
        self.id_scope.get(id).cloned()
    }

    // ── interning / scope lists ──

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.scope_ids.get(s) {
            return i;
        }
        let rc: Arc<str> = Arc::from(s);
        let id = self.scope_names.len() as u32;
        self.scope_names.push(rc.clone());
        self.scope_ids.insert(rc, id);
        id
    }

    pub fn push_scope(&mut self, list: &Scopes, name: &str) -> Scopes {
        let mut cur = list.clone();
        for part in name.split_whitespace() {
            let id = self.intern(part);
            let parent_hash = cur.as_ref().map_or(0xcbf29ce484222325, |n| n.hash);
            let hash = (parent_hash ^ (id as u64 + 1)).wrapping_mul(0x100000001b3);
            cur = Some(Arc::new(ScopeNode { parent: cur, id, hash }));
        }
        cur
    }

    pub fn scope_strs<'a>(&'a self, list: &Scopes) -> Vec<&'a str> {
        let mut v = Vec::new();
        let mut cur = list.as_ref();
        while let Some(n) = cur {
            v.push(&*self.scope_names[n.id as usize]);
            cur = n.parent.as_ref();
        }
        v.reverse();
        v
    }

    // ── grammar loading ──

    pub fn load_scope(&mut self, scope: &str) -> Option<usize> {
        if let Some(&g) = self.by_scope.get(scope) {
            return Some(g);
        }
        let path = self.scope_paths.get(scope)?.clone();
        let text = std::fs::read_to_string(path).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        let g = self.grammars.len();
        self.grammars.push(Grammar { scope: scope.to_string(), root: 0, repo: HashMap::new() });
        self.by_scope.insert(scope.to_string(), g);

        let mut repo = HashMap::new();
        if let Some(r) = v["repository"].as_object() {
            for (k, rv) in r {
                let id = self.compile(g, rv, &mut repo);
                repo.insert(k.clone(), id);
            }
        }
        let patterns = self.compile_patterns(g, &v["patterns"], &mut repo);
        let root = self.add(Rule {
            grammar: g,
            name: None,
            content_name: None,
            kind: Kind::Container,
            patterns,
            caps_a: vec![],
            caps_b: vec![],
        });
        self.grammars[g].root = root;
        self.grammars[g].repo = repo;
        Some(g)
    }

    fn add(&mut self, r: Rule) -> RuleId {
        self.rules.push(Arc::new(r));
        self.rules.len() - 1
    }

    fn compile_patterns(&mut self, g: usize, v: &Value, repo: &mut HashMap<String, RuleId>) -> Vec<RuleId> {
        let mut out = Vec::new();
        for p in v.as_array().into_iter().flatten() {
            if p["disabled"].as_bool() == Some(true) {
                continue;
            }
            out.push(self.compile(g, p, repo));
        }
        out
    }

    fn compile_caps(&mut self, g: usize, v: &Value, repo: &mut HashMap<String, RuleId>) -> CapList {
        let mut list: CapList = Vec::new();
        let Some(o) = v.as_object() else { return list };
        for (k, cv) in o {
            let Ok(i) = k.parse::<usize>() else { continue };
            if list.len() <= i {
                list.resize(i + 1, None);
            }
            let retok = if cv["patterns"].is_array() {
                let patterns = self.compile_patterns(g, &cv["patterns"], repo);
                Some(self.add(Rule {
                    grammar: g,
                    name: None,
                    content_name: None,
                    kind: Kind::Container,
                    patterns,
                    caps_a: vec![],
                    caps_b: vec![],
                }))
            } else {
                None
            };
            list[i] = Some(CapRule {
                name: cv["name"].as_str().map(str::to_string),
                content_name: cv["contentName"].as_str().map(str::to_string),
                retok,
            });
        }
        list
    }

    fn compile(&mut self, g: usize, v: &Value, repo: &mut HashMap<String, RuleId>) -> RuleId {
        if let Some(o) = v["repository"].as_object() {
            for (k, rv) in o {
                let id = self.compile(g, rv, repo);
                repo.insert(k.clone(), id);
            }
        }
        let name = v["name"].as_str().map(str::to_string);
        let content_name = v["contentName"].as_str().map(str::to_string);
        let s = |k: &str| v[k].as_str();

        let mut rule = Rule { grammar: g, name, content_name, kind: Kind::Container, patterns: vec![], caps_a: vec![], caps_b: vec![] };

        if let Some(inc) = s("include") {
            rule.kind = Kind::Include(if inc == "$self" {
                Inc::SelfG
            } else if inc == "$base" {
                Inc::Base
            } else if let Some(n) = inc.strip_prefix('#') {
                Inc::Repo(n.to_string())
            } else if let Some((sc, n)) = inc.split_once('#') {
                Inc::Ext(sc.to_string(), Some(n.to_string()))
            } else {
                Inc::Ext(inc.to_string(), None)
            });
        } else if let Some(m) = s("match") {
            rule.kind = Kind::Match(Source::new(m));
            let caps = &v["captures"];
            rule.caps_a = self.compile_caps(g, caps, repo);
        } else if let Some(b) = s("begin") {
            let begin = Source::new(b);
            let both = self.compile_caps(g, &v["captures"], repo);
            let bc = if v["beginCaptures"].is_object() { self.compile_caps(g, &v["beginCaptures"], repo) } else { both.clone() };
            rule.caps_a = bc;
            rule.patterns = self.compile_patterns(g, &v["patterns"], repo);
            if let Some(w) = s("while") {
                rule.caps_b = if v["whileCaptures"].is_object() { self.compile_caps(g, &v["whileCaptures"], repo) } else { both };
                rule.kind = Kind::BeginWhile { begin, while_: Source::new(w) };
            } else {
                rule.caps_b = if v["endCaptures"].is_object() { self.compile_caps(g, &v["endCaptures"], repo) } else { both };
                rule.kind = Kind::BeginEnd {
                    begin,
                    end: Source::new(s("end").unwrap_or("\u{FFFF}")),
                    apply_end_last: v["applyEndPatternLast"].as_bool().unwrap_or(false) || v["applyEndPatternLast"].as_i64() == Some(1),
                };
            }
        } else {
            rule.patterns = self.compile_patterns(g, &v["patterns"], repo);
        }
        self.add(rule)
    }

    // ── include resolution / flattening ──

    fn resolve_include(&mut self, g: usize, inc: &Inc, base: usize) -> Option<RuleId> {
        match inc {
            Inc::SelfG => Some(self.grammars[g].root),
            Inc::Base => Some(self.grammars[base].root),
            Inc::Repo(n) => self.grammars[g].repo.get(n).copied(),
            Inc::Ext(scope, name) => {
                let g2 = self.load_scope(scope)?;
                match name {
                    None => Some(self.grammars[g2].root),
                    Some(n) => self.grammars[g2].repo.get(n).copied(),
                }
            }
        }
    }

    fn expand(&mut self, id: RuleId, base: usize, out: &mut Vec<RuleId>, seen: &mut HashSet<RuleId>) {
        if !seen.insert(id) {
            return;
        }
        let rule = self.rules[id].clone();
        match &rule.kind {
            Kind::Match(_) | Kind::BeginEnd { .. } | Kind::BeginWhile { .. } => out.push(id),
            Kind::Container => {
                for &p in &rule.patterns {
                    self.expand(p, base, out, seen);
                }
            }
            Kind::Include(inc) => {
                if let Some(t) = self.resolve_include(rule.grammar, inc, base) {
                    self.expand(t, base, out, seen);
                }
            }
        }
    }

    fn flatten(&mut self, owner: RuleId, base: usize) -> Arc<Vec<RuleId>> {
        if let Some(f) = self.flat.get(&(owner, base)) {
            return f.clone();
        }
        let rule = self.rules[owner].clone();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for &p in &rule.patterns {
            self.expand(p, base, &mut out, &mut seen);
        }
        let rc = Arc::new(out);
        self.flat.insert((owner, base), rc.clone());
        rc
    }

    fn dynamic_source(&mut self, pat: String) -> Arc<Source> {
        self.dyn_src.entry(pat.clone()).or_insert_with(|| Source::new(&pat)).clone()
    }
}

// ───────────────────────────── state stack ─────────────────────────────

pub struct Stack {
    parent: Option<Arc<Stack>>,
    rule: RuleId,
    enter: AtomicI64,
    anchor: AtomicI64,
    begin_eol: bool,
    end_rule: Option<Arc<Source>>,
    name_scopes: Scopes,
    content_scopes: Scopes,
}

impl Stack {
    fn reset(self: &Arc<Self>) {
        let mut cur = Some(self);
        while let Some(n) = cur {
            n.enter.store(-1, Relaxed);
            n.anchor.store(-1, Relaxed);
            cur = n.parent.as_ref();
        }
    }

    fn copy(&self) -> Stack {
        Stack {
            parent: self.parent.clone(),
            rule: self.rule,
            enter: AtomicI64::new(self.enter.load(Relaxed)),
            anchor: AtomicI64::new(self.anchor.load(Relaxed)),
            begin_eol: self.begin_eol,
            end_rule: self.end_rule.clone(),
            name_scopes: self.name_scopes.clone(),
            content_scopes: self.content_scopes.clone(),
        }
    }

    fn has_same_rule_as(&self, other: &Stack) -> bool {
        let mut el = Some(self);
        while let Some(e) = el {
            if e.enter.load(Relaxed) != other.enter.load(Relaxed) {
                break;
            }
            if e.rule == other.rule {
                return true;
            }
            el = e.parent.as_deref();
        }
        false
    }
}

pub struct Token {
    pub start: usize,
    pub end: usize,
    pub scopes: Scopes,
}

struct LineTokens {
    tokens: Vec<Token>,
    last: usize,
}

impl LineTokens {
    fn produce_scopes(&mut self, scopes: &Scopes, end: usize) {
        if self.last >= end {
            return;
        }
        self.tokens.push(Token { start: self.last, end, scopes: scopes.clone() });
        self.last = end;
    }
    fn produce(&mut self, stack: &Stack, end: usize) {
        self.produce_scopes(&stack.content_scopes, end);
    }
}

// ───────────────────────────── tokenizer ─────────────────────────────

/// Cheap multiplicative hasher: the cache keys are (pointer, flags), SipHash is overkill.
#[derive(Default)]
struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517cc1b727220a95);
        }
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (self.0.rotate_left(5) ^ n as u64).wrapping_mul(0x517cc1b727220a95);
    }
    fn write_u8(&mut self, n: u8) {
        self.0 = (self.0.rotate_left(5) ^ n as u64).wrapping_mul(0x517cc1b727220a95);
    }
}

/// (pattern identity, anchor flags) -> (searched from, start of the leftmost match if any).
type LineCache = HashMap<(usize, u8), (usize, Option<usize>), std::hash::BuildHasherDefault<FxHasher>>;

enum Cand {
    End,
    Rule(RuleId),
}

fn expand_name(name: &Option<String>, line: &str, caps: &Caps) -> Option<String> {
    let n = name.as_ref()?;
    if !n.contains('$') {
        return Some(n.clone());
    }
    let mut out = String::new();
    let b: Vec<char> = n.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$' {
            let braced = b.get(i + 1) == Some(&'{');
            let mut j = i + 1 + braced as usize;
            let st = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > st {
                let idx: usize = b[st..j].iter().collect::<String>().parse().unwrap_or(0);
                let mut text = caps.get(idx).copied().flatten().map(|(s, e)| line[s..e].to_string()).unwrap_or_default();
                if braced {
                    let mut k = j;
                    while k < b.len() && b[k] != '}' {
                        k += 1;
                    }
                    let modif: String = b[j..k].iter().collect();
                    if modif.ends_with("downcase") {
                        text = text.to_lowercase();
                    } else if modif.ends_with("upcase") {
                        text = text.to_uppercase();
                    }
                    j = k + 1;
                }
                out.push_str(text.trim_start_matches('.'));
                i = j;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Some(out)
}

pub struct Tokenizer {
    pub reg: Registry,
    base: usize,
}

impl Tokenizer {
    pub fn new(mut reg: Registry, scope: &str) -> Option<Self> {
        let base = reg.load_scope(scope)?;
        Some(Self { reg, base })
    }

    pub fn initial_state(&mut self) -> Arc<Stack> {
        let root = self.reg.grammars[self.base].root;
        let scope = self.reg.grammars[self.base].scope.clone();
        let scopes = self.reg.push_scope(&None, &scope);
        Arc::new(Stack {
            parent: None,
            rule: root,
            enter: AtomicI64::new(-1),
            anchor: AtomicI64::new(-1),
            begin_eol: false,
            end_rule: None,
            name_scopes: scopes.clone(),
            content_scopes: scopes,
        })
    }

    /// `line` must include its trailing '\n'.
    pub fn tokenize_line(&mut self, line: &str, stack: &Arc<Stack>) -> (Vec<Token>, Arc<Stack>) {
        stack.reset();
        self.reg.dyn_src.clear();
        let mut out = LineTokens { tokens: Vec::new(), last: 0 };
        let st = self.tokenize_string(line, true, 0, stack.clone(), &mut out, true);
        (out.tokens, st)
    }

    fn scan(
        &mut self,
        line: &str,
        pos: usize,
        top: &Arc<Stack>,
        allow_a: bool,
        allow_g: bool,
        cache: &mut LineCache,
    ) -> Option<(Cand, Arc<Caps>)> {
        let rule = self.reg.rules[top.rule].clone();
        let pats = self.reg.flatten(top.rule, self.base);
        let (end_src, end_last) = match &rule.kind {
            Kind::BeginEnd { end, apply_end_last, .. } => (Some(top.end_rule.clone().unwrap_or_else(|| end.clone())), *apply_end_last),
            _ => (None, false),
        };
        let rules = &self.reg.rules;
        let mut region = Region::new();

        // Leftmost match among all candidates; ties go to the earlier candidate. Only the start
        // is tracked here: captures are collected once, for the winner.
        let mut best: Option<(usize, Cand, Arc<Source>)> = None;
        let mut probe = |cand: Cand, src: &Arc<Source>, best: &mut Option<(usize, Cand, Arc<Source>)>| -> bool {
            let anchored = src.has_g;
            let key = (Arc::as_ptr(src) as usize, ((allow_a as u8) << 1) | (allow_g && anchored) as u8);
            let found: Option<usize> = match cache.get(&key) {
                Some((from, res)) if !anchored && *from <= pos && res.is_none_or(|s| s >= pos) => *res,
                _ => {
                    let res = src.regex(allow_a, allow_g).and_then(|re| re.search_with_options(line, pos, line.len(), SearchOptions::SEARCH_OPTION_NONE, Some(&mut region)));
                    cache.insert(key, (pos, res));
                    res
                }
            };
            if let Some(start) = found {
                if best.as_ref().is_none_or(|b| start < b.0) {
                    *best = Some((start, cand, src.clone()));
                }
                return start == pos;
            }
            false
        };

        let mut done = false;
        if !end_last {
            if let Some(e) = &end_src {
                done = probe(Cand::End, e, &mut best);
            }
        }
        if !done {
            for &p in pats.iter() {
                let src = match &rules[p].kind {
                    Kind::Match(s) => s,
                    Kind::BeginEnd { begin, .. } | Kind::BeginWhile { begin, .. } => begin,
                    _ => continue,
                };
                if probe(Cand::Rule(p), src, &mut best) {
                    done = true;
                    break;
                }
            }
        }
        if !done && end_last {
            if let Some(e) = &end_src {
                probe(Cand::End, e, &mut best);
            }
        }

        let (_, cand, src) = best?;
        // Re-run the winner to get its capture groups.
        let re = src.regex(allow_a, allow_g)?;
        re.search_with_options(line, pos, line.len(), SearchOptions::SEARCH_OPTION_NONE, Some(&mut region))?;
        let caps: Caps = (0..region.len()).map(|i| region.pos(i)).collect();
        Some((cand, Arc::new(caps)))
    }

    fn handle_captures(&mut self, line: &str, is_first: bool, stack: &Arc<Stack>, out: &mut LineTokens, caps_rules: &CapList, caps: &Caps) {
        if caps_rules.is_empty() {
            return;
        }
        let n = caps_rules.len().min(caps.len());
        let mut local: Vec<(Scopes, usize)> = Vec::new();
        let max_end = caps[0].map_or(0, |c| c.1);
        for i in 0..n {
            let Some(cr) = &caps_rules[i] else { continue };
            let Some((cs, ce)) = caps[i] else { continue };
            if ce == cs {
                continue;
            }
            if cs > max_end {
                break;
            }
            while let Some((sc, end)) = local.last().cloned() {
                if end > cs {
                    break;
                }
                out.produce_scopes(&sc, end);
                local.pop();
            }
            match local.last() {
                Some((sc, _)) => {
                    let sc = sc.clone();
                    out.produce_scopes(&sc, cs)
                }
                None => out.produce(stack, cs),
            }

            if let Some(rid) = cr.retok {
                let name = expand_name(&cr.name, line, caps);
                let ns = match name {
                    Some(n) => self.reg.push_scope(&stack.content_scopes, &n),
                    None => stack.content_scopes.clone(),
                };
                let cn = expand_name(&cr.content_name, line, caps);
                let cs_list = match cn {
                    Some(n) => self.reg.push_scope(&ns, &n),
                    None => ns.clone(),
                };
                let clone = Arc::new(Stack {
                    parent: Some(stack.clone()),
                    rule: rid,
                    enter: AtomicI64::new(cs as i64),
                    anchor: AtomicI64::new(-1),
                    begin_eol: false,
                    end_rule: None,
                    name_scopes: ns,
                    content_scopes: cs_list,
                });
                let sub = &line[..ce];
                self.tokenize_string(sub, is_first && cs == 0, cs, clone, out, false);
                continue;
            }
            if let Some(n) = expand_name(&cr.name, line, caps) {
                let base = local.last().map(|l| l.0.clone()).unwrap_or_else(|| stack.content_scopes.clone());
                let sc = self.reg.push_scope(&base, &n);
                local.push((sc, ce));
            }
        }
        while let Some((sc, end)) = local.pop() {
            out.produce_scopes(&sc, end);
        }
    }

    fn tokenize_string(
        &mut self,
        line: &str,
        mut is_first: bool,
        mut pos: usize,
        mut stack: Arc<Stack>,
        out: &mut LineTokens,
        check_while: bool,
    ) -> Arc<Stack> {
        let len = line.len();
        let mut anchor: i64 = -1;
        let mut cache: LineCache = LineCache::default();

        if check_while {
            anchor = if stack.begin_eol { 0 } else { -1 };
            let mut whiles: Vec<Arc<Stack>> = Vec::new();
            let mut cur = Some(stack.clone());
            while let Some(n) = cur {
                if matches!(self.reg.rules[n.rule].kind, Kind::BeginWhile { .. }) {
                    whiles.push(n.clone());
                }
                cur = n.parent.clone();
            }
            while let Some(w) = whiles.pop() {
                let rule = self.reg.rules[w.rule].clone();
                let Kind::BeginWhile { while_, .. } = &rule.kind else { continue };
                let src = w.end_rule.clone().unwrap_or_else(|| while_.clone());
                let m = src.regex(is_first, anchor == pos as i64).and_then(|re| {
                    let mut region = Region::new();
                    re.search_with_options(line, pos, len, SearchOptions::SEARCH_OPTION_NONE, Some(&mut region))?;
                    Some((0..region.len()).map(|i| region.pos(i)).collect::<Caps>())
                });
                match m {
                    Some(caps) if caps[0].is_some_and(|c| c.0 == pos || true) => {
                        let (cs, ce) = caps[0].unwrap();
                        out.produce(&w, cs);
                        self.handle_captures(line, is_first, &w, out, &rule.caps_b, &caps);
                        out.produce(&w, ce);
                        anchor = ce as i64;
                        if ce > pos {
                            pos = ce;
                            is_first = false;
                        }
                    }
                    _ => {
                        stack = w.parent.clone().unwrap_or(w);
                        break;
                    }
                }
            }
        }

        let mut guard = 0usize;
        loop {
            guard += 1;
            if guard > len * 4 + 64 {
                out.produce(&stack, len);
                return stack;
            }
            let top = stack.clone();
            let Some((cand, caps)) = self.scan(line, pos, &top, is_first, anchor == pos as i64, &mut cache) else {
                out.produce(&stack, len);
                return stack;
            };
            let (ms, me) = caps[0].unwrap();
            let advanced = me > pos;

            match cand {
                Cand::End => {
                    let rule = self.reg.rules[stack.rule].clone();
                    out.produce(&stack, ms);
                    let mut s2 = stack.copy();
                    s2.content_scopes = s2.name_scopes.clone();
                    let s2 = Arc::new(s2);
                    self.handle_captures(line, is_first, &s2, out, &rule.caps_b, &caps);
                    out.produce(&s2, me);
                    let popped = stack.clone();
                    stack = stack.parent.clone().unwrap_or_else(|| stack.clone());
                    anchor = popped.anchor.load(Relaxed);
                    if !advanced && popped.enter.load(Relaxed) == pos as i64 {
                        stack = popped;
                        out.produce(&stack, len);
                        return stack;
                    }
                }
                Cand::Rule(rid) => {
                    let rule = self.reg.rules[rid].clone();
                    out.produce(&stack, ms);
                    let before = stack.clone();
                    let name = expand_name(&rule.name, line, &caps);
                    let ns = match name {
                        Some(n) => self.reg.push_scope(&stack.content_scopes, &n),
                        None => stack.content_scopes.clone(),
                    };
                    let mut node = Stack {
                        parent: Some(stack.clone()),
                        rule: rid,
                        enter: AtomicI64::new(pos as i64),
                        anchor: AtomicI64::new(anchor),
                        begin_eol: me == len,
                        end_rule: None,
                        name_scopes: ns.clone(),
                        content_scopes: ns.clone(),
                    };
                    match &rule.kind {
                        Kind::BeginEnd { end, .. } => {
                            let pushed = Arc::new(node.copy());
                            stack = pushed;
                            self.handle_captures(line, is_first, &stack, out, &rule.caps_a, &caps);
                            out.produce(&stack, me);
                            anchor = me as i64;
                            let cn = expand_name(&rule.content_name, line, &caps);
                            let cl = match cn {
                                Some(n) => self.reg.push_scope(&ns, &n),
                                None => ns.clone(),
                            };
                            node.content_scopes = cl;
                            if end.backref {
                                let resolved = resolve_backrefs(&end.pat, line, &caps);
                                node.end_rule = Some(self.reg.dynamic_source(resolved));
                            }
                            stack = Arc::new(node);
                            if !advanced && before.has_same_rule_as(&stack) {
                                stack = stack.parent.clone().unwrap();
                                out.produce(&stack, len);
                                return stack;
                            }
                        }
                        Kind::BeginWhile { while_, .. } => {
                            let pushed = Arc::new(node.copy());
                            stack = pushed;
                            self.handle_captures(line, is_first, &stack, out, &rule.caps_a, &caps);
                            out.produce(&stack, me);
                            anchor = me as i64;
                            let cn = expand_name(&rule.content_name, line, &caps);
                            let cl = match cn {
                                Some(n) => self.reg.push_scope(&ns, &n),
                                None => ns.clone(),
                            };
                            node.content_scopes = cl;
                            if while_.backref {
                                let resolved = resolve_backrefs(&while_.pat, line, &caps);
                                node.end_rule = Some(self.reg.dynamic_source(resolved));
                            }
                            stack = Arc::new(node);
                            if !advanced && before.has_same_rule_as(&stack) {
                                stack = stack.parent.clone().unwrap();
                                out.produce(&stack, len);
                                return stack;
                            }
                        }
                        _ => {
                            let tmp = Arc::new(node);
                            self.handle_captures(line, is_first, &tmp, out, &rule.caps_a, &caps);
                            out.produce(&tmp, me);
                            // A match rule is popped immediately.
                            if !advanced {
                                stack = before;
                                out.produce(&stack, len);
                                return stack;
                            }
                        }
                    }
                }
            }
            if me > pos {
                pos = me;
                is_first = false;
            }
        }
    }
}

// Compile-time proof that the tokenizer and its states can cross threads.
const _: () = {
    const fn send<T: Send>() {}
    const fn send_sync<T: Send + Sync>() {}
    send::<Tokenizer>();
    send_sync::<Stack>();
};

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(lang: &str) -> Tokenizer {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let reg = Registry::new(&assets).unwrap();
        let scope = reg.scope_for_language(lang).unwrap();
        Tokenizer::new(reg, &scope).unwrap()
    }

    fn dump(t: &mut Tokenizer, lines: &[&str]) -> Vec<Vec<(String, String)>> {
        let mut st = t.initial_state();
        let mut all = Vec::new();
        for l in lines {
            let text = format!("{l}\n");
            let (toks, next) = t.tokenize_line(&text, &st);
            st = next;
            all.push(
                toks.iter()
                    .map(|k| (text[k.start..k.end].to_string(), t.reg.scope_strs(&k.scopes).join(" ")))
                    .collect(),
            );
        }
        all
    }

    #[test]
    fn typescript_basics() {
        let mut t = tk("typescript");
        let r = dump(&mut t, &["const x: number = 1; // hi"]);
        let find = |s: &str| r[0].iter().find(|(txt, _)| txt == s).map(|(_, sc)| sc.clone()).unwrap_or_default();
        assert!(find("const").contains("storage.type"), "const -> {}", find("const"));
        assert!(find("number").contains("support.type.primitive"), "number -> {}", find("number"));
        assert!(find("1").contains("constant.numeric"), "1 -> {}", find("1"));
        assert!(r[0].iter().any(|(_, sc)| sc.contains("comment.line")), "{:?}", r[0]);
    }

    #[test]
    fn multiline_state_carries_over() {
        let mut t = tk("typescript");
        let r = dump(&mut t, &["/* a", "b */ let y = 2;"]);
        assert!(r[1][0].1.contains("comment.block"), "{:?}", r[1]);
        assert!(r[1].iter().any(|(txt, sc)| txt == "let" && sc.contains("storage.type")), "{:?}", r[1]);
    }

    #[test]
    fn template_string_embedded_expression() {
        let mut t = tk("typescript");
        let r = dump(&mut t, &["const s = `a ${b + 1} c`;"]);
        assert!(r[0].iter().any(|(txt, sc)| txt == "b" && sc.contains("meta.template.expression")), "{:?}", r[0]);
    }

    #[test]
    fn json_and_rust_load() {
        let mut j = tk("json");
        let r = dump(&mut j, &["{\"a\": [1, true, null]}"]);
        assert!(r[0].iter().any(|(_, sc)| sc.contains("constant.numeric")), "{:?}", r[0]);
        let mut rs = tk("rust");
        let r = dump(&mut rs, &["fn main() { let x = 5; }"]);
        assert!(r[0].iter().any(|(txt, sc)| txt == "fn" && sc.contains("keyword")), "{:?}", r[0]);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// Not a correctness test: prints tokenizer throughput on a TS-like file.
    #[test]
    fn throughput() {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let reg = Registry::new(&assets).unwrap();
        let scope = reg.scope_for_language("typescript").unwrap();
        let mut t = Tokenizer::new(reg, &scope).unwrap();
        let mut st = t.initial_state();
        let line = "export function fn123(a: number, b: string): string { return b + a * 123; }\n";
        let n = 5000;
        let t0 = Instant::now();
        for _ in 0..n {
            let (_, next) = t.tokenize_line(line, &st);
            st = next;
        }
        let el = t0.elapsed();
        eprintln!("[bench] {n} lines in {el:?} = {:.0} lines/s, {:.1} us/line", n as f64 / el.as_secs_f64(), el.as_secs_f64() * 1e6 / n as f64);
    }
}
