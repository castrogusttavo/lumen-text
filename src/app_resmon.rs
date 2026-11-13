//! Resource panel glue (RAM item of the bottom bar): a picker listing the editor and everything
//! it started with PSS/RSS and uptime, a Stop / Restart menu per row, and the optional soft
//! memory budget. Measuring happens only while the panel is open (every 2 s) and, with a budget
//! set, every 10 s over the editor and its servers; with no budget and no panel nothing runs.

use crate::{
    app::App,
    app_ask::{AskAction, procs_question},
    app_overlay::{Action, Item, Overlay, choice_picker},
    resmon::{self, Kind, ResPick, Row, Target},
};
use serde_json::Value;
use std::time::Instant;

fn item(label: String, detail: String, p: ResPick) -> Item {
    Item { label, detail, action: Action::Res(p) }
}

impl App {
    /// Settings hook: the memory budget.
    pub(crate) fn res_apply_setting(&mut self, key: &str, v: &Value) -> bool {
        if key != "mem_budget_mb" {
            return false;
        }
        self.config.mem_budget_mb = resmon::clamp_budget(v.as_f64());
        self.res.next_budget = Instant::now();
        self.res.over = false;
        self.res.warned = false;
        true
    }

    fn lsp_is_idle(&self, i: usize) -> bool {
        let l = &self.lsps[i];
        if l.is_generic() {
            return l.open_uris.is_empty();
        }
        let biome = l.name == "biome";
        !self.docs.iter().any(|d| {
            if biome {
                d.biome_open && d.biome_root.clone().flatten().as_ref() == Some(&l.root)
            } else {
                d.lsp_open && d.lsp_root.as_ref() == Some(&l.root)
            }
        })
    }

    /// Every process Lumen runs, measured now (one `ps` for the process trees).
    pub(crate) fn res_rows(&self) -> Vec<Row> {
        let tree = resmon::process_tree();
        let mut rows = Vec::new();
        let own = crate::mem::snapshot();
        rows.push(Row {
            kind: Kind::Editor,
            name: "Lumen".into(),
            target: Target::Editor,
            pss_kb: own.map_or(0, |m| m.pss_kb),
            rss_kb: own.map_or(0, |m| m.rss_kb),
            up: Some(self.res.started.elapsed()),
            idle: false,
        });
        let mut push = |kind, name: String, target, pid: Option<u32>, up, idle| {
            let (pss_kb, rss_kb) = pid.map_or((0, 0), |p| resmon::tree_mem(&tree, p));
            rows.push(Row { kind, name, target, pss_kb, rss_kb, up, idle });
        };
        for (i, l) in self.lsps.iter().enumerate() {
            let name = if l.mirror.is_some() { format!("{} (lean)", l.name) } else { l.name.clone() };
            push(Kind::Lsp, name, Target::Lsp(l.pid()), Some(l.pid()), Some(l.started.elapsed()), self.lsp_is_idle(i));
        }
        for (n, t) in self.terms.tabs.iter().enumerate() {
            if t.term.alive {
                push(
                    Kind::Terminal,
                    t.title(n + 1),
                    Target::Term(t.id),
                    t.term.pid(),
                    Some(t.term.started.elapsed()),
                    false,
                );
            }
        }
        for s in &self.ai.sessions {
            if s.term.alive {
                push(
                    Kind::Agent,
                    s.name.clone(),
                    Target::Agent(s.id),
                    s.term.pid(),
                    Some(s.term.started.elapsed()),
                    false,
                );
            }
        }
        if let Some(d) = &self.dbg {
            let pid = match d {
                crate::app_debug::Debugger::Node(m) => m.adapter_pid(),
                crate::app_debug::Debugger::Native { dap, .. } => dap.adapter_pid(),
            };
            push(Kind::Debug, "Debug session".into(), Target::Debug, pid, None, false);
        }
        if let Some(r) = &self.panel.running {
            push(Kind::Task, self.panel.title.clone(), Target::Task, u32::try_from(r.pid).ok(), None, false);
        }
        if let Some(label) = self.build.running_label() {
            push(
                Kind::Build,
                label,
                Target::Build,
                self.build.job_group(),
                self.build.started.map(|t| t.elapsed()),
                false,
            );
        }
        rows
    }

