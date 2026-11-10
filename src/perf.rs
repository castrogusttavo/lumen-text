//! Performance regression tests. Thresholds are deliberately generous (slow CI machines);
//! they only catch large regressions. Each test prints its measured number to stderr.
#![allow(dead_code)]

/// Frame-profile laps for `perf::tests::frame_breakdown`: each `lap!("name")` charges the wall time since the
/// previous lap (or [`prof::start`]) to `name`, so the laps of one frame partition it exactly. Test builds only:
/// release builds contain no `lap!` calls at all.
#[cfg(test)]
pub(crate) mod prof {
    use std::{cell::RefCell, collections::BTreeMap, time::Instant};

    thread_local! {
        static ACC: RefCell<BTreeMap<&'static str, (f64, u64)>> = const { RefCell::new(BTreeMap::new()) };
        static LAST: RefCell<Option<Instant>> = const { RefCell::new(None) };
    }

    /// Begin a lap sequence (also discards time spent outside of the measured frame).
    pub fn start() {
        LAST.with(|l| *l.borrow_mut() = Some(Instant::now()));
    }

    pub fn lap(name: &'static str) {
        let now = Instant::now();
        let Some(prev) = LAST.with(|l| l.borrow_mut().replace(now)) else { return };
        ACC.with(|a| {
            let mut a = a.borrow_mut();
            let e = a.entry(name).or_insert((0.0, 0));
            e.0 += now.duration_since(prev).as_secs_f64();
            e.1 += 1;
        });
    }

    /// Take and clear the accumulated (total seconds, calls) per lap name.
    pub fn take() -> BTreeMap<&'static str, (f64, u64)> {
        LAST.with(|l| *l.borrow_mut() = None);
        ACC.with(|a| std::mem::take(&mut *a.borrow_mut()))
    }
}

