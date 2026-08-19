use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Span,
    Frame,
};

pub(crate) mod color;
mod dialogs;
mod diff_pane;
mod keybind_help;
mod menus;
mod mobile;
mod navigator;
mod onboarding;
mod panes;
mod release_notes;
mod scrollbar;
mod settings;
pub(crate) mod sidebar;
pub(crate) mod signal_tray_popup;
pub(crate) mod status;
mod tab_surface;
mod tabs;
mod text;
mod widgets;
mod worker_summary;

use self::dialogs::{
    render_confirm_close_overlay, render_new_linked_worktree_overlay,
    render_open_existing_worktree_overlay, render_remove_worktree_overlay, render_rename_overlay,
};
use self::diff_pane::{render_diff_popup_overlay, render_diff_zone};
use self::keybind_help::render_keybind_help_overlay;
use self::menus::{
    render_context_menu, render_copy_mode_overlay, render_global_launcher_menu,
    render_navigate_overlay, render_prefix_overlay, render_resize_overlay,
};
use self::mobile::{
    compute_mobile_header_hit_areas, is_mobile_width, mobile_switcher_max_scroll_for_height,
    mobile_toast_banner_rect, render_mobile_header, render_mobile_panel,
    render_mobile_toast_banner,
};
use self::navigator::render_navigator_overlay;
pub(crate) use self::onboarding::onboarding_welcome_continue_rect;
use self::onboarding::render_onboarding_overlay;
pub(crate) use self::panes::popup_pane_rects;
use self::panes::{render_empty, render_popup_pane, resize_popup_pane};
pub(crate) use self::release_notes::{
    product_announcement_display_lines, release_notes_close_button_rect,
    release_notes_display_lines, release_notes_wrapped_line_count, PRODUCT_ANNOUNCEMENT_MODAL_SIZE,
    RELEASE_NOTES_MODAL_SIZE,
};
use self::release_notes::{render_product_announcement_overlay, render_release_notes_overlay};
pub(crate) use self::scrollbar::{
    pane_scrollbar_rect, release_notes_scrollbar_rect, scrollbar_offset_from_drag_row,
    scrollbar_offset_from_row, scrollbar_thumb_grab_offset, should_show_scrollbar,
};
use self::settings::render_settings_overlay;
#[cfg(test)]
pub(crate) use self::sidebar::workspace_drop_indicator_row;
use self::sidebar::{render_sidebar, render_sidebar_collapsed};
use self::status::{
    config_diagnostic_rows, copy_feedback_rect, render_config_diagnostic, render_copy_feedback,
    render_pane_size_pin, render_toast_notification, toast_notification_rect,
};
pub(crate) use self::tab_surface::{
    compute_tab_surface, render_tab_surface, resize_tab_surface, TabSurfaceLayout,
};
use self::tabs::render_tab_bar;
use self::worker_summary::render_worker_summaries_overlay;
pub(crate) use self::{
    dialogs::{
        confirm_close_button_rects, confirm_close_popup_rect, new_linked_worktree_button_rects,
        new_linked_worktree_inner_rect, open_existing_worktree_button_rects,
        open_existing_worktree_inner_rect, open_existing_worktree_max_visible_rows,
        open_existing_worktree_visible_start, remove_worktree_button_rects,
        remove_worktree_popup_rect, rename_button_rects,
    },
    settings::{
        settings_button_rects, settings_popup_height, settings_show_primary_action,
        SETTINGS_POPUP_WIDTH,
    },
    sidebar::{
        agent_panel_entries, all_agent_panel_entries, collapsed_sidebar_sections,
        collapsed_sidebar_toggle_rect, compute_workspace_card_areas, expanded_sidebar_toggle_rect,
        normalized_workspace_scroll, rows_with_departing, sidebar_agent_entries,
        sidebar_agent_live_entries, sidebar_agent_row_members, sidebar_space_row_members,
        sidebar_tree_breadcrumb_rect, sidebar_tree_handle, sidebar_trunk_segment_members,
        worker_summary_badge, worker_summary_badge_rect, workspace_drop_slots,
        workspace_group_chevron_rect, workspace_list_entries, workspace_list_entries_expanded,
        workspace_list_entries_whole_fleet, workspace_list_rect, workspace_list_scroll_metrics,
        workspace_list_scrollbar_rect, workspace_parent_group_state, AgentPanelEntry,
        WorkspaceListEntry,
    },
    worker_summary::{
        worker_summaries_action_row, worker_summaries_close_button_rect,
        worker_summaries_inner_rect, worker_summaries_popup_rect, worker_summaries_total_rows,
        worker_summaries_visible_rows,
    },
};

/// The sidebar's one content column. Production code reaches it from inside
/// `sidebar`; the layout tests assert against it directly.
#[cfg(test)]
pub(crate) use self::sidebar::sidebar_content_rect;

/// The notification tray's geometry, its badge artwork, and the hit tests over
/// it. Named `signal_tray_*` outside the sidebar module because "tray" alone
/// says nothing about which surface it belongs to.
pub(crate) use self::sidebar::tray::{
    active as signal_tray_active, badge_at as signal_tray_badge_at,
    build_scene as build_signal_tray_scene, decode_scene as decode_signal_tray_scene,
    encode_scene as encode_signal_tray_scene, graphics_layer as signal_tray_graphics_layer,
    image as signal_tray_image, menu_at as signal_tray_menu_at,
    motion_fingerprint as signal_tray_motion_fingerprint,
    rasterise_scene as rasterise_signal_tray_scene, TrayScene,
};

/// Where the tray's badge artwork is composited.
///
/// Resolved from the live sidebar rect on every pass, so a divider drag moves
/// the image with the badges rather than leaving it behind.
pub(crate) fn signal_tray_graphics_rect(
    app: &crate::app::state::AppState,
) -> ratatui::layout::Rect {
    let area = self::sidebar::sidebar_content_rect(app.view.sidebar_rect);
    self::sidebar::tray::grid_rect(self::sidebar::tray::tray_rect(app, area))
}

/// Where the sidebar's ambient particle-field wash is composited: the same content column
/// [`self::sidebar::particle_background::image`] generated pixels for, so the placement's grid
/// always matches the image it is displaying.
pub(crate) fn sidebar_particle_field_rect(
    app: &crate::app::state::AppState,
) -> ratatui::layout::Rect {
    self::sidebar::sidebar_content_rect(app.view.sidebar_rect)
}

pub(crate) use self::{
    keybind_help::keybind_help_lines,
    mobile::{
        mobile_switcher_areas, mobile_switcher_max_scroll, mobile_switcher_target_at,
        mobile_switcher_workspace_doc_range, MobileSwitcherTarget,
    },
    panes::{apply_pane_chrome, pane_inner_rect, pane_is_scrolled_back},
    tab_surface::{tab_surface_cursor, tab_surface_hyperlinks, TabSurfaceView},
    tabs::{compute_tab_bar_view, TabLabelDecor},
    widgets::{centered_popup_rect, modal_stack_areas},
};
use crate::app::state::ViewLayout;
use crate::app::{AppState, Mode};
use crate::terminal::TerminalRuntimeRegistry;

const COLLAPSED_WIDTH: u16 = 4; // num + space + dot + separator

/// The diff zone's own width when shown. Fixed rather than user-tunable for
/// v1 — `AppState::diff_zone_width_threshold` is the tunable knob (whether the
/// zone shows at all); see `data/herdr-diff-pane-scoping-20260818/report.md` §D.
const DIFF_ZONE_WIDTH: u16 = 100;

/// Compute view geometry and reconcile pane sizes.
/// Called before render to separate mutation from drawing.
#[cfg_attr(not(test), allow(dead_code))]
pub fn compute_view(app: &mut AppState, area: Rect) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    compute_view_with_runtime_registry(app, &terminal_runtimes, area);
}

pub fn compute_view_with_runtime_registry(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

pub fn compute_view_with_cell_size(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_internal(app, terminal_runtimes, area, true, cell_size);
}

/// Compute view geometry for a client-sized render without resizing pane runtimes.
///
/// This is used by the headless server when a non-foreground client needs its
/// own frame size while the shared pane runtimes stay pinned to the foreground
/// client.
pub(crate) fn compute_view_without_resizing_panes(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        false,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

fn resize_background_tab_panes_to_area(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    terminal_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        for (tab_idx, tab) in ws.tabs.iter().enumerate() {
            if app.active == Some(ws_idx) && tab_idx == ws.active_tab_index() {
                continue;
            }
            resize_tab_surface(app, terminal_runtimes, tab, terminal_area, cell_size);
        }
    }
}

fn resize_background_tab_panes_for_desktop(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    main_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let (_, terminal_area) = desktop_tab_bar_and_terminal_area(app, ws, main_area);
        for (tab_idx, tab) in ws.tabs.iter().enumerate() {
            if app.active == Some(ws_idx) && tab_idx == ws.active_tab_index() {
                continue;
            }
            resize_tab_surface(app, terminal_runtimes, tab, terminal_area, cell_size);
        }
    }
}

fn desktop_tab_bar_and_terminal_area(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    main_area: Rect,
) -> (Rect, Rect) {
    let hide_single_tab_bar = app.hide_tab_bar_when_single_tab && ws.tabs.len() == 1;
    if !hide_single_tab_bar && main_area.height > 1 {
        match app.tab_bar_position {
            crate::config::TabBarPositionConfig::Top => {
                let [tab_bar_rect, terminal_area] =
                    Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main_area);
                (tab_bar_rect, terminal_area)
            }
            crate::config::TabBarPositionConfig::Bottom => {
                let [terminal_area, tab_bar_rect] =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(main_area);
                (tab_bar_rect, terminal_area)
            }
        }
    } else {
        (Rect::default(), main_area)
    }
}

