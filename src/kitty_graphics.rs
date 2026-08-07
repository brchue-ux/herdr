use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use ratatui::layout::Rect;

use crate::app::state::AppState;
use crate::app::Mode;
use crate::ghostty::{
    KittyImageDescriptor, KittyImageFormat, KittyImagePlacement, KittyPlacementRenderInfo,
};
use crate::layout::PaneId;
use crate::terminal::TerminalRuntimeRegistry;

const KITTY_CHUNK_BYTES: usize = 3072;
const HOST_IMAGE_ID_BASE: u32 = 10_000;
const PANE_GRAPHICS_IMAGE_ID_BIT: u32 = 1 << 31;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HostCellSize {
    pub width_px: u32,
    pub height_px: u32,
}

impl HostCellSize {
    /// The cell Herdr assumes when nothing on the machine will tell it one.
    ///
    /// Deliberately a whole cell rather than a pair of independent numbers: the
    /// card is laid out in this space, so the *ratio* between the two is what
    /// decides whether it comes out square or squashed, and a mismatched pair
    /// distorts every card drawn against it.
    pub(crate) const FALLBACK: Self = Self {
        width_px: 8,
        height_px: 16,
    };

    /// The narrowest and widest a real terminal cell gets.
    ///
    /// Not taste: a cell narrower than this is a font no one can read, and one
    /// wider is a display no one has. The bounds are here to catch a *reported*
    /// cell that is arithmetic rather than a measurement — the constant
    /// `ws_xpixel` case above lands at two or three pixels wide on a wide
    /// window, which is inside no font's range and outside these bounds.
    const MIN_WIDTH_PX: u32 = 5;
    const MAX_WIDTH_PX: u32 = 128;
    const MIN_HEIGHT_PX: u32 = 10;
    const MAX_HEIGHT_PX: u32 = 256;
    /// A cell is taller than it is wide, always, and by a bounded amount.
    const MIN_ASPECT: f32 = 1.05;
    const MAX_ASPECT: f32 = 4.0;

    /// Whether this looks like a cell a terminal actually draws in.
    ///
    /// Checked wherever a cell size arrives from outside Herdr, because a
    /// wrong-but-nonzero cell is worse than no cell at all: `is_known` says yes
    /// to it, and everything downstream then rasterises into a pixel space that
    /// does not exist.
    pub(crate) fn is_plausible(self) -> bool {
        if !(Self::MIN_WIDTH_PX..=Self::MAX_WIDTH_PX).contains(&self.width_px)
            || !(Self::MIN_HEIGHT_PX..=Self::MAX_HEIGHT_PX).contains(&self.height_px)
        {
            return false;
        }
        let aspect = self.height_px as f32 / self.width_px as f32;
        (Self::MIN_ASPECT..=Self::MAX_ASPECT).contains(&aspect)
    }

    /// This cell if it is believable, and the fallback if it is not.
    pub(crate) fn or_fallback(self) -> Self {
        if self.is_plausible() {
            self
        } else {
            Self::FALLBACK
        }
    }

    /// The gate every externally reported cell passes on the way in.
    ///
    /// Distinct from [`Self::or_fallback`] in what it does with *nothing*: an
    /// unknown cell stays unknown, because a client whose own config has Kitty
    /// graphics off reports `0x0` and the server reads that absence as "send no
    /// graphics". Only a cell that claims to be a measurement and is not one is
    /// replaced.
    pub(crate) fn plausible_or_unknown(self) -> Self {
        if self.is_known() {
            self.or_fallback()
        } else {
            self
        }
    }

