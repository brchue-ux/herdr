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

/// How much of its slot a badge is actually drawn at.
///
/// A fifth off the size the slot would allow. The slot geometry is unchanged —
/// the tray still reserves the same cells, so the hit tests and the tier search
/// are untouched — and the badge is simply drawn smaller inside it and stays
/// centred by the offsets below. The badge art is described over a normalised
/// box (`tray_art`'s `Pen`), so this shrinks the mark, its border and its
/// halo together rather than cropping any of them.
const BADGE_SCALE: f32 = 0.80;

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
/// The eight palette roles, and since the header row stopped drawing its own
/// copy of the signals this table is their only home. `Idle` is the panel's
/// muted grey — the same grey the fleet pulse row rests in.
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
    // A hue that resolves to the resting grey on some themes would leave a live
    // badge looking dead, so it falls back to the bright neutral.
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
    let p = &app.sidebar_palette;
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

    if artwork_covers_grid(app, tray) {
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

/// Whether badges are going to be drawn over the grid, so [`render`] must
/// leave the fallback marks off it.
///
/// Two ways that happens and they are equally binding: this Herdr rasterised
/// the artwork into [`AppState::signal_tray_graphics`], or it handed a
/// [`TrayScene`] to a client that rasterises it. The marks are the *no
/// graphics* form, and a mark showing through a mostly-transparent badge is
/// worse than either alone — so what matters is that badges are coming, not
/// which machine drew them.
///
/// Third, and it is a *not*: badges that exist are still not coming to a pass
/// whose overlay covers the tray, because an image is never placed under one
/// (`crate::ui::OverlayOcclusion`). The popover a badge click opens is anchored
/// above the tray precisely so the badges stay readable behind it, and leaving
/// the marks off for artwork that pass is not going to place is what emptied
/// the tray on that click instead.
fn artwork_covers_grid(app: &AppState, tray: Rect) -> bool {
    if crate::ui::overlay_occlusion(app).hides(tray) {
        return false;
    }
    app.signal_tray_graphics.is_some() || app.signal_tray_graphics_client_rasterized
}

/// One badge, as the wire carries it: what it is saying and where it is in
/// saying it. Ordered by [`FleetSignal::ALL`] inside a [`TrayScene`], which is
/// the same order [`slot_rect`] lays the grid out in.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrayBadge {
    /// What the badge is drawn as.
    state: BadgeState,
    /// Where it is in its animation, as the engine's envelope — the value
    /// [`motion`] resolves, already read off the server's clock.
    motion: f32,
}

/// Everything a client needs to rasterise the tray's badge artwork itself, in
/// place of server-embedded tray pixels. Sent as the opaque payload of
/// `ServerMessage::TrayScene` to clients that set
/// `ClientMessage::Hello::wants_client_rasterized_signal_tray`.
///
/// The field list is [`crate::app::runtime`]'s own `signal_tray_graphics_key`
/// read forwards: that hash is exactly what the artwork depends on, so what it
/// folds in is what has to cross the wire. The palette and host theme it hashes
/// arrive here already resolved into [`BadgePaint`], because two colours are
/// what the rasteriser actually reads.
///
/// # What is deliberately not here
///
/// Cell size. It is in the cache key server-side because the server rasterises
/// against the *client's* reported cell, but the client already knows its own
/// and is the authority on it — the same reason [`super::image_card::CardScene`]
/// ships no cell or font metrics.
///
/// # Not a scope cut
///
/// Unlike `CardScene`, nothing is dropped on the way: a badge's whole motion is
/// one `f32` envelope ([`motion`]'s own doc says why), so the client
/// reconstructs the identical artwork rather than a reduced one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrayScene {
    /// The cell rect the badges are laid out in — [`grid_rect`]'s answer, which
    /// is also where the placement lands.
    grid: Rect,
    /// The eight badges, in [`FleetSignal::ALL`] order. A fixed array rather
    /// than a `Vec` so a decoded scene cannot carry seven badges or nine.
    badges: [TrayBadge; FleetSignal::COUNT],
    /// The two colours a badge is cut from.
    paint: BadgePaint,
}