/// Rebuild the sidebar's cards when their content moved, and drop them when the
/// pixel path is not live.
///
/// They are left exactly as they are when nothing changed:
/// [`sidebar::image_card::build_cards`] hashes the cards it would draw and
/// returns `Unchanged` when that hash matches what is already held, so a fleet
/// whose cards change about once every ninety seconds rasterises about that
/// often rather than on every frame.
///
/// Returns whether a card is going to be drawn over each of these rows, which
/// is what `ViewState::sidebar_card_layers_published` carries to both halves of
/// the pixel path. A pass that leaves the foreground client's cards alone
/// answers `false`: the artwork exists, but not for this pass, so that pass
/// keeps drawing its character cards and is sent none of the images.
///
/// "A card is coming" and "this pass drew one" are the same answer everywhere
/// except when every attached viewer rasterises its own cards, where the cards
/// are coming and none of them are drawn here — see
/// [`sidebar::image_card::CardsUpdate::Delegated`].
///
/// It also stamps each row's resolved motion offset back onto `cards`, so the
/// character renderer draws the tree's connectors at the cells the placement
/// actually used rather than deriving a second answer — see
/// `crate::app::state::WorkspaceCardArea::motion_cells`.
/// Whether this pass's overlay reaches any row the tree would draw a pixel card
/// over.
///
/// The test is against each row's own card frame rather than against the
/// sidebar column, because the two menus that open *inside* the panel — the
/// global launcher and a Space's context menu — normally sit well below the
/// last card, and falling back to character cards for them would flip the whole
/// tree's art on every click that changed nothing about it. A frame is grown by
/// one cell on each side so a card's bloom, which reaches past its own box, is
/// counted too.
fn overlay_hides_sidebar_cards(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
) -> bool {
    let occlusion = overlay_occlusion(app);
    match occlusion {
        OverlayOcclusion::None => false,
        OverlayOcclusion::Screen => true,
        OverlayOcclusion::Panel(_) => {
            cards
                .iter()
                .filter_map(|card| card.card_frame)
                .any(|frame| {
                    occlusion.hides(Rect::new(
                        frame.x.saturating_sub(1),
                        frame.y.saturating_sub(1),
                        frame.width.saturating_add(2),
                        frame.height.saturating_add(2),
                    ))
                })
        }
    }
}

fn update_sidebar_card_layers(
    app: &mut AppState,
    cards: &mut [crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> bool {
    // The host's answer to the capability probe as well as the user's opt-in:
    // a pass that publishes layers is a pass whose character cards stand down
    // (`image_card::shape_covers_row`), so publishing them where no client will
    // be sent them draws the tree as bare connectors. See
    // `AppState::host_paints_pixel_surfaces`.
    if !app.host_paints_pixel_surfaces() {
        app.sidebar_card_layers.clear();
        return false;
    }
    if !cell_size.is_known() {
        // A pass that does not know the host's cell size cannot speak for its
        // pixels — a virtual client rendering to a buffer, or the headless
        // server sizing a background frame. Leave the foreground client's cards
        // exactly as they are rather than clearing artwork this pass has no way
        // to redraw, which would make every background frame cost the
        // foreground one a re-encode and a re-upload.
        return false;
    }
    // An overlay reaching into the tree takes the cards it covers off the host
    // terminal (`OverlayOcclusion`), and a card that is not placed must not
    // stand its character card down — that is the whole of the "rows vanish on
    // a menu click" defect. Answering `false` puts the characters back for
    // exactly the passes whose artwork the overlay is going to withhold.
    //
    // The layers are left alone rather than cleared, for the reason the cell
    // check above leaves them alone: this pass cannot redraw them, and dropping
    // them would make closing the menu cost a full re-raster of the tree.
    if overlay_hides_sidebar_cards(app, cards) {
        return false;
    }
    let build = sidebar::image_card::build_cards(
        app,
        cards,
        sidebar_area,
        cell_size,
        &app.sidebar_card_layers,
    );
    for (card, offset) in cards.iter_mut().zip(build.motion) {
        card.motion_cells = offset;
        // Read here rather than threaded out of `build_cards`, because it is a
        // pure read of the animation engine and the one place a card's whole
        // transition state is resolved must be the same place its offset is.
        card.arriving =
            sidebar::image_card::row_arrival(app, card) != sidebar::motion::ArrivalBeat::Settled;
    }
    match build.update {
        sidebar::image_card::CardsUpdate::Unchanged => {}
        sidebar::image_card::CardsUpdate::Rebuilt(layers) => app.sidebar_card_layers = layers,
        sidebar::image_card::CardsUpdate::Empty => app.sidebar_card_layers.clear(),
        // Published by somebody else. The panel holds nothing — anything held
        // here would be pixels no attached viewer is sent — but the answer is
        // still `true`, because a card *is* going to be drawn over every one of
        // these rows and the character cards underneath have to stand down for
        // it exactly as they would for artwork built here. Any artwork left over
        // from before delegation started is dropped with it, so nothing stale
        // can be placed if a viewer that draws its own cards leaves.
        sidebar::image_card::CardsUpdate::Delegated => {
            app.sidebar_card_layers.clear();
            return true;
        }
    }
    // Published, not merely intended: a build that produced nothing falls back
    // to the character cards rather than blanking the tree.
    !app.sidebar_card_layers.is_empty()
}

/// Splits `main_area` (the frame minus the sidebar) into the terminal zone's
/// content area and the diff zone, or leaves it whole with an empty diff
/// area when there is not enough remaining width for a real third zone.
///
/// Mirrors `is_mobile_width`'s shape (compare a live width to a config
/// threshold every pass) but tests `main_area.width`, not the pre-split frame
/// width: the diff fold is about whether three zones fit side by side, so it
/// must account for however much the sidebar is currently taking rather than
/// assume a fixed sidebar cost — see
/// `data/herdr-diff-pane-scoping-20260818/report.md` §D.
fn split_diff_zone(app: &AppState, main_area: Rect) -> (Rect, Rect) {
    if main_area.width < app.diff_zone_width_threshold {
        return (main_area, Rect::default());
    }
    let [content_area, diff_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(DIFF_ZONE_WIDTH)])
            .areas(main_area);
    (content_area, diff_area)
}

fn compute_view_internal(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    app.refresh_sidebar_palette();

    if is_mobile_width(area, app.mobile_width_threshold) {
        compute_mobile_view(app, terminal_runtimes, area, resize_panes, cell_size);
        return;
    }

    let sidebar_w = if app.sidebar_collapsed {
        match app.sidebar_collapsed_mode {
            crate::config::SidebarCollapsedModeConfig::Compact => COLLAPSED_WIDTH,
            crate::config::SidebarCollapsedModeConfig::Hidden => 0,
        }
    } else {
        app.sidebar_width
            .clamp(app.sidebar_min_width, app.sidebar_max_width)
    };

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(1)]).areas(area);

    let (content_area, diff_area) = split_diff_zone(app, main_area);

    let (tab_bar_rect, terminal_area) = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| desktop_tab_bar_and_terminal_area(app, ws, content_area))
        .unwrap_or((Rect::default(), content_area));

    if !app.sidebar_collapsed {
        app.workspace_scroll = normalized_workspace_scroll(app, sidebar_area, app.workspace_scroll);
    } else {
        app.workspace_scroll = app
            .workspace_scroll
            .min(app.workspaces.len().saturating_sub(1));
    }

    let mut workspace_card_areas = if app.sidebar_collapsed {
        Vec::new()
    } else {
        compute_workspace_card_areas(app, sidebar_area)
    };
    // The tree's cards, rasterised over the cells the layout just gave them.
    // Here rather than in `render` because it is a mutation, and after the card
    // areas because it draws into exactly those rects: the pixel card never
    // decides its own geometry.
    let sidebar_card_layers_published =
        update_sidebar_card_layers(app, &mut workspace_card_areas, sidebar_area, cell_size);

    // A collapsed sidebar draws no tree and so has nothing to explain; asking
    // anyway would walk every pane of every Space for a row that is not there.
    let sidebar_view_hidden = if app.sidebar_collapsed {
        crate::app::agent_view::AgentViewHidden::default()
    } else {
        sidebar::agent_view_hidden(app)
    };

    let tab_label_decor = TabLabelDecor::from_state(app);
    let tab_bar_view = app
        .active
        .and_then(|ws_idx| app.workspaces.get(ws_idx))
        .map(|ws| {
            compute_tab_bar_view(
                ws,
                tab_bar_rect,
                app.tab_scroll,
                app.tab_scroll_follow_active,
                app.mouse_capture,
                tab_label_decor,
            )
        })
        .unwrap_or_default();
    app.tab_scroll = tab_bar_view.scroll;

    let TabSurfaceLayout {
        pane_infos,
        split_borders,
    } = compute_tab_surface(
        app,
        terminal_runtimes,
        terminal_area,
        resize_panes,
        cell_size,
    );
    if resize_panes {
        resize_background_tab_panes_for_desktop(app, terminal_runtimes, content_area, cell_size);
        resize_popup_pane(app, terminal_runtimes, terminal_area, cell_size);
    }

    let toast_hit_area = app
        .toast
        .as_ref()
        .map(|toast| {
            toast_notification_rect(
                area,
                toast,
                warning_banner_present(app),
                toast.position.unwrap_or(app.toast_config.herdr.position),
            )
        })
        .unwrap_or_default();

    app.view = crate::app::ViewState {
        layout: ViewLayout::Desktop,
        sidebar_rect: sidebar_area,
        workspace_card_areas,
        sidebar_card_layers_published,
        sidebar_view_hidden,
        tab_bar_rect,
        tab_hit_areas: tab_bar_view.tab_hit_areas,
        tab_scroll_left_hit_area: tab_bar_view.scroll_left_hit_area,
        tab_scroll_right_hit_area: tab_bar_view.scroll_right_hit_area,
        new_tab_hit_area: tab_bar_view.new_tab_hit_area,
        terminal_area,
        diff_area,
        mobile_header_rect: Rect::default(),
        mobile_menu_hit_area: Rect::default(),
        toast_hit_area,
        pane_infos,
        split_borders,
    };
    app.sync_copy_mode_search_geometry();
}

