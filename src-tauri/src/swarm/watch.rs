//! Watching a member's own stream: what it loaded, and whether it is still alive.
//!
//! Two questions, one place, because both are answered by the same notifications
//! and both are asked while the member is still working.
//!
//! **What did it load?** The brief tells a worker to load its skills. Whether it
//! did is not something the worker gets to report: a claim in a transcript is not
//! evidence, and the whole point of assigning a skill is that the run can tell the
//! difference between a task that used the method and one that worked from memory.
//! So the engine reads the member's `tool_call` notifications. They already flow
//! through the sink — the same stream the window renders — and this sits in that
//! path, records the loads, and passes everything through untouched.
//!
//! **Is it still alive?** `last_at` on a member only moves when it writes to the
//! board, so a member with a turn in flight looks identical whether it is thinking
//! or whether its provider stopped answering. The engine's only defence was a
//! thirty-minute ceiling, and the twenty-first live run spent twenty-five of those
//! minutes watching two workers that had emitted nothing at all: the proxy was
//! returning 400s and the run could not tell that from work. A turn that has said
//! nothing since it began is not busy, it is a model call that is not answering —
//! and the difference is visible here, because every chunk of an answer arrives as
//! a notification.
//!
//! The one honest exception is a tool. A worker running `npm test` or a release
//! build emits nothing for minutes and is working perfectly. So the watch also
//! tracks which tool calls are open: silence with a tool in flight is the tool's
//! silence, and silence with nothing in flight is the model's.
//!
//! The parse lives in [`super::skills::loads_from_tool_call`] so the window's
//! `skill-use.ts` and this agree about what a load is. Two implementations of the
//! same rule that disagree would make the panel and the ledger describe different
//! runs.
//!
//! **What is it doing?** The third question, and the one the first two cannot
//! answer. A member's state word says `working` for the whole of a thirteen
//! minute turn, and a live run spent thirteen of those minutes with the panel
//! reporting a verifier that was working and nothing else. The same
//! notifications that answer the other two questions say exactly what it is
//! doing: a `tool_call` names the tool, a `tool_call_update` says when it
//! finished, a chunk of prose says it is thinking, and a permission request says
//! it is waiting on the gate. So the watch keeps a bounded ring of those, with
//! the token total at the moment each one landed, and the panel draws the ring
//! for the member the user selected.
//!
//! The ring is per session, not per member: a respawn starts a new one, and the
//! member's total spend is the ledger's job rather than the feed's. That is why
//! the token reading here is folded from the same `acp://usage` events the gate
//! folds, with the same [`Usage::observe`] — the ring answers "how fast", the
//! ledger answers "how much", and neither is a second opinion about the other.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use crate::acp::bridge::{EventSink, EVENT_REQUEST, EVENT_USAGE};

/// How many announced tool call titles are kept per member.
///
/// A permission request arrives moments after its announcement, so the window
/// that matters is tiny. The cap exists so a long run cannot grow this without
/// limit, and it is generous enough that a burst of parallel calls is never
/// evicted before it is asked about.
const TITLES_KEPT: usize = 64;

/// How many activity items a member's feed keeps.
///
/// Two hundred, the same order as the board tail, because it answers the same
/// kind of question: what has this member been doing lately. A turn that runs a
/// suite, reads four files and writes a patch produces a dozen lines, so two
/// hundred is several turns of history — enough to see a pattern, small enough
/// that one member's feed is a few tens of kilobytes on the wire.
const ACTIVITY_KEPT: usize = 200;

/// How much of a member's prose one feed line keeps.
///
/// A chunk is a token or two, so the line is a summary of what the member said
/// rather than the saying of it. The transcript is the place for the whole
/// answer; this is the place for enough of it to recognise which answer it was.
const MESSAGE_KEPT: usize = 240;

use super::skills;
use super::{Activity, ActivityItem, ActivityKind, Reading, Usage, RATE_KEPT};

/// What a member's stream last said, and what it is waiting on.
///
/// Times are `Instant`, not wall clock: this is a duration question ("how long
/// has it been quiet") and a clock that can jump backwards would answer it wrong.
#[derive(Debug, Default)]
struct Clock {
    /// When the last notification arrived.
    seen: Option<Instant>,
    /// When the current turn was asked for.
    ///
    /// A turn that has not spoken yet is silent from its own start, not from the
    /// previous turn's last word. Without this, a member that finished a turn ten
    /// minutes ago and was just given a new one would read as ten minutes silent
    /// the instant it began.
    turn: Option<Instant>,
    /// The same moment as `turn`, in the clock the panel reads.
    ///
    /// Recorded twice because the two readers ask different questions. `turn` is
    /// an `Instant` because the quiet clock is a duration question and a wall
    /// clock that jumps would answer it wrong. The panel asks "how long has this
    /// turn been running", which needs a time it can subtract from its own clock.
    ///
    /// Cleared when the turn ends, which `turn` is not: the quiet clock keeps its
    /// anchor so a member that ended a turn and has said nothing since is still
    /// measured from the turn's start rather than from the beginning of time.
    turn_at: Option<String>,
    /// Tool calls announced and not yet finished, by id.
    open: BTreeSet<String>,
}

impl Clock {
    /// When the current quiet spell began: the later of the turn's start and the
    /// last notification.
    fn quiet_since(&self) -> Option<Instant> {
        match (self.seen, self.turn) {
            (Some(seen), Some(turn)) => Some(seen.max(turn)),
            (Some(seen), None) => Some(seen),
            (None, Some(turn)) => Some(turn),
            (None, None) => None,
        }
    }
}

