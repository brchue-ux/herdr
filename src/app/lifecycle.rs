//! What stage a row's work is at, and how bad the problem on it is.
//!
//! Two facts about the fleet, resolved in one place so every surface that draws
//! them draws the same answer. Both are pure functions of published tokens and
//! the state Herdr detected for itself — no clock, no PTY, no render pass — so
//! the whole vocabulary is testable as data.
//!
//! Three properties this module is responsible for holding:
//!
//! - **The two are independent facts, not two readings of one.** A stage says
//!   where the work is; a severity says how badly it is going. Every combination
//!   is meaningful, and *"running, but in serious trouble"* is the one the split
//!   exists for. Nothing here may derive one from the other.
//! - **Detection is the floor, publication is the ceiling.** Herdr can see four
//!   of the five stages on its own, from the screen. It cannot see the fifth:
//!   a prompt sitting idle looks identical whether the work behind it succeeded
//!   or failed, and no manifest will ever tell them apart. So a fleet that knows
//!   says so through [`STAGE_TOKEN`], and a fleet that says nothing still gets a
//!   correct four-stage reading rather than nothing at all.
//! - **A publisher cannot make a row cost more.** Both tokens ride
//!   `workspace.report_metadata`, which is already rate-limited, already capped,
//!   already durable, and already expires on its own TTL. Nothing here holds
//!   state of its own, so there is nothing to leak when a publisher goes away.

use crate::anim::cell::{LifecycleStage, Severity};
use crate::detect::AgentState;

/// The published token naming which lifecycle stage a row's work is at.
///
/// Reserved the same way `owner` and the worker summary line are, and read the
/// same way: a name Herdr interprets structurally rather than displays.
pub(crate) const STAGE_TOKEN: &str = "lifecycle";

/// The published token naming how bad the problem on a row is.
pub(crate) const SEVERITY_TOKEN: &str = "severity";

/// The stage a row is at.
///
/// What the fleet published wins, because it is the only party that can know
/// the difference between finishing and failing. Everything else falls back to
/// what detection saw, which covers the other four stages honestly:
///
/// | detected | stage | why |
/// |---|---|---|
/// | `Working` | `Running` | work is happening |
/// | `Blocked` | `Waiting` | stopped on a person |
/// | `Idle` | `Done` | agent finished, prompt visible |
/// | `Unknown` | `Queued` | a shell with no agent in it — nothing has started |
///
/// An unrecognised token value falls back rather than erroring: a publisher that
/// invents a stage name should degrade to Herdr's own reading, not blank the
/// row's hue.
pub(crate) fn stage(published: Option<&str>, detected: AgentState) -> LifecycleStage {
    if let Some(stage) = published.and_then(parse_stage) {
        return stage;
    }
    match detected {
        AgentState::Working => LifecycleStage::Running,
        AgentState::Blocked => LifecycleStage::Waiting,
        AgentState::Idle => LifecycleStage::Done,
        AgentState::Unknown => LifecycleStage::Queued,
    }
}

/// How bad the problem on a row is.
///
/// Absent is [`Severity::Clear`], and so is anything unrecognised. A row with
/// nothing published about it is a row with nothing wrong with it, which is the
/// only safe default: the alternative is a fleet that has never heard of this
/// token rendering as though every one of its rows were on fire.
pub(crate) fn severity(published: Option<&str>) -> Severity {
    published.and_then(parse_severity).unwrap_or_default()
}

fn parse_stage(value: &str) -> Option<LifecycleStage> {
    match normalize(value).as_str() {
        "queued" | "pending" | "waiting-to-start" => Some(LifecycleStage::Queued),
        "running" | "working" => Some(LifecycleStage::Running),
        "waiting" | "blocked" => Some(LifecycleStage::Waiting),
        "done" | "completed" | "finished" => Some(LifecycleStage::Done),
        "failed" | "error" => Some(LifecycleStage::Failed),
        _ => None,
    }
}

fn parse_severity(value: &str) -> Option<Severity> {
    match normalize(value).as_str() {
        "clear" | "none" | "ok" => Some(Severity::Clear),
        "mild" | "notice" | "info" => Some(Severity::Mild),
        "serious" | "warn" | "warning" => Some(Severity::Serious),
        "critical" | "severe" | "fatal" => Some(Severity::Critical),
        _ => None,
    }
}

/// Trimmed, lowercased, and with either separator spelled one way.
///
/// Publishers are shell scripts and agents, so `Serious`, `serious` and
/// `waiting_to_start` all arrive and all mean what they say.
fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_covers_four_stages_and_publication_supplies_the_fifth() {
        assert_eq!(stage(None, AgentState::Working), LifecycleStage::Running);
        assert_eq!(stage(None, AgentState::Blocked), LifecycleStage::Waiting);
        assert_eq!(stage(None, AgentState::Idle), LifecycleStage::Done);
        assert_eq!(stage(None, AgentState::Unknown), LifecycleStage::Queued);
        // The one detection can never reach: an idle prompt after a failure is
        // byte-identical to an idle prompt after a success.
        assert_eq!(
            stage(Some("failed"), AgentState::Idle),
            LifecycleStage::Failed
        );
    }

    #[test]
    fn an_unrecognised_publication_degrades_to_what_was_detected() {
        assert_eq!(
            stage(Some("marinating"), AgentState::Working),
            LifecycleStage::Running
        );
        assert_eq!(
            stage(Some(""), AgentState::Blocked),
            LifecycleStage::Waiting
        );
    }

    #[test]
    fn a_row_nobody_published_about_has_nothing_wrong_with_it() {
        assert_eq!(severity(None), Severity::Clear);
        assert_eq!(severity(Some("catastrophe")), Severity::Clear);
        assert_eq!(severity(Some("  Critical ")), Severity::Critical);
        assert_eq!(severity(Some("WARN")), Severity::Serious);
    }

    #[test]
    fn the_two_channels_are_read_from_two_tokens() {
        // The contract the split rests on: neither resolver can see the other's
        // input, so no value of one can move the other.
        assert_ne!(STAGE_TOKEN, SEVERITY_TOKEN);
        // No severity name is also a stage name, so a fleet that publishes one
        // into the other's token gets the fallback rather than a silent
        // cross-channel reading.
        for value in ["clear", "mild", "serious", "critical"] {
            assert_eq!(
                stage(Some(value), AgentState::Working),
                LifecycleStage::Running
            );
        }
        for value in ["queued", "running", "done", "failed"] {
            assert_eq!(severity(Some(value)), Severity::Clear);
        }
    }

    #[test]
    fn separators_and_case_are_not_part_of_the_vocabulary() {
        assert_eq!(
            stage(Some("Waiting_To_Start"), AgentState::Idle),
            LifecycleStage::Queued
        );
    }
}
