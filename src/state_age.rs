//! How long an agent has held its current state, rendered as an elapsed-time
//! token.
//!
//! Deliberately *not* a stall alarm. A threshold badge ("stalled") is a
//! heuristic laid over a detection subsystem that is otherwise kept
//! evidence-based, and a badge that fires on a healthy long-running agent
//! costs more trust than it buys. An elapsed time has no false-positive
//! concept at all: it reports the one fact the runtime actually knows, and
//! leaves the judgement to the reader.
//!
//! One unit, always, because the sidebar row this lands on is measured in
//! single columns: `9s`, `47m`, `3h`, `6d`. The unit is floored rather than
//! rounded, so the token never claims more elapsed time than has passed.

use std::time::Duration;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// Render an age as a single-unit elapsed-time token.
pub fn format(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < MINUTE {
        format!("{secs}s")
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else {
        format!("{}d", secs / DAY)
    }
}

/// How long until [`format`] would render something different.
///
/// This is what makes the token affordable. The obvious implementation
/// repaints once a second forever; this one repaints only when the drawn
/// characters would actually change, so an agent that has been working for two
/// hours costs one wake-up an hour rather than 3600 of them.
pub(crate) fn next_change_after(age: Duration) -> Duration {
    let secs = age.as_secs();
    let resolution = if secs < MINUTE {
        1
    } else if secs < HOUR {
        MINUTE
    } else if secs < DAY {
        HOUR
    } else {
        DAY
    };
    // Time from `age` to the next multiple of `resolution`. Subsecond
    // remainder is carried so the wake-up lands on the boundary rather than
    // drifting a fraction past it every tick.
    let elapsed_in_unit = Duration::new(secs % resolution, age.subsec_nanos());
    Duration::from_secs(resolution).saturating_sub(elapsed_in_unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_unit_floors_rather_than_rounds() {
        assert_eq!(format(Duration::from_millis(0)), "0s");
        assert_eq!(format(Duration::from_millis(999)), "0s");
        assert_eq!(format(Duration::from_secs(59)), "59s");
        assert_eq!(format(Duration::from_secs(60)), "1m");
        // 119s is very nearly two minutes and must not claim to be.
        assert_eq!(format(Duration::from_secs(119)), "1m");
        assert_eq!(format(Duration::from_secs(47 * 60)), "47m");
        assert_eq!(format(Duration::from_secs(60 * 60 - 1)), "59m");
        assert_eq!(format(Duration::from_secs(60 * 60)), "1h");
        assert_eq!(format(Duration::from_secs(23 * 60 * 60)), "23h");
        assert_eq!(format(Duration::from_secs(24 * 60 * 60)), "1d");
        assert_eq!(format(Duration::from_secs(6 * 24 * 60 * 60 + 7)), "6d");
    }

    /// The sidebar hands rows a column budget, so a token that can blow past
    /// three columns is a token that steals from the label beside it.
    #[test]
    fn a_token_stays_narrow_across_a_year() {
        for secs in [0, 9, 59, 60, 3599, 3600, 86_399, 86_400, 365 * 86_400] {
            let rendered = format(Duration::from_secs(secs));
            assert!(
                rendered.len() <= 4,
                "{secs}s rendered as {rendered}, wider than the row can spare"
            );
        }
    }

    #[test]
    fn the_wake_up_lands_exactly_on_the_boundary_that_changes_the_text() {
        let cases = [
            Duration::from_millis(0),
            Duration::from_millis(1),
            Duration::from_millis(1500),
            Duration::from_secs(59),
            Duration::from_secs(60),
            Duration::from_secs(61),
            Duration::from_secs(3599),
            Duration::from_secs(3600),
            Duration::from_secs(86_399),
            Duration::from_secs(86_400),
            Duration::from_secs(500_000),
        ];
        for age in cases {
            let wait = next_change_after(age);
            assert!(
                wait > Duration::ZERO,
                "{age:?} scheduled a zero wait, which would spin the loop"
            );
            // Just before the boundary the text is unchanged; at it, changed.
            let just_before = age + wait - Duration::from_millis(1);
            assert_eq!(
                format(just_before),
                format(age),
                "{age:?} woke up early: {just_before:?} still renders the same"
            );
            assert_ne!(
                format(age + wait),
                format(age),
                "{age:?} woke up late: {:?} renders the same",
                age + wait
            );
        }
    }

    /// The point of the resolution ladder: a long-lived state must not cost a
    /// wake-up per second.
    #[test]
    fn an_old_state_is_cheap_to_keep_current() {
        assert_eq!(
            next_change_after(Duration::from_secs(2 * HOUR)),
            Duration::from_secs(HOUR)
        );
        assert_eq!(
            next_change_after(Duration::from_secs(3 * DAY)),
            Duration::from_secs(DAY)
        );
    }
}
