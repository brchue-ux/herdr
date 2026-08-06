//! The cell-grid animation engine.
//!
//! A visual element — a sidebar row, a pane, a status glyph, a notification —
//! has a life: it arrives, it sits there, it leaves. This module owns that
//! life as an explicit state machine per element, and nothing else. It says
//! *where an element is in its life*; [`behaviour`] says what any given named
//! behaviour looks like; [`cell`] says what one cell can express. Keeping the
//! three apart is what lets a new behaviour be a value rather than a branch,
//! and a new kind of element be a variant rather than a render-pipeline change.
//!
//! The medium is a character-cell grid. Full per-cell control of TrueColor
//! foreground and background, per-cell attributes, and per-cell coverage; no
//! per-character motion, because a glyph cannot leave its cell. That is a
//! property of terminals, not a missing feature here.
//!
//! Five properties this module is responsible for holding:
//!
//! - **An element's life is driven by membership, not by bookkeeping.** A
//!   caller publishes the set of elements that exist right now
//!   ([`Animator::observe`], the same shape [`crate::app::pane_activity`] uses)
//!   and arrivals and departures fall out of it. Nothing has to remember to
//!   call a teardown, so nothing can leak a half-finished animation.
//! - **Rate changes bend a loop; they never jump it.** The idle phase
//!   accumulates turns rather than dividing elapsed time by a period, so a
//!   behaviour whose speed follows a live metric speeds up and slows down
//!   smoothly instead of snapping to a new phase every time the metric moves.
//!   This is the whole reason the engine holds state at all.
//! - **Drawing is a pure read.** [`Animator::frame`] takes `&self` and reads no
//!   clock, so a render pass can ask as often as it likes and every ask agrees.
//!   All clock work happens in [`Animator::advance`], on the app loop.
//! - **A frame that no cell could show is not a frame.** `advance` reports a
//!   change only when a *quantized* element state moved, so the loop is never
//!   woken to repaint a difference below the resolution of a terminal cell.
//! - **An idle Herdr arms no deadline.** [`Animator::next_deadline`] is `None`
//!   once nothing is animating, and each behaviour declares its own frame
//!   spacing, so the cost of the engine is exactly the cost of what is
//!   configured to move.

pub(crate) mod behaviour;
pub(crate) mod cell;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use behaviour::{Behaviour, Catalogue, DriveInputs};
use cell::{CellExtent, CellPaint, CellPos, InkPalette};

/// Cap on elements the engine will track at once.
///
/// Far above any real sidebar or pane count; this exists so a caller that
/// publishes a runaway membership set degrades by refusing new elements rather
/// than by growing without bound.
const MAX_ELEMENTS: usize = 512;

/// Quantization of an element's published position, in steps per unit.
///
/// The diff granularity: anything finer than this is a difference no sequence
/// of cells could render differently, so reporting it would spend a frame on
/// nothing.
const POSITION_STEPS: f32 = 512.0;

/// What an animated element is.
///
/// A new kind of element costs one variant here and nothing in the render
/// pipeline. The variant is also the element's *family*, which is what lets
/// several subsystems share one engine: reconciling sidebar rows can never
/// retire a notification, because they are not the same family.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ElementId {
    /// One workspace's row in the sidebar tree, by canonical workspace id.
    WorkspaceRow(String),
    /// One owned agent pane's row in the sidebar tree, by pane id.
    ///
    /// A separate family from [`Self::WorkspaceRow`] because the two membership
    /// sets move at completely different rates: Spaces are long-lived, while a
    /// worker's row arrives and leaves within one piece of work. Reconciling
    /// one must never retire the other.
    AgentRow(crate::layout::PaneId),
    /// One terminal's own surface, by the id the work-volume sampler keys on.
    Terminal(crate::terminal::TerminalId),
    /// The sidebar tree's own view, as a whole.
    ///
    /// A family of exactly one, and deliberately not [`Self::Named`]: a
    /// subsystem that reconciles `Named` by membership — the fleet signal bar
    /// does — would retire any other named element it did not publish, which
    /// mid-switch means the incoming view is told to leave the moment it
    /// arrives. Driven by [`Animator::enter`]/[`Animator::leave`] rather than
    /// by a membership set, because there is nothing to enumerate.
    TreeView,
    /// One badge in the notification tray, by the signal it stands for.
    ///
    /// Its own family rather than a [`Self::Named`] element, and that is not
    /// tidiness. The fleet signal bar reconciles `Named` against the signals
    /// that are *live*, so a tray badge published as `Named` would be retired
    /// the moment the bar's own pass ran — and a resting badge, which is most
    /// of them most of the time, would never survive a single frame. The tray
    /// publishes all eight always, because rest is one of the three things a
    /// badge has to be able to say.
    TrayBadge(crate::app::fleet_signals::FleetSignal),
    /// A singleton surface a subsystem names for itself — a notification bar,
    /// an overlay.
    Named(&'static str),
}

/// Which membership set an element belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    WorkspaceRow,
    AgentRow,
    Terminal,
    TreeView,
    TrayBadge,
    Named,
}

