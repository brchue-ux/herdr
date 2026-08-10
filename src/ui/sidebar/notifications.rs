//! The fleet pulse: one counted line on the panel's reserved header row.
//!
//! This row used to be a second copy of the notification tray — the same eight
//! [`FleetSignal`]s, in the same order, drawn as marks instead of as badges.
//! Two readouts of one set of booleans is one readout and some noise, so the
//! row now says the thing the tray structurally cannot: **how much**, rather
//! than **what kind**.
//!
//! Three numbers, and each is a different question from the one the tray
//! answers:
//!
//! - `3 running` — panes with an agent working in them. Not a signal at all:
//!   nobody owns a running agent and it clears itself, which is exactly why
//!   [`crate::app::fleet_signals`] refused it a slot. As a count it is the most
//!   useful single fact about a fleet, and the tray has nowhere to put it.
//! - `1 needs you` — panes with something outstanding for the captain. The
//!   tray's first row says *which of four kinds* of waiting exist; this says how
//!   many panes are actually waiting, which is the number that decides whether
//!   to go and look. Four lit badges can mean four panes or one.
//! - `quota 62%` — the account's 5-hour window, read from the same
//!   [`crate::quota`] token the sidebar's `quota_5h` token renders. Nothing in
//!   the tray reads it, because it is not something you can go and clear.
//!
//! Three properties this module is responsible for holding:
//!
//! - **Nothing here decides what is true.** Every reading comes from
//!   [`crate::app::fleet_signals`] and every frame from the animation engine.
//!   This module owns wording, colour and width, and nothing else.
//! - **Narrowing shortens words, never drops a number.** The ladder gives up
//!   the long labels first and then the labels themselves, so all three numbers
//!   survive down to eight columns. Below that the row draws nothing rather
//!   than a prefix of itself, because two of three numbers with no way to tell
//!   which two is worse than no row at all.
//! - **An unpublished reading is absent, not zero.** The quota segment is
//!   dropped whole when no publisher has reported one — `quota 0%` and "no
//!   quota reporter wired up" are opposite facts.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::fleet_signals::{FleetSignal, FleetSignals};
use crate::app::state::{AppState, Palette};

/// What sits between two readings.
///
/// A middot rather than a bar or a comma: it separates without reading as
/// punctuation belonging to either side, and it is one cell wide in every font
/// Herdr has to survive.
const SEPARATOR: &str = " · ";

/// What sits between two readings once the row cannot afford the spaces.
const TIGHT_SEPARATOR: &str = "·";

/// At or above this percentage the quota reading stops being muted.
const QUOTA_WARN_PERCENT: f64 = 75.0;

/// At or above this percentage the quota reading is drawn as an alert.
const QUOTA_ALERT_PERCENT: f64 = 90.0;

/// Which of the three readings a span belongs to, for styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Running,
    Awaiting,
    Quota,
    Separator,
}

/// Which form the row draws in.
///
/// Ordered widest first, which is also the order [`Pulse::tier`] searches: the
/// row always says as much as the panel can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tier {
    /// Every reading spelled out: `3 running · 1 needs you · quota 62%`.
    Full,
    /// The numbers with the shortest labels that still name them:
    /// `3 run · 1 you · 62%`.
    Compact,
    /// The numbers alone: `3·1·62%`. Position and colour carry what the words
    /// carried — the counts are always running then waiting, and each keeps the
    /// colour it had in the wider tiers — which is the same trade the tree's
    /// own state dots make. The wider tiers are the legend for this one.
    Tight,
}

impl Tier {
    const ALL: [Self; 3] = [Self::Full, Self::Compact, Self::Tight];

    /// What separates two readings in this tier.
    fn separator(self) -> &'static str {
        match self {
            Self::Full | Self::Compact => SEPARATOR,
            Self::Tight => TIGHT_SEPARATOR,
        }
    }
}

/// One frame's reading of the fleet, ready to be measured and drawn.
///
/// Resolved once per frame and handed to both the width query and the
/// renderer, so the columns reserved for the row and the columns it draws
/// cannot come from two different passes over the panes. That matters for cost
/// as much as for correctness: resolving is a walk of every pane in every tab
/// in every workspace, and this row is not allowed to walk it twice.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pulse {
    signals: FleetSignals,
}

impl Pulse {
    /// Read the fleet, or `None` when the row is switched off or has nowhere to
    /// draw.
    pub(super) fn resolve(app: &AppState) -> Option<Self> {
        app.fleet_pulse_active().then(|| Self {
            signals: FleetSignals::resolve(app),
        })
    }

