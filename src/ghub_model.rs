//! What the GitHub page shows, worked out from the models of `ghub`: tones, states and their
//! words, merge readiness, grouping, filtering, times. Plain functions, no drawing (the same
//! rules as Athas' `github_panel::model`).

use crate::ghub::{Check, IssueItem, PrDetail, PrItem, RunItem};

/// The color family a state is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Success,
    Error,
    Warning,
    Accent,
    Merged,
    Muted,
}

// ───────────── times (UTC, from the ISO 8601 the API prints) ─────────────

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date (year, month, day) of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds since the epoch of "2026-11-21T18:31:15Z" (a fraction and a `+00:00` are tolerated).
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

/// "just now", "5m ago", "2h ago", "yesterday", "3d ago", "2w ago".
pub fn time_ago(value: &str, now: i64) -> String {
    let Some(t) = parse_iso(value) else { return value.to_string() };
    let minutes = (now - t) / 60;
    let days = (now - t) / 86_400;
    if minutes < 1 {
        "just now".into()
    } else if minutes < 60 {
        format!("{minutes}m ago")
    } else if minutes < 1440 {
        format!("{}h ago", minutes / 60)
    } else if days == 1 {
        "yesterday".into()
    } else if days < 7 {
        format!("{days}d ago")
    } else {
        format!("{}w ago", days / 7)
    }
}

/// The time at the end of a list row: "0m", "5m", "2h", "3d".
pub fn short_time(value: &str, now: i64) -> String {
    let Some(t) = parse_iso(value) else { return "—".into() };
    let minutes = ((now - t) / 60).max(0);
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 1440 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / 1440)
    }
}

pub fn format_duration(seconds: i64) -> String {
    let s = seconds.max(0);
    let (h, m, r) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {r:02}s")
    } else {
        format!("{r}s")
    }
}

/// How long something ran (or has been running while `active`).
pub fn elapsed(start: &str, end: &str, active: bool, now: i64) -> Option<String> {
    let a = parse_iso(start)?;
    let z = if active { now } else { parse_iso(end)? };
    Some(format_duration(z - a))
}

/// A day heading for runs: "Today", "Yesterday", "Nov 21", "Nov 21, 2025" (UTC days).
pub fn day_title(value: &str, now: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let Some(t) = parse_iso(value) else { return "Earlier".into() };
    let (day, today) = (t.div_euclid(86_400), now.div_euclid(86_400));
    if day == today {
        return "Today".into();
    }
    if day == today - 1 {
        return "Yesterday".into();
    }
    let (y, m, d) = civil_from_days(day);
    let (ty, _, _) = civil_from_days(today);
    let name = MONTHS[(m - 1).clamp(0, 11) as usize];
    if y == ty { format!("{name} {d}") } else { format!("{name} {d}, {y}") }
}

pub fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

pub fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

// ───────────── pull requests ─────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrStatus {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrStatus {
    pub fn of(p: &PrItem) -> Self {
        match p.state.as_str() {
            "MERGED" => PrStatus::Merged,
            "CLOSED" => PrStatus::Closed,
            _ if p.draft => PrStatus::Draft,
            _ => PrStatus::Open,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PrStatus::Open => "Open",
            PrStatus::Draft => "Draft",
            PrStatus::Merged => "Merged",
            PrStatus::Closed => "Closed",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            PrStatus::Open => Tone::Success,
            PrStatus::Draft => Tone::Muted,
            PrStatus::Merged => Tone::Merged,
            PrStatus::Closed => Tone::Error,
        }
    }
}

/// The review decision as a badge: (label, tone).
pub fn review_badge(decision: &str) -> Option<(&'static str, Tone)> {
    match decision {
        "APPROVED" => Some(("Approved", Tone::Success)),
        "CHANGES_REQUESTED" => Some(("Changes requested", Tone::Error)),
        "REVIEW_REQUIRED" => Some(("Review required", Tone::Warning)),
        _ => None,
    }
}

/// What the merge button says, and whether the pull request can be merged now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeStatus {
    pub text: &'static str,
    pub tone: Tone,
    pub ready: bool,
}

