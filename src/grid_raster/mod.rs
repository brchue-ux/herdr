//! Re-presenting a region of Herdr's own character grid as pixels.
//!
//! # Why this exists
//!
//! Herdr composes every frame into a [`ratatui::buffer::Buffer`] — one cell per
//! character, each carrying its symbol, its colours and its attributes — and
//! then writes that buffer to the host terminal as text for the terminal to set
//! in its own font. This module is the other half of a second option: take the
//! same buffer and draw the glyphs *into an image*, which the terminal is then
//! handed as a Kitty graphics placement at `z >= 0` and composites over the
//! cells it came from.
//!
//! Two things fall out of that, and they are the reason it is worth the cost.
//!
//! **It is the only band that is reliably drawn.** Herdr's existing whole-screen
//! artwork sits at negative `z`, under the text, which a terminal is free to
//! accept and then ignore — Rio on Vulkan and wgpu draws every glyph *before*
//! any image layer, so an opaque wash at `z = -2` erases the screen
//! (`data/herdr-rio-render-capability-research-20260810/report.md` §2-3,
//! firstmate home). `z >= 0` has no such failure mode here: a surface that
//! already contains the text cannot hide it.
//!
//! **Resolution stops being one cell.** Everything Herdr can currently say
//! about a character it must say in that character's own cell — a foreground,
//! a background, an attribute. Once the grid is pixels, it is pixels.
//!
//! # Rasterising the composed buffer rather than the emulator's grid
//!
//! The input here is the buffer Herdr *just finished composing*, not the
//! terminal emulator's cell grid that fed it. That is deliberate and it is
//! where most of this module's leverage comes from: by the time a pane region
//! reaches the buffer it has already had the selection highlight, the copy-mode
//! search highlights, the inactive-pane dimming and the cursor applied to it by
//! [`crate::ui::panes`]. Rasterising the buffer inherits every one of those for
//! free and — more importantly — inherits any future one automatically, where
//! reading the emulator's grid would silently drop them and keep dropping them.
//!
//! It also means this module is a pure function of a `Buffer`: no PTY, no lock,
//! no async, testable against a hand-built buffer.
//!
//! # What the text underneath is still for
//!
//! Herdr does **not** blank the cells it draws over. The image is opaque and
//! covers them exactly, so nothing shows through — but the characters are still
//! there, which means a terminal that drops the placement degrades to a working
//! text pane instead of an empty rectangle, and anything reading the screen
//! rather than looking at it still finds real text.
//!
//! # Known gaps, deliberately left
//!
//! - **Bold is synthesised, not a second face.** One face is loaded; bold is
//!   drawn by emboldening its coverage rather than by opening the family's bold
//!   file. Italic likewise is not slanted.
//! - **Fallback chains, ligatures, emoji and CJK shaping are absent.** Each
//!   glyph is looked up in the one loaded face and drawn on its own; a
//!   character the face lacks renders as nothing.
//! - **Family-name resolution is missing**, so fidelity to a *configured* font
//!   needs the file named explicitly — see [`font`].
//!
//! All three are named in the plan this slice belongs to rather than discovered
//! later; none of them is load-bearing for proving the compositing path.

pub(crate) mod font;

use ab_glyph::{Font, Glyph, PxScale, ScaleFont};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::terminal_theme::TerminalTheme;
use crate::ui::color::Rgb;

/// An RGBA surface, sized in whole cells, ready to be published as a graphics
/// layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridRaster {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Straight (non-premultiplied) RGBA8, row-major, fully opaque.
    pub(crate) rgba: Vec<u8>,
}

/// What a cell resolves to once `Color::Reset`, the host palette, `REVERSED`
/// and `DIM` have all been applied. Split out so the resolution can be tested
/// without rendering a glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellColors {
    fg: Rgb,
    bg: Rgb,
}

/// Herdr's own last-resort colours, for a host terminal that never reported
/// its own. Deliberately the same neutral pair the rest of Herdr assumes rather
/// than pure black and white, which would make an unreported theme render
/// noticeably harsher than the terminal it is imitating.
const FALLBACK_FG: Rgb = (0xd0, 0xd0, 0xd0);
const FALLBACK_BG: Rgb = (0x10, 0x10, 0x10);