    /// The readings this tier draws, in order, already worded.
    ///
    /// Separators are pieces too rather than something the renderer inserts
    /// between pieces, so the measured width and the drawn width come from
    /// walking the same list.
    fn pieces(&self, tier: Tier) -> Vec<(Role, String)> {
        let mut readings: Vec<(Role, String)> = Vec::with_capacity(3);

        readings.push((
            Role::Running,
            match tier {
                Tier::Full => format!("{} running", self.signals.running()),
                Tier::Compact => format!("{} run", self.signals.running()),
                Tier::Tight => self.signals.running().to_string(),
            },
        ));
        readings.push((
            Role::Awaiting,
            match tier {
                Tier::Full => format!("{} needs you", self.signals.awaiting()),
                Tier::Compact => format!("{} you", self.signals.awaiting()),
                Tier::Tight => self.signals.awaiting().to_string(),
            },
        ));
        if let Some(percent) = self.signals.quota_percent() {
            let percent = crate::quota::format_percent(percent);
            readings.push((
                Role::Quota,
                match tier {
                    Tier::Full => format!("quota {percent}%"),
                    Tier::Compact | Tier::Tight => format!("{percent}%"),
                },
            ));
        }

        let mut pieces = Vec::with_capacity(readings.len() * 2);
        for (index, reading) in readings.into_iter().enumerate() {
            if index > 0 {
                pieces.push((Role::Separator, tier.separator().to_string()));
            }
            pieces.push(reading);
        }
        pieces
    }

    /// Columns this tier occupies.
    ///
    /// Measured from the same list the renderer draws from, so the reserved
    /// width and the drawn width cannot drift apart the way they could if the
    /// number were written down here. Unlike the old bar's, these widths depend
    /// on the readings — a fleet of three panes and a fleet of thirty do not
    /// take the same columns — which is why nothing may cache them.
    fn tier_width(&self, tier: Tier) -> u16 {
        self.pieces(tier)
            .iter()
            .map(|(_, text)| crate::ui::text::display_width_u16(text))
            .sum()
    }

    /// The most detailed tier that fits `available` columns, or `None` when not
    /// even the compact form does.
    fn tier(&self, available: u16) -> Option<Tier> {
        Tier::ALL
            .into_iter()
            .find(|tier| self.tier_width(*tier) <= available)
    }

    /// Columns the row will occupy on a header row `available` wide.
    ///
    /// `0` when the row cannot hold even the compact form. The layout asks this
    /// before drawing so the session status beside it knows what is left.
    pub(super) fn width(&self, available: u16) -> u16 {
        self.tier(available).map_or(0, |tier| self.tier_width(tier))
    }