    pub(crate) fn try_from_terminal(area: Rect) -> Option<Self> {
        let Ok(size) = crossterm::terminal::window_size() else {
            return None;
        };
        if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
            return None;
        }
        let derived = Self {
            width_px: (size.width as u32 / size.columns as u32).max(1),
            height_px: (size.height as u32 / size.rows as u32).max(1),
        };
        // A derived cell that is not a believable cell is discarded rather than
        // clamped. Clamping would keep the half of the pair that happened to
        // land in range and silently change the aspect; the caller's fallback
        // is at least a coherent cell.
        if !derived.is_plausible() {
            tracing::debug!(
                width_px = derived.width_px,
                height_px = derived.height_px,
                pixel_width = size.width,
                pixel_height = size.height,
                columns = size.columns,
                rows = size.rows,
                "terminal pixel size gives an implausible cell; ignoring it"
            );
            return None;
        }
        Some(derived.for_area(area))
    }

    pub(crate) fn is_known(self) -> bool {
        self.width_px > 0 && self.height_px > 0
    }

    pub(crate) fn fallback_for_area(area: Rect) -> Self {
        Self::FALLBACK.for_area(area)
    }

    fn for_area(self, area: Rect) -> Self {
        if area.width == 0 || area.height == 0 {
            return Self::default();
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostViewKey {
    workspace_index: usize,
    tab_index: usize,
}

#[derive(Debug)]
struct HostPlacement {
    surface: HostSurfaceId,
    area: Rect,
    cell_size: HostCellSize,
    source_key: HostSourceKey,
    placement: KittyImagePlacement,
    scrollback_offset: u32,
    /// A looping frame sequence to transmit and arm the moment `placement`'s root frame is
    /// (re-)uploaded. `None` for every terminal-sourced placement and for any layer that never
    /// called [`crate::app::state::GraphicsLayer::with_animation`].
    animation: Option<crate::app::state::GraphicsAnimation>,
}

/// Which rect of the host viewport a placement is anchored to.
///
/// Panes are the original and only anchor; the sidebar is a sibling rect of the
/// tab surface rather than a pane, so it needs its own identity in every id and
/// cache key a pane placement already gets.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum HostSurfaceId {
    Pane(PaneId),
    Sidebar,
    /// The notification tray's badge artwork, which Herdr draws itself. Its own
    /// identity rather than a share of `Sidebar`, so a client publishing to the
    /// sidebar and the tray redrawing cannot evict one another.
    SignalTray,
    /// The sidebar tree's own rasterised cards.
    ///
    /// A separate identity from [`Self::Sidebar`] rather than a second layer on
    /// it, because the two have different owners: `Sidebar` is whatever an API
    /// client put there, and this is the TUI drawing its own rows. Sharing one
    /// identity would make either of them silently replace the other's image,
    /// and a client painting a backdrop under the tree is exactly the case that
    /// has to work.
    ///
    /// Carries the card's slot in the tree because a card is its own object: the
    /// shapes path publishes one placement per card, and giving each its own
    /// identity is what lets one card be re-uploaded, moved or dropped without
    /// disturbing the others. The sheet path publishes a single layer and uses
    /// slot `0`.
    SidebarCards(u16),
    /// The sidebar's ambient particle-field wash, drawn by the TUI itself. Its own identity for
    /// the same reason `SignalTray` has one: a client's own sidebar backdrop and this wash are
    /// two placements, not one silently replacing the other.
    SidebarParticleField,
    /// The whole-terminal solar-system background scene's ambient loop, drawn by the TUI itself.
    /// Its own identity, sibling to `SidebarParticleField`, so this and a client's own sidebar or
    /// pane images are separate placements rather than one replacing the other.
    BackgroundScene,
    /// The same scene's event-driven overlay (asteroid impacts, comets), layered above
    /// `BackgroundScene` but still below every pane's own text. Separate from `BackgroundScene`
    /// itself so the (rarely regenerated) ambient loop and the (regenerated only while something
    /// is live) overlay never have to share one upload/caching lifecycle.
    BackgroundEffects,
}

impl HostSurfaceId {
    /// Feeds the surface's identity into a host id hash. A pane contributes
    /// exactly its raw id, so every host image and placement id a shipped pane
    /// placement resolves to is unchanged by the sidebar existing.
    fn hash_identity(self, hasher: &mut DefaultHasher) {
        match self {
            Self::Pane(pane_id) => pane_id.raw().hash(hasher),
            Self::Sidebar => "surface.sidebar".hash(hasher),
            Self::SignalTray => "surface.signal-tray".hash(hasher),
            Self::SidebarCards(slot) => {
                "surface.sidebar.cards".hash(hasher);
                slot.hash(hasher);
            }
            Self::SidebarParticleField => "surface.sidebar.particle-field".hash(hasher),
            Self::BackgroundScene => "surface.background.scene".hash(hasher),
            Self::BackgroundEffects => "surface.background.effects".hash(hasher),
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum HostSourceKey {
    Terminal {
        pane_id: PaneId,
        image_id: u32,
    },
    /// An API-owned image layer composited over `surface`.
    Layer {
        surface: HostSurfaceId,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct ImageSignature {
    image_width: u32,
    image_height: u32,
    format_code: u32,
    data_len: usize,
    data_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct PlacementSignature {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
    z: i32,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClippedPlacement {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct HostGraphicsCache {
    images: HashMap<u32, ImageSignature>,
    placements: HashMap<(u32, u32), PlacementSignature>,
    /// Host image currently backing each (pane, source image id) pair.
    sources: HashMap<HostSourceKey, u32>,
    view: Option<HostViewKey>,
}

static KITTY_GRAPHICS_ENABLED: AtomicBool = AtomicBool::new(false);
static LOCAL_HOST_GRAPHICS: OnceLock<Mutex<HostGraphicsCache>> = OnceLock::new();

pub(crate) fn set_enabled(enabled: bool) {
    KITTY_GRAPHICS_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn is_enabled() -> bool {
    KITTY_GRAPHICS_ENABLED.load(Ordering::Acquire)
}

/// Gate for `[experimental] kitty_graphics_local_transport` — whether images
/// may skip the escape stream (`t=d`) for a local temp file (`t=f`), and pick
/// a raw pixel format the detected terminal is fast at, instead of always
/// sending PNG. See `host_graphics_is_local` for why this is a separate,
/// narrower opt-in than `kitty_graphics` itself.
static LOCAL_GRAPHICS_TRANSPORT_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_local_transport_enabled(enabled: bool) {
    LOCAL_GRAPHICS_TRANSPORT_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn local_transport_enabled() -> bool {
    LOCAL_GRAPHICS_TRANSPORT_ENABLED.load(Ordering::Acquire)
}

/// Which host terminal emulator this process's environment claims to be
/// running under.
///
/// Used only to pick a pixel format the terminal is known to be fast at —
/// see `preferred_card_pixel_format`. A terminal herdr cannot positively
/// identify is `Other` and gets no format upgrade: guessing costs real
/// throughput here rather than nothing, because a terminal's *worst* raw
/// format can be slower than PNG (Rio's `f=24` is 2.9x slower than its
/// `f=32`, measured — see the PR description).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTerminalKind {
    Kitty,
    Rio,
    #[default]
    Other,
}

pub(crate) fn host_terminal_kind_for_env(
    term_program: Option<&str>,
    term: Option<&str>,
    kitty_window_id_set: bool,
) -> HostTerminalKind {
    if term_program.is_some_and(|value| value.eq_ignore_ascii_case("rio")) {
        return HostTerminalKind::Rio;
    }
    if kitty_window_id_set || term.is_some_and(|value| value == "xterm-kitty") {
        return HostTerminalKind::Kitty;
    }
    HostTerminalKind::Other
}

/// Reads this process's own environment. Correct for the monolithic
/// (`--no-session`) app loop, which *is* the terminal-attached process; for
/// the split server, this is the server process's environment, which only
/// agrees with the terminal's own when server and client are co-located.
pub(crate) fn host_terminal_kind() -> HostTerminalKind {
    host_terminal_kind_for_env(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
    )
}

/// Whether this process is positively known to share a filesystem with the
/// terminal that will parse the Kitty escapes it writes.
///
/// `false` — including "we could not tell" — is the only safe default:
/// handing a remote terminal a local path renders nothing. Any of the
/// standard SSH client env vars means this process is running inside a
/// remote shell, so its stdout is reaching the real terminal over the
/// network rather than sharing a filesystem with it.
pub(crate) fn host_graphics_locality_for_env(
    ssh_tty: bool,
    ssh_connection: bool,
    ssh_client: bool,
) -> bool {
    !(ssh_tty || ssh_connection || ssh_client)
}

pub(crate) fn host_graphics_is_local() -> bool {
    host_graphics_locality_for_env(
        std::env::var_os("SSH_TTY").is_some(),
        std::env::var_os("SSH_CONNECTION").is_some(),
        std::env::var_os("SSH_CLIENT").is_some(),
    )
}

/// The host-capability probe run by a client at attach time, before it sends
/// `ClientMessage::Hello`.
///
/// Reads the *client* process's own environment — which, unlike the split
/// server's environment, is guaranteed to be the environment of the terminal
/// this client is actually attached to — and packages it for the server to
/// classify with [`host_terminal_kind_for_env`] / [`host_graphics_locality_for_env`].
/// Sending the raw facts rather than a pre-classified [`HostTerminalKind`]
/// keeps the classification rule itself in one place, exercised the same way
/// for a client's own report and for the monolithic (`--no-session`) path's
/// direct env read.
pub(crate) fn host_terminal_report_from_env() -> crate::protocol::HostTerminalReport {
    crate::protocol::HostTerminalReport {
        term_program: std::env::var("TERM_PROGRAM").ok(),
        term: std::env::var("TERM").ok(),
        kitty_window_id_set: std::env::var_os("KITTY_WINDOW_ID").is_some(),
        is_local: host_graphics_locality_for_env(
            std::env::var_os("SSH_TTY").is_some(),
            std::env::var_os("SSH_CONNECTION").is_some(),
            std::env::var_os("SSH_CLIENT").is_some(),
        ),
    }
}

/// The raw pixel format `host_terminal_kind` is known to be fast at when fed
/// by local transport. `None` for any terminal herdr cannot positively name
/// — callers keep PNG rather than guess.
fn preferred_local_pixel_format(kind: HostTerminalKind) -> Option<KittyImageFormat> {
    match kind {
        // Rio expands RGB to RGBA one pixel at a time; handed RGBA directly
        // it does no conversion at all. Measured 2.9x over RGB, at 1440p.
        HostTerminalKind::Rio => Some(KittyImageFormat::Rgba),
        // kitty pays no expansion cost either way; RGB is smaller to write.
        HostTerminalKind::Kitty => Some(KittyImageFormat::Rgb),
        HostTerminalKind::Other => None,
    }
}

/// The pixel format herdr's own sidebar cards should be rasterised in.
///
/// PNG (what herdr has always sent) unless local transport is enabled,
/// locality is positively established, and the terminal is one herdr has a
/// known-fast raw format for.
///
/// `is_opaque` gates the RGB case specifically: `f=24` has no alpha channel,
/// and herdr's cards are genuinely translucent — rounded corners, glow
/// falloff, the gutter around occupied rows all rely on real alpha rather
/// than a binary mask. Handing a translucent image to `f=24` would not
/// refuse or warn, it would silently clip every soft edge to hard opaque,
/// which is a worse regression than staying on PNG. RGBA has no such
/// caveat — it carries alpha same as PNG — so Rio's upgrade is unconditional.
///
/// `kind` and `is_local` come from `AppState::host_terminal_kind` /
/// `AppState::host_graphics_is_local`, which are populated from the actually
/// attached client's own host-capability probe (see
/// `host_terminal_report_from_env` and `ClientConnection::host_terminal_kind`)
/// — never from this process's own environment, which for the split server is
/// not necessarily the attached terminal's.
pub(crate) fn preferred_card_pixel_format(
    is_opaque: bool,
    kind: HostTerminalKind,
    is_local: bool,
) -> crate::api::schema::PaneGraphicsFormat {
    preferred_card_pixel_format_for(local_transport_enabled(), is_local, kind, is_opaque)
}

fn preferred_card_pixel_format_for(
    local_transport_enabled: bool,
    is_local: bool,
    kind: HostTerminalKind,
    is_opaque: bool,
) -> crate::api::schema::PaneGraphicsFormat {
    if !local_transport_enabled || !is_local {
        return crate::api::schema::PaneGraphicsFormat::Png;
    }
    match preferred_local_pixel_format(kind) {
        Some(KittyImageFormat::Rgba) => crate::api::schema::PaneGraphicsFormat::Rgba,
        Some(KittyImageFormat::Rgb) if is_opaque => crate::api::schema::PaneGraphicsFormat::Rgb,
        Some(KittyImageFormat::Rgb) | Some(KittyImageFormat::Png) | None => {
            crate::api::schema::PaneGraphicsFormat::Png
        }
    }
}

/// Directory local-transport files are staged in, namespaced by pid so two
/// herdr processes on the same machine never collide and a leftover
/// directory from a killed process is identifiable as stale.
fn local_graphics_dir() -> &'static std::path::Path {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let mut dir = std::env::temp_dir();
        dir.push(format!("herdr-kitty-graphics-{}", std::process::id()));
        dir
    })
}

fn local_graphics_path(host_id: u32) -> std::path::PathBuf {
    local_graphics_dir().join(format!("{host_id}.kitty"))
}

/// Stages `data` for `host_id` as a local file the terminal will read
/// directly (`t=f`), returning the path to reference in the control string.
///
/// Written to a sibling `.tmp` path and renamed into place: `rename(2)` is
/// atomic, so a terminal that already has the previous version of this file
/// open keeps reading it to completion — POSIX unlinks the old directory
/// entry, not the open file — and a terminal that opens the path after the
/// rename only ever sees a complete write. Nothing here waits for the
/// terminal to finish reading; the rename is what makes that safe to skip.
///
/// `host_id` is derived from the image's own content signature
/// (`host_image_id`), so two callers staging the same `host_id` are always
/// staging identical bytes — safe to overwrite redundantly, which matters
/// when more than one attached client shares this process's local-transport
/// directory.
fn write_local_graphics_file(host_id: u32, data: &[u8]) -> Option<std::path::PathBuf> {
    let dir = local_graphics_dir();
    std::fs::create_dir_all(dir).ok()?;
    let final_path = local_graphics_path(host_id);
    let tmp_path = dir.join(format!("{host_id}.tmp"));
    std::fs::write(&tmp_path, data).ok()?;
    std::fs::rename(&tmp_path, &final_path).ok()?;
    Some(final_path)
}

/// Best-effort removal of a host image's staged local-transport file, if any.
/// Called from `encode_delete_image` so a superseded or removed image never
/// leaves a file behind.
fn remove_local_graphics_file(host_id: u32) {
    let _ = std::fs::remove_file(local_graphics_path(host_id));
}

/// Removes the whole local-transport staging directory. Called wherever
/// herdr already tears down host-side Kitty state (`clear_all_host_graphics`)
/// so turning Kitty graphics off, or exiting, leaves nothing behind.
fn cleanup_local_graphics_dir() {
    let _ = std::fs::remove_dir_all(local_graphics_dir());
}

pub(crate) fn paint_local_pane_graphics(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cell_size: HostCellSize,
) -> io::Result<()> {
    let cache = LOCAL_HOST_GRAPHICS.get_or_init(|| Mutex::new(HostGraphicsCache::default()));
    let mut bytes = Vec::new();
    if let Ok(mut cache) = cache.lock() {
        bytes = encode_local_pane_graphics(
            app,
            terminal_runtimes,
            app.view.tab_surface(),
            cell_size,
            &mut cache,
        );
    }
    if bytes.is_empty() {
        return Ok(());
    }

    let mut framed = Vec::with_capacity(bytes.len() + 8);
    framed.extend_from_slice(b"\x1b7");
    framed.extend_from_slice(&bytes);
    framed.extend_from_slice(b"\x1b8");

    let mut stdout = io::stdout().lock();
    stdout.write_all(&framed)?;
    stdout.flush()
}

pub(crate) fn encode_local_pane_graphics(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    cache: &mut HostGraphicsCache,
) -> Vec<u8> {
    let mode_ok = app.mode == Mode::Terminal;
    let cell_ok = cell_size.is_known();
    tracing::debug!(
        mode_ok,
        cell_ok,
        cell_width_px = cell_size.width_px,
        cell_height_px = cell_size.height_px,
        active = ?app.active,
        pane_infos_len = surface.pane_infos.len(),
        "paint_local_pane_graphics entry"
    );
    if !mode_ok || !cell_ok {
        tracing::debug!(
            reason = if !mode_ok {
                "not terminal mode"
            } else {
                "cell size unknown"
            },
            "paint_local_pane_graphics early return"
        );
        return cache.clear_bytes();
    }

    let view_key = active_view_key(app);
    let placements =
        collect_visible_placements(app, terminal_runtimes, surface, cell_size, &cache.images);
    tracing::debug!(
        placements_collected = placements.len(),
        "collect_visible_placements result"
    );

    let mut bytes = Vec::new();
    let view_changed = cache.update_view(view_key);
    encode_graphics_update(
        &mut bytes,
        &placements,
        view_changed,
        &mut cache.images,
        &mut cache.placements,
        &mut cache.sources,
    );
    tracing::debug!(
        placements = placements.len(),
        bytes = bytes.len(),
        cell_width_px = cell_size.width_px,
        cell_height_px = cell_size.height_px,
        "painting kitty graphics placements"
    );
    bytes
}

pub(crate) fn has_visible_pane_graphics(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
) -> bool {
    if app.mode != Mode::Terminal || !cell_size.is_known() {
        return false;
    }

    // Surface layers anchor to viewport chrome rather than to a pane, so they
    // are checked before — and independently of — the active workspace, exactly
    // as `collect_visible_placements` collects them.
    let empty_uploaded = HashMap::new();
    for (surface_id, area, layer) in surface_layer_placement_targets(app) {
        let host_placement =
            layer_host_placement(surface_id, area, cell_size, layer, &empty_uploaded, false);
        if clipped_placement(&host_placement).is_some() {
            return true;
        }
    }

    let Some(ws_idx) = app.active else {
        return false;
    };
    if app
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
        .is_none()
    {
        return false;
    }

    for info in surface.pane_infos {
        if app.pane_graphics_layers.get(&info.id).is_some_and(|layer| {
            let host_placement = layer_host_placement(
                HostSurfaceId::Pane(info.id),
                info.inner_rect,
                cell_size,
                layer,
                &empty_uploaded,
                false,
            );
            clipped_placement(&host_placement).is_some()
        }) {
            return true;
        }

        if let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        {
            let scrollback_offset = runtime
                .scroll_metrics()
                .map(|m| m.offset_from_bottom as u32)
                .unwrap_or(0);
            for placement in runtime.kitty_image_placements_with_data_filter(|_| false) {
                let host_placement = HostPlacement {
                    surface: HostSurfaceId::Pane(info.id),
                    area: info.inner_rect,
                    cell_size,
                    source_key: HostSourceKey::Terminal {
                        pane_id: info.id,
                        image_id: placement.image_id,
                    },
                    placement,
                    scrollback_offset,
                    animation: None,
                };
                if clipped_placement(&host_placement).is_some() {
                    return true;
                }
            }
        }
    }
    false
}

fn encode_graphics_update(
    bytes: &mut Vec<u8>,
    placements: &[HostPlacement],
    view_changed: bool,
    host_images: &mut HashMap<u32, ImageSignature>,
    host_placements: &mut HashMap<(u32, u32), PlacementSignature>,
    sources: &mut HashMap<HostSourceKey, u32>,
) {
    // Prune sources that are no longer visible: a stale entry would keep its
    // old host image referenced and block the superseded-image delete.
    let current_sources: HashSet<HostSourceKey> = placements
        .iter()
        .filter(|placement| {
            matches!(placement.source_key, HostSourceKey::Terminal { .. })
                || clipped_placement(placement).is_some()
        })
        .map(|placement| placement.source_key)
        .collect();
    let mut removed_layer_images = HashSet::new();
    sources.retain(|source, host_id| {
        let retain = current_sources.contains(source);
        if !retain && matches!(source, HostSourceKey::Layer { .. }) {
            removed_layer_images.insert(*host_id);
        }
        retain
    });
    for host_id in removed_layer_images {
        if sources.values().any(|id| *id == host_id) {
            continue;
        }
        encode_delete_image(bytes, host_id);
        host_images.remove(&host_id);
        host_placements.retain(|(image_id, _), _| *image_id != host_id);
    }

    let mut current_placements = HashSet::new();
    for placement in placements {
        let clipped = clipped_placement(placement);
        tracing::debug!(
            surface = ?placement.surface,
            has_clipped = clipped.is_some(),
            grid_cols = placement.placement.render.grid_cols,
            grid_rows = placement.placement.render.grid_rows,
            viewport_col = placement.placement.render.viewport_col,
            viewport_row = placement.placement.render.viewport_row,
            area_w = placement.area.width,
            area_h = placement.area.height,
            "clipped_placement result"
        );
        let Some((clipped, format_code)) = clipped else {
            continue;
        };
        let host_id = host_image_id(placement.surface, &placement.placement);
        let host_placement_id = host_placement_id(placement.source_key, &placement.placement);
        let image_signature = image_signature(placement, format_code);
        let placement_signature =
            placement_signature(clipped, placement.placement.z, placement.scrollback_offset);
        let placement_key = (host_id, host_placement_id);
        current_placements.insert(placement_key);

        // Newly (re-)uploaded here means the loop frames must be transmitted and playback armed
        // — but not yet: kitty only picks up `a=a`'s autonomous clock for a placement that
        // already exists on screen. Deferred past `encode_display_placement` below for exactly
        // that reason; arming it here, before the image has a placement, left the terminal
        // stuck showing the root frame forever in live testing.
        let mut needs_animation_arm = false;
        match host_images.get(&host_id).copied() {
            Some(existing) if existing == image_signature => {}
            Some(_) => {
                encode_delete_image(bytes, host_id);
                host_placements.retain(|(image_id, placement_id), _| {
                    if *image_id == host_id {
                        current_placements.remove(&(*image_id, *placement_id));
                        false
                    } else {
                        true
                    }
                });
                if !encode_upload_image(bytes, placement, format_code, host_id) {
                    continue;
                }
                host_images.insert(host_id, image_signature);
                needs_animation_arm = placement.animation.is_some();
            }
            None => {
                if !encode_upload_image(bytes, placement, format_code, host_id) {
                    continue;
                }
                host_images.insert(host_id, image_signature);
                needs_animation_arm = placement.animation.is_some();
            }
        }

        release_superseded_source_image(
            bytes,
            sources,
            host_images,
            host_placements,
            &mut current_placements,
            placement.source_key,
            host_id,
        );

        // A different view can repaint the same cells with text or overlays and
        // leave the host-side Kitty placement state out of sync with this cache.
        // Re-emit the placement even when its geometry signature is unchanged.
        match host_placements.get_mut(&placement_key) {
            Some(existing) if !view_changed && *existing == placement_signature => {}
            Some(existing) => {
                encode_display_placement(
                    bytes,
                    clipped,
                    host_id,
                    host_placement_id,
                    placement.placement.z,
                );
                *existing = placement_signature;
            }
            None => {
                encode_display_placement(
                    bytes,
                    clipped,
                    host_id,
                    host_placement_id,
                    placement.placement.z,
                );
                host_placements.insert(placement_key, placement_signature);
            }
        }

        if needs_animation_arm {
            if let Some(animation) = &placement.animation {
                encode_animation_frames(
                    bytes,
                    host_id,
                    format_code,
                    placement.placement.image_width,
                    placement.placement.image_height,
                    animation,
                );
            }
        }
    }

    let mut stale_placements = Vec::new();
    for key in host_placements.keys() {
        if current_placements.contains(key) {
            continue;
        }
        stale_placements.push(*key);
    }
    for (host_id, host_placement_id) in stale_placements {
        encode_delete_placement(bytes, host_id, host_placement_id);
        host_placements.remove(&(host_id, host_placement_id));
    }
}

/// Records that `source` is now backed by `host_id` and deletes the host
/// image it previously pointed at once no other source references it.
fn release_superseded_source_image(
    bytes: &mut Vec<u8>,
    sources: &mut HashMap<HostSourceKey, u32>,
    host_images: &mut HashMap<u32, ImageSignature>,
    host_placements: &mut HashMap<(u32, u32), PlacementSignature>,
    current_placements: &mut HashSet<(u32, u32)>,
    source: HostSourceKey,
    host_id: u32,
) {
    let Some(previous) = sources.insert(source, host_id) else {
        return;
    };
    if previous == host_id || sources.values().any(|id| *id == previous) {
        return;
    }
    encode_delete_image(bytes, previous);
    host_images.remove(&previous);
    // The `d=I` delete also removes the image's placements host-side.
    host_placements.retain(|(image_id, placement_id), _| {
        if *image_id == previous {
            current_placements.remove(&(*image_id, *placement_id));
            false
        } else {
            true
        }
    });
}

pub(crate) fn clear_all_host_graphics() -> io::Result<()> {
    let cache = LOCAL_HOST_GRAPHICS.get_or_init(|| Mutex::new(HostGraphicsCache::default()));
    let mut bytes = Vec::new();
    if let Ok(mut cache) = cache.lock() {
        bytes = cache.clear_bytes();
    }
    cleanup_local_graphics_dir();
    if bytes.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()
}

impl HostGraphicsCache {
    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty() && self.placements.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn test_mark_non_empty(&mut self) {
        self.images.insert(
            HOST_IMAGE_ID_BASE,
            ImageSignature {
                image_width: 1,
                image_height: 1,
                format_code: 32,
                data_len: 4,
                data_fingerprint: 1,
            },
        );
    }

    pub(crate) fn clear_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for id in self.images.keys().copied().collect::<Vec<_>>() {
            encode_delete_image(&mut bytes, id);
        }
        self.images.clear();
        self.placements.clear();
        self.sources.clear();
        self.view = None;
        bytes
    }

    fn update_view(&mut self, view_key: Option<HostViewKey>) -> bool {
        if self.view == view_key {
            return false;
        }
        self.view = view_key;
        true
    }
}

fn active_view_key(app: &AppState) -> Option<HostViewKey> {
    let ws_idx = app.active?;
    let ws = app.workspaces.get(ws_idx)?;
    Some(HostViewKey {
        workspace_index: ws_idx,
        tab_index: ws.active_tab_index(),
    })
}

fn collect_visible_placements(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    uploaded_images: &HashMap<u32, ImageSignature>,
) -> Vec<HostPlacement> {
    let mut placements = Vec::new();

    // Chrome surfaces are laid out beside the tab surface, not inside it, so
    // they are collected before the active-workspace gate rather than through
    // the pane walk.
    for (surface_id, area, layer) in surface_layer_placement_targets(app) {
        placements.push(layer_host_placement(
            surface_id,
            area,
            cell_size,
            layer,
            uploaded_images,
            true,
        ));
    }

    let ws_idx = match app.active {
        Some(idx) => idx,
        None => {
            tracing::debug!("collect_visible_placements: no active workspace");
            return placements;
        }
    };
    if app
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
        .is_none()
    {
        tracing::debug!(ws_idx, "collect_visible_placements: no active tab");
        return placements;
    }

    tracing::debug!(
        ws_idx,
        terminal_runtimes_len = terminal_runtimes.len(),
        pane_infos_len = surface.pane_infos.len(),
        "collect_visible_placements: starting iteration"
    );
    for info in surface.pane_infos {
        if let Some(layer) = app.pane_graphics_layers.get(&info.id) {
            placements.push(layer_host_placement(
                HostSurfaceId::Pane(info.id),
                info.inner_rect,
                cell_size,
                layer,
                uploaded_images,
                true,
            ));
        }

        let runtime = match app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            Some(rt) => rt,
            None => {
                tracing::debug!(pane_id = ?info.id, "collect_visible_placements: runtime not found");
                continue;
            }
        };
        for placement in runtime.kitty_image_placements_with_data_filter(|descriptor| {
            let format_code = kitty_format_code(descriptor.format);
            let signature = image_signature_from_descriptor(descriptor, format_code);
            let host_id = host_image_id_for_signature(HostSurfaceId::Pane(info.id), signature);
            uploaded_images.get(&host_id).copied() != Some(signature)
        }) {
            let scrollback_offset = runtime
                .scroll_metrics()
                .map(|m| m.offset_from_bottom as u32)
                .unwrap_or(0);
            placements.push(HostPlacement {
                surface: HostSurfaceId::Pane(info.id),
                area: info.inner_rect,
                cell_size,
                source_key: HostSourceKey::Terminal {
                    pane_id: info.id,
                    image_id: placement.image_id,
                },
                placement,
                scrollback_offset,
                animation: None,
            });
        }
    }

    tracing::debug!(
        placements_len = placements.len(),
        "collect_visible_placements: done"
    );
    placements
}

/// Every non-pane rect that currently carries an image layer.
///
/// This is the second placement source the graphics surface has: it resolves a
/// named surface to the rect the layout put it at, so the layer travels the same
/// clipping, caching, signature and delete-by-id path a pane layer does.
///
/// Three owners feed it. API clients publish onto
/// [`crate::api::schema::GraphicsSurface`]; the notification tray publishes its
/// own badge artwork, and the sidebar tree publishes its rasterised cards, both
/// of which are Herdr's own drawing rather than a client's. They are separate
/// [`HostSurfaceId`]s on purpose — sharing one would mean a client setting a
/// sidebar image erased the tray or the cards, and either of those redrawing
/// erased the client's image.
fn surface_layer_placement_targets(
    app: &AppState,
) -> impl Iterator<Item = (HostSurfaceId, Rect, &crate::app::state::GraphicsLayer)> {
    app.surface_graphics_layers
        .iter()
        .map(|(surface, layer)| match surface {
            crate::api::schema::GraphicsSurface::Sidebar => {
                (HostSurfaceId::Sidebar, app.view.sidebar_rect, layer)
            }
        })
        .chain(app.signal_tray_graphics.as_ref().map(|layer| {
            (
                HostSurfaceId::SignalTray,
                crate::ui::signal_tray_graphics_rect(app),
                layer,
            )
        }))
        .chain(app.sidebar_particle_field.as_ref().map(|layer| {
            (
                HostSurfaceId::SidebarParticleField,
                crate::ui::sidebar_particle_field_rect(app),
                layer,
            )
        }))
        .chain(
            app.background_scene
                .as_ref()
                .map(|layer| (HostSurfaceId::BackgroundScene, app.screen_rect(), layer)),
        )
        .chain(
            app.background_effects_layer
                .as_ref()
                .map(|layer| (HostSurfaceId::BackgroundEffects, app.screen_rect(), layer)),
        )
        // The TUI's own sidebar cards join here rather than through the API
        // map, so they travel the same clipping, dedup, signature and
        // delete-by-id path as a client's layer without being reachable — or
        // clearable — by `surface.graphics.*`. Their rect is the tree's, not
        // the sidebar's: a card's image is only as large as that card and the
        // reach of its bloom.
        //
        // One entry per card under the shapes path and exactly one under the
        // sheet path, each at its own slot, so a card that changed is the only
        // thing re-uploaded and a card that went away is the only thing deleted.
        //
        // Withheld from a pass that did not build them, but only under the
        // shapes path. The layers are the foreground client's, and a pass that
        // left them alone laid its rows out without them and drew its character
        // cards — so a *shape* arriving there would stand a transparent outline
        // over a border, a chip and a title a few pixels off, the doubling
        // `image_card::shape_covers_row` exists to prevent, arrived at from the
        // other side. A *sheet* cannot double anything: it is opaque over every
        // cell a row owns, so it simply covers the characters, which is what the
        // default path did before shapes existed and still has to do. Only the
        // cards are ever withheld; every other surface this client is entitled
        // to keeps flowing either way.
        .chain(
            if !app.sidebar_card_shapes || app.view.sidebar_card_layers_published {
                app.sidebar_card_layers.as_slice()
            } else {
                &[]
            }
            .iter()
            .enumerate()
            .map(|(slot, cards)| {
                (
                    HostSurfaceId::SidebarCards(slot.try_into().unwrap_or(u16::MAX)),
                    // The panel box, not the card's own rect, with the card's
                    // position carried in the layer's viewport offset. At rest
                    // the two spell the same placement; while a row is sliding
                    // they do not, and the clip that falls out of this is what
                    // keeps a card travelling past the panel's edge from being
                    // drawn over the terminal panes.
                    cards.clip,
                    &cards.layer,
                )
            }),
        )
}

/// Builds the host placement for one API-owned image layer over `area`.
///
/// Panes pass their interior rect; chrome surfaces pass whatever rect the layout
/// gave them. Everything downstream — clipping, ids, cache signatures — is
/// keyed on `surface`, so neither source can collide with the other.
fn layer_host_placement(
    surface: HostSurfaceId,
    area: Rect,
    cell_size: HostCellSize,
    layer: &crate::app::state::GraphicsLayer,
    uploaded_images: &HashMap<u32, ImageSignature>,
    include_data: bool,
) -> HostPlacement {
    let format = pane_graphics_kitty_format(layer.format);
    let format_code = kitty_format_code(format);
    let signature = ImageSignature {
        image_width: layer.image_width,
        image_height: layer.image_height,
        format_code,
        data_len: layer.data.len(),
        data_fingerprint: layer.data_fingerprint,
    };
    let host_id = layer_host_image_id(surface, signature);
    let already_uploaded = uploaded_images.get(&host_id).copied() == Some(signature);
    let data = if !include_data || already_uploaded {
        Vec::new()
    } else {
        layer.data.clone()
    };
    // Loop frames only matter the moment the root frame above is actually (re-)uploaded — see
    // `encode_graphics_update`, which reaches them through this same `data.is_empty()` gate via
    // `encode_upload_image`. Skipped otherwise so a visibility-only caller like
    // `has_visible_pane_graphics` never clones a whole frame sequence just to check geometry.
    let animation = if !include_data || already_uploaded {
        None
    } else {
        layer.animation.clone()
    };
    let render = layer.render;
    let grid_cols = if render.grid_cols == 0 {
        u32::from(area.width)
    } else {
        render.grid_cols
    };
    let grid_rows = if render.grid_rows == 0 {
        u32::from(area.height)
    } else {
        render.grid_rows
    };

    HostPlacement {
        surface,
        area,
        cell_size,
        source_key: HostSourceKey::Layer { surface },
        scrollback_offset: 0,
        animation,
        placement: KittyImagePlacement {
            image_id: 1,
            placement_id: 1,
            z: render.z,
            x_offset: 0,
            y_offset: 0,
            image_width: layer.image_width,
            image_height: layer.image_height,
            format,
            data_len: layer.data.len(),
            data_fingerprint: layer.data_fingerprint,
            data,
            render: KittyPlacementRenderInfo {
                pixel_width: layer.image_width,
                pixel_height: layer.image_height,
                grid_cols,
                grid_rows,
                viewport_col: render.viewport_col,
                viewport_row: render.viewport_row,
                source_x: 0,
                source_y: 0,
                source_width: 0,
                source_height: 0,
            },
        },
    }
}

fn pane_graphics_kitty_format(format: crate::api::schema::PaneGraphicsFormat) -> KittyImageFormat {
    match format {
        crate::api::schema::PaneGraphicsFormat::Png => KittyImageFormat::Png,
        crate::api::schema::PaneGraphicsFormat::Rgb => KittyImageFormat::Rgb,
        crate::api::schema::PaneGraphicsFormat::Rgba => KittyImageFormat::Rgba,
    }
}

fn host_image_id(surface: HostSurfaceId, placement: &KittyImagePlacement) -> u32 {
    let format_code = kitty_format_code(placement.format);
    host_image_id_for_signature(
        surface,
        ImageSignature {
            image_width: placement.image_width,
            image_height: placement.image_height,
            format_code,
            data_len: placement.data_len,
            data_fingerprint: placement.data_fingerprint,
        },
    )
}

fn host_image_id_for_signature(surface: HostSurfaceId, signature: ImageSignature) -> u32 {
    let mut hasher = DefaultHasher::new();
    surface.hash_identity(&mut hasher);
    signature.hash(&mut hasher);
    HOST_IMAGE_ID_BASE + ((hasher.finish() as u32) % 900_000)
}

fn layer_host_image_id(surface: HostSurfaceId, signature: ImageSignature) -> u32 {
    let mut hasher = DefaultHasher::new();
    surface.hash_identity(&mut hasher);
    signature.hash(&mut hasher);
    PANE_GRAPHICS_IMAGE_ID_BIT | ((hasher.finish() as u32) & !PANE_GRAPHICS_IMAGE_ID_BIT)
}

fn host_placement_id(source_key: HostSourceKey, placement: &KittyImagePlacement) -> u32 {
    let mut hasher = DefaultHasher::new();
    match source_key {
        HostSourceKey::Terminal { pane_id, .. } => pane_id.raw().hash(&mut hasher),
        HostSourceKey::Layer { surface } => {
            "pane.graphics".hash(&mut hasher);
            surface.hash_identity(&mut hasher);
        }
    }
    placement.image_id.hash(&mut hasher);
    placement.placement_id.hash(&mut hasher);
    1 + ((hasher.finish() as u32) % 900_000)
}

fn encode_delete_image(out: &mut Vec<u8>, id: u32) {
    remove_local_graphics_file(id);
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
}

fn encode_delete_placement(out: &mut Vec<u8>, host_id: u32, host_placement_id: u32) {
    let _ = write!(
        out,
        "\x1b_Ga=d,d=i,i={host_id},p={host_placement_id},q=2;\x1b\\"
    );
}

fn encode_upload_image(
    out: &mut Vec<u8>,
    placement: &HostPlacement,
    format_code: u32,
    host_id: u32,
) -> bool {
    if placement.placement.data.is_empty() {
        return false;
    }

    if local_transport_enabled() && host_graphics_is_local() {
        if let Some(path) = write_local_graphics_file(host_id, &placement.placement.data) {
            encode_upload_image_via_file(
                out,
                &path,
                format_code,
                placement.placement.image_width,
                placement.placement.image_height,
                host_id,
            );
            return true;
        }
        // Staging the file failed (disk full, permissions, ...) — fall
        // through to `t=d`, the transport that never needs one.
    }

    let control = format!(
        "a=t,t=d,f={format_code},s={},v={},i={host_id},q=2",
        placement.placement.image_width, placement.placement.image_height,
    );
    encode_kitty_data(out, &control, "", &placement.placement.data);
    true
}

/// Transmits a Kitty image by local file (`t=f`) instead of base64-in-escape
/// (`t=d`): the terminal reads pixels off disk itself, so this never puts
/// them in the escape stream at all.
///
/// The payload here is the base64-encoded *path*, not pixel data — always a
/// few dozen bytes, so unlike [`encode_kitty_data`] this never chunks
/// (`m=0` unconditionally, one control sequence). `KITTY_CHUNK_BYTES`
/// deliberately does not apply on this path.
fn encode_upload_image_via_file(
    out: &mut Vec<u8>,
    path: &std::path::Path,
    format_code: u32,
    width: u32,
    height: u32,
    host_id: u32,
) {
    let control = format!("a=t,t=f,f={format_code},s={width},v={height},i={host_id},q=2");
    let encoded_path =
        base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes());
    let _ = write!(out, "\x1b_G{control},m=0;{encoded_path}\x1b\\");
}

/// Appends one additional loop frame to an image whose root frame was just transmitted (Kitty's
/// `a=f`, "transmit frame"). Shares the root frame's dimensions and format; `gap_ms` is how long
/// the terminal shows it once autonomous playback is armed.
fn encode_transmit_frame(
    out: &mut Vec<u8>,
    host_id: u32,
    format_code: u32,
    width: u32,
    height: u32,
    gap_ms: u32,
    data: &[u8],
) {
    let control = format!("a=f,i={host_id},f={format_code},s={width},v={height},z={gap_ms},q=2");
    encode_kitty_data(out, &control, ",a=f", data);
}

/// Arms autonomous playback of the frames already transmitted for `host_id` (Kitty's `a=a`,
/// "control animation"). The root frame carries no gap of its own by spec, so it is set
/// explicitly here for the loop-back leg before `s=3,v=1` starts looped playback. From here the
/// terminal runs its own clock — see `data/herdr-native-animation-playback-verify` for the
/// empirical confirmation that this sends zero further protocol bytes once armed.
fn encode_arm_animation(out: &mut Vec<u8>, host_id: u32, root_gap_ms: u32) {
    let _ = write!(out, "\x1b_Ga=a,i={host_id},r=1,z={root_gap_ms},q=2;\x1b\\");
    let _ = write!(out, "\x1b_Ga=a,i={host_id},s=3,v=1,q=2;\x1b\\");
}

/// Transmits every extra loop frame and arms playback for an image whose root frame was just
/// (re-)uploaded by [`encode_upload_image`]. Only called from the two `encode_graphics_update`
/// branches that just did that upload, so this runs exactly once per distinct image signature —
/// the same cache that gives every other layer a no-op on an unchanged frame is what keeps this
/// from re-arming on every tick.
fn encode_animation_frames(
    out: &mut Vec<u8>,
    host_id: u32,
    format_code: u32,
    width: u32,
    height: u32,
    animation: &crate::app::state::GraphicsAnimation,
) {
    if animation.frames.is_empty() {
        return;
    }
    for frame in &animation.frames {
        encode_transmit_frame(
            out,
            host_id,
            format_code,
            width,
            height,
            animation.frame_gap_ms,
            frame,
        );
    }
    encode_arm_animation(out, host_id, animation.frame_gap_ms);
}

fn encode_display_placement(
    out: &mut Vec<u8>,
    clipped: ClippedPlacement,
    host_id: u32,
    host_placement_id: u32,
    z: i32,
) {
    let _ = write!(out, "\x1b[{};{}H", clipped.y + 1, clipped.x + 1);
    let mut control = format!(
        "a=p,i={host_id},p={host_placement_id},c={},r={},z={z},C=1,q=2",
        clipped.cols, clipped.rows,
    );
    if clipped.source_x > 0 {
        let _ = write!(control, ",x={}", clipped.source_x);
    }
    if clipped.source_y > 0 {
        let _ = write!(control, ",y={}", clipped.source_y);
    }
    if clipped.source_width > 0 {
        let _ = write!(control, ",w={}", clipped.source_width);
    }
    if clipped.source_height > 0 {
        let _ = write!(control, ",h={}", clipped.source_height);
    }
    if clipped.x_offset > 0 {
        let _ = write!(control, ",X={}", clipped.x_offset);
    }
    if clipped.y_offset > 0 {
        let _ = write!(control, ",Y={}", clipped.y_offset);
    }

    let _ = write!(out, "\x1b_G{control};\x1b\\");
}

fn clipped_placement(placement: &HostPlacement) -> Option<(ClippedPlacement, u32)> {
    if placement.area.width == 0 || placement.area.height == 0 {
        tracing::debug!(
            area_w = placement.area.width,
            area_h = placement.area.height,
            "clipped_placement: area zero"
        );
        return None;
    }
    let render = placement.placement.render;
    if render.grid_cols == 0 || render.grid_rows == 0 {
        tracing::debug!(
            grid_cols = render.grid_cols,
            grid_rows = render.grid_rows,
            "clipped_placement: grid zero"
        );
        return None;
    }
    let format_code = kitty_format_code(placement.placement.format);

    let left_clip_cells = if render.viewport_col < 0 {
        render.viewport_col.saturating_neg() as u32
    } else {
        0
    };
    let top_clip_cells = if render.viewport_row < 0 {
        render.viewport_row.saturating_neg() as u32
    } else {
        0
    };
    let viewport_col = render.viewport_col.max(0) as u32;
    let viewport_row = render.viewport_row.max(0) as u32;
    tracing::debug!(
        viewport_col = viewport_col,
        viewport_row = viewport_row,
        area_w = placement.area.width,
        area_h = placement.area.height,
        scrollback_offset = placement.scrollback_offset,
        raw_viewport_row = render.viewport_row,
        cond1 = viewport_col >= placement.area.width as u32,
        cond2 = viewport_row >= placement.area.height as u32,
        "clipped_placement: viewport check"
    );
    if viewport_col >= placement.area.width as u32 || viewport_row >= placement.area.height as u32 {
        return None;
    }

    let visible_cols = render
        .grid_cols
        .saturating_sub(left_clip_cells)
        .min(placement.area.width as u32 - viewport_col);
    let visible_rows = render
        .grid_rows
        .saturating_sub(top_clip_cells)
        .min(placement.area.height as u32 - viewport_row);
    tracing::debug!(
        visible_cols = visible_cols,
        visible_rows = visible_rows,
        left_clip_cells = left_clip_cells,
        top_clip_cells = top_clip_cells,
        "clipped_placement: visible dims check"
    );
    if visible_cols == 0 || visible_rows == 0 {
        return None;
    }

    let source_width = if render.source_width == 0 {
        placement.placement.image_width
    } else {
        render.source_width
    };
    let source_height = if render.source_height == 0 {
        placement.placement.image_height
    } else {
        render.source_height
    };
    let pixel_width = render
        .pixel_width
        .max(
            render
                .grid_cols
                .saturating_mul(placement.cell_size.width_px),
        )
        .max(1);
    let pixel_height = render
        .pixel_height
        .max(
            render
                .grid_rows
                .saturating_mul(placement.cell_size.height_px),
        )
        .max(1);

    let crop_left_px = left_clip_cells.saturating_mul(placement.cell_size.width_px);
    let crop_top_px = top_clip_cells.saturating_mul(placement.cell_size.height_px);
    let visible_width_px = visible_cols.saturating_mul(placement.cell_size.width_px);
    let visible_height_px = visible_rows.saturating_mul(placement.cell_size.height_px);

    let source_x = render.source_x + scale_pixels(crop_left_px, source_width, pixel_width);
    let source_y = render.source_y + scale_pixels(crop_top_px, source_height, pixel_height);
    let source_width = scale_pixels(visible_width_px, source_width, pixel_width)
        .max(1)
        .min(placement.placement.image_width.saturating_sub(source_x));
    let source_height = scale_pixels(visible_height_px, source_height, pixel_height)
        .max(1)
        .min(placement.placement.image_height.saturating_sub(source_y));

    if source_width == 0 || source_height == 0 {
        tracing::debug!(
            source_width = source_width,
            source_height = source_height,
            image_width = placement.placement.image_width,
            image_height = placement.placement.image_height,
            "clipped_placement: source dims zero"
        );
        return None;
    }

    tracing::debug!("clipped_placement: success");
    Some((
        ClippedPlacement {
            x: placement.area.x + viewport_col as u16,
            y: placement.area.y + viewport_row as u16,
            cols: visible_cols,
            rows: visible_rows,
            source_x,
            source_y,
            source_width,
            source_height,
            x_offset: if left_clip_cells == 0 {
                placement.placement.x_offset
            } else {
                0
            },
            y_offset: if top_clip_cells == 0 {
                placement.placement.y_offset
            } else {
                0
            },
        },
        format_code,
    ))
}

fn scale_pixels(value: u32, source: u32, dest: u32) -> u32 {
    ((value as u64).saturating_mul(source as u64) / dest.max(1) as u64).min(u32::MAX as u64) as u32
}

fn image_signature(placement: &HostPlacement, format_code: u32) -> ImageSignature {
    ImageSignature {
        image_width: placement.placement.image_width,
        image_height: placement.placement.image_height,
        format_code,
        data_len: placement.placement.data_len,
        data_fingerprint: placement.placement.data_fingerprint,
    }
}

fn image_signature_from_descriptor(
    descriptor: KittyImageDescriptor,
    format_code: u32,
) -> ImageSignature {
    ImageSignature {
        image_width: descriptor.image_width,
        image_height: descriptor.image_height,
        format_code,
        data_len: descriptor.data_len,
        data_fingerprint: descriptor.data_fingerprint,
    }
}

fn placement_signature(
    clipped: ClippedPlacement,
    z: i32,
    scrollback_offset: u32,
) -> PlacementSignature {
    PlacementSignature {
        x: clipped.x,
        y: clipped.y,
        cols: clipped.cols,
        rows: clipped.rows,
        source_x: clipped.source_x,
        source_y: clipped.source_y,
        source_width: clipped.source_width,
        source_height: clipped.source_height,
        x_offset: clipped.x_offset,
        y_offset: clipped.y_offset,
        z,
        scrollback_offset,
    }
}

fn kitty_format_code(format: KittyImageFormat) -> u32 {
    match format {
        KittyImageFormat::Rgb => 24,
        KittyImageFormat::Rgba => 32,
        KittyImageFormat::Png => 100,
    }
}

/// `continuation_extra` is appended to every continuation chunk's control data after `m=`.
/// Plain image transmission needs nothing here — the spec has a continuation chunk carry only
/// `m` and optionally `q`. Animation frame transmission (`a=f`) is the one exception: the spec
/// requires every continuation chunk to also repeat `a=f`, or the terminal has no way to tell a
/// continuation apart from the default `a=t` action and silently misroutes it. Confirmed live:
/// without this, a chunked (>3072-byte) loop frame reached kitty but playback never advanced
/// past the frame after it — see `sidebar_particle_field` verification notes.
fn encode_kitty_data(out: &mut Vec<u8>, control: &str, continuation_extra: &str, data: &[u8]) {
    let mut chunks = data.chunks(KITTY_CHUNK_BYTES).peekable();
    let Some(first) = chunks.next() else {
        return;
    };
    let more = if chunks.peek().is_some() { 1 } else { 0 };
    let encoded = base64::engine::general_purpose::STANDARD.encode(first);
    let _ = write!(out, "\x1b_G{control},m={more};{encoded}\x1b\\");

    while let Some(chunk) = chunks.next() {
        let more = if chunks.peek().is_some() { 1 } else { 0 };
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = write!(out, "\x1b_Gm={more}{continuation_extra};{encoded}\x1b\\");
    }
}

#[cfg(test)]
mod host_cell_size_is_a_measurement {
    use super::*;

    /// The shape that reached the captain's screen.
    ///
    /// His terminal reported a pty pixel width that did not track the window,
    /// so `ws_xpixel / columns` shrank as the window grew: about 4 px per cell
    /// on a 1910-wide window and about 2 px on a 3428-wide one, against a real
    /// cell of about 12. Every one of those has to be refused, because a cell
    /// that small is not a small font — it is arithmetic on a field the
    /// terminal never filled in, and the sidebar's cards get rasterised into it
    /// and then stretched back out by the terminal.
    #[test]
    fn a_cell_no_font_could_draw_in_is_refused() {
        for (width_px, height_px) in [(2, 8), (3, 9), (4, 12), (1, 1), (2, 12)] {
            assert!(
                !HostCellSize {
                    width_px,
                    height_px
                }
                .is_plausible(),
                "{width_px}x{height_px} was accepted as a terminal cell"
            );
        }
    }

    /// And the cells real terminals actually have are not refused, including
    /// the fallback Herdr assumes when nothing will tell it one.
    #[test]
    fn the_cells_real_terminals_have_are_accepted() {
        for (width_px, height_px) in [
            (8, 16),  // the fallback, and a common 1x cell
            (12, 24), // the captain's
            (6, 13),
            (7, 15),
            (10, 21),
            (20, 42), // a HiDPI terminal reporting physical pixels
            (24, 48),
        ] {
            assert!(
                HostCellSize {
                    width_px,
                    height_px
                }
                .is_plausible(),
                "{width_px}x{height_px} was refused"
            );
        }
        assert!(HostCellSize::FALLBACK.is_plausible());
    }

    /// A refused cell becomes a coherent one rather than a clamped half of
    /// itself: the card is laid out against the *ratio*, so keeping whichever
    /// of the pair happened to be in range would distort every card.
    #[test]
    fn a_refused_cell_falls_back_whole() {
        let bogus = HostCellSize {
            width_px: 2,
            height_px: 16,
        };
        assert_eq!(bogus.or_fallback(), HostCellSize::FALLBACK);
        let good = HostCellSize {
            width_px: 12,
            height_px: 24,
        };
        assert_eq!(good.or_fallback(), good);
    }

    /// The original defect, pinned: a pty whose pixel fields are a stale
    /// constant yields a cell that *shrinks as the window grows*.
    ///
    /// Upstream's cell-size fix (#2160) prefers the ioctl whenever it reports
    /// any nonzero pixels and only queries the host when it reports none, so
    /// on an SSH pty carrying a constant `ws_xpixel` the division below is
    /// still what the client computes. This gate is what stops it reaching the
    /// card path.
    ///
    /// The sweep is the regime the defect was measured in — a 3440px-wide
    /// display, which is 200-390 columns at a real 9px cell, where the stale
    /// constant divides down to the 2-4px that was reported. Note the bound
    /// this does *not* claim: the gate refuses a cell for being implausible,
    /// so a stale pty at a low column count still divides to something inside
    /// the bounds and is believed. Narrow windows are not covered by this.
    #[test]
    fn a_stale_pty_width_never_yields_a_shrinking_cell_at_the_widths_it_was_seen_at() {
        // Back-computed from the measurement in #50: ~4px at 1910 wide and
        // ~2px at 3428 wide puts the constant the SSH client sent near 800px.
        const STALE_PTY_WIDTH_PX: u32 = 800;
        const STALE_PTY_HEIGHT_PX: u32 = 1080;

        let mut derived_widths = Vec::new();
        let mut handed_to_the_card = Vec::new();
        for columns in [200u32, 250, 300, 350, 390] {
            let rows = 40;
            let derived = HostCellSize {
                width_px: (STALE_PTY_WIDTH_PX / columns).max(1),
                height_px: (STALE_PTY_HEIGHT_PX / rows).max(1),
            };
            assert!(
                !derived.is_plausible(),
                "a {}px cell derived at {columns} columns must be refused",
                derived.width_px
            );
            derived_widths.push(derived.width_px);
            handed_to_the_card.push(derived.or_fallback());
        }

        // The raw division still descends — that is upstream's path, unchanged.
        assert!(
            derived_widths.first() > derived_widths.last(),
            "the sweep must actually reproduce the shrink: {derived_widths:?}"
        );
        // What the card is laid out against does not.
        assert!(
            handed_to_the_card.windows(2).all(|pair| pair[0] == pair[1]),
            "the cell handed to the card path must not track window width: {handed_to_the_card:?}"
        );
        assert_eq!(handed_to_the_card[0], HostCellSize::FALLBACK);
    }

    /// And a host that answers `CSI 16 t` is believed as-is. 9x18 is the
    /// captain's terminal; upstream's client reaches this only when the ioctl
    /// reports no pixels at all.
    #[test]
    fn a_reported_host_cell_is_taken_whole() {
        let reported = HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        assert!(reported.is_plausible());
        assert_eq!(reported.or_fallback(), reported);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn test_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
        HostPlacement {
            surface: HostSurfaceId::Pane(PaneId::from_raw(1)),
            area: Rect::new(0, 0, 20, 10),
            cell_size: HostCellSize {
                width_px: 10,
                height_px: 10,
            },
            source_key: HostSourceKey::Terminal {
                pane_id: PaneId::from_raw(1),
                image_id: 7,
            },
            scrollback_offset: 0,
            animation: None,
            placement: KittyImagePlacement {
                image_id: 7,
                placement_id: 3,
                z: 0,
                x_offset: 0,
                y_offset: 0,
                image_width: 30,
                image_height: 30,
                format: KittyImageFormat::Rgba,
                data_len: 30 * 30 * 4,
                data_fingerprint: 42,
                data: vec![255; 30 * 30 * 4],
                render: KittyPlacementRenderInfo {
                    pixel_width: 0,
                    pixel_height: 0,
                    grid_cols: 3,
                    grid_rows: 3,
                    viewport_col,
                    viewport_row,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                },
            },
        }
    }

    fn pane_layer_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
        let mut placement = test_placement(viewport_col, viewport_row);
        placement.source_key = HostSourceKey::Layer {
            surface: placement.surface,
        };
        placement
    }

    #[test]
    fn terminal_placement_id_preserves_legacy_identity() {
        let placement = test_placement(0, 0);
        let mut legacy = DefaultHasher::new();
        PaneId::from_raw(1).raw().hash(&mut legacy);
        placement.placement.image_id.hash(&mut legacy);
        placement.placement.placement_id.hash(&mut legacy);
        let expected = 1 + ((legacy.finish() as u32) % 900_000);

        assert_eq!(
            host_placement_id(placement.source_key, &placement.placement),
            expected
        );
        assert_ne!(
            host_placement_id(
                HostSourceKey::Layer {
                    surface: placement.surface,
                },
                &placement.placement,
            ),
            expected
        );
    }

    #[test]
    fn pane_graphics_image_ids_are_disjoint_from_terminal_image_ids() {
        let placement = test_placement(0, 0);
        let signature = image_signature(&placement, kitty_format_code(placement.placement.format));
        let terminal_id = host_image_id_for_signature(placement.surface, signature);
        let pane_graphics_id = layer_host_image_id(placement.surface, signature);

        assert_eq!(terminal_id & PANE_GRAPHICS_IMAGE_ID_BIT, 0);
        assert_ne!(pane_graphics_id & PANE_GRAPHICS_IMAGE_ID_BIT, 0);
    }

    #[test]
    fn clipped_placement_handles_positive_viewport_without_wrapping() {
        let placement = test_placement(2, 2);
        let (clipped, _) = clipped_placement(&placement).expect("visible placement");

        assert_eq!(clipped.x, 2);
        assert_eq!(clipped.y, 2);
        assert_eq!(clipped.cols, 3);
        assert_eq!(clipped.rows, 3);
        assert_eq!(clipped.source_x, 0);
        assert_eq!(clipped.source_y, 0);
    }

    #[test]
    fn clipped_placement_crops_negative_viewport_offsets() {
        let placement = test_placement(-1, -1);
        let (clipped, _) = clipped_placement(&placement).expect("partially visible placement");

        assert_eq!(clipped.x, 0);
        assert_eq!(clipped.y, 0);
        assert_eq!(clipped.cols, 2);
        assert_eq!(clipped.rows, 2);
        assert_eq!(clipped.source_x, 10);
        assert_eq!(clipped.source_y, 10);
    }

    #[test]
    fn pane_graphics_layer_defaults_to_full_pane_grid() {
        let pane_id = PaneId::from_raw(9);
        let inner_rect = Rect::new(2, 1, 8, 3);
        let layer = crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            80,
            30,
            vec![255; 80 * 30 * 4],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        );

        let placement = layer_host_placement(
            HostSurfaceId::Pane(pane_id),
            inner_rect,
            HostCellSize {
                width_px: 10,
                height_px: 10,
            },
            &layer,
            &HashMap::new(),
            true,
        );
        let (clipped, format_code) = clipped_placement(&placement).expect("visible layer");

        assert_eq!(format_code, 32);
        assert_eq!(clipped.x, 2);
        assert_eq!(clipped.y, 1);
        assert_eq!(clipped.cols, 8);
        assert_eq!(clipped.rows, 3);
        assert_eq!(placement.placement.data.len(), 80 * 30 * 4);
    }

    fn test_cell_size() -> HostCellSize {
        HostCellSize {
            width_px: 10,
            height_px: 10,
        }
    }

    fn sidebar_layer(
        placement: crate::api::schema::PaneGraphicsPlacementParams,
    ) -> crate::app::state::GraphicsLayer {
        crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            200,
            200,
            vec![7; 200 * 200 * 4],
            placement,
        )
    }

    /// An `AppState` with a sidebar rect but deliberately no workspaces: the
    /// sidebar is chrome, so its placement must not depend on a live tab.
    fn app_with_sidebar(
        sidebar_rect: Rect,
        placement: crate::api::schema::PaneGraphicsPlacementParams,
    ) -> AppState {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.view.sidebar_rect = sidebar_rect;
        app.surface_graphics_layers.insert(
            crate::api::schema::GraphicsSurface::Sidebar,
            sidebar_layer(placement),
        );
        app
    }

    fn empty_surface() -> crate::ui::TabSurfaceView<'static> {
        crate::ui::TabSurfaceView {
            pane_infos: &[],
            split_borders: &[],
        }
    }

    #[test]
    fn sidebar_layer_anchors_to_the_sidebar_rect_and_fills_it() {
        let app = app_with_sidebar(
            Rect::new(0, 0, 26, 20),
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        );
        let placements = collect_visible_placements(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
            &HashMap::new(),
        );

        assert_eq!(placements.len(), 1);
        let placement = &placements[0];
        assert_eq!(placement.surface, HostSurfaceId::Sidebar);
        assert_eq!(
            placement.source_key,
            HostSourceKey::Layer {
                surface: HostSurfaceId::Sidebar,
            }
        );
        let (clipped, _) = clipped_placement(placement).expect("visible sidebar layer");
        assert_eq!((clipped.x, clipped.y), (0, 0));
        assert_eq!((clipped.cols, clipped.rows), (26, 20));
    }

    #[test]
    fn sidebar_layer_clips_to_the_sidebar_and_never_spills_into_panes() {
        // A grid far wider and taller than the sidebar, offset inside it.
        let app = app_with_sidebar(
            Rect::new(4, 2, 26, 20),
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 6,
                viewport_row: 5,
                grid_cols: 200,
                grid_rows: 200,
                z: 0,
            },
        );
        let placements = collect_visible_placements(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
            &HashMap::new(),
        );
        let (clipped, _) = clipped_placement(&placements[0]).expect("visible sidebar layer");

        assert_eq!((clipped.x, clipped.y), (10, 7));
        // Right edge stops at the sidebar's, not the terminal's.
        assert_eq!(u32::from(clipped.x) + clipped.cols, 4 + 26);
        assert_eq!(u32::from(clipped.y) + clipped.rows, 2 + 20);
    }

    /// A card part-way through its slide is cropped at the panel's edge.
    ///
    /// The slide deliberately puts a card past that edge — that is what makes
    /// an arrival read as coming in from somewhere — so the thing that stops it
    /// from being drawn over the terminal panes is this crop, and it is the same
    /// crop a client's own sidebar layer already gets.
    #[test]
    fn a_card_sliding_past_the_panel_edge_is_cropped_at_it() {
        let panel = Rect::new(0, 1, 26, 20);
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.view.sidebar_rect = panel;
        app.view.sidebar_card_layers_published = true;
        app.sidebar_card_shapes = true;

        let mut card = sidebar_card_layer(Rect::new(2, 3, 20, 6));
        card.clip = panel;
        // Most of the card is past the panel's right edge, which is where it is
        // on the first frames of an arrival.
        card.layer.render.viewport_col = 18;
        card.layer.render.viewport_row = 2;
        app.sidebar_card_layers = vec![card];

        let placements = collect_visible_placements(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
            &HashMap::new(),
        );
        let (clipped, _) = clipped_placement(&placements[0]).expect("a card part-way in");
        assert_eq!((clipped.x, clipped.y), (18, 3));
        assert_eq!(
            u32::from(clipped.x) + clipped.cols,
            u32::from(panel.x) + u32::from(panel.width),
            "a sliding card reached past the panel and over the panes"
        );
        // Cropped by cropping the *source*, so what is on screen is still the
        // card's own pixels at 1:1 rather than the whole card squeezed into
        // fewer columns.
        assert_eq!(clipped.source_x, 0);
        assert_eq!(
            clipped.source_width,
            clipped.cols * test_cell_size().width_px
        );

        // And once it is entirely off the panel there is nothing to draw at all.
        app.sidebar_card_layers[0].layer.render.viewport_col = 40;
        let placements = collect_visible_placements(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
            &HashMap::new(),
        );
        assert!(
            clipped_placement(&placements[0]).is_none(),
            "a card clear of the panel was still drawn somewhere"
        );
    }

    #[test]
    fn sidebar_layer_disappears_when_the_sidebar_has_no_width() {
        // The mobile layout leaves `sidebar_rect` empty; a stored layer must
        // then place nothing rather than land at the origin.
        let app = app_with_sidebar(
            Rect::default(),
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        );
        let placements = collect_visible_placements(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
            &HashMap::new(),
        );

        assert_eq!(placements.len(), 1, "the layer is still stored");
        assert!(
            clipped_placement(&placements[0]).is_none(),
            "but nothing is drawn"
        );
        assert!(!has_visible_pane_graphics(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
        ));
    }

    #[test]
    fn sidebar_layer_z_reaches_the_emitted_placement() {
        for z in [
            0,
            -1,
            crate::api::schema::GRAPHICS_Z_BELOW_BACKGROUND,
            i32::MIN,
        ] {
            let app = app_with_sidebar(
                Rect::new(0, 0, 26, 20),
                crate::api::schema::PaneGraphicsPlacementParams {
                    z,
                    ..Default::default()
                },
            );
            let mut cache = HostGraphicsCache::default();
            let bytes = encode_local_pane_graphics(
                &app,
                &TerminalRuntimeRegistry::new(),
                empty_surface(),
                test_cell_size(),
                &mut cache,
            );
            let emitted = String::from_utf8_lossy(&bytes).into_owned();

            assert!(emitted.contains("a=t"), "z={z} uploads the image");
            assert!(
                emitted.contains(&format!(",z={z},")),
                "z={z} reaches the placement control: {emitted:?}"
            );
        }
    }

    #[test]
    fn dropping_the_sidebar_layer_deletes_its_host_image() {
        let mut app = app_with_sidebar(
            Rect::new(0, 0, 26, 20),
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        );
        let mut cache = HostGraphicsCache::default();
        let runtimes = TerminalRuntimeRegistry::new();
        let bytes = encode_local_pane_graphics(
            &app,
            &runtimes,
            empty_surface(),
            test_cell_size(),
            &mut cache,
        );
        assert!(String::from_utf8_lossy(&bytes).contains("a=t"));
        let host_id = *cache.images.keys().next().expect("uploaded host image");

        app.surface_graphics_layers.clear();
        let bytes = encode_local_pane_graphics(
            &app,
            &runtimes,
            empty_surface(),
            test_cell_size(),
            &mut cache,
        );

        assert!(String::from_utf8_lossy(&bytes).contains(&format!("a=d,d=I,i={host_id}")));
        assert!(cache.is_empty());
    }

    /// End-to-end through the same [`encode_local_pane_graphics`] entry point the real pty write
    /// uses: the sidebar's ambient wash reaches the terminal as one `a=t` upload, its loop
    /// frames as `a=f`, playback armed with `a=a`, and a second pass with nothing changed writes
    /// nothing at all — the property `data/herdr-native-animation-playback-verify` confirmed on
    /// a real terminal.
    #[test]
    fn sidebar_particle_field_arms_playback_through_the_full_pipeline() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.sidebar_particle_field = Some(
            crate::app::state::GraphicsLayer::new(
                crate::api::schema::PaneGraphicsFormat::Rgba,
                4,
                4,
                vec![1; 4 * 4 * 4],
                crate::api::schema::PaneGraphicsPlacementParams {
                    z: -1,
                    ..Default::default()
                },
            )
            .with_animation(crate::app::state::GraphicsAnimation {
                frame_gap_ms: 100,
                frames: vec![vec![2; 4 * 4 * 4], vec![3; 4 * 4 * 4]],
            }),
        );

        let mut cache = HostGraphicsCache::default();
        let runtimes = TerminalRuntimeRegistry::new();
        let bytes = encode_local_pane_graphics(
            &app,
            &runtimes,
            empty_surface(),
            test_cell_size(),
            &mut cache,
        );
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("a=t"), "root frame uploaded");
        assert_eq!(
            text.matches("a=f").count(),
            2,
            "both loop frames transmitted"
        );
        assert!(text.contains("s=3,v=1"), "playback armed");

        let bytes = encode_local_pane_graphics(
            &app,
            &runtimes,
            empty_surface(),
            test_cell_size(),
            &mut cache,
        );
        assert!(
            bytes.is_empty(),
            "an unchanged wash writes nothing on the next pass — the terminal owns the clock"
        );
    }

    #[test]
    fn sidebar_and_pane_layers_never_share_a_host_id() {
        let signature = ImageSignature {
            image_width: 4,
            image_height: 4,
            format_code: 32,
            data_len: 64,
            data_fingerprint: 11,
        };
        let pane = HostSurfaceId::Pane(PaneId::from_raw(1));

        assert_ne!(
            layer_host_image_id(pane, signature),
            layer_host_image_id(HostSurfaceId::Sidebar, signature)
        );
        assert_ne!(
            host_placement_id(
                HostSourceKey::Layer { surface: pane },
                &test_placement(0, 0).placement,
            ),
            host_placement_id(
                HostSourceKey::Layer {
                    surface: HostSurfaceId::Sidebar,
                },
                &test_placement(0, 0).placement,
            )
        );
    }

    fn sidebar_card_layer(rect: Rect) -> crate::ui::sidebar::SidebarCardLayer {
        crate::ui::sidebar::SidebarCardLayer {
            rect,
            // A settled card clips to nothing wider than itself, which is how
            // the panel drew before motion existed and what these tests measure.
            clip: rect,
            signature: 1,
            content_signature: 1,
            undissolved: None,
            layer: crate::app::state::GraphicsLayer::new(
                crate::api::schema::PaneGraphicsFormat::Rgba,
                u32::from(rect.width) * test_cell_size().width_px,
                u32::from(rect.height) * test_cell_size().height_px,
                vec![3; 8],
                crate::api::schema::PaneGraphicsPlacementParams {
                    grid_cols: u32::from(rect.width),
                    grid_rows: u32::from(rect.height),
                    ..Default::default()
                },
            ),
        }
    }

    /// The TUI's own cards and a client's sidebar layer are two placements on
    /// one rect, not one placement fighting over it.
    ///
    /// They have different owners — `surface.graphics.set` owns one and the
    /// sidebar renderer owns the other — so a client painting a backdrop under
    /// the tree has to survive the tree drawing itself on top. Sharing a
    /// surface identity would make each one's upload delete the other's.
    #[test]
    fn the_sidebar_cards_are_a_second_placement_beside_a_client_layer() {
        let mut app = app_with_sidebar(
            Rect::new(0, 0, 26, 20),
            crate::api::schema::PaneGraphicsPlacementParams {
                z: crate::api::schema::GRAPHICS_Z_BELOW_BACKGROUND,
                ..Default::default()
            },
        );
        app.sidebar_card_layers = vec![sidebar_card_layer(Rect::new(1, 2, 20, 12))];
        app.view.sidebar_card_layers_published = true;

        let placements = collect_visible_placements(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
            &HashMap::new(),
        );
        assert_eq!(placements.len(), 2);

        let cards = placements
            .iter()
            .find(|placement| placement.surface == HostSurfaceId::SidebarCards(0))
            .expect("the sidebar's own cards reach the pipeline");
        assert_eq!(
            cards.source_key,
            HostSourceKey::Layer {
                surface: HostSurfaceId::SidebarCards(0),
            }
        );
        // The tree's rect, not the whole panel's: the sheet is only as large as
        // the cards it holds.
        let (clipped, _) = clipped_placement(cards).expect("the cards are visible");
        assert_eq!((clipped.x, clipped.y), (1, 2));
        assert_eq!((clipped.cols, clipped.rows), (20, 12));

        // Two surfaces, two host images, two placement ids. Either colliding
        // would make one layer's upload delete the other's.
        let client = placements
            .iter()
            .find(|placement| placement.surface == HostSurfaceId::Sidebar)
            .expect("the client's own layer is still there");
        assert_ne!(
            host_image_id(cards.surface, &cards.placement),
            host_image_id(client.surface, &client.placement)
        );
        assert_ne!(
            host_placement_id(cards.source_key, &cards.placement),
            host_placement_id(client.source_key, &client.placement)
        );
    }

    /// Chrome exists whether or not a tab does, and the retained fast path has
    /// to agree with the collector about that or it skips a repaint it owed.
    #[test]
    fn the_sidebar_cards_are_visible_without_an_active_workspace() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.active = None;
        app.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.sidebar_card_layers = vec![sidebar_card_layer(Rect::new(0, 1, 24, 10))];
        app.view.sidebar_card_layers_published = true;

        assert!(has_visible_pane_graphics(
            &app,
            &TerminalRuntimeRegistry::new(),
            empty_surface(),
            test_cell_size(),
        ));
    }

    #[test]
    fn dropping_the_sidebar_cards_deletes_their_host_image() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.sidebar_card_layers = vec![sidebar_card_layer(Rect::new(0, 1, 24, 10))];
        app.view.sidebar_card_layers_published = true;

        let mut cache = HostGraphicsCache::default();
        let runtimes = TerminalRuntimeRegistry::new();
        let bytes = encode_local_pane_graphics(
            &app,
            &runtimes,
            empty_surface(),
            test_cell_size(),
            &mut cache,
        );
        assert!(String::from_utf8_lossy(&bytes).contains("a=t"));
        let host_id = *cache.images.keys().next().expect("uploaded host image");

        // A panel that stopped drawing pixel cards — narrowed past the card
        // shell, or graphics turned off — leaves nothing behind on the host.
        app.sidebar_card_layers.clear();
        let bytes = encode_local_pane_graphics(
            &app,
            &runtimes,
            empty_surface(),
            test_cell_size(),
            &mut cache,
        );
        assert!(String::from_utf8_lossy(&bytes).contains(&format!("a=d,d=I,i={host_id}")));
        assert!(cache.is_empty());
    }

    #[test]
    fn pane_host_ids_are_unchanged_by_the_surface_generalisation() {
        // The pane path shipped first; its host ids are derived from the raw
        // pane id alone and must stay that way.
        let signature = ImageSignature {
            image_width: 4,
            image_height: 4,
            format_code: 32,
            data_len: 64,
            data_fingerprint: 11,
        };
        let pane_id = PaneId::from_raw(17);

        let mut legacy = DefaultHasher::new();
        pane_id.raw().hash(&mut legacy);
        signature.hash(&mut legacy);
        let legacy = legacy.finish();

        assert_eq!(
            host_image_id_for_signature(HostSurfaceId::Pane(pane_id), signature),
            HOST_IMAGE_ID_BASE + ((legacy as u32) % 900_000)
        );
        assert_eq!(
            layer_host_image_id(HostSurfaceId::Pane(pane_id), signature),
            PANE_GRAPHICS_IMAGE_ID_BIT | ((legacy as u32) & !PANE_GRAPHICS_IMAGE_ID_BIT)
        );
    }

    #[test]
    fn pane_graphics_layer_full_pane_grid_tracks_pane_resize() {
        let layer = crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            80,
            30,
            vec![255; 80 * 30 * 4],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        );
        let cell_size = HostCellSize {
            width_px: 10,
            height_px: 10,
        };
        let pane_id = PaneId::from_raw(9);

        let wide_rect = Rect::new(2, 1, 20, 10);
        let wide_placement = layer_host_placement(
            HostSurfaceId::Pane(pane_id),
            wide_rect,
            cell_size,
            &layer,
            &HashMap::new(),
            true,
        );
        let (wide_clipped, _) = clipped_placement(&wide_placement).expect("visible layer");
        assert_eq!(wide_clipped.cols, 20);
        assert_eq!(wide_clipped.rows, 10);

        // A resize (e.g. a split or a terminal resize) recomputes `PaneInfo`
        // fresh every frame; the placement must follow it to the new full rect
        // without any extra addressing work, matching the funding spike's
        // finding that pane-content overlays already reach whole-pane rects.
        let narrow_rect = Rect::new(2, 1, 6, 4);
        let narrow_placement = layer_host_placement(
            HostSurfaceId::Pane(pane_id),
            narrow_rect,
            cell_size,
            &layer,
            &HashMap::new(),
            true,
        );
        let (narrow_clipped, _) = clipped_placement(&narrow_placement).expect("visible layer");
        assert_eq!(narrow_clipped.cols, 6);
        assert_eq!(narrow_clipped.rows, 4);
    }

    #[test]
    fn graphics_update_uploads_once_then_repositions_only() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let first = String::from_utf8_lossy(&bytes);
        assert!(first.contains("a=t"));
        assert!(first.contains("a=p"));

        bytes.clear();
        let same = test_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[same],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert!(bytes.is_empty());

        let mut z_changed = test_placement(0, 0);
        z_changed.placement.z = 1;
        encode_graphics_update(
            &mut bytes,
            &[z_changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let z_changed_bytes = String::from_utf8_lossy(&bytes);
        assert!(!z_changed_bytes.contains("a=t"));
        assert!(z_changed_bytes.contains("a=p"));

        bytes.clear();
        let moved = test_placement(0, 1);
        encode_graphics_update(
            &mut bytes,
            &[moved],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let moved_bytes = String::from_utf8_lossy(&bytes);
        assert!(!moved_bytes.contains("a=t"));
        assert!(moved_bytes.contains("a=p"));
    }

    #[test]
    fn encode_transmit_frame_emits_a_f_with_dims_format_and_gap() {
        let mut out = Vec::new();
        encode_transmit_frame(&mut out, 42, 32, 10, 20, 600, &[1, 2, 3, 4]);
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("\x1b_Ga=f,i=42,f=32,s=10,v=20,z=600,q=2"));
    }

    /// A frame over one chunk (`KITTY_CHUNK_BYTES`) must repeat `a=f` on every continuation
    /// chunk. Confirmed live against a real terminal: without this, kitty has no way to tell a
    /// continuation chunk apart from the default `a=t` action, and a chunked frame's data never
    /// properly lands — playback advances to it once and then stalls.
    #[test]
    fn encode_transmit_frame_repeats_a_f_on_every_continuation_chunk() {
        let mut out = Vec::new();
        let data = vec![7u8; KITTY_CHUNK_BYTES * 2 + 100];
        encode_transmit_frame(&mut out, 42, 32, 100, 100, 600, &data);
        let text = String::from_utf8_lossy(&out);
        let chunks: Vec<&str> = text.split("\x1b_G").filter(|s| !s.is_empty()).collect();
        assert_eq!(chunks.len(), 3, "3072*2+100 bytes needs 3 chunks");
        assert!(chunks[0].starts_with("a=f,i=42,f=32,s=100,v=100,z=600,q=2,m=1;"));
        assert!(
            chunks[1].starts_with("m=1,a=f;"),
            "continuation chunk missing the required a=f repeat: {:?}",
            chunks[1]
        );
        assert!(
            chunks[2].starts_with("m=0,a=f;"),
            "final continuation chunk missing the required a=f repeat: {:?}",
            chunks[2]
        );
    }

    /// The plain (non-frame) upload path must NOT gain an `a=f` on its continuation chunks —
    /// only animation frame transmission needs that repeat.
    #[test]
    fn encode_upload_image_continuation_chunks_stay_bare() {
        let mut out = Vec::new();
        let mut placement = test_placement(0, 0);
        placement.placement.data = vec![7u8; KITTY_CHUNK_BYTES * 2 + 100];
        encode_upload_image(
            &mut out,
            &placement,
            kitty_format_code(placement.placement.format),
            42,
        );
        let text = String::from_utf8_lossy(&out);
        let chunks: Vec<&str> = text.split("\x1b_G").filter(|s| !s.is_empty()).collect();
        assert_eq!(chunks.len(), 3);
        assert!(chunks[1].starts_with("m=1;"), "{:?}", chunks[1]);
        assert!(chunks[2].starts_with("m=0;"), "{:?}", chunks[2]);
    }

    #[test]
    fn encode_arm_animation_sets_root_gap_before_starting_loop_playback() {
        let mut out = Vec::new();
        encode_arm_animation(&mut out, 42, 600);
        let text = String::from_utf8_lossy(&out);
        let root_gap = "\x1b_Ga=a,i=42,r=1,z=600,q=2;\x1b\\";
        let play = "\x1b_Ga=a,i=42,s=3,v=1,q=2;\x1b\\";
        assert!(
            text.starts_with(root_gap),
            "the root frame carries no gap of its own by spec, so it must be set before the \
             loop-back leg plays"
        );
        assert!(text.ends_with(play));
    }

    #[test]
    fn encode_animation_frames_is_a_noop_with_no_extra_frames() {
        let mut out = Vec::new();
        encode_animation_frames(
            &mut out,
            1,
            32,
            10,
            10,
            &crate::app::state::GraphicsAnimation {
                frame_gap_ms: 100,
                frames: Vec::new(),
            },
        );
        assert!(
            out.is_empty(),
            "a layer that opted into animation but has no extra frames must not arm playback \
             it never uploaded frames for"
        );
    }

    /// The same property proved on a real terminal in
    /// `data/herdr-native-animation-playback-verify`: one upload transmits every loop frame and
    /// arms playback, and a second pass over an unchanged animated layer produces zero further
    /// bytes — from there the terminal's own clock is what advances frames, not another Herdr
    /// write.
    #[test]
    fn animated_layer_transmits_frames_and_arms_playback_exactly_once() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        fn animation() -> crate::app::state::GraphicsAnimation {
            // Under one KITTY_CHUNK_BYTES chunk each, so "one a=f per frame" holds exactly —
            // chunking's own "a=f" repeat-per-continuation-chunk behavior has its own dedicated
            // test (`encode_transmit_frame_repeats_a_f_on_every_continuation_chunk`).
            crate::app::state::GraphicsAnimation {
                frame_gap_ms: 100,
                frames: vec![vec![9; 20 * 20 * 4], vec![10; 20 * 20 * 4]],
            }
        }

        let mut placement = test_placement(0, 0);
        placement.animation = Some(animation());

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let first = String::from_utf8_lossy(&bytes);
        assert!(
            first.contains("a=t"),
            "the root frame still uploads via the existing path"
        );
        assert_eq!(
            first.matches("a=f").count(),
            2,
            "one a=f per extra loop frame"
        );
        assert!(
            first.contains("a=a"),
            "playback armed after the frames land"
        );
        assert!(
            first.contains("s=3,v=1"),
            "loop mode armed, not just the root gap set"
        );

        bytes.clear();
        let mut same = test_placement(0, 0);
        same.animation = Some(animation());
        encode_graphics_update(
            &mut bytes,
            &[same],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert!(
            bytes.is_empty(),
            "an unchanged animated layer re-arms nothing on a later pass"
        );
    }

    #[test]
    fn view_change_redisplays_unchanged_visible_placement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(placements.len(), 1);

        bytes.clear();
        let same = test_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[same],
            true,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(!redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn surface_reset_deletes_then_reuploads_and_redisplays_placement() {
        let mut cache = HostGraphicsCache::default();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.placements.len(), 1);

        bytes = cache.clear_bytes();
        let same = test_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[same],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );

        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(redisplay.contains("a=d,d=I"));
        assert!(redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.placements.len(), 1);
    }

    #[test]
    fn scrollback_offset_change_redisplays_placement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        bytes.clear();
        let mut scrolled = test_placement(0, 0);
        scrolled.scrollback_offset = 3;
        encode_graphics_update(
            &mut bytes,
            &[scrolled],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(!redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
    }

    #[test]
    fn empty_image_data_does_not_mark_image_uploaded() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let mut placement = test_placement(0, 0);
        placement.placement.data.clear();

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        assert!(bytes.is_empty());
        assert!(images.is_empty());
        assert!(placements.is_empty());
    }

    #[test]
    fn same_image_signature_reuses_host_upload_across_source_image_ids() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);

        bytes.clear();
        let mut same_image_new_source_id = test_placement(0, 0);
        same_image_new_source_id.placement.image_id = 8;
        same_image_new_source_id.placement.placement_id = 4;
        same_image_new_source_id.placement.data.clear();
        encode_graphics_update(
            &mut bytes,
            &[same_image_new_source_id],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let reused = String::from_utf8_lossy(&bytes);
        assert!(!reused.contains("a=t"));
        assert!(reused.contains("a=p"));
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn replaced_image_content_deletes_superseded_host_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        let superseded_host_id = *images.keys().next().expect("uploaded host image");

        // Same source image id, new pixel content: the fresh content maps to
        // a fresh host image id, so the replaced one must be deleted.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains("a=t"), "changed content re-uploads");
        assert!(
            update.contains(&format!("a=d,d=I,i={superseded_host_id}")),
            "superseded host image is deleted"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn shared_host_image_survives_while_another_source_references_it() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin.source_key = HostSourceKey::Terminal {
                pane_id: PaneId::from_raw(1),
                image_id: twin.placement.image_id,
            };
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1, "same content dedups to one host image");

        // One source moves to new content while the other still shows the
        // old image: the shared host image must survive.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed, twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(!update.contains("a=d,d=I"), "shared host image survives");
        assert_eq!(images.len(), 2);
    }

    #[test]
    fn stale_source_entry_does_not_block_superseded_image_delete() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin.source_key = HostSourceKey::Terminal {
                pane_id: PaneId::from_raw(1),
                image_id: twin.placement.image_id,
            };
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 2);
        let shared_host_id = *images.keys().next().expect("uploaded host image");

        // The twin source is gone and the survivor changed content: the
        // vanished source's stale entry must not keep the old host image
        // alive.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            update.contains(&format!("a=d,d=I,i={shared_host_id}")),
            "old host image is deleted once its last live source moves on"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn stale_placement_deletes_placement_not_image_immediately() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(placements.len(), 1);

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains("a=d,d=i"));
        assert!(!delete.contains("d=I"));
        assert!(placements.is_empty());
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn removed_pane_layer_deletes_unreferenced_host_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(0, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let host_id = *images.keys().next().expect("uploaded pane layer");

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains(&format!("a=d,d=I,i={host_id}")));
        assert!(images.is_empty());
        assert!(placements.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn clipped_pane_layer_deletes_unreferenced_host_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(0, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let host_id = *images.keys().next().expect("uploaded pane layer");

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(100, 100)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains(&format!("a=d,d=I,i={host_id}")));
        assert!(images.is_empty());
        assert!(placements.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn clipped_terminal_source_retains_identity_for_later_content_replacement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let original_host_id = *images.keys().next().expect("uploaded terminal image");

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(100, 100)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);

        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains(&format!("a=d,d=I,i={original_host_id}")));
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn removed_pane_layer_preserves_image_shared_with_terminal_source() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[pane_layer_placement(0, 0), test_placement(4, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(4, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(!update.contains("a=d,d=I"));
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn maximum_pane_graphics_stream_payload_fits_client_graphics_frame() {
        let mut placement = pane_layer_placement(0, 0);
        placement.placement.format = KittyImageFormat::Png;
        placement.placement.image_width = 1;
        placement.placement.image_height = 1;
        placement.placement.data = vec![1_u8; crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES];
        placement.placement.data_len = placement.placement.data.len();
        let (clipped, format_code) = clipped_placement(&placement).expect("visible placement");
        let host_id = host_image_id(placement.surface, &placement.placement);
        let mut encoded = Vec::new();

        assert!(encode_upload_image(
            &mut encoded,
            &placement,
            format_code,
            host_id,
        ));
        encode_display_placement(&mut encoded, clipped, host_id, 1, 0);

        assert!(encoded.len() < crate::protocol::MAX_GRAPHICS_FRAME_SIZE);
    }

    #[test]
    fn view_change_deletes_stale_placement_immediately() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            true,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains("a=d,d=i"));
        assert!(placements.is_empty());
    }
}