/// The bounded ring of things a member did, and what it had spent by then.
///
/// One lock for both, because they are read together: an item's token reading is
/// only meaningful next to the item, and a reader that took the ring and the
/// counter under two locks could report a line whose reading came from after it.
#[derive(Debug, Default)]
struct Feed {
    /// Oldest first. The panel reverses it; the ring does not, because the
    /// oldest is what falls off the front.
    items: VecDeque<ActivityItem>,
    /// What this session has spent, folded from the `acp://usage` readings.
    ///
    /// The same [`Usage::observe`] the gate folds with, so the two cannot
    /// disagree about what a reading means. Per session rather than per member,
    /// because the ring is per session too: a respawn starts a new feed, and a
    /// rate that carried the previous session's total would show a spike that
    /// never happened.
    tokens: Usage,
    /// The token readings of the current turn, oldest first.
    ///
    /// A second ring rather than a slice of `items`, because the two are sampled
    /// at different moments and answer different questions. `items` is one line
    /// per thing the member did; this is one reading per `acp://usage` event,
    /// which is the moment the number actually moved. Deriving the series from
    /// the lines would put a point on every tool call that spent nothing and
    /// none on a model call that spent everything.
    ///
    /// Cleared when a turn starts and when it ends, so the series is the turn and
    /// nothing else. That is what makes a flat sparkline mean "not spending"
    /// rather than "spending nothing lately": a parked member has no turn, so it
    /// has no readings, so its card draws a flat line.
    series: VecDeque<Reading>,
}

impl Feed {
    /// Add a line, dropping the oldest if the ring is full.
    ///
    /// The token reading is stamped here rather than by the caller, so no caller
    /// can record a line without one and no two callers can disagree about which
    /// reading a line carries.
    fn push(&mut self, mut item: ActivityItem) {
        item.tokens = self.tokens.total();
        if self.items.len() >= ACTIVITY_KEPT {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    /// Record a chunk of prose as one line.
    ///
    /// A chunk is a token or two, and a ring of two hundred of them would hold
    /// about twenty seconds of a member's thinking — which is not a feed, it is
    /// a buffer. So consecutive chunks fold into the line they belong to: one
    /// line per run of prose, holding its opening words. The line's `at` moves
    /// with the last chunk, because the feed is ordered by when a thing last
    /// happened and a member that is still talking is still on that line.
    ///
    /// Once the line is full the rest of the run is dropped rather than started
    /// again. The transcript has the whole of it, and a feed that turned one long
    /// answer into eight lines would push out the tool calls the pane exists to
    /// show.
    fn say(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let at = super::now();
        let tokens = self.tokens.total();
        if let Some(last) = self.items.back_mut() {
            if last.kind == ActivityKind::Message {
                let room = MESSAGE_KEPT.saturating_sub(last.title.chars().count());
                if room > 0 {
                    last.title.extend(text.chars().take(room));
                }
                last.at = at;
                last.tokens = tokens;
                return;
            }
        }
        let title: String = text.trim_start().chars().take(MESSAGE_KEPT).collect();
        self.push(ActivityItem {
            at,
            kind: ActivityKind::Message,
            title,
            status: "said".to_string(),
            id: None,
            tokens,
        });
    }

    /// The tool call running right now, if any.
    ///
    /// Derived from the ring rather than tracked beside it. A second list of open
    /// calls would be a second thing to keep in step with the first, and the ring
    /// already holds every announcement and every status — so the answer is a
    /// scan, and a scan cannot go stale.
    ///
    /// Newest first, so a member running two calls in parallel shows the one it
    /// started last. The `judged` set is what makes a closed call stay closed: an
    /// announcement for `c1` followed by a `completed` for `c1` must not leave the
    /// announcement looking open just because it is the older line.
    fn current(&self) -> Option<ActivityItem> {
        let mut judged: BTreeSet<&str> = BTreeSet::new();
        for item in self.items.iter().rev() {
            let Some(id) = item.id.as_deref() else {
                continue;
            };
            if !matches!(
                item.kind,
                ActivityKind::ToolCall | ActivityKind::ToolUpdate
            ) {
                continue;
            }
            if !judged.insert(id) {
                continue;
            }
            if !closed(&item.status) {
                return Some(item.clone());
            }
        }
        None
    }

    /// The feed, newest first.
    fn recent(&self) -> Vec<ActivityItem> {
        self.items.iter().rev().cloned().collect()
    }

    /// Take a reading of what the member has spent, for the rate series.
    ///
    /// Called from the usage event rather than from every line, because the
    /// usage event is the moment the number moved. A reading taken on a tool
    /// call that spent nothing would be a point with a zero rate, and forty of
    /// those would flatten the sparkline into a line that says nothing.
    fn sample(&mut self) {
        if self.series.len() >= RATE_KEPT {
            self.series.pop_front();
        }
        self.series.push_back(Reading {
            at: super::now(),
            tokens: self.tokens.total(),
        });
    }

    /// Drop the series, because the turn it described is over.
    fn clear_series(&mut self) {
        self.series.clear();
    }

    /// The series, oldest first.
    fn readings(&self) -> Vec<Reading> {
        self.series.iter().cloned().collect()
    }
}

/// What a member's stream says about its liveness, as the engine reads it.
///
/// Two facts, because one is not enough to tell a hang from work. `quiet` alone
/// would fail a worker that is eight minutes into a test suite; `tool` alone would
/// excuse a model call that never answered because the last thing it did was open
/// a tool. Together they answer the only question the engine has: is anything
/// expected to arrive, and has it been too long.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pulse {
    /// Seconds since the member last said anything. `None` means no reading —
    /// a member with no watch, which is a member that never started.
    pub quiet: Option<i64>,
    /// A tool call is running, so silence is the tool's and not the model's.
    pub tool: bool,
}

/// A sink that records what a member loaded and whether it is still talking, then
/// forwards everything.
pub struct MemberWatch {
    inner: Arc<dyn EventSink>,
    loaded: Arc<Mutex<Vec<String>>>,
    pulse: Arc<Mutex<Clock>>,
    /// What each announced tool call is, by id.
    ///
    /// The permission request for a call carries only its `toolCallId`, so this
    /// is the only place the call's name exists. Bounded: a long turn announces
    /// many calls and only the ones still open can be asked about.
    titles: Arc<Mutex<HashMap<String, String>>>,
    /// How many requests this member is blocked on.
    ///
    /// A count rather than a flag, because a member can have more than one
    /// outstanding — a permission and an elicitation can overlap — and the first
    /// answer must not clear a wait the second request still holds.
    ///
    /// Here rather than on the run because the gate is the only object that sees
    /// both ends of a request, and the drive loop is the only object that saves
    /// the run. The watch is the seam between them: the gate counts, the loop
    /// copies the count onto the roster on its next tick.
    blocked: Arc<Mutex<usize>>,
    /// What this member has been doing, and what it had spent by then.
    ///
    /// Here rather than on the run for the same reason the pulse is: the watch is
    /// the only object that sees every notification, and the drive loop is the
    /// only object that saves the run. The snapshot reads this directly, so the
    /// feed never has to survive a save to reach the panel.
    feed: Arc<Mutex<Feed>>,
}

impl MemberWatch {
    pub fn new(inner: Arc<dyn EventSink>) -> Self {
        Self {
            inner,
            loaded: Arc::new(Mutex::new(Vec::new())),
            pulse: Arc::new(Mutex::new(Clock::default())),
            titles: Arc::new(Mutex::new(HashMap::new())),
            blocked: Arc::new(Mutex::new(0)),
            feed: Arc::new(Mutex::new(Feed::default())),
        }
    }

