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
    style::{Color, Modifier, Style},
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
/// Eight signals, eight different palette roles. The ones that restate
/// something the tree already
/// says take the tree's own colour for it — `review` is the teal Herdr already
/// uses for a done-but-unseen pane, `ask` the red it uses for blocked, `busy`
/// the yellow it uses for working — so the bar and the rows under it never
/// disagree about what a colour means.
///
/// Deliberately not `accent`. The palette has six hues plus `text`, and
/// `accent` is a *copy* of one of them — `blue` in every bundled theme — so a
/// slot drawn in it would be indistinguishable from `push`. It is user-settable
/// on top of that, so any slot bound to it could be made to collide with any
/// other at will. `pr` takes the bright neutral instead.
///
/// Two live slots *may* share a colour, and on some bundled themes they do:
/// `dracula` gives `blue` and `teal` the same value, `vesper` does the same to
/// `peach` and `yellow`. That is survivable for exactly the reason Herdr's own
/// state dots are: a slot is identified by its mark and its fixed position, and
/// colour only has to say live-or-resting. What is *not* survivable is a live
/// slot that draws in the resting grey, which is what `terminal` would do to
/// `report` — see [`live_style`].
fn live_color(signal: FleetSignal, p: &Palette) -> Color {
    match signal {
        FleetSignal::Review => p.teal,
        FleetSignal::Ask => p.red,
        FleetSignal::Report => p.mauve,
        FleetSignal::Busy => p.yellow,
        FleetSignal::Stopped => p.peach,
        FleetSignal::Dirty => p.green,
        FleetSignal::Push => p.blue,
        FleetSignal::Pr => p.text,
    }
}

/// How a live slot is drawn, as against [`resting_style`].
///
/// Bold as well as coloured. Bold is the one distinction from the resting state
/// that no palette can take away, and some can: the `terminal` theme resolves
/// `mauve` to the same value as `overlay0`, which would leave a live `report`
/// looking exactly like a quiet one. The colour falls back to `text` whenever it
/// would land on the resting grey, and the weight covers whatever is left.
fn live_style(signal: FleetSignal, p: &Palette) -> Style {
    let color = live_color(signal, p);
    let color = if color == p.overlay0 { p.text } else { color };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// How a quiet slot is drawn: named, muted, and holding still.
fn resting_style(p: &Palette) -> Style {
    Style::default().fg(p.overlay0)
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
        spans.push(Span::styled(text, resting_style(&app.palette)));
        return;
    }

    let style = live_style(signal, &app.palette);
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

    /// A live slot must never be drawable as a resting one. Colour alone
    /// cannot promise that: the `terminal` theme resolves `mauve` to the same
    /// value as `overlay0`, and `test_new` collapses the whole palette, so the
    /// live style falls back to `text` and carries bold on top.
    ///
    /// Note what is *not* asserted: that no two live signals share a colour.
    /// `dracula` gives `blue` and `teal` one value and `vesper` does the same to
    /// `peach` and `yellow`, so on those themes two slots genuinely match. That
    /// is the same trade Herdr's own state dots make - identity is carried by a
    /// distinct one-cell mark in a fixed position (see
    /// `every_mark_is_one_cell_and_no_two_are_the_same`), and colour only has to
    /// say live-or-resting.
    #[test]
    fn a_live_slot_is_never_drawn_the_way_a_resting_one_is() {
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
            let resting = resting_style(&p);
            for signal in FleetSignal::ALL {
                let live = live_style(signal, &p);
                assert_ne!(
                    live, resting,
                    "{signal:?} is drawn identically live and at rest on {name}"
                );
                assert!(
                    live.add_modifier.contains(Modifier::BOLD),
                    "{signal:?} is not bold when live on {name}"
                );
            }
        }
    }
}
