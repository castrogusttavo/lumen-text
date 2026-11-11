//! Text-level rewrites done once the text events of a paragraph are merged: GitHub emoji
//! shortcodes (`:rocket:`) and GFM-style autolinks for bare URLs (`https://..`, `www.`) and e-mail
//! addresses. Code spans, key caps and math are never touched.

use super::emoji_table::EMOJI;
use super::*;
use std::borrow::Cow;

/// The emoji for shortcode `name` (without the colons).
pub(super) fn emoji(name: &str) -> Option<&'static str> {
    EMOJI.binary_search_by(|(k, _)| (*k).cmp(name)).ok().map(|i| EMOJI[i].1)
}

/// `text` with every known `:shortcode:` replaced by its emoji (unknown ones stay as typed).
pub(super) fn replace_shortcodes(text: &str) -> Cow<'_, str> {
    if !text.contains(':') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(i) = rest.find(':') {
        let after = &rest[i + 1..];
        let n = after.bytes().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-')).count();
        let hit = (n > 0 && after.as_bytes().get(n) == Some(&b':')).then(|| emoji(&after[..n])).flatten();
        match hit {
            Some(e) => {
                out.push_str(&rest[..i]);
                out.push_str(e);
                rest = &after[n + 1..];
                changed = true;
            }
            None => {
                out.push_str(&rest[..=i]);
                rest = after;
            }
        }
    }
    if !changed {
        return Cow::Borrowed(text);
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn url_char_ok_before(c: Option<char>) -> bool {
    c.is_none_or(|c| c.is_whitespace() || "(*_~\"'".contains(c))
}

/// End of a bare URL starting at `s[0]`: up to whitespace or `<`, minus trailing punctuation and an
/// unbalanced closing parenthesis.
fn url_end(s: &str) -> usize {
    let mut end = s.find(|c: char| c.is_whitespace() || c == '<').unwrap_or(s.len());
    loop {
        let t = &s[..end];
        let Some(last) = t.chars().last() else { break };
        if "?!.,:*_~'\";".contains(last) {
            end -= last.len_utf8();
        } else if last == ')' && t.matches('(').count() < t.matches(')').count() {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// `(start, end, target)` of the bare URLs in `text`.
fn find_urls(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = vec![];
    let mut from = 0;
    while from < text.len() {
        let rest = &text[from..];
        let cand = ["https://", "http://", "www."].iter().filter_map(|p| rest.find(p).map(|i| (i, *p))).min();
        let Some((i, p)) = cand else { break };
        let at = from + i;
        let before = text[..at].chars().last();
        let len = url_end(&text[at..]);
        let body = &text[at..at + len];
        let ok = url_char_ok_before(before)
            && len > p.len()
            && (p != "www." || body[4..].contains('.'))
            && body[p.len()..].chars().next().is_some_and(|c| c.is_alphanumeric());
        if ok {
            let target = if p == "www." { format!("http://{body}") } else { body.to_string() };
            out.push((at, at + len, target));
            from = at + len;
        } else {
            from = at + p.len();
        }
    }
    out
}

/// `(start, end)` of the bare e-mail addresses in `text`.
fn find_emails(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = vec![];
    let mut last_end = 0;
    for (at, _) in text.match_indices('@') {
        if at < last_end {
            continue;
        }
        let local = |c: u8| c.is_ascii_alphanumeric() || b".+-_".contains(&c);
        let mut s = at;
        while s > last_end && local(b[s - 1]) {
            s -= 1;
        }
        let mut e = at + 1;
        while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'.' || b[e] == b'-' || b[e] == b'_') {
            e += 1;
        }
        while e > at + 1 && matches!(b[e - 1], b'.' | b'-' | b'_') {
            e -= 1;
        }
        let domain = &text[at + 1..e];
        let before_ok = s == 0 || !(b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'/' || b[s - 1] == b':');
        if s < at && domain.contains('.') && !domain.starts_with('.') && before_ok {
            out.push((s, e));
            last_end = e;
        }
    }
    out
}

/// `text` cut into plain pieces and bare links: `(piece, Some(target))` for a link.
pub(super) fn linkify(text: &str) -> Vec<(&str, Option<String>)> {
    let maybe_url = text.contains("://") || text.contains("www.");
    let maybe_mail = text.contains('@');
    if !maybe_url && !maybe_mail {
        return vec![(text, None)];
    }
    let mut hits: Vec<(usize, usize, String)> = if maybe_url { find_urls(text) } else { vec![] };
    if maybe_mail {
        for (s, e) in find_emails(text) {
            if !hits.iter().any(|(a, b, _)| s < *b && e > *a) {
                hits.push((s, e, format!("mailto:{}", &text[s..e])));
            }
        }
        hits.sort_by_key(|h| h.0);
    }
    let mut out = vec![];
    let mut at = 0;
    for (s, e, t) in hits {
        if s > at {
            out.push((&text[at..s], None));
        }
        out.push((&text[s..e], Some(t)));
        at = e;
    }
    if at < text.len() {
        out.push((&text[at..], None));
    }
    out
}

fn needs_rewrite(t: &str) -> bool {
    t.contains(':') || t.contains("www.") || t.contains('@')
}

/// Spans with emoji shortcodes replaced and bare URLs / e-mail addresses linked. Text that already
/// is a link only gets its shortcodes replaced; code, key caps and math stay as typed.
pub(super) fn rewrite(spans: Vec<Inl>, urls: &mut Urls) -> Vec<Inl> {
    let skip = F_CODE | F_KBD | F_MATH;
    let any =
        spans.iter().any(|s| matches!(s, Inl::Text { text, flags, .. } if flags & skip == 0 && needs_rewrite(text)));
    if !any {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + 2);
    for s in spans {
        match s {
            Inl::Text { text, flags, link } if flags & skip == 0 && needs_rewrite(&text) => {
                if link != 0 {
                    out.push(Inl::Text { text: replace_shortcodes(&text).into_owned(), flags, link });
                    continue;
                }
                for (piece, target) in linkify(&text) {
                    match target {
                        Some(t) => {
                            out.push(Inl::Text { text: piece.to_string(), flags: flags | F_LINK, link: urls.id(&t) })
                        }
                        None => out.push(Inl::Text { text: replace_shortcodes(piece).into_owned(), flags, link: 0 }),
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether `url` names a video file (by extension).
pub(super) fn is_video(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    [".mp4", ".webm", ".mov", ".m4v", ".ogv", ".mkv"].iter().any(|e| path.ends_with(e))
}

/// A paragraph that is nothing but one link to a video file: the video to show as a card.
pub(super) fn lone_video(spans: &[Inl], urls: &Urls) -> Option<Img> {
    let mut link = 0;
    let mut alt = String::new();
    for s in spans {
        match s {
            Inl::Text { text, link: l, .. } if *l != 0 && (link == 0 || link == *l) => {
                link = *l;
                alt.push_str(text);
            }
            Inl::Text { text, .. } if text.trim().is_empty() => {}
            Inl::Soft => {}
            _ => return None,
        }
    }
    let url = urls.get(link)?.clone();
    if !is_video(&url) {
        return None;
    }
    let alt = alt.trim();
    let alt = if alt == url { String::new() } else { alt.to_string() };
    Some(Img { alt, url, link, size: (None, None), video: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcodes() {
        assert_eq!(emoji("rocket"), Some("\u{1f680}"));
        assert_eq!(emoji("snake"), Some("\u{1f40d}"));
        assert_eq!(emoji("white_check_mark"), Some("\u{2705}"));
        assert_eq!(emoji("+1"), Some("\u{1f44d}"));
        assert_eq!(emoji("no_such_thing"), None);
        assert_eq!(replace_shortcodes("go :rocket: now"), "go \u{1f680} now");
        assert_eq!(replace_shortcodes("::snake:: 10:30:45 :nope:"), ":\u{1f40d}: 10:30:45 :nope:");
        assert!(matches!(replace_shortcodes("no colon"), Cow::Borrowed(_)));
        // the table is sorted, so the binary search finds every entry
        assert!(EMOJI.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(EMOJI.len() > 1500);
    }

    #[test]
    fn bare_urls_and_emails() {
        let p = linkify("see https://pypi.org, or www.rust-lang.org. Mail a.b@ex.com!");
        let links: Vec<(&str, String)> = p.iter().filter_map(|(s, t)| t.clone().map(|t| (*s, t))).collect();
        assert_eq!(
            links,
            [
                ("https://pypi.org", "https://pypi.org".to_string()),
                ("www.rust-lang.org", "http://www.rust-lang.org".to_string()),
                ("a.b@ex.com", "mailto:a.b@ex.com".to_string()),
            ]
        );
        assert_eq!(
            p.iter().map(|(s, _)| *s).collect::<String>(),
            "see https://pypi.org, or www.rust-lang.org. Mail a.b@ex.com!"
        );
        let w = linkify("(https://en.wikipedia.org/wiki/Rust_(language)) end");
        assert_eq!(w[1].0, "https://en.wikipedia.org/wiki/Rust_(language)");
        assert!(linkify("http:// nothing @ here").iter().all(|(_, t)| t.is_none()));
        assert_eq!(linkify("plain").len(), 1);
    }

    #[test]
    fn videos() {
        assert!(is_video("demo.MP4"));
        assert!(is_video("https://x.io/a.webm?raw=1"));
        assert!(!is_video("https://x.io/a.png"));
    }
}
