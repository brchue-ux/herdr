//! The last few things herdr said about itself, kept rather than shown once.
//!
//! # What this is, and what it deliberately is not
//!
//! herdr's terminal area is a real PTY. The captain's ruling on that is on
//! record and is not reopened here: *do not build the literal three-region
//! architecture — herdr's real PTY session already **is** what the artifact is
//! simulating*. So this is **not** a cap on pane output, not a wrapper around a
//! shell, and not a second place a command's own text is drawn in full.
//!
//! It is herdr's own voice, plus — since the merge documented below — a short
//! record of what the wrapped agent was actually seen doing. Until now that
//! voice was a single transient toast: herdr said one thing, held it for a few
//! seconds, and forgot it — so a burst of three things happening at once showed
//! the reader one of them, and looking away for five seconds meant the sentence
//! was gone. A48 is the correction, and its own finding is the reason it is a
//! *count* and not a percentage:
//!
//! > A17's "30–40% of frame height" was retired because a percentage was never
//! > the quantity — *a terminal has LINES*.
//!
//! So the stream is [`TERM_MAX`] lines. Eight — raised from A48's original six
//! once the feed started also carrying agent activity, not just herdr's own
//! toasts. Not eight lines' worth of height, and not a fraction of the frame:
//! eight lines.
//!
//! # Why it observes rather than being written to, for toasts
//!
//! There are twenty places in this codebase that raise a toast, and a feed that
//! had to be appended to at each of them would be nineteen places to forget. So
//! this watches the one field they all write — `AppState::toast` — and records a
//! line whenever it changes to something new. One hook, on the app loop, beside
//! the machine register's, and every existing caller keeps working untouched.
//!
//! # Where the agent-activity lines come from
//!
//! `crate::detect::command_marker` already finds every `⏺ Bash(...)` bullet a
//! Claude Code pane prints and diffs scan-to-scan so a caller sees only the
//! newly-appeared ones — but historically that signal was thrown away after
//! triggering a sidebar animation (`crate::app::cmd_ack`), never stored. This
//! feed is the durable home for it: when a fresh Bash marker is detected, its
//! command text is pushed here as a new line, the same append-and-cap
//! mechanism a toast uses, via [`Self::record_command`]. Unlike the toast path
//! there is nothing to dedupe against — the detection loop already reports
//! only genuinely new markers — so every call is a new line.
//!
//! # Why it is a runtime fact and not a picture
//!
//! Per this project's runtime/client boundary rule: what herdr has said about
//! this session, and what it observed the wrapped agent do, are **session
//! facts**, not TUI presentation state, so they live here in
//! [`crate::app::state::AppState`] and are published through the session API.
//! Where the stream is drawn, how wide it is, and what it reserves for the
//! machine corner are the client's, and live in `crate::ui`.

use std::collections::VecDeque;
use std::time::Instant;

use crate::app::state::{ToastKind, ToastNotification};

/// How many lines the stream holds.
///
/// **A48's original number was six; eight is the captain's own stated number
/// once agent-command lines joined toasts in the same stream.** Enough to
/// carry a burst — a workspace opening, an agent running two commands and a
/// git refresh landing all inside a second — and short enough that the stream
/// stays a *margin note* rather than becoming a second pane. The card's
/// finding is quoted in this module's own header: a terminal has lines, so
/// the cap is in lines.
pub(crate) const TERM_MAX: usize = 8;

/// What kind of thing produced one [`StatusLine`].
///
/// A superset of [`ToastKind`] rather than a variant folded into it: a
/// command line never goes through toast delivery (no popup, no sound, no
/// position), so giving it its own `ToastKind` variant would mean handling
/// it — or explicitly not handling it — at every one of that enum's ~20
/// existing match sites that are genuinely about toast delivery mechanics.
/// This enum only has to answer "how does this line color itself," which is
/// all [`crate::ui::status::render_status_feed`] and the API's `kind` string
/// need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusLineKind {
    Toast(ToastKind),
    /// A shell command herdr saw a Claude Code pane report running.
    AgentCommand,
}

