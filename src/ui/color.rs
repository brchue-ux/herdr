//! Shared colour maths.
//!
//! These helpers used to be private to `src/ui/panes.rs`, where they were added
//! to derive the pane selection highlight from the host terminal background.
//! They are shared now because palette derivation needs the same maths.
//!
//! Resolution prefers the *measured* host terminal palette — the OSC 4 replies
//! Herdr already collects into [`TerminalTheme::palette`] — and only falls back
//! to a static table when the host did not answer.

use ratatui::style::Color;

use crate::terminal_theme::{RgbColor, TerminalTheme};

pub(crate) type Rgb = (u8, u8, u8);

pub(crate) const WHITE: Rgb = (255, 255, 255);
pub(crate) const BLACK: Rgb = (0, 0, 0);

pub(crate) fn terminal_theme_to_rgb(color: RgbColor) -> Rgb {
    (color.r, color.g, color.b)
}

pub(crate) fn mix_rgb(base: Rgb, target: Rgb, amount: f32) -> Rgb {
    fn channel(base: u8, target: u8, amount: f32) -> u8 {
        (f32::from(base) + (f32::from(target) - f32::from(base)) * amount).round() as u8
    }
    (
        channel(base.0, target.0, amount),
        channel(base.1, target.1, amount),
        channel(base.2, target.2, amount),
    )
}

/// WCAG 2.x relative luminance over linearised sRGB.
pub(crate) fn relative_luminance(color: Rgb) -> f32 {
    fn channel(value: u8) -> f32 {
        let value = f32::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.0) + 0.7152 * channel(color.1) + 0.0722 * channel(color.2)
}

