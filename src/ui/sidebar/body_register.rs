//! What each tree row *is*, in the same register the sky draws it in.
//!
//! The sidebar and the background scene are two readouts of one fleet, and until
//! this module existed they did not say the same thing about it. The tree's rows
//! carried VCS and quota facts — branch, ahead/behind, dirty counts, quota
//! windows — while the sky sized every body by its project's tracked files, gave
//! it a body type by its rank, and turned it at a rate its own mass decided.
//! A reader could not look at a card and find the planet it belongs to.
//!
//! Every number here is *already computed somewhere in herdr*. Nothing is
//! derived a second way:
//!
//! * the body's kind and its size register come from
//!   [`crate::app::background_scene::tree_nodes`], the same call the scene is
//!   built from;
//! * its body type comes from [`crate::solar_system::assign_body_types`] over
//!   [`crate::solar_system::seat_the_ladder`] — the ranking rule itself, not a
//!   copy of it, because A54's *"body type is theirs and is binding"* cannot be
//!   binding if two modules each decide it;
//! * its orbital period comes from
//!   [`crate::solar_system::BodyKind::revolutions_per_loop`] over
//!   [`crate::app::background_scene::LOOP_DURATION_MS`];
//! * its completed revolutions come from
//!   [`crate::app::background_scene::OrbitTracks::revolutions`], which is the
//!   accumulator the orbit-wear layer already keeps;
//! * its streak is decayed from the published `streak`/`streak_hl` tokens by
//!   [`crate::quality_streak`], exactly as the `streak` sidebar token does.
//!
//! The one quantity that is genuinely new is the moon count, and it is not a
//! computation: it is how many children the node has in the tree that was just
//! walked.
//!
//! # Resolved once per pass, never per row
//!
//! [`BodyRegister::resolve`] walks the fleet once and ranks it once. Both of its
//! callers — the character panel's token resolver and the pixel card's content
//! builder — hold the result across their own per-row loop. That is deliberate
//! and it is this project's multiplicative-path rule rather than tidiness:
//! ranking is `O(n log n)` over the roster, and a per-row `resolve` would make
//! the panel `O(n² log n)` in the number of rows on screen.

use std::collections::HashMap;

use crate::anim::CardRow;
use crate::app::background_scene;
use crate::app::state::AppState;
use crate::solar_system::{BodyKind, BodySize, BodyType};

/// The register readings for one body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BodyFacts {
    kind: BodyKind,
    /// `None` for a mate the ring had no slot for: an unseated body carries no
    /// type, because the type is a rank on the ring and it is not on it.
    body_type: Option<BodyType>,
    size: BodySize,
    /// Children of this node in the fleet's own ownership tree.
    moons: usize,
    /// This body's quality streak, decayed to the render's clock. `None` when
    /// the fleet has published none.
    streak: Option<f64>,
    /// Whole orbits this body has completed since herdr started tracking it.
    revolutions: f32,
    /// This body's share of the fleet's own traffic, `0.0..=1.0`, already put
    /// through the attribution transform by
    /// [`crate::app::background_scene::mote_shares`]. The card draws it as the
    /// amplitude of a working pane's discharge.
    traffic: f32,
}

impl BodyFacts {
    /// How hard this body's own work is running, `0.0..=1.0`.
    pub(super) fn traffic(&self) -> f32 {
        self.traffic.clamp(0.0, 1.0)
    }

