//! The sidebar tree's card shell.
//!
//! A tree row is a stack of token lines. The card is the frame drawn around
//! that stack: a border on all four sides, the state mark promoted to a chip on
//! the first content row, the state label repeated as a pill on the last, and a
//! static bottom-up glow that makes the box read as lit from its closing rule.
//!
//! Nothing here moves. The glow is a gradient — a shape, not a motion — so a
//! card looks the same in a still capture as it does on screen, and the shell
//! needs no animation primitive to be correct.
//!
//! The shell is decoration *around* the row, never a new vocabulary: the chip
//! carries whatever [`crate::ui::status::state_mark`] returns and the pill
//! carries whatever [`crate::ui::status::state_label`] returns, so replacing
//! either alphabet stays the one-place edit it is today.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::app::state::Palette;
use crate::terminal_theme::TerminalTheme;
use crate::ui::color::{ensure_contrast, mix_rgb, resolve_color_rgb, Rgb};

/// Rows the shell spends on itself: the top border and the bottom rule.
///
/// The layout adds these to a row's folded line count and the renderer draws
/// them at exactly these offsets. If the two ever disagree, a row's reserved
/// height and its drawn lines part company — the same failure
/// [`super::tree_prefix_width`] exists to prevent on the horizontal axis.
pub(super) const CHROME_ROWS: u16 = 2;

/// Columns the shell spends on itself: the two borders, and the pad inside each.
pub(super) const CHROME_COLS: u16 = 4;

/// Columns the chip pads a one-cell state mark out to.
///
/// The mark keeps its own column and gains one of padding on each side, which
/// is what turns it from a character on a line into a plate on a card. Every
/// mark is one cell wide (`state_marks_are_one_column_wide`), so this is a
/// constant rather than a measurement — and it is the two columns the card
/// costs a row's name over and above the frame's four.
const CHIP_WIDTH: usize = 3;

/// Columns between the subtitle and the status pill sharing its row.
const PILL_GAP: u16 = 1;

/// The narrowest fold width the card shell is drawn at.
///
/// Below this the card stops being a card and becomes a frame around an
/// ellipsis, so the tree falls back to today's styled line rather than spend
/// six columns saying less.
///
/// The number is the report's own arithmetic, taken at the display-depth cap
/// where the tree is narrowest. A worker row's subtitle line spends 7 columns
/// on rails, 4 on the frame and its pads, [`PILL_GAP`] before the pill, and 9
/// on the widest state pill (`▐blocked▌`), so a sidebar `W` columns wide leaves
/// `W - 23` for the subtitle itself. Eleven columns — `scout · iam`, the
/// shortest real subtitle in the fleet this geometry was measured against —
/// puts the floor at `W = 34`. Fold width is `W - 2`, hence 32.
///
/// Deliberately a whole-panel decision measured against
/// [`super::row_fold_width`], not a per-row one: a tree that drew cards at
/// depth 0 and bare lines at depth 2 would be two layouts stacked on top of
/// each other, and a fold width that depended on what it produced would feed
/// its own input.
pub(super) const MIN_FOLD_WIDTH: u16 = 32;

/// How a tree row is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowShell {
    /// Today's styled line: rails, then tokens, no frame.
    Line,
    /// The card: the row's lines wrapped in a bordered, lit box.
    Card,
}

impl RowShell {
    /// The shell a panel this wide draws, for every row in it.
    pub(super) fn for_fold_width(fold_width: u16) -> Self {
        if fold_width >= MIN_FOLD_WIDTH {
            Self::Card
        } else {
            Self::Line
        }
    }

    pub(super) fn is_card(self) -> bool {
        self == Self::Card
    }

    /// Rows this shell adds around a row's content lines.
    pub(super) fn chrome_rows(self) -> u16 {
        match self {
            Self::Line => 0,
            Self::Card => CHROME_ROWS,
        }
    }

    /// Columns this shell takes out of a row's content budget.
    pub(super) fn chrome_cols(self) -> u16 {
        match self {
            Self::Line => 0,
            Self::Card => CHROME_COLS,
        }
    }
}

/// The glow's ramp, top border to closing rule.
///
/// Four stops rather than a straight line because a linear ramp reads as a flat
/// wash: the light has to stay low through the title and gather under the rule
/// for the card to look lit from below rather than tinted all over.
const GLOW_STOPS: [f32; 4] = [0.05, 0.10, 0.16, 0.26];

