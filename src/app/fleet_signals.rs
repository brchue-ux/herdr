//! The eight things a fleet can be waiting on, rolled up from state Herdr
//! already holds.
//!
//! Every signal here is a *reading* of an existing source, never a new one.
//! `review`, `ask` and `stopped` read the per-pane agent state the detector
//! already publishes; `report` reads the summary tokens
//! [`crate::app::worker_summary`] groups; `busy` reads the work-volume level
//! [`crate::app::pane_activity`] samples; `dirty`, `push` and `pr` read the
//! counts the background Git and forge refreshes already cache on each
//! [`crate::workspace::Workspace`]. Nothing in this module runs a subprocess,
//! touches the network, or keeps a clock of its own — which is what makes the
//! whole roll-up a pure function of [`AppState`] and testable without a PTY.
//!
//! Three properties this module is responsible for holding:
//!
//! - **The eight are a fixed, ordered set.** [`FleetSignal::ALL`] is the order
//!   they are always read in, so a reader learns the positions once and a
//!   signal never moves under them. Adding a ninth is an explicit edit here,
//!   not something a caller can do by publishing a new name.
//! - **A signal is live or it is not.** [`FleetSignals::is_live`] is a
//!   boolean, and the only continuously varying quantity — how hard the fleet
//!   is working — is carried separately in [`FleetSignals::intensity`]. That
//!   split is what stops a renderer from having to invent a threshold.
//! - **A source that has never answered is quiet, not alarming.** The Git and
//!   forge caches are `None` until their first refresh lands, and `None` reads
//!   as "nothing outstanding" rather than as an alert, so a Herdr that has just
//!   started does not light its own bar up.

use crate::app::state::AppState;
use crate::detect::AgentState;

/// One of the eight things the fleet can be waiting on.
///
/// Ordered by who is being waited on: the three that want the captain come
/// first, then the fleet's own pulse and the worker that has stopped answering,
/// then the three counts of work that is outstanding somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FleetSignal {
    /// An agent finished while the captain was looking somewhere else.
    ///
    /// Herdr's own `done` state: a pane that is idle and whose
    /// [`crate::pane::PaneState::seen`] flag is still false.
    Review,
    /// An agent is blocked on a human answer — [`AgentState::Blocked`].
    Ask,
    /// A worker has published a completion summary that is still standing.
    ///
    /// The same `summary` token family the worker-summary badge counts, so the
    /// bar and the badge can never disagree about whether a report exists.
    Report,
    /// Something in the fleet is producing output right now.
    ///
    /// The only analog signal of the eight: its [`FleetSignals::intensity`] is
    /// the busiest pane's smoothed work volume, which is what lets a
    /// work-volume-driven behaviour breathe at the fleet's own tempo.
    Busy,
    /// A pane the fleet owns is no longer running an agent.
    ///
    /// Owned means a mate published an `owner` token for it, so this is a
    /// worker that was launched to do something and is now back at a bare
    /// prompt — not any shell pane the captain happens to have open.
    Stopped,
    /// Uncommitted work in some checkout.
    Dirty,
    /// Commits that have not moved between a branch and its upstream, in
    /// either direction.
    Push,
    /// Open pull requests on some checkout's remote.
    Pr,
}

impl FleetSignal {
    /// How many signals there are. The bar's whole readability rests on this
    /// being a small fixed number, so it is spelled once here and every width
    /// and every array length is derived from it.
    pub(crate) const COUNT: usize = 8;

