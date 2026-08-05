//! The notification tray: two rows of four badges at the foot of the panel.
//!
//! The tray is a rectangle at the bottom of the Spaces panel, and its two rows
//! each mean something — the first four are the fleet waiting on you, the last
//! four are the repository waiting on you. See
//! [`crate::app::fleet_signals::FleetSignal`] for the set and
//! [`crate::app::signal_tray`] for what a click is allowed to do.
//!
//! Three properties this module is responsible for holding:
//!
//! - **The tray is measured before the tree is laid out.** [`reserved_rows`] is
//!   the single number every caller subtracts, so the tree above cannot be laid
//!   out over the top of the tray or leave a gap above it. A panel too short to
//!   hold both draws the tree and no tray: the tree is the thing the panel is
//!   *for*, and a tray that had eaten it would be a bad trade.
//! - **Layout and hit testing come from the same function.** [`slot_rect`] is
//!   what the renderer draws into, what the image is placed on, and what
//!   [`badge_at`] tests against, so a badge can never be drawn in one place and
//!   clicked in another.
//! - **The character grid is the fallback, not the design.** The badges are
//!   images ([`super::tray_art`]); this module draws the marks in cells only for
//!   a host that cannot show one, so the tray still works — and is still
//!   testable against a `TestBackend` — without graphics.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::tray_art::{self, BadgePaint, Rgba};
use crate::app::fleet_signals::FleetSignal;
use crate::app::signal_tray::{self, BadgeState};
use crate::app::state::{AppState, Palette};

/// The tray's header row, carrying its name and the legend button.
const HEADER_ROWS: u16 = 1;
/// One clear row under the badges, so the tray does not sit flush on the
/// panel's footer.
const FOOT_ROWS: u16 = 1;

/// What the legend button is drawn as.
const MENU_LABEL: &str = "···";

/// The tray's own name on its header row.
const TRAY_LABEL: &str = "signals";

/// The fewest rows the tree may be left with before the tray gives up its
/// place entirely.
///
/// The panel exists to show the fleet. A tray that squeezed the tree down to
/// two rows would be a readout of a tree nobody can read.
const MIN_TREE_ROWS: u16 = 6;

/// The fewest columns one slot can be drawn in.
const MIN_SLOT_COLS: u16 = 3;

/// Which form the tray draws in.
///
/// Ordered tallest first, which is also the order [`Tier::tallest_fitting`]
/// searches: the tray always draws as large as the panel can hold, because the
/// badge's interior detail is exactly what is lost first when it shrinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Four rows per badge row. The size the marks were drawn for.
    Tall,
    /// Three rows per badge row. The honest compromise on a short panel: the
    /// marks still read, and their interior detail is softer.
    Short,
}

impl Tier {
    const ALL: [Self; 2] = [Self::Tall, Self::Short];

    /// Rows one row of badges occupies.
    fn slot_rows(self) -> u16 {
        match self {
            Self::Tall => 4,
            Self::Short => 3,
        }
    }

    /// Rows the whole tray occupies, header and foot included.
    pub(crate) fn rows(self) -> u16 {
        HEADER_ROWS + self.slot_rows() * 2 + FOOT_ROWS
    }

    /// The tallest tier that leaves the tree at least [`MIN_TREE_ROWS`].
    fn tallest_fitting(available: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|tier| available >= tier.rows() + MIN_TREE_ROWS)
    }
}

/// Whether the tray is drawn at all right now.
///
/// Off by default in config, and never drawn over a collapsed panel — a
/// collapsed sidebar has no room for a name, let alone eight of them.
pub(crate) fn active(app: &AppState) -> bool {
    app.sidebar_signal_tray.enabled && !app.sidebar_collapsed
}

/// The tier this panel can hold, or `None` when it can hold none.
fn tier(app: &AppState, area: Rect) -> Option<Tier> {
    if !active(app) || area.width < MIN_SLOT_COLS * FleetSignal::PER_ROW as u16 {
        return None;
    }
    Tier::tallest_fitting(area.height)
}

