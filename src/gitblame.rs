//! The blame popover: a card for the commit behind a line, as Markdown for the hover popup.
//!
//! The card has the hash, author, date, the full message, the files the commit changed (each one
//! opens the commit's diff of that file) and the actions "Copy hash", "Open on GitHub" and "File
//! history from this line" (`git log -L`). The actions are `lumen:git:` links, parsed back by
//! [`parse_link`] when the popup reports a click.
//!
//! Everything is plain code over `git`; the App runs [`blame_markdown`] and [`line_history`] on a
//! worker thread (see `app_gitext`).

use crate::gitx::Commit;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn run(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn is_hash(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// The all-zero hash `git blame` gives to lines that are not committed yet.
pub fn is_uncommitted(hash: &str) -> bool {
    !hash.is_empty() && hash.chars().all(|c| c == '0')
}

/// The commit that last changed the 0-based `line` of `path` (the all-zero hash when the line is
/// not committed).
pub fn blame_hash(path: &Path, line: usize) -> Option<String> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let l = format!("{0},{0}", line + 1);
    let out = run(dir, &["blame", "-L", &l, "--porcelain", "--", &name])?;
    let h = out.split_whitespace().next()?;
    is_hash(h).then(|| h.to_string())
}

/// What the card shows about a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitCard {
    pub hash: String,
    pub author: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Author time, seconds since the epoch.
    pub at: i64,
    pub message: String,
}

/// Format for `git show`: hash, author, short date, author time, full message.
const CARD_FORMAT: &str = "--format=%H%x1f%an%x1f%ad%x1f%at%x1f%B";

pub fn parse_card(out: &str) -> Option<CommitCard> {
    let mut p = out.splitn(5, '\x1f');
    let hash = p.next()?.trim().to_string();
    let author = p.next()?.to_string();
    let date = p.next()?.to_string();
    let at: i64 = p.next()?.trim().parse().ok()?;
    let message = p.next()?.trim().to_string();
    is_hash(&hash).then_some(CommitCard { hash, author, date, at, message })
}

pub fn commit_card(dir: &Path, hash: &str) -> Option<CommitCard> {
    if !is_hash(hash) {
        return None;
    }
    parse_card(&run(dir, &["show", "-s", "--date=short", CARD_FORMAT, hash])?)
}

/// Commits that changed the 0-based `line` of `path`, newest first (`git log -L`).
pub fn line_history(path: &Path, line: usize) -> Vec<Commit> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return Vec::new() };
    let spec = format!("{0},{0}:{1}", line + 1, name.to_string_lossy());
    let fmt = "--pretty=format:%H%x1f%h%x1f%an%x1f%ar%x1f%s";
    run(dir, &["log", "-L", &spec, fmt, "-n", "60"]).map(|o| parse_line_log(&o)).unwrap_or_default()
}

/// Keep the commit lines of `git log -L` output (the diffs between them are skipped).
pub fn parse_line_log(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\x1f').collect();
            if p.len() != 5 || !is_hash(p[0]) {
                return None;
            }
            Some(Commit {
                hash: p[0].to_string(),
                short: p[1].to_string(),
                author: p[2].to_string(),
                when: p[3].to_string(),
                subject: p[4].to_string(),
            })
        })
        .collect()
}

/// `https://github.com/<slug>/commit/<hash>`.
pub fn commit_url(slug: &str, hash: &str) -> String {
    format!("https://github.com/{slug}/commit/{hash}")
}

// ───────────── links ─────────────

/// Percent-encode everything but `A-Za-z0-9._~/-` (a link target may hold spaces, colons...).
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'/' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a `lumen:git:` link asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitLink {
    CopyHash(String),
    /// Diff of one file in a commit.
    Diff {
        hash: String,
        path: PathBuf,
    },
    /// History of one (0-based) line of a file.
    History {
        path: PathBuf,
        line: usize,
    },
}

pub fn link_copy(hash: &str) -> String {
    format!("lumen:git:copy:{hash}")
}

