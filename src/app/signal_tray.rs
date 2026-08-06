//! The bottom notification tray: the eight signals as things you can act on.
//!
//! [`crate::app::fleet_signals`] answers *whether* each of the eight is live.
//! This module answers the three questions a slot has to answer before it earns
//! a place in a tray you can click:
//!
//! - **What does it cover?** [`TrayBadge::items`] — the actual panes, checkouts
//!   and pull requests behind the slot, named, so the popup can say `2ndmate-herdr
//!   is waiting on you` rather than `1 agent blocked`.
//! - **Where does it take you?** [`TrayTarget`] — one destination per item, and
//!   a badge covering more than one thing cycles rather than guessing.
//! - **What may a click do?** [`TrayAction`] — and this is a safety boundary,
//!   not a preference. See below.
//!
//! ## The authority boundary
//!
//! A badge click may settle a **routine** choice in place. It may never perform
//! a destructive, irreversible or security-sensitive act without an explicit
//! confirmation that states what will happen first.
//!
//! Four of the eight offer nothing but a jump ([`TrayAction::JumpOnly`] and
//! [`TrayAction::OpenSummaries`]), and that is deliberate rather than
//! unfinished: restarting a worker that died needs a launch command Herdr does
//! not record per pane, and "re-run the failed jobs" is a decision, not a yes.
//! The two that run git ([`FleetSignal::Push`], [`FleetSignal::Sync`]) are
//! [`TrayAction::Confirm`], which means the popup prints the exact command,
//! the branch and the counts before there is a button to press.
//!
//! Nothing here forces, discards or overwrites work. `sync` runs
//! `git pull --rebase`, and it *refuses* on a tree that is not clean — see
//! [`TrayBadge::refusal`] — because rebasing over uncommitted work is the one
//! move in this tray that could destroy something. A checkout whose dirty state
//! has never been read is refused for the same reason: "we do not know" has to
//! fail the same way "it is dirty" does.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::app::fleet_signals::{FleetSignal, FleetSignals};
use crate::app::state::AppState;
use crate::detect::AgentState;
use crate::layout::PaneId;

/// How often the question text behind a blocked pane is re-read.
///
/// The renderer is pure and takes `&AppState`, so the text a blocked agent is
/// showing has to be copied into state before it can be drawn. Reading it costs
/// a lock on the pane's terminal core and a render of its bottom buffer, so it
/// is taken on a clock rather than per frame, and only while something is
/// actually going to draw it.
const QUESTION_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

/// How many lines of a blocked pane's screen are kept as "the question".
///
/// Enough to carry a prompt and its options, few enough that a popup anchored
/// over a sidebar can show all of it. A pane that is blocked on something
/// longer than this is exactly the case the `open pane` escape hatch exists
/// for.
const QUESTION_LINES: usize = 6;

/// How many items one badge will name before it stops counting them out.
const MAX_ITEMS: usize = 32;

/// How a badge is drawn right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum BadgeState {
    /// The condition is not true. The mark is engraved into the tray rather
    /// than faded out: a faded badge looks broken, a carved one looks like a
    /// slot that is simply empty right now.
    #[default]
    Idle,
    /// The condition is true and standing.
    Active,
    /// The condition crossed from *happening* to *finished and waiting on you*.
    Attention,
}

impl BadgeState {
    pub(crate) fn is_live(self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// The catalogue behaviour this state moves on.
    ///
    /// **This mapping is the state language.** The visual target is explicit
    /// that a badge's state has to survive a reader who cannot separate the
    /// hues, so the three states are three *rhythms* first and three colours
    /// second: rest breathes slowly and never travels, active snaps on the
    /// stated curve, and attention snaps on the same curve at better than twice
    /// the rate. All three names are declared on the element's lifecycle, so
    /// each keeps its own accumulated phase and a badge escalating does not
    /// restart the clock of the state it left.
    pub(crate) fn behaviour(self) -> &'static str {
        match self {
            Self::Idle => crate::anim::behaviour::names::BADGE_REST,
            Self::Active => crate::anim::behaviour::names::BADGE_CHARGE,
            Self::Attention => crate::anim::behaviour::names::BADGE_ALERT,
        }
    }

    /// The lifecycle every tray badge lives on.
    ///
    /// Every state's behaviour is declared up front rather than swapped in when
    /// the state changes, because [`crate::anim::Lifecycle`] is read only when
    /// an element is *new* — a lifecycle that carried one name would freeze a
    /// badge on whatever state it happened to mount in.
    pub(crate) fn lifecycle() -> crate::anim::Lifecycle {
        crate::anim::Lifecycle::still()
            .with_idle(Self::Idle.behaviour())
            .with_idle(Self::Active.behaviour())
            .with_idle(Self::Attention.behaviour())
    }
}

/// When a live badge escalates from [`BadgeState::Active`] to
/// [`BadgeState::Attention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escalation {
    /// The moment it goes live. A blocked agent is *already* waiting, and an
    /// unseen finish *is* the unseen condition — there is no quieter earlier
    /// stage for either to sit in.
    OnArrival,
    /// When the count behind it grows. A second report landing is news; the
    /// first one still standing an hour later is not.
    OnIncrease,
    /// Never. `push` is a standing state — commits do not become more urgent by
    /// sitting there — so it lights and then holds still.
    Never,
}