/// Rows the tray takes off the bottom of the panel's content column.
///
/// The one number the tree's layout subtracts. `0` whenever the tray is off or
/// the panel cannot hold it, which is what makes an unconfigured Herdr's
/// geometry byte-for-byte what it was before the tray existed.
pub(crate) fn reserved_rows(app: &AppState, area: Rect) -> u16 {
    tier(app, area).map_or(0, Tier::rows)
}

/// The tray's whole rect, or an empty one when it is not drawn.
///
/// `area` is the panel's content column — everything left of the divider bar,
/// which is what [`crate::ui::sidebar_content_rect`] returns. The tray sits at
/// its foot, above the row the `new` button and the collapse toggle share.
pub(crate) fn tray_rect(app: &AppState, area: Rect) -> Rect {
    let rows = reserved_rows(app, area);
    if rows == 0 || area.height <= rows {
        return Rect::default();
    }
    // One row is left for the panel's footer, the same row the tree already
    // keeps clear.
    let bottom = area.y
        + area
            .height
            .saturating_sub(super::WORKSPACE_SECTION_FOOTER_ROWS);
    Rect::new(area.x, bottom.saturating_sub(rows), area.width, rows)
}

/// The rect the eight badges are laid out in: everything but header and foot.
pub(crate) fn grid_rect(tray: Rect) -> Rect {
    if tray.height <= HEADER_ROWS + FOOT_ROWS {
        return Rect::default();
    }
    Rect::new(
        tray.x,
        tray.y + HEADER_ROWS,
        tray.width,
        tray.height - HEADER_ROWS - FOOT_ROWS,
    )
}

/// One badge's slot, by its index in [`FleetSignal::ALL`].
///
/// Row-major: the first four fill the top row, the last four the bottom. The
/// leftover columns from an uneven division are spread across the leading slots
/// one at a time rather than pooled at one end, so the grid stays even.
pub(crate) fn slot_rect(grid: Rect, index: usize) -> Rect {
    let per_row = FleetSignal::PER_ROW as u16;
    if grid.width < per_row || grid.height < 2 || index >= FleetSignal::COUNT {
        return Rect::default();
    }
    let column = index as u16 % per_row;
    let row = index as u16 / per_row;

    let base = grid.width / per_row;
    let extra = grid.width % per_row;
    let x = grid.x + base * column + column.min(extra);
    let width = base + u16::from(column < extra);

    let slot_rows = grid.height / 2;
    let y = grid.y + slot_rows * row;
    Rect::new(x, y, width, slot_rows)
}

/// The legend button at the right end of the header row.
pub(crate) fn menu_rect(tray: Rect) -> Rect {
    let width = crate::ui::text::display_width_u16(MENU_LABEL);
    if tray.height == 0 || tray.width <= width {
        return Rect::default();
    }
    Rect::new(tray.x + tray.width - width, tray.y, width, HEADER_ROWS)
}

/// Which badge covers this cell, if any.
///
/// Recomputed from the live panel rect rather than cached, for the same reason
/// the worker-summary badge's hit test is: the panel moves whenever the divider
/// is dragged or the layout changes, and a stale hit rect would open the wrong
/// badge.
pub(crate) fn badge_at(app: &AppState, col: u16, row: u16) -> Option<FleetSignal> {
    let tray = tray_rect(app, super::sidebar_content_rect(app.view.sidebar_rect));
    let grid = grid_rect(tray);
    FleetSignal::ALL
        .into_iter()
        .enumerate()
        .find(|(index, _)| contains(slot_rect(grid, *index), col, row))
        .map(|(_, signal)| signal)
}

