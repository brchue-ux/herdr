//! What one cell of an animated element looks like on one frame.
//!
//! The medium is a character-cell grid, so this type is the whole of what an
//! animation is allowed to say about a cell: its foreground, its background,
//! its attributes, how much of its own glyph is present, and — for a cell that
//! is pure decoration — which glyph that is. A glyph never moves, stretches, or
//! leaves its cell. That is a property of the terminal, not a gap in this type,
//! and it is why nothing here has a position delta.
//!
//! Four properties this module is responsible for holding:
//!
//! - **A cell paint is a patch, never a replacement.** Every field is optional
//!   or a tri-state, so an animation that says nothing about a cell leaves it
//!   exactly as the settled rendering drew it. Dropping every frame of an
//!   animation must leave the element identical to its unanimated self.
//! - **Colour is resolved once, in RGB.** Behaviours name inks symbolically
//!   ([`Ink`]) and a caller resolves them against the palette it is already
//!   drawing with, so the same behaviour reads correctly on a light theme, a
//!   dark theme, and whatever the host terminal actually reported.
//! - **Sub-cell resolution comes from the glyph set or not at all.** Text cells
//!   express coverage as a mix toward the background, because a letter cannot
//!   be half-drawn; filled cells express it through the eighth-block ramp,
//!   which genuinely resolves eight steps inside one cell. A decoration cell
//!   gets a third option: [`CellPaint::glyph`] swaps in a glyph of the *same
//!   display width*, which is how an effect resolves a position finer than a
//!   cell — or takes a shape no styling of the settled glyph could express.
//! - **A glyph substitution is an offer, not a command.** [`CellPaint::glyph`]
//!   is honoured only by a call site drawing pure decoration, through
//!   [`CellPaint::glyph_over`], which refuses any substitute whose display
//!   width differs from the glyph it would replace. [`CellPaint::text_style`]
//!   never applies one at all: an animation must not be able to garble a label
//!   it was only asked to emphasise.

use ratatui::style::{Modifier, Style};

use crate::ui::color::{mix_rgb, resolve_color_rgb, Rgb};

/// Where a cell sits inside the element being animated.
///
/// Element-relative, never screen-relative: an element that moves because the
/// sidebar scrolled or a pane was resized keeps painting the same way, and no
/// animation state has to be rewritten when geometry changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellPos {
    pub(crate) col: u16,
    pub(crate) row: u16,
}

impl CellPos {
    pub(crate) fn new(col: u16, row: u16) -> Self {
        Self { col, row }
    }

    /// The single-row case: a token span, a status glyph, a connector.
    pub(crate) fn col(col: u16) -> Self {
        Self { col, row: 0 }
    }
}

/// The element's own cell grid.
///
/// A token span is `cols × 1`; a pane's content area is its full rect. Both go
/// through the same field maths, which is what keeps a behaviour written for
/// one usable on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellExtent {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl CellExtent {
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// A single row of `cols` cells.
    pub(crate) fn row(cols: u16) -> Self {
        Self { cols, rows: 1 }
    }

    /// Normalised position of `pos` along each axis, in `0.0..=1.0`.
    ///
    /// A one-cell axis normalises to `0.0` rather than dividing by zero, so a
    /// sweep across a one-column element is simply uniform.
    pub(crate) fn normalize(self, pos: CellPos) -> (f32, f32) {
        fn axis(index: u16, extent: u16) -> f32 {
            let last = extent.saturating_sub(1);
            if last == 0 {
                return 0.0;
            }
            f32::from(index.min(last)) / f32::from(last)
        }
        (axis(pos.col, self.cols), axis(pos.row, self.rows))
    }

    pub(crate) fn is_empty(self) -> bool {
        self.cols == 0 || self.rows == 0
    }
}

/// Attribute changes an animation makes to a cell.
///
/// Tri-state per attribute for the same reason the sidebar's own token styling
/// is: `None` means "whatever the settled rendering decided", which is not the
/// same as "off".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AttrPatch {
    pub(crate) bold: Option<bool>,
    pub(crate) dim: Option<bool>,
    pub(crate) italic: Option<bool>,
    pub(crate) underline: Option<bool>,
    pub(crate) reverse: Option<bool>,
}

