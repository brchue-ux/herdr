//! The two account-level quota windows a fleet publisher can report: the
//! 5-hour (session) window and the 7-day (weekly) window.
//!
//! Herdr has no native quota metering of its own. What it knows comes from
//! [`SESSION_TOKEN`] and [`WEEKLY_TOKEN`], two workspace metadata tokens a
//! fleet-side publisher (`fm-quota-publish.sh`) writes with
//! `workspace.report_metadata`, mirroring how [`crate::app::lifecycle`]'s
//! `lifecycle`/`severity` tokens and the sidebar's `outcome`/`streak` tokens
//! already carry fleet facts into a row. Each token's value is
//! `<percentUsed>` or `<percentUsed>@<resetsAt RFC3339>` — the raw reset
//! timestamp rather than a pre-computed duration, so the countdown a reader
//! sees is always computed against the current render, never a duration that
//! started going stale the moment it was published.
//!
//! [`SystemTime`] is threaded through as a parameter rather than read here
//! with `SystemTime::now()`, the same way [`crate::state_age`] takes its
//! `Instant` from the caller: it keeps this module a pure function of its
//! inputs, testable without a real clock.

use std::time::{Duration, SystemTime};

/// The workspace metadata token carrying the 5-hour (session) window.
pub(crate) const SESSION_TOKEN: &str = "quota_5h";

/// The workspace metadata token carrying the 7-day (weekly) window.
pub(crate) const WEEKLY_TOKEN: &str = "quota_7d";

/// One window's reading: how much of it is used, and when it resets.
///
/// `resets_at` is absent exactly when the publisher's own source had none —
/// see `fm-quota-publish.sh`'s "no trailing @" case — which reads as a
/// percentage with no countdown rather than inventing one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct QuotaReadout {
    pub percent_used: f64,
    pub resets_at: Option<SystemTime>,
}

/// Parse a `quota_5h`/`quota_7d` token value: `<percentUsed>[@<resetsAt>]`.
///
/// Any malformed piece — an unparseable percentage, or a `resetsAt` that is
/// not RFC3339 — fails the whole token rather than showing half a fact.
pub(crate) fn parse(raw: &str) -> Option<QuotaReadout> {
    let (percent_str, resets_str) = match raw.split_once('@') {
        Some((percent, resets)) => (percent, Some(resets)),
        None => (raw, None),
    };
    let percent_used: f64 = percent_str.parse().ok()?;
    let resets_at = match resets_str {
        Some(resets_str) => Some(parse_rfc3339(resets_str)?),
        None => None,
    };
    Some(QuotaReadout {
        percent_used,
        resets_at,
    })
}

/// The percentage, formatted with no more precision than the source
/// published: whole when it is whole, one decimal when it is not.
pub(crate) fn format_percent(percent_used: f64) -> String {
    if percent_used.fract() == 0.0 {
        format!("{percent_used:.0}")
    } else {
        format!("{percent_used:.1}")
    }
}

/// One window's readout line: `<label> <percent>%, resets in <age>` — or, with
/// no reset timestamp published, just `<label> <percent>%`.
///
/// Reuses [`crate::state_age::format`] for the countdown rather than a
/// second elapsed-time formatter: a reset five hours away and a state five
/// hours old are the same shape of fact, one unit, floored.
pub(crate) fn format_readout(label: &str, readout: &QuotaReadout, now: SystemTime) -> String {
    let percent = format_percent(readout.percent_used);
    match readout.resets_at {
        Some(resets_at) => {
            let remaining = resets_at.duration_since(now).unwrap_or(Duration::ZERO);
            format!(
                "{label} {percent}%, resets in {}",
                crate::state_age::format(remaining)
            )
        }
        None => format!("{label} {percent}%"),
    }
}

/// Parse an RFC 3339 timestamp (`2026-08-08T03:40:00.227534+00:00` or
/// `...Z`) into a [`SystemTime`].
///
/// Hand-rolled rather than a dependency: the repo has no date/time crate
/// (`rg` over `Cargo.toml` turns up none), this is the one place Herdr needs
/// to parse one, and the fleet's own publishers only ever emit this exact
/// shape (`quota-axi --json`'s `resetsAt`).
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (time, offset_secs) = if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        (time, 0i64)
    } else {
        let sign_idx = rest.rfind(['+', '-'])?;
        let (time, offset) = rest.split_at(sign_idx);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (oh, om) = offset[1..].split_once(':')?;
        let oh: i64 = oh.parse().ok()?;
        let om: i64 = om.parse().ok()?;
        if !(0..24).contains(&oh) || !(0..60).contains(&om) {
            return None;
        }
        (time, sign * (oh * 3600 + om * 60))
    };

    // Drop sub-second precision: no window reset needs finer than a second,
    // and Herdr has no use for it once this becomes a countdown.
    let time = time.split_once('.').map_or(time, |(whole, _)| whole);
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some()
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let unix_seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    if unix_seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(unix_seconds as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs((-unix_seconds) as u64))
    }
}