impl ElementId {
    pub(crate) fn family(&self) -> Family {
        match self {
            Self::WorkspaceRow(_) => Family::WorkspaceRow,
            Self::AgentRow(_) => Family::AgentRow,
            Self::Terminal(_) => Family::Terminal,
            Self::TreeView => Family::TreeView,
            Self::TrayBadge(_) => Family::TrayBadge,
            Self::Named(_) => Family::Named,
        }
    }

    pub(crate) fn workspace_row(workspace_id: &str) -> Self {
        Self::WorkspaceRow(workspace_id.to_string())
    }

    pub(crate) fn agent_row(pane_id: crate::layout::PaneId) -> Self {
        Self::AgentRow(pane_id)
    }
}

/// Where an element is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Arriving. Bounded by the mount stage's own duration.
    Mount,
    /// Present. Unbounded, and the only phase that loops.
    Idle,
    /// Leaving. Bounded, and the element is still drawn throughout — a renderer
    /// that wants exit animations reads [`Animator::retiring`].
    Dismount,
    /// Gone. Held for exactly as long as it takes the next advance to drop it,
    /// so no consumer can observe a resurrection.
    Retired,
}

impl Phase {
    /// True when this phase runs to a deadline rather than forever.
    fn is_bounded(self) -> bool {
        matches!(self, Self::Mount | Self::Dismount)
    }
}

/// One bounded phase: what to play, and for how long.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stage {
    /// Name of the behaviour to play, resolved against the engine's catalogue.
    /// An unregistered name plays nothing; it is never an error, because a
    /// missing decoration must never be able to break the thing it decorates.
    pub(crate) behaviour: String,
    pub(crate) duration: Duration,
}

impl Stage {
    pub(crate) fn new(behaviour: impl Into<String>, duration: Duration) -> Self {
        Self {
            behaviour: behaviour.into(),
            duration: duration.max(Duration::from_millis(1)),
        }
    }
}

/// The behaviours an element plays through its life.
///
/// Every phase is optional. A lifecycle with nothing in it is legal and costs
/// nothing: the element exists, holds still, and arms no deadline.
///
/// `idle` is a *list* rather than one name because one element is routinely
/// drawn several ways at once — a sidebar row whose state icon shimmers while
/// its branch name pulses is one row, one arrival, two steady behaviours. Each
/// declared behaviour gets its own accumulated phase, which is what stops two
/// behaviours with different periods, or different live rate drives, from being
/// forced to run at each other's tempo. Declaring them up front is also what
/// lets [`Animator::advance`] accumulate for a behaviour that is only named
/// later, at draw time.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Lifecycle {
    pub(crate) mount: Option<Stage>,
    /// Every steady behaviour this element can be asked to draw. A name the
    /// caller later asks for that is not in here simply does not play — the
    /// element has no phase for it, and freezing one mid-effect would look far
    /// more broken than leaving it settled.
    pub(crate) idle: Vec<String>,
    pub(crate) dismount: Option<Stage>,
}

impl Lifecycle {
    /// The still lifecycle: an element that is tracked but never animates.
    pub(crate) fn still() -> Self {
        Self::default()
    }

    pub(crate) fn with_idle(mut self, behaviour: impl Into<String>) -> Self {
        let behaviour = behaviour.into();
        if !self.idle.contains(&behaviour) {
            self.idle.push(behaviour);
        }
        self
    }

    pub(crate) fn with_mount(mut self, stage: Stage) -> Self {
        self.mount = Some(stage);
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_dismount(mut self, stage: Stage) -> Self {
        self.dismount = Some(stage);
        self
    }

    fn stage(&self, phase: Phase) -> Option<&Stage> {
        match phase {
            Phase::Mount => self.mount.as_ref(),
            Phase::Dismount => self.dismount.as_ref(),
            Phase::Idle | Phase::Retired => None,
        }
    }

    /// Which declared idle behaviour a caller's request resolves to.
    ///
    /// No override means the first declared behaviour, so an element with
    /// exactly one — the common case — needs no name at the call site.
    fn idle_slot(&self, requested: Option<&str>) -> Option<(usize, &str)> {
        let index = match requested {
            None => 0,
            Some(name) => self.idle.iter().position(|declared| declared == name)?,
        };
        self.idle.get(index).map(|name| (index, name.as_str()))
    }
}

/// What an element resolves to right now.
///
/// Borrowed from the engine rather than copied, so asking is free and a render
/// pass can ask per element without allocating.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ElementFrame<'a> {
    pub(crate) phase: Phase,
    /// The behaviour being played, or `None` when this phase has none — a
    /// still element, or a name that is not registered.
    pub(crate) behaviour: Option<&'a Behaviour>,
    /// `0.0..=1.0` through a bounded phase; accumulated whole turns through the
    /// unbounded idle phase, which [`Behaviour::cell`] wraps.
    ///
    /// A dismount counts *down*: it is its mount played backwards, so it starts
    /// at `1.0` and reaches `0.0` as the element goes. A caller reads the same
    /// number either way and never has to know which direction it is in.
    pub(crate) progress: f32,
    /// The live signals this element last published.
    pub(crate) inputs: DriveInputs,
}