#[cfg(test)]
mod local_transport_tests {
    //! Behavioural coverage for the local-transport/pixel-format feature.
    //!
    //! The detection functions under test are pure (they take their inputs
    //! as parameters rather than reading `std::env`), so most of this module
    //! never touches process environment or global state and is safe under
    //! any test ordering or parallelism. The handful of tests that do
    //! exercise `encode_upload_image`'s real gating — `LOCAL_GRAPHICS_TRANSPORT_ENABLED`
    //! and the real SSH_* environment — mutate process-wide state and are
    //! only guaranteed correct under `cargo test -- --test-threads=1`,
    //! matching the rest of this project's convention for global/env-backed
    //! state.
    use super::*;

    // ---- host_terminal_kind_for_env ----------------------------------

    #[test]
    fn identifies_rio_from_term_program_case_insensitively() {
        assert_eq!(
            host_terminal_kind_for_env(Some("rio"), None, false),
            HostTerminalKind::Rio
        );
        assert_eq!(
            host_terminal_kind_for_env(Some("Rio"), None, false),
            HostTerminalKind::Rio
        );
    }

    #[test]
    fn identifies_kitty_from_window_id_or_term() {
        assert_eq!(
            host_terminal_kind_for_env(None, None, true),
            HostTerminalKind::Kitty
        );
        assert_eq!(
            host_terminal_kind_for_env(None, Some("xterm-kitty"), false),
            HostTerminalKind::Kitty
        );
    }

