//! Incremental highlighter. Tokenization runs on a BACKGROUND THREAD (one worker per
//! `Highlighter`, spawned lazily); the UI thread only (a) tells the worker how far it needs states,
//! (b) re-tokenizes the few visible lines from their saved start state when drawing.
//!
//! Thread model
//! * One `Tokenizer` per language lives in an `Arc<TokenizerCell>` (grammar + compiled regexes are
//!   expensive, so every document of a language shares it). Its engine types are `Send` (Arc/atomics),
//!   so a plain `Mutex` is enough; no per-thread regex caches are needed (`onig::Regex` is Send+Sync).
//! * The worker owns a `Rope` snapshot (`Buffer::snapshot`, O(1)), starts at the last published
//!   line-start state and tokenizes forward, publishing states in ~1 ms batches into
//!   `Shared.job.states` (the only state shared with the UI) and bumping `Shared.ready`.
//! * The tokenizer mutex is held for ONE line at a time. The UI announces itself in `ui_waiting`;
//!   the worker never queues on the mutex (it `try_lock`s and backs off while the UI waits or
//!   holds it), so the UI waits for at most the line currently being tokenized (~0.1 ms).
//! * `invalidate(line)` truncates the states and drops the snapshot. A job in flight is validated
//!   when it publishes: entries at or before the lowest invalidated line are kept (they only depend
//!   on unchanged text), the rest is discarded and the job ends; the next `advance` installs a
//!   fresh snapshot and the worker resumes from the last valid state.
//! * `advance` is cheap: it records the target, installs a snapshot if needed, wakes the worker and
//!   (only when the viewport is just a few lines ahead of the frontier) waits idle, bounded by its
//!   budget, so freshly edited text is usually coloured in the same frame instead of flashing plain.
//! * Dropping the `Highlighter` stops and joins the worker.

use crate::{
    buffer::Buffer,
    lsp::Wake,
    textmate::{Registry, Stack, Tokenizer},
    theme::{Rgba, Theme},
};
use ropey::Rope;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering::{Acquire, Relaxed, Release}},
        Arc, Condvar, Mutex, MutexGuard, TryLockError,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const MAX_LINE_BYTES: usize = 20_000;
/// The worker tokenizes this many lines beyond what the viewport asked for (smooth scrolling).
const LOOKAHEAD: usize = 400;
/// `advance` blocks (idle, up to its budget) for the worker only when the gap is at most this.
const WAIT_GAP: usize = 256;
/// Lines this close to the frontier stay plain for a frame instead of being tokenized on the UI thread.
const NEAR: usize = 256;
const PUBLISH_EVERY: Duration = Duration::from_millis(1);
const WAKE_EVERY: Duration = Duration::from_millis(8);

/// (byte start, byte end, color, style flags: `ui::BOLD | ITALIC | UNDERLINE`).
pub type Spans = Rc<Vec<(usize, usize, Rgba, u8)>>;

/// A tokenizer plus the "UI wants the lock" flag the worker yields to.
pub struct TokenizerCell {
    m: Mutex<Tokenizer>,
    ui_waiting: AtomicUsize,
}

/// One tokenizer (grammar + compiled rules) is shared by every document of a language.
pub type SharedTokenizer = Arc<TokenizerCell>;

pub fn make_tokenizer(assets: &std::path::Path, scope: &str) -> Option<SharedTokenizer> {
    let reg = Registry::new(assets).ok()?;
    Some(Arc::new(TokenizerCell { m: Mutex::new(Tokenizer::new(reg, scope)?), ui_waiting: AtomicUsize::new(0) }))
}

struct UiWait<'a>(&'a AtomicUsize);
impl Drop for UiWait<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Release);
    }
}

/// Lock held by the UI thread. Field order matters: the mutex is released before the flag drops,
/// so the worker cannot slip in between two consecutive UI locks that were taken back to back.
struct UiGuard<'a> {
    g: MutexGuard<'a, Tokenizer>,
    _w: UiWait<'a>,
}
impl Deref for UiGuard<'_> {
    type Target = Tokenizer;
    fn deref(&self) -> &Tokenizer {
        &self.g
    }
}
impl DerefMut for UiGuard<'_> {
    fn deref_mut(&mut self) -> &mut Tokenizer {
        &mut self.g
    }
}

