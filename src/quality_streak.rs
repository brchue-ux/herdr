//! The quality streak a fleet publishes about one of its own mates: how well
//! the work has been going lately, and how bad the defect it currently owns is.
//!
//! Herdr scores nothing itself. What it knows arrives as three workspace
//! metadata tokens a fleet-side publisher (`fm-quality-event.sh`) writes with
//! `workspace.report_metadata`, the same fleet-facts-ride-tokens path
//! [`crate::app::lifecycle`]'s `lifecycle`/`severity` and [`crate::quota`]'s
//! `quota_5h`/`quota_7d` already use:
//!
//! | token | value | what it says |
//! |---|---|---|
//! | [`STREAK_TOKEN`] | `<score>@<unix seconds>` | the running total **and** the instant it was true |
//! | [`HALF_LIFE_TOKEN`] | `<win days>/<loss days>` | the decay constants, published rather than compiled in |
//! | [`DEFECT_TOKEN`] | `S1`..`S4`, or `-` | the open defect's severity, or that there is none |
//!
//! # Why the score carries its own timestamp
//!
//! Because **the decay is computed here, at read time, on every render** —
//! `stored × 2^(−(now − ts) / 86400 / hl)` — and never accumulated by a tick of
//! Herdr's own. Nothing ticks while Herdr is stopped, and metadata tokens are
//! persisted and carried across a handoff, so a Herdr restarted after two days
//! would otherwise redraw a two-day-old score at full heat. With the instant in
//! the token the same arithmetic redraws it two days colder, cold from the first
//! frame, with no state of its own to be wrong. It is the same reason
//! [`crate::quota`] publishes a reset *timestamp* rather than a countdown, and
//! it is also what lets the publisher stay silent: a token nobody has touched
//! for a week is still correct, so nothing has to heartbeat.
//!
//! Which half-life applies is decided by the **sign of the stored score**, not
//! by the sign of the decayed one: wins fade on the win half-life and losses on
//! the (slower) loss one. That asymmetry is the whole of "trust recovers slowly"
//! in the design — there is no fudge factor anywhere else to find it in.
//!
//! [`SystemTime`] is threaded in as a parameter rather than read here, exactly
//! as [`crate::quota`] does, so every function in this module is a pure function
//! of its inputs and testable without a clock.
//!
//! Design: `data/herdr-severity-and-weighting/report.md` sections D and F in the
//! fleet's own repository.

use std::time::SystemTime;

use crate::anim::cell::LifecycleStage;

/// The workspace metadata token carrying the streak: `<score>@<unix seconds>`.
pub(crate) const STREAK_TOKEN: &str = "streak";

/// The workspace metadata token carrying the decay constants, in days:
/// `<win half-life>/<loss half-life>`.
pub(crate) const HALF_LIFE_TOKEN: &str = "streak_hl";

/// The workspace metadata token carrying the open defect's severity.
///
/// Deliberately *not* folded into [`crate::app::lifecycle`]'s `severity` token,
/// though both are severities: that one is a four-word vocabulary
/// (`clear`/`mild`/`serious`/`critical`) describing how a whole row is going and
/// colours everything the row draws, while this one is the fleet's own S1–S4
/// defect ladder and answers a narrower question — is a defect open on this row
/// at all, and how loud should its marker be. A publisher can send either
/// without implying the other.
pub(crate) const DEFECT_TOKEN: &str = "sev";

/// The half-lives used when a fleet publishes a streak but no [`HALF_LIFE_TOKEN`]
/// (or an unusable one): 5 days for wins, 10 for losses.
///
/// The two dominant half-lives in the design's own weighting table, so a
/// publisher that omits the token gets the decay the table describes rather than
/// no readout at all. Unlike [`parse`], an unusable value here falls back
/// instead of failing the whole readout: the constants are a tuning knob, not
/// the fact being reported, and a mistyped knob must not blank a score that
/// parsed perfectly well.
pub(crate) const DEFAULT_HALF_LIVES: HalfLives = HalfLives {
    win_days: 5.0,
    loss_days: 10.0,
};

/// A streak as published: the score, and the instant it was true.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StreakReadout {
    /// The running total at [`Self::published_at`], before any decay.
    pub stored: f64,
    pub published_at: SystemTime,
}

/// How fast a win and a loss fade, in days.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HalfLives {
    pub win_days: f64,
    pub loss_days: f64,
}