    /// What the member announced it was calling, by tool call id.
    ///
    /// `None` when the call was never announced, which is the case the gate has
    /// to keep handling: an unannounced request is as undescribed as it was
    /// before this existed.
    pub fn title_of(&self, tool_call_id: &str) -> Option<String> {
        self.titles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool_call_id)
            .cloned()
    }

    /// The skills this member has loaded so far, in the order it loaded them.
    pub fn loaded(&self) -> Vec<String> {
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Note that a turn is being asked for.
    ///
    /// Called by the engine immediately before it sends a prompt, so the quiet
    /// clock starts at the prompt rather than at whatever the member last said.
    pub fn turn_started(&self) {
        let mut pulse = self.pulse.lock().unwrap_or_else(|e| e.into_inner());
        pulse.turn = Some(Instant::now());
        pulse.turn_at = Some(super::now());
        // A tool left open by a turn that was cancelled or restarted is not this
        // turn's tool. Clearing here is what stops a dead turn's open call from
        // excusing the next turn's silence forever.
        pulse.open.clear();
        drop(pulse);
        // The rate series is the turn's, so a new turn starts a new one. Without
        // this a member's second turn would be drawn against its first turn's
        // readings, and the rate across the join would be a spike that never
        // happened.
        self.feed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear_series();
    }

    /// Note that the turn is over.
    ///
    /// Only the panel's clock stops. `turn` stays where it is, because the quiet
    /// clock measures silence from the turn's start and a member that ended a
    /// turn without saying anything is still silent from then.
    pub fn turn_ended(&self) {
        self.pulse
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .turn_at = None;
        // A member between turns is spending nothing, and its card has to say
        // so. Keeping the finished turn's readings would draw a busy sparkline
        // for a member that has been parked for ten minutes.
        self.feed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear_series();
    }

    /// When the current turn was asked for, in wall clock. `None` between turns.
    pub fn turn_at(&self) -> Option<String> {
        self.pulse
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .turn_at
            .clone()
    }

    /// Note that the member is blocked on a request it has not been answered.
    pub fn request_opened(&self) {
        let mut blocked = self.blocked.lock().unwrap_or_else(|e| e.into_inner());
        *blocked = blocked.saturating_add(1);
    }

    /// Note that one of the member's requests has been answered.
    pub fn request_answered(&self) {
        let mut blocked = self.blocked.lock().unwrap_or_else(|e| e.into_inner());
        *blocked = blocked.saturating_sub(1);
        drop(blocked);

        // The feed's line for the request stops saying `pending` at the same
        // moment the roster stops saying `waiting`. With two requests open the
        // pairing is a guess — the gate answers whichever one it was asked
        // about, and this only knows that one of them is done — but a line that
        // says `pending` for the rest of the run is a worse lie than an answer
        // attributed to the wrong request, and the count on the roster is the
        // authoritative number either way.
        let mut feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(item) = feed
            .items
            .iter_mut()
            .find(|item| item.kind == ActivityKind::Permission && item.status == "pending")
        {
            item.status = "answered".to_string();
        }
    }

    /// Whether the member is blocked on a request right now.
    pub fn is_blocked(&self) -> bool {
        *self.blocked.lock().unwrap_or_else(|e| e.into_inner()) > 0
    }

    /// How long the member has been quiet, in seconds.
    ///
    /// `None` means it has never spoken and no turn has been asked for — a member
    /// that has not started, which is a different failure and not one a clock can
    /// diagnose.
    pub fn quiet_for(&self) -> Option<i64> {
        let pulse = self.pulse.lock().unwrap_or_else(|e| e.into_inner());
        pulse
            .quiet_since()
            .map(|since| Instant::now().duration_since(since).as_secs() as i64)
    }

    /// Whether a tool call is running right now.
    ///
    /// Silence with a tool in flight is the tool's silence. A worker running a
    /// test suite for eight minutes is working, and failing it for being quiet
    /// would punish the swarm for doing the thing it was asked to do.
    pub fn tool_in_flight(&self) -> bool {
        !self
            .pulse
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open
            .is_empty()
    }

    /// Both liveness facts at once, for the engine's wedge check.
    pub fn pulse(&self) -> Pulse {
        Pulse {
            quiet: self.quiet_for(),
            tool: self.tool_in_flight(),
        }
    }

    /// What this member is doing, and — when asked — the last two hundred things
    /// it did.
    ///
    /// `recent` is opt-in because the panel polls every two seconds. `current` is
    /// one item and every member gets it: the roster's question is "what is each
    /// of them doing", and a state word cannot answer it. The feed is two hundred
    /// items and only the selected member gets it, because a twenty-one member
    /// run would otherwise carry four thousand items per poll for a pane that can
    /// only ever show one member.
    pub fn activity(&self, with_recent: bool) -> Activity {
        let feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
        Activity {
            current: feed.current(),
            recent: if with_recent {
                feed.recent()
            } else {
                Vec::new()
            },
            series: feed.readings(),
        }
    }

    /// Record a notification, if it is one.
    ///
    /// Public so the parse can be exercised without a bridge: the shape is the
    /// app server's, and a test that has to stand up a process to check it would
    /// not be run.
    pub fn observe(&self, event: &str, payload: &Value) {
        // The gate forwards every event here before it handles it, so the watch
        // sees the two that are not notifications as well. Both are needed: the
        // usage reading is the only source for what a line cost, and the request
        // is the only source for a member that is blocked rather than working.
        if event.starts_with(EVENT_USAGE) {
            self.observe_usage(payload);
            return;
        }
        if event.starts_with(EVENT_REQUEST) {
            self.observe_request(payload);
            return;
        }
        if !event.starts_with("acp://notification") {
            return;
        }
        if payload.get("method").and_then(Value::as_str) != Some("session/update") {
            return;
        }
        let Some(update) = payload.get("params").and_then(|p| p.get("update")) else {
            return;
        };
        let kind = update.get("sessionUpdate").and_then(Value::as_str);

        // Anything at all is a sign of life, including a chunk of a thought. The
        // clock is the whole point of this half of the watch, so it moves before
        // any of the narrower questions below decide to return early.
        {
            let mut pulse = self.pulse.lock().unwrap_or_else(|e| e.into_inner());
            pulse.seen = Some(Instant::now());
            match kind {
                Some("tool_call") => {
                    if let Some(id) = update.get("toolCallId").and_then(Value::as_str) {
                        // A call announced already finished (a fast read) never
                        // opens, so it cannot excuse a later silence.
                        if !finished(update) {
                            pulse.open.insert(id.to_string());
                        }
                    }
                }
                Some("tool_call_update") => {
                    if let Some(id) = update.get("toolCallId").and_then(Value::as_str) {
                        if finished(update) {
                            pulse.open.remove(id);
                        }
                    }
                }
                _ => {}
            }
        }

        // What the call is, kept by id.
        //
        // The permission request for a call carries only its `toolCallId`, so
        // this announcement is the only place the call's name exists. Recorded
        // for every announcement, not only MCP ones: the gate decides what to do
        // with a name, and this half only has to remember it.
        if kind == Some("tool_call") {
            if let (Some(id), Some(title)) = (
                update.get("toolCallId").and_then(Value::as_str),
                update.get("title").and_then(Value::as_str),
            ) {
                let mut titles = self.titles.lock().unwrap_or_else(|e| e.into_inner());
                // A turn announces a bounded number of calls; the cap keeps a
                // long run from growing this without limit. Oldest out first,
                // because a call that has not been asked about in this many
                // announcements is not the one being asked about now.
                if titles.len() >= TITLES_KEPT && !titles.contains_key(id) {
                    if let Some(oldest) = titles.keys().next().cloned() {
                        titles.remove(&oldest);
                    }
                }
                titles.insert(id.to_string(), title.to_string());
            }
        }

        // The feed. Recorded before the skill parse returns early, because a
        // member's activity is not only its loads: the pane exists to show the
        // reads, the suites and the prose as well.
        self.record_activity(kind, update);

        // Only the announcement. `tool_call_update` carries the result and would
        // count the same load twice.
        if kind != Some("tool_call") {
            return;
        }
        let names = skills::loads_from_tool_call(update);
        if names.is_empty() {
            return;
        }
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        for name in names {
            if !loaded.contains(&name) {
                loaded.push(name);
            }
        }
    }

    /// Fold a usage reading into the feed's own token counter.
    ///
    /// The gate folds the same reading into the member's ledger, and both use
    /// [`Usage::observe`], so the two cannot disagree about what a reading means.
    /// They answer different questions: the ledger says what the member has
    /// spent, the feed says how fast it is spending it.
    fn observe_usage(&self, payload: &Value) {
        let count = |key: &str| payload.get(key).and_then(Value::as_u64).unwrap_or(0);
        let mut feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
        feed.tokens
            .observe(count("input"), count("output"), count("cached"));
        // The reading is taken here rather than on every line, because this is
        // the moment the number moved. It is also the only moment a member that
        // is thinking rather than calling tools produces anything at all, so a
        // series sampled from the feed's lines would be empty for exactly the
        // member whose silence the sparkline exists to explain.
        feed.sample();
    }

    /// Record a request the gate is about to hold open for this member.
    ///
    /// A member blocked on a permission is not working and not idle, and the
    /// roster says `waiting` for it. The feed says what it is waiting on, which
    /// is the part the state word cannot: a request for a command outside the
    /// worktree reads very differently from a request for a form.
    fn observe_request(&self, payload: &Value) {
        let method = payload.get("method").and_then(Value::as_str).unwrap_or("");
        if method != "session/request_permission" && method != "elicitation/create" {
            return;
        }
        let id = payload
            .pointer("/params/toolCall/toolCallId")
            .and_then(Value::as_str)
            .map(str::to_string);
        // The request names only a tool call id. The announcement that carried
        // the name is the only place it exists, which is the same reason the gate
        // asks the watch rather than reading the request.
        let title = id
            .as_deref()
            .and_then(|id| self.title_of(id))
            .unwrap_or_else(|| method.to_string());
        let mut feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
        feed.push(ActivityItem {
            at: super::now(),
            kind: ActivityKind::Permission,
            title,
            status: "pending".to_string(),
            id,
            tokens: 0,
        });
    }

    /// Record one line of the feed from a `session/update`.
    fn record_activity(&self, kind: Option<&str>, update: &Value) {
        let id = update
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_string);
        match kind {
            Some("tool_call") | Some("tool_call_update") => {
                // An update often carries only a status, so the name comes from
                // the announcement. Without that fallback a finished call would
                // read as `a tool call` and the pane would lose the one word the
                // user is looking for.
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| id.as_deref().and_then(|id| self.title_of(id)))
                    .unwrap_or_else(|| "a tool call".to_string());
                let kind = if kind == Some("tool_call") {
                    ActivityKind::ToolCall
                } else {
                    ActivityKind::ToolUpdate
                };
                let status = update
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("in-progress")
                    .to_string();
                let mut feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
                feed.push(ActivityItem {
                    at: super::now(),
                    kind,
                    title,
                    status,
                    id,
                    tokens: 0,
                });
            }
            Some("agent_message_chunk") => {
                let text = update
                    .pointer("/content/text")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let mut feed = self.feed.lock().unwrap_or_else(|e| e.into_inner());
                feed.say(text);
            }
            _ => {}
        }
    }
}