pub fn link_diff(hash: &str, path: &Path) -> String {
    format!("lumen:git:diff:{hash}:{}", enc(&path.to_string_lossy()))
}

pub fn link_history(path: &Path, line: usize) -> String {
    format!("lumen:git:hist:{}:{line}", enc(&path.to_string_lossy()))
}

pub fn parse_link(url: &str) -> Option<GitLink> {
    let rest = url.strip_prefix("lumen:git:")?;
    let mut p = rest.split(':');
    match p.next()? {
        "copy" => {
            let h = p.next()?;
            is_hash(h).then(|| GitLink::CopyHash(h.to_string()))
        }
        "diff" => {
            let h = p.next()?;
            let path = PathBuf::from(dec(p.next()?));
            is_hash(h).then(|| GitLink::Diff { hash: h.to_string(), path })
        }
        "hist" => {
            let path = PathBuf::from(dec(p.next()?));
            Some(GitLink::History { path, line: p.next()?.parse().ok()? })
        }
        _ => None,
    }
}

// ───────────── the card ─────────────

/// Escape what Markdown would read as formatting.
pub fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// A free-text block (a commit message, a comment) as Markdown that reads like the original: no
/// accidental lists, headings or links; blank lines kept; cut after `max_lines` lines.
pub fn text_block(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    for l in lines.iter().take(max_lines) {
        let e = md_escape(l.trim_end());
        let t = e.trim_start();
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        let listy = t.starts_with("- ") || t.starts_with("+ ") || (digits > 0 && t[digits..].starts_with(['.', ')']));
        // A hard break at the end keeps the line breaks of the message.
        out.push(if listy { format!("\\{e}") } else { e });
    }
    if lines.len() > max_lines {
        out.push("…".to_string());
    }
    out.join("  \n")
}

/// What the card needs besides the commit.
pub struct CardCtx<'a> {
    /// Absolute path of the file the line belongs to.
    pub file: &'a Path,
    /// Repository root: the changed files are paths under it.
    pub top: &'a Path,
    /// 0-based blamed line.
    pub line: usize,
    /// `owner/repo` of the GitHub remote, when there is one.
    pub slug: Option<&'a str>,
    /// Now, seconds since the epoch (the card says "3 days ago").
    pub now: i64,
}

/// Files listed in the card before "… and N more".
const FILES_SHOWN: usize = 12;
const MESSAGE_LINES: usize = 20;

pub fn card_markdown(c: &CommitCard, files: &[(char, String)], ctx: &CardCtx) -> String {
    let short = &c.hash[..c.hash.len().min(8)];
    let mut s =
        format!("**{short}** · {} · {} ({})\n\n", md_escape(&c.author), crate::git::ago(ctx.now - c.at), c.date);
    if !c.message.is_empty() {
        s.push_str(&text_block(&c.message, MESSAGE_LINES));
        s.push_str("\n\n");
    }
    if !files.is_empty() {
        s.push_str(&format!("**Changed files** ({})\n\n", files.len()));
        for (code, p) in files.iter().take(FILES_SHOWN) {
            let abs = ctx.top.join(p);
            s.push_str(&format!("- [{code} {}]({})\n", md_escape(p), link_diff(&c.hash, &abs)));
        }
        if files.len() > FILES_SHOWN {
            s.push_str(&format!("- … and {} more\n", files.len() - FILES_SHOWN));
        }
        s.push('\n');
    }
    let mut actions = vec![format!("[Copy hash]({})", link_copy(&c.hash))];
    if let Some(slug) = ctx.slug {
        actions.push(format!("[Open on GitHub]({})", commit_url(slug, &c.hash)));
    }
    actions.push(format!("[File history from this line]({})", link_history(ctx.file, ctx.line)));
    s.push_str(&actions.join(" · "));
    s
}

