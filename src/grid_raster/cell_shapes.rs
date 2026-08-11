//! The characters a terminal draws itself instead of taking from the font.
//!
//! # Why these are not glyphs
//!
//! Box drawing and the block elements are the only characters whose *job* is to
//! meet their neighbours. A `│` has to continue into the `│` in the cell below
//! with no seam, a `─` has to reach both side edges, and a `█` has to fill its
//! cell exactly so a wall of them is one solid field. Nothing about a font
//! guarantees any of that: a face's glyphs are sized from its own em box and
//! then fitted into whatever cell the host terminal chose, so the ink lands
//! *inside* the cell with a margin — which is correct for a letter and wrong
//! for a rule.
//!
//! Drawn from the face, at a 10x21 cell, `█` came out 19 pixels tall, leaving a
//! two-pixel dark seam between every pair of stacked blocks, and `│` broke into
//! a dashed line at every cell boundary (measured live under kitty, PR #133
//! follow-up). Every terminal emulator worth the name — kitty, Ghostty, iTerm2,
//! Windows Terminal — has the same module this one is: these characters are
//! computed from the cell rectangle and the font is not consulted.
//!
//! So this module maps a character to a handful of rectangles in *cell pixel*
//! coordinates, each snapped to the pixel grid for the line work so the strokes
//! stay crisp. A rectangle that runs to `cell_width` or `cell_height` reaches
//! the cell edge exactly, which is what makes two neighbouring cells join with
//! no seam and no overlap.
//!
//! # What is deliberately still a glyph
//!
//! The dashed lines (`┄ ┅ ┆ ┇ ┈ ┉ ┊ ┋ ╌ ╍ ╎ ╏`), the double lines
//! (`═ ║ ╔ … ╬`), the arcs (`╭ ╮ ╯ ╰`) and the diagonals (`╱ ╲ ╳`) return
//! [`None`] and keep taking the font path. Each needs geometry this
//! rectangle-only representation cannot express — a dash pattern, a pair of
//! strokes cut at their junctions, a quarter ellipse, a slope — and drawing
//! them badly here would be worse than the seam they have today.

/// A shape occupying one cell, as up to four axis-aligned rectangles in pixel
/// coordinates relative to the cell's top-left corner.
///
/// Rectangles are unioned rather than summed: `┼` is a horizontal pair and a
/// vertical pair that overlap in the middle, and ink that overlaps ink is still
/// just ink.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellShape {
    rects: [Rect; MAX_RECTS],
    len: usize,
    /// Uniform coverage multiplier. Only the shade blocks (`░ ▒ ▓`) use it;
    /// everything else is solid.
    alpha: f32,
}

/// `┼` and its heavy variants are the widest case: one rectangle per arm.
const MAX_RECTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

impl CellShape {
    fn new(alpha: f32) -> Self {
        Self {
            rects: [Rect::default(); MAX_RECTS],
            len: 0,
            alpha,
        }
    }