    /// Every signal, in the fixed order they are always read in.
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Review,
        Self::Ask,
        Self::Report,
        Self::Busy,
        Self::Stopped,
        Self::Dirty,
        Self::Push,
        Self::Pr,
    ];

    /// The word this signal answers to.
    ///
    /// Short enough that eight of them plus their marks fit a wide sidebar, and
    /// a real word rather than an abbreviation because the resting bar's whole
    /// job is to say what the eight things are.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Ask => "ask",
            Self::Report => "report",
            Self::Busy => "busy",
            Self::Stopped => "stopped",
            Self::Dirty => "dirty",
            Self::Push => "push",
            Self::Pr => "pr",
        }
    }

    /// The one-cell mark that stands in for the name when the panel is narrow.
    ///
    /// Reused from Herdr's own vocabulary wherever one exists — `●` and `◉` and
    /// `◐` are the marks the state dot already draws for done, blocked and
    /// working, and `~` and `↑` are git's own porcelain marks, already drawn by
    /// the `git_dirty` and `git_status` tokens. A reader who knows the tree
    /// already knows most of the bar.
    pub(crate) fn mark(self) -> &'static str {
        match self {
            Self::Review => "●",
            Self::Ask => "◉",
            Self::Report => "≡",
            Self::Busy => "◐",
            Self::Stopped => "⊘",
            Self::Dirty => "~",
            Self::Push => "↑",
            Self::Pr => "⋔",
        }
    }

    /// The animation element this signal owns while it is live.
    ///
    /// `&'static str` because the set is fixed and closed: an element key can
    /// never be composed from anything a pane published, so no amount of fleet
    /// traffic can grow the engine's element table.
    pub(crate) fn element_key(self) -> &'static str {
        match self {
            Self::Review => "fleet-signal.review",
            Self::Ask => "fleet-signal.ask",
            Self::Report => "fleet-signal.report",
            Self::Busy => "fleet-signal.busy",
            Self::Stopped => "fleet-signal.stopped",
            Self::Dirty => "fleet-signal.dirty",
            Self::Push => "fleet-signal.push",
            Self::Pr => "fleet-signal.pr",
        }
    }

    /// The animation element id for this signal.
    pub(crate) fn element_id(self) -> crate::anim::ElementId {
        crate::anim::ElementId::Named(self.element_key())
    }

    /// True when answering this signal costs a `git status` scan.
    fn needs_git_dirty(self) -> bool {
        matches!(self, Self::Dirty)
    }

    /// True when answering this signal costs a read of the branch's upstream.
    fn needs_git_ahead_behind(self) -> bool {
        matches!(self, Self::Push)
    }

    /// True when answering this signal costs a network round trip to the forge.
    fn needs_pull_requests(self) -> bool {
        matches!(self, Self::Pr)
    }
}

/// Which of the eight are live right now, plus the one analog level.
///
/// A plain value rather than a borrow of [`AppState`]: a render pass resolves
/// it once per frame and every consumer of that frame — the layout, the
/// renderer, the animation membership set — reads the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct FleetSignals {
    live: [bool; FleetSignal::COUNT],
    /// Busiest pane's smoothed work volume, in `0.0..=1.0`.
    fleet_activity: f32,
}

impl FleetSignals {
    /// Read all eight out of the app's current state.
    pub(crate) fn resolve(app: &AppState) -> Self {
        let mut signals = Self {
            fleet_activity: fleet_activity(app),
            ..Self::default()
        };

        for workspace in &app.workspaces {
            if workspace.git_dirty().is_some_and(|dirty| !dirty.is_clean()) {
                signals.set(FleetSignal::Dirty);
            }
            if workspace
                .git_ahead_behind()
                .is_some_and(|(ahead, behind)| ahead > 0 || behind > 0)
            {
                signals.set(FleetSignal::Push);
            }
            if workspace
                .pull_requests()
                .is_some_and(|counts| counts.open > 0)
            {
                signals.set(FleetSignal::Pr);
            }

            for pane in workspace.tabs.iter().flat_map(|tab| tab.panes.values()) {
                let Some(terminal) = app.terminals.get(&pane.attached_terminal_id) else {
                    continue;
                };
                let owned = terminal
                    .metadata_tokens
                    .get(crate::app::agent_tree::OWNER_TOKEN)
                    .is_some_and(|owner| !owner.trim().is_empty());

                match terminal.state {
                    AgentState::Blocked => signals.set(FleetSignal::Ask),
                    AgentState::Idle if !pane.seen => signals.set(FleetSignal::Review),
                    // A pane the fleet owns and that is no longer running an
                    // agent is a worker that stopped. An unowned shell is just
                    // a shell, and lighting the bar for one would make the
                    // signal useless in any session with a spare terminal open.
                    AgentState::Unknown if owned => signals.set(FleetSignal::Stopped),
                    _ => {}
                }

                if crate::app::worker_summary::is_summary_line(
                    terminal
                        .metadata_tokens
                        .get(crate::app::worker_summary::SUMMARY_TOKEN),
                ) {
                    signals.set(FleetSignal::Report);
                }
            }
        }

        if signals.fleet_activity > 0.0 {
            signals.set(FleetSignal::Busy);
        }
        signals
    }

    fn set(&mut self, signal: FleetSignal) {
        self.live[index(signal)] = true;
    }

    pub(crate) fn is_live(&self, signal: FleetSignal) -> bool {
        self.live[index(signal)]
    }

    /// True when anything at all is asserting.
    ///
    /// Test-facing only: the bar draws all eight slots whether or not any of
    /// them is live, so nothing in the draw path has a reason to ask.
    #[cfg(test)]
    pub(crate) fn any_live(&self) -> bool {
        self.live.iter().any(|live| *live)
    }