    fn res_items(&self, rows: &[Row]) -> Vec<Item> {
        let mut items = Vec::new();
        let total = resmon::total_kb(rows);
        let budget = self.config.mem_budget_mb;
        let mut head = format!("Total {}", resmon::fmt_kb(total));
        if budget > 0 {
            head.push_str(&format!(" of {} budget", resmon::fmt_kb(u64::from(budget) * 1024)));
        }
        let note =
            if budget > 0 && total / 1024 > u64::from(budget) { "over budget" } else { "PSS, editor and children" };
        items.push(item(head, note.into(), ResPick::Noop));
        let idle: Vec<&Row> = rows.iter().filter(|r| r.idle).collect();
        if !idle.is_empty() {
            let kb: u64 = idle.iter().map(|r| r.pss_kb).sum();
            items.push(item(
                format!("Stop {} idle language server{}", idle.len(), if idle.len() == 1 { "" } else { "s" }),
                format!("frees about {}; they start again on next use", resmon::fmt_kb(kb)),
                ResPick::StopIdle,
            ));
        }
        for r in rows {
            let act = if r.target == Target::Editor { ResPick::Noop } else { ResPick::Menu(r.target.clone()) };
            items.push(item(format!("{}: {}", r.kind.label(), r.name), resmon::row_detail(r), act));
        }
        items
    }

    /// Open the panel (click on the RAM item).
    pub fn open_resources(&mut self) {
        let rows = self.res_rows();
        let items = self.res_items(&rows);
        let sel = if self.res.over && items.get(1).is_some_and(|i| matches!(i.action, Action::Res(ResPick::StopIdle))) {
            1
        } else {
            0
        };
        self.overlay = Overlay::Picker(choice_picker(resmon::TITLE, items, sel));
        self.res.next_refresh = Some(Instant::now() + resmon::REFRESH);
        self.request_redraw();
    }

    fn res_panel_open(&self) -> bool {
        matches!(&self.overlay, Overlay::Picker(p) if p.title == resmon::TITLE)
    }