impl HalfLives {
    /// The half-life that applies to `stored`, chosen by its **sign**.
    ///
    /// Zero counts as a win: it decays to itself either way, and picking the
    /// loss branch for it would say something the value does not.
    fn for_score(self, stored: f64) -> f64 {
        if stored < 0.0 {
            self.loss_days
        } else {
            self.win_days
        }
    }
}

/// Parse a [`STREAK_TOKEN`] value: `<score>@<unix seconds>`.
///
/// Both halves are required, and any malformed piece fails the whole token
/// rather than showing half a fact — the same rule [`crate::quota::parse`]
/// follows, and here it is load-bearing rather than tidy: a score with no
/// instant cannot be decayed, and drawing it undecayed is precisely the
/// falsely-hot readout this module exists to prevent.
pub(crate) fn parse(raw: &str) -> Option<StreakReadout> {
    let (score, ts) = raw.trim().split_once('@')?;
    let stored: f64 = score.trim().parse().ok()?;
    if !stored.is_finite() {
        return None;
    }
    let seconds: u64 = ts.trim().parse().ok()?;
    Some(StreakReadout {
        stored,
        published_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
    })
}

/// Parse a [`HALF_LIFE_TOKEN`] value: `<win days>/<loss days>`.
///
/// Non-positive or non-finite half-lives are rejected: a zero half-life is a
/// division by zero and a negative one *grows* the score with age, which would
/// turn the oldest streak on the fleet into the hottest.
pub(crate) fn parse_half_lives(raw: &str) -> Option<HalfLives> {
    let (win, loss) = raw.trim().split_once('/')?;
    let win_days: f64 = win.trim().parse().ok()?;
    let loss_days: f64 = loss.trim().parse().ok()?;
    if !(win_days.is_finite() && loss_days.is_finite()) || win_days <= 0.0 || loss_days <= 0.0 {
        return None;
    }
    Some(HalfLives {
        win_days,
        loss_days,
    })
}

/// The half-lives to decay with, given whatever the row published.
///
/// Absent or unusable falls back to [`DEFAULT_HALF_LIVES`]; see its own note for
/// why this is the one piece of the readout that degrades rather than fails.
pub(crate) fn half_lives(published: Option<&str>) -> HalfLives {
    published
        .and_then(parse_half_lives)
        .unwrap_or(DEFAULT_HALF_LIVES)
}

/// What the streak reads *now*: `stored × 2^(−elapsed_days / half_life)`.
///
/// A `now` earlier than the publish instant — two machines whose clocks
/// disagree, a token restored from a snapshot — decays by zero rather than by a
/// negative age. The alternative is a readout that a clock skew makes *hotter*
/// than the fleet ever earned, and a marker that overstates the work is worse
/// than one that is a few hours behind.
pub(crate) fn decayed(readout: StreakReadout, half_lives: HalfLives, now: SystemTime) -> f64 {
    let elapsed = now
        .duration_since(readout.published_at)
        .unwrap_or(std::time::Duration::ZERO);
    let elapsed_days = elapsed.as_secs_f64() / 86_400.0;
    let half_life = half_lives.for_score(readout.stored);
    readout.stored * 2f64.powf(-(elapsed_days / half_life))
}

/// How hot a mate's streak is reading, in five bands.
///
/// Thresholds from the design's own table, lower bound inclusive. Cold is a
/// *negative* total rather than a small one: a fleet at zero has done nothing
/// lately, which is not the same statement as a fleet that has been dropping
/// things, and only the second one should read as burnt out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FlameBand {
    /// Below zero: losses outweigh wins. No flame.
    Cold,
    /// `0..8` — barely alight.
    Ember,
    /// `8..20`.
    Low,
    /// `20..38`.
    Steady,
    /// `38` and up. Reached by sustained work; there is no ceiling above it.
    Hot,
}

impl FlameBand {
    /// The band a decayed total falls in.
    pub(crate) fn of(value: f64) -> Self {
        // NaN cannot arrive here — `parse` rejects a non-finite score and decay
        // only ever multiplies it by a finite positive factor — but the
        // comparison ladder is written so that if one ever did it would read as
        // the quietest band rather than the loudest.
        if value >= 38.0 {
            Self::Hot
        } else if value >= 20.0 {
            Self::Steady
        } else if value >= 8.0 {
            Self::Low
        } else if value >= 0.0 {
            Self::Ember
        } else {
            Self::Cold
        }
    }

