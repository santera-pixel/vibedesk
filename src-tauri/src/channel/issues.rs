//! The issue buffer.
//!
//! Every backend can see something go wrong — a console error, an uncaught
//! exception, a failed request, a non-zero exit. They all land here in one
//! shape, so a tool result can carry "what broke" without the agent asking for
//! it.
//!
//! Two properties matter more than they look. A repeated error increments a
//! count and moves to the back rather than filling the ring with copies,
//! because a loop that throws the same exception sixty times is one problem and
//! not sixty. And every entry carries a sequence number, so "new since the last
//! call" survives the reordering that deduplication causes — an index into the
//! deque would not.

use std::collections::VecDeque;

use serde::Serialize;

/// How bad an entry is. Ordered so a caller can filter to errors only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// One thing that went wrong, deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub level: Level,
    pub source: String,
    pub text: String,
    pub at: String,
    /// How many times this exact source and text have arrived.
    pub count: u32,
    /// Arrival order, used to answer "new since the last call". Not serialised:
    /// it is bookkeeping, not something the agent acts on.
    #[serde(skip)]
    pub seq: u64,
}

impl Issue {
    /// An entry the channeler itself produced, rather than one the target did.
    ///
    /// Used when the channeler cannot read the target's issues at all: the
    /// failure to observe is itself something the agent needs to see, and
    /// dropping it would make a blind test look like a clean one.
    pub fn note(level: Level, source: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            level,
            source: source.into(),
            text: text.into(),
            at: String::new(),
            count: 1,
            seq: 0,
        }
    }
}

/// A bounded, deduplicated buffer of issues.
pub struct Issues {
    items: VecDeque<Issue>,
    cap: usize,
    next_seq: u64,
    drained_seq: u64,
}

impl Issues {
    pub fn new(cap: usize) -> Self {
        Self {
            items: VecDeque::new(),
            cap: cap.max(1),
            next_seq: 1,
            drained_seq: 0,
        }
    }

    /// Record an issue, folding a repeat into the entry already there.
    pub fn push(
        &mut self,
        level: Level,
        source: impl Into<String>,
        text: impl Into<String>,
        at: impl Into<String>,
    ) {
        let source = source.into();
        let text = text.into();
        let at = at.into();
        let seq = self.next_seq;
        self.next_seq += 1;

        if let Some(pos) = self
            .items
            .iter()
            .position(|item| item.source == source && item.text == text)
        {
            let mut item = self
                .items
                .remove(pos)
                .expect("the position came from this deque");
            item.count += 1;
            item.seq = seq;
            item.at = at;
            item.level = level;
            self.items.push_back(item);
            return;
        }

        self.items.push_back(Issue {
            level,
            source,
            text,
            at,
            count: 1,
            seq,
        });
        while self.items.len() > self.cap {
            self.items.pop_front();
        }
    }

    /// Everything the caller has not seen, oldest first, capped at `limit`.
    ///
    /// The second value is how many new entries were left out, so a caller can
    /// say "and 14 more" instead of truncating silently. Only what was actually
    /// returned is marked seen — otherwise a capped drain would swallow the
    /// entries it did not show.
    pub fn drain_new(&mut self, limit: usize) -> (Vec<Issue>, usize) {
        let mut new: Vec<Issue> = self
            .items
            .iter()
            .filter(|item| item.seq > self.drained_seq)
            .cloned()
            .collect();
        new.sort_by_key(|item| item.seq);

        let total = new.len();
        if total > limit {
            new.truncate(limit);
        }
        if let Some(last) = new.last() {
            self.drained_seq = last.seq;
        }
        (new, total.saturating_sub(limit))
    }

    /// Everything still in the ring, optionally filtered to a minimum level.
    pub fn all(&self, at_least: Option<Level>) -> Vec<Issue> {
        let mut items: Vec<Issue> = self
            .items
            .iter()
            .filter(|item| at_least.map(|level| item.level >= level).unwrap_or(true))
            .cloned()
            .collect();
        items.sort_by_key(|item| item.seq);
        items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.drained_seq = self.next_seq.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer() -> Issues {
        Issues::new(8)
    }

    #[test]
    fn a_repeat_increments_rather_than_duplicating() {
        let mut issues = buffer();
        issues.push(Level::Error, "console", "boom", "t1");
        issues.push(Level::Error, "console", "boom", "t2");

        assert_eq!(issues.len(), 1);
        let all = issues.all(None);
        assert_eq!(all[0].count, 2);
        assert_eq!(all[0].at, "t2", "the latest arrival wins the timestamp");
    }

    #[test]
    fn the_ring_drops_the_oldest() {
        let mut issues = Issues::new(2);
        issues.push(Level::Error, "a", "one", "t1");
        issues.push(Level::Error, "b", "two", "t2");
        issues.push(Level::Error, "c", "three", "t3");

        let all = issues.all(None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].text, "two");
        assert_eq!(all[1].text, "three");
    }

    #[test]
    fn drain_returns_only_what_is_new() {
        let mut issues = buffer();
        issues.push(Level::Error, "console", "first", "t1");

        let (first, more) = issues.drain_new(10);
        assert_eq!(first.len(), 1);
        assert_eq!(more, 0);

        let (second, _) = issues.drain_new(10);
        assert!(second.is_empty(), "nothing new since the last drain");

        issues.push(Level::Warn, "console", "second", "t2");
        let (third, _) = issues.drain_new(10);
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].text, "second");
    }

    #[test]
    fn a_repeat_after_a_drain_is_new_again() {
        let mut issues = buffer();
        issues.push(Level::Error, "console", "boom", "t1");
        let _ = issues.drain_new(10);

        issues.push(Level::Error, "console", "boom", "t2");
        let (new, _) = issues.drain_new(10);
        assert_eq!(new.len(), 1, "a repeat is news, not a duplicate");
        assert_eq!(new[0].count, 2);
    }

    #[test]
    fn a_capped_drain_reports_the_remainder_and_keeps_it() {
        let mut issues = buffer();
        for index in 0..5 {
            issues.push(Level::Error, "console", format!("error {index}"), "t");
        }

        let (shown, more) = issues.drain_new(2);
        assert_eq!(shown.len(), 2);
        assert_eq!(more, 3);

        let (rest, more) = issues.drain_new(10);
        assert_eq!(rest.len(), 3, "the capped entries were not swallowed");
        assert_eq!(more, 0);
    }

    #[test]
    fn filtering_by_level_keeps_errors_only() {
        let mut issues = buffer();
        issues.push(Level::Info, "log", "fyi", "t1");
        issues.push(Level::Warn, "log", "careful", "t2");
        issues.push(Level::Error, "log", "broken", "t3");

        let errors = issues.all(Some(Level::Error));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].text, "broken");

        let warnings_up = issues.all(Some(Level::Warn));
        assert_eq!(warnings_up.len(), 2);
    }
}