    #[test]
    fn rio_term_program_wins_over_a_stray_kitty_marker() {
        assert_eq!(
            host_terminal_kind_for_env(Some("rio"), Some("xterm-kitty"), true),
            HostTerminalKind::Rio
        );
    }

    #[test]
    fn unidentified_terminals_are_other() {
        assert_eq!(
            host_terminal_kind_for_env(None, None, false),
            HostTerminalKind::Other
        );
        assert_eq!(
            host_terminal_kind_for_env(Some("WezTerm"), Some("xterm-256color"), false),
            HostTerminalKind::Other
        );
    }

    // ---- host_graphics_locality_for_env ------------------------------

    #[test]
    fn locality_is_established_only_with_no_ssh_markers() {
        assert!(host_graphics_locality_for_env(false, false, false));
        assert!(!host_graphics_locality_for_env(true, false, false));
        assert!(!host_graphics_locality_for_env(false, true, false));
        assert!(!host_graphics_locality_for_env(false, false, true));
        assert!(!host_graphics_locality_for_env(true, true, true));
    }

    // ---- preferred_local_pixel_format --------------------------------

    #[test]
    fn preferred_local_pixel_format_matches_measured_terminals() {
        assert_eq!(
            preferred_local_pixel_format(HostTerminalKind::Rio),
            Some(KittyImageFormat::Rgba)
        );
        assert_eq!(
            preferred_local_pixel_format(HostTerminalKind::Kitty),
            Some(KittyImageFormat::Rgb)
        );
        assert_eq!(preferred_local_pixel_format(HostTerminalKind::Other), None);
    }