/// Days since the Unix epoch for a proleptic-Gregorian civil date.
///
/// Howard Hinnant's `days_from_civil`
/// (<https://howardhinnant.github.io/date_algorithms.html>), public domain:
/// exact for every year this timestamp source will ever emit, and small
/// enough to carry here rather than pull in a date crate for one conversion.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11], Mar = 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_with_fractional_seconds_and_zero_offset_matches_the_epoch() {
        // Cross-checked against `date -u -d 2026-08-08T03:40:00Z +%s`.
        let parsed = parse_rfc3339("2026-08-08T03:40:00.227534+00:00").unwrap();
        assert_eq!(
            parsed
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1_786_160_400
        );
    }

    #[test]
    fn rfc3339_z_suffix_is_the_same_instant_as_a_zero_offset() {
        let z = parse_rfc3339("2026-08-08T03:40:00Z").unwrap();
        let offset = parse_rfc3339("2026-08-08T03:40:00+00:00").unwrap();
        assert_eq!(z, offset);
    }

    #[test]
    fn rfc3339_positive_offset_shifts_earlier_in_utc() {
        // 05:00 local at +05:00 is 00:00 UTC the same day.
        let local = parse_rfc3339("2026-08-08T05:00:00+05:00").unwrap();
        let utc = parse_rfc3339("2026-08-08T00:00:00Z").unwrap();
        assert_eq!(local, utc);
    }

    #[test]
    fn rfc3339_rejects_garbage() {
        assert!(parse_rfc3339("not-a-timestamp").is_none());
        assert!(parse_rfc3339("2026-08-08").is_none());
        assert!(parse_rfc3339("2026-13-08T00:00:00Z").is_none());
    }

    #[test]
    fn bare_percent_has_no_reset_countdown() {
        let readout = parse("15").unwrap();
        assert_eq!(readout.percent_used, 15.0);
        assert_eq!(readout.resets_at, None);
    }

    #[test]
    fn percent_and_reset_timestamp_both_parse() {
        let readout = parse("15@2026-08-10T05:00:00.227558+00:00").unwrap();
        assert_eq!(readout.percent_used, 15.0);
        assert!(readout.resets_at.is_some());
    }

    #[test]
    fn a_malformed_reset_timestamp_fails_the_whole_token() {
        assert!(parse("15@not-a-timestamp").is_none());
    }

    #[test]
    fn a_non_numeric_percent_fails_the_whole_token() {
        assert!(parse("not-a-number").is_none());
        assert!(parse("not-a-number@2026-08-10T05:00:00Z").is_none());
    }

    #[test]
    fn readout_with_no_reset_omits_the_countdown() {
        let readout = QuotaReadout {
            percent_used: 15.0,
            resets_at: None,
        };
        assert_eq!(
            format_readout("week", &readout, SystemTime::UNIX_EPOCH),
            "week 15%"
        );
    }

    #[test]
    fn readout_with_a_reset_reads_as_one_phrase() {
        let now = SystemTime::UNIX_EPOCH;
        let readout = QuotaReadout {
            percent_used: 42.0,
            resets_at: Some(now + Duration::from_secs(2 * 3600 + 14 * 60)),
        };
        assert_eq!(
            format_readout("session", &readout, now),
            "session 42%, resets in 2h"
        );
    }

    #[test]
    fn a_reset_already_in_the_past_reads_as_no_time_left_rather_than_underflowing() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(3600);
        let readout = QuotaReadout {
            percent_used: 0.0,
            resets_at: Some(SystemTime::UNIX_EPOCH),
        };
        assert_eq!(
            format_readout("session", &readout, now),
            "session 0%, resets in 0s"
        );
    }

    #[test]
    fn a_fractional_percent_keeps_one_decimal() {
        assert_eq!(format_percent(12.5), "12.5");
        assert_eq!(format_percent(12.0), "12");
    }
}