impl TokenizerCell {
    fn lock_ui(&self) -> UiGuard<'_> {
        self.ui_waiting.fetch_add(1, Acquire);
        let w = UiWait(&self.ui_waiting);
        let g = self.m.lock().unwrap_or_else(|p| p.into_inner());
        UiGuard { g, _w: w }
    }

    /// Worker side: never waits in the mutex queue, yields to the UI. `None` = stop requested.
    fn lock_bg(&self, stop: &AtomicBool) -> Option<MutexGuard<'_, Tokenizer>> {
        loop {
            if stop.load(Relaxed) {
                return None;
            }
            if self.ui_waiting.load(Acquire) == 0 {
                match self.m.try_lock() {
                    Ok(g) => return Some(g),
                    Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                    Err(TryLockError::WouldBlock) => {}
                }
            }
            std::thread::sleep(Duration::from_micros(30));
        }
    }
}

fn is_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Same text as `Buffer::line` (no line break), for a snapshot.
fn rope_line(r: &Rope, l: usize) -> String {
    if l >= r.len_lines() {
        return String::new();
    }
    let s = r.line(l);
    let mut n = s.len_chars();
    if n > 0 && is_break(s.char(n - 1)) {
        let lf = s.char(n - 1) == '\n';
        n -= 1;
        if lf && n > 0 && s.char(n - 1) == '\r' {
            n -= 1;
        }
    }
    s.slice(..n).to_string()
}

/// State after tokenizing `text` (no line break) from `st`.
fn step(tk: &mut Tokenizer, mut text: String, st: &Arc<Stack>) -> Arc<Stack> {
    if text.len() > MAX_LINE_BYTES {
        return st.clone(); // too long: leave plain, keep state
    }
    text.push('\n');
    tk.tokenize_line(&text, st).1
}

// ───────────────────────────── shared state ─────────────────────────────

struct Job {
    /// states[i] = tokenizer state at the start of line i (the published prefix).
    states: Vec<Arc<Stack>>,
    /// How many states are wanted (set by `advance`).
    want: usize,
    /// Snapshot for the worker; `None` after an invalidate until the next `advance`.
    rope: Option<Rope>,
    /// While a job runs, every `invalidate(line)` appends `line + 1` (the first state index that
    /// became invalid). Cleared whenever a job starts or the worker goes idle.
    inval: Vec<usize>,
    running: bool,
}