/// Snapshots what the tray would be rasterised from right now.
///
/// A **pure read**, like [`render`] — every value it takes is one the app loop
/// has already settled — so building a scene for a client cannot make the
/// artwork disagree with the frame it travels with.
///
/// `None` when there is nothing to draw, matching [`image`]'s own `None`.
pub(crate) fn build_scene(app: &AppState) -> Option<TrayScene> {
    let area = super::sidebar_content_rect(app.view.sidebar_rect);
    let grid = grid_rect(tray_rect(app, area));
    if grid.width == 0 || grid.height == 0 {
        return None;
    }
    let reading = signal_tray::resolve(app);
    Some(TrayScene {
        grid,
        badges: FleetSignal::ALL.map(|signal| {
            let state = reading.badge(signal).state;
            TrayBadge {
                state,
                motion: motion(app, signal, state),
            }
        }),
        paint: BadgePaint {
            attention: rgb_of(app.palette.peach, app),
            surface: badge_surface(app),
        },
    })
}

/// Encodes a [`TrayScene`] as the opaque bincode payload carried by
/// `ServerMessage::TrayScene { bytes }`.
pub(crate) fn encode_scene(scene: &TrayScene) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(scene, bincode::config::standard())
}

/// Decodes a [`TrayScene`] from the opaque bincode payload carried by
/// `ServerMessage::TrayScene { bytes }`.
pub(crate) fn decode_scene(bytes: &[u8]) -> Result<TrayScene, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(bytes, bincode::config::standard()).map(|(scene, _)| scene)
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
    rasterise_scene(&build_scene(app)?, cell_width, cell_height)
}

/// Draws a [`TrayScene`] — this pass's own, server-side, or one that arrived
/// over the wire — at this machine's cell size.
///
/// The one rasteriser. A client rasterising a scene it was sent runs exactly
/// the code the server would have run, against its own cell, so the two cannot
/// draw the tray differently.
pub(crate) fn rasterise_scene(
    scene: &TrayScene,
    cell_width: u32,
    cell_height: u32,
) -> Option<(Rect, Rgba)> {
    let grid = scene.grid;
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

    for (index, signal) in FleetSignal::ALL.into_iter().enumerate() {
        let slot = slot_rect(grid, index);
        if slot.width == 0 || slot.height == 0 {
            continue;
        }
        let slot_w = u32::from(slot.width) * cell_width;
        let slot_h = u32::from(slot.height) * cell_height;
        // Square, and inset so two badges never touch: the gap is what lets the
        // eye count eight things rather than read one strip. `BADGE_SCALE` then
        // takes the badge off the slot it is centred in — see its own note.
        let fitted = slot_w.min(slot_h).saturating_sub(2);
        let size = ((fitted as f32) * BADGE_SCALE).round() as u32;
        if size == 0 {
            continue;
        }
        let badge = &scene.badges[index];
        let art = tray_art::render_badge(signal, badge.state, size, scene.paint, badge.motion);
        let ox = u32::from(slot.x - grid.x) * cell_width + (slot_w.saturating_sub(size)) / 2;
        let oy = u32::from(slot.y - grid.y) * cell_height + (slot_h.saturating_sub(size)) / 2;
        blit(&mut canvas, &art, ox, oy);
    }

    Some((grid, canvas))
}

/// The artwork as the graphics layer that publishes it.
///
/// One construction for both sides of the boundary — the server embedding its
/// own raster and a client compositing the one it drew from a `TrayScene` — so
/// format, placement and z cannot drift apart between them.
///
/// The format is [`crate::kitty_graphics::preferred_sidebar_pixel_format`]'s,
/// the same decision the tree's cards go through, rather than the unconditional
/// raw RGBA this used to hardcode. The badges are a handful of small marks on a
/// transparent ground, so PNG carries them at a tenth of the bytes; four bytes
/// a pixel plus base64 is only ever right when the pixels never touch the
/// escape stream. [`rasterise_scene`] opens on a fully transparent canvas and
/// the badges never tile it, so the artwork is never opaque and the `f=24` case
/// can be answered without scanning for it.
///
/// `None` when the pixels cannot be encoded, which is exactly the card path's
/// own answer to the same failure: no layer, and the character marks the
/// badges are drawn over stand on their own.
pub(crate) fn graphics_layer(
    image: Rgba,
    host_terminal_kind: crate::kitty_graphics::HostTerminalKind,
    host_graphics_is_local: bool,
) -> Option<crate::app::state::GraphicsLayer> {
    let format = crate::kitty_graphics::preferred_sidebar_pixel_format(
        false,
        host_terminal_kind,
        host_graphics_is_local,
    );
    let data = crate::kitty_graphics::encode_layer_pixels(
        format,
        image.width,
        image.height,
        &image.pixels,
    )?;
    Some(crate::app::state::GraphicsLayer::new(
        format,
        image.width,
        image.height,
        data,
        crate::api::schema::PaneGraphicsPlacementParams {
            viewport_col: 0,
            viewport_row: 0,
            grid_cols: 0,
            grid_rows: 0,
            // Over the text. The badges *are* the tray; the fallback marks
            // underneath them are what a host with no graphics gets, and on
            // a host with graphics they are meant to be covered.
            z: 0,
        },
    ))
}