fn resolve_cell_colors(cell: &ratatui::buffer::Cell, theme: &TerminalTheme) -> CellColors {
    let default_fg = theme
        .foreground
        .map(crate::ui::color::terminal_theme_to_rgb)
        .unwrap_or(FALLBACK_FG);
    let default_bg = theme
        .background
        .map(crate::ui::color::terminal_theme_to_rgb)
        .unwrap_or(FALLBACK_BG);

    let style = cell.style();
    // `Color::Reset` means "whatever the host is using", which is exactly the
    // default the theme reports — so it resolves here rather than being left as
    // a hole for the rasteriser to guess at.
    let mut fg = crate::ui::color::resolve_color_rgb(style.fg.unwrap_or(Color::Reset), theme)
        .unwrap_or(default_fg);
    let mut bg = crate::ui::color::resolve_color_rgb(style.bg.unwrap_or(Color::Reset), theme)
        .unwrap_or(default_bg);

    if style.add_modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if style.add_modifier.contains(Modifier::DIM) {
        fg = crate::ui::color::mix_rgb(fg, bg, 0.5);
    }
    // `HIDDEN` keeps the cell's background and drops its ink, which is what the
    // attribute means; the glyph is skipped separately.
    CellColors { fg, bg }
}

/// Whether this cell's symbol should have a glyph drawn for it at all.
///
/// Ratatui stores a double-width character in one cell and leaves the cell to
/// its right holding an empty symbol; drawing "nothing" for that one is correct
/// and also what keeps the wide glyph from being cut in half, since glyphs are
/// clipped to the surface rather than to their own cell.
fn glyph_is_worth_drawing(symbol: &str, hidden: bool) -> bool {
    !hidden && !symbol.is_empty() && symbol.chars().any(|ch| !ch.is_whitespace())
}

/// Draws `area` of `buffer` as an image at `cell_width x cell_height` pixels
/// per cell.
///
/// Returns `None` when there is no monospaced face to set the text in, when the
/// cell size is degenerate, or when `area` is empty — every one of which is a
/// reason for the caller to leave the pane as text rather than publish a
/// broken surface.
pub(crate) fn rasterise_region(
    buffer: &Buffer,
    area: Rect,
    cell_width: u32,
    cell_height: u32,
    theme: &TerminalTheme,
    font_override: Option<&str>,
) -> Option<GridRaster> {
    let face = font::grid_font(font_override)?;
    rasterise_region_with_font(buffer, area, cell_width, cell_height, theme, face)
}

/// The body of [`rasterise_region`] with the face passed in, so tests can pin a
/// specific file instead of racing the process-wide `OnceLock` the config path
/// populates.
pub(crate) fn rasterise_region_with_font(
    buffer: &Buffer,
    area: Rect,
    cell_width: u32,
    cell_height: u32,
    theme: &TerminalTheme,
    face: &font::GridFont,
) -> Option<GridRaster> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let fit = face.fit_to_cell(cell_width, cell_height)?;

    let width = u32::from(area.width) * cell_width;
    let height = u32::from(area.height) * cell_height;
    // Guards a geometry that would allocate absurdly before it allocates. The
    // ceiling is far above any real terminal and exists so a corrupt cell size
    // cannot turn into a multi-gigabyte request.
    if width == 0 || height == 0 || width > 32_768 || height > 32_768 {
        return None;
    }

    let mut surface = vec![0u8; (width as usize) * (height as usize) * 4];
    let scaled = face.face().as_scaled(PxScale::from(fit.px));

    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = buffer.cell((area.x + col, area.y + row)) else {
                continue;
            };
            let colors = resolve_cell_colors(cell, theme);
            let origin_x = u32::from(col) * cell_width;
            let origin_y = u32::from(row) * cell_height;

            fill_cell(
                &mut surface,
                width,
                origin_x,
                origin_y,
                cell_width,
                cell_height,
                colors.bg,
            );

            let style = cell.style();
            let hidden = style.add_modifier.contains(Modifier::HIDDEN);
            if glyph_is_worth_drawing(cell.symbol(), hidden) {
                let bold = style.add_modifier.contains(Modifier::BOLD);
                for ch in cell.symbol().chars() {
                    draw_glyph(
                        &mut surface,
                        width,
                        height,
                        &scaled,
                        ch,
                        origin_x as f32 + fit.x_offset,
                        origin_y as f32 + fit.baseline,
                        colors.fg,
                        bold,
                    );
                }
            }

            if !hidden && style.add_modifier.contains(Modifier::UNDERLINED) {
                let underline = crate::ui::color::resolve_color_rgb(
                    style.underline_color.unwrap_or(Color::Reset),
                    theme,
                )
                .unwrap_or(colors.fg);
                // Just below the baseline, and never in the last pixel row,
                // where two stacked rows' underlines would touch.
                let y = origin_y + (fit.baseline as u32 + 1).min(cell_height.saturating_sub(1));
                draw_rule(&mut surface, width, origin_x, y, cell_width, underline);
            }
            if !hidden && style.add_modifier.contains(Modifier::CROSSED_OUT) {
                let y = origin_y + cell_height / 2;
                draw_rule(&mut surface, width, origin_x, y, cell_width, colors.fg);
            }
        }
    }

    Some(GridRaster {
        width,
        height,
        rgba: surface,
    })
}