impl AttrPatch {
    pub(crate) const NONE: Self = Self {
        bold: None,
        dim: None,
        italic: None,
        underline: None,
        reverse: None,
    };

    pub(crate) const fn bold() -> Self {
        Self {
            bold: Some(true),
            ..Self::NONE
        }
    }

    pub(crate) const fn dim() -> Self {
        Self {
            dim: Some(true),
            ..Self::NONE
        }
    }

    pub(crate) const fn reverse() -> Self {
        Self {
            reverse: Some(true),
            ..Self::NONE
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self == Self::NONE
    }

    fn apply(self, mut style: Style) -> Style {
        fn set(style: Style, value: Option<bool>, modifier: Modifier) -> Style {
            match value {
                Some(true) => style.add_modifier(modifier),
                Some(false) => style.remove_modifier(modifier),
                None => style,
            }
        }
        style = set(style, self.bold, Modifier::BOLD);
        style = set(style, self.dim, Modifier::DIM);
        style = set(style, self.italic, Modifier::ITALIC);
        style = set(style, self.underline, Modifier::UNDERLINED);
        set(style, self.reverse, Modifier::REVERSED)
    }
}

/// A colour named by role rather than by value.
///
/// Behaviours are written against roles so one definition works on every theme.
/// The caller resolves them with [`InkPalette`] from whatever it is already
/// drawing with, which is also what keeps the host terminal's *measured*
/// palette authoritative rather than a second static table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    /// The surface this element composites against.
    Surface,
    /// The element's own settled foreground.
    Own,
    /// The palette accent.
    Accent,
    /// The colour of *what is being signalled* on this element right now.
    ///
    /// A role rather than a hue so one behaviour can carry a whole vocabulary:
    /// the caller resolves it from the category of the thing it is drawing —
    /// work arriving, work finishing, a failure, a branch going quiet — and the
    /// same catalogue entry then reads as four different signals.
    Signal,
    /// A literal colour, for a behaviour that genuinely means one hue.
    Fixed(Rgb),
}

/// The concrete colours [`Ink`] resolves against for one element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InkPalette {
    pub(crate) surface: Rgb,
    pub(crate) own: Rgb,
    pub(crate) accent: Rgb,
    /// What [`Ink::Signal`] resolves to. Defaults to the accent, so a call site
    /// with no category of its own still draws a signal behaviour correctly.
    pub(crate) signal: Rgb,
}

impl InkPalette {
    /// Resolve the three roles from the app palette and the style a call site
    /// was already going to draw with.
    ///
    /// `Color::Reset` and unresolvable indexed colours fall back to the palette
    /// surface, so an element drawn against the host's own background still
    /// animates instead of silently doing nothing.
    ///
    /// `surface` is what the call site's own panel is filled with, for the
    /// surfaces that have a fill of their own — the sidebar's ground is
    /// `palette.sidebar_bg` rather than `panel_bg`, and an element with no
    /// explicit background must composite against the colour on screen under it
    /// rather than against the app-wide panel colour. `None` says the call site
    /// draws on the app's panel background, which is the answer everywhere
    /// outside a panel with its own fill.
    pub(crate) fn resolve(
        base: Style,
        surface: Option<Rgb>,
        palette: &crate::app::state::Palette,
        host: &crate::terminal_theme::TerminalTheme,
    ) -> Self {
        let rgb = |color| resolve_color_rgb(color, host);
        let surface = base
            .bg
            .and_then(rgb)
            .or(surface)
            .or_else(|| rgb(palette.panel_bg))
            .unwrap_or(crate::ui::color::BLACK);
        let accent = rgb(palette.accent).unwrap_or(surface);
        Self {
            surface,
            own: base
                .fg
                .and_then(rgb)
                .or_else(|| rgb(palette.text))
                .unwrap_or(crate::ui::color::WHITE),
            accent,
            signal: accent,
        }
    }

    /// Bind [`Ink::Signal`] to the colour of what is actually being signalled.
    ///
    /// The colour is lifted away from the surface first, so a vocabulary
    /// entry that happens to sit close to the host terminal's background — a
    /// muted grey on a grey theme, a green on a green one — still arrives
    /// visible rather than invisible.
    pub(crate) fn with_signal(mut self, signal: Rgb) -> Self {
        self.signal =
            crate::ui::color::ensure_contrast(signal, self.surface, SIGNAL_CONTRAST_FLOOR);
        self
    }