/// The same ramp for the row the cursor is on, lifted rather than recoloured.
///
/// Selection is a change of intensity, not of hue: a selected blocked card must
/// still read as blocked.
const GLOW_STOPS_LIFTED: [f32; 4] = [0.10, 0.18, 0.26, 0.38];

/// How far toward the state hue a card's edges and plates sit, over the panel.
const TOP_BORDER_MIX: f32 = 0.55;
const SIDE_BORDER_MIX: f32 = 0.50;
const BOTTOM_RULE_MIX: f32 = 0.35;
const CHIP_MIX: f32 = 0.30;
const PILL_MIX: f32 = 0.62;

/// Contrast floor for text drawn on one of the shell's own plates.
///
/// The chip and the pill print the panel background over a tinted fill. On the
/// themes the mixes above were chosen against that reads as a plate — dark ink
/// on a lit key — and every one of them clears this floor untouched. The floor
/// exists for the palettes Herdr did not pick: a custom theme or a host that
/// reported something unexpected can put the fill anywhere, and dark-on-dark is
/// not a plate, it is a hole.
///
/// WCAG's large-text threshold rather than its body-text one, because both
/// plates print a handful of bold characters and the stricter floor would
/// relight the default themes to fix a problem they do not have.
const PLATE_CONTRAST_FLOOR: f32 = 3.0;

/// Where a card's glow amount lands `row` rows down a card `height` rows tall.
///
/// Piecewise-linear through [`GLOW_STOPS`], so the default four-row card hits
/// the four stops exactly and a taller card (more configured token lines) still
/// ramps across the same range instead of stopping short.
fn glow_amount(stops: &[f32; 4], row: u16, height: u16) -> f32 {
    let position = if height <= 1 {
        1.0
    } else {
        f32::from(row.min(height - 1)) / f32::from(height - 1)
    };
    let scaled = (position * 3.0).clamp(0.0, 3.0);
    let stop = (scaled.floor() as usize).min(2);
    let fraction = scaled - stop as f32;
    stops[stop] + (stops[stop + 1] - stops[stop]) * fraction
}

/// One card's colours, resolved once for the whole box.
///
/// The ground is the panel's own fill when the theme paints one — a card's
/// gradient, glow and plates all land on it, and the plate's legibility floor is
/// measured against it, so a mix toward a colour that is nowhere on screen is a
/// visible seam around every card.
///
/// `None` for every mix when neither the panel fill nor the panel background has
/// a colour of its own — `Color::Reset` under the terminal theme means "whatever
/// the host is using", and a gradient toward an unknown colour is a guess. The
/// card still draws its frame in the state hue; it just does not tint what it
/// cannot measure.
struct CardInk {
    accent: Rgb,
    panel: Option<Rgb>,
}

impl CardInk {
    fn new(accent: Color, p: &Palette, host: &TerminalTheme) -> Option<Self> {
        Some(Self {
            accent: resolve_color_rgb(accent, host)?,
            panel: super::panel_fill_rgb(p, host).or_else(|| resolve_color_rgb(p.panel_bg, host)),
        })
    }

    /// The accent moved `amount` of the way toward the panel background.
    fn toward_panel(&self, amount: f32) -> Color {
        match self.panel {
            Some(panel) => rgb(mix_rgb(self.accent, panel, amount)),
            None => rgb(self.accent),
        }
    }

    /// The panel background lifted `amount` of the way toward the accent.
    fn glow(&self, amount: f32) -> Option<Color> {
        self.panel
            .map(|panel| rgb(mix_rgb(panel, self.accent, amount)))
    }

    /// One of the shell's plates — the chip or the pill — as `(fill, ink)`.
    ///
    /// Dark ink on a lit key, always. When a state's fill is too close to the
    /// panel for that to be legible, the *key* is brightened rather than the
    /// ink: lifting the ink instead would flip those two states to light-on-dark
    /// and leave the pill reading as two different controls depending on which
    /// state it happened to be reporting.
    fn plate(&self, mix: f32) -> (Color, Color) {
        let Some(panel) = self.panel else {
            return (rgb(self.accent), rgb(self.accent));
        };
        let fill = ensure_contrast(
            mix_rgb(self.accent, panel, mix),
            panel,
            PLATE_CONTRAST_FLOOR,
        );
        (rgb(fill), rgb(panel))
    }
}

fn rgb(color: Rgb) -> Color {
    Color::Rgb(color.0, color.1, color.2)
}