    /// The word this band draws as.
    ///
    /// A word and not only a colour, because the readout has to survive being
    /// read as text — by a colour-blind reader, by a monochrome theme, and by
    /// `herdr pane read --format text`, which is how this token is verified
    /// against a live fleet in the first place.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Ember => "ember",
            Self::Low => "low",
            Self::Steady => "steady",
            Self::Hot => "hot",
        }
    }

    /// Every band, coldest first. For a matrix or a test.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [Self::Cold, Self::Ember, Self::Low, Self::Steady, Self::Hot];
}

/// One Space's streak line: `streak 23.8 steady`.
///
/// The number as well as the band, because five bands cannot show a streak
/// climbing *within* a band, and "am I about to tip over" is most of what a
/// reader wants from it. One decimal: the publisher rounds to two, and the
/// second one is noise at this width.
pub(crate) fn format_readout(value: f64, band: FlameBand) -> String {
    format!("streak {value:.1} {}", band.label())
}

/// The severity of the defect a row currently owns, on the fleet's own ladder.
///
/// Ordered quietest first, so `S4 < S1` compares the way the intensities do
/// rather than the way the names count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DefectSeverity {
    /// Cosmetic or trivial.
    S4,
    /// The default a fleet lands on when nobody states one.
    S3,
    /// A real problem.
    S2,
    /// The worst thing the ladder can say.
    S1,
}

impl DefectSeverity {
    /// How loud this severity's marker is drawn, as a fraction of the full
    /// intensity the marker is allowed: **S4 25% · S3 50% · S2 75% · S1 100%**.
    ///
    /// A fraction rather than a colour, because the hue is spoken for: it
    /// carries the row's [`LifecycleStage`], and a marker that changed hue with
    /// severity would make a stage change and a severity change look like the
    /// same event. This number is the *only* thing severity moves — see
    /// [`crate::anim::cell::marker_ink`], where the two channels meet.
    pub(crate) fn intensity(self) -> f32 {
        match self {
            Self::S4 => 0.25,
            Self::S3 => 0.50,
            Self::S2 => 0.75,
            Self::S1 => 1.00,
        }
    }

    /// Every severity, quietest first. For a matrix or a test.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 4] = [Self::S4, Self::S3, Self::S2, Self::S1];
}

/// Whether a row has an open defect, and how loud its marker should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefectMark {
    /// The fleet stated a severity for the open defect.
    Rated(DefectSeverity),
    /// Nobody stated one, but Herdr detected the row as failed on its own.
    ///
    /// Drawn at full intensity, and that is deliberate: an unrated failure is a
    /// defect of *unknown* size, and unknown must not read quieter than a stated
    /// S4. It is also exactly what the marker did before severity had a channel,
    /// so a fleet that publishes nothing sees no change at all.
    Detected,
}

impl DefectMark {
    /// The fraction of full intensity this mark draws at.
    pub(crate) fn intensity(self) -> f32 {
        match self {
            Self::Rated(severity) => severity.intensity(),
            Self::Detected => 1.0,
        }
    }
}

/// Whether to draw a defect marker on a row, and how loudly.
///
/// The same "detection is the floor, publication is the ceiling" rule
/// [`crate::app::lifecycle`] is built on, with one addition it does not need:
/// the fleet can say **`-`, no open defect**, and that silences the marker even
/// on a row detection reads as failed. That is not detection being overruled for
/// its own sake — the two are answering different questions. A stage says where
/// the work is; this says whether anybody currently owns a bug in it. A mate
/// whose last task failed and whose defect is closed is a real state, and the
/// fleet is the only party that can know it.
///
/// An unrecognised value falls back to detection rather than to `-`, so a
/// publisher that invents a spelling loses the intensity, never the warning.
pub(crate) fn defect_mark(published: Option<&str>, stage: LifecycleStage) -> Option<DefectMark> {
    match published.map(str::trim) {
        Some("-") | Some("") | Some("none") => None,
        Some(value) => match parse_severity(value) {
            Some(severity) => Some(DefectMark::Rated(severity)),
            None => detected_mark(stage),
        },
        None => detected_mark(stage),
    }
}

fn detected_mark(stage: LifecycleStage) -> Option<DefectMark> {
    (stage == LifecycleStage::Failed).then_some(DefectMark::Detected)
}