    pub(crate) fn ink(self, ink: Ink) -> Rgb {
        match ink {
            Ink::Surface => self.surface,
            Ink::Own => self.own,
            Ink::Accent => self.accent,
            Ink::Signal => self.signal,
            Ink::Fixed(rgb) => rgb,
        }
    }
}

/// Contrast a signal colour must clear against the surface it lights up.
///
/// Below WCAG's text floors on purpose: a charge is a mark, not a label, and
/// forcing 4.5:1 would wash a whole vocabulary toward the same near-white on a
/// dark theme and the same near-black on a light one — destroying exactly the
/// hue separation the vocabulary exists to carry. This is the floor at which a
/// mark is unmistakably present.
const SIGNAL_CONTRAST_FLOOR: f32 = 2.2;

/// The eighth-block ramp, lightest first.
///
/// Nine entries so index `0` is genuinely empty: a ramp that starts at `▁`
/// cannot express "nothing here yet", which is exactly what the first frame of
/// a reveal needs.
const COVERAGE_BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// What an animation says about one cell on one frame.
///
/// Every field is a patch over the settled rendering. A default `CellPaint`
/// changes nothing, which is the state every animation returns to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellPaint {
    pub(crate) fg: Option<Rgb>,
    pub(crate) bg: Option<Rgb>,
    pub(crate) attrs: AttrPatch,
    /// How much of the cell's own glyph is present, in `0.0..=1.0`.
    ///
    /// `1.0` for anything that is not a reveal, so a caller that ignores this
    /// field still draws every non-revealing behaviour correctly.
    pub(crate) coverage: f32,
    /// A glyph offered in place of the cell's settled one.
    ///
    /// `None` for every behaviour that does not deal in shape, which is nearly
    /// all of them. Read it through [`CellPaint::glyph_over`] rather than
    /// directly: that is where the same-width rule is enforced, and it is the
    /// only reason a substitution cannot move a column.
    pub(crate) glyph: Option<char>,
}

impl Default for CellPaint {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            attrs: AttrPatch::NONE,
            coverage: 1.0,
            glyph: None,
        }
    }
}

impl CellPaint {
    /// True when this paint would draw the cell exactly as it already is.
    ///
    /// The per-frame diff is built on this: an element every one of whose cells
    /// is settled costs no repaint at all.
    pub(crate) fn is_settled(&self) -> bool {
        self.fg.is_none()
            && self.bg.is_none()
            && self.attrs.is_empty()
            && self.coverage >= 1.0
            && self.glyph.is_none()
    }

    /// The glyph this cell should actually draw, given the one it settles to.
    ///
    /// A substitute is taken only when it occupies exactly the columns the
    /// settled glyph did. That is the whole of what the old style-only rule was
    /// protecting: no cell count, no column, and no reserved width can move,
    /// whatever a behaviour asks for. A behaviour that asks for something wider
    /// or narrower is simply not honoured — a decoration is never allowed to
    /// break the thing it decorates.
    pub(crate) fn glyph_over(&self, settled: char) -> char {
        match self.glyph {
            Some(glyph) if display_width(glyph) == display_width(settled) => glyph,
            _ => settled,
        }
    }

    /// Fold this paint into the style a text cell was going to be drawn with.
    ///
    /// Coverage becomes a mix toward the surface rather than a partial glyph: a
    /// letter cannot be half-drawn, and dimming it toward the background is the
    /// honest cell-grid rendering of "not fully here yet". A cell with no
    /// coverage at all still draws its glyph in the surface colour rather than
    /// being blanked, so a reveal never changes the element's width mid-flight.
    pub(crate) fn text_style(&self, base: Style, palette: InkPalette) -> Style {
        let mut style = base;
        if let Some(fg) = self.fg {
            style = style.fg(rgb_color(fg));
        }
        if let Some(bg) = self.bg {
            style = style.bg(rgb_color(bg));
        }
        if self.coverage < 1.0 {
            let from = self
                .fg
                .or_else(|| color_rgb(style.fg))
                .unwrap_or(palette.own);
            let to = self
                .bg
                .or_else(|| color_rgb(style.bg))
                .unwrap_or(palette.surface);
            style = style.fg(rgb_color(mix_rgb(
                from,
                to,
                1.0 - self.coverage.clamp(0.0, 1.0),
            )));
        }
        self.attrs.apply(style)
    }