pub fn merge_status(status: PrStatus, merge_state: &str, mergeable: &str, review: &str) -> MergeStatus {
    let blocked = |text, tone| MergeStatus { text, tone, ready: false };
    match status {
        PrStatus::Merged => return blocked("Merged", Tone::Merged),
        PrStatus::Closed => return blocked("Closed without merging", Tone::Muted),
        _ => {}
    }
    let ms = merge_state.to_ascii_uppercase();
    if mergeable.eq_ignore_ascii_case("CONFLICTING") || ms == "DIRTY" {
        return blocked("Has conflicts", Tone::Error);
    }
    if status == PrStatus::Draft {
        return blocked("Draft", Tone::Muted);
    }
    match ms.as_str() {
        "BLOCKED" => match review {
            "CHANGES_REQUESTED" => blocked("Changes requested", Tone::Error),
            "" | "REVIEW_REQUIRED" => blocked("Review required", Tone::Warning),
            _ => blocked("Blocked by checks", Tone::Warning),
        },
        "BEHIND" => blocked("Behind base branch", Tone::Warning),
        "UNSTABLE" => MergeStatus { text: "Merge", tone: Tone::Warning, ready: true },
        "CLEAN" | "HAS_HOOKS" => MergeStatus { text: "Merge", tone: Tone::Success, ready: true },
        _ => blocked("Checking mergeability", Tone::Muted),
    }
}

pub fn merge_status_of(d: &PrDetail) -> MergeStatus {
    merge_status(PrStatus::of(&d.item), &d.merge_state, &d.item.mergeable, &d.item.review)
}

// ───────────── checks and runs ─────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Success,
    Failure,
    Cancelled,
    Skipped,
    ActionRequired,
    Neutral,
    Running,
    Queued,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunState {
    pub phase: Phase,
    pub label: String,
    pub tone: Tone,
    pub active: bool,
    pub failed: bool,
}

fn humanize(v: &str) -> String {
    let t = v.replace('_', " ");
    let mut c = t.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => "Unknown".into(),
    }
}

/// The phase of a run, job, step or check from its `status` and `conclusion` (lowercase).
pub fn run_state(status: &str, conclusion: &str) -> RunState {
    let st =
        |phase, label: &str, tone, active, failed| RunState { phase, label: label.to_string(), tone, active, failed };
    match conclusion {
        "success" => return st(Phase::Success, "Success", Tone::Success, false, false),
        "failure" | "startup_failure" | "error" => return st(Phase::Failure, "Failed", Tone::Error, false, true),
        "timed_out" => return st(Phase::Failure, "Timed out", Tone::Error, false, true),
        "cancelled" => return st(Phase::Cancelled, "Cancelled", Tone::Muted, false, false),
        "skipped" => return st(Phase::Skipped, "Skipped", Tone::Muted, false, false),
        "action_required" => return st(Phase::ActionRequired, "Action required", Tone::Warning, false, false),
        "neutral" | "stale" => return st(Phase::Neutral, &humanize(conclusion), Tone::Muted, false, false),
        "pending" => return st(Phase::Queued, "Pending", Tone::Warning, true, false),
        _ => {}
    }
    match status {
        "in_progress" => st(Phase::Running, "Running", Tone::Accent, true, false),
        "waiting" | "queued" | "pending" | "requested" => st(Phase::Queued, "Queued", Tone::Warning, true, false),
        _ => st(
            Phase::Unknown,
            &humanize(if conclusion.is_empty() { status } else { conclusion }),
            Tone::Muted,
            false,
            false,
        ),
    }
}

pub fn check_state(c: &Check) -> RunState {
    run_state(&c.status, &c.conclusion)
}

/// Passing / failing / running counts of the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Tally {
    pub pass: usize,
    pub fail: usize,
    pub pending: usize,
}

pub fn tally(checks: &[Check]) -> Tally {
    let mut t = Tally::default();
    for c in checks {
        let s = check_state(c);
        match s.phase {
            Phase::Success | Phase::Skipped | Phase::Neutral => t.pass += 1,
            Phase::Failure | Phase::ActionRequired => t.fail += 1,
            Phase::Cancelled => t.fail += 1,
            _ => t.pending += 1,
        }
    }
    t
}

impl Tally {
    /// "3/4 checks passed", "1 failing", "2 running".
    pub fn text(&self) -> String {
        let total = self.pass + self.fail + self.pending;
        if total == 0 {
            return "No checks".into();
        }
        if self.fail > 0 {
            return format!("{} failing", self.fail);
        }
        if self.pending > 0 {
            return format!("{} running", self.pending);
        }
        format!("{}/{} checks passed", self.pass, total)
    }