/// Whether this cell is the legend button.
pub(crate) fn menu_at(app: &AppState, col: u16, row: u16) -> bool {
    let tray = tray_rect(app, super::sidebar_content_rect(app.view.sidebar_rect));
    contains(menu_rect(tray), col, row)
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

/// The colour a badge's fallback mark draws in.
///
/// The same palette roles the signal bar uses, so a reader who has learnt the
/// bar has learnt the tray. `Idle` is the panel's muted grey — the same grey a
/// resting bar slot draws in.
fn mark_style(signal: FleetSignal, state: BadgeState, p: &Palette) -> Style {
    let live = match signal {
        FleetSignal::Ask => p.red,
        FleetSignal::Review => p.teal,
        FleetSignal::Report => p.mauve,
        FleetSignal::Stopped => p.peach,
        FleetSignal::Push => p.blue,
        FleetSignal::Sync => p.green,
        FleetSignal::Pr => p.text,
        FleetSignal::Checks => p.yellow,
    };
    // Same fallback the bar makes: a hue that resolves to the resting grey on
    // some themes would leave a live badge looking dead.
    let live = if live == p.overlay0 { p.text } else { live };
    match state {
        BadgeState::Idle => Style::default().fg(p.overlay0),
        BadgeState::Active => Style::default().fg(live).add_modifier(Modifier::BOLD),
        BadgeState::Attention => Style::default()
            .fg(p.peach)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    }
}

/// Draw the tray into the panel's content column.
///
/// What lands on the character grid is the header, the legend button and — only
/// when there is no artwork — the eight fallback marks. The crafted badges are
/// an image the app loop rasterised, which the graphics pipeline composites over
/// exactly these slots (see [`super::tray_art`]).
///
/// The two are deliberately **exclusive**, not stacked. A badge is mostly
/// transparent by design — amendment one, no plate background — so a mark drawn
/// underneath it does not get covered, it shows *through*, and a stray `●` in
/// the middle of a speech bubble is worse than either alone. The artwork's
/// presence in [`crate::app::state::AppState::signal_tray_graphics`] is the
/// single fact that decides, and it is already state the app loop maintains, so
/// this stays a pure read.
pub(crate) fn render(app: &AppState, frame: &mut Frame, area: Rect) {
    let tray = tray_rect(app, area);
    if tray.width == 0 || tray.height == 0 {
        return;
    }
    let p = &app.sidebar_palette();
    let reading = signal_tray::resolve(app);

    // The header: the tray's name, and the legend at the right end. The name is
    // the first half of what stops the tray being eight pictures nobody can
    // name; the legend behind the button is the other half.
    //
    // The name is also where the fleet's own pulse lands. "Something is
    // working" used to be a ninth slot called `busy`, and it did not belong
    // there — nobody owns it, it clears itself, and it points at every pane at
    // once. It is a good ambient reading all the same, so it tints the tray
    // rather than occupying one of the eight. This is the cheap version of that:
    // one label, one existing palette role, no new element and no new clock.
    let menu = menu_rect(tray);
    let name_width = tray.width.saturating_sub(menu.width.saturating_add(1));
    if name_width > 0 {
        let working = reading.activity() > 0.0;
        let name_style = if working {
            Style::default().fg(p.overlay1)
        } else {
            Style::default().fg(p.overlay0)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(TRAY_LABEL, name_style)),
            Rect::new(tray.x, tray.y, name_width, HEADER_ROWS),
        );
    }
    if menu.width > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(MENU_LABEL, Style::default().fg(p.overlay1)))
                .alignment(Alignment::Right),
            menu,
        );
    }

    if app.signal_tray_graphics.is_some() {
        return;
    }

    let grid = grid_rect(tray);
    for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
        let slot = slot_rect(grid, index);
        if slot.width == 0 || slot.height == 0 {
            continue;
        }
        let badge = reading.badge(signal);
        let mark = Rect::new(slot.x, slot.y + slot.height / 2, slot.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                signal.mark(),
                mark_style(signal, badge.state, p),
            )))
            .alignment(Alignment::Center),
            mark,
        );
    }
}