/// Whether a tool call update says the call is over.
///
/// `pending` and `in_progress` are the open states; `completed` and `failed` are
/// the closed ones. A missing status is treated as open, because the announcement
/// of a call is the start of it and the result arrives in a later update.
fn finished(update: &Value) -> bool {
    update
        .get("status")
        .and_then(Value::as_str)
        .map(closed)
        .unwrap_or(false)
}

/// Whether a status word means the call it belongs to is over.
///
/// One rule, two readers: the liveness clock asks it of a raw update, and the
/// feed asks it of a line it has already recorded. Two spellings of the same
/// answer would let the roster say a tool is running while the pane says it
/// finished.
fn closed(status: &str) -> bool {
    matches!(status, "completed" | "failed")
}

impl EventSink for MemberWatch {
    fn emit(&self, event: &str, payload: Value) {
        self.observe(event, &payload);
        self.inner.emit(event, payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::bridge::RecordingSink;

    fn watch() -> (Arc<MemberWatch>, Arc<RecordingSink>) {
        let recorder = Arc::new(RecordingSink::default());
        let watch = Arc::new(MemberWatch::new(recorder.clone()));
        (watch, recorder)
    }

    fn tool_call(title: &str, raw: Value) -> Value {
        serde_json::json!({
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "c1",
                    "title": title,
                    "rawInput": raw,
                }
            }
        })
    }