fn parse_severity(value: &str) -> Option<DefectSeverity> {
    match value.trim().to_ascii_uppercase().as_str() {
        "S1" => Some(DefectSeverity::S1),
        "S2" => Some(DefectSeverity::S2),
        "S3" => Some(DefectSeverity::S3),
        "S4" => Some(DefectSeverity::S4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fixed wall clock to publish against, so every case below states its own
    /// age rather than borrowing the machine's.
    const NOW_SECS: u64 = 1_786_147_200; // 2026-08-08T00:00:00Z

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
    }

    fn published_days_ago(days: f64) -> SystemTime {
        now() - Duration::from_secs_f64(days * 86_400.0)
    }

    #[test]
    fn a_streak_carries_its_score_and_the_instant_it_was_true() {
        let readout = parse("23.75@1754319131").expect("a well-formed streak token");
        assert_eq!(readout.stored, 23.75);
        assert_eq!(
            readout.published_at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_754_319_131)
        );
        assert_eq!(parse(" -4.00@1754319131 ").map(|r| r.stored), Some(-4.0));
    }

    #[test]
    fn half_a_fact_is_no_fact() {
        // A score with no instant cannot be decayed, so it is not a readout.
        assert_eq!(parse("23.75"), None);
        assert_eq!(parse("@1754319131"), None);
        assert_eq!(parse("23.75@"), None);
        assert_eq!(parse("hot@1754319131"), None);
        assert_eq!(parse("23.75@yesterday"), None);
        assert_eq!(parse("inf@1754319131"), None);
    }

    #[test]
    fn the_decay_constants_are_published_not_compiled_in() {
        assert_eq!(
            parse_half_lives("5/10"),
            Some(HalfLives {
                win_days: 5.0,
                loss_days: 10.0
            })
        );
        assert_eq!(
            parse_half_lives("3.5/12"),
            Some(HalfLives {
                win_days: 3.5,
                loss_days: 12.0
            })
        );
        // A knob nobody turned, or turned to nonsense, leaves the readout
        // standing on the design's own constants.
        assert_eq!(half_lives(None), DEFAULT_HALF_LIVES);
        assert_eq!(half_lives(Some("5")), DEFAULT_HALF_LIVES);
        assert_eq!(half_lives(Some("0/10")), DEFAULT_HALF_LIVES);
        assert_eq!(half_lives(Some("-5/10")), DEFAULT_HALF_LIVES);
        assert_eq!(half_lives(Some("five/ten")), DEFAULT_HALF_LIVES);
    }

    #[test]
    fn a_score_read_at_the_instant_it_was_published_is_the_score() {
        let readout = StreakReadout {
            stored: 23.75,
            published_at: now(),
        };
        assert_eq!(decayed(readout, DEFAULT_HALF_LIVES, now()), 23.75);
    }

    #[test]
    fn one_half_life_of_age_halves_a_win_and_two_quarter_it() {
        let hl = DEFAULT_HALF_LIVES;
        let five_days = StreakReadout {
            stored: 40.0,
            published_at: published_days_ago(5.0),
        };
        let ten_days = StreakReadout {
            stored: 40.0,
            published_at: published_days_ago(10.0),
        };
        assert!((decayed(five_days, hl, now()) - 20.0).abs() < 1e-9);
        assert!((decayed(ten_days, hl, now()) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_loss_fades_on_the_slower_half_life_than_a_win() {
        let hl = DEFAULT_HALF_LIVES;
        // The same magnitude, the same age, opposite signs: the win has run
        // through two half-lives where the loss has run through one. This is
        // the whole of "trust recovers slowly" — there is no other asymmetry
        // in the model to find it in.
        let win = StreakReadout {
            stored: 40.0,
            published_at: published_days_ago(10.0),
        };
        let loss = StreakReadout {
            stored: -40.0,
            published_at: published_days_ago(10.0),
        };
        assert!((decayed(win, hl, now()) - 10.0).abs() < 1e-9);
        assert!((decayed(loss, hl, now()) + 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_two_day_old_hot_streak_is_no_longer_hot_when_herdr_reads_it_cold() {
        // The restart case this module exists for: nothing ticked for two days,
        // and the readout still has to arrive two days colder. `stored` is well
        // inside `Hot`; two days of the 5-day win half-life take it out of it.
        let readout = StreakReadout {
            stored: 45.0,
            published_at: published_days_ago(2.0),
        };
        let value = decayed(readout, DEFAULT_HALF_LIVES, now());
        assert!(
            (value - 34.0).abs() < 0.5,
            "45 decayed over 2 days of a 5-day half-life should be ~34.0, got {value}"
        );
        assert_eq!(FlameBand::of(45.0), FlameBand::Hot);
        assert_eq!(FlameBand::of(value), FlameBand::Steady);
    }

    #[test]
    fn a_clock_that_runs_backwards_cannot_make_a_streak_hotter() {
        let readout = StreakReadout {
            stored: 20.0,
            published_at: now() + Duration::from_secs(86_400),
        };
        assert_eq!(decayed(readout, DEFAULT_HALF_LIVES, now()), 20.0);
    }

    #[test]
    fn every_band_is_reachable_and_thresholded_on_its_lower_bound() {
        assert_eq!(FlameBand::of(-0.01), FlameBand::Cold);
        assert_eq!(FlameBand::of(-20.0), FlameBand::Cold);
        assert_eq!(FlameBand::of(0.0), FlameBand::Ember);
        assert_eq!(FlameBand::of(7.99), FlameBand::Ember);
        assert_eq!(FlameBand::of(8.0), FlameBand::Low);
        assert_eq!(FlameBand::of(19.99), FlameBand::Low);
        assert_eq!(FlameBand::of(20.0), FlameBand::Steady);
        assert_eq!(FlameBand::of(37.99), FlameBand::Steady);
        assert_eq!(FlameBand::of(38.0), FlameBand::Hot);
        assert_eq!(FlameBand::of(1_000.0), FlameBand::Hot);
        // Five bands, five words, no two the same.
        let labels: std::collections::HashSet<_> =
            FlameBand::ALL.iter().map(|band| band.label()).collect();
        assert_eq!(labels.len(), FlameBand::ALL.len());
    }

    #[test]
    fn the_readout_shows_the_number_as_well_as_the_band() {
        assert_eq!(
            format_readout(23.75, FlameBand::Steady),
            "streak 23.8 steady"
        );
        assert_eq!(format_readout(-4.0, FlameBand::Cold), "streak -4.0 cold");
        assert_eq!(format_readout(0.0, FlameBand::Ember), "streak 0.0 ember");
    }

    #[test]
    fn the_four_intensity_steps_are_the_four_the_design_specifies() {
        assert_eq!(DefectSeverity::S4.intensity(), 0.25);
        assert_eq!(DefectSeverity::S3.intensity(), 0.50);
        assert_eq!(DefectSeverity::S2.intensity(), 0.75);
        assert_eq!(DefectSeverity::S1.intensity(), 1.00);
        // Monotone, quietest first, in both the ordering and the intensity.
        for pair in DefectSeverity::ALL.windows(2) {
            assert!(pair[0] < pair[1]);
            assert!(pair[0].intensity() < pair[1].intensity());
        }
    }

    #[test]
    fn a_published_severity_rates_the_mark_at_any_stage() {
        for stage in LifecycleStage::ALL {
            assert_eq!(
                defect_mark(Some("S1"), stage),
                Some(DefectMark::Rated(DefectSeverity::S1)),
                "a stated S1 must survive stage {stage:?}"
            );
            assert_eq!(
                defect_mark(Some(" s4 "), stage),
                Some(DefectMark::Rated(DefectSeverity::S4))
            );
        }
    }

    #[test]
    fn a_dash_means_no_open_defect_and_draws_nothing() {
        for stage in LifecycleStage::ALL {
            assert_eq!(defect_mark(Some("-"), stage), None);
            assert_eq!(defect_mark(Some(" - "), stage), None);
        }
        // Including the one case where it overrules a detected failure: the
        // fleet is the only party that knows a defect has been closed.
        assert_eq!(defect_mark(Some("-"), LifecycleStage::Failed), None);
    }

    #[test]
    fn nothing_published_leaves_detection_holding_the_marker() {
        assert_eq!(
            defect_mark(None, LifecycleStage::Failed),
            Some(DefectMark::Detected)
        );
        assert_eq!(defect_mark(None, LifecycleStage::Running), None);
        // An invented spelling loses the intensity, never the warning.
        assert_eq!(
            defect_mark(Some("catastrophic"), LifecycleStage::Failed),
            Some(DefectMark::Detected)
        );
        assert_eq!(
            defect_mark(Some("catastrophic"), LifecycleStage::Done),
            None
        );
    }

    #[test]
    fn an_unrated_failure_is_never_quieter_than_a_stated_one() {
        let unrated = DefectMark::Detected.intensity();
        for severity in DefectSeverity::ALL {
            assert!(
                DefectMark::Rated(severity).intensity() <= unrated,
                "{severity:?} must not out-shout an unknown-size failure"
            );
        }
    }
}