    /// Draw the row into `area`, which must be exactly [`Self::width`] columns
    /// wide.
    pub(super) fn render(&self, app: &AppState, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(tier) = self.tier(area.width) else {
            return;
        };

        let palette = &app.sidebar_palette;
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (role, text) in self.pieces(tier) {
            let style = self.style(role, palette);
            match self.animated_element(role) {
                Some(element) => {
                    let animation = app
                        .anim
                        .frame(&element, None)
                        .filter(|frame| frame.behaviour.is_some());
                    super::push_animated_span(
                        &mut spans,
                        text,
                        style,
                        animation,
                        super::backdrop_rgb(app),
                        &app.palette,
                        &app.host_terminal_theme,
                    );
                }
                None => spans.push(Span::styled(text, style)),
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
            area,
        );
    }

    /// How one reading is drawn.
    ///
    /// The whole row rests in the panel's muted grey and a reading leaves it
    /// only when it has something to say, which is the same rule the tray's
    /// badges follow and the reason the two never disagree about what colour
    /// means. Bold as well as coloured on every alerting reading: bold is the
    /// one distinction no palette can take away, and some can — `terminal`
    /// resolves several hues onto the resting grey.
    fn style(&self, role: Role, p: &Palette) -> Style {
        let resting = Style::default().fg(p.overlay0);
        let alert = |color: Color| {
            let color = if color == p.overlay0 { p.text } else { color };
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        };
        match role {
            Role::Separator => resting,
            // Yellow is already Herdr's colour for a working pane, in the tree
            // and in the tray alike.
            Role::Running if self.signals.running() > 0 => alert(p.yellow),
            // Red is already the colour of a blocked agent. A fleet with
            // nothing waiting on the captain is the good case and stays grey.
            Role::Awaiting if self.signals.awaiting() > 0 => alert(p.red),
            Role::Quota => match self.signals.quota_percent() {
                Some(percent) if percent >= QUOTA_ALERT_PERCENT => alert(p.red),
                Some(percent) if percent >= QUOTA_WARN_PERCENT => alert(p.peach),
                _ => resting,
            },
            Role::Running | Role::Awaiting => resting,
        }
    }

    /// The animation element a reading borrows while it is alerting, if any.
    ///
    /// Only `needs you` moves, and it moves on an element the fleet signals
    /// already publish rather than one this row invents: whichever of the four
    /// captain-facing signals is live, in their fixed order. That keeps the
    /// engine's element table exactly as it was — the row cannot mount anything
    /// — while still making the one reading that means "go and look" the only
    /// thing on the header row that moves.
    fn animated_element(&self, role: Role) -> Option<crate::anim::ElementId> {
        if role != Role::Awaiting || self.signals.awaiting() == 0 {
            return None;
        }
        FleetSignal::ALL
            .into_iter()
            .take(FleetSignal::PER_ROW)
            .find(|signal| self.signals.is_live(*signal))
            .map(FleetSignal::element_id)
    }
}

/// Columns the pulse row will occupy on a header row `available` wide.
///
/// For callers that only need the geometry — hit-testing the control that sits
/// after the row — and so resolve the fleet for themselves. The render path
/// must use [`Pulse::resolve`] once and pass the value around instead.
pub(super) fn fleet_pulse_width(app: &AppState, available: u16) -> u16 {
    Pulse::resolve(app).map_or(0, |pulse| pulse.width(available))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;

    /// A pulse with the readings set directly, so the wording and width tests
    /// do not have to build a fleet to say what they mean.
    fn pulse(running: usize, awaiting: usize, quota: Option<f64>) -> Pulse {
        Pulse {
            signals: FleetSignals::test_reading(running, awaiting, quota),
        }
    }

    #[test]
    fn the_row_states_all_three_readings_in_full() {
        let text: String = pulse(3, 1, Some(62.0))
            .pieces(Tier::Full)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(text, "3 running · 1 needs you · quota 62%");
    }

    #[test]
    fn the_compact_tier_shortens_the_words_and_keeps_every_number() {
        let compact: String = pulse(3, 1, Some(62.0))
            .pieces(Tier::Compact)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(compact, "3 run · 1 you · 62%");

        let tight: String = pulse(3, 1, Some(62.0))
            .pieces(Tier::Tight)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(tight, "3·1·62%");

        let pulse = pulse(3, 1, Some(62.0));
        assert!(
            pulse.tier_width(Tier::Tight) < pulse.tier_width(Tier::Compact),
            "the tight tier is not actually narrower than the compact one"
        );
        assert!(
            pulse.tier_width(Tier::Compact) < pulse.tier_width(Tier::Full),
            "the compact tier is not actually narrower than the full one"
        );
    }

    /// Every tier states every reading the fleet published. Narrowing may take
    /// away words; it may never take away a number.
    #[test]
    fn no_tier_drops_a_reading() {
        let pulse = pulse(3, 1, Some(62.0));
        for tier in Tier::ALL {
            let roles: Vec<Role> = pulse
                .pieces(tier)
                .into_iter()
                .map(|(role, _)| role)
                .filter(|role| *role != Role::Separator)
                .collect();
            assert_eq!(
                roles,
                [Role::Running, Role::Awaiting, Role::Quota],
                "{tier:?} does not state all three readings, in order"
            );
        }
    }

    /// The one thing the row may drop is a reading nobody published. A count of
    /// zero is a fact and stays.
    #[test]
    fn an_unpublished_quota_is_absent_and_a_zero_count_is_not() {
        let text: String = pulse(0, 0, None)
            .pieces(Tier::Full)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(text, "0 running · 0 needs you");
        assert!(!text.contains("quota"));

        let zeroed: String = pulse(0, 0, Some(0.0))
            .pieces(Tier::Full)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(zeroed, "0 running · 0 needs you · quota 0%");
    }

    /// The reserved width and the drawn width are the same number, or the
    /// session status beside the row is laid out over the top of it.
    #[test]
    fn every_tier_draws_exactly_the_width_it_reserves() {
        for reading in [(3, 1, Some(62.0)), (0, 0, None), (128, 99, Some(7.5))] {
            let pulse = pulse(reading.0, reading.1, reading.2);
            for tier in Tier::ALL {
                let drawn: usize = pulse
                    .pieces(tier)
                    .iter()
                    .map(|(_, text)| crate::ui::text::display_width(text))
                    .sum();
                assert_eq!(
                    drawn,
                    usize::from(pulse.tier_width(tier)),
                    "{tier:?} draws {drawn} columns but reserves {}",
                    pulse.tier_width(tier)
                );
            }
        }
    }

    #[test]
    fn the_widest_tier_that_fits_is_the_one_chosen() {
        let pulse = pulse(3, 1, Some(62.0));
        assert_eq!(pulse.tier(pulse.tier_width(Tier::Full)), Some(Tier::Full));
        assert_eq!(
            pulse.tier(pulse.tier_width(Tier::Full) - 1),
            Some(Tier::Compact)
        );
        assert_eq!(
            pulse.tier(pulse.tier_width(Tier::Compact) - 1),
            Some(Tier::Tight)
        );
        // One column short of the numbers alone: there is nothing honest left
        // to draw, so nothing is drawn.
        assert_eq!(pulse.tier(pulse.tier_width(Tier::Tight) - 1), None);
        assert_eq!(pulse.width(pulse.tier_width(Tier::Tight) - 1), 0);
        assert_eq!(pulse.tier(0), None);
    }

    /// The row is grey until it has something to say, and every alerting
    /// reading is distinguishable from a resting one on every bundled theme —
    /// `terminal` in particular collapses hues onto the resting grey.
    #[test]
    fn a_reading_that_alerts_is_never_drawn_the_way_a_quiet_one_is() {
        for (name, p) in [
            ("catppuccin", Palette::catppuccin()),
            ("catppuccin_latte", Palette::catppuccin_latte()),
            ("terminal", Palette::terminal()),
            ("tokyo_night", Palette::tokyo_night()),
            ("tokyo_night_day", Palette::tokyo_night_day()),
            ("dracula", Palette::dracula()),
            ("nord", Palette::nord()),
            ("gruvbox", Palette::gruvbox()),
            ("gruvbox_light", Palette::gruvbox_light()),
            ("one_dark", Palette::one_dark()),
            ("one_light", Palette::one_light()),
            ("solarized", Palette::solarized()),
            ("solarized_light", Palette::solarized_light()),
            ("kanagawa", Palette::kanagawa()),
            ("kanagawa_lotus", Palette::kanagawa_lotus()),
            ("rose_pine", Palette::rose_pine()),
            ("rose_pine_dawn", Palette::rose_pine_dawn()),
            ("vesper", Palette::vesper()),
        ] {
            let resting = Style::default().fg(p.overlay0);
            let quiet = pulse(0, 0, Some(10.0));
            for role in [Role::Running, Role::Awaiting, Role::Quota] {
                assert_eq!(
                    quiet.style(role, &p),
                    resting,
                    "{role:?} is not resting on a quiet fleet on {name}"
                );
            }

            let loud = pulse(2, 1, Some(95.0));
            for role in [Role::Running, Role::Awaiting, Role::Quota] {
                let style = loud.style(role, &p);
                assert_ne!(
                    style, resting,
                    "{role:?} is drawn identically alerting and at rest on {name}"
                );
                assert!(
                    style.add_modifier.contains(Modifier::BOLD),
                    "{role:?} is not bold when alerting on {name}"
                );
            }

            // The middle quota band is an alert too, and a distinct one.
            assert_ne!(
                pulse(0, 0, Some(80.0)).style(Role::Quota, &p),
                resting,
                "a three-quarters-spent quota reads as quiet on {name}"
            );
        }
    }

    /// Only the reading that means "go and look" moves, and only on an element
    /// the fleet signals already publish — this row must not be able to mount
    /// anything of its own.
    #[test]
    fn only_the_waiting_count_borrows_an_animation_element() {
        let quiet = pulse(4, 0, Some(20.0));
        for role in [Role::Running, Role::Awaiting, Role::Quota, Role::Separator] {
            assert_eq!(quiet.animated_element(role), None, "{role:?} moved at rest");
        }

        let mut signals = FleetSignals::test_reading(0, 2, None);
        signals.set_for_test(FleetSignal::Review);
        let waiting = Pulse { signals };
        assert_eq!(
            waiting.animated_element(Role::Awaiting),
            Some(FleetSignal::Review.element_id())
        );
        for role in [Role::Running, Role::Quota, Role::Separator] {
            assert_eq!(waiting.animated_element(role), None, "{role:?} moved");
        }
    }

    /// The flagship wins when more than one kind of waiting is live, so the
    /// motion a reader sees is always the most urgent thing's.
    #[test]
    fn the_most_urgent_live_signal_drives_the_motion() {
        let mut signals = FleetSignals::test_reading(0, 3, None);
        signals.set_for_test(FleetSignal::Stopped);
        signals.set_for_test(FleetSignal::Ask);
        signals.set_for_test(FleetSignal::Review);
        assert_eq!(
            Pulse { signals }.animated_element(Role::Awaiting),
            Some(FleetSignal::Ask.element_id())
        );
    }

    /// The row does not exist until it is configured on, whatever the fleet is
    /// doing.
    #[test]
    fn an_unconfigured_herdr_has_no_pulse_row() {
        let app = AppState::test_new();
        assert!(!app.fleet_pulse_active());
        assert!(Pulse::resolve(&app).is_none());
        assert_eq!(fleet_pulse_width(&app, 80), 0);
    }
}