/// Where one badge is in its animation right now, as the engine's envelope.
///
/// A **pure read** of the engine, exactly as `render` is a pure read of state:
/// [`crate::anim::Animator::frame`] takes `&self` and consults no clock, so
/// asking here cannot make the artwork disagree with anything else drawn this
/// pass. All the clock work happened in `Animator::advance`, on the app loop.
///
/// The state picks which of the element's three declared idle behaviours is
/// read, and the behaviour supplies the whole motion character. Nothing about
/// the curve — the ramp, the overshoot, the swing back — is expressed here or
/// anywhere else in the tray: this asks the catalogue where the badge is and
/// [`super::tray_art`] says what that means in pixels. That split is the point.
/// There is no second animation path for badges.
///
/// `0.0` whenever the engine has nothing for this badge, which is the settled
/// artwork — a host with no graphics, animation switched off, or a tray that
/// has only just been turned on and has not been published yet.
fn motion(app: &AppState, signal: FleetSignal, state: BadgeState) -> f32 {
    let id = signal.badge_element_id();
    let Some(frame) = app.anim.frame(&id, Some(state.behaviour())) else {
        return 0.0;
    };
    let Some(behaviour) = frame.behaviour else {
        return 0.0;
    };
    // A badge is one object, so its behaviours are uniform and every cell of
    // the notional extent resolves the same. Asking for the first cell of a
    // 1×1 extent is asking for the envelope itself.
    behaviour.strength(
        crate::anim::cell::CellPos::new(0, 0),
        crate::anim::cell::CellExtent::new(1, 1),
        frame.progress,
    )
}

/// Steps of the envelope the artwork can actually tell apart.
///
/// The badge image is re-rasterised when this number moves, so it is the
/// tray's frame rate expressed as a resolution rather than as a clock. 128
/// steps over a travel of three pixels is far finer than any pixel could show,
/// which is what makes the engine's own frame tier — not this — the thing that
/// decides how often the tray redraws.
const MOTION_STEPS: f32 = 128.0;

