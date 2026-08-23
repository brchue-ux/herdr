//! The eight things a fleet can be waiting on, rolled up from state Herdr
//! already holds.
//!
//! Every signal here is a *reading* of an existing source, never a new one.
//! `ask`, `review` and `stopped` read the per-pane agent state the detector
//! already publishes; `report` reads the summary tokens
//! [`crate::app::worker_summary`] groups; `push`, `sync`, `pr` and `checks`
//! read the counts the background Git and forge refreshes already cache on each
//! [`crate::workspace::Workspace`]. Nothing in this module runs a subprocess,
//! touches the network, or keeps a clock of its own — which is what makes the
//! whole roll-up a pure function of [`AppState`] and testable without a PTY.
//!
//! Four properties this module is responsible for holding:
//!
//! - **The eight are a fixed, ordered set.** [`FleetSignal::ALL`] is the order
//!   they are always read in, so a reader learns the positions once and a
//!   signal never moves under them. Adding a ninth is an explicit edit here,
//!   not something a caller can do by publishing a new name.
//! - **Every one of the eight is an action item.** A signal earns its slot by
//!   having an owner, a clearing act and a destination. That test is why the
//!   set is what it is: `busy` has no owner and clears itself, and `dirty` is
//!   lit in every session anyone actually works in and is only the precondition
//!   for `push`, so neither is a signal. How hard the fleet is working is still
//!   read here — as [`FleetSignals::activity`], the tray's own tint — but it is
//!   not one of the eight things you can act on.
//! - **A signal is live or it is not.** [`FleetSignals::is_live`] is a boolean.
//!   The only continuously varying quantity in the whole roll-up is
//!   [`FleetSignals::activity`], and keeping it off the eight is what stops a
//!   renderer from having to invent a threshold.
//! - **A source that has never answered is quiet, not alarming.** The Git and
//!   forge caches are `None` until their first refresh lands, and `None` reads
//!   as "nothing outstanding" rather than as an alert, so a Herdr that has just
//!   started does not light its own tray up. A quota window nobody has
//!   published reads as absent for the same reason, never as zero.
//!
//! The same walk also counts three things that are *not* signals — how many
//! panes are running, how many are waiting on the captain, and the account's
//! quota window. They are counted here rather than in a second pass because
//! this one already visits every pane in every tab in every workspace, and the
//! surface that draws them ([`crate::ui::sidebar::notifications`]) runs once a
//! frame.

use crate::app::state::AppState;
use crate::detect::AgentState;

/// One of the eight things the fleet can be waiting on.
///
/// Two rows of four, and the split is the whole ordering: the first four are
/// the fleet waiting on the captain, the last four are the repository waiting
/// on him. Within each row they run from the most immediate to the most
/// deferrable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FleetSignal {
    /// An agent is blocked on a human answer — [`AgentState::Blocked`].
    ///
    /// The flagship: the only one of the eight that is already waiting the
    /// instant it goes live, and the only one whose answer is a yes or a no.
    Ask,
    /// An agent finished while the captain was looking somewhere else.
    ///
    /// Herdr's own `done` state: a pane that is idle and whose
    /// [`crate::pane::PaneState::seen`] flag is still false.
    Review,
    /// A worker has published a completion summary that is still standing.
    ///
    /// The same `summary` token family the worker-summary badge counts, so the
    /// tray and the badge can never disagree about whether a report exists.
    Report,
    /// A pane the fleet owns is no longer running an agent.
    ///
    /// Owned means a mate published an `owner` token for it, so this is a
    /// worker that was launched to do something and is now back at a bare
    /// prompt — not any shell pane the captain happens to have open.
    Stopped,
    /// Commits on a branch that have not left the machine — `ahead > 0`.
    Push,
    /// Commits on the upstream that this checkout does not have — `behind > 0`.
    ///
    /// Split from [`Self::Push`] rather than sharing its slot because the two
    /// are opposite actions with opposite risk: one publishes work, the other
    /// rewrites the branch it lands on. A slot whose click did one of two
    /// contradictory things would be worse than no slot.
    Sync,
    /// A pull request is waiting on the captain specifically: a review has been
    /// requested of him, or one of his own has been sent back for changes.
    ///
    /// Not "a pull request is open". Open pull requests are the steady state of
    /// a working repository and reading them as an alert lights the slot
    /// permanently.
    Pr,
    /// A check run on one of the captain's own pull requests is red or has not
    /// finished.
    Checks,
}