fn fill_cell(
    surface: &mut [u8],
    surface_width: u32,
    origin_x: u32,
    origin_y: u32,
    cell_width: u32,
    cell_height: u32,
    color: Rgb,
) {
    for y in origin_y..origin_y + cell_height {
        let row_start = ((y as usize) * (surface_width as usize) + origin_x as usize) * 4;
        for x in 0..cell_width as usize {
            let index = row_start + x * 4;
            surface[index] = color.0;
            surface[index + 1] = color.1;
            surface[index + 2] = color.2;
            surface[index + 3] = 0xff;
        }
    }
}

fn draw_rule(
    surface: &mut [u8],
    surface_width: u32,
    origin_x: u32,
    y: u32,
    cell_width: u32,
    color: Rgb,
) {
    let row_start = ((y as usize) * (surface_width as usize) + origin_x as usize) * 4;
    for x in 0..cell_width as usize {
        let index = row_start + x * 4;
        if index + 3 >= surface.len() {
            return;
        }
        surface[index] = color.0;
        surface[index + 1] = color.1;
        surface[index + 2] = color.2;
        surface[index + 3] = 0xff;
    }
}

/// Draws one character with its baseline at `(pen_x, baseline_y)`, blending its
/// coverage over whatever the cell fill already put there.
///
/// Clipped to the whole surface rather than to the originating cell: a glyph
/// that overhangs — a descender, an italic, the right half of a double-width
/// character — belongs in the neighbouring pixels, and clipping it per cell is
/// what would make it look wrong.
#[allow(clippy::too_many_arguments)]
fn draw_glyph<F, SF>(
    surface: &mut [u8],
    surface_width: u32,
    surface_height: u32,
    scaled: &SF,
    ch: char,
    pen_x: f32,
    baseline_y: f32,
    color: Rgb,
    bold: bool,
) where
    F: Font,
    SF: ScaleFont<F>,
{
    let glyph_id = scaled.glyph_id(ch);
    let glyph = Glyph {
        id: glyph_id,
        scale: PxScale::from(scaled.scale().y),
        position: ab_glyph::point(pen_x, baseline_y),
    };
    let Some(outlined) = scaled.font().outline_glyph(glyph) else {
        return;
    };
    let bounds = outlined.px_bounds();
    outlined.draw(|x, y, coverage| {
        if coverage <= 0.0 {
            return;
        }
        let px = bounds.min.x as i32 + x as i32;
        let py = bounds.min.y as i32 + y as i32;
        if px < 0 || py < 0 || px >= surface_width as i32 || py >= surface_height as i32 {
            return;
        }
        // Synthetic bold: push coverage up so the same outline lays down more
        // ink. Not a substitute for the family's bold face — it thickens the
        // edges rather than redrawing the stems — but it keeps bold visibly
        // bolder without a second font file, which is the honest trade for a
        // module that ships no fonts.
        let coverage = if bold {
            (coverage * 1.45).min(1.0)
        } else {
            coverage
        };
        let index = ((py as usize) * (surface_width as usize) + px as usize) * 4;
        blend_pixel(&mut surface[index..index + 4], color, coverage);
    });
}