/// One row's card, resolved from the layout's frame rect and the row's state.
pub(super) struct Card<'a> {
    /// The box, in screen coordinates. Its columns are measured against the
    /// fold width rather than the drawn width, so the frame's right edge cannot
    /// move when the scrollbar comes and goes.
    frame: Rect,
    /// The state hue everything on this card ramps toward.
    accent: Color,
    /// Whether this row is the selected/active one.
    lifted: bool,
    p: &'a Palette,
    host: &'a TerminalTheme,
}

impl<'a> Card<'a> {
    pub(super) fn new(
        frame: Rect,
        accent: Color,
        lifted: bool,
        p: &'a Palette,
        host: &'a TerminalTheme,
    ) -> Option<Self> {
        (frame.width > CHROME_COLS && frame.height > CHROME_ROWS).then_some(Self {
            frame,
            accent,
            lifted,
            p,
            host,
        })
    }

    /// Columns inside the frame and its pads, before any control drawn over the
    /// row takes its share.
    pub(super) fn content_width(&self) -> u16 {
        self.frame.width.saturating_sub(CHROME_COLS)
    }

    /// The chip the state mark is drawn in on the first content row.
    ///
    /// Returned as a `(text, style)` pair shaped exactly like
    /// [`crate::ui::status::state_dot`]'s, so the token layout measures and
    /// places it as the state icon it still is — the chip is padding and a
    /// fill, not a different token.
    pub(super) fn chip(&self, mark: &str) -> (String, Style) {
        let plate = format!("{mark:^CHIP_WIDTH$}");
        let ink = CardInk::new(self.accent, self.p, self.host);
        let Some(ink) = ink else {
            return (plate, Style::default().fg(self.accent));
        };
        let (fill, text) = ink.plate(CHIP_MIX);
        (
            plate,
            Style::default()
                .fg(text)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        )
    }

    /// Columns a pill for `label` needs, caps included.
    pub(super) fn pill_width(label: &str) -> u16 {
        crate::ui::text::display_width_u16(label).saturating_add(2)
    }

    /// Columns the pill takes out of the row it shares, gap included, or zero
    /// when it does not fit and is dropped.
    pub(super) fn pill_reservation(&self, label: &str) -> u16 {
        let width = Self::pill_width(label).saturating_add(PILL_GAP);
        // A pill that leaves nothing for the subtitle is not a status readout,
        // it is a status readout wearing the row.
        if width >= self.content_width() {
            0
        } else {
            width
        }
    }

    /// Paint the glow before the row's content is drawn.
    ///
    /// First, not last: the chip and the pill carry fills of their own, and a
    /// gradient painted over them would erase the plates that make them plates.
    pub(super) fn render_glow(&self, frame: &mut Frame, list_bottom: u16) {
        let Some(ink) = CardInk::new(self.accent, self.p, self.host) else {
            return;
        };
        let stops = if self.lifted {
            &GLOW_STOPS_LIFTED
        } else {
            &GLOW_STOPS
        };
        let buf = frame.buffer_mut();
        for row in 0..self.frame.height {
            let y = self.frame.y + row;
            if y >= list_bottom {
                break;
            }
            let Some(bg) = ink.glow(glow_amount(stops, row, self.frame.height)) else {
                continue;
            };
            for x in self.frame.x..self.frame.x + self.frame.width {
                buf[(x, y)].set_style(Style::default().bg(bg));
            }
        }
    }