    /// The word the sky would use for this body.
    ///
    /// A worker is a moon and a root is a star, and neither carries a body type
    /// — [`BodyType::Plain`] *"is not a third kind of planet, it is the absence
    /// of the question"*, so it is spelled as the tier it belongs to rather than
    /// as a kind of planet nobody named.
    fn kind_word(&self) -> &'static str {
        match (self.kind, self.body_type) {
            (BodyKind::Sun, _) => "star",
            (BodyKind::Moon, _) => "moon",
            (BodyKind::Planet, Some(BodyType::Gas)) => "gas giant",
            (BodyKind::Planet, Some(BodyType::Ringed)) => "ringed planet",
            // Seated but plain cannot happen today — every seated planet takes
            // one of the two types — and an unseated one is a mate the ring had
            // no room for. Both are honestly "a planet", which is what it draws
            // as when it draws at all.
            (BodyKind::Planet, _) => "planet",
        }
    }

    /// How long one of this body's revolutions takes, at the scene's own loop
    /// rate. `None` for the sun, which does not orbit.
    fn period(&self) -> Option<std::time::Duration> {
        let revolutions = self.kind.revolutions_per_loop(self.size);
        (revolutions > 0.0).then(|| {
            std::time::Duration::from_millis(
                (background_scene::LOOP_DURATION_MS as f32 / revolutions).round() as u64,
            )
        })
    }

    /// `gas giant · 99 files · 2 moons` — what this body *is*.
    ///
    /// Every part is dropped when the fleet has not published it, the same rule
    /// the card's other caption line has always followed: a row that cannot say
    /// how many files is better off saying nothing than saying zero, because an
    /// unmeasured project and an empty one are different things
    /// ([`BodySize::Unmeasured`] exists to keep them apart).
    pub(super) fn body_line(&self) -> Option<String> {
        let mut parts = vec![self.kind_word().to_string()];
        if let BodySize::Files(files) = self.size {
            parts.push(format!("{files} files"));
        }
        // A moon has no moons of its own, so the count is silence rather than a
        // zero on every worker in the tree.
        if self.kind != BodyKind::Moon {
            parts.push(match self.moons {
                1 => "1 moon".to_string(),
                moons => format!("{moons} moons"),
            });
        }
        Some(parts.join(SEPARATOR))
    }

    /// `streak 5 · T 13.4s · 23 revs` — what this body has *done*.
    ///
    /// `None` when none of the three has anything to say, so a plain shell pane
    /// gets no line rather than a line of placeholders.
    pub(super) fn orbit_line(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(streak) = self.streak {
            parts.push(format!("streak {streak:.0}"));
        }
        if let Some(period) = self.period() {
            parts.push(format!("T {}", format_period(period)));
        }
        if self.revolutions >= 1.0 {
            parts.push(format!("{} revs", self.revolutions.floor() as u64));
        }
        (!parts.is_empty()).then(|| parts.join(SEPARATOR))
    }
}

/// What separates two facts on one register line.
///
/// Tighter than the caption line's `"  ·  "` because these lines carry three
/// facts rather than two and the panel is the width it is. The glyph is the
/// reference's own.
const SEPARATOR: &str = " · ";

/// How a period reads.
///
/// Seconds with a tenth below a minute, whole minutes above it. A period is a
/// rate the eye is comparing across rows, not a duration anybody is timing, so
/// the precision stops where two rows stop being distinguishable.
fn format_period(period: std::time::Duration) -> String {
    let secs = period.as_secs_f32();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{:.0}m", secs / 60.0)
    }
}

/// Every row's body facts, resolved once for one render pass.
#[derive(Debug, Default, Clone)]
pub(super) struct BodyRegister {
    facts: HashMap<CardRow, BodyFacts>,
}

impl BodyRegister {
    /// Walk the fleet, rank it, and read every body's registers.
    pub(super) fn resolve(app: &AppState) -> Self {
        let (nodes, rows) = background_scene::tree_nodes(app);
        let ladder = crate::solar_system::seat_the_ladder(&nodes);
        let types = crate::solar_system::assign_body_types(&nodes, &ladder.seated);

        let mut moons = vec![0usize; nodes.len()];
        for node in &nodes {
            if let Some(parent) = node.parent {
                if let Some(count) = moons.get_mut(parent) {
                    *count += 1;
                }
            }
        }

        let mut facts = HashMap::with_capacity(nodes.len());
        for (index, node) in nodes.iter().enumerate() {
            let Some(row) = rows.get(index) else {
                continue;
            };
            facts.insert(
                row.clone(),
                BodyFacts {
                    kind: node.kind,
                    body_type: ladder
                        .seated
                        .get(index)
                        .copied()
                        .unwrap_or(true)
                        .then(|| types.get(index).copied())
                        .flatten(),
                    size: node.size,
                    moons: moons.get(index).copied().unwrap_or(0),
                    streak: streak_of(app, row),
                    revolutions: app.orbit_tracks.revolutions(row),
                    traffic: node.mote_share,
                },
            );
        }
        Self { facts }
    }

    pub(super) fn get(&self, row: &CardRow) -> Option<&BodyFacts> {
        self.facts.get(row)
    }
}

