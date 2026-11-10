//! A bounded log of what a language server printed (stderr) and of the JSON-RPC traffic summary,
//! shown by "LSP: Show server log". Fixed memory: at most [`MAX_LINES`] lines of [`MAX_LINE`] chars.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

pub const MAX_LINES: usize = 400;
pub const MAX_LINE: usize = 240;

#[derive(Debug)]
pub struct LogRing {
    lines: VecDeque<String>,
    t0: Instant,
    /// Lines dropped because the ring was full.
    pub dropped: usize,
}

impl Default for LogRing {
    fn default() -> Self {
        LogRing { lines: VecDeque::new(), t0: Instant::now(), dropped: 0 }
    }
}

pub type SharedLog = Arc<Mutex<LogRing>>;

impl LogRing {
    /// `tag` is a short channel name: "stderr", "->" (sent), "<-" (received), "app".
    pub fn push(&mut self, tag: &str, text: &str) {
        let mut line: String = text.chars().take(MAX_LINE).collect();
        if text.chars().count() > MAX_LINE {
            line.push('…');
        }
        let secs = self.t0.elapsed().as_secs_f32();
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(format!("[{secs:8.2}s] {tag:<6} {}", line.trim_end()));
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        if self.dropped > 0 {
            s.push_str(&format!("(… {} older lines dropped)\n", self.dropped));
        }
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

pub fn push(log: &SharedLog, tag: &str, text: &str) {
    if let Ok(mut l) = log.lock() {
        l.push(tag, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_bounded_and_truncates_long_lines() {
        let mut r = LogRing::default();
        for i in 0..(MAX_LINES + 50) {
            r.push("stderr", &format!("line {i}"));
        }
        assert_eq!(r.len(), MAX_LINES);
        assert_eq!(r.dropped, 50);
        let t = r.text();
        assert!(t.starts_with("(… 50 older lines dropped)"));
        assert!(t.contains("line 449") && !t.contains("line 49\n"));
        r.push("->", &"x".repeat(5000));
        assert!(r.text().lines().last().unwrap().chars().count() < MAX_LINE + 40);
    }
}