/// The tray's badge artwork, as one image covering [`grid_rect`].
///
/// One image rather than eight, because the graphics surface holds one layer
/// per surface and eight placements would be eight of everything — eight ids,
/// eight cache entries, eight chances for one to be left behind when the panel
/// moves.
///
/// `None` when there is nothing to draw. The caller is the app loop, never the
/// renderer: rasterising is a mutation and `render` only draws.
pub(crate) fn image(app: &AppState, cell_width: u32, cell_height: u32) -> Option<(Rect, Rgba)> {
    let area = super::sidebar_content_rect(app.view.sidebar_rect);
    let grid = grid_rect(tray_rect(app, area));
    if grid.width == 0 || grid.height == 0 || cell_width == 0 || cell_height == 0 {
        return None;
    }

    let width = u32::from(grid.width) * cell_width;
    let height = u32::from(grid.height) * cell_height;
    let mut canvas = Rgba {
        width,
        height,
        pixels: vec![0; (width as usize) * (height as usize) * 4],
    };

    let paint = BadgePaint {
        attention: rgb_of(app.palette.peach, app),
        // Herdr paints no global background fill, so what an engraved badge is
        // actually cut into is whatever the host terminal reported. A host that
        // never answered falls back to the canvas the marks were designed
        // against rather than to black, which would over-darken every carve.
        surface: app
            .host_terminal_theme
            .background
            .map(crate::ui::color::terminal_theme_to_rgb)
            .map_or(DEFAULT_CANVAS, normalise),
    };
    let reading = signal_tray::resolve(app);

    for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
        let slot = slot_rect(grid, index);
        if slot.width == 0 || slot.height == 0 {
            continue;
        }
        let slot_w = u32::from(slot.width) * cell_width;
        let slot_h = u32::from(slot.height) * cell_height;
        // Square, and inset so two badges never touch: the gap is what lets the
        // eye count eight things rather than read one strip.
        let size = slot_w.min(slot_h).saturating_sub(2);
        if size == 0 {
            continue;
        }
        let badge = tray_art::render_badge(signal, reading.badge(signal).state, size, paint);
        let ox = u32::from(slot.x - grid.x) * cell_width + (slot_w.saturating_sub(size)) / 2;
        let oy = u32::from(slot.y - grid.y) * cell_height + (slot_h.saturating_sub(size)) / 2;
        blit(&mut canvas, &badge, ox, oy);
    }

    Some((grid, canvas))
}

fn blit(canvas: &mut Rgba, badge: &Rgba, ox: u32, oy: u32) {
    for y in 0..badge.height {
        for x in 0..badge.width {
            let (dx, dy) = (ox + x, oy + y);
            if dx >= canvas.width || dy >= canvas.height {
                continue;
            }
            let src = (((y * badge.width) + x) * 4) as usize;
            let dst = (((dy * canvas.width) + dx) * 4) as usize;
            canvas.pixels[dst..dst + 4].copy_from_slice(&badge.pixels[src..src + 4]);
        }
    }
}

/// The canvas the marks were designed against, for a host that never reported
/// its own background. Sampled from the reference illustration, like the rest of
/// the tray's colour.
const DEFAULT_CANVAS: [f32; 3] = [
    0x09 as f32 / 255.0,
    0x11 as f32 / 255.0,
    0x1C as f32 / 255.0,
];

fn normalise((r, g, b): crate::ui::color::Rgb) -> [f32; 3] {
    [
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    ]
}