/// One thing herdr said, or observed the wrapped agent do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusLine {
    pub(crate) kind: StatusLineKind,
    /// The whole line, already joined: `title` and `context` are two halves of
    /// one sentence and the stream has one line per sentence.
    pub(crate) text: String,
    /// When herdr said it. Kept so the stream can dim what is old without
    /// dropping it — the point of a stream is that the last eight are *there*,
    /// even the one from a minute ago.
    pub(crate) at: Instant,
}

/// The last [`TERM_MAX`] lines, newest last.
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusFeed {
    lines: VecDeque<StatusLine>,
    /// The toast this feed has already recorded, so a toast still on screen is
    /// not appended again on every tick.
    recorded: Option<ToastNotification>,
}

impl StatusFeed {
    /// Record whatever herdr is currently saying, if it has not been recorded.
    ///
    /// Returns whether the stream moved, so the caller can decide whether the
    /// frame needs repainting — the same contract
    /// `App::observe_machine_register` follows, and for the same reason: this
    /// runs on the tick loop and a hook that always claimed a change would arm
    /// a repaint forever.
    ///
    /// A toast being *cleared* is not a change to the stream. The line stays:
    /// that is the whole difference between a stream and a toast.
    pub(crate) fn observe(&mut self, toast: Option<&ToastNotification>, now: Instant) -> bool {
        let Some(toast) = toast else {
            return false;
        };
        if self.recorded.as_ref() == Some(toast) {
            return false;
        }
        self.recorded = Some(toast.clone());
        self.push(StatusLine {
            kind: StatusLineKind::Toast(toast.kind),
            text: line_text(toast),
            at: now,
        });
        true
    }

    /// Record one shell command a Claude Code pane was just seen to run.
    ///
    /// `text` is already the whole line — see
    /// `crate::detect::bash_command_text` for pulling the command itself out
    /// of the raw `⏺ Bash(...)` marker, and the call site in
    /// [`crate::app::api::App::handle_internal_event`] for how a pane id
    /// becomes a readable label. There is nothing to dedupe here: the
    /// detection loop this is fed from
    /// (`crate::detect::diff_new_markers`) already reports only markers that
    /// are new since the last scan, so every call is a genuinely new line.
    pub(crate) fn record_command(&mut self, text: String, now: Instant) {
        self.push(StatusLine {
            kind: StatusLineKind::AgentCommand,
            text,
            at: now,
        });
    }

    fn push(&mut self, line: StatusLine) {
        self.lines.push_back(line);
        while self.lines.len() > TERM_MAX {
            self.lines.pop_front();
        }
    }