/// Where one item behind a badge takes you.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrayTarget {
    /// A pane in a Space. Focusing it is the jump.
    Pane { ws_idx: usize, pane_id: PaneId },
    /// A second mate's worker-summary view, which already ships and is already
    /// opened from a badge on that mate's row.
    Summaries { owner: String },
    /// A checkout, named by the Space that holds it.
    Checkout { ws_idx: usize },
    /// A page on the forge.
    Url(String),
}

/// One of the things a badge covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayItem {
    /// What to call it: a worker's handle, a branch, a repository.
    pub label: String,
    /// What about it: the question being asked, the counts, the state.
    /// Rendered under the label, wrapped, and may be several lines.
    pub detail: Vec<String>,
    pub target: TrayTarget,
}

/// What the popup for a badge is allowed to offer.
///
/// See the module docs: this is the authority boundary, expressed as a type so
/// a new badge has to choose one rather than inherit whatever its neighbour
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayAction {
    /// Open the item, and nothing else. Nothing here is a safe yes or no.
    JumpOnly,
    /// A yes and a no, both answered straight into the selected pane.
    YesNo,
    /// Open the item, plus one sweep that is not aimed at any single item.
    JumpAndSweep,
    /// Open the view that already exists for this, rather than a pane.
    OpenSummaries,
    /// One button that runs a named command, with the command, its branch and
    /// its counts printed above it.
    Confirm,
}

/// What an in-place button will do, resolved before it is drawn so the popup
/// can print it and the handler can run it without deciding anything twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrayCommand {
    /// Send a yes or a no to a pane that is blocked on one.
    Answer {
        ws_idx: usize,
        pane_id: PaneId,
        yes: bool,
    },
    /// Clear the unseen flag on every pane that has one.
    MarkAllSeen,
    /// `git push` in this Space's checkout, to `branch`'s own upstream.
    Push { ws_idx: usize, branch: String },
    /// `git pull --rebase` in this Space's checkout, from the same upstream.
    Sync { ws_idx: usize, branch: String },
}

impl TrayCommand {
    /// The exact `git` arguments this runs, for the two commands that run one.
    ///
    /// **No remote and no refspec, on purpose.** `ahead` and `behind` are read
    /// against the branch's *configured upstream*, so that upstream is the only
    /// remote these counts can honestly be acted on — naming `origin` would
    /// push work somewhere the count was never measured against. In a fleet
    /// whose Spaces deliberately keep `origin` pointing at a repository nobody
    /// may push to, that is not a stylistic preference.
    ///
    /// A branch with no upstream simply has no counts, so no button is ever
    /// drawn for it; if git is asked anyway it refuses and says so, which the
    /// popover prints.
    pub(crate) fn argv(&self) -> Option<Vec<String>> {
        match self {
            Self::Push { .. } => Some(vec!["push".to_string()]),
            Self::Sync { .. } => Some(vec!["pull".to_string(), "--rebase".to_string()]),
            Self::Answer { .. } | Self::MarkAllSeen => None,
        }
    }

    /// Exactly what this will do, in the words the popup prints above the
    /// button. A confirmation the user cannot read is not a confirmation.
    pub(crate) fn description(&self, app: &AppState) -> String {
        match self {
            Self::Answer { yes, .. } => {
                format!(
                    "sends \"{}\" and enter to the pane",
                    if *yes { "y" } else { "n" }
                )
            }
            Self::MarkAllSeen => "clears the unseen mark on every finished pane".to_string(),
            // No remote and no refspec, deliberately — see `TrayCommand::argv`.
            // The branch is named in the popup's own body line above this, so
            // the reader still sees which branch without the command claiming a
            // remote Herdr has not checked.
            Self::Push { ws_idx, branch } => {
                format!(
                    "runs: git -C {} push  ({branch} → its upstream)",
                    checkout_label(app, *ws_idx)
                )
            }
            Self::Sync { ws_idx, branch } => {
                format!(
                    "runs: git -C {} pull --rebase  ({branch} ← its upstream)",
                    checkout_label(app, *ws_idx)
                )
            }
        }
    }
}

/// One of the eight, as the tray draws and clicks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayBadge {
    pub signal: FleetSignal,
    pub state: BadgeState,
    pub action: TrayAction,
    /// Everything this badge covers, in tree order. Empty whenever the badge is
    /// idle, and never empty when it is live.
    pub items: Vec<TrayItem>,
}

impl TrayBadge {
    /// The item the popup is currently pointed at.
    pub(crate) fn item(&self, index: usize) -> Option<&TrayItem> {
        self.items.get(index % self.items.len().max(1))
    }

    /// The command the in-place button would run for `index`, when there is
    /// one that is safe to offer.
    ///
    /// `None` is the answer for every [`TrayAction::JumpOnly`] badge, and also
    /// for a badge whose refusal has fired — a refused `sync` degrades to a
    /// jump rather than offering a button that would not run.
    pub(crate) fn command(&self, app: &AppState, index: usize, yes: bool) -> Option<TrayCommand> {
        if self.refusal(app, index).is_some() {
            return None;
        }
        let item = self.item(index)?;
        match (self.action, &item.target) {
            (TrayAction::YesNo, TrayTarget::Pane { ws_idx, pane_id }) => {
                Some(TrayCommand::Answer {
                    ws_idx: *ws_idx,
                    pane_id: *pane_id,
                    yes,
                })
            }
            (TrayAction::JumpAndSweep, _) => Some(TrayCommand::MarkAllSeen),
            (TrayAction::Confirm, TrayTarget::Checkout { ws_idx }) => {
                let branch = branch_of(app, *ws_idx)?;
                Some(match self.signal {
                    FleetSignal::Push => TrayCommand::Push {
                        ws_idx: *ws_idx,
                        branch,
                    },
                    _ => TrayCommand::Sync {
                        ws_idx: *ws_idx,
                        branch,
                    },
                })
            }
            _ => None,
        }
    }

