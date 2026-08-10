//! What a mate has absorbed, and how much of it is still showing.
//!
//! A mate that has taken six finished workers back should not look identical,
//! sitting still, to one that has taken none. The captain's objection to the
//! first sketch of this was that a permanent tally "could get clunky being
//! perm" — a mark that only ever grows is a mark that eventually owns the card.
//! What he approved instead is **decaying concentric rings**: one ring per
//! worker absorbed, each new ring pushing the ones before it further out and
//! dimmer, and the stack capped so the oldest ring falls off the edge rather
//! than the card filling up. The count keeps climbing; the drawing does not.
//!
//! # What "absorbed" means, and why nothing new reports it
//!
//! It is the `completed` relation signal Herdr already accepts — a fleet saying
//! *this worker finished the work it was given* through `workspace.report_signal`
//! ([`crate::app::relation_signal`]). That signal already travels **away** from
//! the worker's row toward the trunk, which is the absorption: the ring is
//! simply what the trunk end keeps once the charge has arrived and expired.
//! No publisher has to send a second thing, and no counter here can disagree
//! with the animation, because both are the same report.
//!
//! Who gets the ring is the *owner* the sidebar tree already derives — the
//! `owner` token, or the structural edge — so the residue is keyed by a tree
//! **name** (`2ndmate-explore`), not by a workspace or pane id. That is
//! deliberate: a mate's Space can be renumbered and a worker's pane replaced,
//! and the ownership handle a fleet publishes outlives both. It is the same
//! handle [`crate::ui::sidebar::space_tree_name`] resolves a Space node with,
//! so a credited ring always lands on a row that exists.
//!
//! # Why this is a count and not a list of timestamps
//!
//! Because the decay is positional, not clocked. Ring *age* is how many workers
//! have been absorbed since — index in the stack — exactly as the approved
//! concept draws it, so the readout is a pure function of one integer and is
//! identical on every client, in every frame, with no clock to be wrong about
//! and nothing to tick. A card at rest stays at rest.
//!
//! Everything here is pure data: no clock, no state of its own beyond the
//! counts, and testable without a PTY or a render pass.

use std::collections::HashMap;

/// Rings the card will draw at once.
///
/// Eight, from the approved concept. It is a ceiling on the *drawing* only —
/// [`Residue::absorbed`] keeps counting past it — and it is what makes the
/// visual bounded: absorption nine pushes absorption one off the outside of the
/// stack instead of adding a ninth contour to a card that has room for eight.
pub(crate) const MAX_RINGS: usize = 8;

/// Opacity of the newest ring.
///
/// The concept's `0.6`, kept as the ceiling rather than raised, because the
/// rings sit *under* the card's own title and chip: this is residue, the
/// quietest thing on the card, and it has to stay readable as a background
/// texture rather than compete with the state the card is actually in.
pub(crate) const RING_ALPHA: f32 = 0.6;

/// How much of that opacity the oldest drawable ring has lost.
///
/// The concept's `0.7`, so a full stack runs from `0.6` down to `0.6 × 0.3875`
/// and the outermost ring is faint but not absent. Fading the last ring all the
/// way to zero would make the eighth absorption invisible and the ninth
/// indistinguishable from it — the cap has to be reached *visibly* for the
/// eviction to read as an eviction.
pub(crate) const RING_FADE: f32 = 0.7;

/// Distinct owners the residue will track.
///
/// The same ceiling [`crate::metadata_tokens::MAX_SEQUENCE_SOURCES`] puts on
/// publisher identities, and here for the same reason: a name arrives from a
/// published token, so an unbounded map is an unbounded allocation a fleet
/// controls. Past the cap a name that is already tracked still counts up — only
/// *new* names are refused — so a steady fleet is never the one that gets cut
/// off.
pub(crate) const MAX_TRACKED_OWNERS: usize = 32;

/// How many finished workers each mate has taken back.
///
/// Keyed by the owner handle the sidebar tree resolves; see the module doc for
/// why that is a name and not an id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Residue {
    by_owner: HashMap<String, u32>,
}

impl Residue {
    /// Credit one absorbed worker to `owner`.
    ///
    /// Returns whether anything changed, so the caller can decide whether the
    /// panel needs redrawing without comparing counts itself. A blank owner or
    /// a new name past [`MAX_TRACKED_OWNERS`] is silently nothing: a signal
    /// that cannot be attributed is not an error, exactly as an unresolvable
    /// carrier is not one in [`crate::app::relation_signal`].
    pub(crate) fn absorb(&mut self, owner: &str) -> bool {
        let owner = owner.trim();
        if owner.is_empty() {
            return false;
        }
        if let Some(count) = self.by_owner.get_mut(owner) {
            // Saturating rather than wrapping: a fleet that has genuinely run
            // four billion workers under one mate should keep a full stack of
            // rings, not roll back to a bare card.
            *count = count.saturating_add(1);
            return true;
        }
        if self.by_owner.len() >= MAX_TRACKED_OWNERS {
            return false;
        }
        self.by_owner.insert(owner.to_string(), 1);
        true
    }

    /// Everything `owner` has absorbed, uncapped.
    pub(crate) fn absorbed(&self, owner: &str) -> u32 {
        self.by_owner.get(owner.trim()).copied().unwrap_or(0)
    }

    /// Rings `owner`'s card draws: the count, capped at [`MAX_RINGS`].
    pub(crate) fn rings(&self, owner: &str) -> u8 {
        rings_for(self.absorbed(owner))
    }

