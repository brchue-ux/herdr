//! The monospaced face the re-presented grid is set in, and how it is found.
//!
//! This is a sibling of [`crate::ui::sidebar::image_card`]'s font search and
//! deliberately not a share of it. That one wants a *proportional* face for
//! card titles; this one wants the *monospaced* face the host terminal is
//! already drawing the user's text in, because the whole point of re-presenting
//! a pane as pixels is that the result still looks like that pane.
//!
//! Herdr ships no font, here as there. A face has to come off the machine.
//!
//! # Fidelity, and what this module cannot do on its own
//!
//! Herdr cannot ask the host terminal which font file it opened — no terminal
//! protocol carries that, and over SSH the terminal is not even on this
//! machine's filesystem. So an exactly-faithful re-presentation needs the face
//! named to Herdr rather than discovered by it, which is what
//! `[experimental] pixel_text_font` is for: point it at the same file the
//! terminal was configured with and the pixels are set in the real thing.
//!
//! Absent that, [`CANDIDATES`] is a fallback, not a claim of fidelity. It picks
//! *a* monospaced face so the path works out of the box; it does not pretend to
//! be the user's. [`GridFont::source`] reports which file was actually opened
//! precisely so the difference is visible rather than silent.
//!
//! Resolving a *family name* — `"Fira Code"` — to a file is the missing third
//! option, and it is what would let Herdr read the host terminal's own config
//! and match it. It needs a name-table reader `ab_glyph` does not expose; see
//! the module doc on [`super`] for where that sits in the plan.

use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};

/// Where a desktop keeps a monospaced face, most preferred first.
///
/// Ordered by how widely deployed each is rather than by taste: this list only
/// runs when nobody named a face, so the goal is to find *something* metrically
/// sane, not to guess well. Static paths rather than a fontconfig query for the
/// same reason the card's search uses them — a missing file is a cheaper
/// failure than a missing library, and linking fontconfig to pick a fallback is
/// a dependency this does not need.
#[cfg(all(unix, not(target_os = "macos")))]
const CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/liberation-mono/LiberationMono-Regular.ttf",
    "/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeMono.ttf",
];

#[cfg(target_os = "macos")]
const CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
    "/Library/Fonts/Courier New.ttf",
];

#[cfg(windows)]
const CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\CascadiaMono.ttf",
    r"C:\Windows\Fonts\consola.ttf",
    r"C:\Windows\Fonts\lucon.ttf",
    r"C:\Windows\Fonts\cour.ttf",
];

/// A loaded monospaced face, plus the file it came from.
pub(crate) struct GridFont {
    font: FontVec,
    source: String,
}

/// Everything needed to place one glyph inside one cell, at one cell size.
///
/// Derived from the *cell*, not from a font size: Herdr is handed the cell the
/// host terminal already chose and has to fill exactly that box, which is the
/// inverse of how a terminal normally works. See [`GridFont::fit_to_cell`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellFit {
    /// Font size in pixels: the largest that fits the cell in both axes.
    pub(crate) px: f32,
    /// Baseline offset from the top of the cell, in pixels.
    pub(crate) baseline: f32,
    /// Left offset from the cell's own edge, in pixels, centring a glyph whose
    /// advance is narrower than the cell.
    pub(crate) x_offset: f32,
}