    /// Why this badge's in-place action is not on offer for `index`, when it is
    /// not.
    ///
    /// The one rule that matters: **`sync` refuses on a tree that is not
    /// provably clean.** `git pull --rebase` over uncommitted work is the only
    /// move in this tray that can destroy something, so the refusal is a
    /// property of the badge rather than a check inside the runner — the button
    /// is never drawn, and the popup degrades to the jump it would have offered
    /// anyway.
    ///
    /// An unread tree refuses exactly like a dirty one. The Git status cache is
    /// keyed on refs and cannot vouch for the working tree, so `None` there
    /// means "nobody has looked", and acting on that is the same gamble as
    /// acting on a known-dirty tree.
    pub(crate) fn refusal(&self, app: &AppState, index: usize) -> Option<String> {
        if self.signal != FleetSignal::Sync {
            return None;
        }
        let Some(item) = self.item(index) else {
            // No item to act on is itself a reason not to act.
            return Some("that checkout is gone — nothing to sync".to_string());
        };
        let TrayTarget::Checkout { ws_idx } = item.target else {
            return None;
        };
        // Every arm below refuses except the one that has *seen* a clean tree.
        // A missing workspace refuses too: the Space can be closed while its own
        // popup is open, and "we cannot find it" must fail the way "it is dirty"
        // does rather than falling through to no refusal at all.
        match app.workspaces.get(ws_idx).map(|w| w.git_dirty()) {
            Some(Some(dirty)) if dirty.is_clean() => None,
            Some(Some(_)) => Some("the tree has uncommitted work — open it instead".to_string()),
            Some(None) => Some("the tree has not been read yet — open it instead".to_string()),
            None => Some("that checkout is gone — nothing to sync".to_string()),
        }
    }
}

/// The eight badges as they stand right now.
///
/// An array rather than a `Vec`, and deliberately not `Default`: the eight are
/// a fixed, ordered set, so a reading that held some other number of them would
/// be a reading of something that is not this tray. That is also what makes
/// [`Self::badge`] total — it indexes by the signal's own fixed position rather
/// than searching and needing something to fall back on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayReading {
    badges: [TrayBadge; FleetSignal::COUNT],
    /// The fleet's own pulse, `0.0..=1.0` — the tray's tint, not a slot.
    activity_permille: u16,
}

impl TrayReading {
    pub(crate) fn badge(&self, signal: FleetSignal) -> &TrayBadge {
        &self.badges[index_of(signal)]
    }

    pub(crate) fn badges(&self) -> impl Iterator<Item = &TrayBadge> {
        self.badges.iter()
    }

    /// How hard the fleet is working, in `0.0..=1.0`.
    ///
    /// Stored as permille so the whole reading stays `Eq` and can be compared
    /// against the last one to decide whether the tray's image needs redrawing.
    pub(crate) fn activity(&self) -> f32 {
        f32::from(self.activity_permille) / 1000.0
    }

    /// True when anything at all is asserting.
    ///
    /// Test-facing only: the tray draws all eight slots whether or not any of
    /// them is live, so nothing in the draw path has a reason to ask.
    #[cfg(test)]
    pub(crate) fn any_live(&self) -> bool {
        self.badges.iter().any(|badge| badge.state.is_live())
    }

    /// All eight badges as animation elements, with the fleet's pulse as their
    /// live drive.
    ///
    /// **All eight, always** — unlike the bar's membership, which is only the
    /// live ones. A badge at rest is still saying something, and an element
    /// that dropped out of the set every time its signal cleared would replay
    /// its arrival every time one came back.
    ///
    /// The drive is the fleet's own activity rather than the badge's own
    /// intensity, because a badge's intensity is already carried by *which*
    /// behaviour it plays. What is left for a live drive to say is how hard the
    /// fleet as a whole is working, which is the tray's existing tint concept
    /// applied to tempo.
    pub(crate) fn animation_membership(
        &self,
    ) -> impl Iterator<Item = (crate::anim::ElementId, crate::anim::behaviour::DriveInputs)> + '_
    {
        let activity = self.activity();
        self.badges.iter().map(move |badge| {
            (
                badge.signal.badge_element_id(),
                crate::anim::behaviour::DriveInputs { activity },
            )
        })
    }
}

/// The tray's memory between frames.
///
/// Two things have to be remembered rather than derived, and nothing else is.
/// Escalation is a *transition*, so it needs the previous magnitudes to compare
/// against; and the question a blocked agent is asking lives behind a terminal
/// lock the pure renderer cannot take, so it is snapshotted here.
#[derive(Debug, Clone, Default)]
pub(crate) struct SignalTrayState {
    /// Last observed magnitude per signal, in [`FleetSignal::ALL`] order.
    magnitudes: [usize; FleetSignal::COUNT],
    /// Whether each signal has escalated and not yet been acknowledged.
    attention: [bool; FleetSignal::COUNT],
    /// The open popup, if any.
    pub(crate) popup: Option<SignalTrayPopup>,
    /// What each blocked pane is showing, as of the last refresh.
    questions: HashMap<PaneId, Vec<String>>,
    last_question_refresh: Option<Instant>,
}