    // ---- preferred_card_pixel_format_for ------------------------------

    #[test]
    fn card_format_stays_png_when_local_transport_disabled() {
        assert_eq!(
            preferred_card_pixel_format_for(false, true, HostTerminalKind::Rio, true),
            crate::api::schema::PaneGraphicsFormat::Png
        );
    }

    #[test]
    fn card_format_stays_png_when_locality_not_established() {
        assert_eq!(
            preferred_card_pixel_format_for(true, false, HostTerminalKind::Rio, true),
            crate::api::schema::PaneGraphicsFormat::Png
        );
    }

    #[test]
    fn card_format_upgrades_rio_to_rgba_regardless_of_opacity() {
        assert_eq!(
            preferred_card_pixel_format_for(true, true, HostTerminalKind::Rio, false),
            crate::api::schema::PaneGraphicsFormat::Rgba
        );
        assert_eq!(
            preferred_card_pixel_format_for(true, true, HostTerminalKind::Rio, true),
            crate::api::schema::PaneGraphicsFormat::Rgba
        );
    }

    #[test]
    fn card_format_upgrades_kitty_to_rgb_only_when_opaque() {
        assert_eq!(
            preferred_card_pixel_format_for(true, true, HostTerminalKind::Kitty, true),
            crate::api::schema::PaneGraphicsFormat::Rgb
        );
        assert_eq!(
            preferred_card_pixel_format_for(true, true, HostTerminalKind::Kitty, false),
            crate::api::schema::PaneGraphicsFormat::Png,
            "a translucent card handed to f=24 would clip every soft edge to opaque"
        );
    }