    /// Refresh the open panel and run the budget check; asks for wake-ups only when it needs them.
    pub(crate) fn res_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if let Some(at) = self.res.next_refresh {
            if !self.res_panel_open() {
                self.res.next_refresh = None;
            } else if now >= at {
                self.res.next_refresh = Some(now + resmon::REFRESH);
                self.res_refresh();
                want(now + resmon::REFRESH);
            } else {
                want(at);
            }
        }
        if self.config.mem_budget_mb == 0 {
            if self.res.over || self.res.warned {
                self.res.observe(0, 0);
            }
            return;
        }
        if now >= self.res.next_budget {
            self.res.next_budget = now + resmon::BUDGET_EVERY;
            let kb = self.res_budget_total_kb();
            let mb = self.config.mem_budget_mb;
            if self.res.observe(kb, mb) {
                let msg = format!(
                    "Memory {} is over the {} budget: click the RAM item to stop idle servers",
                    resmon::fmt_kb(kb),
                    resmon::fmt_kb(u64::from(mb) * 1024)
                );
                self.toast(msg.clone(), false);
                self.say_for(msg, 5);
            }
            self.request_redraw();
        }
        want(self.res.next_budget);
    }

    /// Cheap total for the budget: the editor and its language servers / debug adapter (no `ps`).
    fn res_budget_total_kb(&self) -> u64 {
        let own = crate::mem::snapshot().map_or(0, |m| m.pss_kb);
        own + self.lsps.iter().filter_map(|l| crate::mem::pss_of(l.pid())).sum::<u64>()
    }

    fn res_refresh(&mut self) {
        let Overlay::Picker(p) = &self.overlay else { return };
        if !p.field.text.is_empty() {
            return; // the user is filtering: keep the list still
        }
        let (sel, scroll) = (p.sel, p.scroll);
        let rows = self.res_rows();
        let items = self.res_items(&rows);
        let mut np = choice_picker(resmon::TITLE, items, sel);
        np.scroll = scroll.min(np.filtered.len().saturating_sub(1));
        self.overlay = Overlay::Picker(np);
        self.request_redraw();
    }

    fn res_name(&self, t: &Target) -> String {
        match t {
            Target::Editor => "Lumen".into(),
            Target::Lsp(pid) => self.lsps.iter().find(|l| l.pid() == *pid).map_or("server".into(), |l| l.name.clone()),
            Target::Term(id) => self.terms.get(*id).map_or("terminal".into(), |t| t.title(1)),
            Target::Agent(id) => {
                self.ai.sessions.iter().find(|s| s.id == *id).map_or("agent".into(), |s| s.name.clone())
            }
            Target::Debug => "debug session".into(),
            Target::Task => self.panel.title.clone(),
            Target::Build => self.build.running_label().unwrap_or_else(|| "Build & Run".into()),
        }
    }

    pub fn res_pick(&mut self, p: ResPick) {
        match p {
            ResPick::Noop => {
                // Keep the panel: the Enter closed it, so open it again.
                self.open_resources();
            }
            ResPick::Back => self.open_resources(),
            ResPick::Menu(t) => {
                let name = self.res_name(&t);
                let mut items = Vec::new();
                let (stop, detail) = match &t {
                    Target::Lsp(_) => ("Stop", "ends the server; it starts again on next use"),
                    Target::Term(_) => ("Close", "ends the terminal and what it runs (asks first when a job runs)"),
                    Target::Agent(_) => ("Stop", "closes the session; asks first, its unsaved state is lost"),
                    Target::Debug => ("Stop", "ends the debug session and the debugged program"),
                    Target::Task => ("Stop", "ends the task"),
                    _ => ("Stop", "ends the job"),
                };
                items.push(item(format!("{stop} {name}"), detail.into(), ResPick::Stop(t.clone())));
                if matches!(t, Target::Lsp(_)) {
                    items.push(item(
                        format!("Restart {name}"),
                        "stops it now; documents reopen on a fresh server".into(),
                        ResPick::Restart(t.clone()),
                    ));
                }
                items.push(item("Back".into(), "to the resource list".into(), ResPick::Back));
                self.overlay = Overlay::Picker(choice_picker(&format!("{}: {name}", Kind::label_of(&t)), items, 0));
            }
            ResPick::Stop(t) => {
                let name = self.res_name(&t);
                self.res_stop(&t);
                self.say(format!("stopped {name}"));
                if self.overlay.is_none() {
                    self.open_resources();
                }
            }
            ResPick::Restart(t) => {
                let name = self.res_name(&t);
                self.res_stop(&t);
                self.say(format!("restarting {name}"));
                if self.overlay.is_none() {
                    self.open_resources();
                }
            }
            ResPick::StopIdle => {
                let pids: Vec<u32> =
                    (0..self.lsps.len()).filter(|i| self.lsp_is_idle(*i)).map(|i| self.lsps[i].pid()).collect();
                for pid in &pids {
                    self.res_stop_lsp(*pid);
                }
                self.res.next_budget = Instant::now();
                self.say(format!(
                    "stopped {} idle language server{}",
                    pids.len(),
                    if pids.len() == 1 { "" } else { "s" }
                ));
                self.open_resources();
            }
        }
    }

    fn res_stop(&mut self, t: &Target) {
        match t {
            Target::Editor => {}
            Target::Lsp(pid) => self.res_stop_lsp(*pid),
            Target::Term(id) => self.close_terminal_request_id(*id),
            Target::Agent(id) => {
                if let Some(i) = self.ai.sessions.iter().position(|s| s.id == *id) {
                    // The question card is the confirmation: an agent holds unsaved conversation state.
                    let name = self.ai.sessions[i].name.clone();
                    self.show_ask(procs_question(&[name], AskAction::CloseAiSession(*id)));
                }
            }
            Target::Debug => self.debug_stop(),
            Target::Task => self.stop_task(),
            Target::Build => {
                self.build_stop_quiet();
            }
        }
    }

    /// Stop one language server; documents that used it reopen on a fresh one by themselves.
    fn res_stop_lsp(&mut self, pid: u32) {
        let Some(i) = self.lsps.iter().position(|l| l.pid() == pid) else { return };
        let mut l = self.lsps.remove(i);
        if l.is_generic() {
            l.shutdown();
            self.lspx_forget_docs(&l);
            return;
        }
        self.lspx.problems.clear_source(&l.name);
        crate::doc::unregister_mirror(&l.root);
        l.shutdown();
        let biome = l.name == "biome";
        for d in &mut self.docs {
            if biome {
                if d.biome_root.clone().flatten().as_ref() == Some(&l.root) {
                    d.biome_open = false;
                }
            } else if d.lsp_root.as_ref() == Some(&l.root) {
                d.lsp_open = false;
            }
        }
    }
}

impl Kind {
    /// Menu title prefix for a target.
    pub fn label_of(t: &Target) -> &'static str {
        match t {
            Target::Editor => Kind::Editor,
            Target::Lsp(_) => Kind::Lsp,
            Target::Term(_) => Kind::Terminal,
            Target::Agent(_) => Kind::Agent,
            Target::Debug => Kind::Debug,
            Target::Task => Kind::Task,
            Target::Build => Kind::Build,
        }
        .label()
    }
}
