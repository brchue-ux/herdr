//! The proportional face the card's text is set in, and how it is found.
//!
//! Herdr ships no font. A terminal application's one guaranteed face is the
//! host terminal's own, which is monospaced and which Herdr cannot read the
//! file for — so a proportional face has to come off the machine, and a machine
//! is allowed not to have one.
//!
//! That is the whole design here: look for a sans face in the places every
//! desktop puts one, take the first that parses, and if none does, report it.
//! [`super::available`] is what the sidebar asks, and a `false` there sends the
//! panel down the character path it already had. There is no bundled fallback
//! and no degraded pixel card: a card whose title cannot be set is not a card.
//!
//! `[experimental] sidebar_card_font` overrides the search with an explicit
//! path, which is the escape hatch for a machine whose fonts are somewhere this
//! list does not know about.

use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont};

/// Where a desktop keeps a proportional sans face, most preferred first.
///
/// Ordered by how close each is to the face the card was measured in — the
/// prototypes set the card in Ubuntu Sans — and then by how likely the file is
/// to exist at all. Every entry is a *static* path rather than a fontconfig
/// query because linking fontconfig to pick a font is a dependency this does
/// not need, and because a missing file is a cheaper failure than a missing
/// library.
#[cfg(all(unix, not(target_os = "macos")))]
const CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/ubuntu/UbuntuSans[wdth,wght].ttf",
    "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
];

#[cfg(target_os = "macos")]
const CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
    "/System/Library/Fonts/Geneva.ttf",
];

#[cfg(windows)]
const CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    r"C:\Windows\Fonts\tahoma.ttf",
    r"C:\Windows\Fonts\verdana.ttf",
];

/// The loaded face, plus where it came from so the log can name it.
pub(super) struct CardFont {
    font: FontVec,
    source: String,
}

impl CardFont {
    fn load(path: &str) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let font = FontVec::try_from_vec(bytes).ok()?;
        Some(Self {
            font,
            source: path.to_string(),
        })
    }

    pub(super) fn source(&self) -> &str {
        &self.source
    }

    /// Metrics for this face at `px`, in pixels.
    pub(super) fn metrics(&self, px: f32) -> FontMetrics {
        let scaled = self.font.as_scaled(PxScale::from(px));
        FontMetrics {
            ascent: scaled.ascent(),
            line_height: scaled.height(),
        }
    }

    /// Advance width of `text` at `px`, kerning included, tracking zero.
    ///
    /// Tracking is zero because the reference *measured* zero: "Project Alpha"
    /// is 137 px of ink where the face renders 138 px naturally at the same cap
    /// height. The airiness in the reference is padding and a light weight, and
    /// every earlier attempt that added letterspacing was adding an error.
    pub(super) fn width(&self, text: &str, px: f32) -> f32 {
        let scaled = self.font.as_scaled(PxScale::from(px));
        let mut width = 0.0;
        let mut previous = None;
        for ch in text.chars() {
            let id = self.font.glyph_id(ch);
            if let Some(previous) = previous {
                width += scaled.kern(previous, id);
            }
            width += scaled.h_advance(id);
            previous = Some(id);
        }
        width
    }

    /// Rasterise `text` with its baseline at `(x, y)`, calling `plot` for every
    /// covered pixel with its coverage in 0..=1.
    ///
    /// Coverage rather than a colour, so the caller owns compositing: the title
    /// and the tidbit differ only in the ink they hand to `blend`, and the chip
    /// label prints over a plate rather than over the card.
    pub(super) fn draw(
        &self,
        text: &str,
        px: f32,
        x: f32,
        y: f32,
        mut plot: impl FnMut(i32, i32, f32),
    ) {
        let scaled = self.font.as_scaled(PxScale::from(px));
        let mut pen = x;
        let mut previous = None;
        for ch in text.chars() {
            let id = self.font.glyph_id(ch);
            if let Some(previous) = previous {
                pen += scaled.kern(previous, id);
            }
            let glyph = Glyph {
                id,
                scale: PxScale::from(px),
                position: ab_glyph::point(pen, y),
            };
            if let Some(outlined) = self.font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    plot(
                        bounds.min.x as i32 + gx as i32,
                        bounds.min.y as i32 + gy as i32,
                        coverage,
                    );
                });
            }
            pen += scaled.h_advance(id);
            previous = Some(id);
        }
    }
}

/// Vertical metrics at one size, all in pixels from the baseline.
#[derive(Debug, Clone, Copy)]
pub(super) struct FontMetrics {
    /// Distance from the baseline up to the face's ascender. A line is placed
    /// by its top edge and then set at `top + ascent`, so this is the only
    /// metric the card needs beyond the line height itself.
    pub ascent: f32,
    pub line_height: f32,
}

static CARD_FONT: OnceLock<Option<CardFont>> = OnceLock::new();

/// The card face, loaded once per process.
///
/// `override_path` is `[experimental] sidebar_card_font`. It is read on the
/// first call only — a face is a process-lifetime fact and re-reading it every
/// frame would put a `stat` in the render loop for a value that cannot change
/// without a restart anyway.
pub(super) fn card_font(override_path: Option<&str>) -> Option<&'static CardFont> {
    CARD_FONT
        .get_or_init(|| {
            if let Some(path) = override_path {
                let loaded = CardFont::load(path);
                if loaded.is_none() {
                    tracing::warn!(
                        path,
                        "experimental.sidebar_card_font could not be read as a font; \
                         the sidebar keeps its character cards"
                    );
                }
                return loaded;
            }
            let found = CANDIDATES.iter().find_map(|path| CardFont::load(path));
            match &found {
                Some(font) => tracing::debug!(source = font.source(), "sidebar card font"),
                None => tracing::info!(
                    "no proportional font found for sidebar image cards; \
                     the sidebar keeps its character cards. Set \
                     [experimental] sidebar_card_font to a .ttf to override."
                ),
            }
            found
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The search is a list of *files*, so the only way it can succeed is by a
    /// file existing. This asserts the shape rather than the outcome: a CI box
    /// with no fonts is a legitimate machine and must take the fallback.
    #[test]
    fn every_candidate_is_an_absolute_path_to_a_font_file() {
        for path in CANDIDATES {
            assert!(
                path.starts_with('/') || path.contains(":\\"),
                "{path} is not absolute"
            );
            let lower = path.to_ascii_lowercase();
            assert!(
                lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc"),
                "{path} is not a font file"
            );
        }
    }

    /// Whatever face this machine has, the metrics it reports have to be the
    /// right way up, or every baseline the card computes is wrong.
    #[test]
    fn a_face_that_loads_reports_metrics_the_right_way_up() {
        let Some(font) = card_font(None) else {
            return;
        };
        let metrics = font.metrics(14.0);
        assert!(metrics.ascent > 0.0, "ascent should be above the baseline");
        assert!(
            metrics.line_height > metrics.ascent,
            "a line has to be taller than its ascender, or the descenders collide"
        );
        assert!(font.width("Project Alpha", 14.0) > font.width("Project", 14.0));
        assert_eq!(font.width("", 14.0), 0.0);
    }
}