    fn push(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let rect = Rect { x0, y0, x1, y1 };
        if rect.is_empty() || self.len >= MAX_RECTS {
            return;
        }
        self.rects[self.len] = rect;
        self.len += 1;
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How much of the pixel at `(x, y)` — the square `[x, x+1) x [y, y+1)` in
    /// cell-local coordinates — this shape covers.
    ///
    /// Area coverage rather than a centre test, so the fractional edges of the
    /// eighth blocks (`▂` on an odd cell height) antialias instead of jumping a
    /// whole pixel. The line work is snapped to integers upstream, so it comes
    /// out of the same arithmetic perfectly crisp.
    pub(crate) fn coverage(&self, x: u32, y: u32) -> f32 {
        let px0 = x as f32;
        let py0 = y as f32;
        let px1 = px0 + 1.0;
        let py1 = py0 + 1.0;
        let mut best: f32 = 0.0;
        for rect in &self.rects[..self.len] {
            let w = (rect.x1.min(px1) - rect.x0.max(px0)).max(0.0);
            let h = (rect.y1.min(py1) - rect.y0.max(py0)).max(0.0);
            best = best.max(w * h);
        }
        (best * self.alpha).clamp(0.0, 1.0)
    }
}

/// Per-direction stroke weights for `U+2500..=U+257F`, as
/// `[up, down, left, right]` where `0` is absent, `1` light and `2` heavy.
///
/// An all-zero row is a character this module does not draw; it keeps the font
/// path. See the module doc for which those are and why.
#[rustfmt::skip]
const BOX_WEIGHTS: [[u8; 4]; 128] = [
    [0, 0, 1, 1], // U+2500 ─ light horizontal
    [0, 0, 2, 2], // U+2501 ━ heavy horizontal
    [1, 1, 0, 0], // U+2502 │ light vertical
    [2, 2, 0, 0], // U+2503 ┃ heavy vertical
    [0, 0, 0, 0], // U+2504 ┄ light triple dash horizontal
    [0, 0, 0, 0], // U+2505 ┅ heavy triple dash horizontal
    [0, 0, 0, 0], // U+2506 ┆ light triple dash vertical
    [0, 0, 0, 0], // U+2507 ┇ heavy triple dash vertical
    [0, 0, 0, 0], // U+2508 ┈ light quadruple dash horizontal
    [0, 0, 0, 0], // U+2509 ┉ heavy quadruple dash horizontal
    [0, 0, 0, 0], // U+250A ┊ light quadruple dash vertical
    [0, 0, 0, 0], // U+250B ┋ heavy quadruple dash vertical
    [0, 1, 0, 1], // U+250C ┌ light down and right
    [0, 1, 0, 2], // U+250D ┍ down light and right heavy
    [0, 2, 0, 1], // U+250E ┎ down heavy and right light
    [0, 2, 0, 2], // U+250F ┏ heavy down and right
    [0, 1, 1, 0], // U+2510 ┐ light down and left
    [0, 1, 2, 0], // U+2511 ┑ down light and left heavy
    [0, 2, 1, 0], // U+2512 ┒ down heavy and left light
    [0, 2, 2, 0], // U+2513 ┓ heavy down and left
    [1, 0, 0, 1], // U+2514 └ light up and right
    [1, 0, 0, 2], // U+2515 ┕ up light and right heavy
    [2, 0, 0, 1], // U+2516 ┖ up heavy and right light
    [2, 0, 0, 2], // U+2517 ┗ heavy up and right
    [1, 0, 1, 0], // U+2518 ┘ light up and left
    [1, 0, 2, 0], // U+2519 ┙ up light and left heavy
    [2, 0, 1, 0], // U+251A ┚ up heavy and left light
    [2, 0, 2, 0], // U+251B ┛ heavy up and left
    [1, 1, 0, 1], // U+251C ├ light vertical and right
    [1, 1, 0, 2], // U+251D ┝ vertical light and right heavy
    [2, 1, 0, 1], // U+251E ┞ up heavy and right down light
    [1, 2, 0, 1], // U+251F ┟ down heavy and right up light
    [2, 2, 0, 1], // U+2520 ┠ vertical heavy and right light
    [2, 1, 0, 2], // U+2521 ┡ down light and right up heavy
    [1, 2, 0, 2], // U+2522 ┢ up light and right down heavy
    [2, 2, 0, 2], // U+2523 ┣ heavy vertical and right
    [1, 1, 1, 0], // U+2524 ┤ light vertical and left
    [1, 1, 2, 0], // U+2525 ┥ vertical light and left heavy
    [2, 1, 1, 0], // U+2526 ┦ up heavy and left down light
    [1, 2, 1, 0], // U+2527 ┧ down heavy and left up light
    [2, 2, 1, 0], // U+2528 ┨ vertical heavy and left light
    [2, 1, 2, 0], // U+2529 ┩ down light and left up heavy
    [1, 2, 2, 0], // U+252A ┪ up light and left down heavy
    [2, 2, 2, 0], // U+252B ┫ heavy vertical and left
    [0, 1, 1, 1], // U+252C ┬ light down and horizontal
    [0, 1, 2, 1], // U+252D ┭ left heavy and right down light
    [0, 1, 1, 2], // U+252E ┮ right heavy and left down light
    [0, 1, 2, 2], // U+252F ┯ down light and horizontal heavy
    [0, 2, 1, 1], // U+2530 ┰ down heavy and horizontal light
    [0, 2, 2, 1], // U+2531 ┱ right light and left down heavy
    [0, 2, 1, 2], // U+2532 ┲ left light and right down heavy
    [0, 2, 2, 2], // U+2533 ┳ heavy down and horizontal
    [1, 0, 1, 1], // U+2534 ┴ light up and horizontal
    [1, 0, 2, 1], // U+2535 ┵ left heavy and right up light
    [1, 0, 1, 2], // U+2536 ┶ right heavy and left up light
    [1, 0, 2, 2], // U+2537 ┷ up light and horizontal heavy
    [2, 0, 1, 1], // U+2538 ┸ up heavy and horizontal light
    [2, 0, 2, 1], // U+2539 ┹ right light and left up heavy
    [2, 0, 1, 2], // U+253A ┺ left light and right up heavy
    [2, 0, 2, 2], // U+253B ┻ heavy up and horizontal
    [1, 1, 1, 1], // U+253C ┼ light vertical and horizontal
    [1, 1, 2, 1], // U+253D ┽ left heavy and right vertical light
    [1, 1, 1, 2], // U+253E ┾ right heavy and left vertical light
    [1, 1, 2, 2], // U+253F ┿ vertical light and horizontal heavy
    [2, 1, 1, 1], // U+2540 ╀ up heavy and down horizontal light
    [1, 2, 1, 1], // U+2541 ╁ down heavy and up horizontal light
    [2, 2, 1, 1], // U+2542 ╂ vertical heavy and horizontal light
    [2, 1, 2, 1], // U+2543 ╃ left up heavy and right down light
    [2, 1, 1, 2], // U+2544 ╄ right up heavy and left down light
    [1, 2, 2, 1], // U+2545 ╅ left down heavy and right up light
    [1, 2, 1, 2], // U+2546 ╆ right down heavy and left up light
    [2, 1, 2, 2], // U+2547 ╇ down light and up horizontal heavy
    [1, 2, 2, 2], // U+2548 ╈ up light and down horizontal heavy
    [2, 2, 2, 1], // U+2549 ╉ right light and left vertical heavy
    [2, 2, 1, 2], // U+254A ╊ left light and right vertical heavy
    [2, 2, 2, 2], // U+254B ╋ heavy vertical and horizontal
    [0, 0, 0, 0], // U+254C ╌ light double dash horizontal
    [0, 0, 0, 0], // U+254D ╍ heavy double dash horizontal
    [0, 0, 0, 0], // U+254E ╎ light double dash vertical
    [0, 0, 0, 0], // U+254F ╏ heavy double dash vertical
    [0, 0, 0, 0], // U+2550 ═ double horizontal
    [0, 0, 0, 0], // U+2551 ║ double vertical
    [0, 0, 0, 0], // U+2552 ╒ down single and right double
    [0, 0, 0, 0], // U+2553 ╓ down double and right single
    [0, 0, 0, 0], // U+2554 ╔ double down and right
    [0, 0, 0, 0], // U+2555 ╕ down single and left double
    [0, 0, 0, 0], // U+2556 ╖ down double and left single
    [0, 0, 0, 0], // U+2557 ╗ double down and left
    [0, 0, 0, 0], // U+2558 ╘ up single and right double
    [0, 0, 0, 0], // U+2559 ╙ up double and right single
    [0, 0, 0, 0], // U+255A ╚ double up and right
    [0, 0, 0, 0], // U+255B ╛ up single and left double
    [0, 0, 0, 0], // U+255C ╜ up double and left single
    [0, 0, 0, 0], // U+255D ╝ double up and left
    [0, 0, 0, 0], // U+255E ╞ vertical single and right double
    [0, 0, 0, 0], // U+255F ╟ vertical double and right single
    [0, 0, 0, 0], // U+2560 ╠ double vertical and right
    [0, 0, 0, 0], // U+2561 ╡ vertical single and left double
    [0, 0, 0, 0], // U+2562 ╢ vertical double and left single
    [0, 0, 0, 0], // U+2563 ╣ double vertical and left
    [0, 0, 0, 0], // U+2564 ╤ down single and horizontal double
    [0, 0, 0, 0], // U+2565 ╥ down double and horizontal single
    [0, 0, 0, 0], // U+2566 ╦ double down and horizontal
    [0, 0, 0, 0], // U+2567 ╧ up single and horizontal double
    [0, 0, 0, 0], // U+2568 ╨ up double and horizontal single
    [0, 0, 0, 0], // U+2569 ╩ double up and horizontal
    [0, 0, 0, 0], // U+256A ╪ vertical single and horizontal double
    [0, 0, 0, 0], // U+256B ╫ vertical double and horizontal single
    [0, 0, 0, 0], // U+256C ╬ double vertical and horizontal
    [0, 0, 0, 0], // U+256D ╭ light arc down and right
    [0, 0, 0, 0], // U+256E ╮ light arc down and left
    [0, 0, 0, 0], // U+256F ╯ light arc up and left
    [0, 0, 0, 0], // U+2570 ╰ light arc up and right
    [0, 0, 0, 0], // U+2571 ╱ light diagonal upper right to lower left
    [0, 0, 0, 0], // U+2572 ╲ light diagonal upper left to lower right
    [0, 0, 0, 0], // U+2573 ╳ light diagonal cross
    [0, 0, 1, 0], // U+2574 ╴ light left
    [1, 0, 0, 0], // U+2575 ╵ light up
    [0, 0, 0, 1], // U+2576 ╶ light right
    [0, 1, 0, 0], // U+2577 ╷ light down
    [0, 0, 2, 0], // U+2578 ╸ heavy left
    [2, 0, 0, 0], // U+2579 ╹ heavy up
    [0, 0, 0, 2], // U+257A ╺ heavy right
    [0, 2, 0, 0], // U+257B ╻ heavy down
    [0, 0, 1, 2], // U+257C ╼ light left and heavy right
    [1, 2, 0, 0], // U+257D ╽ light up and heavy down
    [0, 0, 2, 1], // U+257E ╾ heavy left and light right
    [2, 1, 0, 0], // U+257F ╿ heavy up and light down
];

/// The geometry for `ch` in a `cell_width x cell_height` cell, or [`None`] when
/// this character is one the font should draw.
pub(crate) fn shape_for(ch: char, cell_width: u32, cell_height: u32) -> Option<CellShape> {
    if cell_width == 0 || cell_height == 0 {
        return None;
    }
    let w = cell_width as f32;
    let h = cell_height as f32;
    let shape = match ch as u32 {
        code @ 0x2500..=0x257F => box_drawing(BOX_WEIGHTS[(code - 0x2500) as usize], w, h)?,
        0x2580..=0x259F => block_element(ch, w, h)?,
        _ => return None,
    };
    (!shape.is_empty()).then_some(shape)
}

/// Line thickness in pixels for a light (`weight == 1`) and a heavy
/// (`weight == 2`) stroke.
///
/// Whole pixels, from the cell's narrow axis so a stroke is the same thickness
/// horizontally and vertically, and heavy is exactly twice light so the two are
/// still told apart at the smallest cell anyone runs.
fn stroke_px(weight: u8, cell_width: f32, cell_height: f32) -> f32 {
    if weight == 0 {
        return 0.0;
    }
    let light = (cell_width.min(cell_height) / 8.0).round().max(1.0);
    if weight >= 2 {
        light * 2.0
    } else {
        light
    }
}

/// The band a stroke of `thickness` occupies across `extent`, snapped to whole
/// pixels and centred.
fn band(thickness: f32, extent: f32) -> (f32, f32) {
    let start = ((extent - thickness) / 2.0).floor().max(0.0);
    (start, (start + thickness).min(extent))
}

/// One rectangle per arm, each running from the cell edge to the far side of
/// the band the perpendicular arms occupy — which is what fills the junction
/// square and leaves no notch at a corner.
///
/// An arm with no perpendicular arm to meet (`╴`, `╵`) stops at the cell's
/// centre instead, which is what makes a stub half a line long.
fn box_drawing(weights: [u8; 4], w: f32, h: f32) -> Option<CellShape> {
    let [up, down, left, right] = weights;
    if up == 0 && down == 0 && left == 0 && right == 0 {
        return None;
    }

    let vertical = stroke_px(up.max(down), w, h);
    let horizontal = stroke_px(left.max(right), w, h);
    let (vx0, vx1) = band(vertical, w);
    let (hy0, hy1) = band(horizontal, h);

    // Where an arm stops when it has a perpendicular band to meet, and where it
    // stops when it does not.
    let (left_stop, right_stop) = if vertical > 0.0 {
        (vx1, vx0)
    } else {
        ((w / 2.0).ceil(), (w / 2.0).floor())
    };
    let (up_stop, down_stop) = if horizontal > 0.0 {
        (hy1, hy0)
    } else {
        ((h / 2.0).ceil(), (h / 2.0).floor())
    };

    let mut shape = CellShape::new(1.0);
    if left > 0 {
        let (y0, y1) = band(stroke_px(left, w, h), h);
        shape.push(0.0, y0, left_stop, y1);
    }
    if right > 0 {
        let (y0, y1) = band(stroke_px(right, w, h), h);
        shape.push(right_stop, y0, w, y1);
    }
    if up > 0 {
        let (x0, x1) = band(stroke_px(up, w, h), w);
        shape.push(x0, 0.0, x1, up_stop);
    }
    if down > 0 {
        let (x0, x1) = band(stroke_px(down, w, h), w);
        shape.push(x0, down_stop, x1, h);
    }
    Some(shape)
}

/// `U+2580..=U+259F`: fractions of the cell, exactly.
///
/// The eighths are the point of the range and the reason the fractions are not
/// rounded: a sparkline built from `▁▂▃▄▅▆▇█` is read by comparing bar heights,
/// so a bar that snaps to the nearest pixel row loses steps at small cell
/// sizes. The edges antialias instead.
fn block_element(ch: char, w: f32, h: f32) -> Option<CellShape> {
    let eighth = |n: f32| n / 8.0;
    let mut shape = CellShape::new(1.0);
    match ch {
        // Upper half, then the lower eighths from one up to the full block.
        '\u{2580}' => shape.push(0.0, 0.0, w, h / 2.0),
        '\u{2581}'..='\u{2588}' => {
            let filled = (ch as u32 - 0x2580) as f32;
            shape.push(0.0, h - h * eighth(filled), w, h);
        }
        // The left eighths run the other way: 2589 is seven eighths, 258F one.
        '\u{2589}'..='\u{258F}' => {
            let filled = (0x2590 - ch as u32) as f32;
            shape.push(0.0, 0.0, w * eighth(filled), h);
        }
        '\u{2590}' => shape.push(w / 2.0, 0.0, w, h),
        // The shades are the full cell at a fraction of the ink. A stipple
        // would be closer to the printed character, but at cell sizes this
        // small it aliases into stripes, and this is what the eye reads as a
        // lighter tone anyway.
        '\u{2591}' | '\u{2592}' | '\u{2593}' => {
            let alpha = match ch {
                '\u{2591}' => 0.25,
                '\u{2592}' => 0.5,
                _ => 0.75,
            };
            shape = CellShape::new(alpha);
            shape.push(0.0, 0.0, w, h);
        }
        '\u{2594}' => shape.push(0.0, 0.0, w, h * eighth(1.0)),
        '\u{2595}' => shape.push(w - w * eighth(1.0), 0.0, w, h),
        // The quadrants, as one rectangle each, unioned.
        '\u{2596}'..='\u{259F}' => {
            const QUADRANTS: [[bool; 4]; 10] = [
                // [upper-left, upper-right, lower-left, lower-right]
                [false, false, true, false], // 2596 ▖
                [false, false, false, true], // 2597 ▗
                [true, false, false, false], // 2598 ▘
                [true, false, true, true],   // 2599 ▙
                [true, false, false, true],  // 259A ▚
                [true, true, true, false],   // 259B ▛
                [true, true, false, true],   // 259C ▜
                [false, true, false, false], // 259D ▝
                [false, true, true, false],  // 259E ▞
                [false, true, true, true],   // 259F ▟
            ];
            let quadrant = QUADRANTS[(ch as u32 - 0x2596) as usize];
            let (mx, my) = (w / 2.0, h / 2.0);
            if quadrant[0] {
                shape.push(0.0, 0.0, mx, my);
            }
            if quadrant[1] {
                shape.push(mx, 0.0, w, my);
            }
            if quadrant[2] {
                shape.push(0.0, my, mx, h);
            }
            if quadrant[3] {
                shape.push(mx, my, w, h);
            }
        }
        _ => return None,
    }
    Some(shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Total coverage over a whole cell, in pixels' worth of ink.
    fn ink(shape: &CellShape, w: u32, h: u32) -> f32 {
        let mut total = 0.0;
        for y in 0..h {
            for x in 0..w {
                total += shape.coverage(x, y);
            }
        }
        total
    }

    /// Every cell size worth checking: the captain's own 10x21, a wide-cell
    /// case, and a cell small enough that a whole-pixel stroke is most of it.
    const CELLS: [(u32, u32); 4] = [(10, 21), (9, 19), (14, 18), (4, 8)];

    #[test]
    fn the_full_block_covers_every_pixel_of_its_cell() {
        for (w, h) in CELLS {
            let shape = shape_for('\u{2588}', w, h).expect("the full block is drawn here");
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        shape.coverage(x, y),
                        1.0,
                        "{w}x{h}: full block leaves pixel ({x}, {y}) uncovered, which is the \
                         seam between two stacked blocks"
                    );
                }
            }
        }
    }

    #[test]
    fn a_vertical_rule_reaches_both_cell_edges() {
        for (w, h) in CELLS {
            for ch in ['\u{2502}', '\u{2503}', '\u{253C}'] {
                let shape = shape_for(ch, w, h).expect("a vertical arm is drawn here");
                let lit = |y: u32| (0..w).any(|x| shape.coverage(x, y) > 0.0);
                assert!(
                    lit(0) && lit(h - 1),
                    "{w}x{h}: {ch:?} does not reach both cell edges, so a stacked pair breaks"
                );
                // The same columns at the top and the bottom, or the rule bends
                // where two cells meet.
                let columns = |y: u32| (0..w).filter(|&x| shape.coverage(x, y) > 0.0).count();
                assert_eq!(columns(0), columns(h - 1), "{w}x{h}: {ch:?} changes width");
            }
        }
    }

    #[test]
    fn a_horizontal_rule_reaches_both_cell_edges() {
        for (w, h) in CELLS {
            for ch in ['\u{2500}', '\u{2501}', '\u{253C}'] {
                let shape = shape_for(ch, w, h).expect("a horizontal arm is drawn here");
                let lit = |x: u32| (0..h).any(|y| shape.coverage(x, y) > 0.0);
                assert!(
                    lit(0) && lit(w - 1),
                    "{w}x{h}: {ch:?} does not reach both cell edges, so a run of them breaks"
                );
            }
        }
    }

    #[test]
    fn a_corner_reaches_the_edges_its_arms_point_at_and_no_others() {
        let (w, h) = (10, 21);
        // ┌ turns down and right: it must touch the right and bottom edges and
        // leave the left and top alone.
        let shape = shape_for('\u{250C}', w, h).expect("the corner is drawn here");
        let col = |x: u32| (0..h).any(|y| shape.coverage(x, y) > 0.0);
        let row = |y: u32| (0..w).any(|x| shape.coverage(x, y) > 0.0);
        assert!(col(w - 1), "no ink at the right edge");
        assert!(row(h - 1), "no ink at the bottom edge");
        assert!(!col(0), "ink at the left edge, where no arm points");
        assert!(!row(0), "ink at the top edge, where no arm points");
    }

    #[test]
    fn a_corner_joins_its_own_arms() {
        // The junction must be filled: walking the horizontal arm inwards from
        // the right edge and the vertical arm downwards from the bottom, the
        // two must overlap rather than stop short of each other.
        for (w, h) in CELLS {
            let shape = shape_for('\u{250C}', w, h).expect("the corner is drawn here");
            let mid_y = (0..h)
                .find(|&y| shape.coverage(w - 1, y) > 0.0)
                .expect("the horizontal arm reaches the right edge");
            let mid_x = (0..w)
                .find(|&x| shape.coverage(x, h - 1) > 0.0)
                .expect("the vertical arm reaches the bottom edge");
            assert!(
                shape.coverage(mid_x, mid_y) > 0.0,
                "{w}x{h}: the corner has a notch where its arms should meet"
            );
        }
    }

    #[test]
    fn a_stub_stops_at_the_middle() {
        let (w, h) = (10, 21);
        let shape = shape_for('\u{2574}', w, h).expect("the left stub is drawn here");
        assert!(
            (0..h).any(|y| shape.coverage(0, y) > 0.0),
            "the stub does not start at the left edge"
        );
        assert!(
            (0..h).all(|y| shape.coverage(w - 1, y) == 0.0),
            "the stub runs to the right edge, so it is a full rule, not a stub"
        );
    }

    #[test]
    fn heavy_is_thicker_than_light() {
        for (w, h) in CELLS {
            let light = shape_for('\u{2502}', w, h).expect("light vertical");
            let heavy = shape_for('\u{2503}', w, h).expect("heavy vertical");
            assert!(
                ink(&heavy, w, h) > ink(&light, w, h),
                "{w}x{h}: the heavy vertical is not thicker than the light one"
            );
        }
    }

    #[test]
    fn the_eighth_blocks_step_evenly() {
        let (w, h) = (16, 32);
        let mut previous = 0.0;
        for step in 1..=8u32 {
            let ch = char::from_u32(0x2580 + step).expect("a block element");
            let shape = shape_for(ch, w, h).expect("the lower eighths are drawn here");
            let filled = ink(&shape, w, h);
            let expected = (w * h) as f32 * (step as f32 / 8.0);
            assert!(
                (filled - expected).abs() < 0.5,
                "{ch:?}: covered {filled} pixels, expected {expected}"
            );
            assert!(
                filled > previous,
                "{ch:?} is not taller than the step below"
            );
            previous = filled;
        }
    }

    #[test]
    fn the_half_blocks_tile_into_a_whole_one() {
        for (w, h) in CELLS {
            for (a, b) in [('\u{2580}', '\u{2584}'), ('\u{258C}', '\u{2590}')] {
                let first = shape_for(a, w, h).expect("a half block");
                let second = shape_for(b, w, h).expect("its opposite half");
                for y in 0..h {
                    for x in 0..w {
                        let total = first.coverage(x, y) + second.coverage(x, y);
                        assert!(
                            (total - 1.0).abs() < 0.01,
                            "{w}x{h}: {a:?} and {b:?} cover pixel ({x}, {y}) {total} times, so \
                             they overlap or leave a gap"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_quadrants_tile_into_a_whole_cell() {
        let (w, h) = (10, 20);
        let corners = ['\u{2598}', '\u{259D}', '\u{2596}', '\u{2597}'];
        for y in 0..h {
            for x in 0..w {
                let total: f32 = corners
                    .iter()
                    .map(|&ch| shape_for(ch, w, h).expect("a quadrant").coverage(x, y))
                    .sum();
                assert!(
                    (total - 1.0).abs() < 0.01,
                    "the four quadrants cover pixel ({x}, {y}) {total} times"
                );
            }
        }
    }

    #[test]
    fn the_shades_are_ordered_and_partial() {
        let (w, h) = (10, 21);
        let mut previous = 0.0;
        for ch in ['\u{2591}', '\u{2592}', '\u{2593}'] {
            let shape = shape_for(ch, w, h).expect("a shade block");
            let coverage = shape.coverage(w / 2, h / 2);
            assert!(
                coverage > previous && coverage < 1.0,
                "{ch:?} covers {coverage}, which is not a partial tone above the one before it"
            );
            previous = coverage;
        }
    }

    #[test]
    fn the_characters_this_module_declines_keep_the_font_path() {
        // Dashes, doubles, arcs and diagonals need geometry a rectangle list
        // cannot express — see the module doc.
        for ch in [
            '\u{2504}', '\u{250A}', '\u{254C}', '\u{2550}', '\u{2551}', '\u{2554}', '\u{256C}',
            '\u{256D}', '\u{2570}', '\u{2571}', '\u{2573}', 'A', ' ', '\u{25CF}',
        ] {
            assert!(
                shape_for(ch, 10, 21).is_none(),
                "{ch:?} should be left to the font"
            );
        }
    }

    #[test]
    fn a_degenerate_cell_is_declined_rather_than_drawn() {
        assert!(shape_for('\u{2588}', 0, 21).is_none());
        assert!(shape_for('\u{2588}', 10, 0).is_none());
    }

    #[test]
    fn every_weight_in_the_table_is_a_known_stroke() {
        for (index, weights) in BOX_WEIGHTS.iter().enumerate() {
            for weight in weights {
                assert!(
                    *weight <= 2,
                    "U+{:04X} has an unknown stroke weight {weight}",
                    0x2500 + index
                );
            }
        }
    }

    #[test]
    fn every_drawn_box_character_produces_ink() {
        for (index, weights) in BOX_WEIGHTS.iter().enumerate() {
            if weights.iter().all(|w| *w == 0) {
                continue;
            }
            let ch = char::from_u32(0x2500 + index as u32).expect("a box drawing character");
            for (w, h) in CELLS {
                let shape = shape_for(ch, w, h).expect("a weighted row is drawn");
                assert!(
                    ink(&shape, w, h) > 0.0,
                    "{w}x{h}: {ch:?} draws nothing despite having arms"
                );
            }
        }
    }

    #[test]
    fn every_block_element_produces_ink() {
        for code in 0x2580..=0x259Fu32 {
            let ch = char::from_u32(code).expect("a block element");
            let shape = shape_for(ch, 10, 21).expect("the whole block range is drawn");
            assert!(ink(&shape, 10, 21) > 0.0, "{ch:?} draws nothing");
        }
    }
}