impl FleetSignal {
    /// How many signals there are. The bar's whole readability rests on this
    /// being a small fixed number, so it is spelled once here and every width
    /// and every array length is derived from it.
    pub(crate) const COUNT: usize = 8;

    /// Every signal, in the fixed order they are always read in.
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Ask,
        Self::Review,
        Self::Report,
        Self::Stopped,
        Self::Push,
        Self::Sync,
        Self::Pr,
        Self::Checks,
    ];

    /// How many slots one row of the tray holds.
    ///
    /// Four, so [`Self::ALL`] read in order fills row one and then row two. The
    /// tray's two rows mean different things — see the type docs — so this is a
    /// property of the set, not of the layout that draws it.
    pub(crate) const PER_ROW: usize = 4;

    /// The word this signal answers to.
    ///
    /// Short enough that eight of them plus their marks fit a wide sidebar, and
    /// a real word rather than an abbreviation because the resting bar's whole
    /// job is to say what the eight things are.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Review => "review",
            Self::Report => "report",
            Self::Stopped => "stopped",
            Self::Push => "push",
            Self::Sync => "sync",
            Self::Pr => "pr",
            Self::Checks => "checks",
        }
    }

    /// One line saying what this signal means, for the tray's legend.
    ///
    /// Phrased as the fact that is true while the slot is lit, not as the
    /// action — the action is on the button in the popup, where it belongs.
    pub(crate) fn meaning(self) -> &'static str {
        match self {
            Self::Ask => "agent blocked on you",
            Self::Review => "finished, not looked at",
            Self::Report => "a summary is standing",
            Self::Stopped => "owned worker went away",
            Self::Push => "commits not on the remote",
            Self::Sync => "remote has commits you lack",
            Self::Pr => "a pull request wants you",
            Self::Checks => "ci or a bot review needs you",
        }
    }

    /// The one-cell mark that stands in for the name when the panel is narrow.
    ///
    /// Reused from Herdr's own vocabulary wherever one exists — `●` and `◉` are
    /// the marks the state dot already draws for done and blocked, and `↑` and
    /// `↓` are git's own porcelain marks, already drawn by the `git_status`
    /// token. A reader who knows the tree already knows most of the tray.
    ///
    /// These marks are the *fallback*, not the design. The tray draws the eight
    /// as images (see [`crate::ui::sidebar::tray`]); one cell of a font Herdr
    /// does not control cannot carry a mark with an interior detail, which is
    /// exactly what tells `report` and `checks` apart.
    pub(crate) fn mark(self) -> &'static str {
        match self {
            Self::Ask => "◉",
            Self::Review => "●",
            Self::Report => "≡",
            Self::Stopped => "⊘",
            Self::Push => "↑",
            Self::Sync => "↓",
            Self::Pr => "⋔",
            Self::Checks => "✓",
        }
    }

    /// The animation element this signal owns while it is live.
    ///
    /// `&'static str` because the set is fixed and closed: an element key can
    /// never be composed from anything a pane published, so no amount of fleet
    /// traffic can grow the engine's element table.
    pub(crate) fn element_key(self) -> &'static str {
        match self {
            Self::Ask => "fleet-signal.ask",
            Self::Review => "fleet-signal.review",
            Self::Report => "fleet-signal.report",
            Self::Stopped => "fleet-signal.stopped",
            Self::Push => "fleet-signal.push",
            Self::Sync => "fleet-signal.sync",
            Self::Pr => "fleet-signal.pr",
            Self::Checks => "fleet-signal.checks",
        }
    }

    /// The animation element id for this signal.
    pub(crate) fn element_id(self) -> crate::anim::ElementId {
        crate::anim::ElementId::Named(self.element_key())
    }

    /// The animation element this signal's *tray badge* owns.
    ///
    /// A different element from [`Self::element_id`], in a different family,
    /// and that separation is load-bearing rather than incidental. The bar's
    /// element exists only while the signal is live, because a slot that is not
    /// asserting has nothing to play. A badge's exists always, because rest is
    /// one of the three things a badge says — see
    /// [`crate::anim::ElementId::TrayBadge`].
    pub(crate) fn badge_element_id(self) -> crate::anim::ElementId {
        crate::anim::ElementId::TrayBadge(self)
    }

    /// True when answering this signal costs a `git status` scan.
    ///
    /// None of the eight *light* on a dirty tree — that is exactly why `dirty`
    /// is not one of them. `sync` reads it anyway, because a rebase over
    /// uncommitted work is the one thing in this tray that could destroy
    /// something, and the refusal has to be able to see the tree to make it.
    /// A source that is never refreshed reads as `None`, and `sync` treats
    /// `None` as "refuse" — so without this demand the slot would be honest but
    /// permanently useless.
    fn needs_git_dirty(self) -> bool {
        matches!(self, Self::Sync)
    }

    /// True when answering this signal costs a read of the branch's upstream.
    fn needs_git_ahead_behind(self) -> bool {
        matches!(self, Self::Push | Self::Sync)
    }

    /// True when answering this signal costs a network round trip to the forge.
    fn needs_pull_requests(self) -> bool {
        matches!(self, Self::Pr | Self::Checks)
    }
}