fn compute_mobile_view(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let header_h = area.height.min(2);
    let (header_rect, terminal_area) = if area.height > header_h {
        let [header_rect, terminal_area] =
            Layout::vertical([Constraint::Length(header_h), Constraint::Min(1)]).areas(area);
        (header_rect, terminal_area)
    } else {
        (area, Rect::default())
    };

    if app.mode == Mode::Navigate {
        let switcher_viewport_h = area.height.saturating_sub(header_h + 1);
        let max_scroll = mobile_switcher_max_scroll_for_height(app, switcher_viewport_h);
        app.mobile_switcher_scroll = app.mobile_switcher_scroll.min(max_scroll);
    }

    let TabSurfaceLayout {
        pane_infos,
        split_borders,
    } = compute_tab_surface(
        app,
        terminal_runtimes,
        terminal_area,
        resize_panes,
        cell_size,
    );
    if resize_panes {
        resize_background_tab_panes_to_area(app, terminal_runtimes, terminal_area, cell_size);
        resize_popup_pane(app, terminal_runtimes, terminal_area, cell_size);
    }
    let header_hits = compute_mobile_header_hit_areas(app, header_rect);

    let toast_hit_area = app
        .toast
        .as_ref()
        .map(|_| mobile_toast_banner_rect(area, warning_banner_present(app)))
        .unwrap_or_default();

    app.view = crate::app::ViewState {
        layout: ViewLayout::Mobile,
        sidebar_rect: Rect::default(),
        workspace_card_areas: Vec::new(),
        sidebar_card_layers_published: false,
        sidebar_view_hidden: Default::default(),
        tab_bar_rect: Rect::default(),
        tab_hit_areas: Vec::new(),
        tab_scroll_left_hit_area: Rect::default(),
        tab_scroll_right_hit_area: Rect::default(),
        new_tab_hit_area: Rect::default(),
        terminal_area,
        diff_area: Rect::default(),
        mobile_header_rect: header_rect,
        mobile_menu_hit_area: header_hits.menu,
        toast_hit_area,
        pane_infos,
        split_borders,
    };
    app.sync_copy_mode_search_geometry();
}

/// Render the UI — reads AppState but does not mutate it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn render(app: &AppState, frame: &mut Frame) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_with_runtime_registry(app, &terminal_runtimes, frame);
}

pub fn render_with_runtime_registry(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    let tab_bar_area = app.view.tab_bar_rect;
    let terminal_area = app.view.terminal_area;

    render_navigation_chrome(app, terminal_runtimes, frame);
    if app.view.layout != ViewLayout::Mobile {
        render_tab_bar(app, frame, tab_bar_area);
    }
    if app
        .active
        .and_then(|ws_idx| app.workspaces.get(ws_idx))
        .is_some()
    {
        render_tab_surface(app, terminal_runtimes, app.view.tab_surface(), frame);
    } else {
        render_empty(app, frame, terminal_area);
    }

    // Ambient notifications sit above panes, but below interactive overlays.
    render_notifications(app, frame, terminal_area);
    render_popup_pane(app, terminal_runtimes, frame, terminal_area);

    if app.view.layout == ViewLayout::Desktop {
        if !app.view.diff_area.is_empty() {
            render_diff_zone(app, frame, app.view.diff_area);
        } else if app.diff_popup_open {
            render_diff_popup_overlay(app, frame, terminal_area);
        }
    }

    let mode_bar_area = if app.view.layout == ViewLayout::Desktop
        && app.tab_bar_position == crate::config::TabBarPositionConfig::Bottom
        && tab_bar_area.height > 0
    {
        tab_bar_area
    } else {
        terminal_area
    };

    match app.mode {
        Mode::Onboarding => render_onboarding_overlay(app, frame, frame.area()),
        Mode::ReleaseNotes => render_release_notes_overlay(app, frame, frame.area()),
        Mode::ProductAnnouncement => render_product_announcement_overlay(app, frame, frame.area()),
        Mode::Navigate if app.view.layout == ViewLayout::Mobile => {
            render_mobile_panel(app, terminal_runtimes, frame, frame.area())
        }
        Mode::Navigate => render_navigate_overlay(app, frame, mode_bar_area),
        Mode::Prefix => render_prefix_overlay(app, frame, mode_bar_area),
        Mode::Copy => render_copy_mode_overlay(app, frame, mode_bar_area),
        Mode::Resize => render_resize_overlay(app, frame, mode_bar_area),
        Mode::ConfirmClose => {
            render_confirm_close_overlay(app, terminal_runtimes, frame, terminal_area)
        }
        Mode::ContextMenu => {
            render_context_menu(app, frame);
        }
        Mode::Settings => render_settings_overlay(app, frame, frame.area()),
        Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
            render_rename_overlay(app, frame, frame.area())
        }
        Mode::NewLinkedWorktree => render_new_linked_worktree_overlay(app, frame, frame.area()),
        Mode::OpenExistingWorktree => {
            render_open_existing_worktree_overlay(app, frame, frame.area())
        }
        Mode::ConfirmRemoveWorktree => render_remove_worktree_overlay(app, frame, frame.area()),
        Mode::GlobalMenu => render_global_launcher_menu(app, frame),
        Mode::KeybindHelp => render_keybind_help_overlay(app, frame),
        Mode::Navigator => render_navigator_overlay(app, terminal_runtimes, frame),
        Mode::WorkerSummaries => render_worker_summaries_overlay(app, frame, frame.area()),
        Mode::SignalTray => self::signal_tray_popup::render(app, frame),
        Mode::Terminal => {}
    }

    apply_background_legibility(app, frame);
}

/// Adapt each cell's foreground colour for legibility against the persistent whole-terminal
/// background scene (`src/solar_system.rs`), as the very last drawing step so every other
/// renderer's own fg/bg decisions are already final.
///
/// Only a cell whose own background was left at [`Color::Reset`] lets the background scene show
/// through underneath it (`src/app/runtime.rs`'s own doc on the scene's `z` ordering: "above the
/// cell background, below text"); a cell with an opaque PTY-derived background paints over the
/// scene regardless, so its own fg is left untouched here. See
/// `crate::app::background_legibility` for the smoothed/hysteresis-gated decision this reads.
fn apply_background_legibility(app: &AppState, frame: &mut Frame) {
    if !app.background_scene_active() {
        return;
    }
    let Some(grid) = app.background_legibility.as_ref() else {
        return;
    };

    let buffer = frame.buffer_mut();
    let area = buffer.area;
    for row in area.y..area.y + area.height {
        for col in area.x..area.x + area.width {
            let cell = &mut buffer[(col, row)];
            if cell.bg != ratatui::style::Color::Reset {
                continue;
            }
            let Some(fg_rgb) = self::color::resolve_color_rgb(cell.fg, &app.host_terminal_theme)
            else {
                continue;
            };
            let Some(legibility) = grid.cell(row, col) else {
                continue;
            };

            let (fg, scrim) = legibility.render(fg_rgb);
            cell.set_fg(ratatui::style::Color::Rgb(fg.0, fg.1, fg.2));
            if let Some(scrim) = scrim {
                cell.set_bg(ratatui::style::Color::Rgb(scrim.0, scrim.1, scrim.2));
            }
        }
    }
}

fn render_navigation_chrome(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    if app.view.layout == ViewLayout::Mobile {
        render_mobile_header(app, terminal_runtimes, frame, app.view.mobile_header_rect);
    } else if app.view.sidebar_rect.width > 0 {
        if app.sidebar_collapsed {
            render_sidebar_collapsed(app, frame, app.view.sidebar_rect);
        } else {
            render_sidebar(app, terminal_runtimes, frame, app.view.sidebar_rect);
        }
    }
}

/// Whether the top-right warning banner is drawn at all, and so whether the
/// toast below it is offset by a row.
///
/// One predicate because `compute_view` decides the toast's *hit* area and
/// `render_notifications` decides where it is *drawn*; two spellings of this
/// would put the click somewhere other than the pixels.
fn warning_banner_present(app: &AppState) -> bool {
    app.config_diagnostic.is_some() || app.pane_size_pin.is_some()
}