    /// The block glyph that represents this cell's coverage.
    ///
    /// For filled surfaces — a meter, a wash over a pane, a bar — where the
    /// glyph set really does resolve eight steps inside one cell. Text cells
    /// use [`text_style`](Self::text_style) instead.
    pub(crate) fn coverage_block(&self) -> char {
        let step = (self.coverage.clamp(0.0, 1.0) * 8.0).round() as usize;
        COVERAGE_BLOCKS[step.min(COVERAGE_BLOCKS.len() - 1)]
    }

    /// Quantized form used for per-frame diffing.
    ///
    /// Colour is compared at full 8-bit depth because that is what actually
    /// reaches the terminal, and coverage at the eight steps the block ramp can
    /// resolve. Anything finer than that is a difference no cell can show, so
    /// letting it request a repaint would be spending frames on nothing.
    pub(crate) fn digest(&self) -> u64 {
        fn channel(rgb: Option<Rgb>) -> u64 {
            match rgb {
                None => 0,
                Some((r, g, b)) => 1 << 24 | u64::from(r) << 16 | u64::from(g) << 8 | u64::from(b),
            }
        }
        fn tri(value: Option<bool>) -> u64 {
            match value {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            }
        }
        let attrs = tri(self.attrs.bold)
            | tri(self.attrs.dim) << 2
            | tri(self.attrs.italic) << 4
            | tri(self.attrs.underline) << 6
            | tri(self.attrs.reverse) << 8;
        let coverage = (self.coverage.clamp(0.0, 1.0) * 8.0).round() as u64;
        // A glyph swap is the coarsest difference a cell can show and the most
        // visible, so it is compared exactly rather than quantized.
        let glyph = self.glyph.map_or(0, |glyph| u64::from(glyph) | 1 << 32);
        channel(self.fg)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(17)
            ^ channel(self.bg).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ attrs.rotate_left(41)
            ^ coverage.rotate_left(53)
            ^ glyph.wrapping_mul(0xD6E8_FEB8_6659_FD93)
    }
}

/// Columns a glyph occupies, with anything unmeasurable treated as one.
///
/// Control characters and unassigned code points report `None`; a decoration
/// call site has already reserved one column for the glyph it settled to, so
/// treating an unmeasurable glyph as one column compares like with like rather
/// than silently letting a zero-width substitute through.
fn display_width(glyph: char) -> usize {
    unicode_width::UnicodeWidthChar::width(glyph).unwrap_or(1)
}

fn rgb_color(rgb: Rgb) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2)
}