/// Which of the eight are live right now, plus the fleet's own pulse.
///
/// A plain value rather than a borrow of [`AppState`]: a render pass resolves
/// it once per frame and every consumer of that frame — the layout, the
/// renderer, the animation membership set — reads the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct FleetSignals {
    live: [bool; FleetSignal::COUNT],
    /// Busiest pane's smoothed work volume, in `0.0..=1.0`.
    fleet_activity: f32,
    /// Panes with an agent actually working right now.
    running: usize,
    /// Panes with something outstanding for the captain.
    awaiting: usize,
    /// The account's 5-hour window, as a percentage used, when a publisher has
    /// reported one.
    quota_percent: Option<f64>,
}

impl FleetSignals {
    /// Read all eight out of the app's current state.
    pub(crate) fn resolve(app: &AppState) -> Self {
        let mut signals = Self {
            fleet_activity: fleet_activity(app),
            ..Self::default()
        };

        for workspace in &app.workspaces {
            // The quota windows are account-level facts a fleet publisher
            // happens to write onto a workspace, so every workspace that
            // carries one is reporting the same account. The worst reading
            // wins rather than the first: a stale publisher that has not
            // caught up must not talk the readout down.
            if let Some(percent) = workspace
                .metadata_tokens
                .get(crate::quota::SESSION_TOKEN)
                .and_then(crate::quota::parse)
                .map(|readout| readout.percent_used)
            {
                signals.quota_percent = Some(match signals.quota_percent {
                    Some(seen) => seen.max(percent),
                    None => percent,
                });
            }

            if let Some((ahead, behind)) = workspace.git_ahead_behind() {
                if ahead > 0 {
                    signals.set(FleetSignal::Push);
                }
                if behind > 0 {
                    signals.set(FleetSignal::Sync);
                }
            }
            if let Some(counts) = workspace.pull_requests() {
                if counts.review_requested > 0 || counts.changes_requested > 0 {
                    signals.set(FleetSignal::Pr);
                }
                if counts.checks_failing > 0 || counts.checks_pending > 0 {
                    signals.set(FleetSignal::Checks);
                }
            }

            for pane in workspace.tabs.iter().flat_map(|tab| tab.panes.values()) {
                let Some(terminal) = app.terminals.get(&pane.attached_terminal_id) else {
                    continue;
                };
                let owned = terminal
                    .metadata_tokens
                    .get(crate::app::agent_tree::OWNER_TOKEN)
                    .is_some_and(|owner| !owner.trim().is_empty());

                // Whether this one pane has anything outstanding for the
                // captain. Accumulated per pane rather than per signal so a
                // pane that is both unlooked-at and carrying a standing report
                // is one thing waiting, not two.
                let mut awaiting = false;

                match terminal.state {
                    AgentState::Blocked => {
                        signals.set(FleetSignal::Ask);
                        awaiting = true;
                    }
                    AgentState::Idle if !pane.seen => {
                        signals.set(FleetSignal::Review);
                        awaiting = true;
                    }
                    // A pane the fleet owns and that is no longer running an
                    // agent is a worker that stopped. An unowned shell is just
                    // a shell, and lighting the tray for one would make the
                    // signal useless in any session with a spare terminal open.
                    //
                    // A worker that is still *starting* reads as `Unknown` too:
                    // process detection claims the agent before the screen
                    // detector has confirmed its prompt, and the pane holds that
                    // state for the whole startup grace window. That is the
                    // opposite of stopped, so it is excluded by the same flag
                    // that gates the startup completion notification.
                    AgentState::Unknown
                        if owned && !terminal.agent_process_acquisition_pending() =>
                    {
                        signals.set(FleetSignal::Stopped);
                        awaiting = true;
                    }
                    AgentState::Working => signals.running += 1,
                    _ => {}
                }

                if crate::app::worker_summary::is_summary_line(
                    terminal
                        .metadata_tokens
                        .get(crate::app::worker_summary::SUMMARY_TOKEN),
                ) {
                    signals.set(FleetSignal::Report);
                    awaiting = true;
                }

                signals.awaiting += usize::from(awaiting);
            }
        }

        signals
    }

