//! The fleet signal bar: eight fixed slots on the panel's reserved header row.
//!
//! The bar is always drawn once it is configured on, and always draws all eight
//! slots. A slot whose signal is quiet is its own name in the panel's muted
//! grey; a slot whose signal is live is its own colour, animated by
//! [`crate::anim`], and stays that way until the signal clears. That is the
//! whole design: the resting bar is the legend for the alerting bar, so a
//! reader learns what the eight things are by looking at a fleet where nothing
//! is happening.
//!
//! Three properties this module is responsible for holding:
//!
//! - **A slot never moves.** [`crate::app::fleet_signals::FleetSignal::ALL`] is
//!   the order, always, and no slot is ever omitted for being quiet. Position
//!   is half of what makes a one-cell mark readable at a glance, and a bar that
//!   packed only its live slots would make position meaningless.
//! - **Narrowing drops detail, never slots.** The width ladder gives up the
//!   names first and then the gaps between marks, so eight signals survive down
//!   to eight columns. Below that the bar draws nothing rather than a prefix of
//!   itself, because five of eight marks with no way to tell which five is
//!   worse than no bar at all.
//! - **Nothing here decides what is true.** Every reading comes from
//!   [`crate::app::fleet_signals`], and every frame from the animation engine.
//!   This module owns colour, glyph placement and width, and nothing else.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::fleet_signals::{FleetSignal, FleetSignals};
use crate::app::state::{AppState, Palette};

/// Columns between two slots, in every tier that has room for any.
const SLOT_GAP: u16 = 1;

/// Which form the bar draws in.
///
/// Ordered widest first, which is also the order [`Tier::widest_fitting`]
/// searches: the bar always shows as much as the panel can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tier {
    /// Every slot's mark and its name: `●review ◉ask ≡report …`.
    Named,
    /// Marks alone, one gap between them: `● ◉ ≡ ◐ ⊘ ~ ↑ ⋔`.
    Marks,
    /// Marks alone, no gaps: `●◉≡◐⊘~↑⋔`. One column per signal, which is the
    /// floor — there is no honest way to say eight things in seven columns.
    Tight,
}

impl Tier {
    const ALL: [Self; 3] = [Self::Named, Self::Marks, Self::Tight];

    /// Columns this tier occupies.
    ///
    /// Measured from the same table the renderer draws from, so the reserved
    /// width and the drawn width cannot drift apart the way they could if the
    /// number were written down here.
    pub(super) fn width(self) -> u16 {
        let slots = FleetSignal::COUNT as u16;
        let gaps = slots.saturating_sub(1);
        match self {
            Self::Named => {
                FleetSignal::ALL
                    .into_iter()
                    .map(|signal| crate::ui::text::display_width_u16(&slot_text(signal, self)))
                    .sum::<u16>()
                    + gaps * SLOT_GAP
            }
            Self::Marks => slots + gaps * SLOT_GAP,
            Self::Tight => slots,
        }
    }

    /// The most detailed tier that fits `available` columns, or `None` when not
    /// even one column per signal fits.
    pub(super) fn widest_fitting(available: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|tier| tier.width() <= available)
    }
}

/// What one slot draws in a given tier.
///
/// The named tier hangs the name straight off the mark with no space between
/// them — `●review` rather than `● review` — which buys back eight columns and
/// still reads as one thing, because the mark is a glyph and the name is a
/// word. The two narrow tiers draw the mark alone.
fn slot_text(signal: FleetSignal, tier: Tier) -> String {
    match tier {
        Tier::Named => format!("{}{}", signal.mark(), signal.name()),
        Tier::Marks | Tier::Tight => signal.mark().to_string(),
    }
}

/// The colour a slot draws in while its signal is live.
///
/// Eight signals, eight distinct palette roles, so no two live slots are told
/// apart by position alone. The four that restate something the tree already
/// says take the tree's own colour for it — `review` is the teal Herdr already
/// uses for a done-but-unseen pane, `ask` the red it uses for blocked, `busy`
/// the yellow it uses for working — so the bar and the rows under it never
/// disagree about what a colour means.
fn live_color(signal: FleetSignal, p: &Palette) -> Color {
    match signal {
        FleetSignal::Review => p.teal,
        FleetSignal::Ask => p.red,
        FleetSignal::Report => p.mauve,
        FleetSignal::Busy => p.yellow,
        FleetSignal::Stopped => p.peach,
        FleetSignal::Dirty => p.green,
        FleetSignal::Push => p.blue,
        FleetSignal::Pr => p.accent,
    }
}