    /// Draw the frame, and the pill on the last content row, over the content
    /// the row already drew.
    ///
    /// Last, because the border columns sit inside the rect the row's paragraph
    /// is rendered into: a frame drawn first would be a frame the row could
    /// overwrite with a long enough name.
    pub(super) fn render_frame(&self, frame: &mut Frame, list_bottom: u16, pill: Option<&Pill>) {
        let ink = CardInk::new(self.accent, self.p, self.host);
        let hue = |amount: f32| {
            ink.as_ref()
                .map(|ink| ink.toward_panel(amount))
                .unwrap_or(self.accent)
        };
        let stops = if self.lifted {
            &GLOW_STOPS_LIFTED
        } else {
            &GLOW_STOPS
        };
        let glow = |row: u16| {
            ink.as_ref()
                .and_then(|ink| ink.glow(glow_amount(stops, row, self.frame.height)))
        };

        let bottom = self.frame.height.saturating_sub(1);
        for row in 0..self.frame.height {
            let y = self.frame.y + row;
            if y >= list_bottom {
                return;
            }
            let (left, right, style) = if row == 0 {
                ("╭", "╮", Style::default().fg(hue(TOP_BORDER_MIX)))
            } else if row == bottom {
                (
                    "╰",
                    "╯",
                    Style::default()
                        .fg(hue(BOTTOM_RULE_MIX))
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("│", "│", Style::default().fg(hue(SIDE_BORDER_MIX)))
            };
            let style = match glow(row) {
                Some(bg) => style.bg(bg),
                None => style,
            };
            let buf = frame.buffer_mut();
            buf[(self.frame.x, y)].set_symbol(left);
            buf[(self.frame.x, y)].set_style(style);
            let right_x = self.frame.x + self.frame.width - 1;
            buf[(right_x, y)].set_symbol(right);
            buf[(right_x, y)].set_style(style);
            if row == 0 || row == bottom {
                for x in self.frame.x + 1..right_x {
                    buf[(x, y)].set_symbol("─");
                    buf[(x, y)].set_style(style);
                }
            }
        }

        if let Some(pill) = pill {
            self.render_pill(frame, pill, list_bottom);
        }
    }

    /// The status pill, right-aligned on the card's last content row.
    ///
    /// Drawn over the row rather than laid out in it, exactly as the worker
    /// summary badge is, which is why the row reserved
    /// [`Card::pill_reservation`] columns for it before its tokens were placed.
    fn render_pill(&self, frame: &mut Frame, pill: &Pill, list_bottom: u16) {
        let width = Self::pill_width(&pill.label);
        if width.saturating_add(PILL_GAP) >= self.content_width() {
            return;
        }
        let bottom = self.frame.height.saturating_sub(1);
        let Some(row) = bottom.checked_sub(1) else {
            return;
        };
        let y = self.frame.y + row;
        if y >= list_bottom {
            return;
        }
        let Some(ink) = CardInk::new(self.accent, self.p, self.host) else {
            return;
        };
        let stops = if self.lifted {
            &GLOW_STOPS_LIFTED
        } else {
            &GLOW_STOPS
        };
        let back = ink.glow(glow_amount(stops, row, self.frame.height));
        let (fill, text) = ink.plate(PILL_MIX);
        let cap = match back {
            Some(bg) => Style::default().fg(fill).bg(bg),
            None => Style::default().fg(fill),
        };
        let label_style = Style::default()
            .fg(text)
            .bg(fill)
            .add_modifier(Modifier::BOLD);

        // The pill closes one column inside the right border, over the pad the
        // subtitle was never allowed to reach.
        let x = self.frame.x + self.frame.width - 1 - width;
        let buf = frame.buffer_mut();
        buf[(x, y)].set_symbol("▐");
        buf[(x, y)].set_style(cap);
        let label_x = x + 1;
        buf.set_stringn(
            label_x,
            y,
            &pill.label,
            usize::from(width.saturating_sub(2)),
            label_style,
        );
        let close_x = x + width - 1;
        buf[(close_x, y)].set_symbol("▌");
        buf[(close_x, y)].set_style(cap);
    }
}

/// The status readout a card repeats at its foot.
pub(super) struct Pill {
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything a card mixes — its borders, its glow, and the plates whose
    /// legibility floor is measured against it — lands on the panel's own fill
    /// when a theme paints one. A card lit against a colour nowhere on screen
    /// is a seam around every row.
    #[test]
    fn a_cards_gradient_and_plates_are_measured_against_the_panels_own_fill() {
        let host = TerminalTheme::default();
        let mut p = Palette::catppuccin();
        p.panel_bg = Color::Rgb(30, 30, 46);

        // No fill: the panel background, exactly as before.
        let plain = CardInk::new(Color::Rgb(200, 200, 200), &p, &host).expect("an accent resolves");
        assert_eq!(plain.panel, Some((30, 30, 46)));

        // A theme fill is what the card is actually drawn on.
        p.sidebar_bg = Color::Rgb(12, 34, 56);
        let filled =
            CardInk::new(Color::Rgb(200, 200, 200), &p, &host).expect("an accent resolves");
        assert_eq!(filled.panel, Some((12, 34, 56)));
        assert_eq!(filled.plate(CHIP_MIX).1, Color::Rgb(12, 34, 56));

        // And a panel with neither still declines to tint what it cannot
        // measure.
        p.sidebar_bg = Color::Reset;
        p.panel_bg = Color::Reset;
        let unmeasured =
            CardInk::new(Color::Rgb(200, 200, 200), &p, &host).expect("an accent resolves");
        assert_eq!(unmeasured.panel, None);
    }