impl ElementFrame<'_> {
    /// What this element does to one of its cells, this frame.
    ///
    /// A frame with no behaviour resolves to a paint that changes nothing, so a
    /// caller never has to branch on whether an animation is configured.
    pub(crate) fn cell(&self, pos: CellPos, extent: CellExtent, palette: InkPalette) -> CellPaint {
        match self.behaviour {
            None => CellPaint::default(),
            Some(behaviour) => behaviour.cell(pos, extent, self.progress, self.inputs, palette),
        }
    }

    /// True when every cell of this element resolves identically, so a caller
    /// may resolve one cell and style a whole span with it.
    pub(crate) fn is_uniform(&self) -> bool {
        self.behaviour.is_none_or(Behaviour::is_uniform)
    }
}

#[derive(Debug, Clone)]
struct Element {
    lifecycle: Lifecycle,
    phase: Phase,
    /// When the current phase began.
    entered_at: Instant,
    /// How far through a bounded phase this element is, in `0.0..=1.0`.
    ///
    /// Recomputed by [`Animator::advance`] rather than derived from the clock
    /// on demand, which is what makes [`Animator::frame`] a pure read: two
    /// consumers asking in the same frame can never be told different things.
    progress: f32,
    /// Accumulated turns through each declared idle behaviour's own loop, in
    /// the order [`Lifecycle::idle`] declares them.
    ///
    /// Integrated rather than derived from elapsed time so a changing rate
    /// bends the loop instead of jumping it — see this module's header. One per
    /// behaviour so two behaviours on the same element keep their own periods
    /// and their own live rates.
    cycles: Vec<f32>,
    inputs: DriveInputs,
    /// Quantized position last published, for the per-frame diff.
    position: u32,
}

/// The engine: every element's life, plus the behaviours they can play.
#[derive(Debug)]
pub(crate) struct Animator {
    catalogue: Catalogue,
    elements: HashMap<ElementId, Element>,
    last_advanced_at: Option<Instant>,
    /// Floor under every behaviour's declared frame interval.
    ///
    /// A host that cannot afford a behaviour's natural cadence — a server
    /// drawing for a remote client over a socket — raises this and gets the
    /// same animations at a coarser step. It never changes a behaviour's
    /// *period*, so nothing runs at a different speed there, only at a
    /// different resolution.
    frame_floor: Duration,
}

impl Default for Animator {
    fn default() -> Self {
        Self {
            catalogue: Catalogue::built_in(),
            elements: HashMap::new(),
            last_advanced_at: None,
            frame_floor: Duration::ZERO,
        }
    }
}

impl Animator {
    pub(crate) fn set_frame_floor(&mut self, floor: Duration) {
        self.frame_floor = floor;
    }

    /// Drop every element without playing anything out.
    ///
    /// For a host that has stopped drawing entirely. The engine holds
    /// presentation state only, so forgetting it loses nothing true — the next
    /// pass that publishes a membership set rebuilds it, and every element
    /// arrives settled rather than replaying an arrival nobody saw.
    ///
    /// Returns whether there was anything to forget.
    pub(crate) fn forget_all(&mut self) -> bool {
        let had_any = !self.elements.is_empty();
        self.elements.clear();
        self.last_advanced_at = None;
        had_any
    }

    /// The behaviour catalogue, for a subsystem registering its own.
    pub(crate) fn catalogue_mut(&mut self) -> &mut Catalogue {
        &mut self.catalogue
    }

    pub(crate) fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    /// Bring one element into existence, or update the live signals of one that
    /// already exists.
    ///
    /// For elements that do not come from a membership set — a notification
    /// bar, a view transition, anything singular. `lifecycle` is read only when
    /// the element is new; an element already mid-flight keeps the life it
    /// entered with, so a repeated call can never restart an animation.
    pub(crate) fn enter(
        &mut self,
        id: ElementId,
        lifecycle: &Lifecycle,
        inputs: DriveInputs,
        now: Instant,
    ) {
        self.admit(id, lifecycle, inputs, now);
    }

    /// Ask an element to leave.
    ///
    /// It enters its dismount phase and is dropped when that finishes, or
    /// immediately when it has no dismount stage. An element already leaving is
    /// left alone rather than restarted.
    pub(crate) fn leave(&mut self, id: &ElementId, now: Instant) {
        if let Some(element) = self.elements.get_mut(id) {
            begin_departure(element, now);
        }
    }