impl SignalTrayState {
    /// Fold a fresh set of magnitudes in, returning whether anything moved.
    ///
    /// A signal that clears drops its attention with it: the escalation exists
    /// to say "this became true while you were looking away", and something
    /// that is no longer true has nothing left to say.
    pub(crate) fn observe(&mut self, magnitudes: [usize; FleetSignal::COUNT]) -> bool {
        let before = (self.magnitudes, self.attention);
        for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
            let now = magnitudes[index];
            let was = self.magnitudes[index];
            self.attention[index] = match escalation(signal) {
                _ if now == 0 => false,
                Escalation::OnArrival => true,
                Escalation::OnIncrease => self.attention[index] || now > was,
                Escalation::Never => false,
            };
            self.magnitudes[index] = now;
        }
        before != (self.magnitudes, self.attention)
    }

    /// Drop one signal's escalation, because the captain has now looked at it.
    pub(crate) fn acknowledge(&mut self, signal: FleetSignal) {
        self.attention[index_of(signal)] = false;
    }

    fn escalated(&self, signal: FleetSignal) -> bool {
        self.attention[index_of(signal)]
    }

    /// What this pane was showing at the last question refresh.
    pub(crate) fn question(&self, pane_id: PaneId) -> &[String] {
        self.questions.get(&pane_id).map_or(&[], Vec::as_slice)
    }

    /// Whether the question snapshot is due to be re-read at `now`.
    pub(crate) fn questions_are_due(&self, now: Instant) -> bool {
        self.last_question_refresh
            .is_none_or(|last| now.duration_since(last) >= QUESTION_REFRESH_INTERVAL)
    }

    /// Replace the question snapshot wholesale.
    ///
    /// Wholesale rather than merged, so a pane that stopped being blocked stops
    /// having a remembered question — a stale question under a live badge would
    /// be worse than none.
    pub(crate) fn set_questions(&mut self, now: Instant, questions: HashMap<PaneId, Vec<String>>) {
        self.questions = questions;
        self.last_question_refresh = Some(now);
    }

    /// Trim a blocked pane's screen down to the part that is the question.
    ///
    /// The last paragraph, in order: trailing blank lines are dropped, then
    /// lines are taken upward until the blank run above the prompt. A prompt and
    /// its options are the last thing an agent drew, and the blank line above
    /// them is the agent's own statement about where the prompt begins — a
    /// better boundary than any line count, and the reason this reads a
    /// paragraph rather than "the last six lines".
    ///
    /// [`QUESTION_LINES`] is the ceiling, not the target. A prompt longer than
    /// that keeps its *last* lines, since the options are what the yes and the
    /// no refer to; anything cut off is exactly the case the `open pane` escape
    /// hatch exists for.
    pub(crate) fn question_lines(text: &str) -> Vec<String> {
        let mut lines: Vec<String> = text
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        let mut kept: Vec<String> = Vec::new();
        for line in lines.into_iter().rev() {
            if line.trim().is_empty() || kept.len() >= QUESTION_LINES {
                break;
            }
            kept.push(line);
        }
        kept.reverse();
        kept
    }
}

/// The open popup: which badge, which of its items, and how the last in-place
/// act it ran turned out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignalTrayPopup {
    pub signal: FleetSignal,
    /// Which item `next` has cycled to. Taken modulo the item count when read,
    /// so a badge whose items shrink under an open popup cannot index past
    /// them.
    pub item: usize,
    /// Whether the legend is showing instead of the badge's own contents.
    pub legend: bool,
    /// What the last in-place act reported.
    pub outcome: Option<TrayOutcome>,
}

/// What an in-place act reported when it finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayOutcome {
    pub ok: bool,
    pub message: String,
}

fn index_of(signal: FleetSignal) -> usize {
    FleetSignal::ALL
        .iter()
        .position(|candidate| *candidate == signal)
        .unwrap_or(0)
}

fn escalation(signal: FleetSignal) -> Escalation {
    match signal {
        // Already waiting the instant they are true.
        FleetSignal::Ask | FleetSignal::Review | FleetSignal::Stopped => Escalation::OnArrival,
        // News when the count grows, quiet while it simply stands.
        FleetSignal::Report | FleetSignal::Sync | FleetSignal::Pr | FleetSignal::Checks => {
            Escalation::OnIncrease
        }
        // Commits do not get more urgent by sitting on the machine.
        FleetSignal::Push => Escalation::Never,
    }
}

/// Which authority each of the eight is given. See the module docs.
pub(crate) fn tray_action(signal: FleetSignal) -> TrayAction {
    match signal {
        FleetSignal::Ask => TrayAction::YesNo,
        FleetSignal::Review => TrayAction::JumpAndSweep,
        FleetSignal::Report => TrayAction::OpenSummaries,
        FleetSignal::Push | FleetSignal::Sync => TrayAction::Confirm,
        // No in-place action, deliberately. Restarting a worker that died needs
        // a launch command Herdr does not record per pane, and "re-run the
        // failed jobs" is a decision rather than a yes.
        FleetSignal::Stopped | FleetSignal::Pr | FleetSignal::Checks => TrayAction::JumpOnly,
    }
}