fn color_rgb(color: Option<ratatui::style::Color>) -> Option<Rgb> {
    color.and_then(crate::ui::color::color_to_rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_paint_changes_nothing() {
        let paint = CellPaint::default();
        assert!(paint.is_settled());
        let base = Style::default().fg(ratatui::style::Color::Rgb(1, 2, 3));
        let palette = InkPalette {
            surface: (0, 0, 0),
            own: (1, 2, 3),
            accent: (9, 9, 9),
            signal: (9, 9, 9),
        };
        assert_eq!(paint.text_style(base, palette), base);
    }

    #[test]
    fn a_one_cell_axis_normalises_instead_of_dividing_by_zero() {
        let extent = CellExtent::new(1, 1);
        assert_eq!(extent.normalize(CellPos::new(0, 0)), (0.0, 0.0));
        // And an out-of-range cell clamps rather than running past 1.0.
        assert_eq!(
            CellExtent::new(4, 1).normalize(CellPos::col(99)),
            (1.0, 0.0)
        );
    }

    #[test]
    fn coverage_resolves_eight_steps_inside_one_cell() {
        let blocks: Vec<char> = (0..=8)
            .map(|step| {
                CellPaint {
                    coverage: step as f32 / 8.0,
                    ..CellPaint::default()
                }
                .coverage_block()
            })
            .collect();
        assert_eq!(blocks, COVERAGE_BLOCKS.to_vec());
    }

    #[test]
    fn partial_coverage_dims_text_toward_the_surface_without_blanking_it() {
        let palette = InkPalette {
            surface: (0, 0, 0),
            own: (200, 200, 200),
            accent: (0, 0, 255),
            signal: (0, 0, 255),
        };
        let base = Style::default().fg(ratatui::style::Color::Rgb(200, 200, 200));
        let half = CellPaint {
            coverage: 0.5,
            ..CellPaint::default()
        };
        assert_eq!(
            half.text_style(base, palette).fg,
            Some(ratatui::style::Color::Rgb(100, 100, 100))
        );
        // Fully uncovered still resolves to a colour, never to "no glyph": the
        // element must not change width part-way through a reveal.
        let none = CellPaint {
            coverage: 0.0,
            ..CellPaint::default()
        };
        assert_eq!(
            none.text_style(base, palette).fg,
            Some(ratatui::style::Color::Rgb(0, 0, 0))
        );
    }

    #[test]
    fn an_attribute_patch_is_tri_state() {
        let base = Style::default().add_modifier(Modifier::DIM | Modifier::BOLD);
        let patch = AttrPatch {
            bold: Some(false),
            italic: Some(true),
            ..AttrPatch::NONE
        };
        let style = patch.apply(base);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
        assert!(style.sub_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        // Untouched attributes survive: `None` is not `Some(false)`.
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_digest_ignores_changes_no_cell_could_show() {
        let a = CellPaint {
            coverage: 0.500,
            ..CellPaint::default()
        };
        let b = CellPaint {
            coverage: 0.505,
            ..CellPaint::default()
        };
        assert_eq!(a.digest(), b.digest(), "sub-step coverage is not a frame");

        // But a difference the terminal can actually draw is one.
        let c = CellPaint {
            coverage: 0.625,
            ..CellPaint::default()
        };
        assert_ne!(a.digest(), c.digest());
    }

    #[test]
    fn a_substitution_of_the_wrong_width_is_refused_rather_than_drawn() {
        let paint = |glyph| CellPaint {
            glyph: Some(glyph),
            ..CellPaint::default()
        };
        // A wide substitute would push every column right of it one cell over,
        // which is exactly the failure the old style-only rule existed to stop.
        assert_eq!(paint('한').glyph_over('─'), '─');
        // A same-width one is taken, because that is the whole point.
        assert_eq!(paint('╫').glyph_over('─'), '╫');
        // Including over a blank, which a foreground colour alone could never
        // ink — the connector's third cell is a space.
        assert_eq!(paint('▌').glyph_over(' '), '▌');
        // And a paint offering nothing leaves the settled glyph alone.
        assert_eq!(CellPaint::default().glyph_over('├'), '├');
    }

    #[test]
    fn a_paint_that_offers_a_glyph_is_never_settled() {
        // Or the per-frame diff would skip the frame that takes it away again,
        // and a discharge would be left burned into the line.
        let arc = CellPaint {
            glyph: Some('╫'),
            ..CellPaint::default()
        };
        assert!(!arc.is_settled());
        assert_ne!(arc.digest(), CellPaint::default().digest());
        // Two different marks are two different frames.
        let other = CellPaint {
            glyph: Some('╪'),
            ..CellPaint::default()
        };
        assert_ne!(arc.digest(), other.digest());
    }

    #[test]
    fn text_never_takes_a_glyph_substitution() {
        // The line the amended rule draws: decoration may change shape, a label
        // may not. `text_style` is the path every label takes.
        let palette = InkPalette {
            surface: (0, 0, 0),
            own: (200, 200, 200),
            accent: (0, 0, 255),
            signal: (0, 255, 0),
        };
        let base = Style::default().fg(ratatui::style::Color::Rgb(200, 200, 200));
        let arc = CellPaint {
            glyph: Some('╫'),
            ..CellPaint::default()
        };
        assert_eq!(
            arc.text_style(base, palette),
            base,
            "a glyph offer must not leak into a label's styling either"
        );
    }

    #[test]
    fn the_digest_separates_a_missing_colour_from_a_black_one() {
        let absent = CellPaint::default();
        let black = CellPaint {
            fg: Some((0, 0, 0)),
            ..CellPaint::default()
        };
        assert_ne!(absent.digest(), black.digest());
        // And foreground is not confusable with background.
        let black_bg = CellPaint {
            bg: Some((0, 0, 0)),
            ..CellPaint::default()
        };
        assert_ne!(black.digest(), black_bg.digest());
    }
}