    /// Publish the set of elements in one family that exist right now.
    ///
    /// Arrivals mount, departures dismount, and everything else keeps the life
    /// it already had. Only this family is reconciled, so subsystems sharing
    /// one engine cannot retire each other's elements.
    ///
    /// `lifecycle` is used for elements this call brings into existence.
    /// Returns whether anything a renderer draws changed.
    pub(crate) fn observe<I>(
        &mut self,
        now: Instant,
        family: Family,
        lifecycle: &Lifecycle,
        live: I,
    ) -> bool
    where
        I: IntoIterator<Item = (ElementId, DriveInputs)>,
    {
        let mut seen: Vec<ElementId> = Vec::new();
        let mut changed = false;
        for (id, inputs) in live {
            debug_assert_eq!(id.family(), family, "observe was handed a foreign family");
            seen.push(id.clone());
            changed |= self.admit(id, lifecycle, inputs, now);
        }
        for (id, element) in &mut self.elements {
            if id.family() == family && !seen.contains(id) {
                changed |= begin_departure(element, now);
            }
        }
        self.advance(now) || changed
    }

    /// Returns whether this call changed the element's phase, which is a change
    /// a renderer can see even when the position has not moved yet.
    fn admit(
        &mut self,
        id: ElementId,
        lifecycle: &Lifecycle,
        inputs: DriveInputs,
        now: Instant,
    ) -> bool {
        match self.elements.get_mut(&id) {
            Some(element) => {
                element.inputs = inputs;
                // An element that reappears while it was leaving arrives again
                // rather than resuming: it really did go away, and animating it
                // back from where the exit had reached would be animating
                // history.
                if matches!(element.phase, Phase::Dismount | Phase::Retired) {
                    element.phase = opening_phase(&element.lifecycle);
                    element.entered_at = now;
                    element.progress = 0.0;
                    element.cycles.fill(0.0);
                    return true;
                }
                false
            }
            None => {
                if self.elements.len() >= MAX_ELEMENTS {
                    return false;
                }
                self.elements.insert(
                    id,
                    Element {
                        phase: opening_phase(lifecycle),
                        cycles: vec![0.0; lifecycle.idle.len()],
                        lifecycle: lifecycle.clone(),
                        entered_at: now,
                        progress: 0.0,
                        inputs,
                        position: 0,
                    },
                );
                true
            }
        }
    }

    /// Move every element to where it is due at `now`, and drop the ones that
    /// have finished leaving.
    ///
    /// Runs on every loop pass, not only while something is being drawn, so a
    /// dismount always completes and an element can never strand mid-exit.
    /// Returns whether any element's published position changed.
    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let elapsed = self
            .last_advanced_at
            .and_then(|last| now.checked_duration_since(last))
            .unwrap_or_default();
        self.last_advanced_at = Some(now);

        let mut changed = false;
        for element in self.elements.values_mut() {
            let before = element.position;

            // Bounded phases end on their own deadline; the idle phase never
            // does.
            while element.phase.is_bounded() {
                let Some(stage) = element.lifecycle.stage(element.phase) else {
                    break;
                };
                let Some(over) = now
                    .checked_duration_since(element.entered_at)
                    .and_then(|since| since.checked_sub(stage.duration))
                else {
                    break;
                };
                element.phase = match element.phase {
                    Phase::Mount => Phase::Idle,
                    _ => Phase::Retired,
                };
                // Credit the overshoot to the phase being entered rather than
                // discarding it, so an irregular loop cadence does not slow the
                // whole life down.
                element.entered_at = now.checked_sub(over).unwrap_or(now);
                element.progress = 0.0;
            }

            element.progress = match element.lifecycle.stage(element.phase) {
                Some(stage) => (now
                    .checked_duration_since(element.entered_at)
                    .unwrap_or_default()
                    .min(stage.duration)
                    .as_secs_f32()
                    / stage.duration.as_secs_f32())
                .clamp(0.0, 1.0),
                None => 0.0,
            };

            if element.phase == Phase::Idle {
                for (index, name) in element.lifecycle.idle.iter().enumerate() {
                    let Some(behaviour) = self.catalogue.get(name) else {
                        continue;
                    };
                    let Some(cycles) = element.cycles.get_mut(index) else {
                        continue;
                    };
                    let period = behaviour.effective_period(element.inputs);
                    // Kept inside one turn so a long-lived element never loses
                    // resolution to floating-point magnitude.
                    *cycles =
                        (*cycles + elapsed.as_secs_f32() / period.as_secs_f32()).rem_euclid(1.0);
                }
            }

            element.position = quantize_position(element);
            changed |= element.position != before;
        }