    /// How strongly this signal is asserting itself, in `0.0..=1.0`.
    ///
    /// This is what an animated behaviour's live drive binds to. Seven of the
    /// eight are yes-or-no facts, so a live one is fully asserted at `1.0`;
    /// only [`FleetSignal::Busy`] has a real analog level behind it, and it
    /// reports the busiest pane's own smoothed work volume so the mark breathes
    /// at the tempo the fleet is actually working at.
    pub(crate) fn intensity(&self, signal: FleetSignal) -> f32 {
        if !self.is_live(signal) {
            return 0.0;
        }
        match signal {
            FleetSignal::Busy => self.fleet_activity.clamp(0.0, 1.0),
            _ => 1.0,
        }
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
/// the same way: the bar stands for whatever is happening anywhere, and
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
/// the bar would render three slots that could never light up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FleetSignalDemand {
    pub(crate) git_dirty: bool,
    pub(crate) git_ahead_behind: bool,
    pub(crate) pull_requests: bool,
}

impl FleetSignalDemand {
    /// What the whole bar needs. Every signal is always drawn, so a bar that is
    /// on needs every source that any of the eight reads.
    pub(crate) fn for_all_signals() -> Self {
        Self {
            git_dirty: FleetSignal::ALL.iter().any(|s| s.needs_git_dirty()),
            git_ahead_behind: FleetSignal::ALL.iter().any(|s| s.needs_git_ahead_behind()),
            pull_requests: FleetSignal::ALL.iter().any(|s| s.needs_pull_requests()),
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
        // signal must never move under them.
        assert_eq!(
            FleetSignal::ALL.map(FleetSignal::name),
            ["review", "ask", "report", "busy", "stopped", "dirty", "push", "pr"]
        );
    }

    /// The compact tier draws marks alone, one column each. A mark two cells
    /// wide would shift every slot after it and the bar would stop being a
    /// fixed set of positions.
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

    #[test]
    fn outstanding_git_and_forge_work_light_their_own_slots() {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
            staged: 0,
            unstaged: 2,
            untracked: 0,
        });
        workspace.cached_pull_requests = Some(crate::forge::PullRequestCounts {
            open: 3,
            draft: 1,
            review_requested: 0,
        });
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();

        let signals = FleetSignals::resolve(&app);
        assert!(signals.is_live(FleetSignal::Dirty));
        assert!(signals.is_live(FleetSignal::Pr));
        assert!(!signals.is_live(FleetSignal::Push), "nothing is ahead");
        assert_eq!(signals.intensity(FleetSignal::Dirty), 1.0);
    }

    #[test]
    fn a_branch_behind_its_upstream_counts_as_unmoved_work_too() {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.cached_git_ahead_behind = Some((0, 4));
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();

        assert!(FleetSignals::resolve(&app).is_live(FleetSignal::Push));
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

    /// `busy` is the one signal with a real analog level behind it; the other
    /// seven are yes-or-no and assert fully when they assert at all.
    #[test]
    fn only_busy_reports_a_partial_intensity() {
        let mut signals = FleetSignals::default();
        signals.set(FleetSignal::Busy);
        signals.set(FleetSignal::Ask);
        signals.fleet_activity = 0.25;

        assert_eq!(signals.intensity(FleetSignal::Busy), 0.25);
        assert_eq!(signals.intensity(FleetSignal::Ask), 1.0);
        assert_eq!(signals.intensity(FleetSignal::Dirty), 0.0);
    }

    #[test]
    fn only_live_signals_reach_the_animation_membership_set() {
        let mut signals = FleetSignals::default();
        signals.set(FleetSignal::Dirty);
        signals.set(FleetSignal::Review);

        let membership: Vec<_> = signals.animation_membership().collect();
        assert_eq!(
            membership
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
            vec![
                FleetSignal::Review.element_id(),
                FleetSignal::Dirty.element_id(),
            ],
            "membership must follow the fixed order, live entries only"
        );
        assert!(membership.iter().all(|(_, inputs)| inputs.activity == 1.0));
    }

    /// The bar draws all eight, so it needs every source any of the eight
    /// reads. A demand that missed one would render a slot that could never
    /// light up, which is worse than not drawing it.
    #[test]
    fn a_configured_bar_demands_every_source_its_slots_read() {
        let demand = FleetSignalDemand::for_all_signals();
        assert!(demand.git_dirty);
        assert!(demand.git_ahead_behind);
        assert!(demand.pull_requests);
    }
}