/// Read the whole tray out of the app's current state.
///
/// Pure, and a pure function of [`AppState`] alone: the item lists come from
/// the same walk [`FleetSignals::resolve`] makes, so a badge can never be lit
/// with nothing behind it or dark with something behind it.
pub(crate) fn resolve(app: &AppState) -> TrayReading {
    let signals = FleetSignals::resolve(app);
    let items = collect_items(app);

    let badges = FleetSignal::ALL.map(|signal| {
        let items = items.get(&index_of(signal)).cloned().unwrap_or_default();
        let state = if !signals.is_live(signal) || items.is_empty() {
            BadgeState::Idle
        } else if app.signal_tray.escalated(signal) {
            BadgeState::Attention
        } else {
            BadgeState::Active
        };
        TrayBadge {
            signal,
            state,
            action: tray_action(signal),
            items,
        }
    });

    TrayReading {
        badges,
        activity_permille: (signals.activity() * 1000.0).round().clamp(0.0, 1000.0) as u16,
    }
}

/// The magnitude behind each signal, in [`FleetSignal::ALL`] order.
///
/// Fed to [`SignalTrayState::observe`], which is the only thing that turns a
/// change in these numbers into an escalation.
pub(crate) fn magnitudes(app: &AppState) -> [usize; FleetSignal::COUNT] {
    let items = collect_items(app);
    let signals = FleetSignals::resolve(app);
    let mut out = [0usize; FleetSignal::COUNT];
    for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
        out[index] = if signals.is_live(signal) {
            items.get(&index).map_or(0, Vec::len)
        } else {
            0
        };
    }
    out
}