fn render_notifications(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    // herdr's own status stream, under everything else it says. Drawn first so a
    // toast raised in the same frame stands over it rather than under it: the
    // toast is what herdr is saying *now*, and the stream is what it has said.
    if app.background_scene_active() {
        crate::ui::status::render_status_feed(
            frame,
            app.status_feed_rect(),
            &app.status_feed,
            app.machine_corner_rect(),
            &app.palette,
        );
    }
    let diagnostic_area = if app.view.layout == ViewLayout::Mobile {
        terminal_area
    } else {
        frame.area()
    };
    let mut banner_rows = 0;
    if let Some(message) = &app.config_diagnostic {
        render_config_diagnostic(frame, diagnostic_area, message, &app.palette);
        banner_rows = config_diagnostic_rows(message, diagnostic_area);
    }
    // Stacked under the config diagnostic rather than over it: both are the same
    // banner, and the config one is already where existing surfaces expect it.
    if let Some(pin) = &app.pane_size_pin {
        render_pane_size_pin(frame, diagnostic_area, pin, banner_rows, &app.palette);
    }
    // The toast/copy-feedback offset is "a banner is present", one row, as it
    // has always been — a multi-line diagnostic did not widen it either. It
    // must read the same predicate `compute_view` used for the toast's hit
    // area, or the toast is clicked somewhere other than where it is drawn.
    let has_warning_banner = warning_banner_present(app);
    let mut copy_feedback_offset = u16::from(has_warning_banner);
    let mut toast_rect = None;
    if let Some(toast) = &app.toast {
        if app.view.layout == ViewLayout::Mobile {
            render_mobile_toast_banner(
                frame,
                frame.area(),
                toast,
                has_warning_banner,
                &app.palette,
            );
        } else {
            render_toast_notification(
                frame,
                frame.area(),
                toast,
                has_warning_banner,
                toast.position.unwrap_or(app.toast_config.herdr.position),
                &app.palette,
            );
            toast_rect = Some(toast_notification_rect(
                frame.area(),
                toast,
                has_warning_banner,
                toast.position.unwrap_or(app.toast_config.herdr.position),
            ));
        }
        if app.view.layout == ViewLayout::Mobile {
            toast_rect = Some(mobile_toast_banner_rect(frame.area(), has_warning_banner));
        }
    }
    if let Some(feedback) = &app.copy_feedback {
        let area = if app.view.layout == ViewLayout::Mobile {
            frame.area()
        } else {
            terminal_area
        };
        if let Some(toast_rect) = toast_rect {
            copy_feedback_offset = copy_feedback_offset_for_toast(
                area,
                feedback,
                copy_feedback_offset,
                app.toast_config.clipboard.position,
                toast_rect,
            );
        }
        render_copy_feedback(
            frame,
            area,
            feedback,
            copy_feedback_offset,
            app.toast_config.clipboard.position,
            &app.palette,
        );
    }
}

fn copy_feedback_offset_for_toast(
    area: Rect,
    feedback: &crate::app::state::CopyFeedback,
    base_offset: u16,
    position: crate::config::ToastClipboardPosition,
    toast_rect: Rect,
) -> u16 {
    let feedback_rect = copy_feedback_rect(area, feedback, base_offset, position);
    if rects_overlap(feedback_rect, toast_rect) {
        base_offset.saturating_add(toast_rect.height)
    } else {
        base_offset
    }
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x.saturating_add(b.width)
        && b.x < a.x.saturating_add(a.width)
        && a.y < b.y.saturating_add(b.height)
        && b.y < a.y.saturating_add(a.height)
}

/// What the current mode's overlay is painting over, and therefore where a
/// Kitty graphics surface may not be placed this pass.
///
/// # Why a surface has to stand down for an overlay at all
///
/// A Kitty image at `z = 0` composites *above* the cell text, so an image left
/// on screen under an open menu draws over the menu rather than under it. That
/// is why [`crate::kitty_graphics::encode_local_pane_graphics`] has withheld
/// graphics outside [`Mode::Terminal`] since the first commit that drew pane
/// images.
///
/// # Why it is a rect and not a bool
///
/// That gate was written when a pane image was the only graphics surface there
/// was. The sidebar's pixel cards, the notification tray's badges and the
/// background scenes joined the same placement list later and inherited a rule
/// that was never about them — so opening a five-row menu in the sidebar
/// footer, or merely pressing the prefix key for a one-row bar under the panes,
/// deleted every card and every badge on the panel. The card and tray
/// *characters* stand down for artwork that is coming
/// ([`sidebar::image_card::shape_covers_row`], `sidebar::tray`'s
/// `artwork_covers_grid`), so what the user saw was not a fallback but bare
/// tree rails: rows dropping out of the sidebar on a click that destroyed
/// nothing.
///
/// Naming the overlay's own box instead answers the question the gate was
/// really asking — *would this image be drawn over the overlay?* — for exactly
/// the cells the overlay owns, and leaves every other surface where it is.
///
/// # Keeping this in step with [`render`]
///
/// One arm per mode, matched exhaustively and with no wildcard, against the
/// same dispatch `render` uses. A mode whose overlay this cannot bound answers
/// [`OverlayOcclusion::Screen`], which is exactly the old behaviour — so the
/// safe answer is also the default, and a new mode that is added to `render`
/// without being classified here fails to compile rather than silently
/// blanking the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayOcclusion {
    /// Nothing is overlaid: every surface is placed.
    None,
    /// A bounded panel. Only a surface reaching into it stands down.
    Panel(Rect),
    /// An overlay whose painted extent this cannot name, so nothing is placed.
    Screen,
}

impl OverlayOcclusion {
    /// Whether a surface drawn over `area` would land on this pass's overlay.
    pub(crate) fn hides(self, area: Rect) -> bool {
        match self {
            Self::None => false,
            Self::Screen => true,
            Self::Panel(panel) => rects_overlap(panel, area),
        }
    }
}

/// The one-row bar `render` draws for the prefix/copy/resize/navigate modes,
/// read off the same two rects that decide it there.
fn mode_bar_rect(app: &AppState) -> Rect {
    let area = if app.view.layout == ViewLayout::Desktop
        && app.tab_bar_position == crate::config::TabBarPositionConfig::Bottom
        && app.view.tab_bar_rect.height > 0
    {
        app.view.tab_bar_rect
    } else {
        app.view.terminal_area
    };
    if area.height == 0 {
        return Rect::default();
    }
    Rect::new(area.x, area.y + area.height - 1, area.width, 1)
}

/// See [`OverlayOcclusion`].
pub(crate) fn overlay_occlusion(app: &AppState) -> OverlayOcclusion {
    match app.mode {
        Mode::Terminal => OverlayOcclusion::None,
        // The bottom bars: one row of `mode_bar_area`, nowhere near the panel.
        Mode::Prefix | Mode::Copy | Mode::Resize => OverlayOcclusion::Panel(mode_bar_rect(app)),
        // Mobile navigate is the whole panel; desktop navigate is the bar.
        Mode::Navigate if app.view.layout == ViewLayout::Mobile => OverlayOcclusion::Screen,
        Mode::Navigate => OverlayOcclusion::Panel(mode_bar_rect(app)),
        Mode::ContextMenu => app
            .context_menu_rect()
            .map_or(OverlayOcclusion::Screen, OverlayOcclusion::Panel),
        Mode::GlobalMenu => OverlayOcclusion::Panel(app.global_menu_rect()),
        // The popover is anchored *above* the tray on purpose — its own doc:
        // "never over it: the badges have to stay visible" — which the blanket
        // gate made untrue the moment a badge was clicked.
        Mode::SignalTray => self::signal_tray_popup::view(app)
            .map_or(OverlayOcclusion::Screen, |view| {
                OverlayOcclusion::Panel(view.outer)
            }),
        Mode::WorkerSummaries => {
            self::worker_summary::worker_summaries_popup_rect(app.screen_rect())
                .map_or(OverlayOcclusion::Screen, OverlayOcclusion::Panel)
        }
        // Everything else keeps the blanket answer: a full-screen overlay, a
        // dimmed backdrop, or a dialog drawing more than one box.
        Mode::ConfirmClose
        | Mode::ConfirmRemoveWorktree
        | Mode::RenameWorkspace
        | Mode::RenameTab
        | Mode::RenamePane
        | Mode::NewLinkedWorktree
        | Mode::OpenExistingWorktree
        | Mode::Settings
        | Mode::Onboarding
        | Mode::ReleaseNotes
        | Mode::ProductAnnouncement
        | Mode::KeybindHelp
        | Mode::Navigator => OverlayOcclusion::Screen,
    }
}

fn dim_background(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
}