/// Columns the bar will occupy on a header row `available` wide.
///
/// `0` when the bar is switched off or when the row cannot hold even the
/// tightest tier. The layout asks this before drawing so the session status
/// beside it knows what is left.
pub(super) fn fleet_signal_bar_width(app: &AppState, available: u16) -> u16 {
    if !app.fleet_signal_bar_active() {
        return 0;
    }
    Tier::widest_fitting(available).map_or(0, Tier::width)
}

/// Draw the bar into `area`, which must be exactly
/// [`fleet_signal_bar_width`] columns wide.
pub(super) fn render_fleet_signal_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 || !app.fleet_signal_bar_active() {
        return;
    }
    let Some(tier) = Tier::widest_fitting(area.width) else {
        return;
    };

    let signals = FleetSignals::resolve(app);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
        if index > 0 && tier != Tier::Tight {
            spans.push(Span::raw(" ".repeat(usize::from(SLOT_GAP))));
        }
        push_slot(&mut spans, app, &signals, signal, tier);
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

/// One slot's spans, resting or live.
///
/// A quiet slot is a plain grey span and asks the animation engine nothing — it
/// has no element, because an element that never moves is state the engine
/// would carry for no reason. A live slot takes its own colour and whatever
/// frame its element is on, which is what makes the change from resting to
/// alerting a change in both colour and motion rather than in colour alone.
fn push_slot(
    spans: &mut Vec<Span<'static>>,
    app: &AppState,
    signals: &FleetSignals,
    signal: FleetSignal,
    tier: Tier,
) {
    let text = slot_text(signal, tier);
    if !signals.is_live(signal) {
        spans.push(Span::styled(
            text,
            Style::default().fg(app.palette.overlay0),
        ));
        return;
    }

    let style = Style::default().fg(live_color(signal, &app.palette));
    let frame = app
        .anim
        .frame(&signal.element_id(), None)
        .filter(|frame| frame.behaviour.is_some());
    super::push_animated_span(
        spans,
        text,
        style,
        frame,
        &app.palette,
        &app.host_terminal_theme,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_gives_up_detail_before_it_gives_up_a_signal() {
        // Every tier still says all eight things; what shortens is how much it
        // says about each.
        assert_eq!(Tier::Tight.width(), FleetSignal::COUNT as u16);
        assert!(Tier::Marks.width() > Tier::Tight.width());
        assert!(Tier::Named.width() > Tier::Marks.width());
    }

    #[test]
    fn the_widest_tier_that_fits_is_the_one_chosen() {
        assert_eq!(Tier::widest_fitting(Tier::Named.width()), Some(Tier::Named));
        assert_eq!(
            Tier::widest_fitting(Tier::Named.width() - 1),
            Some(Tier::Marks)
        );
        assert_eq!(
            Tier::widest_fitting(Tier::Marks.width() - 1),
            Some(Tier::Tight)
        );
        // One column short of one column per signal: there is nothing honest
        // left to draw, so nothing is drawn.
        assert_eq!(Tier::widest_fitting(Tier::Tight.width() - 1), None);
        assert_eq!(Tier::widest_fitting(0), None);
    }

    /// The reserved width and the drawn width are the same number or the
    /// session status beside the bar is laid out over the top of it.
    #[test]
    fn every_tier_draws_exactly_the_width_it_reserves() {
        for tier in Tier::ALL {
            let gaps =
                usize::from(SLOT_GAP) * usize::from(tier != Tier::Tight) * (FleetSignal::COUNT - 1);
            let drawn = FleetSignal::ALL
                .into_iter()
                .map(|signal| crate::ui::text::display_width(&slot_text(signal, tier)))
                .sum::<usize>()
                + gaps;
            assert_eq!(
                drawn,
                usize::from(tier.width()),
                "{tier:?} draws {drawn} columns but reserves {}",
                tier.width()
            );
        }
    }

    #[test]
    fn the_named_tier_names_every_signal() {
        for signal in FleetSignal::ALL {
            let text = slot_text(signal, Tier::Named);
            assert!(text.starts_with(signal.mark()));
            assert!(
                text.ends_with(signal.name()),
                "{signal:?} does not draw its own name in the named tier"
            );
        }
    }

    #[test]
    fn no_two_live_signals_share_a_colour_and_none_is_the_resting_grey() {
        let p = Palette::catppuccin();
        let mut used: Vec<Color> = Vec::new();
        for signal in FleetSignal::ALL {
            let color = live_color(signal, &p);
            assert_ne!(
                color, p.overlay0,
                "{signal:?} is the same colour live as it is at rest"
            );
            assert!(
                !used.contains(&color),
                "{signal:?} reuses a colour another signal already has"
            );
            used.push(color);
        }
    }
}