    #[test]
    fn the_shell_is_a_whole_panel_decision_taken_at_one_threshold() {
        assert_eq!(RowShell::for_fold_width(MIN_FOLD_WIDTH), RowShell::Card);
        assert_eq!(RowShell::for_fold_width(MIN_FOLD_WIDTH - 1), RowShell::Line);
        assert_eq!(RowShell::for_fold_width(0), RowShell::Line);
    }

    /// The floor is the report's arithmetic, not a taste: at the display-depth
    /// cap a card has to hold the widest pill and a readable subtitle beside
    /// it. Spelling the sum out here is what stops the constant drifting away
    /// from the reason it has that value.
    #[test]
    fn the_narrow_fallback_threshold_is_the_width_the_subtitle_stops_fitting_at() {
        const DEEPEST_RAIL: u16 = 7; // tree_prefix_width(2, false, 0)
        const WIDEST_PILL: u16 = 9; // "▐blocked▌"
        const SHORTEST_SUBTITLE: u16 = 11; // "scout · iam"
        let floor = DEEPEST_RAIL + CHROME_COLS + PILL_GAP + WIDEST_PILL + SHORTEST_SUBTITLE;
        assert_eq!(floor, MIN_FOLD_WIDTH);
    }

    #[test]
    fn a_four_row_card_lands_on_the_ramp_stops_exactly() {
        for (row, stop) in GLOW_STOPS.iter().enumerate() {
            let amount = glow_amount(&GLOW_STOPS, row as u16, 4);
            assert!(
                (amount - stop).abs() < 0.0005,
                "row {row} gave {amount}, wanted {stop}"
            );
        }
    }

    /// The glow is a *shape*: it only ever brightens downward, so the card
    /// reads as lit from its closing rule whatever height its token layout
    /// gives it.
    #[test]
    fn the_glow_only_ever_ramps_downward() {
        for height in 3..=8u16 {
            for stops in [&GLOW_STOPS, &GLOW_STOPS_LIFTED] {
                for row in 1..height {
                    assert!(
                        glow_amount(stops, row, height) > glow_amount(stops, row - 1, height),
                        "height {height} row {row} did not brighten"
                    );
                }
            }
        }
    }

    #[test]
    fn the_selected_ramp_lifts_the_card_without_recolouring_it() {
        for row in 0..4u16 {
            assert!(glow_amount(&GLOW_STOPS_LIFTED, row, 4) > glow_amount(&GLOW_STOPS, row, 4));
        }
    }

    /// The chip is padding around the mark, never a mark of its own: whatever
    /// `state_mark` returns comes back out of it unchanged, one column wider on
    /// each side.
    #[test]
    fn the_chip_pads_the_mark_it_was_given_and_changes_nothing_else() {
        let p = Palette::catppuccin();
        let host = TerminalTheme::default();
        let card = Card::new(Rect::new(0, 0, 20, 4), p.yellow, false, &p, &host)
            .expect("frame is wide enough for a card");
        for mark in ["!", ">", "-", " "] {
            let (text, _) = card.chip(mark);
            assert_eq!(crate::ui::text::display_width(&text), CHIP_WIDTH);
            assert_eq!(text.trim_matches(' '), mark.trim_matches(' '));
        }
    }

    #[test]
    fn a_pill_that_would_leave_no_subtitle_is_dropped() {
        let p = Palette::catppuccin();
        let host = TerminalTheme::default();
        let card = Card::new(Rect::new(0, 0, 20, 4), p.yellow, false, &p, &host)
            .expect("frame is wide enough for a card");
        assert_eq!(card.content_width(), 16);
        assert!(card.pill_reservation("working") > 0);
        assert_eq!(card.pill_reservation("a-very-long-state-label"), 0);
    }

    #[test]
    fn a_frame_with_no_room_inside_it_is_not_a_card() {
        let p = Palette::catppuccin();
        let host = TerminalTheme::default();
        assert!(Card::new(Rect::new(0, 0, CHROME_COLS, 4), p.yellow, false, &p, &host).is_none());
        assert!(Card::new(Rect::new(0, 0, 20, CHROME_ROWS), p.yellow, false, &p, &host).is_none());
    }
}