/// Every badge's envelope, quantised and folded into one number.
///
/// The tray's artwork is redrawn when its cache key moves, so the key has to
/// carry the animation or a moving badge would rasterise once and then hold
/// still forever. Quantised for the same reason the engine quantises its own
/// positions: a difference no pixel could show is not worth a raster.
pub(crate) fn motion_fingerprint(app: &AppState) -> u64 {
    let reading = signal_tray::resolve(app);
    let mut folded: u64 = 0;
    for signal in FleetSignal::ALL {
        let state = reading.badge(signal).state;
        let step = (motion(app, signal, state) * MOTION_STEPS).round().max(0.0) as u64;
        folded = folded.wrapping_mul(0x0100_0193) ^ step;
    }
    folded
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

/// What an engraved badge is cut into.
///
/// The panel's own fill when a theme paints one, and otherwise — Herdr paints no
/// global background fill — whatever the host terminal reported. A host that
/// never answered falls back to the canvas the marks were designed against
/// rather than to black, which would over-darken every carve.
fn badge_surface(app: &AppState) -> [f32; 3] {
    super::panel_fill_rgb(&app.palette, &app.host_terminal_theme)
        .or_else(|| {
            app.host_terminal_theme
                .background
                .map(crate::ui::color::terminal_theme_to_rgb)
        })
        .map_or(DEFAULT_CANVAS, normalise)
}

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

    /// A badge is carved into the panel it sits in, not into the host's own
    /// colour: with a theme fill, the two are different colours and the carve
    /// belongs to the one on screen around it.
    #[test]
    fn a_badge_is_engraved_into_whatever_the_panel_is_filled_with() {
        let mut app = app_with_tray(42, 30);

        // No fill, no measured host background: the canvas the marks were
        // designed against, exactly as before.
        assert_eq!(badge_surface(&app), DEFAULT_CANVAS);

        // A host that answered is what an unfilled panel is showing.
        app.host_terminal_theme = app.host_terminal_theme.with_color(
            crate::terminal_theme::DefaultColorKind::Background,
            crate::terminal_theme::RgbColor {
                r: 0,
                g: 51,
                b: 102,
            },
        );
        assert_eq!(badge_surface(&app), normalise((0, 51, 102)));

        // And a theme that paints the panel wins over both.
        app.palette.sidebar_bg = ratatui::style::Color::Rgb(12, 34, 56);
        app.refresh_sidebar_palette();
        assert_eq!(badge_surface(&app), normalise((12, 34, 56)));
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

    /// A tray with one signal standing, and its badges published to the engine
    /// and advanced by `elapsed`.
    ///
    /// Drives the real membership path rather than reaching into the animator,
    /// for the same reason the fleet pulse's own test does: the app loop is what
    /// publishes, and a test that mounted elements by hand would pass over a
    /// tray that never published any.
    fn animated_tray(elapsed: std::time::Duration) -> AppState {
        let mut app = app_with_tray(42, 40);
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        // `push` goes live on a branch with commits that have not left.
        app.workspaces[0].cached_git_ahead_behind = Some((2, 0));

        let now = std::time::Instant::now();
        let lifecycle = BadgeState::lifecycle();
        let live: Vec<_> = signal_tray::resolve(&app).animation_membership().collect();
        app.anim
            .observe(now, crate::anim::Family::TrayBadge, &lifecycle, live);
        app.anim.advance(now + elapsed);
        app
    }

    /// The engine is what moves a badge, and the movement reaches the pixels.
    ///
    /// The whole point of the acceptance criterion "no bespoke badge effects
    /// path": nothing in the tray holds a clock, so the *only* way this image
    /// can differ between two calls is the animator having advanced.
    #[test]
    fn the_artwork_moves_because_the_engine_moved() {
        use std::time::Duration;

        let settled = animated_tray(Duration::ZERO);
        assert_eq!(
            signal_tray::resolve(&settled)
                .badge(FleetSignal::Push)
                .state,
            BadgeState::Active,
            "the fixture did not light the badge this test is about"
        );

        // A quarter of the way up the ramp, and again at the snap.
        let early = animated_tray(Duration::from_millis(200));
        let snapped = animated_tray(Duration::from_millis(800));

        let pixels = |app: &AppState| image(app, 9, 18).expect("an enabled tray has an image").1;
        let (a, b, c) = (pixels(&settled), pixels(&early), pixels(&snapped));
        assert_ne!(a, c, "the artwork did not move between rest and the snap");
        assert_ne!(b, c, "the artwork did not move through its ramp");

        // And the fingerprint moved with it, or the app loop would rasterise
        // once and then believe there was nothing to redraw.
        assert_ne!(
            motion_fingerprint(&settled),
            motion_fingerprint(&snapped),
            "the graphics cache key did not follow the animation"
        );
    }

    /// Every badge is published, not only the ones that are lit.
    ///
    /// The bar publishes live signals; the tray publishes all eight, because
    /// rest is one of the three things a badge says. A tray that only published
    /// its lit badges would have seven of eight frozen.
    #[test]
    fn all_eight_badges_are_published_including_the_resting_ones() {
        let app = animated_tray(std::time::Duration::from_millis(100));
        for signal in FleetSignal::ALL {
            let state = signal_tray::resolve(&app).badge(signal).state;
            assert!(
                app.anim
                    .frame(&signal.badge_element_id(), Some(state.behaviour()))
                    .is_some_and(|frame| frame.behaviour.is_some()),
                "{signal:?} in {state:?} has no element to move"
            );
        }
    }

    /// A badge with no element resolves to the settled artwork rather than to
    /// nothing — the animation is an addition, and it fails off.
    #[test]
    fn a_badge_the_engine_has_never_seen_is_simply_still() {
        let app = app_with_tray(42, 40);
        assert!(app.anim.is_empty());
        for signal in FleetSignal::ALL {
            assert_eq!(motion(&app, signal, BadgeState::Idle), 0.0);
        }
        assert!(image(&app, 9, 18).is_some(), "a still tray still draws");
    }

    /// The gates. Motion is artwork, so it is off wherever the artwork is.
    #[test]
    fn the_badges_only_animate_where_the_artwork_can_be_drawn() {
        let mut app = app_with_tray(42, 40);
        app.kitty_graphics_enabled = true;
        app.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        assert!(app.signal_tray_animation_active());

        app.sidebar_signal_tray.animate = false;
        assert!(!app.signal_tray_animation_active());
        app.sidebar_signal_tray.animate = true;

        app.kitty_graphics_enabled = false;
        assert!(
            !app.signal_tray_animation_active(),
            "a host drawing the fallback marks cannot animate them"
        );
        app.kitty_graphics_enabled = true;

        app.sidebar_collapsed = true;
        assert!(!app.signal_tray_animation_active());
        app.sidebar_collapsed = false;

        app.sidebar_signal_tray.enabled = false;
        assert!(!app.signal_tray_animation_active());
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

        // And artwork drawn by the client covers them exactly as artwork drawn
        // here does. Which machine rasterised the badges is not a fact the
        // fallback marks are entitled to have an opinion about.
        app.signal_tray_graphics = None;
        app.signal_tray_graphics_client_rasterized = true;
        assert_eq!(
            marks_drawn(&app),
            0,
            "the fallback marks were drawn under badges the client is about to composite"
        );
    }

    /// The marks come back for an overlay that takes the badges off the
    /// terminal, and stay off for one that does not reach them.
    ///
    /// A Kitty image composites above the cell text, so no image is placed under
    /// an open overlay ([`crate::ui::OverlayOcclusion`]) — and the marks stood
    /// down for artwork that pass was no longer going to place, which is why
    /// clicking a badge emptied the tray it opened. The popover is anchored
    /// *above* the tray on purpose, so it is also the case that must not flip
    /// the marks back on.
    #[test]
    fn the_marks_come_back_for_an_overlay_that_takes_the_badges_off_the_terminal() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app_with_tray(42, 34);
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.mode = crate::app::Mode::Terminal;
        app.view.terminal_area = Rect::new(42, 0, 58, 34);
        app.signal_tray_graphics = Some(crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            1,
            1,
            vec![0, 0, 0, 0],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        ));

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
            0,
            "the badges are coming, so this starts from the wrong state"
        );

        let tray = tray_rect(&app, content(&app));
        assert!(tray.height > 0, "the tray is off, so this tests nothing");

        // An overlay drawn over the tray: the badges are not placed, so the
        // marks are the only thing that can say what the eight slots hold.
        app.mode = crate::app::Mode::ContextMenu;
        app.context_menu = Some(crate::app::state::ContextMenuState {
            kind: crate::app::state::ContextMenuKind::Workspace { ws_idx: 0 },
            x: 1,
            y: tray.y,
            list: crate::app::state::MenuListState::new(0),
        });
        assert_eq!(
            marks_drawn(&app),
            FleetSignal::COUNT,
            "the tray drew neither its badges nor its marks"
        );

        // One that misses it entirely leaves the badges alone.
        app.context_menu = Some(crate::app::state::ContextMenuState {
            kind: crate::app::state::ContextMenuKind::Workspace { ws_idx: 0 },
            x: 60,
            y: 1,
            list: crate::app::state::MenuListState::new(0),
        });
        assert_eq!(
            marks_drawn(&app),
            0,
            "a menu nowhere near the tray put marks under badges that are still coming"
        );
    }

    /// A fleet with something to say on every row, so the scene under test is
    /// not eight idle badges.
    fn scene_app() -> AppState {
        let mut app = app_with_tray(42, 34);
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.workspaces[0].cached_git_ahead_behind = Some((3, 1));
        app
    }

    /// A `TrayScene` built from a real fleet survives an encode/decode round
    /// trip byte-for-byte — the contract `ServerMessage::TrayScene` rests on,
    /// checked without a live terminal or a connected client on either end.
    #[test]
    fn tray_scene_round_trips_through_bincode() {
        let app = scene_app();
        let scene = build_scene(&app).expect("the tray has a grid to draw in");

        let bytes = encode_scene(&scene).expect("encode TrayScene");
        let decoded = decode_scene(&bytes).expect("decode TrayScene");

        assert_eq!(scene, decoded);
    }

    /// Garbage bytes decode to an error rather than a panic — the client's
    /// only defence against a version-skewed or corrupted `TrayScene` payload.
    #[test]
    fn tray_scene_decode_rejects_garbage_bytes() {
        assert!(decode_scene(&[0xff, 0x00, 0x13, 0x37]).is_err());
    }

    /// What crosses the wire is the *reading*, not the picture: a scene is a
    /// few dozen bytes where the artwork it stands for is tens of kilobytes of
    /// RGBA. This is the whole point of the message, so it is asserted rather
    /// than assumed.
    #[test]
    fn a_scene_is_orders_of_magnitude_smaller_than_the_artwork_it_stands_for() {
        let app = scene_app();
        let scene = build_scene(&app).expect("the tray has a grid to draw in");
        let bytes = encode_scene(&scene).expect("encode TrayScene");
        let (_, image) = image(&app, 9, 18).expect("the tray rasterises");

        assert!(
            bytes.len() < 1024,
            "a tray scene should be well under a kilobyte, was {}",
            bytes.len()
        );
        assert!(
            bytes.len() * 100 < image.pixels.len(),
            "a {}-byte scene against {} bytes of pixels is not the saving this exists for",
            bytes.len(),
            image.pixels.len()
        );
    }

    /// The client draws what the server would have drawn — every pixel, not an
    /// approximation of it. Unlike `CardScene` nothing is dropped on the way
    /// over, so this is an equality and not a resemblance.
    #[test]
    fn a_scene_rasterised_from_the_wire_is_the_artwork_the_server_would_have_drawn() {
        let app = scene_app();
        let here = image(&app, 9, 18).expect("the tray rasterises");

        let bytes = encode_scene(&build_scene(&app).expect("scene")).expect("encode");
        let there = rasterise_scene(&decode_scene(&bytes).expect("decode"), 9, 18)
            .expect("the scene rasterises");

        assert_eq!(here.0, there.0, "the badges landed on a different grid");
        assert_eq!(here.1.width, there.1.width);
        assert_eq!(here.1.height, there.1.height);
        assert_eq!(
            here.1.pixels, there.1.pixels,
            "the client drew different badges from the ones the server would have"
        );
    }

    /// A badge mid-animation reaches the wire where it actually is. Shipping
    /// the states without the envelope would look right in a screenshot and be
    /// frozen in motion — the same failure the artwork's cache key exists to
    /// prevent, arrived at from the other side.
    #[test]
    fn a_scene_carries_where_each_badge_is_in_its_animation() {
        let mut app = scene_app();
        app.sidebar_signal_tray.animate = true;
        let now = std::time::Instant::now();
        app.anim.advance(now);

        let resting = build_scene(&app).expect("scene");
        assert!(
            resting.badges.iter().all(|badge| badge.motion == 0.0),
            "a tray with no animation observed yet should be settled"
        );

        // Whatever the engine says about a badge is what the wire says about
        // it; the scene never resolves motion of its own.
        let mut moved = resting.clone();
        moved.badges[0].motion = 0.5;
        assert_ne!(resting, moved);
        let bytes = encode_scene(&moved).expect("encode");
        assert_eq!(
            decode_scene(&bytes).expect("decode").badges[0].motion,
            0.5,
            "the badge's position in its animation did not survive the wire"
        );
    }

    /// The tray takes the format the transport policy names, and that format
    /// is not raw pixels on a link that carries them.
    ///
    /// The tray used to hardcode `PaneGraphicsFormat::Rgba` — four bytes a
    /// pixel plus a third again for base64, 224 KiB per upload at a 42-column
    /// panel, on a link that for the captain's own fleet is an SSH hop. The
    /// tree's cards have always routed through
    /// `preferred_sidebar_pixel_format`; this is the tray joining them, stated
    /// against the policy itself so the two cannot drift apart.
    #[test]
    fn the_tray_takes_the_format_the_transport_policy_names() {
        let app = animated_tray(std::time::Duration::from_millis(200));
        for (kind, is_local) in [
            (crate::kitty_graphics::HostTerminalKind::Rio, false),
            (crate::kitty_graphics::HostTerminalKind::Rio, true),
            (crate::kitty_graphics::HostTerminalKind::Kitty, true),
            (crate::kitty_graphics::HostTerminalKind::Other, true),
        ] {
            let (_, artwork) = image(&app, 8, 16).expect("an enabled tray has an image");
            let layer =
                graphics_layer(artwork, kind, is_local).expect("the tray encodes its artwork");
            assert_eq!(
                layer.format,
                crate::kitty_graphics::preferred_sidebar_pixel_format(false, kind, is_local),
                "the tray chose its own format for {kind:?} (local={is_local})"
            );
        }
    }

    /// And the format that policy names by default is worth taking: the badges
    /// are a handful of small marks on a transparent ground.
    #[test]
    fn the_badges_carry_an_order_of_magnitude_smaller_as_png() {
        let app = animated_tray(std::time::Duration::from_millis(200));
        let (_, artwork) = image(&app, 8, 16).expect("an enabled tray has an image");
        let raw = artwork.pixels.len();
        let png = crate::kitty_graphics::encode_layer_pixels(
            crate::api::schema::PaneGraphicsFormat::Png,
            artwork.width,
            artwork.height,
            &artwork.pixels,
        )
        .expect("png encode");
        assert!(
            png.len() * 4 < raw,
            "PNG carried the badges in {} bytes against {raw} raw",
            png.len()
        );
    }

    /// A tray whose badges are published to the engine on `now`, so a caller
    /// stepping the animator from that same instant is stepping the clock the
    /// elements were mounted against.
    ///
    /// Deliberately not [`animated_tray`], which takes its own `Instant::now()`
    /// internally: two clocks means the first advances land before the
    /// animator's own origin and are clamped away, and the badge simply does
    /// not move for the first stretch of the measurement.
    fn tray_published_at(now: std::time::Instant, lit: bool) -> AppState {
        let mut app = app_with_tray(42, 40);
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        if lit {
            // `push` goes live on a branch with commits that have not left.
            app.workspaces[0].cached_git_ahead_behind = Some((2, 0));
        }
        let live: Vec<_> = signal_tray::resolve(&app).animation_membership().collect();
        app.anim.observe(
            now,
            crate::anim::Family::TrayBadge,
            &BadgeState::lifecycle(),
            live,
        );
        app
    }

    /// How many of a span of badge frames are worth handing the terminal.
    fn published_over(app: &mut AppState, start: std::time::Instant, frames: u32) -> (u32, u32) {
        let mut published = crate::app::state::PublishedSurfaceRaster::default();
        let (mut drawn, mut sent) = (0, 0);
        for step in 0..frames {
            app.anim
                .advance(start + std::time::Duration::from_millis(50) * step);
            let Some((_, artwork)) = image(app, 8, 16) else {
                continue;
            };
            drawn += 1;
            if published.accept(artwork.width, artwork.height, &artwork.pixels) {
                sent += 1;
            }
        }
        (drawn, sent)
    }

    /// Eight resting badges cost the terminal almost nothing, and one lit badge
    /// costs it almost everything.
    ///
    /// This is the whole claim of [`crate::app::state::PublishedSurfaceRaster`]
    /// stated as a test rather than as a comment, and it is one test rather
    /// than two because the two numbers only mean something against each
    /// other. A rule that merely slowed the tray down would move both.
    ///
    /// Rest's motion is a carve breathing by a fraction of one 8-bit level per
    /// frame; a lit badge's mark travels and brightens. The first is not worth
    /// a 328x128 upload twenty times a second and the second is worth every
    /// frame it asks for, and the same rule says so.
    #[test]
    fn a_resting_tray_stops_re_uploading_and_a_lit_one_does_not() {
        const FRAMES: u32 = 200;
        let now = std::time::Instant::now();

        let mut resting = tray_published_at(now, false);
        assert!(
            FleetSignal::ALL
                .into_iter()
                .all(
                    |signal| signal_tray::resolve(&resting).badge(signal).state == BadgeState::Idle
                ),
            "the resting fixture lit a badge"
        );
        let (rest_drawn, rest_sent) = published_over(&mut resting, now, FRAMES);

        let mut lit = tray_published_at(now, true);
        assert_eq!(
            signal_tray::resolve(&lit).badge(FleetSignal::Push).state,
            BadgeState::Active,
            "the lit fixture lit no badge"
        );
        let (lit_drawn, lit_sent) = published_over(&mut lit, now, FRAMES);

        assert!(
            rest_sent * 3 < rest_drawn,
            "a resting tray published {rest_sent} of {rest_drawn} rasters; \
             nothing was saved on the fleet's common case"
        );
        assert!(
            lit_sent * 10 > lit_drawn * 8,
            "a lit badge published only {lit_sent} of {lit_drawn} rasters; \
             this is a throttle, not a bound on what the screen may drift"
        );
    }
}
