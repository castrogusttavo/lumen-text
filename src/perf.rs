//! Performance regression tests. Thresholds are deliberately generous (slow CI machines);
//! they only catch large regressions. Each test prints its measured number to stderr.
#![allow(dead_code)]

#[cfg(test)]
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
        let cpu = || {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
            ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
        };
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
        let cpu = || {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
            ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
        };
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
}