    /// The stream, oldest first.
    pub(crate) fn lines(&self) -> impl ExactSizeIterator<Item = &StatusLine> {
        self.lines.iter()
    }

    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// One toast as one line.
///
/// The two halves are joined with an en-dash-ish separator rather than being
/// stacked, because the stream's whole budget is eight *lines* and spending
/// two of them on one event would let a single agent state change fill a
/// quarter of it.
fn line_text(toast: &ToastNotification) -> String {
    let title = toast.title.trim();
    let context = toast.context.trim();
    match (title.is_empty(), context.is_empty()) {
        (true, true) => String::new(),
        (false, true) => title.to_string(),
        (true, false) => context.to_string(),
        (false, false) => format!("{title} · {context}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(title: &str) -> ToastNotification {
        ToastNotification {
            kind: ToastKind::Finished,
            title: title.to_string(),
            context: "ctx".to_string(),
            position: None,
            target: None,
        }
    }

    #[test]
    fn the_stream_holds_eight_lines_and_no_more() {
        let mut feed = StatusFeed::default();
        let now = Instant::now();
        for index in 0..20 {
            assert!(feed.observe(Some(&toast(&format!("line {index}"))), now));
        }
        assert_eq!(feed.len(), TERM_MAX);
        assert_eq!(TERM_MAX, 8);
        // The eight it kept are the eight most recent, oldest first.
        let texts: Vec<&str> = feed.lines().map(|line| line.text.as_str()).collect();
        assert_eq!(texts.first().copied(), Some("line 12 · ctx"));
        assert_eq!(texts.last().copied(), Some("line 19 · ctx"));
    }

    #[test]
    fn a_recorded_command_is_a_new_line_every_time() {
        let mut feed = StatusFeed::default();
        let now = Instant::now();
        feed.record_command("git status".to_string(), now);
        feed.record_command("git status".to_string(), now);
        assert_eq!(feed.len(), 2);
        assert!(feed
            .lines()
            .all(|line| line.kind == StatusLineKind::AgentCommand));
    }

    /// The whole point of the merge: toasts and agent commands interleave in
    /// one stream, oldest first, and the eight-line cap and eviction order
    /// apply across both sources together rather than per-source.
    #[test]
    fn toasts_and_commands_interleave_under_one_cap() {
        let mut feed = StatusFeed::default();
        let now = Instant::now();
        feed.observe(Some(&toast("workspace opened")), now);
        feed.record_command("git status".to_string(), now);
        feed.observe(Some(&toast("agent finished")), now);
        feed.record_command("npm test".to_string(), now);
        feed.record_command("npm build".to_string(), now);
        feed.record_command("npm lint".to_string(), now);
        feed.record_command("cargo check".to_string(), now);
        feed.record_command("cargo test".to_string(), now);
        feed.record_command("cargo build".to_string(), now);
        // Nine events pushed, cap is eight: the oldest (the workspace-opened
        // toast) was evicted first.
        assert_eq!(feed.len(), TERM_MAX);
        let texts: Vec<&str> = feed.lines().map(|line| line.text.as_str()).collect();
        assert_eq!(texts[0], "git status");
        assert_eq!(texts.last().copied(), Some("cargo build"));
        let kinds: Vec<StatusLineKind> = feed.lines().map(|line| line.kind).collect();
        assert_eq!(
            kinds[1],
            StatusLineKind::Toast(ToastKind::Finished),
            "the 'agent finished' toast survives interleaved with commands"
        );
    }

    /// A toast holding on screen is one thing herdr said, not one per tick.
    #[test]
    fn the_same_toast_is_recorded_once() {
        let mut feed = StatusFeed::default();
        let now = Instant::now();
        let toast = toast("opened");
        assert!(feed.observe(Some(&toast), now));
        assert!(!feed.observe(Some(&toast), now));
        assert!(!feed.observe(Some(&toast), now));
        assert_eq!(feed.len(), 1);
    }

    /// The difference between a stream and a toast: the toast going away does
    /// not take the line with it.
    #[test]
    fn clearing_the_toast_leaves_the_line_standing() {
        let mut feed = StatusFeed::default();
        let now = Instant::now();
        feed.observe(Some(&toast("opened")), now);
        assert!(!feed.observe(None, now));
        assert_eq!(feed.len(), 1);
    }

    /// The same words said twice really are two events — a second agent
    /// finishing says exactly what the first one said — so an identical toast
    /// arriving after a gap is a second line, not a duplicate suppressed.
    #[test]
    fn the_same_words_after_a_clear_are_a_second_line() {
        let mut feed = StatusFeed::default();
        let now = Instant::now();
        feed.observe(Some(&toast("done")), now);
        feed.observe(Some(&toast("other")), now);
        feed.observe(Some(&toast("done")), now);
        assert_eq!(feed.len(), 3);
    }

    #[test]
    fn a_toast_with_only_one_half_does_not_print_a_separator() {
        let mut bare = toast("opened");
        bare.context = String::new();
        let mut feed = StatusFeed::default();
        feed.observe(Some(&bare), Instant::now());
        assert_eq!(
            feed.lines().next().map(|line| line.text.as_str()),
            Some("opened")
        );
    }
}