/// The whole card for the 0-based `line` of `path`, or why there is none.
pub fn blame_markdown(path: &Path, line: usize) -> Result<String, String> {
    let dir = path.parent().ok_or("No folder for this file")?;
    let hash = blame_hash(path, line).ok_or("No blame for this line (is the file tracked by git?)")?;
    if is_uncommitted(&hash) {
        return Err("This line is not committed yet".into());
    }
    let top = crate::git::toplevel(dir).ok_or("Not a git repository")?;
    let card = commit_card(&top, &hash).ok_or("The commit could not be read")?;
    let files: Vec<(char, String)> =
        crate::app_gitpanel::commit_files(&top, &hash).into_iter().map(|f| (f.code, f.path)).collect();
    let slug = crate::app_gitpanel::github_remotes(&top, None).into_iter().next().map(|(_, s)| s);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    Ok(card_markdown(&card, &files, &CardCtx { file: path, top: &top, line, slug: slug.as_deref(), now }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-blamecard-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn g(d: &Path, args: &[&str]) {
        Command::new("git").arg("-C").arg(d).args(args).output().unwrap();
    }

    fn repo(tag: &str) -> PathBuf {
        let d = tmp(tag);
        g(&d, &["init", "-q", "-b", "main"]);
        g(&d, &["config", "user.email", "t@t"]);
        g(&d, &["config", "user.name", "Ana *Dev*"]);
        g(&d, &["config", "commit.gpgsign", "false"]);
        d
    }

    #[test]
    fn links_round_trip() {
        let h = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(parse_link(&link_copy(h)), Some(GitLink::CopyHash(h.into())));
        let p = Path::new("/home/a b/src:x/(f).rs");
        assert_eq!(parse_link(&link_diff(h, p)), Some(GitLink::Diff { hash: h.into(), path: p.to_path_buf() }));
        assert_eq!(parse_link(&link_history(p, 41)), Some(GitLink::History { path: p.to_path_buf(), line: 41 }));
        assert!(!link_diff(h, p).contains(' ') && !link_diff(h, p).contains('('));
        assert_eq!(parse_link("lumen:git:copy:not-a-hash!"), None);
        assert_eq!(parse_link("lumen:git:nope:1"), None);
        assert_eq!(parse_link("lumen:goto:/x#1,2"), None);
        assert_eq!(dec(&enc("C:\\x y\\é")), "C:\\x y\\é");
        assert_eq!(dec("100%"), "100%", "a stray percent sign is kept");
    }

    #[test]
    fn markdown_is_escaped_and_lists_are_not_made_by_accident() {
        assert_eq!(md_escape("a_b *c* [d]"), "a\\_b \\*c\\* \\[d\\]");
        let t = text_block("fix: x_y\n\n- not a list\n2. nor this\nplain", 10);
        assert_eq!(t, "fix: x\\_y  \n  \n\\- not a list  \n\\2. nor this  \nplain");
        let cut = text_block("1\n2\n3\n4", 2);
        assert!(cut.ends_with("…") && !cut.contains('3'), "{cut}");
    }

    #[test]
    fn card_has_message_files_and_actions() {
        let c = CommitCard {
            hash: "0123456789abcdef0123456789abcdef01234567".into(),
            author: "Ana".into(),
            date: "2026-11-10".into(),
            at: 1_000_000,
            message: "feat: thing\n\nbody".into(),
        };
        let files: Vec<(char, String)> = (0..14).map(|i| ('M', format!("src/f{i}.rs"))).collect();
        let ctx = CardCtx {
            file: Path::new("/r/src/f0.rs"),
            top: Path::new("/r"),
            line: 4,
            slug: Some("o/r"),
            now: 1_000_000 + 86_400 * 3,
        };
        let md = card_markdown(&c, &files, &ctx);
        assert!(md.starts_with("**01234567** · Ana · 3 days ago (2026-11-10)"), "{md}");
        assert!(md.contains("feat: thing") && md.contains("body"));
        assert!(md.contains("**Changed files** (14)") && md.contains("… and 2 more"));
        let diff = link_diff(&c.hash, &Path::new("/r").join("src/f0.rs"));
        assert!(md.contains(&format!("[M src/f0.rs]({diff})")), "{md}");
        assert!(md.contains("[Copy hash](lumen:git:copy:"));
        assert!(md.contains("(https://github.com/o/r/commit/0123456789abcdef0123456789abcdef01234567)"));
        let hist = link_history(Path::new("/r/src/f0.rs"), 4);
        assert!(md.contains(&format!("[File history from this line]({hist})")), "{md}");
        let no_gh = card_markdown(&c, &[], &CardCtx { slug: None, ..ctx });
        assert!(!no_gh.contains("GitHub") && !no_gh.contains("Changed files"));
    }

    #[test]
    fn card_is_parsed_from_git_show() {
        let c = parse_card("abcdef0123456\x1fAna\x1f2026-01-02\x1f1700000000\x1fsubject\n\nbody\n\n").unwrap();
        assert_eq!(
            (c.hash.as_str(), c.author.as_str(), c.date.as_str(), c.at),
            ("abcdef0123456", "Ana", "2026-01-02", 1_700_000_000)
        );
        assert_eq!(c.message, "subject\n\nbody");
        assert!(parse_card("zz\x1fa\x1fb\x1f1\x1fm").is_none());
        assert!(parse_card("").is_none());
    }

    #[test]
    fn line_log_keeps_only_the_commit_lines() {
        let out = "a1b2c3d4e5\x1fa1b2c3d\x1fAna\x1f2 days ago\x1fsecond\n\ndiff --git a/f b/f\n@@ -1 +1 @@\n-x\n+y\n\n\
                   f6e5d4c3b2\x1ff6e5d4c\x1fBo\x1f3 days ago\x1ffirst";
        let v = parse_line_log(out);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].short.as_str(), v[0].subject.as_str(), v[1].author.as_str()), ("a1b2c3d", "second", "Bo"));
    }

    #[test]
    fn blame_card_and_line_history_on_a_real_repository() {
        let d = repo("real");
        std::fs::create_dir_all(d.join("src")).unwrap();
        let f = d.join("src/a.ts");
        std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
        g(&d, &["add", "."]);
        g(&d, &["commit", "-q", "-m", "first: add a_b\n\nlong body here"]);
        std::fs::write(&f, "one\nTWO\nthree\n").unwrap();
        g(&d, &["commit", "-q", "-am", "second: shout"]);
        std::fs::write(&f, "one\nTWO!\nthree\n").unwrap();
        g(&d, &["commit", "-q", "-am", "third: more"]);

        let h = blame_hash(&f, 0).unwrap();
        assert!(!is_uncommitted(&h));
        let first = commit_card(&d, &h).unwrap();
        assert_eq!(first.author, "Ana *Dev*");
        assert!(first.message.starts_with("first: add a_b") && first.message.contains("long body here"));

        let md = blame_markdown(&f, 0).unwrap();
        assert!(md.contains("Ana \\*Dev\\*"), "{md}");
        assert!(md.contains("first: add a\\_b") && md.contains("long body here"), "{md}");
        assert!(md.contains("[A src/a.ts](lumen:git:diff:"), "{md}");
        assert!(md.contains("[File history from this line](lumen:git:hist:"));
        assert!(!md.contains("Open on GitHub"), "no GitHub remote");

        // The line changed twice: three commits touched it (added, shouted, shouted louder).
        let hist = line_history(&f, 1);
        let subjects: Vec<&str> = hist.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["third: more", "second: shout", "first: add a_b"], "{subjects:?}");

        // An uncommitted line has no card.
        std::fs::write(&f, "one\nTWO!\nthree\nfour\n").unwrap();
        assert!(blame_markdown(&f, 3).unwrap_err().contains("not committed"));
        assert!(blame_markdown(&d.join("nope.ts"), 0).is_err());
        std::fs::remove_dir_all(&d).ok();
    }
}