struct Shared {
    job: Mutex<Job>,
    work_cv: Condvar,         // the worker waits here for work
    progress_cv: Condvar,     // the UI waits here (bounded) for published states
    ready: AtomicUsize,       // == job.states.len(), readable without the lock
    inval_count: AtomicUsize, // invalidates while running (cheap per-line staleness check)
    stop: AtomicBool,
    wake: Mutex<Option<Wake>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Job> {
        self.job.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn wake(&self) {
        let w = self.wake.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(w) = w {
            w();
        }
    }
}

fn worker(shared: Arc<Shared>, tk: SharedTokenizer) {
    loop {
        // ── wait for a job: snapshot, start state, target ──
        let (rope, mut cur, mut base, mut want, count_mark) = {
            let mut j = shared.lock();
            loop {
                if shared.stop.load(Relaxed) {
                    return;
                }
                if j.rope.is_some() && j.states.len() < j.want {
                    break;
                }
                j.running = false;
                j.inval.clear();
                j = shared.work_cv.wait(j).unwrap_or_else(|p| p.into_inner());
            }
            j.running = true;
            j.inval.clear();
            let mark = shared.inval_count.load(Acquire);
            (j.rope.clone().unwrap(), j.states.last().unwrap().clone(), j.states.len(), j.want, mark)
        };
        let total_lines = rope.len_lines();

        // ── tokenize forward; entry k of `batch` is the state with index base + k ──
        let mut batch: Vec<Arc<Stack>> = Vec::new();
        let mut t_batch = Instant::now();
        let mut t_wake = Instant::now();
        loop {
            let next_idx = base + batch.len();
            let done = next_idx >= want || next_idx > total_lines;
            let stale = shared.inval_count.load(Acquire) != count_mark;
            if done || stale || batch.len() >= 512 || t_batch.elapsed() >= PUBLISH_EVERY {
                let mut j = shared.lock();
                if shared.stop.load(Relaxed) {
                    return;
                }
                // States below the lowest invalidated index are still valid; keep them only if the
                // published prefix still ends exactly where this batch begins.
                let limit = j.inval.iter().copied().min().unwrap_or(usize::MAX);
                let invalidated = limit != usize::MAX;
                let n_valid = limit.saturating_sub(base).min(batch.len());
                if j.states.len() == base && n_valid > 0 {
                    j.states.extend(batch.drain(..n_valid));
                    shared.ready.store(j.states.len(), Release);
                    shared.progress_cv.notify_all();
                }
                let contiguous = j.states.len() == base + n_valid;
                want = j.want;
                let finished = done || invalidated || !contiguous;
                drop(j);
                if finished || t_wake.elapsed() >= WAKE_EVERY {
                    shared.wake();
                    t_wake = Instant::now();
                }
                if finished {
                    break; // a fresh job (new snapshot / target) is picked up at the top
                }
                base += n_valid;
                batch.clear();
                t_batch = Instant::now();
                continue;
            }

            let text = rope_line(&rope, next_idx - 1);
            let Some(mut g) = tk.lock_bg(&shared.stop) else { return };
            // A panic inside the engine must not kill the worker (the UI would wait forever).
            let next = catch_unwind(AssertUnwindSafe(|| step(&mut g, text, &cur))).unwrap_or_else(|_| cur.clone());
            drop(g);
            cur = next.clone();
            batch.push(next);
        }
    }
}

// ───────────────────────────── highlighter ─────────────────────────────

pub struct Highlighter {
    tk: SharedTokenizer,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    first: Arc<Stack>, // state at the start of line 0
    line_cache: HashMap<usize, Spans>,
    color_cache: HashMap<u64, (Rgba, u8)>,
    /// Provisional start states for a window far beyond the tokenized prefix (assumes the window
    /// starts at top level; replaced by real states as tokenization catches up). Built on the
    /// calling thread: it is the one place the UI tokenizes, only for far jumps.
    prov: Option<(usize, Vec<Arc<Stack>>)>,
}

impl Highlighter {
    pub fn new(tk: SharedTokenizer) -> Self {
        let first = tk.lock_ui().initial_state();
        let shared = Arc::new(Shared {
            job: Mutex::new(Job { states: vec![first.clone()], want: 0, rope: None, inval: Vec::new(), running: false }),
            work_cv: Condvar::new(),
            progress_cv: Condvar::new(),
            ready: AtomicUsize::new(1),
            inval_count: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            wake: Mutex::new(None),
        });
        Self { tk, shared, worker: None, first, line_cache: HashMap::new(), color_cache: HashMap::new(), prov: None }
    }

    /// Optional: called from the worker (rate limited) whenever new states were published, so an
    /// event-driven UI can redraw. Without it the UI's `ready() <= last` polling still works.
    #[allow(dead_code)]
    pub fn set_wake(&mut self, wake: Wake) {
        *self.shared.wake.lock().unwrap_or_else(|p| p.into_inner()) = Some(wake);
    }

    pub fn theme_changed(&mut self) {
        self.color_cache.clear();
        self.line_cache.clear();
    }

    /// Lines at or after `line` changed: drop their saved states so they are re-tokenized
    /// (the state at the start of `line` itself is still valid).
    pub fn invalidate(&mut self, line: usize) {
        {
            let mut j = self.shared.lock();
            j.states.truncate(line + 1);
            j.rope = None; // the worker needs a fresh snapshot (installed by the next `advance`)
            if j.running {
                j.inval.push(line + 1);
                self.shared.inval_count.fetch_add(1, Release);
            }
            self.shared.ready.store(j.states.len(), Release);
        }
        self.line_cache.clear();
        self.prov = None;
    }

    /// Number of lines whose start state is known.
    pub fn ready(&self) -> usize {
        self.shared.ready.load(Acquire)
    }