fn blend_pixel(pixel: &mut [u8], color: Rgb, coverage: f32) {
    let blend = |dst: u8, src: u8| -> u8 {
        (f32::from(dst) * (1.0 - coverage) + f32::from(src) * coverage).round() as u8
    };
    pixel[0] = blend(pixel[0], color.0);
    pixel[1] = blend(pixel[1], color.1);
    pixel[2] = blend(pixel[2], color.2);
    pixel[3] = 0xff;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn theme() -> TerminalTheme {
        TerminalTheme::default()
    }

    /// A buffer with `text` on its first row, in `style`.
    fn buffer_with(width: u16, height: u16, text: &str, style: Style) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        for (index, ch) in text.chars().enumerate() {
            let Ok(x) = u16::try_from(index) else { break };
            if x >= width {
                break;
            }
            if let Some(cell) = buffer.cell_mut((x, 0)) {
                cell.set_symbol(&ch.to_string());
                cell.set_style(style);
            }
        }
        buffer
    }

    fn any_face() -> Option<font::GridFont> {
        font::all_available_faces()
            .into_iter()
            .next()
            .map(|(_, face)| face)
    }

    fn pixel(raster: &GridRaster, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let index = ((y as usize) * (raster.width as usize) + x as usize) * 4;
        (
            raster.rgba[index],
            raster.rgba[index + 1],
            raster.rgba[index + 2],
            raster.rgba[index + 3],
        )
    }

    /// Counts pixels that are not the cell background, i.e. glyph ink.
    fn ink(raster: &GridRaster, bg: Rgb) -> usize {
        raster
            .rgba
            .chunks_exact(4)
            .filter(|px| (px[0], px[1], px[2]) != bg)
            .count()
    }

    #[test]
    fn the_surface_is_exactly_the_region_in_whole_cells() {
        let Some(face) = any_face() else { return };
        let buffer = buffer_with(20, 5, "hello", Style::default());
        let raster =
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 20, 5), 9, 19, &theme(), &face)
                .expect("a face is loaded and the cell is sane");
        assert_eq!(raster.width, 20 * 9);
        assert_eq!(raster.height, 5 * 19);
        assert_eq!(
            raster.rgba.len(),
            (20 * 9 * 5 * 19 * 4) as usize,
            "RGBA8 is four bytes per pixel with no padding"
        );
    }

    #[test]
    fn every_pixel_is_opaque() {
        let Some(face) = any_face() else { return };
        let buffer = buffer_with(8, 2, "hi", Style::default());
        let raster =
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 8, 2), 9, 19, &theme(), &face)
                .expect("rasterised");
        assert!(
            raster.rgba.chunks_exact(4).all(|px| px[3] == 0xff),
            "a transparent pixel would let the covered text show through and double-draw"
        );
    }

    /// The point of the whole module: characters become ink.
    #[test]
    fn text_puts_ink_on_the_surface_and_blank_cells_do_not() {
        let Some(face) = any_face() else { return };
        let blank = Buffer::empty(Rect::new(0, 0, 8, 1));
        let blank_raster =
            rasterise_region_with_font(&blank, Rect::new(0, 0, 8, 1), 9, 19, &theme(), &face)
                .expect("rasterised");
        assert_eq!(
            ink(&blank_raster, FALLBACK_BG),
            0,
            "an empty grid should be flat background"
        );

        let written = buffer_with(8, 1, "MMMMMMMM", Style::default());
        let written_raster =
            rasterise_region_with_font(&written, Rect::new(0, 0, 8, 1), 9, 19, &theme(), &face)
                .expect("rasterised");
        assert!(
            ink(&written_raster, FALLBACK_BG) > 100,
            "eight Ms should lay down substantial ink"
        );
    }

    #[test]
    fn a_cell_background_fills_its_whole_cell() {
        let Some(face) = any_face() else { return };
        let buffer = buffer_with(
            2,
            1,
            "  ",
            Style::default().bg(Color::Rgb(0x20, 0x80, 0xc0)),
        );
        let raster =
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 2, 1), 9, 19, &theme(), &face)
                .expect("rasterised");
        for y in 0..19 {
            for x in 0..18 {
                assert_eq!(
                    pixel(&raster, x, y),
                    (0x20, 0x80, 0xc0, 0xff),
                    "cell background should cover ({x},{y}) with no gaps"
                );
            }
        }
    }

    /// `REVERSED` is how Herdr's own selection highlight reaches the buffer in
    /// some themes, so getting it wrong would make selected text invisible in a
    /// re-presented pane.
    #[test]
    fn reversed_swaps_foreground_and_background() {
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_style(
            Style::default()
                .fg(Color::Rgb(0xff, 0x00, 0x00))
                .bg(Color::Rgb(0x00, 0x00, 0xff))
                .add_modifier(Modifier::REVERSED),
        );
        let colors = resolve_cell_colors(&cell, &theme());
        assert_eq!(colors.fg, (0x00, 0x00, 0xff));
        assert_eq!(colors.bg, (0xff, 0x00, 0x00));
    }

    #[test]
    fn reset_resolves_to_the_host_theme_rather_than_to_a_hole() {
        let host = TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0xab,
                g: 0xcd,
                b: 0xef,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x01,
                g: 0x02,
                b: 0x03,
            }),
            ..TerminalTheme::default()
        };
        let cell = ratatui::buffer::Cell::default();
        let colors = resolve_cell_colors(&cell, &host);
        assert_eq!(colors.fg, (0xab, 0xcd, 0xef));
        assert_eq!(colors.bg, (0x01, 0x02, 0x03));
    }

    #[test]
    fn dim_moves_the_foreground_toward_the_background() {
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_style(
            Style::default()
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .bg(Color::Rgb(0x00, 0x00, 0x00))
                .add_modifier(Modifier::DIM),
        );
        let colors = resolve_cell_colors(&cell, &theme());
        assert!(
            colors.fg.0 < 0xff && colors.fg.0 > 0x00,
            "dim should land between the two, not at either end: {:?}",
            colors.fg
        );
    }

    #[test]
    fn hidden_text_draws_no_ink() {
        let Some(face) = any_face() else { return };
        let style = Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .bg(Color::Rgb(0x00, 0x00, 0x00));
        let visible = buffer_with(8, 1, "password", style);
        let hidden = buffer_with(8, 1, "password", style.add_modifier(Modifier::HIDDEN));
        let visible_raster =
            rasterise_region_with_font(&visible, Rect::new(0, 0, 8, 1), 9, 19, &theme(), &face)
                .expect("rasterised");
        let hidden_raster =
            rasterise_region_with_font(&hidden, Rect::new(0, 0, 8, 1), 9, 19, &theme(), &face)
                .expect("rasterised");
        assert!(ink(&visible_raster, (0, 0, 0)) > 0);
        assert_eq!(
            ink(&hidden_raster, (0, 0, 0)),
            0,
            "HIDDEN must not leak the characters it is hiding into the pixels"
        );
    }

    #[test]
    fn bold_lays_down_more_ink_than_regular() {
        let Some(face) = any_face() else { return };
        let style = Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .bg(Color::Rgb(0x00, 0x00, 0x00));
        let regular = buffer_with(8, 1, "abcdefgh", style);
        let bold = buffer_with(8, 1, "abcdefgh", style.add_modifier(Modifier::BOLD));
        let regular_ink = ink(
            &rasterise_region_with_font(&regular, Rect::new(0, 0, 8, 1), 9, 19, &theme(), &face)
                .expect("rasterised"),
            (0, 0, 0),
        );
        let bold_ink = ink(
            &rasterise_region_with_font(&bold, Rect::new(0, 0, 8, 1), 9, 19, &theme(), &face)
                .expect("rasterised"),
            (0, 0, 0),
        );
        assert!(
            bold_ink > regular_ink,
            "synthetic bold should be visibly heavier: {bold_ink} vs {regular_ink}"
        );
    }

    #[test]
    fn underline_draws_a_rule_across_the_cell() {
        let Some(face) = any_face() else { return };
        let style = Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .bg(Color::Rgb(0x00, 0x00, 0x00));
        let plain = buffer_with(4, 1, "    ", style);
        let underlined = buffer_with(4, 1, "    ", style.add_modifier(Modifier::UNDERLINED));
        let plain_ink = ink(
            &rasterise_region_with_font(&plain, Rect::new(0, 0, 4, 1), 9, 19, &theme(), &face)
                .expect("rasterised"),
            (0, 0, 0),
        );
        let underlined_ink = ink(
            &rasterise_region_with_font(&underlined, Rect::new(0, 0, 4, 1), 9, 19, &theme(), &face)
                .expect("rasterised"),
            (0, 0, 0),
        );
        assert_eq!(plain_ink, 0, "blank cells have no ink to begin with");
        assert_eq!(
            underlined_ink,
            4 * 9,
            "the rule should span every column of all four cells"
        );
    }

    /// A region offset inside a larger buffer is what a real pane always is —
    /// panes are never at the origin once there is a sidebar or a border.
    #[test]
    fn only_the_named_region_is_rasterised() {
        let Some(face) = any_face() else { return };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 6));
        // Paint a marker outside the region under test.
        if let Some(cell) = buffer.cell_mut((0u16, 0u16)) {
            cell.set_style(Style::default().bg(Color::Rgb(0xff, 0x00, 0x00)));
        }
        if let Some(cell) = buffer.cell_mut((5u16, 2u16)) {
            cell.set_style(Style::default().bg(Color::Rgb(0x00, 0xff, 0x00)));
        }
        let raster =
            rasterise_region_with_font(&buffer, Rect::new(5, 2, 4, 2), 9, 19, &theme(), &face)
                .expect("rasterised");
        assert_eq!(raster.width, 36);
        assert_eq!(raster.height, 38);
        assert_eq!(
            pixel(&raster, 0, 0),
            (0x00, 0xff, 0x00, 0xff),
            "the region's own top-left cell should be at the surface origin"
        );
        assert!(
            !raster
                .rgba
                .chunks_exact(4)
                .any(|px| (px[0], px[1], px[2]) == (0xff, 0x00, 0x00)),
            "a cell outside the region must not appear in the surface"
        );
    }

    #[test]
    fn an_empty_region_is_refused() {
        let Some(face) = any_face() else { return };
        let buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        assert!(
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 0, 4), 9, 19, &theme(), &face)
                .is_none()
        );
        assert!(
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 8, 0), 9, 19, &theme(), &face)
                .is_none()
        );
    }

    #[test]
    fn a_degenerate_cell_size_is_refused_rather_than_allocating() {
        let Some(face) = any_face() else { return };
        let buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        assert!(
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 8, 4), 0, 19, &theme(), &face)
                .is_none()
        );
        assert!(
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 8, 4), 9, 0, &theme(), &face)
                .is_none()
        );
    }

    /// The same buffer must produce the same bytes, or the upload cache in
    /// `kitty_graphics` can never suppress an unchanged frame and every frame
    /// crosses the wire forever.
    #[test]
    fn rasterising_is_deterministic() {
        let Some(face) = any_face() else { return };
        let buffer = buffer_with(12, 3, "deterministic", Style::default());
        let first =
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 12, 3), 9, 19, &theme(), &face)
                .expect("rasterised");
        let second =
            rasterise_region_with_font(&buffer, Rect::new(0, 0, 12, 3), 9, 19, &theme(), &face)
                .expect("rasterised");
        assert_eq!(first, second);
    }

    /// Every face on the machine has to survive the same cell sizes, because
    /// which one is picked is a property of the box, not of the code. The
    /// 4px-tall case is the worst real one measured on the captain's fleet.
    #[test]
    fn every_available_face_rasterises_at_every_measured_cell_size() {
        for (path, face) in font::all_available_faces() {
            for (width, height) in [(9u32, 19u32), (7, 14), (10, 21), (14, 18), (6, 4)] {
                let buffer = buffer_with(8, 2, "Ag|_", Style::default());
                let raster = rasterise_region_with_font(
                    &buffer,
                    Rect::new(0, 0, 8, 2),
                    width,
                    height,
                    &theme(),
                    &face,
                );
                assert!(
                    raster.is_some(),
                    "{path} failed to rasterise at {width}x{height}"
                );
            }
        }
    }
}