    fn set(&mut self, signal: FleetSignal) {
        self.live[index(signal)] = true;
    }

    /// A reading with the three counted facts set directly.
    ///
    /// So a renderer's wording, width and colour tests can say what fleet they
    /// mean in one line instead of assembling panes to imply it.
    #[cfg(test)]
    pub(crate) fn test_reading(
        running: usize,
        awaiting: usize,
        quota_percent: Option<f64>,
    ) -> Self {
        Self {
            running,
            awaiting,
            quota_percent,
            ..Self::default()
        }
    }

    /// Light one signal on a test reading.
    #[cfg(test)]
    pub(crate) fn set_for_test(&mut self, signal: FleetSignal) {
        self.set(signal);
    }

    pub(crate) fn is_live(&self, signal: FleetSignal) -> bool {
        self.live[index(signal)]
    }

    /// True when anything at all is asserting.
    ///
    /// Test-facing only: the tray draws all eight badges whether or not any of
    /// them is live, so nothing in the draw path has a reason to ask.
    #[cfg(test)]
    pub(crate) fn any_live(&self) -> bool {
        self.live.iter().any(|live| *live)
    }

    /// How strongly this signal is asserting itself, in `0.0..=1.0`.
    ///
    /// This is what an animated behaviour's live drive binds to. All eight are
    /// yes-or-no facts, so a live one is fully asserted at `1.0` — the fleet's
    /// analog pulse is [`Self::activity`], which is a property of the tray as a
    /// whole rather than of any one slot.
    pub(crate) fn intensity(&self, signal: FleetSignal) -> f32 {
        f32::from(u8::from(self.is_live(signal)))
    }

    /// How many panes have an agent working in them right now.
    ///
    /// A count, not a lamp, and that is the point: the eight signals answer
    /// *what kind of thing* is outstanding, and this answers *how much*. It is
    /// the one reading in this roll-up that is deliberately not an action item
    /// — nobody owns a running agent and it clears itself — which is exactly
    /// why it is a number on the pulse row rather than a ninth signal.
    pub(crate) fn running(&self) -> usize {
        self.running
    }

    /// How many panes have something outstanding for the captain.
    ///
    /// The first four signals — `ask`, `review`, `report`, `stopped` — are the
    /// fleet waiting on the captain, and this is that same condition counted
    /// over panes instead of collapsed to four booleans. A pane in two of those
    /// conditions at once is one thing waiting, so this is always at most the
    /// number of panes. Not the complement of [`Self::running`]: a worker can
    /// leave a report standing and carry on working, and that report is still
    /// waiting to be read.
    pub(crate) fn awaiting(&self) -> usize {
        self.awaiting
    }

    /// The account's 5-hour quota window, as a percentage used.
    ///
    /// `None` when no publisher has written [`crate::quota::SESSION_TOKEN`],
    /// which is the normal state of a Herdr nobody has wired a quota reporter
    /// into. A reader that has never been published is absent, never zero:
    /// "no reading" and "none used" are opposite facts.
    pub(crate) fn quota_percent(&self) -> Option<f64> {
        self.quota_percent
    }

    /// How hard the fleet is working, in `0.0..=1.0`.
    ///
    /// The busiest pane's smoothed work volume. This used to be a ninth thing
    /// in the list, called `busy`, and it does not belong there: nobody owns
    /// it, it clears itself, and it points at every pane at once rather than
    /// at one. It is a genuinely good ambient reading all the same, so the tray
    /// takes it as its own tint rather than as one of the eight slots.
    pub(crate) fn activity(&self) -> f32 {
        self.fleet_activity.clamp(0.0, 1.0)
    }