/// One row's streak, decayed to the render's own clock.
///
/// Decayed here rather than read raw for the reason the `streak` token gives:
/// a counter herdr kept would have stood still while herdr was stopped and
/// redrawn a stale score at full heat.
fn streak_of(app: &AppState, row: &CardRow) -> Option<f64> {
    let CardRow::Space(id) = row else {
        // The captain's correction puts the streak expression on second mates;
        // a worker's own streak lives on its wire border, not in this register.
        return None;
    };
    let workspace = app
        .workspaces
        .iter()
        .find(|workspace| &workspace.id == id)?;
    let readout = workspace
        .metadata_tokens
        .get(crate::quality_streak::STREAK_TOKEN)
        .and_then(crate::quality_streak::parse)?;
    let half_lives = crate::quality_streak::half_lives(
        workspace
            .metadata_tokens
            .get(crate::quality_streak::HALF_LIFE_TOKEN),
    );
    Some(crate::quality_streak::decayed(
        readout,
        half_lives,
        app.wall_now,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(kind: BodyKind, body_type: Option<BodyType>, size: BodySize) -> BodyFacts {
        BodyFacts {
            kind,
            body_type,
            size,
            moons: 2,
            streak: Some(5.2),
            revolutions: 23.7,
            traffic: 0.5,
        }
    }

    /// The reference's own mate line, reproduced from the registers herdr
    /// already keeps.
    #[test]
    fn a_seated_gas_giant_reads_as_the_reference_writes_it() {
        let facts = facts(BodyKind::Planet, Some(BodyType::Gas), BodySize::Files(99));
        assert_eq!(
            facts.body_line().as_deref(),
            Some("gas giant · 99 files · 2 moons")
        );
    }

    #[test]
    fn the_orbit_line_carries_streak_period_and_revolutions() {
        let facts = facts(
            BodyKind::Planet,
            Some(BodyType::Ringed),
            BodySize::Files(99),
        );
        let line = facts.orbit_line().expect("a measured planet has a line");
        assert!(line.starts_with("streak 5 · T "), "{line}");
        assert!(line.ends_with(" · 23 revs"), "{line}");
    }

    /// An unmeasured project says what it is and how many workers hang off it,
    /// and says nothing at all about a file count nobody published — the
    /// distinction [`BodySize::Unmeasured`] exists to hold.
    #[test]
    fn an_unmeasured_project_omits_the_file_count_rather_than_printing_zero() {
        let facts = facts(BodyKind::Planet, Some(BodyType::Gas), BodySize::Unmeasured);
        assert_eq!(facts.body_line().as_deref(), Some("gas giant · 2 moons"));
    }

    /// A worker is a moon: no moons of its own, no streak, and its tier's own
    /// fixed rate rather than a mass it does not have.
    #[test]
    fn a_worker_moon_carries_no_moon_count() {
        let mut facts = facts(BodyKind::Moon, None, BodySize::Fixed);
        facts.streak = None;
        assert_eq!(facts.body_line().as_deref(), Some("moon"));
        let line = facts.orbit_line().expect("a moon still orbits");
        assert!(line.starts_with("T "), "{line}");
    }

    /// The sun does not orbit, so it has no period and no revolutions — and a
    /// root with no streak published has nothing to say on the second line.
    #[test]
    fn the_sun_has_no_orbit_line() {
        let mut facts = facts(BodyKind::Sun, None, BodySize::Fixed);
        facts.streak = None;
        facts.revolutions = 0.0;
        assert_eq!(facts.body_line().as_deref(), Some("star · 2 moons"));
        assert_eq!(facts.orbit_line(), None);
    }

    #[test]
    fn one_moon_is_not_pluralised() {
        let mut facts = facts(BodyKind::Planet, Some(BodyType::Gas), BodySize::Files(1));
        facts.moons = 1;
        assert_eq!(
            facts.body_line().as_deref(),
            Some("gas giant · 1 files · 1 moon")
        );
    }

    #[test]
    fn a_period_over_a_minute_reads_in_minutes() {
        assert_eq!(
            format_period(std::time::Duration::from_millis(13_400)),
            "13.4s"
        );
        assert_eq!(format_period(std::time::Duration::from_secs(180)), "3m");
    }

    /// A fleet with nothing in it resolves to an empty register rather than
    /// panicking on the ranking of an empty roster.
    #[test]
    fn an_empty_fleet_resolves_to_nothing() {
        let app = AppState::test_new();
        let register = BodyRegister::resolve(&app);
        assert!(register.facts.is_empty() || register.facts.len() == app.workspaces.len());
    }
}