    fn tool_update(id: &str, status: &str) -> Value {
        serde_json::json!({
            "method": "session/update",
            "params": { "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "status": status,
            }}
        })
    }

    fn chunk() -> Value {
        serde_json::json!({
            "method": "session/update",
            "params": { "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "working" },
            }}
        })
    }

    fn chunk_of(text: &str) -> Value {
        serde_json::json!({
            "method": "session/update",
            "params": { "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text },
            }}
        })
    }

    /// A tool call with its own id, so a test can announce more than one.
    fn call(id: &str, title: &str) -> Value {
        serde_json::json!({
            "method": "session/update",
            "params": { "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": id,
                "title": title,
            }}
        })
    }

    /// A usage reading, in the shape the bridge publishes.
    fn usage(input: u64, output: u64) -> Value {
        serde_json::json!({
            "final": false,
            "gauge": true,
            "input": input,
            "output": output,
            "cached": 0,
            "contextUsed": 0,
            "contextSize": 0,
            "secs": 0.0,
        })
    }

    /// A permission request, in the shape the bridge publishes.
    fn permission(tool_call_id: &str) -> Value {
        serde_json::json!({
            "requestId": "perm-1",
            "method": "session/request_permission",
            "params": { "toolCall": { "toolCallId": tool_call_id } },
        })
    }

    const NOTIFY: &str = "acp://notification/swarm/w-t1";
    const USAGE: &str = "acp://usage/swarm/w-t1";
    const REQUEST: &str = "acp://request/swarm/w-t1";

    #[test]
    fn a_skill_tool_call_is_recorded() {
        let (watch, _) = watch();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("skill", serde_json::json!({ "name": "assay" })),
        );
        assert_eq!(watch.loaded(), vec!["assay".to_string()]);
    }

    #[test]
    fn an_announced_tool_call_is_remembered_by_id() {
        // The permission request for a call carries only its id, so this is the
        // only place the call's name exists. Without it the gate cannot tell an
        // MCP call from a request that describes nothing.
        let (watch, _) = watch();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("mcp_codebase_memory.list_projects", serde_json::json!({})),
        );
        assert_eq!(
            watch.title_of("c1").as_deref(),
            Some("mcp_codebase_memory.list_projects")
        );
        assert_eq!(watch.title_of("never-announced"), None);
    }

    #[test]
    fn a_result_update_does_not_erase_the_announced_title() {
        // The request can arrive after the call has been announced and before it
        // has finished; a later update must not take the name away.
        let (watch, _) = watch();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("mcp_donsetch.web_search", serde_json::json!({})),
        );
        watch.emit("acp://notification/swarm/w-t1", tool_update("c1", "completed"));
        assert_eq!(watch.title_of("c1").as_deref(), Some("mcp_donsetch.web_search"));
    }

    #[test]
    fn the_same_skill_loaded_twice_is_recorded_once() {
        let (watch, _) = watch();
        for _ in 0..3 {
            watch.emit(
                "acp://notification/swarm/w-t1",
                tool_call("skill", serde_json::json!({ "name": "assay" })),
            );
        }
        assert_eq!(watch.loaded(), vec!["assay".to_string()]);
    }

    #[test]
    fn a_tool_call_update_does_not_double_count() {
        let (watch, _) = watch();
        watch.emit(
            "acp://notification/swarm/w-t1",
            serde_json::json!({
                "method": "session/update",
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "c1",
                    "title": "Loading skill: assay",
                }}
            }),
        );
        assert!(watch.loaded().is_empty());
    }

    #[test]
    fn everything_is_forwarded_untouched() {
        let (watch, recorder) = watch();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("skill", serde_json::json!({ "name": "assay" })),
        );
        assert_eq!(recorder.on("acp://notification/swarm/w-t1").len(), 1);
    }

    #[test]
    fn a_non_notification_event_is_forwarded_and_ignored() {
        let (watch, recorder) = watch();
        watch.emit("agent://status", serde_json::json!({ "state": "busy" }));
        assert!(watch.loaded().is_empty());
        assert_eq!(recorder.on("agent://status").len(), 1);
    }

    #[test]
    fn a_read_call_is_not_a_load() {
        let (watch, _) = watch();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("read", serde_json::json!({ "name": "src/main.rs" })),
        );
        assert!(watch.loaded().is_empty());
    }

    #[test]
    fn a_member_that_has_not_started_has_no_quiet_clock() {
        let (watch, _) = watch();
        assert_eq!(watch.quiet_for(), None);
    }

    #[test]
    fn a_turn_that_has_said_nothing_is_quiet_from_its_own_start() {
        let (watch, _) = watch();
        // A previous turn spoke, then ended.
        watch.emit("acp://notification/swarm/w-t1", chunk());
        // The next turn begins. The clock restarts here, not at the last word.
        watch.turn_started();
        assert_eq!(watch.quiet_for(), Some(0));
    }

    #[test]
    fn a_chunk_moves_the_quiet_clock() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit("acp://notification/swarm/w-t1", chunk());
        assert_eq!(watch.quiet_for(), Some(0));
    }

    #[test]
    fn a_tool_that_is_running_is_in_flight() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("bash", serde_json::json!({ "command": "npm test" })),
        );
        assert!(watch.tool_in_flight());
    }

    #[test]
    fn a_finished_tool_is_not_in_flight() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("bash", serde_json::json!({ "command": "npm test" })),
        );
        watch.emit("acp://notification/swarm/w-t1", tool_update("c1", "completed"));
        assert!(!watch.tool_in_flight());
    }

    #[test]
    fn a_failed_tool_is_not_in_flight() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("bash", serde_json::json!({ "command": "npm test" })),
        );
        watch.emit("acp://notification/swarm/w-t1", tool_update("c1", "failed"));
        assert!(!watch.tool_in_flight());
    }

    #[test]
    fn a_tool_announced_already_finished_never_opens() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit(
            "acp://notification/swarm/w-t1",
            serde_json::json!({
                "method": "session/update",
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "c9",
                    "title": "read",
                    "status": "completed",
                }}
            }),
        );
        assert!(!watch.tool_in_flight());
    }

    #[test]
    fn a_new_turn_does_not_inherit_the_last_turns_open_tool() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit(
            "acp://notification/swarm/w-t1",
            tool_call("bash", serde_json::json!({ "command": "npm test" })),
        );
        // The turn was cancelled with the tool still open. The next turn must not
        // be excused by it.
        watch.turn_started();
        assert!(!watch.tool_in_flight());
    }

    /* -------------------------------------------------------------- *
     * The turn clock the panel reads
     * -------------------------------------------------------------- */

    #[test]
    fn a_turn_records_when_it_started_and_forgets_it_when_it_ends() {
        // The panel shows a live elapsed time for a working member, and the only
        // honest source is the moment the turn began. A clock that is not
        // cleared goes on reporting a turn that is over.
        let (watch, _) = watch();
        assert_eq!(watch.turn_at(), None, "no turn has been asked for yet");
        watch.turn_started();
        let started = watch.turn_at().expect("a turn in flight has a start");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&started).is_ok(),
            "the panel subtracts this from its own clock, so it must be a time: {started}"
        );
        watch.turn_ended();
        assert_eq!(watch.turn_at(), None);
    }

    #[test]
    fn ending_a_turn_does_not_move_the_quiet_clock() {
        // The quiet clock measures silence from the turn's start, so it keeps its
        // anchor. Clearing it would make a member that ended a turn and said
        // nothing since look silent from the beginning of time.
        let (watch, _) = watch();
        watch.turn_started();
        watch.turn_ended();
        assert_eq!(watch.quiet_for(), Some(0));
    }

    /* -------------------------------------------------------------- *
     * What the member is blocked on
     * -------------------------------------------------------------- */

    #[test]
    fn a_request_blocks_the_member_until_it_is_answered() {
        let (watch, _) = watch();
        assert!(!watch.is_blocked());
        watch.request_opened();
        assert!(watch.is_blocked());
        watch.request_answered();
        assert!(!watch.is_blocked());
    }

    #[test]
    fn two_outstanding_requests_need_two_answers() {
        // A permission and an elicitation can overlap. A flag would let the first
        // answer clear a wait the second request still holds.
        let (watch, _) = watch();
        watch.request_opened();
        watch.request_opened();
        watch.request_answered();
        assert!(watch.is_blocked(), "one request is still outstanding");
        watch.request_answered();
        assert!(!watch.is_blocked());
    }

    #[test]
    fn an_answer_with_no_request_does_not_go_negative() {
        // A request that timed out on the agent's side can be answered twice.
        let (watch, _) = watch();
        watch.request_answered();
        watch.request_answered();
        assert!(!watch.is_blocked());
        watch.request_opened();
        assert!(watch.is_blocked(), "the count is still usable afterwards");
    }

    /* -------------------------------------------------------------- *
     * The feed
     * -------------------------------------------------------------- */

    #[test]
    fn a_tool_call_is_what_the_member_is_doing() {
        // The whole point of the phase: a state word says `working`, this says
        // `npm test`. Without it the panel can only report that something is
        // happening, which is what it did for thirteen minutes of a live run.
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "bash"));
        let activity = watch.activity(false);
        let current = activity.current.expect("a running call is the current one");
        assert_eq!(current.title, "bash");
        assert_eq!(current.status, "in-progress");
        assert_eq!(current.kind, ActivityKind::ToolCall);
        assert_eq!(current.id.as_deref(), Some("c1"));
    }

    #[test]
    fn a_finished_tool_is_no_longer_current_and_says_completed() {
        // The proof the phase asks for: a finished tool shows `completed`. The
        // announcement must not go on looking open just because it is the older
        // line — that is the bug a naive "newest open item" scan has.
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "bash"));
        watch.emit(NOTIFY, tool_update("c1", "completed"));
        assert_eq!(watch.activity(false).current, None);
        let feed = watch.activity(true).recent;
        assert_eq!(feed[0].status, "completed", "newest first");
        assert_eq!(feed[0].title, "bash", "the update carries no title");
        assert_eq!(feed[1].status, "in-progress");
    }

    #[test]
    fn a_failed_tool_is_closed_too() {
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "bash"));
        watch.emit(NOTIFY, tool_update("c1", "failed"));
        assert_eq!(watch.activity(false).current, None);
        assert_eq!(watch.activity(true).recent[0].status, "failed");
    }

    #[test]
    fn two_calls_in_flight_report_the_one_started_last() {
        // A member can run two tools at once. The pane shows one, and the honest
        // one is the most recent: it is the thing the member turned to.
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "read"));
        watch.emit(NOTIFY, call("c2", "bash"));
        assert_eq!(watch.activity(false).current.unwrap().title, "bash");
        // Finishing the newer one falls back to the older, which is still open.
        watch.emit(NOTIFY, tool_update("c2", "completed"));
        assert_eq!(watch.activity(false).current.unwrap().title, "read");
    }

    #[test]
    fn the_feed_is_bounded_and_drops_the_oldest() {
        // Two hundred, like the board tail. A run that announces a thousand calls
        // must not grow this without limit, and the lines that survive must be
        // the recent ones — a feed that kept the first two hundred would show a
        // member's first minute forever.
        let (watch, _) = watch();
        for i in 0..(ACTIVITY_KEPT + 50) {
            watch.emit(NOTIFY, call(&format!("c{i}"), &format!("tool-{i}")));
        }
        let feed = watch.activity(true).recent;
        assert_eq!(feed.len(), ACTIVITY_KEPT);
        assert_eq!(feed[0].title, format!("tool-{}", ACTIVITY_KEPT + 49));
        assert_eq!(feed[ACTIVITY_KEPT - 1].title, "tool-50");
    }

    #[test]
    fn the_feed_is_newest_first() {
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "first"));
        watch.emit(NOTIFY, call("c2", "second"));
        let feed = watch.activity(true).recent;
        assert_eq!(feed[0].title, "second");
        assert_eq!(feed[1].title, "first");
    }

    #[test]
    fn a_feed_is_only_sent_when_it_is_asked_for() {
        // The bound the snapshot depends on: `current` for every member, `recent`
        // for the selected one. A watch that always returned the ring would put
        // four thousand items on the wire every two seconds.
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "bash"));
        assert!(watch.activity(false).recent.is_empty());
        assert!(watch.activity(false).current.is_some());
        assert_eq!(watch.activity(true).recent.len(), 1);
    }

    #[test]
    fn a_run_of_prose_is_one_line_not_one_per_chunk() {
        // A chunk is a token or two. One line per chunk would fill the ring with
        // twenty seconds of thinking and push out everything worth reading.
        let (watch, _) = watch();
        for text in ["The ", "suite ", "fails ", "on ", "t1."] {
            watch.emit(NOTIFY, chunk_of(text));
        }
        let feed = watch.activity(true).recent;
        assert_eq!(feed.len(), 1, "five chunks are one thing said");
        assert_eq!(feed[0].kind, ActivityKind::Message);
        assert_eq!(feed[0].title, "The suite fails on t1.");
    }

    #[test]
    fn prose_after_a_tool_call_starts_a_new_line() {
        // The fold is into the line the chunks belong to, not into whatever is
        // newest: a member that says something, runs a tool and says something
        // else said two things.
        let (watch, _) = watch();
        watch.emit(NOTIFY, chunk_of("reading the file"));
        watch.emit(NOTIFY, call("c1", "read"));
        watch.emit(NOTIFY, chunk_of("it is wrong"));
        let feed = watch.activity(true).recent;
        assert_eq!(feed.len(), 3);
        assert_eq!(feed[0].title, "it is wrong");
        assert_eq!(feed[1].title, "read");
        assert_eq!(feed[2].title, "reading the file");
    }

    #[test]
    fn a_long_message_is_cut_rather_than_kept_whole() {
        // The transcript is the place for the whole answer. This is the place for
        // enough of it to recognise which answer it was — and one line, not
        // eight, because eight would push the tool calls off the ring.
        let (watch, _) = watch();
        for _ in 0..40 {
            watch.emit(NOTIFY, chunk_of(&"x".repeat(20)));
        }
        let feed = watch.activity(true).recent;
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].title.chars().count(), MESSAGE_KEPT);
    }

    #[test]
    fn every_line_carries_the_token_total_at_the_moment_it_landed() {
        // A reading per line, not a total: the difference between two lines over
        // the time between them is a rate, and a rate is what says whether a
        // member is doing anything.
        let (watch, _) = watch();
        watch.emit(USAGE, usage(1_000, 100));
        watch.emit(NOTIFY, call("c1", "read"));
        watch.emit(USAGE, usage(3_000, 400));
        watch.emit(NOTIFY, call("c2", "bash"));
        let feed = watch.activity(true).recent;
        assert_eq!(feed[1].tokens, 1_100, "the first line saw the first reading");
        assert_eq!(feed[0].tokens, 3_400, "the second saw both");
    }

    #[test]
    fn a_usage_reading_is_a_delta_not_a_sum() {
        // The counters are session-cumulative, so folding the same reading twice
        // must add nothing. A sum would report a member that has spent four
        // thousand tokens after two readings of two thousand.
        let (watch, _) = watch();
        watch.emit(USAGE, usage(2_000, 0));
        watch.emit(USAGE, usage(2_000, 0));
        watch.emit(NOTIFY, call("c1", "read"));
        assert_eq!(watch.activity(true).recent[0].tokens, 2_000);
    }

    /// The rate series is the turn's, and a member between turns has none.
    ///
    /// This is what makes a flat sparkline mean "not spending" rather than
    /// "spending nothing lately". A parked member has no turn, so it has no
    /// readings, so its card draws a flat line — and a member that kept its last
    /// turn's readings would draw a busy one for ten minutes after it stopped.
    #[test]
    fn the_rate_series_is_the_current_turn_and_nothing_else() {
        let (watch, _) = watch();
        assert!(
            watch.activity(false).series.is_empty(),
            "a member that has not started has no readings"
        );

        watch.turn_started();
        watch.emit(USAGE, usage(1_000, 100));
        watch.emit(USAGE, usage(2_000, 200));
        let series = watch.activity(false).series;
        assert_eq!(series.len(), 2, "one reading per usage event");
        assert_eq!(series[0].tokens, 1_100, "oldest first");
        assert_eq!(series[1].tokens, 2_200);

        // The turn ends. The member is spending nothing, and the card has to say
        // so rather than replay the turn that just finished.
        watch.turn_ended();
        assert!(
            watch.activity(false).series.is_empty(),
            "a parked member has no rate to draw"
        );

        // A second turn starts a new series rather than joining the first: the
        // rate across the join would be a spike that never happened.
        watch.turn_started();
        watch.emit(USAGE, usage(3_000, 300));
        assert_eq!(watch.activity(false).series.len(), 1);
    }

    /// The series is bounded at the sparkline's width and drops the oldest.
    #[test]
    fn the_rate_series_is_bounded_and_drops_the_oldest() {
        let (watch, _) = watch();
        watch.turn_started();
        for index in 1..=(RATE_KEPT + 5) {
            watch.emit(USAGE, usage(index as u64 * 100, 0));
        }
        let series = watch.activity(false).series;
        assert_eq!(series.len(), RATE_KEPT);
        assert_eq!(
            series[0].tokens,
            600,
            "the oldest went, so the series starts at the sixth reading"
        );
        assert_eq!(series[RATE_KEPT - 1].tokens, (RATE_KEPT as u64 + 5) * 100);
    }

    /// The series is sent for every member, not only the selected one.
    ///
    /// `recent` is scoped because it is two hundred items; the series is forty
    /// numbers and it is the roster's question — twenty-one cards, each asking
    /// whether its member is doing anything.
    #[test]
    fn the_rate_series_is_sent_whether_or_not_the_member_is_selected() {
        let (watch, _) = watch();
        watch.turn_started();
        watch.emit(USAGE, usage(1_000, 0));
        watch.emit(USAGE, usage(2_000, 0));
        assert_eq!(watch.activity(false).series.len(), 2);
        assert!(watch.activity(false).recent.is_empty());
        assert_eq!(watch.activity(true).series.len(), 2);
    }

    #[test]
    fn a_permission_request_is_a_line_and_stops_saying_pending_when_answered() {
        // A member blocked on a permission is not working and not idle. The
        // roster says `waiting`; the feed says what it is waiting on, and stops
        // saying `pending` the moment the gate answers — a line that stayed
        // pending for the rest of the run would read as a hang.
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "bash"));
        watch.emit(REQUEST, permission("c1"));
        let feed = watch.activity(true).recent;
        assert_eq!(feed[0].kind, ActivityKind::Permission);
        assert_eq!(feed[0].title, "bash", "the request names only the id");
        assert_eq!(feed[0].status, "pending");

        watch.request_opened();
        watch.request_answered();
        assert_eq!(watch.activity(true).recent[0].status, "answered");
    }

    #[test]
    fn a_request_the_gate_does_not_answer_is_not_a_line() {
        // The gate holds two methods open. A request it refuses outright is not a
        // wait, and a feed line for it would claim the member was blocked.
        let (watch, _) = watch();
        watch.emit(
            REQUEST,
            serde_json::json!({ "requestId": "r1", "method": "session/something_else" }),
        );
        assert!(watch.activity(true).recent.is_empty());
    }

    #[test]
    fn a_turn_does_not_clear_the_feed() {
        // The feed is the member's history, not the turn's. A worker on its third
        // turn should still show the two before it.
        let (watch, _) = watch();
        watch.emit(NOTIFY, call("c1", "read"));
        watch.turn_started();
        assert_eq!(watch.activity(true).recent.len(), 1);
    }
}