    /// Make sure the background worker covers `until` (plus lookahead). Cheap: it never tokenizes
    /// on this thread (except the synchronous fallback if the worker thread cannot be spawned);
    /// `budget` only bounds an idle wait used when the viewport is a few lines ahead of the frontier.
    pub fn advance(&mut self, buf: &Buffer, until: usize, budget: Duration) {
        let lc = buf.line_count();
        let until = until.min(lc);
        let need = until + 1;
        let want = (need + LOOKAHEAD).min(lc + 1);
        let mut j = self.shared.lock();
        if j.rope.is_none() {
            j.rope = Some(buf.snapshot());
        }
        j.want = want;
        if j.states.len() >= want {
            return;
        }
        if self.worker.is_none() && !self.shared.stop.load(Relaxed) {
            let (sh, tk) = (self.shared.clone(), self.tk.clone());
            self.worker = std::thread::Builder::new().name("hl-worker".into()).stack_size(16 << 20).spawn(move || worker(sh, tk)).ok();
        }
        if self.worker.is_none() {
            // Fallback: tokenize here, within the budget (the pre-worker behaviour).
            let t0 = Instant::now();
            let mut tk = self.tk.lock_ui();
            while j.states.len() < need {
                let i = j.states.len() - 1;
                let next = step(&mut tk, buf.line(i), &j.states[i]);
                j.states.push(next);
                if t0.elapsed() >= budget {
                    break;
                }
            }
            self.shared.ready.store(j.states.len(), Release);
            return;
        }
        self.shared.work_cv.notify_one();
        let len = j.states.len();
        if len < need && need - len <= WAIT_GAP && !budget.is_zero() {
            let _ = self.shared.progress_cv.wait_timeout_while(j, budget, |j| j.states.len() < need);
        }
    }

    /// Start state for line `i` in the provisional window, (re)building the window if needed.
    fn provisional_state(&mut self, buf: &Buffer, i: usize) -> Option<Arc<Stack>> {
        let covered = self.prov.as_ref().is_some_and(|(s, v)| i >= *s && i < *s + v.len());
        if !covered {
            let start = i.saturating_sub(40);
            let end = (i + 160).min(buf.line_count());
            let mut tk = self.tk.lock_ui();
            let mut v = vec![self.first.clone()];
            for l in start..end {
                let next = step(&mut tk, buf.line(l), v.last().unwrap());
                v.push(next);
            }
            self.prov = Some((start, v));
        }
        let (s, v) = self.prov.as_ref()?;
        v.get(i - s).cloned()
    }

    pub fn spans(&mut self, buf: &Buffer, theme: &Theme, i: usize) -> Option<Spans> {
        let provisional = i >= self.ready();
        let start_state = if provisional {
            {
                // The worker is on its way and this line is close: stay plain for a frame rather
                // than tokenize on the UI thread (the next redraw picks the real state up).
                let j = self.shared.lock();
                if j.want > j.states.len() && i < j.states.len() + NEAR {
                    return None;
                }
            }
            self.provisional_state(buf, i)?
        } else {
            self.prov = None;
            self.shared.lock().states.get(i)?.clone()
        };
        if !provisional {
            if let Some(s) = self.line_cache.get(&i) {
                return Some(s.clone());
            }
            if self.line_cache.len() > 600 {
                self.line_cache.clear();
            }
        }
        let mut text = buf.line(i);
        let n = text.len();
        let spans: Vec<(usize, usize, Rgba, u8)> = if n > MAX_LINE_BYTES {
            vec![(0, n, theme.foreground, 0)]
        } else {
            text.push('\n');
            let mut tk = self.tk.lock_ui();
            let (tokens, _) = tk.tokenize_line(&text, &start_state);
            let mut v = Vec::with_capacity(tokens.len());
            for t in tokens {
                let (s, e) = (t.start.min(n), t.end.min(n));
                if s >= e {
                    continue;
                }
                let key = t.scopes.as_ref().map_or(0, |x| x.hash);
                let (color, flags) = match self.color_cache.get(&key) {
                    Some(c) => *c,
                    None => {
                        let strs = tk.reg.scope_strs(&t.scopes);
                        let st = theme.resolve(&strs);
                        let f = (if st.bold { crate::ui::BOLD } else { 0 }) | (if st.italic { crate::ui::ITALIC } else { 0 }) | (if st.underline { crate::ui::UNDERLINE } else { 0 });
                        let c = (st.fg.unwrap_or(theme.foreground), f);
                        self.color_cache.insert(key, c);
                        c
                    }
                };
                v.push((s, e, color, flags));
            }
            v
        };
        let rc = Rc::new(spans);
        if !provisional {
            self.line_cache.insert(i, rc.clone());
        }
        Some(rc)
    }

