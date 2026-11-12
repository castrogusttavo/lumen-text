//! Coverage in the editor: "Tests: Run with coverage" (the run itself is the Tests panel's, with
//! the runner's coverage flags added, see `testrun::with_coverage`), "Tests: Show coverage from
//! file" and "Tests: Hide coverage". A loaded report draws a thin green / yellow / red bar in the
//! gutter of open files (see `Doc::cov`), a percentage next to files in the explorer and the
//! active file's percentage in the status bar. Parsing lives in `coverage.rs`.

use crate::{
    app::App,
    coverage::{self, Report},
    theme::Rgba,
    ui::{Canvas, Rect},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Default)]
pub struct CovState {
    pub report: Option<Box<Report>>,
    /// Bumped whenever the report changes; a document remembers the one its marks come from.
    pub generation: u64,
    /// Test runs collect coverage (set by "Run with coverage", cleared by "Hide coverage"), so
    /// re-runs (watch mode, run again) refresh what is shown.
    pub run: bool,
}

/// Where coverage reports usually land, relative to a project or package folder.
const REPORT_SPOTS: &[&str] = &[
    "coverage/lcov.info",
    "lcov.info",
    "coverage/lcov/lcov.info",
    "target/llvm-cov/lcov.info",
    "coverage/cobertura-coverage.xml",
    "coverage.xml",
    "cobertura.xml",
];