/// Floating overlay for navigate mode — appears at bottom of terminal area.
fn _build_hints(items: &[(&str, &str)], key_style: Style, dim_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (i, (k, desc)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", dim_style));
        }
        spans.push(Span::styled(k.to_string(), key_style));
        spans.push(Span::styled(format!(" {desc}"), dim_style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::keybind_help::keybind_help_groups;
    use super::scrollbar::scrollbar_thumb;
    use super::*;
    use crate::{app::state::ViewLayout, layout::PaneInfo, workspace::Workspace};
    use ratatui::style::Color;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn copy_feedback_offset_only_increases_when_toast_rect_overlaps() {
        let area = Rect::new(0, 0, 80, 24);
        let feedback = crate::app::state::CopyFeedback {
            message: "copied to clipboard".into(),
        };
        let toast = crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "workspace · 1".into(),
            position: None,
            target: None,
        };

        let bottom_right_toast = toast_notification_rect(
            area,
            &toast,
            false,
            crate::config::ToastHerdrPosition::BottomRight,
        );
        assert_eq!(
            copy_feedback_offset_for_toast(
                area,
                &feedback,
                0,
                crate::config::ToastClipboardPosition::TopCenter,
                bottom_right_toast,
            ),
            0
        );

        let bottom_center_toast = Rect::new(28, 21, 24, 3);
        assert_eq!(
            copy_feedback_offset_for_toast(
                area,
                &feedback,
                0,
                crate::config::ToastClipboardPosition::BottomCenter,
                bottom_center_toast,
            ),
            bottom_center_toast.height
        );
    }

    #[test]
    fn workspace_creation_dialog_renders_new_workspace_title() {
        let mut app = crate::app::state::AppState::test_new();
        app.mode = Mode::RenameWorkspace;
        app.pending_workspace_create_cwd = Some("/tmp/project".into());
        app.name_input = "project".into();

        let area = Rect::new(0, 0, 80, 20);
        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let screen = (0..area.height)
            .map(|row| buffer_row_text(terminal.backend().buffer(), area, row))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(screen.contains("new workspace"), "{screen}");
        assert!(screen.contains("project"), "{screen}");
    }

    #[tokio::test]
    async fn focused_pane_cursor_wins_during_terminal_render() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(ratatui::layout::Direction::Horizontal);

        ws.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );
        ws.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"r\r\nb"),
        );
        ws.tabs[0].layout.focus_pane(first_pane);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        let focused = app
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("focused pane info");

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();

        terminal
            .backend_mut()
            .assert_cursor_position((focused.inner_rect.x + 4, focused.inner_rect.y));
    }

    #[test]
    fn mobile_width_uses_header_and_full_width_terminal() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.sidebar_rect, Rect::default());
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 44, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(app.view.mobile_menu_hit_area.height, 2);
        assert_eq!(
            app.view.mobile_menu_hit_area.x + app.view.mobile_menu_hit_area.width,
            44
        );
    }

    #[test]
    fn mobile_config_diagnostic_keeps_command_visible() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.config_diagnostic = Some("config.toml:100:10; herdr config check".into());

        let area = Rect::new(0, 0, 44, 20);
        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let row = buffer_row_text(terminal.backend().buffer(), area, app.view.terminal_area.y);

        assert!(row.contains("config.toml:100:10"), "{row}");
        assert!(row.contains("herdr config check"), "{row}");
    }

    #[test]
    fn desktop_toast_hit_area_uses_full_frame_not_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.layout, ViewLayout::Desktop);
        assert!(app.view.terminal_area.x > 0);
        assert_eq!(app.view.toast_hit_area.x, 0);
        assert_eq!(app.view.toast_hit_area.y, 0);
    }

    #[test]
    fn desktop_toast_hit_area_still_offsets_for_config_diagnostic() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.config_diagnostic = Some("config warning".into());
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.toast_hit_area.x, 0);
        assert_eq!(app.view.toast_hit_area.y, 1);
    }

    fn pane_size_pin_test_app() -> AppState {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app
    }

    fn rendered_frame_text(app: &mut AppState, area: Rect) -> Vec<String> {
        compute_view(app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (area.y..area.y + area.height)
            .map(|row| buffer_row_text(buffer, area, row))
            .collect()
    }

    #[test]
    fn pane_size_pin_says_why_the_panes_do_not_fit() {
        let mut app = pane_size_pin_test_app();
        app.pane_size_pin = Some(crate::app::state::PaneSizePin {
            shared: (100, 30),
            client: (160, 50),
        });

        let rows = rendered_frame_text(&mut app, Rect::new(0, 0, 160, 20));

        // The head of the line carries the shared size, which is what says
        // which other client to detach.
        assert!(
            rows[0].contains("panes pinned to 100x30 by another client"),
            "{:?}",
            rows[0]
        );
        assert!(rows[0].contains("this one is 160x50"), "{:?}", rows[0]);
    }

    #[test]
    fn narrow_client_shortens_the_pin_notice_instead_of_clipping_it() {
        let mut app = pane_size_pin_test_app();
        app.pane_size_pin = Some(crate::app::state::PaneSizePin {
            shared: (120, 40),
            client: (44, 20),
        });

        let rows = rendered_frame_text(&mut app, Rect::new(0, 0, 44, 20));
        let banner = rows
            .iter()
            .find(|row| row.contains("pinned"))
            .expect("the notice should still be drawn on a narrow client");

        // The size that owns the panes survives every width; the prose around
        // it is what gives way, so the row never ends mid-word.
        assert!(banner.contains("120x40"), "{banner:?}");
        assert!(!banner.contains("this one is"), "{banner:?}");
        assert!(banner.trim_end().len() <= 44, "{banner:?}");
    }

    #[test]
    fn no_pane_size_pin_draws_no_banner() {
        let mut app = pane_size_pin_test_app();

        let rows = rendered_frame_text(&mut app, Rect::new(0, 0, 160, 20));

        assert!(
            !rows.iter().any(|row| row.contains("panes pinned to")),
            "{rows:?}"
        );
    }

    #[test]
    fn pane_size_pin_stacks_under_the_config_diagnostic() {
        let mut app = pane_size_pin_test_app();
        app.config_diagnostic = Some("config.toml:100:10; herdr config check".into());
        app.pane_size_pin = Some(crate::app::state::PaneSizePin {
            shared: (100, 30),
            client: (160, 50),
        });

        let rows = rendered_frame_text(&mut app, Rect::new(0, 0, 160, 20));

        assert!(rows[0].contains("herdr config check"), "{:?}", rows[0]);
        assert!(rows[1].contains("panes pinned to 100x30"), "{:?}", rows[1]);
    }

    #[test]
    fn desktop_toast_hit_area_offsets_for_pane_size_pin() {
        let mut app = pane_size_pin_test_app();
        app.pane_size_pin = Some(crate::app::state::PaneSizePin {
            shared: (100, 30),
            client: (160, 50),
        });
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.toast_hit_area.y, 1);
    }

    #[test]
    fn configured_mobile_width_threshold_controls_layout_switch() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.layout, ViewLayout::Desktop);

        app.mobile_width_threshold = 90;
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 80, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 80, 18));
    }

    #[test]
    fn diff_zone_folds_below_threshold_leaving_the_sidebar_and_terminal_untouched() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        // main_area.width = 80 - 26 (default sidebar) = 54, well under the
        // default 300-column threshold.
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert!(app.view.diff_area.is_empty());
        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 26, 20));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 54, 19));
    }

    #[test]
    fn diff_zone_shows_as_a_third_zone_above_threshold_and_shrinks_only_the_terminal() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.diff_zone_width_threshold = 150;

        // main_area.width = 200 - 26 (default sidebar) = 174 >= 150.
        compute_view(&mut app, Rect::new(0, 0, 200, 20));

        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 26, 20));
        assert_eq!(app.view.diff_area, Rect::new(100, 0, 100, 20));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 74, 19));
    }

    #[test]
    fn diff_zone_width_threshold_is_configurable_and_never_moves_the_sidebar() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        // main_area.width = 200 - 26 = 174, under the default 300 threshold.
        compute_view(&mut app, Rect::new(0, 0, 200, 20));
        let sidebar_before = app.view.sidebar_rect;
        assert!(
            app.view.diff_area.is_empty(),
            "default threshold (300) must fold at 174 remaining columns"
        );

        app.diff_zone_width_threshold = 174;
        compute_view(&mut app, Rect::new(0, 0, 200, 20));

        assert!(
            !app.view.diff_area.is_empty(),
            "lowering the configured threshold to exactly the remaining width must show the zone"
        );
        assert_eq!(
            app.view.sidebar_rect, sidebar_before,
            "the sidebar's own geometry must never move when the diff zone folds or unfolds"
        );
    }

    #[test]
    fn diff_zone_never_shows_in_mobile_layout_regardless_of_threshold() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.mobile_width_threshold = 250;
        app.diff_zone_width_threshold = 1;

        compute_view(&mut app, Rect::new(0, 0, 200, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert!(app.view.diff_area.is_empty());
    }

    /// Render-cost evidence for this project's multiplicative-performance-paths
    /// rule: `compute_view_internal` now does one extra `split_diff_zone` check
    /// per render pass, and the resulting narrower `content_area` feeds
    /// `resize_background_tab_panes_for_desktop`, which loops every workspace.
    /// `#[ignore]`d per this project's own "prefer deterministic tests over
    /// wall-clock CI limits" rule — run with `--ignored` and read the printed
    /// per-call durations; this is supporting evidence, not a pass/fail gate.
    #[test]
    #[ignore]
    fn diff_zone_render_cost_at_workspace_scale() {
        fn bench(workspace_count: usize, diff_zone_width_threshold: u16) -> std::time::Duration {
            let mut app = crate::app::state::AppState::test_new();
            app.workspaces = (0..workspace_count)
                .map(|i| Workspace::test_new(&format!("ws{i}")))
                .collect();
            app.active = Some(0);
            app.selected = 0;
            app.mode = Mode::Terminal;
            app.diff_zone_width_threshold = diff_zone_width_threshold;

            let area = Rect::new(0, 0, 220, 50);
            for _ in 0..10 {
                compute_view(&mut app, area);
            }
            const ITERS: u32 = 200;
            let start = std::time::Instant::now();
            for _ in 0..ITERS {
                compute_view(&mut app, area);
            }
            start.elapsed() / ITERS
        }

        // Threshold u16::MAX always folds; 1 always shows the zone (main_area
        // is always >= 1 column wide once a sidebar leaves any room at all).
        let folded_1 = bench(1, u16::MAX);
        let shown_1 = bench(1, 1);
        let folded_15 = bench(15, u16::MAX);
        let shown_15 = bench(15, 1);

        eprintln!(
            "compute_view_internal per-call cost — \
             1 workspace: folded={folded_1:?} shown={shown_1:?} (delta={:?}) | \
             15 workspaces: folded={folded_15:?} shown={shown_15:?} (delta={:?})",
            shown_1.saturating_sub(folded_1),
            shown_15.saturating_sub(folded_15),
        );
    }

    fn diff_pane_test_workspace(diff: Option<crate::workspace::GitDiffText>) -> Workspace {
        let mut ws = Workspace::test_new("one");
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: "repo-key".into(),
            checkout_key: "/repo".into(),
            repo_name: "repo".into(),
            repo_root: "/repo".into(),
            is_linked_worktree: false,
        });
        ws.cached_git_diff = diff;
        ws
    }

    fn sample_diff() -> crate::workspace::GitDiffText {
        use crate::workspace::{GitDiffLine, GitDiffLineKind};
        crate::workspace::GitDiffText {
            lines: vec![
                GitDiffLine {
                    kind: GitDiffLineKind::FileHeader,
                    text: "diff --git a/file.txt b/file.txt".into(),
                },
                GitDiffLine {
                    kind: GitDiffLineKind::Hunk,
                    text: "@@ -1,2 +1,2 @@".into(),
                },
                GitDiffLine {
                    kind: GitDiffLineKind::Removed,
                    text: "-old line".into(),
                },
                GitDiffLine {
                    kind: GitDiffLineKind::Added,
                    text: "+new line".into(),
                },
            ],
            truncated: false,
        }
    }

    /// The captain's own hard constraint: adding the diff pane must not touch
    /// the sidebar's rendered pixels at any width, folded or shown.
    #[test]
    fn sidebar_pixels_are_byte_identical_whether_the_diff_zone_shows_or_folds() {
        let render_sidebar = |diff_zone_width_threshold: u16| -> Vec<String> {
            let mut app = crate::app::state::AppState::test_new();
            app.workspaces = vec![diff_pane_test_workspace(Some(sample_diff()))];
            app.active = Some(0);
            app.selected = 0;
            app.mode = Mode::Terminal;
            app.diff_zone_width_threshold = diff_zone_width_threshold;

            compute_view(&mut app, Rect::new(0, 0, 200, 20));
            let sidebar_rect = app.view.sidebar_rect;

            let mut terminal = Terminal::new(TestBackend::new(200, 20)).unwrap();
            terminal.draw(|frame| render(&app, frame)).unwrap();
            (sidebar_rect.y..sidebar_rect.y + sidebar_rect.height)
                .map(|row| buffer_row_text(terminal.backend().buffer(), sidebar_rect, row))
                .collect()
        };

        // 1 folds the zone (nothing is ever narrower than that), 150 shows it
        // (main_area.width is 174 at this frame size).
        let folded = render_sidebar(u16::MAX);
        let shown = render_sidebar(150);

        assert_eq!(
            folded, shown,
            "the sidebar's rendered cells must be identical whether the diff zone is folded or shown"
        );
    }

    #[test]
    fn diff_zone_renders_added_and_removed_lines_in_green_and_red() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![diff_pane_test_workspace(Some(sample_diff()))];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.diff_zone_width_threshold = 150;

        compute_view(&mut app, Rect::new(0, 0, 200, 20));
        let diff_area = app.view.diff_area;
        assert!(!diff_area.is_empty());

        let mut terminal = Terminal::new(TestBackend::new(200, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let text = (diff_area.y..diff_area.y + diff_area.height)
            .map(|row| buffer_row_text(buffer, diff_area, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("old line"), "{text}");
        assert!(text.contains("new line"), "{text}");

        let removed_row = diff_area.y + 3; // panel border (1) + header (2) + hunk (1) = row index 3 has "-old line"
        let added_row = diff_area.y + 4;
        let removed_fg = buffer[(diff_area.x + 2, removed_row)].fg;
        let added_fg = buffer[(diff_area.x + 2, added_row)].fg;
        assert_eq!(removed_fg, app.palette.red, "removed line must be red");
        assert_eq!(added_fg, app.palette.green, "added line must be green");
    }

    #[test]
    fn folded_diff_pane_is_reachable_via_the_popup_overlay_toggle() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![diff_pane_test_workspace(Some(sample_diff()))];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        // Default threshold (300) folds at this frame size.
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert!(app.view.diff_area.is_empty());

        // Before toggling, nothing but the terminal is drawn where the popup
        // would go.
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let before = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!before.contains("new line"));

        app.diff_popup_open = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let after = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            after.contains("new line"),
            "the popup overlay must show the diff once toggled open while folded: {after}"
        );
    }

    #[test]
    fn desktop_tab_bar_position_controls_geometry_and_mode_bar_placement() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Prefix;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 0, 54, 1));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 54, 19));

        app.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.terminal_area, Rect::new(26, 0, 54, 19));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 19, 54, 1));
        assert!(app.view.tab_hit_areas.iter().all(|rect| rect.y == 19));
        assert_eq!(app.view.new_tab_hit_area.y, 19);

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let mode_row = buffer_row_text(
            terminal.backend().buffer(),
            app.view.tab_bar_rect,
            app.view.tab_bar_rect.y,
        );
        assert!(mode_row.contains("PREFIX"), "{mode_row}");
    }

    #[test]
    fn hide_tab_bar_when_single_tab_toggles_geometry_with_tab_count() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        let single_tab_terminal_area = app.view.terminal_area;
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(single_tab_terminal_area, Rect::new(26, 0, 54, 20));
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());

        app.workspaces[0].test_add_tab(Some("logs"));
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 0, 54, 1));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 54, 19));
        assert_eq!(app.view.tab_hit_areas.len(), 2);
        assert!(app.view.tab_hit_areas.iter().all(|rect| rect.width > 0));
        assert!(app.view.new_tab_hit_area.width > 0);

        assert!(app.workspaces[0].close_tab(1));
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.terminal_area, single_tab_terminal_area);
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());
    }

    #[test]
    fn bottom_tab_bar_still_hides_when_single_tab() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;
        app.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Prefix;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(app.view.terminal_area, Rect::new(26, 0, 54, 20));

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let mode_row = buffer_row_text(
            terminal.backend().buffer(),
            app.view.terminal_area,
            app.view.terminal_area.y + app.view.terminal_area.height - 1,
        );
        assert!(mode_row.contains("PREFIX"), "{mode_row}");
    }

    #[tokio::test]
    async fn hide_tab_bar_when_single_tab_resizes_background_tabs_per_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;

        let mut one_tab_workspace = Workspace::test_new("one");
        let one_tab_pane = one_tab_workspace.tabs[0].root_pane;
        let one_tab_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        one_tab_workspace.tabs[0]
            .runtimes
            .insert(one_tab_pane, one_tab_runtime);

        let mut two_tab_workspace = Workspace::test_new("two");
        let background_tab = two_tab_workspace.test_add_tab(Some("logs"));
        let two_tab_pane = two_tab_workspace.tabs[background_tab].root_pane;
        let two_tab_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        two_tab_workspace.tabs[background_tab]
            .runtimes
            .insert(two_tab_pane, two_tab_runtime);

        app.workspaces = vec![one_tab_workspace, two_tab_workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let one_tab_size = app.workspaces[0].tabs[0].runtimes[&one_tab_pane].current_size();
        let two_tab_size =
            app.workspaces[1].tabs[background_tab].runtimes[&two_tab_pane].current_size();
        assert_eq!(one_tab_size, (20, 53));
        assert_eq!(two_tab_size, (19, 53));
    }

    #[tokio::test]
    async fn mobile_background_tabs_use_mobile_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();

        let mut workspace = Workspace::test_new("mobile");
        let background_tab = workspace.test_add_tab(Some("logs"));
        let background_pane = workspace.tabs[background_tab].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        workspace.tabs[background_tab]
            .runtimes
            .insert(background_pane, runtime);

        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(
            app.workspaces[0].tabs[background_tab].runtimes[&background_pane].current_size(),
            (18, 43)
        );
    }

    #[test]
    fn product_announcement_renders_above_config_diagnostic() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::ProductAnnouncement;
        app.product_announcement = Some(crate::app::state::ProductAnnouncementState {
            version: "0.6.0".into(),
            id: "keybinding-v2".into(),
            title: "Keybinding syntax changed".into(),
            body: "### Update\n- Body".into(),
            scroll: 0,
            preview: false,
        });
        app.config_diagnostic = Some(
            "unsafe direct keybinding: keys.new_workspace = \"n\"\nunsafe direct keybinding: keys.new_tab = \"c\""
                .into(),
        );

        let area = Rect::new(0, 0, 44, 20);
        compute_view(&mut app, area);

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let popup = centered_popup_rect(
            area,
            PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
            PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
        )
        .expect("announcement popup");
        let title_row = popup.y + 1;
        let row = buffer_row_text(buffer, Rect::new(0, title_row, area.width, 1), title_row);

        assert!(row.contains("Keybinding syntax changed"));
        assert!(!row.contains("config warning"));
    }

    #[test]
    fn compute_view_clamps_sidebar_width_to_configured_max() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.sidebar_max_width = 30;
        app.sidebar_width = 999;

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.sidebar_rect.width, 30);
    }

    #[test]
    fn compute_view_clamps_sidebar_width_to_configured_min() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.sidebar_min_width = 22;
        app.sidebar_width = 5;

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.sidebar_rect.width, 22);
    }

    #[test]
    fn hidden_collapsed_sidebar_uses_full_width_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_collapsed = true;
        app.sidebar_collapsed_mode = crate::config::SidebarCollapsedModeConfig::Hidden;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 0, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::new(0, 0, 80, 1));
        assert_eq!(app.view.terminal_area, Rect::new(0, 1, 80, 19));
        assert!(app.view.workspace_card_areas.is_empty());

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
    }

    #[test]
    fn collapsed_sidebar_keeps_active_workspace_highlight_in_terminal_mode() {
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_collapsed = true;
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(1);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let ws_area = collapsed_sidebar_sections(app.view.sidebar_rect);
        let active_row = ws_area.y + 1;
        let active_style = buffer[(ws_area.x, active_row)].style();

        assert_eq!(active_style.bg, Some(app.palette.surface_dim));
    }

    #[test]
    fn expanded_sidebar_workspace_rows_show_state_before_name_without_numbers() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("one");
        let repo = temp_git_repo("main");
        ws.identity_cwd = repo.clone();
        let root_pane = ws.tabs[0].root_pane;
        ws.refresh_git_ahead_behind();

        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let root_terminal_id = app.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&root_terminal_id).unwrap().cwd = repo.clone();
        app.selected = 0;
        app.mode = Mode::Navigate;
        // The branch on the row, explicitly. The shipped default rows are the
        // body registers now (`crate::ui::sidebar::body_register`), and this
        // test is about the *name* and the fold — not about which readout the
        // fleet ships on line two.
        app.sidebar_spaces.rows = vec![
            vec![
                crate::config::SpaceSidebarToken::StateIcon,
                crate::config::SpaceSidebarToken::Workspace,
            ],
            vec![
                crate::config::SpaceSidebarToken::Branch,
                crate::config::SpaceSidebarToken::GitStatus,
            ],
        ];

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let card = app.view.workspace_card_areas[0].rect;
        let line1 = buffer_row_text(buffer, card, card.y);
        let line2 = buffer_row_text(buffer, card, card.y + 1);

        assert!(line1.starts_with("   one"), "line1: {line1:?}");
        assert!(!line1.contains("1 one"));
        // At the default 36-column maximum the name and the branch fit on one
        // line, so the row folds onto it and gives the second line back.
        assert!(line1.contains("main"), "line1: {line1:?}");
        assert_eq!(card.height, 1);
        assert_eq!(line2.trim(), "");

        std::fs::remove_dir_all(repo).ok();
    }

    #[test]
    fn tab_bar_dims_auto_named_tabs_and_emphasizes_custom_tabs() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let custom_tab = ws.test_add_tab(Some("logs"));
        ws.switch_tab(custom_tab);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let auto_rect = app.view.tab_hit_areas[0];
        let custom_rect = app.view.tab_hit_areas[1];
        let auto_style = buffer[(auto_rect.x + 1, auto_rect.y)].style();
        let custom_style = buffer[(custom_rect.x + 1, custom_rect.y)].style();

        assert_eq!(auto_style.fg, Some(app.palette.overlay0));
        assert!(auto_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(custom_style.fg, Some(app.palette.panel_bg));
        assert!(custom_style.add_modifier.contains(Modifier::BOLD));
    }

    /// The sidebar's ink is a function of the palette and the measured host
    /// theme, and it is read once per tree row — so it is derived once, here,
    /// rather than re-floored at every read. This is the assertion that fails
    /// if a later change moves the palette without moving the panel's copy.
    #[test]
    fn compute_view_rederives_the_sidebar_palette_from_the_current_theme() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;

        // Unset is the default, and then the panel draws with the shared ink.
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.sidebar_palette, app.palette);

        app.host_terminal_theme = crate::terminal_theme::TerminalTheme::default().with_color(
            crate::terminal_theme::DefaultColorKind::Background,
            crate::terminal_theme::RgbColor {
                r: 239,
                g: 241,
                b: 245,
            },
        );
        app.palette.sidebar_bg = Color::Rgb(24, 24, 37);
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let authored = crate::app::state::Palette::from_name(&app.theme_name)
            .expect("test_new's theme name should always resolve");
        assert_eq!(
            app.sidebar_palette,
            app.palette.for_sidebar(&authored, &app.host_terminal_theme)
        );
        assert_ne!(
            app.sidebar_palette, app.palette,
            "a panel with its own fill has its own floor, so the two must differ"
        );
    }

    #[test]
    fn tab_bar_uses_surface_dim_when_panel_background_resets() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let custom_tab = ws.test_add_tab(Some("logs"));
        ws.switch_tab(custom_tab);

        app.palette.panel_bg = Color::Reset;
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let custom_rect = app.view.tab_hit_areas[1];
        let custom_style = buffer[(custom_rect.x + 1, custom_rect.y)].style();

        assert_eq!(custom_style.bg, Some(app.palette.accent));
        assert_eq!(custom_style.fg, Some(app.palette.surface_dim));
        assert!(custom_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn new_tab_button_tracks_rightmost_tab_when_tabs_fit() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let last_visible = app
            .view
            .tab_hit_areas
            .iter()
            .rev()
            .find(|rect| rect.width > 0)
            .copied()
            .expect("last visible tab");

        assert_eq!(
            app.view.new_tab_hit_area.x,
            last_visible.x + last_visible.width
        );
    }

    #[test]
    fn tab_bar_shows_scroll_controls_when_tabs_overflow() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        for name in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"] {
            ws.test_add_tab(Some(name));
        }

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.tab_scroll_follow_active = false;
        app.tab_scroll = 2;

        compute_view(&mut app, Rect::new(0, 0, 65, 20));

        assert!(app.view.tab_scroll_left_hit_area.width > 0);
        assert!(app.view.tab_scroll_right_hit_area.width > 0);
        assert_eq!(app.view.tab_hit_areas[0].width, 0);
        assert_eq!(app.view.tab_hit_areas[1].width, 0);
        assert!(app.view.tab_hit_areas[2].width > 0);
        assert!(app.view.new_tab_hit_area.width > 0);

        let last_visible = app
            .view
            .tab_hit_areas
            .iter()
            .rev()
            .find(|rect| rect.width > 0)
            .copied()
            .expect("last visible tab");

        assert_eq!(
            app.view.tab_scroll_right_hit_area.x,
            last_visible.x + last_visible.width
        );
        assert_eq!(
            app.view.new_tab_hit_area.x,
            app.view.tab_scroll_right_hit_area.x + app.view.tab_scroll_right_hit_area.width
        );
    }

    #[test]
    fn tab_bar_clamps_manual_scroll_at_last_visible_tab() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.tab_scroll_follow_active = false;
        app.tab_scroll = usize::MAX;

        compute_view(&mut app, Rect::new(0, 0, 65, 20));

        let last_idx = app.workspaces[0].tabs.len() - 1;
        assert!(app.view.tab_hit_areas[last_idx].width > 0);
        let clamped_scroll = app.tab_scroll;

        app.scroll_tabs_right();

        assert_eq!(app.tab_scroll, clamped_scroll);
        assert!(app.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn pane_scrollbar_rect_uses_reserved_rightmost_column() {
        let info = PaneInfo {
            id: crate::layout::PaneId::from_raw(1),
            rect: Rect::new(0, 0, 12, 8),
            inner_rect: Rect::new(1, 1, 9, 6),
            scrollbar_rect: Some(Rect::new(10, 1, 1, 6)),
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        };

        assert_eq!(pane_scrollbar_rect(&info), Some(Rect::new(10, 1, 1, 6)));
    }

    #[tokio::test]
    async fn compute_view_reserves_terminal_column_when_pane_scrollbar_is_visible() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                12,
                4,
                4096,
                b"000000000000\r\n111111111111\r\n222222222222\r\n333333333333\r\n444444444444\r\n",
            ),
        );

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;

        compute_view(&mut app, Rect::new(0, 0, 40, 12));

        let info = app.view.pane_infos.first().expect("pane info");
        assert_eq!(info.inner_rect.width + 1, app.view.terminal_area.width);
        assert_eq!(
            info.scrollbar_rect,
            Some(Rect::new(
                info.inner_rect.x + info.inner_rect.width,
                info.inner_rect.y,
                1,
                info.inner_rect.height,
            ))
        );
    }

    #[test]
    fn scrollbar_stays_hidden_without_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: 5,
        };

        assert!(!should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_shows_with_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };

        assert!(should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_thumb_reaches_bottom_when_scrolled_to_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        assert_eq!(thumb.top + thumb.len, track.y + track.height);
    }

    #[test]
    fn scrollbar_offset_mapping_hits_top_middle_and_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        assert_eq!(scrollbar_offset_from_row(metrics, track, 4), 20);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 6), 10);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 8), 0);
    }

    #[test]
    fn dragging_from_current_thumb_row_preserves_offset() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 7,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 8);
        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        let row = thumb.top + thumb.len / 2;
        let grab = scrollbar_thumb_grab_offset(metrics, track, row).expect("grab");

        assert_eq!(scrollbar_offset_from_drag_row(metrics, track, row, grab), 7);
    }

    /// **Herdr's own status stream reaches the screen**, in the bottom third,
    /// and holds no more than [`crate::app::status_feed::TERM_MAX`] lines.
    ///
    /// A48 and A24 through the real render path rather than through the
    /// geometry: the rect is one question and whether anything is drawn in it is
    /// another, and the stream is gated on the background scene exactly as the
    /// machine register's corner is.
    #[test]
    fn the_status_stream_is_drawn_in_the_bottom_third_and_holds_six_lines() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.selected = 0;
        app.mode = Mode::Navigate;
        // The scene's own gate, which the stream shares.
        app.kitty_graphics_enabled = true;
        app.persistent_background_enabled = true;
        app.host_terminal_kind = crate::kitty_graphics::HostTerminalKind::Kitty;
        app.every_app_viewer_draws_ambient_wash = true;
        assert!(
            app.background_scene_active(),
            "the fixture's scene is not on"
        );

        let now = std::time::Instant::now();
        for index in 0..10 {
            app.status_feed.observe(
                Some(&crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::Finished,
                    title: format!("herdr-line-{index:02}"),
                    context: String::new(),
                    position: None,
                    target: None,
                }),
                now,
            );
        }

        let area = Rect::new(0, 0, 140, 40);
        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let rows: Vec<String> = (0..area.height)
            .map(|row| buffer_row_text(buffer, area, row))
            .collect();
        let carrying: Vec<u16> = (0..area.height)
            .filter(|row| rows[usize::from(*row)].contains("herdr-line-"))
            .collect();
        assert_eq!(
            carrying.len(),
            crate::app::status_feed::TERM_MAX,
            "the stream drew {} lines, not the six it holds:\n{}",
            carrying.len(),
            rows.join("\n")
        );
        // The six most recent, and the oldest four dropped.
        assert!(
            rows.iter().any(|row| row.contains("herdr-line-09")),
            "the newest line is not on screen"
        );
        assert!(
            !rows.iter().any(|row| row.contains("herdr-line-03")),
            "a line past the cap is still on screen"
        );
        // In the bottom third.
        for row in &carrying {
            assert!(
                *row >= area.height * 2 / 3,
                "the stream drew at row {row} on a {}-row frame, not in the bottom third",
                area.height
            );
        }

        // And with the scene off it is not drawn at all: it is part of that
        // surface family and shares its gate, exactly as the machine corner does.
        app.persistent_background_enabled = false;
        compute_view(&mut app, area);
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            (0..area.height)
                .map(|row| buffer_row_text(buffer, area, row))
                .all(|row| !row.contains("herdr-line-")),
            "the stream drew with the scene off"
        );
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("herdr-ui-test-{unique}"));
        std::fs::create_dir_all(root.join(".git")).expect("create .git dir");
        std::fs::write(
            root.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .expect("write HEAD");
        root
    }

    #[test]
    fn prefix_mode_renders_prefix_indicator() {
        let mut app = crate::app::state::AppState::test_new();
        app.mode = Mode::Prefix;
        app.view.terminal_area = ratatui::layout::Rect::new(0, 0, 60, 4);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4))
            .expect("test terminal");

        terminal
            .draw(|frame| render_prefix_overlay(&app, frame, app.view.terminal_area))
            .expect("draw prefix overlay");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("PREFIX"));
    }

    #[test]
    fn keybind_help_shows_unset_for_optional_actions() {
        let app = crate::app::state::AppState::test_new();
        let groups = keybind_help_groups(&app);

        let workspace_tab = groups
            .iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1
            .clone();
        let panes = groups
            .iter()
            .find(|(name, _)| *name == "panes")
            .expect("panes group")
            .1
            .clone();

        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "previous workspace"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "next workspace"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "previous agent"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "next agent"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "focus agent 1-9"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "switch workspace 1-9"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+h" && label.as_ref() == "focus pane left"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+j" && label.as_ref() == "focus pane down"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+k" && label.as_ref() == "focus pane up"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+l" && label.as_ref() == "focus pane right"));
    }

    #[test]
    fn keybind_help_shows_custom_command_descriptions() {
        let mut app = crate::app::state::AppState::test_new();
        app.keybinds.custom_commands = vec![
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+g"),
                label: "prefix+alt+g".to_string(),
                command: "lazygit".to_string(),
                action: crate::config::CustomCommandAction::Pane,
                description: Some("open lazygit".to_string()),
                width: None,
                height: None,
            },
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+h"),
                label: "prefix+alt+h".to_string(),
                command: "echo hello".to_string(),
                action: crate::config::CustomCommandAction::Shell,
                description: None,
                width: None,
                height: None,
            },
        ];

        let groups = keybind_help_groups(&app);
        let custom = groups
            .iter()
            .find(|(name, _)| *name == "custom")
            .expect("custom group")
            .1
            .clone();
        assert!(custom
            .iter()
            .any(|(key, label)| key == "prefix+alt+g" && label.as_ref() == "open lazygit"));
        assert!(custom
            .iter()
            .any(|(key, label)| key == "prefix+alt+h" && label.as_ref() == "custom command"));

        let rendered_help = keybind_help_lines(&app)
            .into_iter()
            .flat_map(|(_, line)| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("");
        assert!(rendered_help.contains("open lazygit"));
        assert!(rendered_help.contains("custom command"));
    }

    #[test]
    fn keybind_help_compacts_multiple_indexed_ranges() {
        let config: crate::config::Config = toml::from_str(
            r#"
[keys]
switch_tab = ["prefix+1..9", "alt+1..9"]
switch_workspace = "ctrl+1..9"
"#,
        )
        .expect("config parses");

        let mut app = crate::app::state::AppState::test_new();
        app.keybinds = config.keybinds();

        let workspace_tab = keybind_help_groups(&app)
            .into_iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1;

        let switch_tab_key = workspace_tab
            .iter()
            .find(|(_, label)| label.as_ref() == "switch tab 1-9")
            .map(|(key, _)| key.as_str())
            .expect("switch tab help entry");
        let switch_workspace_key = workspace_tab
            .iter()
            .find(|(_, label)| label.as_ref() == "switch workspace 1-9")
            .map(|(key, _)| key.as_str())
            .expect("switch workspace help entry");

        assert_eq!(switch_tab_key, "prefix+1..9 / alt+1..9");
        assert_eq!(switch_workspace_key, "ctrl+1..9");
    }

    /// End-to-end exercise of the whole per-cell legibility pipeline — `solar_system::build_layout`
    /// through `background_legibility::observe` through `apply_background_legibility`'s real
    /// buffer walk — against the actual solar-system frame generator, not a stub. A cell over the
    /// sun's bright, self-luminous disk must darken a light foreground to stay legible; a cell far
    /// off in deep space, already legible, must not be touched. This cannot substitute for looking
    /// at the real render live (see this task's own report for why that check did not run here),
    /// but it does confirm the mechanism this task built is actually reachable and wired, using
    /// the same `solar_system::frame`/`effects_frame` this task's sampler reads from in production.
    #[test]
    fn background_legibility_darkens_text_over_the_sun_and_leaves_deep_space_alone() {
        let mut app = crate::app::state::AppState::test_new();
        app.kitty_graphics_enabled = true;
        app.persistent_background_enabled = true;
        // The legibility pass only runs where a scene actually exists, and a
        // scene only exists on a host that draws an opaque wash below text.
        app.host_terminal_kind = crate::kitty_graphics::HostTerminalKind::Kitty;
        app.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 8,
            height_px: 16,
        };

        // Large enough that the sun's rendered disk (radius is a fraction of `min(width, height)`)
        // fills its whole centre cell brightly, rather than being diluted by mostly-dark
        // surrounding pixels averaged into a tiny cell at a toy resolution.
        let (cols, rows) = (40u32, 20u32);
        let (width_px, height_px) = (cols * 8, rows * 16);
        let nodes = [crate::solar_system::TreeNode {
            label: crate::solar_system::SceneLabel::EMPTY,
            parent: None,
            kind: crate::solar_system::BodyKind::Sun,
            stage: crate::anim::cell::LifecycleStage::Running,
            // `Severity::Critical` sits at `signal_light`'s brightest step (`SEVERITY_LIGHT_REACH`
            // tops out at 0.92, near `SIGNAL_LIGHT_BOUNDS`'s own ceiling) — `Clear`'s far dimmer
            // ink measured well under the WCAG black/white crossover luminance (~0.179) once
            // averaged into an 8x16 cell, which produced no adjustment at all and made this test
            // meaningless; this is the actual bright case the mechanism exists for.
            severity: crate::anim::cell::Severity::Critical,
            size: crate::solar_system::BodySize::Fixed,
            streak: 0.0,
            wear: 0.0,
            motes: 0,
            mote_share: 0.0,
        }];
        let layout = crate::solar_system::build_layout(&nodes, width_px, height_px);

        let bootstrapped = crate::app::background_legibility::observe(
            &mut app.background_legibility,
            &layout,
            0.0,
            &crate::solar_system::SceneEffects::default(),
            None,
            8,
            16,
            std::time::Instant::now(),
        );
        assert!(
            bootstrapped,
            "the first observe call must bootstrap the grid"
        );

        // The sun sits at the scene's own origin — off-centre, right of the panel strip the
        // composition reserves — so the cell it lands in is read off the layout rather than assumed
        // to be the middle one.
        let sun_px = layout.body_position(0, 0.0);
        let (sun_col, sun_row) = (sun_px.0 as u16 / 8, sun_px.1 as u16 / 16);
        let (space_col, space_row) = (0u16, 0u16);

        let area = Rect::new(0, 0, cols as u16, rows as u16);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                let buf = frame.buffer_mut();
                // A light PTY-style foreground on a transparent background — exactly the shape a
                // live agent pane's own text renders as (see `apply_background_legibility`'s doc).
                for row in 0..area.height {
                    for col in 0..area.width {
                        buf[(col, row)].set_fg(Color::Rgb(220, 220, 220));
                        buf[(col, row)].set_bg(Color::Reset);
                    }
                }
                apply_background_legibility(&app, frame);
            })
            .expect("draw");

        let buf = terminal.backend().buffer();
        let over_sun = crate::ui::color::resolve_color_rgb(
            buf[(sun_col, sun_row)].fg,
            &app.host_terminal_theme,
        )
        .expect("fg was rewritten to a concrete Rgb");
        let over_space = crate::ui::color::resolve_color_rgb(
            buf[(space_col, space_row)].fg,
            &app.host_terminal_theme,
        )
        .expect("fg was rewritten to a concrete Rgb");

        assert!(
            crate::ui::color::relative_luminance(over_sun)
                < crate::ui::color::relative_luminance(over_space),
            "text over the sun {over_sun:?} must render darker than text over deep space {over_space:?}"
        );
        // Deep space is already dark, so a light foreground there already clears the contrast
        // floor and `ensure_contrast_toward` must leave it alone rather than lightening it further.
        assert_eq!(over_space, (220, 220, 220));
    }
}