    pub fn tone(&self) -> Tone {
        if self.fail > 0 {
            Tone::Error
        } else if self.pending > 0 {
            Tone::Warning
        } else if self.pass > 0 {
            Tone::Success
        } else {
            Tone::Muted
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RunFilter {
    #[default]
    All,
    InProgress,
    Successful,
    Failed,
}

impl RunFilter {
    pub const ALL: [RunFilter; 4] = [RunFilter::All, RunFilter::InProgress, RunFilter::Successful, RunFilter::Failed];

    pub fn label(self) -> &'static str {
        match self {
            RunFilter::All => "All runs",
            RunFilter::InProgress => "In progress",
            RunFilter::Successful => "Successful",
            RunFilter::Failed => "Failed",
        }
    }

    pub fn matches(self, r: &RunItem) -> bool {
        let s = run_state(&r.status, &r.conclusion);
        match self {
            RunFilter::All => true,
            RunFilter::InProgress => s.active,
            RunFilter::Successful => s.phase == Phase::Success,
            RunFilter::Failed => s.failed || s.phase == Phase::Cancelled,
        }
    }
}

// ───────────── grouping and search ─────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub title: String,
    /// Indices into the list that was grouped.
    pub items: Vec<usize>,
}

fn push_group(out: &mut Vec<Group>, id: &str, title: &str, items: Vec<usize>) {
    if !items.is_empty() {
        out.push(Group { id: id.into(), title: title.into(), items });
    }
}

fn has(q: &str, fields: &[&str]) -> bool {
    fields.iter().any(|f| f.to_lowercase().contains(q))
}

/// `q` is trimmed and lowercased.
pub fn pr_matches(p: &PrItem, q: &str) -> bool {
    q.is_empty()
        || has(
            q,
            &[
                &p.title,
                &format!("#{}", p.number),
                &p.author,
                &p.head,
                &p.base,
                &p.state,
                &p.review,
                if p.draft { "draft" } else { "" },
            ],
        )
        || p.labels.iter().any(|l| l.name.to_lowercase().contains(q))
}

pub fn issue_matches(i: &IssueItem, q: &str) -> bool {
    q.is_empty()
        || has(q, &[&i.title, &format!("#{}", i.number), &i.author, &i.state])
        || i.labels.iter().any(|l| l.name.to_lowercase().contains(q))
}

pub fn run_matches(r: &RunItem, q: &str) -> bool {
    q.is_empty()
        || has(
            q,
            &[&r.title, &r.workflow, &r.event, &r.status, &r.conclusion, &r.branch, &r.sha, &format!("#{}", r.number)],
        )
}

/// Open pull requests first, drafts under them; a list that is not "open" stays one group.
pub fn group_prs(prs: &[PrItem], shown: &[usize], title: &str) -> Vec<Group> {
    let mut out = Vec::new();
    let (mut open, mut draft, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    for &i in shown {
        match PrStatus::of(&prs[i]) {
            PrStatus::Open => open.push(i),
            PrStatus::Draft => draft.push(i),
            _ => rest.push(i),
        }
    }
    push_group(&mut out, "open", title, open);
    push_group(&mut out, "drafts", "Drafts", draft);
    push_group(&mut out, "rest", title, rest);
    out
}

pub fn group_issues(issues: &[IssueItem], shown: &[usize]) -> Vec<Group> {
    let mut out = Vec::new();
    let (open, closed): (Vec<usize>, Vec<usize>) = shown.iter().partition(|&&i| issues[i].state == "OPEN");
    push_group(&mut out, "open", "Open", open);
    push_group(&mut out, "closed", "Closed", closed);
    out
}

/// Runs by the day they started, newest day first (the list arrives newest first already).
pub fn group_runs(runs: &[RunItem], shown: &[usize], now: i64) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for &i in shown {
        let r = &runs[i];
        let title = day_title(if r.created.is_empty() { &r.updated } else { &r.created }, now);
        match out.last_mut() {
            Some(g) if g.title == title => g.items.push(i),
            _ => out.push(Group { id: title.clone(), title, items: vec![i] }),
        }
    }
    out
}

// ───────────── labels and avatars ─────────────

/// RGB of a label color ("d73a4a", with or without `#`).
pub fn label_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let p = |a: usize| u8::from_str_radix(&h[a..a + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

/// A stable hue (0..360) for a login: the same person always gets the same avatar color.
pub fn avatar_hue(login: &str) -> u32 {
    login.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360
}

/// RGB of HSL(h, 0.55, 0.45): the fill of an avatar circle.
pub fn hue_rgb(h: u32) -> (u8, u8, u8) {
    let (s, l) = (0.55f32, 0.45f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h % 360) as f32 / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (f(r), f(g), f(b))
}

/// First letter of a login, uppercased ("?" when empty).
pub fn initial(login: &str) -> String {
    login.chars().next().map_or("?".into(), |c| c.to_uppercase().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghub::Label;

    fn pr(n: u64, state: &str, draft: bool) -> PrItem {
        PrItem { number: n, state: state.into(), draft, title: format!("t{n}"), ..Default::default() }
    }

    #[test]
    fn iso_round_trips_through_days() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_iso("2026-11-21T18:31:15.123Z"), Some(parse_iso("2026-11-21T18:31:15Z").unwrap()));
        assert_eq!(parse_iso("nope"), None);
        assert_eq!(parse_iso("2026-13-01T00:00:00Z"), None);
        for d in [-1000, 0, 11_000, 20_413] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
    }

    #[test]
    fn relative_times() {
        let now = parse_iso("2026-11-21T12:00:00Z").unwrap();
        assert_eq!(time_ago("2026-11-21T11:59:40Z", now), "just now");
        assert_eq!(time_ago("2026-11-21T11:55:00Z", now), "5m ago");
        assert_eq!(time_ago("2026-11-21T09:00:00Z", now), "3h ago");
        assert_eq!(time_ago("2026-11-20T09:00:00Z", now), "yesterday");
        assert_eq!(time_ago("2026-11-17T09:00:00Z", now), "4d ago");
        assert_eq!(time_ago("2026-11-01T09:00:00Z", now), "2w ago");
        assert_eq!(short_time("2026-11-21T11:55:00Z", now), "5m");
        assert_eq!(short_time("2026-11-19T11:55:00Z", now), "2d");
        assert_eq!(short_time("", now), "—");
    }

    #[test]
    fn durations_and_day_titles() {
        assert_eq!(format_duration(9), "9s");
        assert_eq!(format_duration(125), "2m 05s");
        assert_eq!(format_duration(3_900), "1h 05m");
        let now = parse_iso("2026-11-21T12:00:00Z").unwrap();
        assert_eq!(elapsed("2026-11-21T11:00:00Z", "2026-11-21T11:01:30Z", false, now).as_deref(), Some("1m 30s"));
        assert_eq!(elapsed("2026-11-21T11:00:00Z", "", true, now).as_deref(), Some("1h 00m"));
        assert_eq!(day_title("2026-11-21T01:00:00Z", now), "Today");
        assert_eq!(day_title("2026-11-20T23:00:00Z", now), "Yesterday");
        assert_eq!(day_title("2026-03-03T10:00:00Z", now), "Mar 3");
        assert_eq!(day_title("2025-03-03T10:00:00Z", now), "Mar 3, 2025");
    }

    #[test]
    fn pr_status_and_merge_readiness() {
        assert_eq!(PrStatus::of(&pr(1, "MERGED", true)), PrStatus::Merged);
        assert_eq!(PrStatus::of(&pr(1, "OPEN", true)), PrStatus::Draft);
        let ms = |s, state, mg, r| merge_status(s, state, mg, r);
        assert!(ms(PrStatus::Open, "CLEAN", "MERGEABLE", "APPROVED").ready);
        assert_eq!(ms(PrStatus::Open, "DIRTY", "", "").text, "Has conflicts");
        assert_eq!(ms(PrStatus::Open, "CLEAN", "CONFLICTING", "").text, "Has conflicts");
        assert_eq!(ms(PrStatus::Draft, "CLEAN", "", "").text, "Draft");
        assert_eq!(ms(PrStatus::Open, "BLOCKED", "", "CHANGES_REQUESTED").text, "Changes requested");
        assert_eq!(ms(PrStatus::Open, "BLOCKED", "", "").text, "Review required");
        assert_eq!(ms(PrStatus::Open, "BLOCKED", "", "APPROVED").text, "Blocked by checks");
        assert_eq!(ms(PrStatus::Open, "BEHIND", "", "").text, "Behind base branch");
        let u = ms(PrStatus::Open, "UNSTABLE", "", "");
        assert!(u.ready && u.tone == Tone::Warning);
        assert!(!ms(PrStatus::Merged, "CLEAN", "", "").ready);
        assert_eq!(ms(PrStatus::Open, "", "", "").text, "Checking mergeability");
    }

    #[test]
    fn run_states() {
        assert_eq!(run_state("completed", "success").tone, Tone::Success);
        let f = run_state("completed", "timed_out");
        assert!(f.failed && f.label == "Timed out");
        let r = run_state("in_progress", "");
        assert!(r.active && r.phase == Phase::Running);
        assert_eq!(run_state("queued", "").phase, Phase::Queued);
        assert_eq!(run_state("completed", "action_required").phase, Phase::ActionRequired);
        assert_eq!(run_state("", "").label, "Unknown");
    }

    #[test]
    fn check_tally_text_and_tone() {
        let c = |s: &str, k: &str| Check { status: s.into(), conclusion: k.into(), ..Default::default() };
        let t = tally(&[c("completed", "success"), c("completed", "skipped"), c("in_progress", "")]);
        assert_eq!(t, Tally { pass: 2, fail: 0, pending: 1 });
        assert_eq!(t.text(), "1 running");
        assert_eq!(t.tone(), Tone::Warning);
        let t = tally(&[c("completed", "success"), c("completed", "failure")]);
        assert_eq!((t.text().as_str(), t.tone()), ("1 failing", Tone::Error));
        assert_eq!(tally(&[c("completed", "success")]).text(), "1/1 checks passed");
        assert_eq!(tally(&[]).text(), "No checks");
    }

    #[test]
    fn grouping_and_search() {
        let prs = vec![pr(1, "OPEN", false), pr(2, "OPEN", true), pr(3, "OPEN", false)];
        let g = group_prs(&prs, &[0, 1, 2], "Open");
        assert_eq!(
            g.iter().map(|g| (g.id.as_str(), g.items.clone())).collect::<Vec<_>>(),
            vec![("open", vec![0, 2]), ("drafts", vec![1])]
        );
        let mut p = pr(4, "OPEN", false);
        p.author = "Ana".into();
        p.labels = vec![Label { name: "Bug".into(), color: String::new() }];
        assert!(pr_matches(&p, "ana") && pr_matches(&p, "#4") && pr_matches(&p, "bug") && !pr_matches(&p, "zzz"));
        let is = vec![
            IssueItem { number: 1, state: "OPEN".into(), ..Default::default() },
            IssueItem { number: 2, state: "CLOSED".into(), ..Default::default() },
        ];
        let g = group_issues(&is, &[0, 1]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[1].title, "Closed");
    }

    #[test]
    fn runs_group_by_day() {
        let now = parse_iso("2026-11-21T12:00:00Z").unwrap();
        let r = |c: &str| RunItem { id: 1, created: c.into(), ..Default::default() };
        let runs = vec![r("2026-11-21T10:00:00Z"), r("2026-11-21T09:00:00Z"), r("2026-11-20T09:00:00Z")];
        let g = group_runs(&runs, &[0, 1, 2], now);
        assert_eq!((g[0].title.as_str(), g[0].items.len(), g[1].title.as_str()), ("Today", 2, "Yesterday"));
    }

    #[test]
    fn run_filters() {
        let r = |s: &str, c: &str| RunItem { status: s.into(), conclusion: c.into(), ..Default::default() };
        assert!(RunFilter::InProgress.matches(&r("in_progress", "")));
        assert!(RunFilter::Failed.matches(&r("completed", "cancelled")));
        assert!(RunFilter::Successful.matches(&r("completed", "success")));
        assert!(!RunFilter::Successful.matches(&r("completed", "failure")));
    }

    #[test]
    fn label_and_avatar_colors() {
        assert_eq!(label_rgb("d73a4a"), Some((0xd7, 0x3a, 0x4a)));
        assert_eq!(label_rgb("#fff"), None);
        assert_eq!(avatar_hue("ana"), avatar_hue("ana"));
        assert!(avatar_hue("ana") < 360);
        let (r, g, b) = hue_rgb(0);
        assert!(r > g && r > b);
        assert_eq!(initial("ana"), "A");
        assert_eq!(initial(""), "?");
        assert_eq!(short_sha("0123456789"), "0123456");
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(2, "file"), "2 files");
    }
}