        let before = self.elements.len();
        self.elements
            .retain(|_, element| element.phase != Phase::Retired);
        changed || self.elements.len() != before
    }

    /// What `id` resolves to right now.
    ///
    /// `idle_override` replaces the element's own idle behaviour — this is how
    /// a call site that knows what it is drawing (a sidebar token with its own
    /// configured emphasis) reuses an element's life clock without owning it.
    /// Mount and dismount always win: a token cannot keep shimmering while the
    /// row under it is still arriving.
    pub(crate) fn frame(
        &self,
        id: &ElementId,
        idle_override: Option<&str>,
    ) -> Option<ElementFrame<'_>> {
        let element = self.elements.get(id)?;
        let (behaviour, progress) = match element.lifecycle.stage(element.phase) {
            // Leaving is arriving played backwards. Reversing here rather than
            // at a call site is what lets *any* behaviour serve as an exit
            // without a second, mirror-image entry in the catalogue, and it
            // means every consumer of a dismount agrees about which way the
            // effect runs. The curve reverses with it, which is what a rewind
            // should do: an ease-out arrival leaves on an ease-in.
            Some(stage) => (
                self.catalogue.get(&stage.behaviour),
                match element.phase {
                    Phase::Dismount => 1.0 - element.progress,
                    _ => element.progress,
                },
            ),
            None if element.phase == Phase::Idle => {
                match element.lifecycle.idle_slot(idle_override) {
                    Some((index, name)) => (
                        self.catalogue.get(name),
                        element.cycles.get(index).copied().unwrap_or(0.0),
                    ),
                    None => (None, 0.0),
                }
            }
            None => (None, 1.0),
        };
        Some(ElementFrame {
            phase: element.phase,
            behaviour,
            progress,
            inputs: element.inputs,
        })
    }

    /// Elements that are leaving but still have frames to draw.
    ///
    /// A renderer only sees an exit animation if it draws these; a renderer
    /// that does not simply gets an element that disappears at once, which is
    /// what every renderer does today.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn retiring(&self) -> impl Iterator<Item = &ElementId> {
        self.elements
            .iter()
            .filter(|(_, element)| element.phase == Phase::Dismount)
            .map(|(id, _)| id)
    }

    /// When the engine next needs the loop to wake.
    ///
    /// `None` when nothing is animating, so a Herdr with no configured
    /// animation arms no deadline at all. The spacing is the finest any live
    /// element asked for, never finer.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let interval = self
            .elements
            .values()
            .filter_map(|element| self.frame_interval(element))
            .min()?;
        let from = self.last_advanced_at.unwrap_or(now);
        Some(from.checked_add(interval).unwrap_or(now).max(now))
    }

    /// True when at least one element has something left to animate.
    pub(crate) fn is_animating(&self) -> bool {
        self.elements
            .values()
            .any(|element| self.frame_interval(element).is_some())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// How often this element needs a frame, or `None` when it is holding still.
    ///
    /// The finest any of its declared behaviours asked for: an element drawn
    /// two ways at once has to satisfy the smoother of them.
    fn frame_interval(&self, element: &Element) -> Option<Duration> {
        let interval = match element.lifecycle.stage(element.phase) {
            Some(stage) => self
                .catalogue
                .get(&stage.behaviour)
                .map(|behaviour| behaviour.frame_interval)
                // A bounded phase still has to end even when its behaviour is
                // missing, or an element with an unregistered mount name would
                // sit in Mount forever.
                .or(Some(crate::app::ANIMATION_INTERVAL)),
            None if element.phase == Phase::Idle => element
                .lifecycle
                .idle
                .iter()
                .filter_map(|name| self.catalogue.get(name))
                .map(|behaviour| behaviour.frame_interval)
                .min(),
            None => None,
        };
        interval.map(|interval| interval.max(self.frame_floor))
    }
}

/// The phase a newly arrived element opens in.
fn opening_phase(lifecycle: &Lifecycle) -> Phase {
    if lifecycle.mount.is_some() {
        Phase::Mount
    } else {
        Phase::Idle
    }
}

/// Returns whether this actually started a departure, so a caller can tell a
/// real change from a repeat.
fn begin_departure(element: &mut Element, now: Instant) -> bool {
    if matches!(element.phase, Phase::Dismount | Phase::Retired) {
        return false;
    }
    if element.lifecycle.dismount.is_some() {
        element.phase = Phase::Dismount;
        element.entered_at = now;
        element.progress = 0.0;
    } else {
        element.phase = Phase::Retired;
    }
    true
}

/// The element's position, quantized to the finest step a cell could show.
fn quantize_position(element: &Element) -> u32 {
    let phase_bits = match element.phase {
        Phase::Mount => 0u32,
        Phase::Idle => 1,
        Phase::Dismount => 2,
        Phase::Retired => 3,
    };
    if element.phase.is_bounded() {
        return step(element.progress) << 2 | phase_bits;
    }
    // Every declared behaviour folds in, not just the first: an element drawn
    // two ways at once must repaint when *either* of them moves, or the second
    // one silently stops animating.
    let cycles = element
        .cycles
        .iter()
        .fold(0u32, |acc, cycle| acc.rotate_left(9) ^ step(*cycle));
    cycles << 2 | phase_bits
}

fn step(position: f32) -> u32 {
    (position.clamp(0.0, 1.0) * POSITION_STEPS) as u32
}

#[cfg(test)]
mod tests {
    use super::behaviour::names;
    use super::*;

    const PALETTE: InkPalette = InkPalette {
        surface: (0, 0, 0),
        own: (200, 200, 200),
        accent: (0, 0, 255),
        signal: (0, 0, 255),
    };

    fn row(id: &str) -> ElementId {
        ElementId::workspace_row(id)
    }

    fn quiet(ids: &[&str]) -> Vec<(ElementId, DriveInputs)> {
        ids.iter()
            .map(|id| (row(id), DriveInputs::default()))
            .collect()
    }