/// The newest existing report among the usual spots of `dirs`.
pub fn find_report(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .flat_map(|d| REPORT_SPOTS.iter().map(move |s| d.join(s)))
        .filter_map(|p| std::fs::metadata(&p).ok().filter(|m| m.is_file()).map(|m| (m.modified().ok(), p)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Folders a report's relative file names may be relative to: where the tests ran, then the
/// report's folder and the one above it (`coverage/lcov.info` names files from the package root).
fn bases_for(run_dir: &Path, report: &Path) -> Vec<PathBuf> {
    let mut v = vec![run_dir.to_path_buf()];
    if let Some(p) = report.parent() {
        v.push(p.to_path_buf());
        if let Some(pp) = p.parent() {
            v.push(pp.to_path_buf());
        }
    }
    v
}

impl App {
    /// Use `report` from now on: documents get their marks on the next tick (and right away here).
    pub fn coverage_set(&mut self, report: Option<Report>) {
        let st = &mut self.tests.cov;
        st.report = report.map(Box::new);
        st.generation += 1;
        self.coverage_sync_docs();
        self.request_redraw();
    }

    /// Give every document whose marks are older than the report its marks (cheap when nothing
    /// changed: one comparison per document). Called from `tick`.
    pub fn coverage_sync_docs(&mut self) {
        let st = &self.tests.cov;
        let generation = st.generation;
        for d in &mut self.docs {
            if d.cov_gen == generation {
                continue;
            }
            d.cov_gen = generation;
            d.cov.clear();
            let (Some(r), Some(p)) = (st.report.as_ref(), d.path()) else { continue };
            if d.buf.line_count() > 200_000 {
                continue;
            }
            if let Some(fc) = r.get(p) {
                let buf = &d.buf;
                d.cov = coverage::marks_for(fc, buf.line_count(), &|l| buf.line(l));
            }
        }
    }

    /// Read the LCOV report a coverage run wrote; the note appended to the run's summary.
    pub fn coverage_after_run(&mut self, path: &Path, run_dir: &Path, since: SystemTime) -> String {
        let fresh =
            std::fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|t| t + Duration::from_secs(2) >= since);
        let text = if fresh { std::fs::read_to_string(path).ok() } else { None };
        let Some(text) = text else {
            return " · no coverage report was written (is the coverage provider installed?)".into();
        };
        let r = Report::parse(&text, &bases_for(run_dir, path), path.to_path_buf());
        let note = match r.total_pct() {
            Some(p) => format!(" · coverage {p:.1}%"),
            None => " · the coverage report is empty".into(),
        };
        self.coverage_set(Some(r));
        note
    }

    /// "Tests: Show coverage from file": the newest `coverage/lcov.info` (or a Cobertura XML) of
    /// the project or of its package.json test packages.
    pub fn coverage_from_file(&mut self) {
        let mut dirs = vec![self.root.clone()];
        for t in &self.tests.scripts {
            if !dirs.contains(&t.dir) {
                dirs.push(t.dir.clone());
            }
        }
        let Some(path) = find_report(&dirs) else {
            return self.say("no coverage report found (looked for coverage/lcov.info, lcov.info, coverage.xml…)");
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let run_dir = path.parent().and_then(Path::parent).unwrap_or(&self.root).to_path_buf();
                let r = Report::parse(&text, &bases_for(&run_dir, &path), path.clone());
                let msg = self.coverage_message(&r);
                self.coverage_set(Some(r));
                self.say(msg);
            }
            Err(e) => self.say(format!("could not read {}: {e}", path.display())),
        }
    }

    /// "Tests: Hide coverage": drop the report and stop collecting it on test runs.
    pub fn coverage_hide(&mut self) {
        let had = self.tests.cov.report.is_some() || self.tests.cov.run;
        self.tests.cov.run = false;
        self.coverage_set(None);
        self.say(if had { "coverage hidden" } else { "no coverage is shown" });
    }

    /// What a freshly loaded report says: the active file's figures, else the totals.
    fn coverage_message(&self, r: &Report) -> String {
        let src = self.rel(&r.source);
        let active = self.docs.get(self.active).and_then(|d| d.path()).and_then(|p| r.get(p).map(|fc| (p, fc)));
        match (active, r.total_pct()) {
            (Some((p, fc)), _) => format!("coverage of {}: {} — from {src}", self.rel(p), fc.describe()),
            (None, Some(t)) => format!("coverage {t:.1}% over {} files — from {src}", r.files.len()),
            (None, None) => format!("{src} has no line coverage"),
        }
    }

    /// Line coverage of `p` in the loaded report (explorer, status bar).
    pub fn coverage_pct_of(&self, p: &Path) -> Option<f32> {
        self.tests.cov.report.as_ref()?.get(p)?.pct()
    }

    /// Green from 80 %, yellow from 50 %, red below.
    pub fn coverage_color(&self, pct: f32) -> Rgba {
        let c = self.theme.chrome;
        if pct >= 80.0 {
            c.git_added
        } else if pct >= 50.0 {
            c.warning
        } else {
            c.error
        }
    }

    /// Status bar read-out of the active file's coverage; returns the next x.
    pub fn draw_cov_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let Some(pct) = self.docs.get(self.active).and_then(|d| d.path()).and_then(|p| self.coverage_pct_of(p)) else {
            return x;
        };
        let r = self.layout.status;
        let label = format!("Coverage {pct:.0}%");
        let lw = self.tx.ui_width(&label, false);
        let col = self.coverage_color(pct);
        cv.fill_round(Rect::new(x + 8, r.y + r.h / 2 - 3, 6, 6), 3, col);
        let dim = self.theme.chrome.tab_inactive_fg;
        self.tx.draw_ui(cv, x + 20, r.y + (r.h - 18) / 2, &label, dim, Rect::new(x, r.y, lw + 28, r.h), false);
        x + 20 + lw + 12
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    #[test]
    fn report_reaches_open_and_later_opened_files_and_hides() {
        let _g = lock();
        let d = tmp("cov-app");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("coverage")).unwrap();
        std::fs::write(d.join("src/a.ts"), "export const a = 1;\nexport function f() {\n  return 2;\n}\n").unwrap();
        std::fs::write(d.join("src/b.ts"), "x\ny\n").unwrap();
        std::fs::write(
            d.join("coverage/lcov.info"),
            "SF:src/a.ts\nDA:1,1\nDA:2,1\nDA:3,0\nend_of_record\nSF:src/b.ts\nDA:1,1\nDA:2,1\nend_of_record\n",
        )
        .unwrap();
        let mut app = test_app(&d);
        let a = app.open_path(&d.join("src/a.ts"), None).unwrap();
        app.coverage_from_file();
        assert!(app.status.as_ref().is_some_and(|(s, _)| s.contains("67% of lines")), "{:?}", app.status);
        let marks: Vec<(usize, coverage::Cov)> = app.docs[a].cov.iter().map(|m| (m.line, m.cov)).collect();
        assert_eq!(
            marks,
            vec![(0, coverage::Cov::Covered), (1, coverage::Cov::Covered), (2, coverage::Cov::Uncovered)]
        );
        assert_eq!(app.coverage_pct_of(&d.join("src/b.ts")), Some(100.0), "the explorer figure");
        // A file opened later gets its marks on the next tick.
        let b = app.open_path(&d.join("src/b.ts"), None).unwrap();
        assert!(app.docs[b].cov.is_empty());
        app.tick();
        assert_eq!(app.docs[b].cov.len(), 2);
        // Typing on line 3 of a.ts drops that line's (now stale) mark, keeps the others.
        app.activate(a);
        let doc = &mut app.docs[a];
        let p = doc.buf.pos(2, 2);
        doc.ed.click(&mut doc.buf, p, false);
        doc.ed.insert_text(&mut doc.buf, "x");
        doc.sync_edits();
        assert_eq!(app.docs[a].cov.iter().map(|m| m.line).collect::<Vec<_>>(), vec![0, 1]);
        // Hide clears every document.
        app.coverage_hide();
        assert!(app.docs.iter().all(|d| d.cov.is_empty()));
        assert!(app.coverage_pct_of(&d.join("src/b.ts")).is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_newest_report_wins_and_nothing_found_says_so() {
        let d = std::env::temp_dir().join(format!("lumen-cov-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("coverage")).unwrap();
        assert!(find_report(std::slice::from_ref(&d)).is_none());
        std::fs::write(d.join("lcov.info"), "").unwrap();
        assert_eq!(find_report(std::slice::from_ref(&d)), Some(d.join("lcov.info")));
        std::fs::write(d.join("coverage/lcov.info"), "").unwrap();
        let t = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(d.join("coverage/lcov.info")).unwrap().set_modified(t).unwrap();
        assert_eq!(find_report(std::slice::from_ref(&d)), Some(d.join("coverage/lcov.info")));
        std::fs::remove_dir_all(&d).ok();
    }
}
