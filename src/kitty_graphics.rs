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

/// The query that asks the host terminal how big one cell is, in pixels.
///
/// `CSI 16 t` is answered with `CSI 6 ; height ; width t`. It exists because the
/// other way of getting this number — dividing the pty's `ws_xpixel`/`ws_ypixel`
/// by its column and row count — is only as good as those two fields, and they
/// are routinely wrong: Windows has no pixel size to report at all, and an SSH
/// session carries whatever the client sent at pty-request time, which for
/// several common Windows clients is a constant that never tracks the window.
/// A constant `ws_xpixel` divided by a growing column count yields a cell that
/// *shrinks as the window grows*, which is exactly the failure this query
/// exists to end. Every terminal that implements Kitty graphics — the only
/// terminal this number is asked for on — answers this.
pub(crate) const HOST_CELL_SIZE_QUERY_SEQUENCE: &str = "\x1b[16t";

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

    /// [`Self::for_area`] for a cell that arrived from outside this module.
    ///
    /// A zero-sized area means there is nothing to place an image on, and every
    /// cell size handed to the graphics path has to answer that the same way or
    /// `is_known` starts disagreeing with itself depending on where the number
    /// came from.
    pub(crate) fn for_area_public(self, area: Rect) -> Self {
        self.for_area(area)
    }
}

/// Reads `CSI 6 ; height ; width t` — the terminal's answer to
/// [`HOST_CELL_SIZE_QUERY_SEQUENCE`].
///
/// Height comes first in the reply, which is the opposite of every other pair
/// in this file and the reason this parse is a named function rather than a
/// couple of `split` calls at the call site.
pub(crate) fn parse_host_cell_size_report(sequence: &str) -> Option<HostCellSize> {
    let body = sequence.strip_prefix("\x1b[")?.strip_suffix('t')?;
    let mut parts = body.split(';');
    if parts.next()? != "6" {
        return None;
    }
    let height_px: u32 = parts.next()?.parse().ok()?;
    let width_px: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let reported = HostCellSize {
        width_px,
        height_px,
    };
    reported.is_plausible().then_some(reported)
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
            }
            None => {
                if !encode_upload_image(bytes, placement, format_code, host_id) {
                    continue;
                }
                host_images.insert(host_id, image_signature);
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
        // Gated on the pass that is about to be encoded having built them, and
        // not on this client having a cell size of its own. The layers are the
        // foreground client's: a pass that left them alone laid its rows out
        // without them and drew its character cards, so sending it the images
        // would stand a transparent shape over a border, a chip and a title a
        // few pixels off — the doubling `image_card::shape_covers_row` exists
        // to prevent, arrived at from the other side. Only the cards are
        // withheld; every other surface this client is entitled to still flows.
        .chain(
            app.view
                .sidebar_card_layers_published
                .then_some(app.sidebar_card_layers.as_slice())
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(slot, cards)| {
                    (
                        HostSurfaceId::SidebarCards(slot.try_into().unwrap_or(u16::MAX)),
                        cards.rect,
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
    let data = if !include_data || uploaded_images.get(&host_id).copied() == Some(signature) {
        Vec::new()
    } else {
        layer.data.clone()
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

    let control = format!(
        "a=t,t=d,f={format_code},s={},v={},i={host_id},q=2",
        placement.placement.image_width, placement.placement.image_height,
    );
    encode_kitty_data(out, &control, &placement.placement.data);
    true
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

fn encode_kitty_data(out: &mut Vec<u8>, control: &str, data: &[u8]) {
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
        let _ = write!(out, "\x1b_Gm={more};{encoded}\x1b\\");
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

    /// `CSI 16 t` is answered height-first, which is the opposite of every
    /// other pair in this file.
    #[test]
    fn the_cell_size_report_is_read_height_first() {
        assert_eq!(
            parse_host_cell_size_report("\x1b[6;24;12t"),
            Some(HostCellSize {
                width_px: 12,
                height_px: 24,
            })
        );
        // Not a cell size report.
        assert_eq!(parse_host_cell_size_report("\x1b[4;24;12t"), None);
        assert_eq!(parse_host_cell_size_report("\x1b[6;24t"), None);
        assert_eq!(parse_host_cell_size_report("\x1b[6;24;12;9t"), None);
        assert_eq!(parse_host_cell_size_report("\x1b[6;24;12"), None);
        // A reply that parses but describes no real cell is still refused, so a
        // terminal answering nonsense is the same as one not answering.
        assert_eq!(parse_host_cell_size_report("\x1b[6;8;2t"), None);
    }

    /// The query has to be the sequence terminals answer, byte for byte.
    #[test]
    fn the_query_is_csi_16_t() {
        assert_eq!(HOST_CELL_SIZE_QUERY_SEQUENCE, "\u{1b}[16t");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
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
