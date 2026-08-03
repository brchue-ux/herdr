//! What one cell of an animated element looks like on one frame.
//!
//! The medium is a character-cell grid, so this type is the whole of what an
//! animation is allowed to say about a cell: its foreground, its background,
//! its attributes, and how much of its own glyph is present. A glyph never
//! moves, stretches, or leaves its cell. That is a property of the terminal,
//! not a gap in this type, and it is why nothing here has a position delta.
//!
//! Three properties this module is responsible for holding:
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
//!   which genuinely resolves eight steps inside one cell.

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
    /// A literal colour, for a behaviour that genuinely means one hue.
    Fixed(Rgb),
}

/// The concrete colours [`Ink`] resolves against for one element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InkPalette {
    pub(crate) surface: Rgb,
    pub(crate) own: Rgb,
    pub(crate) accent: Rgb,
}

impl InkPalette {
    /// Resolve the three roles from the app palette and the style a call site
    /// was already going to draw with.
    ///
    /// `Color::Reset` and unresolvable indexed colours fall back to the palette
    /// surface, so an element drawn against the host's own background still
    /// animates instead of silently doing nothing.
    pub(crate) fn resolve(
        base: Style,
        palette: &crate::app::state::Palette,
        host: &crate::terminal_theme::TerminalTheme,
    ) -> Self {
        let rgb = |color| resolve_color_rgb(color, host);
        let surface = base
            .bg
            .and_then(rgb)
            .or_else(|| rgb(palette.panel_bg))
            .unwrap_or(crate::ui::color::BLACK);
        Self {
            surface,
            own: base
                .fg
                .and_then(rgb)
                .or_else(|| rgb(palette.text))
                .unwrap_or(crate::ui::color::WHITE),
            accent: rgb(palette.accent).unwrap_or(surface),
        }
    }

    pub(crate) fn ink(self, ink: Ink) -> Rgb {
        match ink {
            Ink::Surface => self.surface,
            Ink::Own => self.own,
            Ink::Accent => self.accent,
            Ink::Fixed(rgb) => rgb,
        }
    }
}

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
}

impl Default for CellPaint {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            attrs: AttrPatch::NONE,
            coverage: 1.0,
        }
    }
}

impl CellPaint {
    /// True when this paint would draw the cell exactly as it already is.
    ///
    /// The per-frame diff is built on this: an element every one of whose cells
    /// is settled costs no repaint at all.
    pub(crate) fn is_settled(&self) -> bool {
        self.fg.is_none() && self.bg.is_none() && self.attrs.is_empty() && self.coverage >= 1.0
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
        channel(self.fg)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(17)
            ^ channel(self.bg).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ attrs.rotate_left(41)
            ^ coverage.rotate_left(53)
    }
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