    /// Forget every owner that is no longer a row anybody can see.
    ///
    /// `live` is the set of tree names currently in the panel. Called when the
    /// tree changes rather than on a timer, so the map cannot outgrow the fleet
    /// that is actually running: a mate that is torn down takes its rings with
    /// it, which is also the correct *reading* — residue belongs to a mate, and
    /// a mate that is gone has none.
    pub(crate) fn retain_live<'a>(&mut self, live: impl IntoIterator<Item = &'a str>) -> bool {
        let live: std::collections::HashSet<&str> = live.into_iter().collect();
        let before = self.by_owner.len();
        self.by_owner
            .retain(|owner, _| live.contains(owner.as_str()));
        before != self.by_owner.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_owner.is_empty()
    }
}

/// The drawn stack height for a raw count.
///
/// Split out from [`Residue::rings`] so the cap is one function both the state
/// and the renderer's own tests reach for, and so the ceiling can be asserted
/// without building a map.
pub(crate) fn rings_for(absorbed: u32) -> u8 {
    absorbed.min(MAX_RINGS as u32) as u8
}

/// One ring of the stack, as the card draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ring {
    /// How many absorptions have happened since this one, so `0` is the newest.
    ///
    /// This *is* the decay clock. A ring does not fade with wall time; it fades
    /// because the mate kept working.
    pub(crate) age: u8,
    /// What the ring draws at, in `0.0..=1.0`.
    pub(crate) alpha: f32,
}

/// The stack `rings` deep, newest first.
///
/// The concept's ramp, with one thing inverted from the reference markup and
/// deliberately so: there, opacity falls with *radius index*, which — because
/// the count only ever grows — makes the brightest ring the oldest one. The
/// form the captain approved is "the oldest ring fades first", so age drives
/// the fade here and the newest absorption is the bright one nearest the card.
/// The geometry is the concept's; the direction is the brief's.
pub(crate) fn stack(rings: u8) -> impl Iterator<Item = Ring> {
    (0..rings.min(MAX_RINGS as u8)).map(|age| Ring {
        age,
        alpha: RING_ALPHA * (1.0 - (f32::from(age) / MAX_RINGS as f32) * RING_FADE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absorbing_credits_the_owner_and_nobody_else() {
        let mut residue = Residue::default();
        assert!(residue.absorb("2ndmate-explore"));
        assert!(residue.absorb("2ndmate-explore"));
        assert_eq!(residue.absorbed("2ndmate-explore"), 2);
        assert_eq!(residue.absorbed("2ndmate-build"), 0);
        assert_eq!(residue.rings("2ndmate-build"), 0);
    }

    #[test]
    fn an_owner_is_matched_after_trimming_so_a_padded_token_is_the_same_mate() {
        let mut residue = Residue::default();
        assert!(residue.absorb("  2ndmate-explore \n"));
        assert_eq!(residue.absorbed("2ndmate-explore"), 1);
    }

    #[test]
    fn a_blank_owner_credits_nothing() {
        let mut residue = Residue::default();
        assert!(!residue.absorb("   "));
        assert!(residue.is_empty());
    }

    /// The whole point of the form: the drawing stops growing while the fleet
    /// does not.
    #[test]
    fn the_ring_stack_is_capped_but_the_count_is_not() {
        let mut residue = Residue::default();
        for _ in 0..40 {
            residue.absorb("mate");
        }
        assert_eq!(residue.absorbed("mate"), 40);
        assert_eq!(usize::from(residue.rings("mate")), MAX_RINGS);
        assert_eq!(stack(residue.rings("mate")).count(), MAX_RINGS);
    }

    #[test]
    fn a_new_owner_past_the_cap_is_refused_but_a_tracked_one_still_counts() {
        let mut residue = Residue::default();
        for index in 0..MAX_TRACKED_OWNERS {
            assert!(residue.absorb(&format!("mate-{index}")));
        }
        assert!(!residue.absorb("one-too-many"));
        assert_eq!(residue.absorbed("one-too-many"), 0);
        assert!(residue.absorb("mate-0"));
        assert_eq!(residue.absorbed("mate-0"), 2);
    }

    /// Age is the decay: the newest ring is the bright one, and each older ring
    /// is dimmer than the ring that displaced it.
    #[test]
    fn the_oldest_ring_is_the_faintest_and_the_newest_is_full_strength() {
        let stack: Vec<Ring> = stack(MAX_RINGS as u8).collect();
        assert_eq!(stack[0].age, 0);
        assert!((stack[0].alpha - RING_ALPHA).abs() < f32::EPSILON);
        for pair in stack.windows(2) {
            assert!(
                pair[1].alpha < pair[0].alpha,
                "ring {} must be fainter than ring {}",
                pair[1].age,
                pair[0].age
            );
        }
        // Faint, but never gone: an eighth absorption that drew nothing would
        // be indistinguishable from the ninth, and the eviction would stop
        // reading as one.
        let oldest = stack[MAX_RINGS - 1];
        assert!(oldest.alpha > 0.0);
        assert!(oldest.alpha < RING_ALPHA);
    }

    #[test]
    fn a_mate_that_has_absorbed_nothing_draws_nothing() {
        assert_eq!(stack(0).count(), 0);
        assert_eq!(rings_for(0), 0);
        assert_eq!(rings_for(1), 1);
    }

    #[test]
    fn a_mate_that_is_gone_takes_its_rings_with_it() {
        let mut residue = Residue::default();
        residue.absorb("stays");
        residue.absorb("goes");
        assert!(residue.retain_live(["stays"]));
        assert_eq!(residue.absorbed("stays"), 1);
        assert_eq!(residue.absorbed("goes"), 0);
        // Idempotent: a tree that has not changed must not report a change.
        assert!(!residue.retain_live(["stays"]));
    }
}