/// A palette colour as `0.0..=1.0` RGB, resolved through the host terminal's
/// *measured* palette rather than a static table — the same funnel every other
/// colour decision in Herdr goes through.
fn rgb_of(color: ratatui::style::Color, app: &AppState) -> [f32; 3] {
    crate::ui::color::resolve_color_rgb(color, &app.host_terminal_theme)
        .map_or(DEFAULT_CANVAS, normalise)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_tray(width: u16, height: u16) -> AppState {
        let mut app = AppState::test_new();
        app.sidebar_signal_tray.enabled = true;
        app.view.sidebar_rect = Rect::new(0, 0, width, height);
        app
    }

    fn content(app: &AppState) -> Rect {
        crate::ui::sidebar::sidebar_content_rect(app.view.sidebar_rect)
    }

    /// The tray never takes the tree's last rows. A panel that cannot hold both
    /// keeps the tree, because the tree is what the panel is for.
    #[test]
    fn a_panel_too_short_for_both_keeps_the_tree_and_drops_the_tray() {
        for height in 0..(Tier::Short.rows() + MIN_TREE_ROWS) {
            let app = app_with_tray(42, height);
            assert_eq!(
                reserved_rows(&app, content(&app)),
                0,
                "a {height}-row panel reserved rows for the tray"
            );
        }
    }

    #[test]
    fn the_tallest_tier_the_panel_can_hold_is_the_one_chosen() {
        let app = app_with_tray(42, Tier::Tall.rows() + MIN_TREE_ROWS);
        assert_eq!(reserved_rows(&app, content(&app)), Tier::Tall.rows());

        let app = app_with_tray(42, Tier::Tall.rows() + MIN_TREE_ROWS - 1);
        assert_eq!(reserved_rows(&app, content(&app)), Tier::Short.rows());
    }

    /// An unconfigured Herdr's panel geometry has to be exactly what it was
    /// before the tray existed, or turning a feature off would not be off.
    #[test]
    fn a_tray_that_is_off_costs_the_panel_nothing() {
        let mut app = app_with_tray(42, 60);
        app.sidebar_signal_tray.enabled = false;
        assert_eq!(reserved_rows(&app, content(&app)), 0);
        assert_eq!(tray_rect(&app, content(&app)), Rect::default());

        // Collapsed is the same answer: there is no room for a name, let alone
        // eight of them.
        app.sidebar_signal_tray.enabled = true;
        app.sidebar_collapsed = true;
        assert_eq!(reserved_rows(&app, content(&app)), 0);
    }

    /// A panel too narrow to give each slot even a mark's width draws no tray
    /// rather than four badges and a truncation.
    #[test]
    fn a_panel_too_narrow_for_four_slots_draws_no_tray() {
        let app = app_with_tray(MIN_SLOT_COLS * 4 + 1, 60);
        assert!(reserved_rows(&app, content(&app)) > 0);
        let app = app_with_tray(MIN_SLOT_COLS * 4, 60);
        assert_eq!(reserved_rows(&app, content(&app)), 0);
    }

    /// Every slot has to be inside the grid, non-empty, and disjoint from every
    /// other — the layout and the hit test share this function, so an overlap
    /// here is a badge that opens its neighbour's popup.
    #[test]
    fn the_eight_slots_tile_the_grid_without_overlapping() {
        for width in [13u16, 21, 42, 61, 78] {
            let app = app_with_tray(width, 60);
            let grid = grid_rect(tray_rect(&app, content(&app)));
            assert!(grid.width > 0, "no grid at width {width}");

            let mut covered: Vec<(u16, u16)> = Vec::new();
            for index in 0..FleetSignal::COUNT {
                let slot = slot_rect(grid, index);
                assert!(slot.width > 0 && slot.height > 0, "empty slot {index}");
                assert!(slot.x >= grid.x && slot.x + slot.width <= grid.x + grid.width);
                assert!(slot.y >= grid.y && slot.y + slot.height <= grid.y + grid.height);
                for y in slot.y..slot.y + slot.height {
                    for x in slot.x..slot.x + slot.width {
                        assert!(
                            !covered.contains(&(x, y)),
                            "slot {index} overlaps at {x},{y}"
                        );
                        covered.push((x, y));
                    }
                }
            }
        }
    }

    /// The hit test must answer with the badge whose slot was drawn there, for
    /// every cell of every slot.
    #[test]
    fn every_cell_of_a_slot_hits_its_own_badge() {
        let app = app_with_tray(42, 60);
        let grid = grid_rect(tray_rect(&app, content(&app)));
        for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
            let slot = slot_rect(grid, index);
            for y in slot.y..slot.y + slot.height {
                for x in slot.x..slot.x + slot.width {
                    assert_eq!(badge_at(&app, x, y), Some(signal), "at {x},{y}");
                }
            }
        }
    }

    /// The tray sits above the panel's footer row, never on it: that row
    /// belongs to the `new` button and the collapse toggle.
    #[test]
    fn the_tray_leaves_the_panel_footer_alone() {
        let app = app_with_tray(42, 60);
        let area = content(&app);
        let tray = tray_rect(&app, area);
        assert_eq!(
            tray.y + tray.height,
            area.y + area.height - super::super::WORKSPACE_SECTION_FOOTER_ROWS
        );
    }

    /// The tray's whole width is inside the panel's content column, so nothing
    /// in it can reach the divider bar's grab band.
    #[test]
    fn the_tray_never_reaches_the_divider() {
        let app = app_with_tray(42, 60);
        let tray = tray_rect(&app, content(&app));
        assert!(tray.x + tray.width < app.view.sidebar_rect.x + app.view.sidebar_rect.width);
        assert!(!menu_at(&app, tray.x + tray.width, tray.y));
    }

    #[test]
    fn the_legend_button_sits_at_the_right_end_of_the_header() {
        let app = app_with_tray(42, 60);
        let tray = tray_rect(&app, content(&app));
        let menu = menu_rect(tray);
        assert_eq!(menu.y, tray.y);
        assert_eq!(menu.x + menu.width, tray.x + tray.width);
        assert!(menu_at(&app, menu.x, menu.y));
        // And it is not a badge: the two hit tests must not both answer.
        assert_eq!(badge_at(&app, menu.x, menu.y), None);
    }

    #[test]
    fn the_image_covers_exactly_the_grid_it_was_measured_from() {
        let app = app_with_tray(42, 60);
        let (grid, image) = image(&app, 9, 18).expect("an enabled tray has an image");
        assert_eq!(image.width, u32::from(grid.width) * 9);
        assert_eq!(image.height, u32::from(grid.height) * 18);
        assert_eq!(
            image.pixels.len(),
            (image.width as usize) * (image.height as usize) * 4
        );
        assert!(
            image.pixels.chunks(4).any(|px| px[3] > 0),
            "the tray image is entirely transparent"
        );
    }

    #[test]
    fn a_tray_that_is_off_has_no_image() {
        let mut app = app_with_tray(42, 60);
        app.sidebar_signal_tray.enabled = false;
        assert!(image(&app, 9, 18).is_none());
    }

    /// The fallback marks and the artwork are exclusive.
    ///
    /// A badge is mostly transparent by design, so a mark drawn under one shows
    /// *through* it rather than being covered — a `●` sitting inside a speech
    /// bubble. This is what the capture of the real render caught.
    #[test]
    fn the_fallback_marks_give_way_to_the_artwork() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app_with_tray(42, 34);
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();

        let marks_drawn = |app: &AppState| {
            let area = app.view.sidebar_rect;
            let mut terminal =
                Terminal::new(TestBackend::new(area.width, area.height)).expect("backend");
            terminal
                .draw(|frame| render(app, frame, crate::ui::sidebar::sidebar_content_rect(area)))
                .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            FleetSignal::ALL
                .into_iter()
                .filter(|signal| {
                    (0..area.height)
                        .any(|y| (0..area.width).any(|x| buffer[(x, y)].symbol() == signal.mark()))
                })
                .count()
        };

        assert_eq!(
            marks_drawn(&app),
            FleetSignal::COUNT,
            "a host with no graphics must still get all eight marks"
        );

        app.signal_tray_graphics = Some(crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            1,
            1,
            vec![0, 0, 0, 0],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        ));
        assert_eq!(
            marks_drawn(&app),
            0,
            "the fallback marks were drawn under artwork that cannot cover them"
        );
    }
}