    fn mounting() -> Lifecycle {
        Lifecycle::still()
            .with_mount(Stage::new(names::WIPE, Duration::from_millis(400)))
            .with_idle(names::PULSE)
            .with_idle(names::SHIMMER)
    }

    #[test]
    fn membership_is_what_makes_an_element_arrive_and_leave() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = mounting();

        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a", "b"]));
        assert_eq!(
            anim.frame(&row("a"), None).expect("live").phase,
            Phase::Mount
        );
        assert_eq!(
            anim.frame(&row("b"), None).expect("live").phase,
            Phase::Mount
        );

        // Past the mount duration both settle into idle without anyone saying so.
        let settled = now + Duration::from_millis(500);
        anim.observe(settled, Family::WorkspaceRow, &life, quiet(&["a", "b"]));
        assert_eq!(
            anim.frame(&row("a"), None).expect("live").phase,
            Phase::Idle
        );

        // Dropping one from the set is what retires it. With no dismount stage
        // it goes at once.
        anim.observe(
            settled + Duration::from_millis(10),
            Family::WorkspaceRow,
            &life,
            quiet(&["a"]),
        );
        assert!(anim.frame(&row("b"), None).is_none());
        assert!(anim.frame(&row("a"), None).is_some());
    }

    #[test]
    fn a_dismount_stage_keeps_an_element_drawable_while_it_leaves() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = mounting().with_dismount(Stage::new(names::FADE, Duration::from_millis(300)));

        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));
        anim.observe(
            now + Duration::from_millis(500),
            Family::WorkspaceRow,
            &life,
            quiet(&["a"]),
        );

        let leaving = now + Duration::from_millis(600);
        anim.observe(leaving, Family::WorkspaceRow, &life, quiet(&[]));
        let frame = anim.frame(&row("a"), None).expect("still drawable");
        assert_eq!(frame.phase, Phase::Dismount);
        assert_eq!(anim.retiring().count(), 1);

        // And it really goes when the exit finishes, whether or not anyone drew
        // a single frame of it.
        anim.advance(leaving + Duration::from_millis(400));
        assert!(anim.frame(&row("a"), None).is_none());
        assert!(anim.is_empty());
    }

    #[test]
    fn an_exit_is_its_arrival_played_backwards() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = Lifecycle::still()
            .with_mount(Stage::new(names::WIPE, Duration::from_millis(400)))
            .with_dismount(Stage::new(names::WIPE, Duration::from_millis(400)));

        // A quarter of the way into the arrival.
        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));
        anim.advance(now + Duration::from_millis(100));
        let arriving = anim.frame(&row("a"), None).expect("arriving").progress;

        let settled = now + Duration::from_millis(500);
        anim.observe(settled, Family::WorkspaceRow, &life, quiet(&["a"]));
        anim.observe(settled, Family::WorkspaceRow, &life, quiet(&[]));
        // Three quarters of the way into the departure is the same picture: a
        // row leaves through the states it arrived through, in reverse.
        anim.advance(settled + Duration::from_millis(300));
        let leaving = anim.frame(&row("a"), None).expect("leaving");
        assert_eq!(leaving.phase, Phase::Dismount);
        assert!(
            (leaving.progress - arriving).abs() < 0.01,
            "a dismount at 3/4 should read like a mount at 1/4, got {} against {arriving}",
            leaving.progress
        );

        // And it ends where the arrival began: fully gone, not fully drawn.
        anim.advance(settled + Duration::from_millis(399));
        let almost = anim.frame(&row("a"), None).expect("still leaving");
        assert!(almost.progress < 0.01, "got {}", almost.progress);
    }

    #[test]
    fn agent_rows_and_space_rows_do_not_retire_each_other() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = mounting();
        let pane = ElementId::agent_row(crate::layout::PaneId::alloc());

        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["space"]));
        anim.observe(
            now,
            Family::AgentRow,
            &life,
            vec![(pane.clone(), DriveInputs::default())],
        );

        // A pass that publishes no agent rows at all must not touch the Space
        // rows, and vice versa: two second mates' groups shrinking is exactly
        // this, one family at a time.
        anim.observe(now, Family::AgentRow, &life, Vec::new());
        assert!(anim.frame(&pane, None).is_none());
        assert!(anim.frame(&row("space"), None).is_some());
    }

    #[test]
    fn an_element_that_comes_back_mid_exit_arrives_again_rather_than_resuming() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = mounting().with_dismount(Stage::new(names::FADE, Duration::from_millis(300)));

        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));
        anim.observe(
            now + Duration::from_millis(500),
            Family::WorkspaceRow,
            &life,
            quiet(&[]),
        );
        assert_eq!(
            anim.frame(&row("a"), None).expect("leaving").phase,
            Phase::Dismount
        );

        anim.observe(
            now + Duration::from_millis(600),
            Family::WorkspaceRow,
            &life,
            quiet(&["a"]),
        );
        let frame = anim.frame(&row("a"), None).expect("back");
        assert_eq!(frame.phase, Phase::Mount);
        assert!(frame.progress < 0.1, "it restarted, not resumed: {frame:?}");
    }

    #[test]
    fn families_reconcile_independently() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = Lifecycle::still().with_idle(names::PULSE);

        anim.enter(
            ElementId::Named("notification-bar"),
            &life,
            DriveInputs::default(),
            now,
        );
        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));

        // Reconciling rows down to nothing must not touch the notification.
        anim.observe(
            now + Duration::from_millis(10),
            Family::WorkspaceRow,
            &life,
            quiet(&[]),
        );
        assert!(anim.frame(&row("a"), None).is_none());
        assert!(anim
            .frame(&ElementId::Named("notification-bar"), None)
            .is_some());

        // And it leaves when it is asked to.
        anim.leave(
            &ElementId::Named("notification-bar"),
            now + Duration::from_millis(20),
        );
        anim.advance(now + Duration::from_millis(30));
        assert!(anim.is_empty());
    }

    #[test]
    fn a_changing_rate_bends_the_loop_instead_of_jumping_it() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = Lifecycle::still().with_idle(names::ACTIVITY);
        let id = row("a");

        let at = |anim: &mut Animator, offset_ms: u64, activity: f32| {
            anim.observe(
                now + Duration::from_millis(offset_ms),
                Family::WorkspaceRow,
                &life,
                [(id.clone(), DriveInputs { activity })],
            );
            anim.frame(&id, None).expect("live").progress
        };

        // Run quietly for a while, then get busy. The phase must move on from
        // where it was, never snap somewhere else.
        at(&mut anim, 0, 0.0);
        let before = at(&mut anim, 400, 0.0);
        let after = at(&mut anim, 450, 1.0);
        let step = (after - before).rem_euclid(1.0);
        assert!(
            step < 0.25,
            "a rate change jumped the phase by {step}: {before} -> {after}"
        );

        // But it genuinely is faster now: the same 50ms covers more ground.
        let quiet_step = {
            let mut calm = Animator::default();
            calm.observe(
                now,
                Family::WorkspaceRow,
                &life,
                [(id.clone(), DriveInputs { activity: 0.0 })],
            );
            let a = at(&mut calm, 400, 0.0);
            let b = at(&mut calm, 450, 0.0);
            (b - a).rem_euclid(1.0)
        };
        assert!(
            step > quiet_step,
            "busier must advance further per frame: {step} vs {quiet_step}"
        );
    }

    #[test]
    fn a_still_engine_arms_no_deadline() {
        let now = Instant::now();
        let mut anim = Animator::default();
        assert_eq!(anim.next_deadline(now), None);
        assert!(!anim.is_animating());

        anim.observe(
            now,
            Family::WorkspaceRow,
            &Lifecycle::still(),
            quiet(&["a"]),
        );
        assert_eq!(
            anim.next_deadline(now),
            None,
            "an element with nothing to play must not wake the loop"
        );
        assert!(anim.frame(&row("a"), None).is_some());
    }

    #[test]
    fn the_deadline_is_the_finest_any_live_element_asked_for() {
        let now = Instant::now();
        let mut anim = Animator::default();

        // `pulse` is the cheap interval; `shimmer` asks for a finer one.
        anim.enter(
            ElementId::Named("slow"),
            &Lifecycle::still().with_idle(names::PULSE),
            DriveInputs::default(),
            now,
        );
        assert_eq!(
            anim.next_deadline(now),
            Some(now + crate::app::ANIMATION_INTERVAL)
        );

        anim.enter(
            ElementId::Named("fast"),
            &Lifecycle::still().with_idle(names::SHIMMER),
            DriveInputs::default(),
            now,
        );
        let deadline = anim.next_deadline(now).expect("armed");
        assert!(deadline < now + crate::app::ANIMATION_INTERVAL);
    }

    #[test]
    fn a_position_no_cell_could_show_is_not_a_frame() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = Lifecycle::still().with_idle(names::PULSE);
        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));

        // A microsecond later the loop has nothing to redraw.
        assert!(
            !anim.advance(now + Duration::from_micros(1)),
            "a sub-step advance must not request a repaint"
        );
        // A whole frame later it does.
        assert!(anim.advance(now + crate::app::ANIMATION_INTERVAL));
    }

    #[test]
    fn an_unregistered_behaviour_name_draws_nothing_and_never_strands_a_phase() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = Lifecycle::still()
            .with_mount(Stage::new("no-such-thing", Duration::from_millis(200)))
            .with_idle("also-not-real");

        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));
        let frame = anim.frame(&row("a"), None).expect("live");
        assert!(frame.behaviour.is_none());
        assert!(frame
            .cell(CellPos::col(0), CellExtent::row(4), PALETTE)
            .is_settled());

        // The mount still ends on schedule, so a typo cannot pin a row in its
        // arrival phase forever.
        anim.advance(now + Duration::from_millis(300));
        assert_eq!(
            anim.frame(&row("a"), None).expect("live").phase,
            Phase::Idle
        );
        // And with no playable idle behaviour the loop goes back to sleep.
        assert_eq!(anim.next_deadline(now + Duration::from_millis(300)), None);
    }

    #[test]
    fn a_call_site_can_override_the_idle_behaviour_but_never_the_arrival() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = mounting();
        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));

        // Mid-mount the row's own arrival wins.
        let mounting_frame = anim.frame(&row("a"), Some(names::SHIMMER)).expect("live");
        assert_eq!(mounting_frame.phase, Phase::Mount);
        assert_eq!(mounting_frame.behaviour, anim.catalogue().get(names::WIPE));

        anim.advance(now + Duration::from_millis(500));
        let idle_frame = anim.frame(&row("a"), Some(names::SHIMMER)).expect("live");
        assert_eq!(idle_frame.phase, Phase::Idle);
        assert_eq!(idle_frame.behaviour, anim.catalogue().get(names::SHIMMER));
        // And with no override it is the first behaviour the row declared.
        assert_eq!(
            anim.frame(&row("a"), None).expect("live").behaviour,
            anim.catalogue().get(names::PULSE)
        );
        // A behaviour the row never declared has no phase of its own, so it is
        // left settled rather than frozen at whatever another one had reached.
        assert!(anim
            .frame(&row("a"), Some(names::WAVE))
            .expect("live")
            .behaviour
            .is_none());
    }

    #[test]
    fn two_behaviours_on_one_element_keep_their_own_periods() {
        let now = Instant::now();
        let mut anim = Animator::default();
        // `pulse` loops over 1600ms and `shimmer` over 1400ms, so after the
        // same elapsed time they must not be at the same place.
        let life = Lifecycle::still()
            .with_idle(names::PULSE)
            .with_idle(names::SHIMMER);
        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));
        anim.advance(now + Duration::from_millis(700));

        let pulse = anim.frame(&row("a"), Some(names::PULSE)).expect("live");
        let shimmer = anim.frame(&row("a"), Some(names::SHIMMER)).expect("live");
        assert!(
            (pulse.progress - shimmer.progress).abs() > 0.01,
            "behaviours with different periods must not share a phase:              {} vs {}",
            pulse.progress,
            shimmer.progress
        );

        // And the element repaints when either of them moves, not only the
        // first: the finer behaviour sets the clock.
        assert!(anim.advance(now + Duration::from_millis(750)));
    }

    #[test]
    fn a_mount_runs_from_nothing_to_fully_arrived() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life =
            Lifecycle::still().with_mount(Stage::new(names::WIPE, Duration::from_millis(400)));
        anim.observe(now, Family::WorkspaceRow, &life, quiet(&["a"]));
        let extent = CellExtent::row(10);

        let coverage_at = |anim: &Animator| -> Vec<f32> {
            let frame = anim.frame(&row("a"), None).expect("live");
            (0..extent.cols)
                .map(|col| frame.cell(CellPos::col(col), extent, PALETTE).coverage)
                .collect()
        };

        let first = coverage_at(&anim);
        assert!(
            first.iter().all(|value| *value == 0.0),
            "a mount must start with nothing drawn: {first:?}"
        );

        anim.advance(now + Duration::from_millis(200));
        let middle = coverage_at(&anim);
        assert!(
            middle.iter().any(|value| *value > 0.0) && middle.iter().any(|value| *value < 1.0),
            "a wipe should be part-way across: {middle:?}"
        );

        // Once idle there is no reveal left: the element is simply itself.
        anim.advance(now + Duration::from_millis(500));
        let settled = coverage_at(&anim);
        assert!(
            settled.iter().all(|value| *value >= 1.0),
            "a settled element must draw normally: {settled:?}"
        );
    }

    #[test]
    fn the_engine_refuses_to_grow_without_bound() {
        let now = Instant::now();
        let mut anim = Animator::default();
        let life = Lifecycle::still().with_idle(names::PULSE);
        let ids: Vec<(ElementId, DriveInputs)> = (0..(MAX_ELEMENTS * 2))
            .map(|index| (row(&format!("w{index}")), DriveInputs::default()))
            .collect();
        anim.observe(now, Family::WorkspaceRow, &life, ids);
        assert_eq!(anim.elements.len(), MAX_ELEMENTS);
    }

    #[test]
    fn a_full_pane_grid_resolves_cheaply() {
        // The size prior work validated as cheap. This is a cost characterization,
        // not a benchmark: what it protects is that resolving a pane-sized
        // element is a plain per-cell loop with no allocation in it.
        let now = Instant::now();
        let mut anim = Animator::default();
        anim.enter(
            ElementId::Named("pane"),
            &Lifecycle::still().with_idle(names::SHIMMER),
            DriveInputs { activity: 0.5 },
            now,
        );
        let frame = anim.frame(&ElementId::Named("pane"), None).expect("live");
        let extent = CellExtent::new(240, 80);
        let mut painted = 0usize;
        for row in 0..extent.rows {
            for col in 0..extent.cols {
                let paint = frame.cell(CellPos::new(col, row), extent, PALETTE);
                painted += usize::from(!paint.is_settled());
            }
        }
        assert_eq!(painted, usize::from(extent.cols) * usize::from(extent.rows));
    }
}