    #[cfg(test)]
    fn probe(&self) -> std::sync::Weak<Shared> {
        Arc::downgrade(&self.shared)
    }
}

impl Drop for Highlighter {
    fn drop(&mut self) {
        self.shared.stop.store(true, Release);
        drop(self.shared.lock()); // the worker either saw `stop` or is already parked on the condvar
        self.shared.work_cv.notify_all();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

/// Tokenize a standalone snippet (hover popups, previews) with a shared tokenizer: for every line
/// the `(byte start, byte end, color, style flags)` runs, same colors as the editor.
pub fn highlight_text(tk: &SharedTokenizer, theme: &Theme, text: &str) -> Vec<Vec<(usize, usize, Rgba, u8)>> {
    let mut g = tk.lock_ui();
    let mut st = g.initial_state();
    let mut cache: HashMap<u64, (Rgba, u8)> = HashMap::new();
    let mut out = Vec::new();
    for line in text.split('\n') {
        let n = line.len();
        if n > MAX_LINE_BYTES {
            out.push(vec![(0, n, theme.foreground, 0)]);
            continue;
        }
        let mut l = String::with_capacity(n + 1);
        l.push_str(line);
        l.push('\n');
        let (tokens, next) = g.tokenize_line(&l, &st);
        st = next;
        let mut v = Vec::with_capacity(tokens.len());
        for t in tokens {
            let (s, e) = (t.start.min(n), t.end.min(n));
            if s >= e {
                continue;
            }
            let key = t.scopes.as_ref().map_or(0, |x| x.hash);
            let (color, flags) = match cache.get(&key) {
                Some(c) => *c,
                None => {
                    let strs = g.reg.scope_strs(&t.scopes);
                    let style = theme.resolve(&strs);
                    let f = (if style.bold { crate::ui::BOLD } else { 0 }) | (if style.italic { crate::ui::ITALIC } else { 0 }) | (if style.underline { crate::ui::UNDERLINE } else { 0 });
                    let c = (style.fg.unwrap_or(theme.foreground), f);
                    cache.insert(key, c);
                    c
                }
            };
            v.push((s, e, color, flags));
        }
        out.push(v);
    }
    out
}

pub fn comment_prefix(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "py" | "sh" | "bash" | "zsh" | "toml" | "yaml" | "yml" | "rb" | "conf" => "#",
        "lua" | "sql" => "--",
        "css" | "scss" | "html" | "htm" | "md" | "json" => "",
        _ => "//",
    }
}

pub fn language_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "json" | "jsonc" => "json",
        "rs" => "rust",
        "py" => "python",
        "go" => "go",
        "md" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sh" | "bash" | "zsh" => "shellscript",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "swift" => "swift",
        "lua" => "lua",
        "rb" => "ruby",
        "php" => "php",
        "sql" => "sql",
        "vue" => "vue",
        "svelte" => "svelte",
        "zig" => "zig",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{EditKind, Sel};
    use std::path::{Path, PathBuf};

    fn assets() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
    }

    fn ts_line(i: usize) -> String {
        match i % 9 {
            0 => format!("export function fn{i}(a: number, b: string): string {{ return b + a * {i}; }}\n"),
            1 => format!("const value{i}: Record<string, number> = {{ alpha: {i}, beta: 'text {i}' }};\n"),
            2 => format!("// comment number {i} describing what happens in the next block of code\n"),
            3 => format!("interface Item{i} {{ id: number; name: string; tags?: string[] }}\n"),
            4 => format!("    if (value{} > {i}) {{ console.log(`item ${{value{}}}`); }}\n", i - 3, i - 3),
            5 => format!("class Service{i} extends Base implements Runnable {{ run(): void {{}} }}\n"),
            6 => format!("const tpl{i} = `multi {i}\n"),
            7 => format!("  still inside ${{tpl{}}} the template`;\n", i - 1),
            _ => format!("const arrow{i} = async (x: number) => await fetch(`/api/{i}/${{x}}`); /* c {i} */\n"),
        }
    }

    fn gen_text(lines: usize) -> String {
        (0..lines).map(ts_line).collect()
    }

    fn theme() -> Theme {
        Theme::load(&assets().join("themes/dark-plus.json")).unwrap()
    }

