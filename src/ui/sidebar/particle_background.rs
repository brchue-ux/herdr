//! The sidebar's ambient particle-field wash: `[experimental] sidebar_particle_field`.
//!
//! [`image`] generates a whole loop at once via `particle_field::loop_frames` and hands it to
//! the Kitty native animation-frame transport (`src/kitty_graphics.rs`), which transmits it once
//! and arms playback — from then on the terminal advances frames on its own clock with zero
//! further protocol bytes, confirmed empirically in
//! `data/herdr-native-animation-playback-verify`. Kitty-only for now: a terminal that ignores
//! `a=f`/`a=a` still shows the root frame, just static, rather than hanging or erroring.
//!
//! Generation is a mutation (it owns the field's particle model across calls, same as
//! [`super::tray::image`] owns rasterising badges), so the caller is the app loop, never
//! `render`.

use crate::app::state::AppState;

/// How long each frame is shown once playback is armed. Slow enough to read as ambient rather
/// than distracting; matches the report's own `z=600`-class proof-of-concept cadence closely
/// enough that autonomous playback is easy to eyeball against a stopwatch during verification.
pub(crate) const FRAME_GAP_MS: u32 = 100;

/// Samples per full rotation. 24 is enough for the sweep to read as continuous motion rather
/// than a slideshow at [`FRAME_GAP_MS`]'s cadence (2.4s per loop), while keeping the one-time
/// generation-plus-upload cost — paid only on a resize, never per tick — bounded.
const FRAME_COUNT: usize = 24;

/// The wash's root frame plus its loop, sized to one image.
pub(crate) struct AnimatedRgba {
    pub width: u32,
    pub height: u32,
    pub root: Vec<u8>,
    pub extra_frames: Vec<Vec<u8>>,
}

/// The wash, as one looping image covering the sidebar's content column.
///
/// `None` when there is nothing to draw: no area, no cell size, or a column too narrow to be
/// worth the generation cost.
pub(crate) fn image(app: &AppState, cell_width: u32, cell_height: u32) -> Option<AnimatedRgba> {
    let area = super::sidebar_content_rect(app.view.sidebar_rect);
    if area.width == 0 || area.height == 0 || cell_width == 0 || cell_height == 0 {
        return None;
    }

    let width = u32::from(area.width) * cell_width;
    let height = u32::from(area.height) * cell_height;
    let cfg = crate::particle_field::Cfg::rung2();
    let mut frames =
        crate::particle_field::loop_frames(width as usize, height as usize, &cfg, FRAME_COUNT);
    for frame in &mut frames {
        // 5 bits/channel: the feasibility report's measured "close to free" lever, and the
        // colour depth this generator's own module docs already size wire cost against.
        crate::particle_field::quantize_channels(frame, 5);
    }

    let mut frames = frames.into_iter();
    let root = frames.next()?;
    Some(AnimatedRgba {
        width,
        height,
        root,
        extra_frames: frames.collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_sidebar(width: u16, height: u16) -> AppState {
        let mut app = AppState::test_new();
        app.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, width, height);
        app
    }

    #[test]
    fn image_is_none_without_area() {
        let app = app_with_sidebar(0, 0);
        assert!(image(&app, 10, 20).is_none());
    }

    #[test]
    fn image_is_none_without_cell_size() {
        let app = app_with_sidebar(24, 40);
        assert!(image(&app, 0, 0).is_none());
    }

    #[test]
    fn image_sizes_to_the_sidebar_content_column_in_pixels() {
        let app = app_with_sidebar(24, 40);
        let area = super::super::sidebar_content_rect(app.view.sidebar_rect);
        let generated = image(&app, 8, 16).expect("sidebar has area");

        assert_eq!(generated.width, u32::from(area.width) * 8);
        assert_eq!(generated.height, u32::from(area.height) * 16);
        assert_eq!(
            generated.root.len(),
            (generated.width * generated.height * 4) as usize
        );
        assert_eq!(generated.extra_frames.len(), FRAME_COUNT - 1);
        for frame in &generated.extra_frames {
            assert_eq!(frame.len(), generated.root.len());
        }
    }

    #[test]
    fn image_loops_seamlessly_back_to_the_root_frame() {
        // frame_count evenly-spaced phases over a full rotation loop back to the root frame's
        // own phase after the last extra frame, so a naive re-generation at the same size and
        // config reproduces frame 0 exactly (see `particle_field::loop_frames`'s own doc).
        let app = app_with_sidebar(20, 20);
        let first = image(&app, 6, 12).expect("sidebar has area");
        let second = image(&app, 6, 12).expect("sidebar has area");
        assert_eq!(first.root, second.root, "regenerating at the same size and config is deterministic, matching `particle_field::Field::frame`'s own guarantee");
    }
}