impl GridFont {
    fn load(path: &str) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let font = FontVec::try_from_vec(bytes).ok()?;
        Some(Self {
            font,
            source: path.to_string(),
        })
    }

    /// The file this face was read from, so a log or a report can name it
    /// rather than leaving the user to guess whether they got their own font.
    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn face(&self) -> &FontVec {
        &self.font
    }

    /// Advance width of `M` at one pixel — the face's own idea of how wide a
    /// cell is, per unit of size.
    ///
    /// `M` rather than an average because this face is monospaced, so every
    /// advance is the same one and `M` is the conventional probe. A face that
    /// turns out not to be monospaced still produces a usable number here; it
    /// just renders with glyphs that do not fill their cells evenly, which is a
    /// visible-but-working failure rather than a panic.
    fn advance_per_px(&self) -> f32 {
        let scaled = self.font.as_scaled(PxScale::from(1.0));
        scaled.h_advance(self.font.glyph_id('M'))
    }

    /// The font size, baseline and left offset that fit a
    /// `cell_width x cell_height` box.
    ///
    /// Sizing is driven by **width first**. A terminal picks a font size and
    /// derives its cell from the result; Herdr sees only the derived cell and
    /// has to invert that, and the advance is the half of the cell the face
    /// controls exactly — so matching the advance to the cell width reproduces
    /// the size the terminal was configured with, where matching height would
    /// not (height is the terminal's line spacing, not a property of the face).
    ///
    /// **But width alone is not safe.** Cell aspect ratios in the wild run from
    /// roughly 1:2 down to well past 1:1, and faces differ in how tall their
    /// ascent/descent band is per unit of advance. A face narrower than the
    /// cell's aspect, sized to fill the width, ends up with an ink band taller
    /// than the cell — measured here at 18.3 px in an 18 px cell for Ubuntu
    /// Mono — and every row then renders through the top and bottom of its own
    /// cell into its neighbours. Vertical bleed corrupts adjacent rows of text;
    /// horizontal underfill just leaves a little air. So the size is the
    /// smaller of the two fits, and a glyph narrower than its cell is centred
    /// in it rather than left-aligned.
    ///
    /// The baseline centres the face's own ascent/descent band in the cell,
    /// rather than sitting at the face's natural line height, so a face with
    /// generous default leading does not sink through the cell floor.
    ///
    /// Returns `None` for a degenerate cell or a face with no usable metrics,
    /// both of which would otherwise divide by zero.
    pub(crate) fn fit_to_cell(&self, cell_width: u32, cell_height: u32) -> Option<CellFit> {
        if cell_width == 0 || cell_height == 0 {
            return None;
        }
        // Both metrics come out of a parsed font file, so both are checked for
        // finiteness as well as sign: a face with a broken `head` table can
        // yield NaN here, and NaN would otherwise sail through a plain `<= 0.0`
        // and divide into an unusable scale.
        let advance = self.advance_per_px();
        if !advance.is_finite() || advance <= 0.0 {
            return None;
        }
        // `descent` is negative in ab_glyph's convention, so the ink band is
        // `ascent - descent` tall.
        let unit = self.font.as_scaled(PxScale::from(1.0));
        let ink_per_px = unit.ascent() - unit.descent();
        if !ink_per_px.is_finite() || ink_per_px <= 0.0 {
            return None;
        }

        let by_width = cell_width as f32 / advance;
        let by_height = cell_height as f32 / ink_per_px;
        let px = by_width.min(by_height);

        let scaled = self.font.as_scaled(PxScale::from(px));
        let ascent = scaled.ascent();
        let ink = ascent - scaled.descent();
        let leading = (cell_height as f32 - ink) / 2.0;
        let glyph_advance = scaled.h_advance(self.font.glyph_id('M'));
        Some(CellFit {
            px,
            baseline: leading + ascent,
            x_offset: ((cell_width as f32 - glyph_advance) / 2.0).max(0.0),
        })
    }
}

/// Every candidate face that exists on this machine.
///
/// Tests measure against all of them rather than against whichever one this box
/// happens to rank first. Monospaced faces differ in advance ratio by well over
/// 10%, so a test that asserted a pixel position against the developer box's
/// DejaVu would be asserting DejaVu, not the code.
#[cfg(test)]
pub(crate) fn all_available_faces() -> Vec<(&'static str, GridFont)> {
    CANDIDATES
        .iter()
        .filter_map(|path| GridFont::load(path).map(|font| (*path, font)))
        .collect()
}