    fn wait_ready(hl: &Highlighter, n: usize, secs: u64) {
        let t0 = Instant::now();
        while hl.ready() < n {
            assert!(t0.elapsed() < Duration::from_secs(secs), "worker stalled at {} of {n}", hl.ready());
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Reference: a fresh synchronous Tokenizer run line by line (the pre-worker algorithm).
    fn reference(buf: &Buffer, theme: &Theme) -> Vec<Vec<(usize, usize, Rgba, u8)>> {
        let reg = Registry::new(&assets()).unwrap();
        let scope = reg.scope_for_language("typescript").unwrap();
        let mut t = Tokenizer::new(reg, &scope).unwrap();
        let mut st = t.initial_state();
        let mut all = Vec::new();
        for i in 0..buf.line_count() {
            let mut text = buf.line(i);
            let n = text.len();
            text.push('\n');
            let (tokens, next) = t.tokenize_line(&text, &st);
            st = next;
            let mut v = Vec::new();
            for tok in tokens {
                let (s, e) = (tok.start.min(n), tok.end.min(n));
                if s >= e {
                    continue;
                }
                let strs = t.reg.scope_strs(&tok.scopes);
                let st = theme.resolve(&strs);
                let f = (if st.bold { crate::ui::BOLD } else { 0 }) | (if st.italic { crate::ui::ITALIC } else { 0 }) | (if st.underline { crate::ui::UNDERLINE } else { 0 });
                v.push((s, e, st.fg.unwrap_or(theme.foreground), f));
            }
            all.push(v);
        }
        all
    }

    fn check_all(hl: &mut Highlighter, buf: &Buffer, theme: &Theme, what: &str) {
        let want = reference(buf, theme);
        for (i, w) in want.iter().enumerate() {
            let got = hl.spans(buf, theme, i).unwrap_or_else(|| panic!("{what}: no spans for line {i}"));
            assert_eq!(&got[..], &w[..], "{what}: line {i} differs: {:?}", buf.line(i));
        }
    }

    #[test]
    fn background_matches_synchronous_tokenizer() {
        let buf = Buffer::from_str(&gen_text(5_000));
        let theme = theme();
        let mut hl = Highlighter::new(make_tokenizer(&assets(), "source.ts").unwrap());
        let t0 = Instant::now();
        hl.advance(&buf, buf.line_count(), Duration::ZERO);
        wait_ready(&hl, buf.line_count() + 1, 120);
        eprintln!("[hl] background tokenization: {:.0} lines/s", 5_000.0 / t0.elapsed().as_secs_f64());
        check_all(&mut hl, &buf, &theme, "background");
    }

    #[test]
    fn edit_during_job_gives_correct_spans() {
        let mut buf = Buffer::from_str(&gen_text(5_000));
        let theme = theme();
        let mut hl = Highlighter::new(make_tokenizer(&assets(), "source.ts").unwrap());
        hl.advance(&buf, buf.line_count(), Duration::ZERO);
        wait_ready(&hl, 600, 60); // mid-job
        // Open a block comment on a line the worker has not reached yet and one it already passed.
        for (k, line) in [4_000usize, 300, 4_500].into_iter().enumerate() {
            let p = buf.line_start(line);
            if k == 2 {
                buf.replace(p, p, "*/ ", EditKind::Typing, Sel::caret(p));
            } else {
                buf.replace(p, p, "/* ", EditKind::Typing, Sel::caret(p));
            }
            let l = buf.take_dirty_line().unwrap();
            hl.invalidate(l);
            hl.advance(&buf, buf.line_count(), Duration::ZERO);
            std::thread::sleep(Duration::from_millis(5 * k as u64));
        }
        wait_ready(&hl, buf.line_count() + 1, 120);
        check_all(&mut hl, &buf, &theme, "after edits");
        // And once more with the job finished: edit, invalidate, resume.
        let p = buf.line_start(10);
        buf.replace(p, p, "`", EditKind::Typing, Sel::caret(p));
        hl.invalidate(buf.take_dirty_line().unwrap());
        hl.advance(&buf, buf.line_count(), Duration::ZERO);
        wait_ready(&hl, buf.line_count() + 1, 120);
        check_all(&mut hl, &buf, &theme, "after late edit");
    }

    #[test]
    fn big_file_background_does_not_block_ui() {
        let buf = Buffer::from_str(&gen_text(100_000));
        let theme = theme();
        let mut hl = Highlighter::new(make_tokenizer(&assets(), "source.ts").unwrap());
        let t0 = Instant::now();
        hl.advance(&buf, 100_000, Duration::from_millis(8));
        let adv = t0.elapsed();
        eprintln!("[hl] advance(100k) on UI thread: {:.2} ms", adv.as_secs_f64() * 1e3);
        assert!(adv.as_millis() < 50, "advance not cheap: {adv:?}");

        // ready() grows while the test thread does nothing but sleep.
        wait_ready(&hl, 200, 30);
        // Baseline: the same 50-line frame with the worker idle (it stops at the viewport target).
        hl.advance(&buf, 60, Duration::ZERO);
        std::thread::sleep(Duration::from_millis(100));
        let mut idle = Duration::ZERO;
        for f in 0..20 {
            hl.theme_changed();
            let t = Instant::now();
            for i in 0..50 {
                let _ = hl.spans(&buf, &theme, i + (f % 3));
            }
            idle += t.elapsed();
        }
        eprintln!("[hl] 50-line frame, worker idle: avg {:.2} ms", idle.as_secs_f64() * 1e3 / 20.0);
        hl.advance(&buf, 100_000, Duration::ZERO);
        let (r0, t_r0) = (hl.ready(), Instant::now());
        // Simulated frames: visible lines re-coloured while the worker is busy.
        let mut worst = Duration::ZERO;
        let mut sum = Duration::ZERO;
        let frames = 40;
        for f in 0..frames {
            hl.theme_changed(); // empty the span cache: every visible line is tokenized again
            hl.advance(&buf, 100_000, Duration::from_millis(8));
            let t = Instant::now();
            for i in 0..50 {
                let _ = hl.spans(&buf, &theme, i + (f % 3));
            }
            let el = t.elapsed();
            worst = worst.max(el);
            sum += el;
            std::thread::sleep(Duration::from_millis(10));
        }
        let single = {
            hl.theme_changed();
            let t = Instant::now();
            let s = hl.spans(&buf, &theme, 20);
            assert!(s.is_some());
            t.elapsed()
        };
        eprintln!(
            "[hl] 50-line frame while worker busy: avg {:.2} ms, worst {:.2} ms; single line {:.2} ms",
            sum.as_secs_f64() * 1e3 / frames as f64,
            worst.as_secs_f64() * 1e3,
            single.as_secs_f64() * 1e3
        );
        assert!(single.as_millis() < 50, "spans() blocked behind the worker: {single:?}");
        assert!(worst.as_millis() < 250, "frame too slow while worker busy: {worst:?}");
        let (r1, dt) = (hl.ready(), t_r0.elapsed());
        eprintln!("[hl] worker progress: {} lines in {:.2} s = {:.0} lines/s while the UI re-coloured", r1 - r0, dt.as_secs_f64(), (r1 - r0) as f64 / dt.as_secs_f64());
        assert!(r1 > r0 + 200, "worker made no progress ({r0} -> {r1})");

        // Far jump: provisional colours immediately, even though the frontier is far behind.
        let t = Instant::now();
        let far = hl.spans(&buf, &theme, 99_000);
        eprintln!("[hl] provisional @99000: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        assert!(far.is_some());
    }

    #[test]
    fn highlight_text_colors_a_snippet_like_the_editor() {
        let tk = make_tokenizer(&assets(), "source.ts").unwrap();
        let th = theme();
        let src = "const a = 1;\n/* open\nstill comment */ let b";
        let lines = highlight_text(&tk, &th, src);
        assert_eq!(lines.len(), 3);
        // `const` and the number get different colors than the identifier between them
        let color_at = |l: usize, byte: usize| lines[l].iter().find(|s| s.0 <= byte && byte < s.1).map(|s| s.2);
        assert_ne!(color_at(0, 0), color_at(0, 6), "keyword vs identifier");
        assert_ne!(color_at(0, 10), color_at(0, 6), "number vs identifier");
        // the block comment carries over to the next line
        assert_eq!(color_at(1, 1), color_at(2, 3), "comment state continues");
        assert_ne!(color_at(2, 3), color_at(2, 20), "code after the comment");
    }

    #[test]
    fn dropping_highlighter_stops_worker() {
        let buf = Buffer::from_str(&gen_text(100_000));
        let tk = make_tokenizer(&assets(), "source.ts").unwrap();
        let mut hl = Highlighter::new(tk.clone());
        let probe = hl.probe();
        hl.advance(&buf, 100_000, Duration::ZERO);
        wait_ready(&hl, 100, 30);
        assert!(probe.upgrade().is_some());
        let t0 = Instant::now();
        drop(hl); // joins the worker mid-job
        eprintln!("[hl] drop with a busy worker: {:.2} ms", t0.elapsed().as_secs_f64() * 1e3);
        assert!(probe.upgrade().is_none(), "worker still holds the shared state after drop");
        // The shared tokenizer is unlocked and usable, and nobody else keeps it alive.
        assert_eq!(Arc::strong_count(&tk), 1);
        assert!(tk.m.try_lock().is_ok());
    }
}