    /// The signals that are live, as animation elements with their live drives.
    ///
    /// This is the membership set the engine reconciles against: an alert
    /// clearing simply drops out of it and the element leaves on its own, so no
    /// call site has to remember to retire anything.
    pub(crate) fn animation_membership(
        &self,
    ) -> impl Iterator<Item = (crate::anim::ElementId, crate::anim::behaviour::DriveInputs)> + '_
    {
        FleetSignal::ALL
            .into_iter()
            .filter(|signal| self.is_live(*signal))
            .map(|signal| {
                (
                    signal.element_id(),
                    crate::anim::behaviour::DriveInputs {
                        activity: self.intensity(signal),
                    },
                )
            })
    }
}

fn index(signal: FleetSignal) -> usize {
    FleetSignal::ALL
        .iter()
        .position(|candidate| *candidate == signal)
        .unwrap_or(0)
}

/// The busiest pane in the whole fleet, in `0.0..=1.0`.
///
/// The busiest rather than the mean, for the same reason a Space row rolls up
/// the same way: the tray stands for whatever is happening anywhere, and
/// averaging one working pane against nine idle ones would report a quiet fleet
/// for a session that is plainly busy.
fn fleet_activity(app: &AppState) -> f32 {
    app.workspaces
        .iter()
        .map(|workspace| app.workspace_activity_level(workspace))
        .fold(0.0, f32::max)
}

/// Which background refreshes a configured bar has to keep armed.
///
/// The Git scan and the forge fetch are demand-gated on something actually
/// rendering their counts, so a bar that draws `dirty`, `push` or `pr` has to
/// declare that demand the same way a configured sidebar token does — otherwise
/// the surface would render slots that could never light up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FleetSignalDemand {
    pub(crate) git_dirty: bool,
    pub(crate) git_ahead_behind: bool,
    pub(crate) pull_requests: bool,
}

impl FleetSignalDemand {
    /// What a surface that only *reads* all eight needs.
    ///
    /// Every signal is always drawn, so a bar that is on needs every source any
    /// of the eight lights from. Deliberately not the `git status` scan: none
    /// of the eight lights on a dirty tree, and that scan's cost scales with
    /// the size of the checkout, so a readout must not arm it.
    pub(crate) fn for_all_signals() -> Self {
        Self {
            git_dirty: false,
            git_ahead_behind: FleetSignal::ALL.iter().any(|s| s.needs_git_ahead_behind()),
            pull_requests: FleetSignal::ALL.iter().any(|s| s.needs_pull_requests()),
        }
    }