/// Everything behind every badge, keyed by the signal's index.
///
/// One walk of the fleet for all eight, so the lists cannot disagree about
/// which panes exist, and in tree order so `next` cycles the way the sidebar
/// reads.
fn collect_items(app: &AppState) -> HashMap<usize, Vec<TrayItem>> {
    let mut items: HashMap<usize, Vec<TrayItem>> = HashMap::new();
    let mut push = |signal: FleetSignal, item: TrayItem| {
        let bucket = items.entry(index_of(signal)).or_default();
        if bucket.len() < MAX_ITEMS {
            bucket.push(item);
        }
    };

    for (ws_idx, workspace) in app.workspaces.iter().enumerate() {
        let space = workspace.display_name_from_terminals(&app.terminals);

        if let Some((ahead, behind)) = workspace.git_ahead_behind() {
            let branch = workspace.branch().unwrap_or_else(|| "HEAD".to_string());
            if ahead > 0 {
                push(
                    FleetSignal::Push,
                    TrayItem {
                        label: format!("{space} · {branch}"),
                        detail: vec![format!("{ahead} commit{} not on the remote", plural(ahead))],
                        target: TrayTarget::Checkout { ws_idx },
                    },
                );
            }
            if behind > 0 {
                push(
                    FleetSignal::Sync,
                    TrayItem {
                        label: format!("{space} · {branch}"),
                        detail: vec![format!(
                            "{behind} commit{} on the remote you do not have",
                            plural(behind)
                        )],
                        target: TrayTarget::Checkout { ws_idx },
                    },
                );
            }
        }

        if let Some(counts) = workspace.pull_requests() {
            let waiting = counts.review_requested + counts.changes_requested;
            if waiting > 0 {
                push(
                    FleetSignal::Pr,
                    TrayItem {
                        label: space.clone(),
                        detail: vec![
                            format!(
                                "{} review{} requested of you",
                                counts.review_requested,
                                plural(counts.review_requested)
                            ),
                            format!(
                                "{} of yours sent back for changes",
                                counts.changes_requested
                            ),
                        ],
                        target: TrayTarget::Url(forge_url(
                            workspace,
                            "is%3Aopen+review-requested%3A%40me",
                        )),
                    },
                );
            }
            if counts.checks_failing + counts.checks_pending > 0 {
                push(
                    FleetSignal::Checks,
                    TrayItem {
                        label: space.clone(),
                        detail: vec![
                            format!("{} of yours red", counts.checks_failing),
                            format!("{} still running", counts.checks_pending),
                        ],
                        target: TrayTarget::Url(forge_url(workspace, "is%3Aopen+author%3A%40me")),
                    },
                );
            }
        }

        for tab in &workspace.tabs {
            for (pane_id, pane) in &tab.panes {
                let Some(terminal) = app.terminals.get(&pane.attached_terminal_id) else {
                    continue;
                };
                let owner = terminal
                    .metadata_tokens
                    .get(crate::app::agent_tree::OWNER_TOKEN)
                    .map(str::trim)
                    .filter(|owner| !owner.is_empty());
                let label = pane_label(terminal, &space);
                let pane_id = *pane_id;
                let target = TrayTarget::Pane { ws_idx, pane_id };

                match terminal.state {
                    AgentState::Blocked => push(
                        FleetSignal::Ask,
                        TrayItem {
                            label: format!("{label} is waiting on you"),
                            detail: app.signal_tray.question(pane_id).to_vec(),
                            target: target.clone(),
                        },
                    ),
                    AgentState::Idle if !pane.seen => push(
                        FleetSignal::Review,
                        TrayItem {
                            label: label.clone(),
                            detail: vec!["finished while you were looking elsewhere".to_string()],
                            target: target.clone(),
                        },
                    ),
                    AgentState::Unknown if owner.is_some() => push(
                        FleetSignal::Stopped,
                        TrayItem {
                            label: label.clone(),
                            detail: vec!["no longer running an agent".to_string()],
                            target: target.clone(),
                        },
                    ),
                    _ => {}
                }

                if crate::app::worker_summary::is_summary_line(
                    terminal
                        .metadata_tokens
                        .get(crate::app::worker_summary::SUMMARY_TOKEN),
                ) {
                    // The destination is the mate's summary view, which already
                    // ships; a worker with no resolvable owner has no such view
                    // and falls back to its own pane.
                    let target = owner.map_or(target, |owner| TrayTarget::Summaries {
                        owner: owner.to_string(),
                    });
                    push(
                        FleetSignal::Report,
                        TrayItem {
                            label,
                            detail: terminal
                                .metadata_tokens
                                .get(crate::app::worker_summary::SUMMARY_TOKEN)
                                .map(|line| vec![line.trim().to_string()])
                                .unwrap_or_default(),
                            target,
                        },
                    );
                }
            }
        }
    }

    items
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// What to call a pane in a tray item.
///
/// The handle first, because that is the name the fleet already knows it by and
/// the one an `owner` token would spell; then whatever the human labelled it;
/// then the Space it lives in, which at least says where to look.
fn pane_label(terminal: &crate::terminal::TerminalState, space: &str) -> String {
    [
        terminal.agent_name.as_deref(),
        terminal.manual_label.as_deref(),
        terminal.terminal_title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|name| !name.is_empty())
    .unwrap_or(space)
    .to_string()
}

fn branch_of(app: &AppState, ws_idx: usize) -> Option<String> {
    app.workspaces.get(ws_idx)?.branch()
}

fn checkout_label(app: &AppState, ws_idx: usize) -> String {
    app.workspaces
        .get(ws_idx)
        .map(|workspace| workspace.cached_identity_cwd.display().to_string())
        .unwrap_or_default()
}

/// A forge page for this checkout, filtered to the thing the badge is about.
///
/// A filter rather than a pull request number: the counts Herdr caches are
/// atomic totals, so it does not know *which* pull request is red, and a
/// filtered list is an honest destination where a guessed number would not be.
fn forge_url(workspace: &crate::workspace::Workspace, query: &str) -> String {
    workspace
        .cached_remote_url
        .as_deref()
        .and_then(crate::forge::RepoSlug::parse)
        .map(|slug| {
            format!(
                "https://{}/{}/{}/pulls?q={query}",
                slug.host, slug.owner, slug.name
            )
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_workspace() -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        app
    }

    /// Three states, three behaviours, and every one of them registered.
    ///
    /// A state whose behaviour name was not in the catalogue would animate
    /// nothing at all — the engine treats an unregistered name as "plays
    /// nothing", deliberately, so a typo here is a badge that silently stops
    /// moving rather than a failure anyone would notice.
    #[test]
    fn each_badge_state_moves_on_its_own_registered_behaviour() {
        let catalogue = crate::anim::behaviour::Catalogue::built_in();
        let mut seen = Vec::new();
        for state in [BadgeState::Idle, BadgeState::Active, BadgeState::Attention] {
            let name = state.behaviour();
            assert!(
                catalogue.get(name).is_some(),
                "{state:?} names {name}, which nothing registered"
            );
            assert!(
                !seen.contains(&name),
                "{state:?} shares {name} with another state"
            );
            seen.push(name);
        }
    }

    /// The lifecycle has to declare all three, not just the one a badge
    /// happened to mount in.
    ///
    /// `Lifecycle` is read only when an element is new, so a badge admitted
    /// while idle and later escalating would have no phase for the behaviour it
    /// is now being asked to play, and would freeze.
    #[test]
    fn a_badge_can_change_state_without_being_remounted() {
        use std::time::{Duration, Instant};

        let lifecycle = BadgeState::lifecycle();
        let mut anim = crate::anim::Animator::default();
        let id = FleetSignal::Ask.badge_element_id();
        let now = Instant::now();
        anim.enter(
            id.clone(),
            &lifecycle,
            crate::anim::behaviour::DriveInputs::default(),
            now,
        );
        anim.advance(now + Duration::from_millis(300));

        for state in [BadgeState::Idle, BadgeState::Active, BadgeState::Attention] {
            let frame = anim
                .frame(&id, Some(state.behaviour()))
                .expect("the badge is mounted");
            assert!(
                frame.behaviour.is_some(),
                "{state:?} resolved to no behaviour on a badge that never changed element"
            );
        }
    }

    /// All eight are published, whatever they are doing.
    #[test]
    fn the_membership_carries_every_badge_not_only_the_lit_ones() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_git_ahead_behind = Some((1, 0));
        let reading = resolve(&app);
        assert!(reading.any_live(), "the fixture lit nothing");

        let published: Vec<_> = reading.animation_membership().map(|(id, _)| id).collect();
        assert_eq!(published.len(), FleetSignal::COUNT);
        for signal in FleetSignal::ALL {
            assert!(published.contains(&signal.badge_element_id()), "{signal:?}");
        }
    }

    /// The safety property the whole tray is built around. `sync` is the only
    /// badge that can destroy work, and it must refuse rather than rebase over
    /// uncommitted changes.
    #[test]
    fn sync_refuses_on_a_dirty_tree_and_offers_no_command() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_git_ahead_behind = Some((0, 3));
        app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
            staged: 0,
            unstaged: 1,
            untracked: 0,
        });

        let reading = resolve(&app);
        let badge = reading.badge(FleetSignal::Sync);
        assert!(badge.state.is_live());
        assert!(badge.refusal(&app, 0).is_some(), "a dirty tree must refuse");
        assert_eq!(
            badge.command(&app, 0, true),
            None,
            "a refused sync must not offer a button that would rebase anyway"
        );
    }

    /// "We have not looked" fails the same way "it is dirty" does. The Git
    /// status cache is keyed on refs and cannot vouch for the working tree, so
    /// an unread tree is not a clean one.
    #[test]
    fn sync_refuses_a_tree_nobody_has_read() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_git_ahead_behind = Some((0, 3));
        app.workspaces[0].cached_git_dirty = None;

        let badge = resolve(&app).badge(FleetSignal::Sync).clone();
        assert!(badge.refusal(&app, 0).is_some());
        assert_eq!(badge.command(&app, 0, true), None);
    }

    #[test]
    fn sync_offers_its_command_on_a_clean_tree() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_git_ahead_behind = Some((0, 3));
        app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts::default());
        app.workspaces[0].cached_git_branch = Some("feature".into());

        let badge = resolve(&app).badge(FleetSignal::Sync).clone();
        assert_eq!(badge.refusal(&app, 0), None);
        assert_eq!(
            badge.command(&app, 0, true),
            Some(TrayCommand::Sync {
                ws_idx: 0,
                branch: "feature".into(),
            })
        );
    }

    /// A dirty tree must never stop a `push`: pushing publishes commits that
    /// are already made and touches nothing in the working tree.
    /// Every arm of the refusal refuses except the one that has *seen* a clean
    /// tree. A Space can be closed while its own popup is open, and "we cannot
    /// find it" has to fail the way "it is dirty" does — a `None` there would
    /// have been read as "nothing to refuse".
    #[test]
    fn a_checkout_that_has_gone_refuses_rather_than_falling_through() {
        let app = app_with_workspace();
        let badge = TrayBadge {
            signal: FleetSignal::Sync,
            state: BadgeState::Active,
            action: tray_action(FleetSignal::Sync),
            items: vec![TrayItem {
                label: "gone".into(),
                detail: Vec::new(),
                // An index past the end of the workspace list: the Space this
                // badge was resolved against has since been closed.
                target: TrayTarget::Checkout { ws_idx: 99 },
            }],
        };
        assert!(badge.refusal(&app, 0).is_some());
        assert_eq!(badge.command(&app, 0, true), None);

        // And a badge with no items at all refuses rather than answering None.
        let empty = TrayBadge {
            items: Vec::new(),
            ..badge
        };
        assert!(empty.refusal(&app, 0).is_some());
    }

    /// The reading holds exactly the eight, so looking one up is total: there is
    /// no empty reading for a lookup to fall off the end of.
    #[test]
    fn every_signal_resolves_to_its_own_badge() {
        let app = app_with_workspace();
        let reading = resolve(&app);
        for signal in FleetSignal::ALL {
            assert_eq!(reading.badge(signal).signal, signal);
        }
    }

    #[test]
    fn push_is_not_refused_by_a_dirty_tree() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_git_ahead_behind = Some((2, 0));
        app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
            staged: 3,
            unstaged: 0,
            untracked: 0,
        });
        app.workspaces[0].cached_git_branch = Some("main".into());

        let badge = resolve(&app).badge(FleetSignal::Push).clone();
        assert_eq!(badge.refusal(&app, 0), None);
        assert!(matches!(
            badge.command(&app, 0, true),
            Some(TrayCommand::Push { .. })
        ));
    }

    /// The three that deliberately have no in-place act must not grow one by
    /// accident. This is the boundary the captain set, asserted rather than
    /// documented.
    #[test]
    fn the_jump_only_badges_never_offer_a_command() {
        for signal in [FleetSignal::Stopped, FleetSignal::Pr, FleetSignal::Checks] {
            assert_eq!(tray_action(signal), TrayAction::JumpOnly, "{signal:?}");
            let badge = TrayBadge {
                signal,
                state: BadgeState::Active,
                action: tray_action(signal),
                items: vec![TrayItem {
                    label: "x".into(),
                    detail: Vec::new(),
                    target: TrayTarget::Checkout { ws_idx: 0 },
                }],
            };
            let app = app_with_workspace();
            assert_eq!(badge.command(&app, 0, true), None, "{signal:?}");
            assert_eq!(badge.command(&app, 0, false), None, "{signal:?}");
        }
    }

    /// The one thing this tray must never do: push somewhere the counts were
    /// not measured against.
    ///
    /// `ahead`/`behind` are read against the branch's *configured upstream*, so
    /// that is the only remote acting on them can honestly mean. Naming a
    /// remote on the command line would send work to whatever `origin` happens
    /// to be — and in a fleet whose Spaces keep `origin` pointing at a
    /// repository nobody may push to, that is a real accident waiting to
    /// happen rather than a style point.
    #[test]
    fn the_git_commands_never_name_a_remote_or_a_refspec() {
        let commands = [
            TrayCommand::Push {
                ws_idx: 0,
                branch: "feature".into(),
            },
            TrayCommand::Sync {
                ws_idx: 0,
                branch: "feature".into(),
            },
        ];
        for command in commands {
            let argv = command.argv().expect("a git command has an argv");
            assert!(
                !argv.iter().any(|arg| arg == "origin"
                    || arg == "feature"
                    || arg.starts_with('-') && arg != "--rebase"),
                "{command:?} named a remote or a refspec: {argv:?}"
            );
        }
        assert_eq!(
            TrayCommand::Push {
                ws_idx: 0,
                branch: "b".into()
            }
            .argv(),
            Some(vec!["push".to_string()])
        );
        assert_eq!(
            TrayCommand::Sync {
                ws_idx: 0,
                branch: "b".into()
            }
            .argv(),
            Some(vec!["pull".to_string(), "--rebase".to_string()])
        );
    }

    /// The two that only touch Herdr's own state must not look like shell
    /// commands, or a caller could hand them to the subprocess runner.
    #[test]
    fn the_in_herdr_acts_have_no_argv_at_all() {
        assert_eq!(TrayCommand::MarkAllSeen.argv(), None);
        assert_eq!(
            TrayCommand::Answer {
                ws_idx: 0,
                pane_id: crate::layout::PaneId::alloc(),
                yes: true,
            }
            .argv(),
            None
        );
    }

    #[test]
    fn a_command_says_exactly_what_it_will_run() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_identity_cwd = std::path::PathBuf::from("/w/repo");
        let described = TrayCommand::Push {
            ws_idx: 0,
            branch: "feat/x".into(),
        }
        .description(&app);
        // The command exactly, and the branch beside it — but no remote, so
        // nothing here can promise a destination Herdr did not check.
        assert!(
            described.contains("git -C /w/repo push") && described.contains("feat/x"),
            "{described}"
        );
        assert!(!described.contains("origin"), "{described}");
    }

    #[test]
    fn a_blocked_agent_escalates_the_moment_it_arrives() {
        let mut state = SignalTrayState::default();
        let mut counts = [0usize; FleetSignal::COUNT];
        counts[index_of(FleetSignal::Ask)] = 1;
        state.observe(counts);
        assert!(state.escalated(FleetSignal::Ask));
    }

    /// `push` is a standing state. Commits do not become more urgent by
    /// sitting on the machine, so the slot lights and then holds still.
    #[test]
    fn push_lights_but_never_escalates() {
        let mut state = SignalTrayState::default();
        let mut counts = [0usize; FleetSignal::COUNT];
        counts[index_of(FleetSignal::Push)] = 1;
        state.observe(counts);
        assert!(!state.escalated(FleetSignal::Push));
        counts[index_of(FleetSignal::Push)] = 9;
        state.observe(counts);
        assert!(!state.escalated(FleetSignal::Push));
    }

    #[test]
    fn a_standing_report_is_quiet_until_another_one_lands() {
        let mut state = SignalTrayState::default();
        let mut counts = [0usize; FleetSignal::COUNT];
        counts[index_of(FleetSignal::Report)] = 1;
        state.observe(counts);
        assert!(
            state.escalated(FleetSignal::Report),
            "the first one is news"
        );

        state.acknowledge(FleetSignal::Report);
        state.observe(counts);
        assert!(
            !state.escalated(FleetSignal::Report),
            "looked at, and standing"
        );

        counts[index_of(FleetSignal::Report)] = 2;
        state.observe(counts);
        assert!(
            state.escalated(FleetSignal::Report),
            "a second one is news again"
        );
    }

    #[test]
    fn a_signal_that_clears_takes_its_escalation_with_it() {
        let mut state = SignalTrayState::default();
        let mut counts = [0usize; FleetSignal::COUNT];
        counts[index_of(FleetSignal::Ask)] = 2;
        state.observe(counts);
        counts[index_of(FleetSignal::Ask)] = 0;
        state.observe(counts);
        assert!(!state.escalated(FleetSignal::Ask));
    }

    #[test]
    fn the_question_is_the_last_thing_the_agent_drew() {
        let text = "boot\n\n\n\nDo you want to proceed?\n  1. yes\n  2. no\n\n\n";
        let lines = SignalTrayState::question_lines(text);
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Do you want to proceed?")
        );
        assert_eq!(lines.last().map(String::as_str), Some("  2. no"));
        assert!(lines.len() <= QUESTION_LINES);
    }

    #[test]
    fn an_empty_screen_yields_no_question_rather_than_blank_lines() {
        assert!(SignalTrayState::question_lines("   \n\n  \n").is_empty());
    }

    /// A prompt longer than the ceiling keeps its *end*: the options are what
    /// the yes and the no in the popup refer to, so losing them would leave the
    /// captain answering a question whose answers he cannot see.
    #[test]
    fn an_over_long_prompt_keeps_the_options_rather_than_the_preamble() {
        let text = "\nline1\nline2\nline3\nline4\nline5\nline6\nline7\n  1. yes\n  2. no\n";
        let lines = SignalTrayState::question_lines(text);
        assert_eq!(lines.len(), QUESTION_LINES);
        assert_eq!(lines.last().map(String::as_str), Some("  2. no"));
    }

    /// A badge is lit if and only if it has something behind it. A lit badge
    /// with an empty popup would be the tray lying about the fleet.
    #[test]
    fn no_badge_is_ever_lit_with_nothing_behind_it() {
        let mut app = app_with_workspace();
        app.workspaces[0].cached_git_ahead_behind = Some((1, 2));
        app.workspaces[0].cached_pull_requests = Some(crate::forge::PullRequestCounts {
            review_requested: 1,
            checks_failing: 1,
            ..Default::default()
        });

        let reading = resolve(&app);
        assert!(reading.any_live());
        for badge in reading.badges() {
            assert_eq!(
                badge.state.is_live(),
                !badge.items.is_empty(),
                "{:?} is lit={:?} with {} items",
                badge.signal,
                badge.state,
                badge.items.len()
            );
        }
    }

    #[test]
    fn every_badge_is_present_in_the_fixed_order() {
        let app = app_with_workspace();
        let reading = resolve(&app);
        assert_eq!(
            reading.badges().map(|b| b.signal).collect::<Vec<_>>(),
            FleetSignal::ALL.to_vec()
        );
    }
}