/// The grid face, loaded once per process.
///
/// `override_path` is `[experimental] pixel_text_font`, read on the first call
/// only. A face is a process-lifetime fact, and re-reading it per frame would
/// put a `read(2)` of a multi-megabyte file inside the render loop for a value
/// that cannot change without a restart.
///
/// An override that does not load falls through to the search rather than
/// failing the pane: a typo in a config path should cost fidelity, not the
/// pane's contents.
pub(crate) fn grid_font(override_path: Option<&str>) -> Option<&'static GridFont> {
    static FONT: OnceLock<Option<GridFont>> = OnceLock::new();
    FONT.get_or_init(|| {
        if let Some(path) = override_path.filter(|path| !path.is_empty()) {
            match GridFont::load(path) {
                Some(font) => {
                    tracing::info!(source = font.source(), "pixel text: using configured face");
                    return Some(font);
                }
                None => tracing::warn!(
                    path,
                    "pixel text: configured face could not be read; searching instead"
                ),
            }
        }
        let found = CANDIDATES.iter().find_map(|path| GridFont::load(path));
        match &found {
            Some(font) => tracing::info!(
                source = font.source(),
                "pixel text: no face configured; using a discovered monospaced fallback"
            ),
            None => tracing::warn!("pixel text: no monospaced face found; panes stay as text"),
        }
        found
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole module is a fallback when no face is installed, so every test
    /// here has to tolerate a bare machine rather than assert one is populated.
    fn faces() -> Vec<(&'static str, GridFont)> {
        all_available_faces()
    }

    #[test]
    fn every_candidate_that_exists_parses() {
        for (path, font) in faces() {
            assert_eq!(font.source(), path);
            assert!(
                font.advance_per_px() > 0.0,
                "{path} parsed but has no advance"
            );
        }
    }

    /// Every cell size below is one measured on a real host terminal, plus the
    /// 14x18 case that first exposed the height overflow and the 4px-tall worst
    /// case from the captain's own fleet.
    const CELLS: &[(u32, u32)] = &[(9, 19), (7, 14), (10, 21), (14, 18), (4, 8), (6, 4)];

    /// A glyph must never be wider than the cell it is drawn in, or every row
    /// smears into its neighbour horizontally.
    #[test]
    fn a_fitted_glyph_never_overflows_the_cell_width() {
        for (path, font) in faces() {
            for &(width, height) in CELLS {
                let Some(fit) = font.fit_to_cell(width, height) else {
                    panic!("{path} refused a {width}x{height} cell");
                };
                let scaled = font.face().as_scaled(PxScale::from(fit.px));
                let advance = scaled.h_advance(font.face().glyph_id('M'));
                assert!(
                    advance <= width as f32 + 0.01,
                    "{path} at {width}x{height}: advance {advance} exceeds the cell width {width}"
                );
                assert!(
                    fit.x_offset >= 0.0 && fit.x_offset * 2.0 <= width as f32 + 0.01,
                    "{path} at {width}x{height}: x_offset {} is not a centring offset",
                    fit.x_offset
                );
            }
        }
    }

    /// Width is what reproduces the terminal's configured size, so it should be
    /// matched exactly whenever the cell is tall enough to allow it. This is
    /// the other half of the clamp: it must not fire when it is not needed.
    #[test]
    fn a_cell_with_room_to_spare_is_filled_edge_to_edge() {
        for (path, font) in faces() {
            // A generously tall cell: no real face needs clamping here.
            let (width, height) = (9u32, 40u32);
            let Some(fit) = font.fit_to_cell(width, height) else {
                panic!("{path} refused a {width}x{height} cell");
            };
            let scaled = font.face().as_scaled(PxScale::from(fit.px));
            let advance = scaled.h_advance(font.face().glyph_id('M'));
            assert!(
                (advance - width as f32).abs() < 0.01,
                "{path}: advance {advance} should match the cell width {width} when height allows"
            );
            assert!(
                fit.x_offset < 0.01,
                "{path}: a filled cell needs no centring"
            );
        }
    }

    /// The failure this guards is an ink band taller than the cell, which puts
    /// the baseline past the cell floor and renders every row's glyphs into the
    /// row beneath it.
    #[test]
    fn the_ink_band_stays_inside_the_cell() {
        for (path, font) in faces() {
            for &(width, height) in CELLS {
                let Some(fit) = font.fit_to_cell(width, height) else {
                    panic!("{path} refused a {width}x{height} cell");
                };
                assert!(
                    fit.baseline > 0.0 && fit.baseline <= height as f32,
                    "{path} at {width}x{height}: baseline {} escapes the cell",
                    fit.baseline
                );
                let scaled = font.face().as_scaled(PxScale::from(fit.px));
                let ink = scaled.ascent() - scaled.descent();
                assert!(
                    ink <= height as f32 + 0.01,
                    "{path} at {width}x{height}: ink band {ink} is taller than the cell"
                );
            }
        }
    }

    #[test]
    fn a_degenerate_cell_is_refused_rather_than_dividing_by_zero() {
        for (path, font) in faces() {
            assert!(
                font.fit_to_cell(0, 19).is_none(),
                "{path} accepted zero width"
            );
            assert!(
                font.fit_to_cell(9, 0).is_none(),
                "{path} accepted zero height"
            );
        }
    }
}