    /// What a surface that can be *acted on* needs, which is strictly more.
    ///
    /// The tray's `sync` refuses on a dirty tree, and a refusal that cannot see
    /// the tree is a refusal that always fires. Nothing else in the eight costs
    /// a source the readout did not already arm.
    pub(crate) fn for_tray() -> Self {
        Self {
            git_dirty: FleetSignal::ALL.iter().any(|s| s.needs_git_dirty()),
            ..Self::for_all_signals()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn the_eight_are_distinct_and_stay_in_one_order() {
        let mut seen = Vec::new();
        for signal in FleetSignal::ALL {
            assert!(
                !seen.contains(&signal),
                "{signal:?} appears twice in the fixed order"
            );
            seen.push(signal);
        }
        assert_eq!(seen.len(), FleetSignal::COUNT);

        // The order is the contract: a reader learns the positions once, so a
        // signal must never move under them. It is also the tray's reading
        // order — the first four are the fleet waiting on you, the last four
        // are the repository waiting on you.
        assert_eq!(
            FleetSignal::ALL.map(FleetSignal::name),
            ["ask", "review", "report", "stopped", "push", "sync", "pr", "checks"]
        );
        assert_eq!(FleetSignal::COUNT, FleetSignal::PER_ROW * 2);
    }

    #[test]
    fn every_signal_says_what_it_means_in_one_line() {
        // The legend is what stops the tray being eight pictures nobody can
        // name, so a slot with no meaning is a slot that cannot ship.
        for signal in FleetSignal::ALL {
            let meaning = signal.meaning();
            assert!(!meaning.trim().is_empty(), "{signal:?} has no meaning line");
            assert!(
                meaning.len() <= 32,
                "{signal:?}'s meaning is {} columns, too wide for the legend",
                meaning.len()
            );
        }
    }

    /// A badge's fallback mark is one column. A mark two cells wide would shift
    /// every slot after it and the tray would stop being a fixed set of
    /// positions.
    #[test]
    fn every_mark_is_one_cell_and_no_two_are_the_same() {
        let mut used: Vec<&'static str> = Vec::new();
        for signal in FleetSignal::ALL {
            let mark = signal.mark();
            assert_eq!(
                mark.width(),
                1,
                "{signal:?} draws {mark:?}, which is not one cell wide"
            );
            assert!(
                !used.contains(&mark),
                "{mark:?} is used twice; a signal must not be told apart by colour alone"
            );
            used.push(mark);
        }
    }

    #[test]
    fn element_keys_are_distinct_so_two_signals_cannot_share_a_life() {
        let mut used: Vec<&'static str> = Vec::new();
        for signal in FleetSignal::ALL {
            assert!(!used.contains(&signal.element_key()));
            used.push(signal.element_key());
        }
    }

    #[test]
    fn a_fresh_fleet_lights_nothing() {
        // Every Git and forge count is `None` before the first refresh lands.
        // A Herdr that has just started must read as quiet rather than as
        // eight alerts nobody can explain.
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();

        let signals = FleetSignals::resolve(&app);
        assert!(!signals.any_live(), "{signals:?} lit up on an empty fleet");
        for signal in FleetSignal::ALL {
            assert_eq!(signals.intensity(signal), 0.0);
        }
    }

    #[test]
    fn a_clean_tree_and_an_empty_queue_are_not_alerts() {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.cached_git_dirty = Some(crate::workspace::GitDirtyCounts::default());
        workspace.cached_git_ahead_behind = Some((0, 0));
        workspace.cached_pull_requests = Some(crate::forge::PullRequestCounts::default());
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();

        assert!(!FleetSignals::resolve(&app).any_live());
    }

    /// The whole reason `pr` was recut. Open pull requests are the steady state
    /// of a working repository; a slot that lights on them is lit forever and
    /// says nothing. What is actionable is a pull request that wants *you*.
    #[test]
    fn open_pull_requests_alone_are_not_an_action_item() {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.cached_pull_requests = Some(crate::forge::PullRequestCounts {
            open: 7,
            draft: 2,
            ..Default::default()
        });
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();

        assert!(!FleetSignals::resolve(&app).is_live(FleetSignal::Pr));
    }

    #[test]
    fn a_pull_request_that_wants_you_lights_pr_either_way_round() {
        for counts in [
            crate::forge::PullRequestCounts {
                open: 1,
                review_requested: 1,
                ..Default::default()
            },
            crate::forge::PullRequestCounts {
                open: 1,
                changes_requested: 1,
                ..Default::default()
            },
        ] {
            let mut app = AppState::test_new();
            let mut workspace = crate::workspace::Workspace::test_new("one");
            workspace.cached_pull_requests = Some(counts);
            app.workspaces = vec![workspace];
            app.ensure_test_terminals();

            let signals = FleetSignals::resolve(&app);
            assert!(
                signals.is_live(FleetSignal::Pr),
                "{counts:?} did not light pr"
            );
            assert_eq!(signals.intensity(FleetSignal::Pr), 1.0);
        }
    }

    #[test]
    fn red_and_unfinished_checks_both_light_the_checks_slot() {
        for counts in [
            crate::forge::PullRequestCounts {
                checks_failing: 1,
                ..Default::default()
            },
            crate::forge::PullRequestCounts {
                checks_pending: 2,
                ..Default::default()
            },
        ] {
            let mut app = AppState::test_new();
            let mut workspace = crate::workspace::Workspace::test_new("one");
            workspace.cached_pull_requests = Some(counts);
            app.workspaces = vec![workspace];
            app.ensure_test_terminals();

            assert!(FleetSignals::resolve(&app).is_live(FleetSignal::Checks));
        }
    }

    /// `push` and `sync` are opposite actions with opposite risk, so they must
    /// never light together off one comparison. A branch that is ahead has work
    /// to publish; a branch that is behind has work to take in.
    #[test]
    fn ahead_and_behind_light_different_slots() {
        let cases = [
            ((3, 0), true, false),
            ((0, 4), false, true),
            ((2, 5), true, true),
            ((0, 0), false, false),
        ];
        for ((ahead, behind), expect_push, expect_sync) in cases {
            let mut app = AppState::test_new();
            let mut workspace = crate::workspace::Workspace::test_new("one");
            workspace.cached_git_ahead_behind = Some((ahead, behind));
            app.workspaces = vec![workspace];
            app.ensure_test_terminals();

            let signals = FleetSignals::resolve(&app);
            assert_eq!(
                signals.is_live(FleetSignal::Push),
                expect_push,
                "push for ahead={ahead} behind={behind}"
            );
            assert_eq!(
                signals.is_live(FleetSignal::Sync),
                expect_sync,
                "sync for ahead={ahead} behind={behind}"
            );
        }
    }

    /// A dirty tree is the precondition for `push`, not a signal of its own: in
    /// a fleet where every crewmate leaves a dirty worktree it would be lit in
    /// every session that is actually being used.
    #[test]
    fn a_dirty_tree_alone_lights_nothing() {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
            staged: 0,
            unstaged: 2,
            untracked: 1,
        });
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();

        assert!(!FleetSignals::resolve(&app).any_live());
    }