/// WCAG 2.x contrast ratio, in `1.0..=21.0`.
pub(crate) fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Static fallback for named colours when the host terminal did not report a
/// palette. `Color::Reset` and `Color::Indexed` have no meaning here.
pub(crate) fn color_to_rgb(color: Color) -> Option<Rgb> {
    match color {
        Color::Reset => None,
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((128, 0, 0)),
        Color::Green => Some((0, 128, 0)),
        Color::Yellow => Some((128, 128, 0)),
        Color::Blue => Some((0, 0, 128)),
        Color::Magenta => Some((128, 0, 128)),
        Color::Cyan => Some((0, 128, 128)),
        Color::Gray => Some((192, 192, 192)),
        Color::DarkGray => Some((128, 128, 128)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((0, 0, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(_) => None,
    }
}

/// The 0..16 ANSI slot a named colour occupies, so a measured host palette
/// entry can be preferred over the static table above.
fn ansi_index(color: Color) -> Option<u8> {
    Some(match color {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        _ => return None,
    })
}

/// The standard xterm-256 cube and greyscale ramp, used only when the host did
/// not report the index.
fn xterm_256_rgb(index: u8) -> Option<Rgb> {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => None,
        16..=231 => {
            let offset = u16::from(index) - 16;
            Some((
                LEVELS[usize::from(offset / 36)],
                LEVELS[usize::from((offset / 6) % 6)],
                LEVELS[usize::from(offset % 6)],
            ))
        }
        _ => {
            let value = 8 + 10 * (index - 232);
            Some((value, value, value))
        }
    }
}

/// Resolve a palette colour to concrete RGB, preferring what the host terminal
/// actually reported. Returns `None` for `Color::Reset`, which means "whatever
/// the host is using" and therefore has no colour of its own.
pub(crate) fn resolve_color_rgb(color: Color, host: &TerminalTheme) -> Option<Rgb> {
    let measured = |index: u8| host.palette[usize::from(index)].map(terminal_theme_to_rgb);
    match color {
        Color::Reset => None,
        Color::Indexed(index) => measured(index).or_else(|| xterm_256_rgb(index)),
        other => ansi_index(other)
            .and_then(measured)
            .or_else(|| color_to_rgb(other)),
    }
}

/// Move `color` away from `background` until it clears `floor`, preserving as
/// much of the original colour as the floor allows.
///
/// The direction is whichever of black/white has more headroom against this
/// background, so mid-grey backgrounds resolve correctly rather than always
/// lightening. When the floor is unreachable the best achievable colour is
/// returned; the result is never lower-contrast than the input.
pub(crate) fn ensure_contrast(color: Rgb, background: Rgb, floor: f32) -> Rgb {
    let current = contrast_ratio(color, background);
    if current >= floor {
        return color;
    }

    let target = if contrast_ratio(WHITE, background) >= contrast_ratio(BLACK, background) {
        WHITE
    } else {
        BLACK
    };
    if contrast_ratio(target, background) <= current {
        return color;
    }

    // Contrast against a fixed background is monotone along the mix toward a
    // pure black/white target, so the smallest sufficient mix bisects cleanly.
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let mid = (low + high) / 2.0;
        if contrast_ratio(mix_rgb(color, target, mid), background) >= floor {
            high = mid;
        } else {
            low = mid;
        }
    }

    let lifted = mix_rgb(color, target, high);
    if contrast_ratio(lifted, background) >= current {
        lifted
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_with(index: u8, color: Rgb) -> TerminalTheme {
        TerminalTheme::default().with_palette_color(
            index,
            RgbColor {
                r: color.0,
                g: color.1,
                b: color.2,
            },
        )
    }

    #[test]
    fn contrast_ratio_matches_wcag_extremes() {
        assert!((contrast_ratio(WHITE, BLACK) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(WHITE, WHITE) - 1.0).abs() < 0.001);
    }

    #[test]
    fn measured_host_palette_beats_the_static_table() {
        // The host says its "white" slot is actually a light grey.
        let host = theme_with(15, (208, 208, 208));
        assert_eq!(
            resolve_color_rgb(Color::White, &host),
            Some((208, 208, 208))
        );
        // Unmeasured slots still resolve from the static table.
        assert_eq!(
            resolve_color_rgb(Color::DarkGray, &host),
            color_to_rgb(Color::DarkGray)
        );
    }

    #[test]
    fn reset_has_no_colour_of_its_own() {
        assert_eq!(
            resolve_color_rgb(Color::Reset, &TerminalTheme::default()),
            None
        );
    }

    #[test]
    fn indexed_falls_back_to_the_xterm_cube() {
        assert_eq!(
            resolve_color_rgb(Color::Indexed(196), &TerminalTheme::default()),
            Some((255, 0, 0))
        );
        assert_eq!(
            resolve_color_rgb(Color::Indexed(232), &TerminalTheme::default()),
            Some((8, 8, 8))
        );
    }

    #[test]
    fn ensure_contrast_leaves_compliant_colours_alone() {
        let color = (200, 200, 200);
        assert_eq!(ensure_contrast(color, BLACK, 4.5), color);
    }

    #[test]
    fn ensure_contrast_darkens_on_a_light_background() {
        let lifted = ensure_contrast(WHITE, WHITE, 4.5);
        assert!(contrast_ratio(lifted, WHITE) >= 4.5);
        assert!(relative_luminance(lifted) < relative_luminance(WHITE));
    }

    #[test]
    fn ensure_contrast_lightens_on_a_dark_background() {
        let lifted = ensure_contrast((20, 20, 20), BLACK, 4.5);
        assert!(contrast_ratio(lifted, BLACK) >= 4.5);
    }

    #[test]
    fn ensure_contrast_picks_the_direction_with_more_headroom() {
        // Above the black/white crossover: lightening tops out at 3.4 here,
        // darkening reaches 6.2, so the floor must be met by going darker.
        let background = (140, 140, 140);
        let lifted = ensure_contrast(background, background, 4.5);
        assert!(relative_luminance(lifted) < relative_luminance(background));
    }

    #[test]
    fn ensure_contrast_never_lowers_contrast_when_the_floor_is_unreachable() {
        let background = (128, 128, 128);
        let before = contrast_ratio(WHITE, background);
        let after = contrast_ratio(ensure_contrast(WHITE, background, 21.0), background);
        assert!(after >= before);
    }

    #[test]
    fn ensure_contrast_keeps_the_lift_minimal() {
        let background = BLACK;
        let lifted = ensure_contrast((40, 0, 0), background, 3.0);
        // Just over the floor, not slammed to pure white.
        assert!(contrast_ratio(lifted, background) >= 3.0);
        assert!(contrast_ratio(lifted, background) < 4.0);
    }
}