    #[test]
    fn card_format_is_the_documented_safe_default_for_an_unknown_terminal() {
        assert_eq!(
            preferred_card_pixel_format_for(true, true, HostTerminalKind::Other, true),
            crate::api::schema::PaneGraphicsFormat::Png
        );
    }

    // ---- local file staging lifecycle --------------------------------

    #[test]
    fn local_graphics_file_round_trips_and_cleans_up() {
        let host_id = 555_101;
        let data = vec![1u8, 2, 3, 4, 5];
        let path = write_local_graphics_file(host_id, &data).expect("write");
        assert_eq!(std::fs::read(&path).unwrap(), data);

        let tmp_path = local_graphics_dir().join(format!("{host_id}.tmp"));
        assert!(
            !tmp_path.exists(),
            "the write-then-rename step left a .tmp behind"
        );

        remove_local_graphics_file(host_id);
        assert!(
            !path.exists(),
            "remove_local_graphics_file left the file behind"
        );
    }

    #[test]
    fn overwriting_a_host_id_replaces_content_via_atomic_rename() {
        let host_id = 555_102;
        write_local_graphics_file(host_id, &[1, 2, 3]).unwrap();
        let path = write_local_graphics_file(host_id, &[9, 9]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), vec![9, 9]);
        remove_local_graphics_file(host_id);
    }

    #[test]
    fn deleting_an_image_removes_its_staged_local_file() {
        let host_id = 555_103;
        write_local_graphics_file(host_id, &[7]).unwrap();
        let path = local_graphics_path(host_id);
        assert!(path.exists());

        let mut bytes = Vec::new();
        encode_delete_image(&mut bytes, host_id);
        assert!(
            !path.exists(),
            "encode_delete_image must remove the staged file, not just emit a=d"
        );
    }

    #[test]
    fn deleting_an_id_with_no_staged_file_does_not_error() {
        // Best-effort: most deletes are for images that were never
        // local-transported at all (t=d fallback, or the feature disabled).
        let mut bytes = Vec::new();
        encode_delete_image(&mut bytes, 555_104);
        assert!(String::from_utf8_lossy(&bytes).contains("a=d,d=I"));
    }

    #[test]
    fn cleanup_removes_the_whole_directory_and_future_writes_still_work() {
        let host_id = 555_105;
        let path = write_local_graphics_file(host_id, &[7]).unwrap();
        assert!(path.exists());

        cleanup_local_graphics_dir();
        assert!(!path.exists());
        assert!(!local_graphics_dir().exists());

        // A later frame must still be able to stage a file: cleanup must not
        // permanently poison the directory for the rest of the process.
        let path_again = write_local_graphics_file(host_id, &[8]).unwrap();
        assert!(path_again.exists());
        remove_local_graphics_file(host_id);
    }

    // ---- encode_upload_image_via_file's control string ----------------

    #[test]
    fn file_transport_control_string_is_a_single_unchunked_sequence() {
        let mut bytes = Vec::new();
        let path = std::path::Path::new("/tmp/herdr-kitty-graphics-1/42.kitty");
        encode_upload_image_via_file(&mut bytes, path, 32, 10, 20, 42);
        let encoded = String::from_utf8_lossy(&bytes);

        assert!(encoded.starts_with("\x1b_Ga=t,t=f,f=32,s=10,v=20,i=42,q=2,m=0;"));
        assert!(encoded.ends_with("\x1b\\"));
        assert_eq!(
            encoded.matches("\x1b_G").count(),
            1,
            "must be exactly one escape sequence, never chunked like t=d payloads"
        );
        assert!(
            !encoded.contains(",m=1"),
            "a base64-encoded path is always small; KITTY_CHUNK_BYTES must not apply here"
        );

        let expected_path_b64 =
            base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes());
        assert!(encoded.contains(&expected_path_b64));
    }

    // ---- encode_upload_image's transport gating ------------------------
    //
    // These mutate `LOCAL_GRAPHICS_TRANSPORT_ENABLED` and real SSH_* env
    // vars, so they are only guaranteed correct under `--test-threads=1` —
    // see the module doc comment.

    fn restore_env_var(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    struct LocalTransportEnvGuard {
        previous_flag: bool,
        ssh_tty: Option<std::ffi::OsString>,
        ssh_connection: Option<std::ffi::OsString>,
        ssh_client: Option<std::ffi::OsString>,
    }

    impl LocalTransportEnvGuard {
        fn capture(enabled: bool) -> Self {
            let guard = Self {
                previous_flag: local_transport_enabled(),
                ssh_tty: std::env::var_os("SSH_TTY"),
                ssh_connection: std::env::var_os("SSH_CONNECTION"),
                ssh_client: std::env::var_os("SSH_CLIENT"),
            };
            set_local_transport_enabled(enabled);
            guard
        }

        fn local() -> Self {
            let guard = Self::capture(true);
            std::env::remove_var("SSH_TTY");
            std::env::remove_var("SSH_CONNECTION");
            std::env::remove_var("SSH_CLIENT");
            guard
        }

        fn remote_via_ssh() -> Self {
            let guard = Self::capture(true);
            std::env::set_var("SSH_TTY", "/dev/pts/9");
            guard
        }
    }

    impl Drop for LocalTransportEnvGuard {
        fn drop(&mut self) {
            set_local_transport_enabled(self.previous_flag);
            restore_env_var("SSH_TTY", self.ssh_tty.take());
            restore_env_var("SSH_CONNECTION", self.ssh_connection.take());
            restore_env_var("SSH_CLIENT", self.ssh_client.take());
        }
    }

    #[test]
    fn upload_uses_local_file_transport_when_enabled_and_local() {
        let _guard = LocalTransportEnvGuard::local();
        let placement = tests::test_placement(0, 0);
        let mut bytes = Vec::new();
        let host_id = 555_201;

        assert!(encode_upload_image(&mut bytes, &placement, 32, host_id));
        let encoded = String::from_utf8_lossy(&bytes);
        assert!(
            encoded.contains("t=f"),
            "expected file transport: {encoded}"
        );
        assert!(!encoded.contains("t=d"));
        assert!(local_graphics_path(host_id).exists());

        remove_local_graphics_file(host_id);
    }

    #[test]
    fn upload_keeps_direct_transport_when_locality_not_established() {
        let _guard = LocalTransportEnvGuard::remote_via_ssh();
        let placement = tests::test_placement(0, 0);
        let mut bytes = Vec::new();
        let host_id = 555_202;

        assert!(encode_upload_image(&mut bytes, &placement, 32, host_id));
        let encoded = String::from_utf8_lossy(&bytes);
        assert!(encoded.contains("t=d"));
        assert!(!encoded.contains("t=f"));
        assert!(
            !local_graphics_path(host_id).exists(),
            "no file should be staged when locality is not established"
        );
    }

    #[test]
    fn upload_keeps_direct_transport_when_feature_flag_is_off() {
        let _guard = LocalTransportEnvGuard::local();
        set_local_transport_enabled(false);
        let placement = tests::test_placement(0, 0);
        let mut bytes = Vec::new();
        let host_id = 555_203;

        assert!(encode_upload_image(&mut bytes, &placement, 32, host_id));
        let encoded = String::from_utf8_lossy(&bytes);
        assert!(
            encoded.contains("t=d"),
            "t=d must stay the default until the experimental flag is turned on"
        );
        assert!(!local_graphics_path(host_id).exists());
    }

    #[test]
    fn upload_of_empty_data_stages_no_file_and_reports_failure() {
        let _guard = LocalTransportEnvGuard::local();
        let mut placement = tests::test_placement(0, 0);
        placement.placement.data.clear();
        let mut bytes = Vec::new();
        let host_id = 555_204;

        assert!(!encode_upload_image(&mut bytes, &placement, 32, host_id));
        assert!(bytes.is_empty());
        assert!(!local_graphics_path(host_id).exists());
    }
}