    /// Sets the agent state of the first pane of the first workspace, and
    /// returns the signals that follow.
    fn signals_for_pane_state(state: AgentState, seen: bool, owner: Option<&str>) -> FleetSignals {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();

        let pane_id = app.workspaces[0].tabs[0]
            .panes
            .keys()
            .copied()
            .next()
            .expect("test workspace has a pane");
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces[0]
            .tabs
            .get_mut(0)
            .and_then(|tab| tab.panes.get_mut(&pane_id))
            .expect("pane exists")
            .seen = seen;
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal exists");
        terminal.state = state;
        if let Some(owner) = owner {
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([(
                    crate::app::agent_tree::OWNER_TOKEN.to_string(),
                    Some(owner.to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );
        }
        FleetSignals::resolve(&app)
    }

    #[test]
    fn an_agent_that_finished_unwatched_is_work_awaiting_review() {
        assert!(signals_for_pane_state(AgentState::Idle, false, None).is_live(FleetSignal::Review));
        // Seen: the captain has already looked at it, so there is nothing left
        // to review and the slot goes back to grey.
        assert!(!signals_for_pane_state(AgentState::Idle, true, None).is_live(FleetSignal::Review));
    }

    #[test]
    fn a_blocked_agent_is_a_decision_waiting_on_the_captain() {
        assert!(signals_for_pane_state(AgentState::Blocked, true, None).is_live(FleetSignal::Ask));
    }

    /// The distinction the `stopped` slot exists to make: a spare shell is not
    /// a stopped worker, and treating it as one would leave the slot lit in
    /// every session anyone actually uses.
    #[test]
    fn only_an_owned_pane_counts_as_a_stopped_worker() {
        assert!(
            !signals_for_pane_state(AgentState::Unknown, true, None).is_live(FleetSignal::Stopped),
            "an unowned shell is just a shell"
        );
        assert!(
            signals_for_pane_state(AgentState::Unknown, true, Some("mate"))
                .is_live(FleetSignal::Stopped),
            "a worker the fleet launched and that is no longer running an agent is stopped"
        );
    }

    /// The other half of that distinction, and the one an agent *starting* can
    /// break. `AgentState::Unknown` is also what a pane reads as for the three
    /// seconds between process detection claiming an agent and the screen
    /// detector confirming its prompt, so a worker the fleet just launched
    /// spends that window looking exactly like a worker that stopped.
    #[test]
    fn a_worker_that_is_still_starting_is_not_a_stopped_worker() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        let pane_id = *app.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = app.workspaces[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal exists")
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([(
                    crate::app::agent_tree::OWNER_TOKEN.to_string(),
                    Some("mate".to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );

        // The real startup path: process detection claims the agent before any
        // screen evidence exists.
        app.handle_app_event(crate::events::AppEvent::AgentProcessDetected {
            pane_id,
            agent: crate::detect::Agent::Pi,
            observed_at: std::time::Instant::now(),
        });
        assert_eq!(
            app.terminals[&terminal_id].state,
            AgentState::Unknown,
            "process detection must not claim a prompt it has not seen"
        );
        assert!(
            !FleetSignals::resolve(&app).is_live(FleetSignal::Stopped),
            "an agent that is still starting has not stopped"
        );

        // Once the screen confirms the prompt the pane is a normal live agent,
        // and a later exit still reads as stopped.
        app.handle_app_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(crate::detect::Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        assert!(!FleetSignals::resolve(&app).is_live(FleetSignal::Stopped));

        app.handle_app_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: None,
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at: std::time::Instant::now(),
        });
        assert!(
            FleetSignals::resolve(&app).is_live(FleetSignal::Stopped),
            "a worker whose agent exited is still a stopped worker"
        );
    }

    /// The failure mode the startup exclusion could introduce: a worker whose
    /// agent dies before it ever draws a prompt must still light `stopped`,
    /// not disappear behind a window that never closed.
    #[test]
    fn a_worker_that_dies_while_starting_is_still_a_stopped_worker() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        let pane_id = *app.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = app.workspaces[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal exists")
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([(
                    crate::app::agent_tree::OWNER_TOKEN.to_string(),
                    Some("mate".to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );

        app.handle_app_event(crate::events::AppEvent::AgentProcessDetected {
            pane_id,
            agent: crate::detect::Agent::Pi,
            observed_at: std::time::Instant::now(),
        });
        app.handle_app_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: None,
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at: std::time::Instant::now(),
        });

        assert!(
            FleetSignals::resolve(&app).is_live(FleetSignal::Stopped),
            "an agent that never reached its prompt still stopped"
        );
    }

    #[test]
    fn a_published_summary_is_a_standing_report() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        let terminal_id = app
            .terminals
            .keys()
            .next()
            .expect("a terminal exists")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal exists")
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([(
                    crate::app::worker_summary::SUMMARY_TOKEN.to_string(),
                    Some("rebased and pushed".to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );

        assert!(FleetSignals::resolve(&app).is_live(FleetSignal::Report));
    }

    /// The eight are all yes-or-no now. The fleet's analog pulse is carried
    /// apart from them, because it is a property of the tray rather than a
    /// thing anybody can act on.
    #[test]
    fn every_signal_is_a_yes_or_no_and_the_pulse_is_carried_apart() {
        let mut signals = FleetSignals::default();
        signals.set(FleetSignal::Ask);
        signals.fleet_activity = 0.25;

        assert_eq!(signals.intensity(FleetSignal::Ask), 1.0);
        assert_eq!(signals.intensity(FleetSignal::Push), 0.0);
        assert_eq!(signals.activity(), 0.25);
        // A busy fleet with nothing outstanding lights no slot at all.
        assert!(!signals.is_live(FleetSignal::Review));
    }

    /// The pulse is a reading, not a signal: it must never make a slot live.
    #[test]
    fn a_working_fleet_with_nothing_outstanding_lights_no_slot() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();

        let signals = FleetSignals::resolve(&app);
        assert!(!signals.any_live());
    }

    #[test]
    fn only_live_signals_reach_the_animation_membership_set() {
        let mut signals = FleetSignals::default();
        signals.set(FleetSignal::Push);
        signals.set(FleetSignal::Review);

        let membership: Vec<_> = signals.animation_membership().collect();
        assert_eq!(
            membership
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
            vec![
                FleetSignal::Review.element_id(),
                FleetSignal::Push.element_id(),
            ],
            "membership must follow the fixed order, live entries only"
        );
        assert!(membership.iter().all(|(_, inputs)| inputs.activity == 1.0));
    }

    /// The bar draws all eight, so it needs every source any of the eight
    /// lights from. A demand that missed one would render a slot that could
    /// never light up, which is worse than not drawing it.
    #[test]
    fn a_configured_bar_demands_every_source_its_slots_light_from() {
        let demand = FleetSignalDemand::for_all_signals();
        assert!(demand.git_ahead_behind);
        assert!(demand.pull_requests);
        // The `git status` scan costs the whole checkout and lights nothing.
        assert!(!demand.git_dirty);
    }

    /// The tray can be clicked, and `sync` refuses on a dirty tree. A refusal
    /// that cannot see the tree is a refusal that always fires, so the tray
    /// arms the scan the readout deliberately does not.
    #[test]
    fn a_tray_additionally_demands_what_its_refusals_read() {
        let demand = FleetSignalDemand::for_tray();
        assert!(demand.git_dirty);
        assert!(demand.git_ahead_behind);
        assert!(demand.pull_requests);
    }
}