/// `lap!("name");` charges the time since the previous lap to `name` in test builds and expands to nothing otherwise.
macro_rules! lap {
    ($name:expr) => {
        #[cfg(test)]
        crate::perf::prof::lap($name);
    };
}
pub(crate) use lap;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use crate::{
        buffer::{Buffer, EditKind, Sel},
        editor::Editor,
        highlight::{Highlighter, make_tokenizer},
        mem,
        textmate::{Registry, Tokenizer},
        theme::Theme,
    };
    use std::{
        path::{Path, PathBuf},
        time::Instant,
    };

    // Startup budgets, ~3x the best of a few runs on the development machine (Intel N305, release build):
    //   App::new on a small project   60 ms    first frame 7 ms        (in process, wall)
    //   process start to first frame  94 ms    into a 100k-line file 111 ms   (headless binary, wall)
    // Fonts dominate: `App::new` is `TextCtx::new` (fontconfig scan, UI and code families); opening and
    // tokenizing the visible window of a 100k-line file adds only ~17 ms.
    const STARTUP_NEW_BUDGET: f64 = 0.25;
    const STARTUP_FRAME_BUDGET: f64 = 0.10;
    const LAUNCH_SMALL_BUDGET: f64 = 0.30;
    const LAUNCH_BIG_BUDGET: f64 = 0.35;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn assets() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
    }

    fn ts_line(i: usize) -> String {
        match i % 8 {
            0 => format!("export function fn{i}(a: number, b: string): string {{ return b + a * {i}; }}\n"),
            1 => format!("const value{i}: Record<string, number> = {{ alpha: {i}, beta: 'text {i}' }};\n"),
            2 => format!("// comment number {i} describing what happens in the next block of code\n"),
            3 => format!("interface Item{i} {{ id: number; name: string; tags?: string[] }}\n"),
            4 => format!("    if (value{} > {i}) {{ console.log(`item ${{value{}}}`); }}\n", i - 3, i - 3),
            5 => format!("class Service{i} extends Base implements Runnable {{ run(): void {{}} }}\n"),
            6 => format!("import {{ helper{i}, other{i} }} from './module{i}';\n"),
            _ => format!("const arrow{i} = async (x: number) => await fetch(`/api/{i}/${{x}}`);\n"),
        }
    }

    fn gen_text(lines: usize) -> String {
        let mut s = String::with_capacity(lines * 80);
        for i in 0..lines {
            s.push_str(&ts_line(i));
        }
        s
    }

    fn big_file() -> (tempdir::Dir, Buffer, f64) {
        let dir = tempdir::Dir::new("big");
        let path = dir.path.join("big.ts");
        std::fs::write(&path, gen_text(100_000)).unwrap();
        let t0 = Instant::now();
        let buf = Buffer::open(&path).unwrap();
        (dir, buf, t0.elapsed().as_secs_f64())
    }

    mod tempdir {
        use std::path::PathBuf;
        pub struct Dir {
            pub path: PathBuf,
        }
        impl Dir {
            pub fn new(tag: &str) -> Self {
                // Unique per call: tests run in parallel and each drops (deletes) its own dir.
                static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!("lumen-perf-{tag}-{}-{n}", std::process::id()));
                std::fs::create_dir_all(&path).unwrap();
                Dir { path }
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    #[test]
    fn buffer_open_edit_and_line_access() {
        let (_dir, mut buf, open) = big_file();
        eprintln!("[perf] open 100k lines ({} bytes): {:.0} ms", buf.len_bytes(), open * 1e3);
        assert_eq!(buf.line_count(), 100_001);
        assert!(open < 1.5, "open too slow: {open:.2}s");

        let mut rng = Rng(0x9E3779B97F4A7C15);
        let n = 2000;
        let t0 = Instant::now();
        for k in 0..n {
            let mid = buf.len_chars() / 2;
            let p = mid + rng.below(2000);
            let sel = Sel::caret(p);
            if k % 2 == 0 {
                buf.replace(p, p, "x", EditKind::Typing, sel);
            } else {
                buf.replace(p, p + 1, "", EditKind::Delete, sel);
            }
        }
        let avg = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        eprintln!("[perf] mid-file edit: {avg:.1} us avg over {n}");
        assert!(avg < 200.0, "edit too slow: {avg:.1} us");

        let lc = buf.line_count();
        let t0 = Instant::now();
        let mut total = 0usize;
        for _ in 0..10_000 {
            total += buf.line(rng.below(lc)).len();
        }
        let el = t0.elapsed();
        eprintln!("[perf] 10k random line(i): {:.1} ms (sum {total})", el.as_secs_f64() * 1e3);
        assert!(el.as_secs_f64() < 0.25, "line() too slow: {el:?}");
    }

    #[test]
    fn tokenizer_throughput() {
        let reg = Registry::new(&assets()).unwrap();
        let scope = reg.scope_for_language("typescript").unwrap();
        let mut t = Tokenizer::new(reg, &scope).unwrap();
        let mut st = t.initial_state();
        let n = 3000;
        let lines: Vec<String> = (0..n).map(ts_line).collect();
        // CPU time of this thread, so other tests/agents competing for cores don't fail it.
        let cpu = thread_cpu;
        let t0 = cpu();
        for l in &lines {
            let (_, next) = t.tokenize_line(l, &st);
            st = next;
        }
        let rate = n as f64 / (cpu() - t0).max(1e-6);
        eprintln!("[perf] tokenizer: {rate:.0} lines/s");
        assert!(rate >= 2500.0, "tokenizer too slow: {rate:.0} lines/s");
    }

    #[test]
    fn highlighter_provisional_window() {
        let buf = Buffer::from_str(&gen_text(100_000));
        let tk = make_tokenizer(&assets(), "source.ts").expect("tokenizer");
        let theme = Theme::load(&assets().join("themes/dark-plus.json")).unwrap();
        let mut hl = Highlighter::new(tk);
        assert!(hl.ready() < 100);
        // CPU time of this thread (the provisional window is tokenized on the calling thread), so
        // other tests/agents competing for cores cannot make it flaky.
        let cpu = thread_cpu;
        let t0 = cpu();
        let spans = hl.spans(&buf, &theme, 90_000);
        let el = cpu() - t0;
        eprintln!("[perf] provisional spans @90000: {:.1} ms (cpu)", el * 1e3);
        assert!(spans.is_some(), "no provisional spans");
        assert!(el < 0.3, "provisional window too slow: {:.0} ms cpu", el * 1e3);
    }

    #[test]
    fn editor_ops() {
        let mut buf = Buffer::from_str(&gen_text(10_000));
        let mut ed = Editor::new(&buf, "//");
        ed.sel = Sel::caret(buf.line_start(5000));
        let t0 = Instant::now();
        for _ in 0..1000 {
            ed.type_char(&mut buf, 'a');
        }
        for _ in 0..1000 {
            ed.backspace(&mut buf);
        }
        let el = t0.elapsed();
        eprintln!("[perf] 1000 type_char + 1000 backspace: {:.1} ms", el.as_secs_f64() * 1e3);
        assert!(el.as_millis() < 150, "typing too slow: {el:?}");

        let mut buf = Buffer::from_str(&gen_text(20_000));
        let mut ed = Editor::new(&buf, "//");
        ed.sel = Sel { anchor: 0, head: 0 };
        let p = buf.line_start(1) + 7; // inside "const value1"
        ed.sel = Sel::caret(p);
        let t0 = Instant::now();
        ed.select_all_occurrences(&mut buf);
        let el = t0.elapsed();
        eprintln!(
            "[perf] select_all_occurrences 20k lines: {:.1} ms ({} cursors)",
            el.as_secs_f64() * 1e3,
            ed.extra.len() + 1
        );
        assert!(el.as_millis() < 300, "select_all_occurrences too slow: {el:?}");
    }

    /// CPU time of this thread, so other tests/agents competing for cores cannot make a test flaky.
    fn thread_cpu() -> f64 {
        #[cfg(unix)]
        {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
            ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
        }
        #[cfg(not(unix))]
        {
            // No per-thread CPU clock here: wall time since the first call (good enough for a budget check).
            static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
        }
    }

    /// A headless `App` (fixture lock held in the returned guard) with a 100k-line file active,
    /// drawn once so layout and highlighting windows exist.
    fn big_app(tag: &str) -> (crate::app_unsaved::testkit::Guard, crate::app::App, Vec<u32>, (i32, i32)) {
        let guard = crate::app_unsaved::testkit::lock();
        let dir = crate::app_unsaved::testkit::tmp(tag);
        let mut app = crate::app_unsaved::testkit::test_app(&dir);
        let path = dir.join("big.ts");
        std::fs::write(&path, gen_text(100_000)).unwrap();
        let t0 = thread_cpu();
        let idx = app.open_path(&path, None).expect("open big.ts");
        let open = thread_cpu() - t0;
        eprintln!("[perf] App::open_path 100k lines: {:.0} ms cpu", open * 1e3);
        assert!(open < 2.0, "opening a 100k-line file in the app too slow: {open:.2}s cpu");
        app.active = idx;
        app.size = (1100, 700);
        let (w, h) = (1100, 700);
        let mut px = vec![0u32; (w * h) as usize];
        for _ in 0..3 {
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let _ = std::fs::remove_dir_all(&dir);
        (guard, app, px, (w, h))
    }

    /// Cost of one drawn frame while scrolling through a 100k-line file.
    #[test]
    fn render_frame_while_scrolling() {
        let (_g, mut app, mut px, (w, h)) = big_app("scroll");
        let n = 120;
        let t0 = thread_cpu();
        for i in 0..n {
            let d = &mut app.docs[app.active];
            d.top = (i * 700) as f64;
            d.target = d.top;
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let avg = (thread_cpu() - t0) * 1e3 / n as f64;
        eprintln!("[perf] frame while scrolling 100k lines: {avg:.2} ms cpu avg over {n}");
        // Every frame jumps 700 lines into untokenized territory, so each one tokenizes its visible lines
        // (~160 us/line) on this thread. Measured ~15 ms (was ~43 ms).
        assert!(avg < 40.0, "frame too slow: {avg:.1} ms cpu");
    }

    /// Let the background tokenizer get past line `upto` (steady state for a file that has been open a while).
    fn settle(app: &mut crate::app::App, px: &mut [u32], w: i32, h: i32, upto: usize) {
        let deadline = Instant::now() + std::time::Duration::from_secs(40);
        while Instant::now() < deadline {
            app.pump_background();
            app.tick();
            app.render_into(&mut crate::ui::Canvas { px, w, h }, 0.016);
            let d = &app.docs[app.active];
            if d.hl.as_ref().is_none_or(|h| h.ready() > upto) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Steady state: the file is fully tokenized, the view scrolls a few lines per frame (wheel).
    #[test]
    fn render_frame_steady_scroll() {
        let (_g, mut app, mut px, (w, h)) = big_app("steady");
        settle(&mut app, &mut px, w, h, 31_000);
        let n = 240;
        let t0 = thread_cpu();
        for i in 0..n {
            let d = &mut app.docs[app.active];
            d.top = 30_000.0 + (i * 3) as f64;
            d.target = d.top;
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let avg = (thread_cpu() - t0) * 1e3 / n as f64;
        eprintln!("[perf] steady scroll (3 lines/frame, tokenized): {avg:.2} ms cpu avg over {n}");
        // Measured ~4.5 ms (was ~11 ms before the minimap row cache and the lazy provisional window).
        assert!(avg < 12.0, "frame too slow: {avg:.1} ms cpu");
    }

    /// The steady scroll budget also holds with a taller line height, a medium weight and ligatures
    /// (advanced shaping), the costliest font settings.
    #[test]
    fn render_frame_steady_scroll_other_fonts() {
        let (_g, mut app, mut px, (w, h)) = big_app("steadyfonts");
        app.apply_setting("buffer_line_height", &serde_json::json!(1.9));
        app.apply_setting("buffer_font_weight", &serde_json::json!(500));
        app.apply_setting("buffer_font_ligatures", &serde_json::json!(true));
        settle(&mut app, &mut px, w, h, 31_000);
        let n = 240;
        let t0 = thread_cpu();
        for i in 0..n {
            let d = &mut app.docs[app.active];
            d.top = 30_000.0 + (i * 3) as f64;
            d.target = d.top;
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let avg = (thread_cpu() - t0) * 1e3 / n as f64;
        eprintln!("[perf] steady scroll, line height 1.9 + weight 500 + ligatures: {avg:.2} ms cpu avg over {n}");
        assert!(avg < 16.0, "frame too slow with non-default fonts: {avg:.1} ms cpu");
    }

    /// Typing latency with a taller line height and no line numbers.
    #[test]
    fn typing_steady_other_line_height() {
        use crate::app::KeyIn;
        use winit::keyboard::{Key, SmolStr};
        let (_g, mut app, mut px, (w, h)) = big_app("typingfonts");
        app.apply_setting("buffer_line_height", &serde_json::json!(2.0));
        app.apply_setting("show_line_numbers", &serde_json::json!(false));
        settle(&mut app, &mut px, w, h, 50_200);
        {
            let d = &mut app.docs[app.active];
            let p = d.buf.line_start(50_000);
            d.ed.sel = Sel::caret(p);
            d.top = 49_990.0;
            d.target = d.top;
        }
        app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        let n = 200;
        let t0 = thread_cpu();
        for _ in 0..n {
            let ki = KeyIn { key: Key::Character(SmolStr::new("a")), text: Some("a".into()) };
            app.handle_key(&ki);
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let avg = (thread_cpu() - t0) * 1e3 / n as f64;
        eprintln!("[perf] steady keystroke + frame, line height 2.0, no numbers: {avg:.2} ms cpu avg over {n}");
        assert!(avg < 25.0, "typing latency too high with non-default line height: {avg:.1} ms cpu per key");
    }

    /// Steady state typing: the file is fully tokenized, a key is typed mid-file.
    #[test]
    fn typing_steady() {
        use crate::app::KeyIn;
        use winit::keyboard::{Key, SmolStr};
        let (_g, mut app, mut px, (w, h)) = big_app("typingsteady");
        settle(&mut app, &mut px, w, h, 50_200);
        {
            let d = &mut app.docs[app.active];
            let p = d.buf.line_start(50_000);
            d.ed.sel = Sel::caret(p);
            d.top = 49_990.0;
            d.target = d.top;
        }
        app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        let n = 200;
        let t0 = thread_cpu();
        for _ in 0..n {
            let ki = KeyIn { key: Key::Character(SmolStr::new("a")), text: Some("a".into()) };
            app.handle_key(&ki);
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let avg = (thread_cpu() - t0) * 1e3 / n as f64;
        eprintln!("[perf] steady keystroke + frame: {avg:.2} ms cpu avg over {n}");
        // Measured ~10 ms (was ~47 ms); the rest is tokenizing the visible lines below the edit on this thread.
        assert!(avg < 25.0, "typing latency too high: {avg:.1} ms cpu per key");
    }

    /// Typing latency: handle a key and draw the frame that shows it, in the middle of 100k lines.
    #[test]
    fn typing_latency_100k_lines() {
        use crate::app::KeyIn;
        use winit::keyboard::{Key, SmolStr};
        let (_g, mut app, mut px, (w, h)) = big_app("typing");
        {
            let d = &mut app.docs[app.active];
            let p = d.buf.line_start(50_000);
            d.ed.sel = Sel::caret(p);
            d.top = 49_990.0;
            d.target = d.top;
        }
        app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        let n = 200;
        let t0 = thread_cpu();
        for _ in 0..n {
            let ki = KeyIn { key: Key::Character(SmolStr::new("a")), text: Some("a".into()) };
            app.handle_key(&ki);
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let avg = (thread_cpu() - t0) * 1e3 / n as f64;
        eprintln!("[perf] keystroke + frame at line 50000 of 100k: {avg:.2} ms cpu avg over {n}");
        assert_eq!(app.docs[app.active].buf.line_count(), 100_001);
        // Measured ~12 ms (was ~51 ms).
        assert!(avg < 30.0, "typing latency too high: {avg:.1} ms cpu per key");
    }

    /// Where a frame goes: wall time per phase (the `lap!` points in `App::draw_all` and `Doc::draw`) for a
    /// steady-scroll frame, a keystroke frame and an idle frame in a 100k-line file. Informational: it prints
    /// the table (run with `--nocapture`) and only checks that the phases add up to the frame.
    #[test]
    fn frame_breakdown() {
        use crate::app::KeyIn;
        use winit::keyboard::{Key, SmolStr};
        let (_g, mut app, mut px, (w, h)) = big_app("breakdown");
        settle(&mut app, &mut px, w, h, 50_200);
        {
            let d = &mut app.docs[app.active];
            let p = d.buf.line_start(50_000);
            d.ed.sel = Sel::caret(p);
            d.top = 49_990.0;
            d.target = d.top;
        }
        app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        let report = |what: &str, n: usize| {
            let acc = super::prof::take();
            let total: f64 = acc.values().map(|v| v.0).sum();
            eprintln!("[perf] frame breakdown, {what}: {:.2} ms/frame over {n} frames", total * 1e3 / n as f64);
            let mut rows: Vec<_> = acc.into_iter().collect();
            rows.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
            for (name, (t, _)) in rows.iter().take(7) {
                eprintln!("[perf]   {name:<22} {:>6.2} ms  {:>4.1}%", t * 1e3 / n as f64, t / total * 100.0);
            }
            total / n as f64
        };
        let n = 120;
        super::prof::take();
        for i in 0..n {
            let d = &mut app.docs[app.active];
            d.top = 49_990.0 + (i * 3) as f64;
            d.target = d.top;
            super::prof::start();
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        report("steady scroll", n);
        for _ in 0..n {
            let ki = KeyIn { key: Key::Character(SmolStr::new("a")), text: Some("a".into()) };
            app.handle_key(&ki);
            super::prof::start();
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        report("keystroke", n);
        // Idle: let every animation finish, then redraw the unchanged scene.
        for _ in 0..200 {
            app.pump_background();
            app.tick();
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        super::prof::take();
        for _ in 0..n {
            super::prof::start();
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
        let idle = report("idle (unchanged scene)", n);
        assert!(idle < 0.050, "an idle frame takes {:.1} ms", idle * 1e3);
    }

    /// Idle: once everything settles, a frame reports "nothing to animate" (so the window loop
    /// stops redrawing) and ticking costs next to no CPU.
    #[test]
    fn idle_does_not_redraw_or_burn_cpu() {
        let (_g, mut app, mut px, (w, h)) = big_app("idle");
        // Let background tokenizing and animations finish.
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        let mut busy = true;
        while busy && Instant::now() < deadline {
            app.pump_background();
            app.tick();
            busy = app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!busy, "the app never went idle (redraw loop keeps running)");
        for _ in 0..30 {
            assert!(
                !app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016),
                "an idle frame asked for another redraw"
            );
        }
        let t0 = thread_cpu();
        let wall = Instant::now();
        while wall.elapsed() < std::time::Duration::from_millis(500) {
            app.pump_background();
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let cpu = thread_cpu() - t0;
        eprintln!("[perf] idle: {:.1} ms cpu over 500 ms of pump+tick", cpu * 1e3);
        assert!(cpu < 0.1, "idle polling burns CPU: {:.0} ms in 500 ms", cpu * 1e3);
    }

    /// PSS is a whole-process number, so under a parallel `cargo test` other tests' fixtures would
    /// inflate it. The measurement therefore runs in a fresh child process (this same test binary,
    /// re-run with a marker variable), which holds nothing but this test's data.
    #[test]
    fn memory_100k_buffer_and_tokenizer() {
        if std::env::var_os("LUMEN_PERF_CHILD").is_some() {
            return memory_100k_measure();
        }
        let exe = std::env::current_exe().expect("test binary path");
        let out = std::process::Command::new(exe)
            .args(["perf::tests::memory_100k_buffer_and_tokenizer", "--exact", "--nocapture", "--test-threads=1"])
            .env("LUMEN_PERF_CHILD", "1")
            .output()
            .expect("spawn child test process");
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "child measurement failed:\n{}", String::from_utf8_lossy(&out.stdout));
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"), "child did not run the test");
    }

    fn memory_100k_measure() {
        // The whole process is measured: wait for any `App` fixture of another test to finish.
        let _g = crate::app_unsaved::testkit::lock();
        let (_dir, buf, _) = big_file();
        let tk = make_tokenizer(&assets(), "source.ts").expect("tokenizer");
        let theme = Theme::load(&assets().join("themes/dark-plus.json")).unwrap();
        let mut hl = Highlighter::new(tk);
        hl.advance(&buf, 20_000, std::time::Duration::from_millis(500));
        let _ = hl.spans(&buf, &theme, 10);
        mem::report("100k buffer + tokenizer");
        let Some(m) = mem::snapshot() else {
            eprintln!("[perf] PSS unavailable, skipping assertion");
            return;
        };
        let mb = m.pss_kb as f64 / 1024.0;
        eprintln!("[perf] PSS {mb:.1} MB");
        assert!(mb < 120.0, "PSS too high: {mb:.1} MB");
    }

    // ------------------------------------------------------------------------------------------
    // Startup and open-time budgets
    // ------------------------------------------------------------------------------------------

    /// The release binary Cargo builds next to the test executables (`target/release/lumen-text`),
    /// if it exists. `cargo test` alone does not build it for a binary crate, so the tests that need
    /// it skip themselves when it is absent; CI runs `cargo build --release` first.
    fn built_binary() -> Option<PathBuf> {
        let p = std::env::current_exe().ok()?.parent()?.parent()?.join("lumen-text");
        p.is_file().then_some(p)
    }

    /// Wall time of one headless launch, from spawning the process to the `[start] first frame` line the
    /// script runner prints once the first frame is drawn (`LUMEN_PERF_MARK`): process start, config and
    /// theme load, fonts, `App::new` with `file` open, and that frame. Best of `runs` (the minimum is the
    /// least noisy estimator on a shared machine).
    fn headless_launch(bin: &Path, project: &Path, file: &Path, runs: usize) -> f64 {
        use std::io::BufRead;
        let state = tempdir::Dir::new("launch-state");
        let mut best = f64::MAX;
        for _ in 0..runs {
            let t0 = Instant::now();
            let mut child = std::process::Command::new(bin)
                .args(["--size", "1000x700", "--keys", "wait:0", "--snap"])
                .arg(state.path.join("snap"))
                .arg(project)
                .arg(file)
                .env("LUMEN_HEADLESS", "1")
                .env("LUMEN_PERF_MARK", "1")
                .env("LUMEN_ASSETS", assets())
                .env("XDG_CONFIG_HOME", state.path.join("cfg"))
                .env("XDG_STATE_HOME", state.path.join("state"))
                .env("XDG_DATA_HOME", state.path.join("data"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn lumen-text");
            let mut lines = std::io::BufReader::new(child.stderr.take().unwrap()).lines();
            let seen = lines.any(|l| l.is_ok_and(|l| l.contains("[start] first frame")));
            let dt = t0.elapsed().as_secs_f64();
            let _ = child.kill();
            let _ = child.wait();
            assert!(seen, "the process exited without drawing a first frame");
            best = best.min(dt);
        }
        best
    }

    /// In-process start: `App::new` on a small project plus the first rendered frame (wall clock: font
    /// loading and the first shaping may not run on this thread).
    #[test]
    fn startup_app_new_and_first_frame() {
        let _g = crate::app_unsaved::testkit::lock();
        let dir = crate::app_unsaved::testkit::tmp("startup");
        let t0 = Instant::now();
        let mut app = crate::app_unsaved::testkit::test_app(&dir);
        let new = t0.elapsed().as_secs_f64();
        app.size = (1000, 700);
        let (w, h) = (1000, 700);
        let mut px = vec![0u32; (w * h) as usize];
        let t1 = Instant::now();
        app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        let first = t1.elapsed().as_secs_f64();
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("[perf] startup: App::new {:.0} ms, first frame {:.0} ms (wall)", new * 1e3, first * 1e3);
        assert!(new < STARTUP_NEW_BUDGET, "App::new too slow: {new:.2}s");
        assert!(first < STARTUP_FRAME_BUDGET, "first frame too slow: {first:.2}s");
    }

    /// Process start to first frame, headless, on a small project (the real binary, wall clock).
    #[test]
    fn startup_binary_to_first_frame() {
        let Some(bin) = built_binary() else {
            eprintln!("[perf] startup_binary_to_first_frame skipped: run `cargo build --release` first");
            return;
        };
        let dir = tempdir::Dir::new("launch-small");
        std::fs::write(dir.path.join("a.ts"), gen_text(200)).unwrap();
        let t = headless_launch(&bin, &dir.path, &dir.path.join("a.ts"), 3);
        eprintln!("[perf] launch to first frame, small project: {:.0} ms wall", t * 1e3);
        assert!(t < LAUNCH_SMALL_BUDGET, "launch too slow: {t:.2}s wall");
    }

    /// Launching straight into a 100k-line file: the buffer opens, the visible window is tokenized
    /// and the first frame is written, without waiting for the rest of the file to be highlighted.
    #[test]
    fn startup_binary_into_100k_line_file() {
        let Some(bin) = built_binary() else {
            eprintln!("[perf] startup_binary_into_100k_line_file skipped: run `cargo build --release` first");
            return;
        };
        let dir = tempdir::Dir::new("launch-big");
        std::fs::write(dir.path.join("big.ts"), gen_text(100_000)).unwrap();
        let t = headless_launch(&bin, &dir.path, &dir.path.join("big.ts"), 3);
        eprintln!("[perf] launch into a 100k-line file: {:.0} ms wall", t * 1e3);
        assert!(t < LAUNCH_BIG_BUDGET, "launch into a 100k-line file too slow: {t:.2}s wall");
    }
}
