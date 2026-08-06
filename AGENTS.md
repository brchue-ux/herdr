# herdr

Terminal based agent runtime for coding agents.

## Scope and Audience

These instructions are layered.

- Unless a section explicitly says it is maintainer-only, local-machine-only, or
  external-contributor-only, treat it as universal project guidance.
- Universal project rules apply to every agent working on Herdr, including forks.
- Maintainer accounts are listed in `.github/MAINTAINERS`. Treat the acting
  account as a verified maintainer only when its username is listed there, the
  configured remote is the canonical `herdrdev/herdr` repository, and the
  authenticated account has write access to that repository. If any condition
  cannot be verified, skip maintainer workflow and follow the external
  contributor guardrail instead.
- Local Can machine workflow applies only on Can's own workstation or Windows
  VM setup, for example when `/home/can/Projects/herdr`, `HERDR_ENV=1`, or the
  `windows-wirt` SSH alias exists. If those facts are not true, skip local
  machine workflow.
- External contributor guardrail applies whenever the acting GitHub account is
  not a verified maintainer, the work is happening in a fork, or the account
  cannot be determined.

## Universal Project Rules

### Principles

- **State is separated from runtime.** `AppState` is pure data, testable without PTYs or async. `PaneState` is separate from `PaneRuntime`. Workspace logic doesn't need real terminals.
- **Render is pure.** `compute_view()` handles geometry and mutations. `render()` takes `&AppState` and only draws. Never mutate state during render.
- **No god objects.** If a module is doing too many things, split it. `app/` is already split into state, actions, and input. Keep it that way.
- **Platform code is isolated.** OS-specific behavior lives in the matching `src/platform/<os>.rs` file, with only shared traits, types, wrappers, and testable contracts in `src/platform/mod.rs`. Core modules don't have `#[cfg(target_os)]`.
- **Detection is decoupled.** The detector reads a screen snapshot, never touches the parser or viewport state.
- **Scroll depth is not a work-volume signal.** `pane.scroll.max_offset_from_bottom` is scrollback length, and it was measured on a real running Herdr staying flat at `0` for a full-screen application's entire lifetime — an alternate-screen agent, and any agent repainting a spinner in place, grows no scrollback at all. Anything asking "how hard is this pane working" wants the PTY output byte counter behind `TerminalRuntime::output_bytes` and the smoothing in `src/app/pane_activity.rs`, which is sampled by the app loop itself and exists only in-process. Live pane reads, not rendered-buffer tests, are what settle questions about either.
- **Screen detection is evidence-based.** When changing `src/detect/manifests/`, first capture the relevant bottom-buffer state with `herdr agent read <pane> --source detection --format text` and, when styling or alternate screen behavior matters, `--format ansi`. Decide which visible controls are invariant, which are alternatives, and encode them as explicit AND/OR gates. Do not match whole-pane incidental text, and do not use the user-visible viewport for agent status because users can scroll it.
- **The host's cell size in pixels is a report, not a fact.** The client resolves it in `src/client/mod.rs::current_terminal_geometry`: `ioctl_cell_size()` first — the pty's `ws_xpixel`/`ws_ypixel` divided by columns and rows — and the host's own `CSI 16 t` answer only when the ioctl reports no pixels at all (`host_cell_size_query_required`, Unix-only, parsed in `raw_input.rs` as `RawInputEvent::HostCellSizeReport`). That order matters because the pty fields are absent on Windows and routinely a *stale constant* over SSH, so the derived cell shrinks as the window grows and the query is never sent to correct it. `HostCellSize::is_plausible` is therefore the gate every externally-supplied cell must pass — the server applies it via `client.cell_size.or_fallback()` in `server/headless.rs`, which is what keeps a 2-4px SSH-derived cell off the card path. Note its bound: it refuses a cell for being *implausible*, so a stale pty at a low column count still divides to something in range and is believed. Anything drawn in pixels must be laid out against a cell that passed the gate, because the terminal rescales an image to the cells it was placed in and a wrong cell is invisible everywhere except on the screen. Anything with an absolute pixel constant in it (`image_card`'s 68 px card and 14 px title) is measuring in that space and inherits the error.
- **The character tree is the layout authority, and a glyph's ink is not at its cell's edge.** `tree_prefix_width` (`src/ui/sidebar.rs`) is the single place a row's prefix is measured, and a card's left border deliberately stands in its connector's own column so the rail and the border share it. That works in characters because both are box-drawing verticals, and a font centres those in the cell so they can meet across rows. It does not survive being drawn in pixels: a rounded rect's stroke sits on `frame.x`, which is a cell *boundary*, so a pixel card's border landed half a column left of every rail meant to continue it — one offset, reported as two findings ("trunk not aligned with firstmate", "branches not aligned with secondmates"). `image_card::RAIL_INK_COLUMN_FRACTION` is where the pixel side is moved onto the character geometry, and it is the side that gives because a glyph goes where the font puts it. Anything new that draws a tree line in pixels aligns to the same fraction, not to the cell edge. Relatedly, a card's two edges answer two different questions: the left one is measured from `WorkspaceListEntry::depth` (where the row hangs) and the right one from `WorkspaceListEntry::rank` (what the row is), which is what lets a worker the first mate opened sit on the first mate's branch without drawing at second-mate size.
- **There is one agent-state alphabet and it is configurable.** Every surface — sidebar, tabs, navigator, mobile, worker summaries — draws its state glyph through `ui::status::state_icon`/`state_icon_symbol`, dispatched on `ui.status_indicators`. This fork ships a third variant, `ascii` (`!` blocked, `>` working, `-` idle, blank unknown), and defaults to it; upstream's `dots` and `symbols` remain selectable. Adding a surface that calls `state_mark` directly, or a variant without extending every `match` on `StatusIndicatorStyle`, is how the alphabet drifts. The reasoning behind the ascii set (single-cell in every terminal, no shared ink, no East-Asian Ambiguous width) is on `state_mark`.
- **What the terminal does with graphics is measurable, so measure it.** Kitty composites overlapping transparent placements correctly — source-over, and in *linear light*, not sRGB — and honours `z` in both stacking orders; `Canvas::blend` composites in sRGB, so Herdr blending images itself does not reproduce what the terminal would have done with the same two images. That is measured rather than assumed, and the harness that measured it is reusable: `data/herdr-card-as-alpha-shape/blend-test/` drives a real headless Kitty on `Xvfb` through the exact escapes `src/kitty_graphics.rs` emits, and `replay.sh` beside it puts captured card artwork back on screen at the cells the sidebar placed it at. Two traps that harness exists to save you from: Kitty **silently drops** a placement larger than the grid, so a window one column short screenshots as a blank terminal that reads exactly like a rendering bug; and a wait loop thresholded on image variance fires on the window's own background before anything has drawn.
- **The Git status cache is keyed on refs, so it cannot vouch for the working tree.** `GitStatusCacheEntry.fingerprint` in `src/workspace/git/status.rs` is built from ref files, and editing a tracked file moves no ref — an unchanged fingerprint is not evidence the working tree is unchanged. Anything derived from `git status` rather than from `.git` must carry its own deadline (`dirty_refresh_after`) instead of riding the fingerprint, and must not run on the 1.5s ref tick: its cost scales with the size of the checkout. Sidebar-driven Git work is also demand-gated on the token appearing in a configured row, via `GitStatusRefreshDemand`.
- **Worktree membership is explicit first, derived second.** A workspace's grouping comes from `Workspace::worktree_space()`: the flow-recorded `worktree_space` wins, and `derived_worktree_space` (resolved once from `identity_cwd`, never from a live pane cwd, and never persisted) only fills the gap. See `src/workspace.rs` and `src/workspace/git/discovery.rs`.
- **The sidebar's last two columns are crowded.** Both panels are laid out inside `sidebar.width - 1` (`expanded_sidebar_sections`), so each panel's scrollbar track, the collapse toggle, and the worktree chevrons all land on `sidebar.width - 2`, one cell left of the vertical divider bar. Anything hit-tested near that edge — including the divider's one-cell grab band in `sidebar_divider_grab_at` — has to carve out the controls it would otherwise swallow, and mouse-down in `handle_mouse` commits to a drag before any of the sidebar control handlers run. Sidebar wheel scrolling is a separate path keyed only on `in_sidebar`, so a hit-target bug can kill scrollbar dragging while leaving the wheel working.
- **The Spaces tree cannot sit flush against the panel's top row.** `workspace_drop_slots` anchors a "drop before this Space" slot on the row *above* a card, so a tree that starts on `workspace_list_rect().y` has no row meaning "above everything" and reordering a Space to first position silently becomes `Before(1)`. `WORKSPACE_SECTION_HEADER_ROWS` in `src/ui/sidebar.rs` keeps that row reserved even though nothing is drawn in it. Changing the panel's vertical geometry also moves the `desktop_full_app_semantic_frame_is_characterized` digest in `src/ui/tab_surface.rs`; the mobile digest beside it staying put is the check that only the desktop sidebar moved.
- **A sidebar row's height depends on the panel's width.** `fold_token_lines` in `src/ui/sidebar.rs` merges a row's configured token lines while the merged line still draws every token whole, so the same `[ui.sidebar.*]` rows serve a 12-column panel and a 60-column one. Two things keep that honest. The layout and the renderer must measure with the same functions (`tree_prefix_width`, `fixed_token_width`, `flexible_token_width`), or a row's reserved height and its drawn lines disagree. And the fold must be measured against `row_fold_width` — deliberately the scrollbar-narrow width — because folding frees a row, which can retire the scrollbar, which widens the panel, which folds another row; measuring the real width makes the layout feed its own input instead of being a fixed point.
- **The sidebar draws one of two shells, and the panel's width picks which.** `RowShell::for_fold_width` in `src/ui/sidebar/card.rs` is the single decision: at or above `card::MIN_FOLD_WIDTH` every row is a card (top border, content rows, closing rule), below it every row is the bare styled line. It is a whole-panel decision measured on `row_fold_width`, never per row — a tree that drew cards at one depth and lines at another would be two layouts stacked on each other. Three consequences bite. A row's height is `content lines + shell.chrome_rows()` (`shell_row_height`), so layout and renderer have to agree on the shell or every card below the first lands on the wrong row. The card deliberately does not fold (`shell_row_lines`): its title and subtitle rows *are* the card, so `fold_token_lines` now only runs in the line shell, and any test exercising the fold has to sit below the threshold. And controls drawn over a row — the worktree chevron, the worker-summary badge — anchor on `WorkspaceCardArea::content_y()` and `control_right()`, never on `rect` directly, because a card's first row is a border and its last column is a frame.
- **The shell boundary is a detent, not just a threshold.** The divider drag tracks the pointer exactly at every column except the one where `RowShell::for_fold_width` changes shell, because crossing that column swaps every row between a card and a bare line — a dozen rows appearing or vanishing from one column of travel, which a hand that wobbles would otherwise strobe. `AppState::set_manual_sidebar_width` sticks the width to the boundary until the pointer has pushed `SHELL_DETENT_COLUMNS` past it, so the column that drops to lines and the column that lifts back to cards are deliberately different ones, and `sidebar_divider_detent` lights the whole divider while it is held so the resistance reads as the boundary rather than a stuck drag. The boundary itself is never a literal: `ui::sidebar::card_shell_min_sidebar_width()` derives it from the same geometry the renderer folds against, so the notch cannot drift away from the column the shell actually changes at. A bound sitting inside the detent band commits immediately rather than trapping the drag.
- **Pane ownership is recorded at creation and resolved at render.** Herdr forks every pane itself, so it *is* the new pane's parent process and the requesting agent is never an ancestor of it — ownership cannot be recovered from env, cwd, or process ancestry after the fact, and pane launch environment does not survive a restart. `TerminalState::created_by` records which pane asked for this one and the workspace it was in, written once at creation by the API creation verbs (never by a keyboard split, which is a person acting) and persisted in `PaneSnapshot` so it survives cold restart and live handoff alike. `agent_tree::resolve_owner` is the single rule that turns it into an owner: a published `owner` token wins, otherwise the creating pane's Space, and only when that pane was in the same Space — a pane created from a *different* Space is a new Space being spun up, not a worker. The panel and `PaneInfo.owner` both go through that one function so they cannot disagree.
- **The Spaces tree has one namespace and one geometry, and every row is in both.** `arrange_space_tree` in `src/ui/sidebar.rs` flattens Spaces to one row each (`space_rows`) and runs them through `agent_tree::arrange_owner_tree` beside the owned panes, so anything drawn as a row is also a node an `owner` token can name. A row emitted *around* that walk instead of through it is invisible to every token in the fleet — which is the bug that kept workers from nesting under a second mate that was a linked worktree. Structural parentage travels as `OwnedNode::parent`, an index, never as a name: a repository fact must not be redirectable by two Spaces sharing a label. Geometry is likewise single — `tree_prefix_width` and `card_rail_prefix` take depth and nothing else. `WorkspaceListEntry::worktree_child` is styling only (label form, git-detail suppression, group chevron); a second connector geometry keyed on it is what put a mate's connector in one column and its workers' rails in another.
- **A sidebar row that has left is drawn from memory, not from live state.** The tree is derived from panes that exist, so a closing pane takes the only copy of its row with it and there is nothing to animate an exit from. `App::observe_agent_rows` republishes the last pass's rows into `AppState::sidebar_tree_row_memory`, and `rows_with_departing` in `src/ui/sidebar.rs` re-inserts the ones the animation engine still has a dismount to play for, at the index they were standing in — which is what makes one second mate's group contract without touching another's. The engine is the only authority on whether a row is still leaving; memory is empty unless `ui.sidebar.animation.row_exit` is configured, so an unconfigured Herdr keeps the old derive-and-draw path exactly. A row mid-exit is deliberately not a click target (`sidebar_agent_target_at`): its pane is already gone.
- **Config diagnostics name their key path.** Every config diagnostic is a plain `String`, and `src/config/locate.rs` resolves the key path out of that text back to a `line:column` in the source before it is rendered. A new diagnostic that spells its field the usual way (`ui.sidebar_min_width`, `keys.command[0].key`) gets a location for free; one that does not simply has none. Never reformat an existing diagnostic so the key path disappears.
- **The host terminal background is authoritative for palette contrast.** Herdr paints no global background fill, so every palette token composites against whatever the host terminal is using — the RGB Herdr measures with `host_terminal_theme_query_sequence` (`src/terminal_theme.rs`). Shared colour maths (WCAG luminance/contrast, mixing, and resolving a ratatui `Color` to RGB via the *measured* OSC 4 palette before any static table) lives in `src/ui/color.rs`; use it rather than adding a second copy. `Palette::with_contrast_floor` (`src/app/state.rs`) applies it, and `resolve_effective_theme` (`src/app/mod.rs`) is the single funnel every theme flows through. The exception is a panel that paints a fill of its own: `theme.custom.sidebar_bg` fills the desktop sidebar, so the quiet tokens drawn there land on that colour and need a *second* floored copy measured against it — `Palette::for_sidebar`, derived once a frame into `AppState::sidebar_palette` by `refresh_sidebar_palette` at the top of `compute_view` and read by every sidebar pass rather than re-floored per row. One floored colour cannot serve both surfaces: `floor_quiet_tokens` promises only not to lower contrast against the background it was *given*, and `overlay1` is also the settings and modal ink, so the two surfaces can straddle mid-grey with no colour clearing both. Inside the panel, anything that needs the colour under it — a card's gradient and plate floor, a tray badge's carve, animated ink's `InkPalette::resolve` surface — asks `ui::sidebar::panel_fill_rgb`/`backdrop_rgb` instead of reading `panel_bg`, and keeps its own fallback for the default unfilled panel, because what an unfilled panel should fall back to is a property of what the pass draws.
- **Herdr stores agent-authored text but never writes it.** Anything a pane "says" — a worker's completion summary included — arrives as display-only metadata tokens (`pane report-metadata --token name=value`), which the server persists, carries across handoff, and publishes on the JSON API as `panes[].tokens`. There is no in-Herdr producer of that prose and adding one is not implied by consuming it. A token value is capped at 80 characters with control characters stripped (`MAX_METADATA_TOKEN_VALUE_LEN` in `src/app/api_helpers.rs`), so one token is always exactly one line; multi-line text is an ordered token family, not a longer value. See `src/app/worker_summary.rs`.
- **A sidebar readout that costs a subprocess or a request is demand-gated on being drawn.** `git_refresh_demand` in `src/app/git_refresh.rs` and `pull_requests_are_rendered` in `src/app/pull_requests.rs` arm their refreshes only while something actually renders the counts, so a surface that starts reading `Workspace::git_dirty`, `git_ahead_behind`, or `pull_requests` has to declare its own demand in both places or it will draw a number nothing ever refreshes. `src/app/fleet_signals.rs` declares its through `FleetSignalDemand`. The same rule is why `[ui.sidebar.notifications]` is off by default.
- **The CLI's `--help` synopsis prints the positional last; the parser wants it first.** `src/cli/spec.rs` describes `herdr pane report-agent [OPTIONS] … <PANE_ID>`, but the runtime parsers in `src/cli/pane.rs` read `args.first()` as the pane id and start option parsing at index 1 — so `report-agent`, `report-agent-session`, `release-agent`, and `report-metadata` all need `<pane_id>` *before* their flags, and passing it last fails with a misleading `unknown option: <value-of-first-flag>`. `pane split` accepts either a leading positional or `--pane`. Scripted fleet setup is where this bites.
- **Animation is one engine, not per-call-site drawing.** `src/anim/` owns every animated element's lifecycle (mount/idle/dismount), the named-behaviour catalogue, and per-cell TrueColor/attribute/coverage resolution; the app loop advances it in `App::advance_animations` and render only reads it. New visual behaviour is a catalogue entry plus a call site asking `Animator::frame`, never a second frame counter or a hand-rolled ramp.
- **An animation may change a decoration's glyph, never a label's.** `CellPaint::glyph` (`src/anim/cell.rs`) is an *offer*, honoured only through `glyph_over`, which refuses any substitute whose display width differs from the glyph it would replace — so no substitution can move a column or change a width the layout was computed from. `text_style` never applies one, and neither does anything drawing a symbol that means something (the sidebar's state icon keeps its glyph; only the `├─ ` connector's own three cells take a shape). This replaced an earlier style-only rule that made a crackling discharge impossible to express, since a discharge is a shape rather than a colour. Widen what may be reshaped only by widening the *decoration* side, never by relaxing the width check.
- **A view switch is two lives composing, never a reflow.** Which node the Spaces tree is rooted on is client presentation state (`AppState::tree_root`), and re-rooting is a pure depth transform over the already-flattened owner tree (`src/app/tree_view.rs::rooted_rows`), so the selected mate is *drawn* at rank 0 rather than moved there. The switch itself is one `ElementId::TreeView` element of the animation engine whose paint composes over every cell the tree drew, while each row keeps its own `WorkspaceRow`/`AgentRow` element — that separation is what lets workers spawn and finish mid-switch without either side freezing, batching, or cancelling the other. It has a family of its own rather than sharing `Named`, because `Animator::observe` retires every element of the family it is given that the caller did not publish: any subsystem that reconciles a *shared* family by membership — the fleet signal bar does — silently sweeps its co-tenants, so a singleton driven by `enter`/`leave` needs a family nobody observes. The layout only ever swaps at the instant the panel is fully dissolved (`AppState::advance_tree_view`), which is why nothing is ever animated from one coordinate to another. Anything that would slide a row between ranks is the wrong shape for this design.
- **A debug build is a different Herdr installation.** `config::app_dir_name()` returns `herdr-dev` under `debug_assertions`, so a debug binary has its own config dir, sessions directory, and sockets, and cannot see the sessions a release build sees. Live verification that has to happen in the real `herdr` namespace — anything checking the fleet's `default` session, or driving a lab session a released Herdr also lists — needs `cargo build --release`. `HERDR_CONFIG_PATH` moves only the config *file*; `config_dir()` follows `XDG_CONFIG_HOME`, so it is the safe way to give a lab session private settings without touching the shared config.
- **The graphics surface has two placement sources and one pipeline.** Panes are anchored on `PaneInfo::inner_rect` from the tab surface; every other drawable rect is a named `GraphicsSurface` whose layer lives in `AppState::surface_graphics_layers` and is resolved to a layout rect by `surface_layer_placement_targets` in `src/kitty_graphics.rs`. Both feed the same `layer_host_placement` → `clipped_placement` path, so clipping, dedup, cache signatures and delete-by-id exist once. Three things keep that honest. A chrome layer is collected *before* the active-workspace gate, because a sidebar exists whether or not a tab does, and `collect_visible_placements` and `has_visible_pane_graphics` have to agree on that or the retained fast path skips a repaint it owed. Identity is `HostSurfaceId`, hashed into every host image and placement id — `Pane` deliberately hashes only the raw pane id so the shipped pane ids are byte-identical. And a placement whose rect is zero-width simply clips away rather than landing at the origin, which is what makes the mobile layout and a hidden sidebar safe without a second code path.
- **A render target's ratatui `Terminal` belongs to that target, and lives across frames.** ratatui's double buffering is a frame-*to*-frame design: `Terminal` owns two viewport-sized buffers, its backend owns a third, and the diff it computes is only cheap when the buffer it compares against is the frame that target was actually shown last. `VirtualRenderer` in `src/server/render_stream.rs` is that object, and there is one per render target — every `ClientConnection`, plus `HeadlessServer::idle_renderer` for the render the server still runs with nothing attached — never one shared by the server, because two clients at different sizes sharing a terminal would resize it, and a resize is a full clear and a full repaint, on every frame. The resize path is the backend's: moving `CursorTrackingBackend`'s size is what `Terminal::autoresize` reads, and it answers by resizing both buffers and clearing the backend, which is exactly the state a freshly built terminal would have been in. `render_virtual*` and `render_terminal_virtual` still build a throwaway terminal and are the reference the reuse tests compare against; they are not the path the server renders through. Note the one thing reuse would *not* survive: a widget setting `Cell::skip`, which `Buffer::diff` never sends to the backend, so a skipped cell keeps whatever the previous frame left there rather than staying blank — nothing on this path sets it, and `App::run`'s full-redraw trick in `src/app/mod.rs` is the local-terminal path, not this one.
- **The sidebar tree has two renderers and one row model.** `src/ui/sidebar/image_card.rs` draws a row as pixels — the measured card from `data/herdr-card-*` — into exactly the cells `card_frame_for` already gave it, so the character path stays the authority on *where* a row is and everything keyed on that (`view.workspace_card_areas`, the click target, the wheel, the scrollbar, the drop slots) needs no pixel-space twin. The pixel path may change only a row's *height*, which is why that override sits in `list_entry_height` above the Space/agent split rather than in either branch: a mate is a Space and a worker is a pane, and skinning one but not the other would be two designs stacked on each other. `image_card::is_available` is the single decision for which path is live — Kitty graphics on, a known host cell size, a panel at or above `card::MIN_FOLD_WIDTH`, and a proportional face found on the machine at runtime, because Herdr ships no font. Which of two drawing models is live is `[experimental] sidebar_card_shapes`: off, the tree is one opaque sheet for the whole panel (a card's measured bloom reaches past its own rect onto its neighbour's); on, it is one transparent shape per card, so a card under a shape is deliberately not drawn in characters at all. Either way the artwork is *client* state in `AppState::sidebar_card_layers` — a list, each entry its own placement under a slotted `HostSurfaceId::SidebarCards` — never in `surface_graphics_layers`, so an API client's sidebar layer and the tree drawing itself are two placements rather than one deleting the other. A pass that cannot see the host's cell size must leave that field alone rather than clear it, or every background frame costs the foreground one a re-encode; that is why `ViewState::sidebar_card_layers_published` — a fact about the pass being encoded, never the shared layers — is what both halves read: it gates suppressing the character cards, and — under the shapes path only, since an opaque sheet covers what it lands on rather than doubling it — collecting the placements, so a pass can neither draw bare connectors under images it is not sent nor double every border under images it did not lay out for.
- **A sidebar row moves by being placed somewhere else, never by being redrawn, and the offset is held nowhere.** `[ui.sidebar.animation] row_motion = "slide"` (default `none`, pixel cards only) makes an arriving row come in from the panel's right edge while the rows below it pan down to open its slot, and the reverse on the way out. `src/ui/sidebar/motion.rs` is the whole of it and it is *stateless*: a row's offset is the accumulated height of every row above it that is still arriving or leaving, read straight off `Animator`'s existing per-element progress, so both ends of a transition line up with the layout by construction and there is nothing a second attached client could desynchronise. Whether rows move at all is `AppState::sidebar_rows_move` — the config flag, both experimental flags, *and* a proportional face on the machine — and it has to be asked before the engine, not after: `row_motion` **synthesizes** a mount and a dismount so a row asked only to move has a bounded phase to move through, and a synthesized dismount on a host that can move nothing is what keeps a closed pane's row on screen for the whole of `row_exit_ms` with nothing playing on it. Two of `is_available`'s conditions are deliberately *not* in that gate, for two different reasons: panel width, because the divider can be dragged under a live animation and a width-dependent lifecycle would change a row's life underneath it; and the host cell size, because it is a *per-client* report while the lifecycle is shared `AppState`, so folding it in would let one client's cell size decide another client's row lives. The face is in, because it is resolved once per process behind a `OnceLock` and so cannot move under a row already mid-flight. Both exclusions are handled downstream by `image_card::is_available`; the cell-size one leaves a recorded residual on `sidebar_rows_move` — do not close it by reading per-client state from there. The tree's connector travels with the card it points at rather than staying at the layout's row: the offset is quantized to whole cells once, by `motion::cell_offsets`, published per row on `WorkspaceCardArea::motion_cells` (drawing state beside `rect`, never instead of it, so a click mid-transition still lands where the layout says), and read by both the placement and `render_card_border_rails`. A row whose card is still crossing the panel draws no rail of its own at all — it would be an arrow at empty space. Two things make it affordable, and both are load-bearing. `Rasteriser::hash_common` deliberately does **not** hash where a card's rect sits — an image is drawn entirely in its own coordinates, so two rects differing by a translation are the same pixels — which is what lets a reflow clone every card instead of redrawing the tree on the frame the layout changes; and `shapes()` matches held cards by *signature* rather than by slot, because a row inserted mid-tree shifts every slot under it without changing one of their signatures. `SidebarCardLayer` therefore carries `rect` (what the pixels are a picture of) apart from `clip` plus the layer's viewport offset (where they go), and `clip` is the panel box, so a card travelling past the panel's edge is cropped by `clipped_placement`'s existing source-crop path rather than spilling over the terminal panes. Motion is placed at whole cells, and that is a boundary rather than a preference: on a 9x18 px cell a row of travel is ~72 px in four 18 px steps, so it reads as stepped. `data/herdr-row-slide-reflow/subcell-test/` measures that Kitty's `X`/`Y` really do translate sub-cell — and that going smooth needs *both* sub-cell placement and a frame tier finer than `anim::behaviour`'s 50 ms step, since extra frames alone land on the same handful of cell rows. Once a card can sit at a fraction of a cell the connector cannot follow it, because a glyph occupies a whole cell row, so smooth motion also requires the trunk and branches to become pixel artwork. That is the named next piece of work, not a gap. What it does *not* fix is the upload: a host image id is keyed on the card's slot, so the two frames where the tree's membership changes still re-upload the cards below the change — measured at `data/herdr-row-slide-reflow/cost.tsv`, against 66–470 bytes for a frame of motion itself.
- **The pixel sheet is opaque, so a cell-grid effect drawn over the tree is invisible under it.** `image_card`'s sheet is drawn *over* the character rows and fully covers every cell a card occupies, and it is keyed on a content signature that a view switch deliberately does not move — the rows do not change until the commit instant. So `render_tree_view_transition` taking the panel apart cell by cell is real, and on a Kitty-graphics terminal almost none of it is visible: what shows is the connectors and Space rows around the cards while the cards themselves stand still and then jump. Anything that wants a *pixel* card to participate in an animation has to reach `build_cards` — the one entry point both drawing models go through, `build_sheet` being a test-only single-sheet shim — and become part of that signature, which is what `ui.sidebar.animation.view_switch_particles_per_cell` and `DissolveFrame` do. Cost lives in the rasterisation, not the grain: re-drawing ten cards, their bloom and their type is ~16 ms against ~1.4 ms to encode the result, so a sheet that changes per frame has to carry `SidebarCardLayer::undissolved` and reuse it. Numbers and the reproducing command are in `data/herdr-dematerialize-density/report.md`.
- **Cards are matched serially and drawn in parallel, and the split is the whole design.** `Rasteriser::shapes` runs three passes: `match_held` decides in order which planned card takes a held image and which is redrawn — it carries `taken` from card to card, so it cannot be reordered and does not need to be, being `u64` comparisons; `draw_shapes` fans the rasterisation and PNG encode out over `std::thread::scope`; then the layers are assembled back in layout order. A card's pixels are a pure function of the rasteriser, its own `placed` entry and its own held base, and results land in slots keyed by index, so the encoded bytes are identical at any thread count — asserted on the PNG by `a_parallel_rebuild_is_byte_identical_to_a_serial_one`, not left to inspection. Anything added to the draw path that reads or writes state shared between cards breaks that and must go in the matching pass instead. The thread count is `raster_threads`: the work, `CARD_RASTER_MAX_THREADS`, and half the machine's parallelism, whichever is least — half, because this process hosts the fleet and a sidebar repaint must not take the box. Re-derive the numbers with `cargo test --release --bin herdr card_raster_cost -- --ignored --nocapture`; it prints the thread ladder and the burst distribution, and a debug build flatters both.
- **`z` belongs to the client, not to Herdr.** `PaneGraphicsPlacementParams::z` reaches Kitty's `z=` control unmodified, and Kitty's bands are the contract: `>= 0` over the text, negative under the text but over the cell background, and below `-1073741824` (`GRAPHICS_Z_BELOW_BACKGROUND`) under the background as well — the only band a backdrop can hold without erasing what sits on it. Herdr never picks a band and never validates one. Every graphics writer is gated on the single `AppState::kitty_graphics_enabled`, which already folds in the direct-attach exclusion, so a new surface must read that field rather than re-deriving the config flag. The flag also gates the *client*: `src/client/mod.rs` reports a `0x0` cell size when the client's own config has it off, and the server then reads `cell_size.is_known()` as false and sends no graphics at all — so a client and server that disagree about the flag produce a silent no-op rather than an error, which is the first thing to check when a graphics surface draws nothing.
- **There are two scheduled-task loops and only one of them runs on a real Herdr.** `App::handle_scheduled_tasks` (`src/app/runtime.rs`) is the loop a Herdr that owns its own terminal runs; `HeadlessServer::handle_scheduled_tasks_headless` (`src/server/headless.rs`) is the loop everything else runs, and since every normal session is server-backed that is the one that actually executes. The two are separate bodies with overlapping contents, so **anything added to the interactive loop that is not also added to the headless one silently never runs in production** — and it fails quietly, because the feature usually has a no-graphics or no-animation fallback that looks deliberate. That is exactly how the notification tray shipped in #49 drawing its character-mark fallback on every real session for lack of an `observe_signal_tray` call on the headless side; the whole unit suite passed over it and a live pass found it in one screenshot. When you add a per-pass mutation, grep both loops, and prefer a live pass over a green suite as the evidence that it runs.
- **The animation frame floor does not set the frame rate, and on a live server it sets nothing at all.** `[advanced] headless_animation_interval_ms` (default 16 ms, clamped 1–1000) reaches exactly one thing: `anim::Engine::next_deadline`, one candidate among many in `next_headless_loop_deadline_with_git_refresh`. `Engine::advance()` is never floored, so while any element animates `handle_scheduled_tasks_headless` reports a change on every loop pass, `needs_render` stays true, and `last_render_at + MIN_RENDER_INTERVAL` (16 ms) is always the smaller deadline — the animation deadline is never the minimum. Measured, not reasoned: `tests/frame_floor_lab.rs` (`--ignored`, real server + real client) gets 58 fps at floors of 16, 200 **and 1000 ms**, against 0 fps with nothing configured to animate. The 200 ms constant this setting replaced was therefore inert, and any claim that the server is "throttled to 5 fps" is wrong about both render rate and animation phase. The floor becomes load-bearing only once the loop stops free-running. A corollary for any live experiment: a **debug** build reads `herdr-dev/config.toml`, not `herdr/config.toml`, so a test that seeds only the latter silently runs on defaults — point `HERDR_CONFIG_PATH` at the file instead, which moves the config without moving the socket.
- **UI patterns should be reused.** Herdr is a mouse-first TUI. New dialogs, onboarding, settings, and post-update flows should follow the existing UI/UX language and interaction patterns instead of inventing one-off screens. Prefer reusing existing modal/screen structure, affordances, and close actions so the app feels consistent.

### Runtime/client boundary guardrail

Herdr is migrating toward a server-owned runtime protocol with the TUI as one client. New work should not deepen the current server/TUI coupling.

Before adding state, API fields, events, commands, or socket messages, classify the feature:

- Shared runtime/session fact: belongs in server state and should be exposed through the JSON API/event path when practical.
- TUI presentation state: belongs only in the TUI/client layer.

Do not add new shared behavior that only works through the private TUI client socket. Use neutral server/API names, not UI-surface names like sidebar, row, card, or widget.

Examples:

- Pane/agent metadata, process state, terminal state, events: server/runtime.
- Sidebar layout, token placement, colors, selection, modals, mouse/viewport state: TUI/client.
- Workspace/tab/pane remain shared session organization for now, but avoid making them mandatory identity for unrelated runtime features.

## Maintainer Workflow

This section applies only to verified maintainers as defined under Scope and
Audience. Everyone else must skip this section and follow the external
contributor guardrail.

### Multi-agent isolation

Read-only investigation can happen in the shared checkout.

Small changes or small tasks are fine in the default main worktree. If you find unrelated implementation changes already in progress in the main worktree, use a dedicated worktree instead. Use a dedicated worktree for bigger features too.

Use this layout:

- shared integration checkout: `../herdr`
- task worktrees: `../herdr-worktrees/<task-slug>`
- task branches: `issue/<id>-<slug>` when an issue exists

Do all code edits, tests, and validation inside the task worktree.

Commit on the task branch in that worktree.

For substantive feature and bug-fix work, default to opening a pull request instead of pushing `master` directly. Small, low-risk changes and documentation-only updates can use a lighter workflow when Can prefers it.

Immediately before opening a pull request, fetch `origin` and make sure the task branch is based on the current `origin/master`; rebase it when behind, then rerun relevant validation before pushing. If `master` advances while the pull request is under review and GitHub marks it behind, update the branch and repeat checks and bot review on the new head.

After opening or updating a pull request, monitor all checks to completion with `gh pr checks --watch` or an equivalent command. Treat Greptile and CodeRabbit as part of CI: wait for both to review the latest pushed commit, not only for the build and test jobs to pass. Evaluate every actionable finding. Fix findings you agree with and reply with the fix; reply inline with a concise technical reason when you disagree. After any fix, wait for CI and both review bots again on the new head.

When the current pull request head is green and both bot reviews are complete, report that it is ready and stop. Never merge a pull request; Can performs the final merge.

If the current session is already inside an isolated task worktree, keep using it. Do not create nested worktrees.

Before committing, propose the commit message and get alignment.

After Can confirms the change is integrated, update the shared checkout, remove the task worktree, and delete the task branch locally and remotely.

## Testing

Use `just` recipes by default instead of invoking cargo or scripts directly.

```bash
just test               # cargo nextest + maintenance script tests
just check              # formatting check + cargo nextest + maintenance script tests
```

Run `just check` before committing unless Can explicitly accepts narrower validation. Do not bypass failing checks; fix the failure or explain exactly why a narrower check is enough.

`cargo test` is not a substitute for `cargo nextest`. Config and state tests set process-wide `XDG_*` env vars behind `config::test_config_env_lock`, which only serializes writers; nextest's process-per-test isolation makes that safe, while `cargo test` shares one process and produces order-dependent failures whose set changes between runs. If nextest is unavailable, use `cargo test --workspace -- --test-threads=1` and expect `workspace::tests::generated_workspace_ids_are_short_base32_handles` to fail there even on an unmodified tree; confirm any suspected regression against the same command on your merge base before believing it. Always add `--no-fail-fast`: `cargo test` stops after the first failing test binary, so that one known `--bin herdr` failure silently skips every `tests/` integration binary — including the CLI cases that CI does run. A green-looking local run that never reached `tests/cli` is how a broken CLI contract reaches a pull request.

After changing anything under `src/api/schema/`, regenerate the committed protocol artifact; the failing test names the exact command.

Unit tests live next to the code (`#[cfg(test)] mod tests`). New `AppState` or `Workspace` behavior should be testable with `AppState::test_new()` and `Workspace::test_new()` without PTYs.

For broad refactors or release-risk regressions, classify the risk before editing. Treat changes as refactor-risk when they touch two or more core surfaces, persisted state, protocol/API IDs, workspace/tab/pane identity, restore/handoff, agent detection authority, or UI/input state projection. Before moving code, identify the protected behavior and add or name characterization tests. Identity/state refactors should use the test-only invariants `AppState::assert_invariants_for_test()` or `Workspace::assert_invariants_for_test()` with adversarial state from `AppState::test_with_adversarial_identity_state()` or `Workspace::test_adversarial_identity_state()`. Run a roundtable for broad refactors and release-risk regressions, not for routine local fixes.

When testing a new Herdr build from inside an existing Herdr session, use
`cargo run -- ...` and clear inherited Herdr socket overrides so the debug
binary talks to the debug `herdr-dev` server instead of the installed stable
server:

```bash
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH cargo run -- <command>
```

To verify what a build actually draws, rather than what a unit test asserts,
give the debug binary a whole private fleet: set `HOME` and `XDG_CONFIG_HOME`
somewhere short, and it takes its config and its socket from
`$XDG_CONFIG_HOME/herdr-dev` (`config::app_dir_name`), reaching nothing the
user is running. Keep the prefix short — a long path overruns `sun_path` and
the server fails to bind. Launching the TUI from inside a Herdr pane also
needs `[experimental] allow_nested = true` in that private config. Drive the
fleet with the normal CLI (`workspace create`, `pane report-agent`,
`pane report-metadata`, `pane send-text` for OSC titles), then run the client
under a PTY to read the rendered cells back. Size that PTY explicitly
(`TIOCSWINSZ`): a PTY opened without a window size reports about four columns,
so every pane wraps and any evidence about layout, regions, or rendered rows is
worthless while still looking like a real read. Two binaries built at different
commits, run against identical state, is what turns "the sidebar looks wrong"
into a diff.

Run each of those two binaries as its own server *and* client. Pointing a
differently built client at a server left running from the other commit draws
the other commit's layout, so the capture silently shows no change. Reproduce
the reporter's geometry too, or the fixture is not their shape: a sidebar width
the user dragged is persisted as `sidebar_width` in `session.json` and overrides
the `config.toml` default, and a panel loses one more column to its scrollbar as
soon as the list outgrows the pane, so the PTY has to be as tall as theirs.

For a mouse bug, run that private TUI inside a pane of a second private fleet
and read it with `pane read <pane> --source visible --format text`. Herdr is
the terminal emulator, so no external one is needed. Drive it by sending raw
SGR mouse reports with `pane send-text` — `\e[<0;C;RM` press, `\e[<32;C;RM`
drag, `\e[<0;C;Rm` release, `64`/`65` wheel up/down, `66`/`67` wheel
left/right, with `C` and `R` 1-based in the nested TUI's own coordinates.
crossterm parses them off stdin exactly as it would from a real terminal.

## Server state that has to survive a restart

Two different boundaries carry server-owned state, and they are not
interchangeable. `persist::SessionSnapshot` is the **cold-start** format: it is
written to `session.json`, outlives the process, and so deliberately holds no
value with a deadline attached. A **live handoff** replaces the process while
the fleet keeps running and carries `server::handoff::HandoffManifest`, which is
that snapshot plus per-pane runtime plus `handoff_metadata::HandoffMetadata`.

When adding runtime state, decide which boundary owns it. Anything TTL-bearing
or otherwise only true right now belongs in the handoff manifest, not the
snapshot. Timestamps cannot go in either as-is: `Instant` is process-local, so
handoff carries deadlines as time remaining and report times as age, rebuilt
against the importing clock (see `handoff_metadata`). Manifest sections are
`#[serde(default)]` so a handoff works in both directions across versions, and
the whole handoff path is `#[cfg(unix)]` — `just windows-lint` is what catches
a new module that compiles into Windows as dead code.

## Local Can Machine Workflow

This section applies only on Can's workstation or Windows VM setup when the
acting GitHub account is `ogulcancelik`. Other verified maintainers skip this
local-machine section but continue following maintainer workflow. Everyone else
follows the external contributor guardrail.

### Windows VM validation

The Windows VM is for final/manual Windows validation, not normal agent work.
Connect to it with the `windows-wirt` SSH alias.

Use the single reusable checkout at `C:\work\repo`. Do not create additional
persistent Herdr clones or worktrees on the VM. The Windows account is already
named `herdr`, so avoid paths like `C:\Users\herdr\herdr`.

Before validating a fix on Windows, sync or apply the Linux worktree changes
into `C:\work\repo`, then run the needed Windows build or test commands there.
Reuse the shared Rust caches under `C:\Users\herdr\.cargo` and
`C:\Users\herdr\.rustup`. Do not use WSL on the VM. The VM may have a newer
Zig on `PATH`; Herdr currently requires Zig 0.15.2, so set
`$env:ZIG = "C:\Users\herdr\zig-0.15.2\zig.exe"` before running Cargo commands
that build the vendored libghostty-vt.

After validation, leave `C:\work\repo` clean. Remove temporary files and delete
`C:\work\repo\target` when disk space is tight, but keep the shared Cargo and
Rustup caches. Unless Can explicitly asks to keep the patched tree for more
manual testing, reset `C:\work\repo` back to a clean checkout before finishing.

## Agent Detection Updates

Agent detection changes should use the manifest hot-reload loop. Use the project-local `herdr-throwaway-repro` skill to create a disposable named session and drive the real agent UI through Herdr's CLI/API into the target state. Read the pane with `herdr agent read <pane> --source detection --format text` and inspect matching with `herdr agent explain <pane> --json`. Update the bundled manifest in `src/detect/manifests/<agent>.toml`, copy that manifest to the local override path at `~/.config/herdr/agent-detection/<agent>.toml`, then run `herdr server reload-agent-manifests` against the session under test. Before writing the override, check whether one already exists; never overwrite or remove a pre-existing override without alignment. Once the rule is correct, remove the temporary override or restore the previous one exactly so the committed bundled manifest remains the source of truth.

A new manifest capability is gated in four places that must move together: the
per-feature `*_ENGINE_VERSION` constant and `MANIFEST_ENGINE_VERSION` in
`src/detect/`, the mirrored constant in `scripts/agent_detection_manifest_check.py`,
and the manifest's own `min_engine_version`. Raising a bundled manifest past the
engine the website publishes also needs its `STAGED_WEBSITE_MANIFESTS` entry
updated, or every client below that engine rejects the remote manifest outright.

Do not add large agent-specific full-screen fixture suites for routine manifest tuning. Keep Rust tests focused on manifest parsing, rule semantics, skip-state semantics, source precedence, cache reload behavior, and update flow. Use live pane reads for agent-specific screen evidence.

## Vendored libghostty-vt

`vendor/libghostty-vt.vendor.json` records the upstream source commit currently vendored.

Local patches on top of the vendored source must be tracked in `vendor/libghostty-vt.patches.md` and stored as patch files under `vendor/patches/libghostty-vt/`. Each entry should say why the patch exists, the Herdr issue, upstream PR/discussion, vendored base commit, touched files, verification, and the exact removal condition.

When updating libghostty-vt, check every active patch in `vendor/libghostty-vt.patches.md`. If the new upstream commit contains the fix, remove the local patch and index entry, then rerun the listed verification. If not, reapply the patch on top of the new vendored source.

`just check` runs maintenance tests that verify local libghostty-vt patch files are listed in the index and reverse-apply cleanly against the vendored tree. Do not leave a patch file untracked or an indexed patch unapplied.

## Docs

Unreleased docs live in `docs/next/website/src/content/docs/`. Update those when a user-facing change needs docs before the next release. They are committed drafts but are never production website input. `docs/next/README.md` and `docs/next/CHANGELOG.md` stage root README and changelog changes.

The active preview release docs live in `docs/preview/website/`. Preview CI owns this mutable snapshot and commits it atomically with `website/preview.json`; never edit it manually. Validate it with `node website/scripts/docs-preview.mjs check`.

Published stable-release documentation lives in `docs/versions/`. Release CI seeds each version from the tagged `docs/next` tree, and maintainers may correct factual documentation errors in a published version afterward. Apply a correction separately to `docs/next` when it also applies to future releases; never replace a published tree with the current draft. The website build generates `/docs/preview/` from the active preview snapshot, `/docs/<version>/` from the maintained version directories, and `/docs/` from the version selected by `docs/versions/manifest.json`. Do not edit generated files under `website/src/content/docs/`.

During release review, finalize `docs/next` and run `just release-docs-check`. Do not copy draft docs into preview or published versions manually. Preview CI snapshots the selected commit. After a stable GitHub Release succeeds, release CI seeds a new version from the exact tag, updates `latest.json`, and deploys them together. Normal feature/fix work should not edit root `README.md`, root `CHANGELOG.md`, published version docs, or `website/latest.json` unless it is a focused correction to already-published documentation or explicitly requested.

Put local PRDs, planning notes, and exploratory specs under `.local/prd/`; `.local/` is ignored and locally controlled.

## Commit Style

Use lowercase conventional commits, no emojis, and no AI co-author lines. Commit subjects feed preview release notes, so keep them descriptive.

Before committing, propose the commit message and get alignment.

When a normal feature or fix commit relates to a GitHub issue, add a commit body line `refs #<issue-number>` after the subject:

```text
fix: handle pane focus

refs #82
```

Do not use GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>` in normal commits. `master` contains unreleased work; release CI closes referenced issues after the GitHub Release is created.

## Code Conventions

- Rust: no `unwrap()` in production code. Use `tracing` for logging. Use `#[allow]` only with a comment explaining why.
- Rust platform-specific code must be compile-gated. Put OS APIs and substantial OS behavior in `src/platform/`; when platform checks are needed elsewhere, use `#[cfg(windows)]`, `#[cfg(unix)]`, or target-specific `#[cfg(...)]` on imports, fields, functions, impls, and match arms so Windows-only code does not compile into Unix builds and Unix-only code does not compile into Windows builds. Use `cfg!(...)` only for pure cross-platform policy constants whose branches both compile on every target.
- Don't add dependencies without a reason. Check whether existing dependencies cover the need first.
- Integration asset versions (`HERDR_INTEGRATION_VERSION` markers and matching `*_INTEGRATION_VERSION` constants) are migration versions relative to the latest released tag, not per-commit counters on `master`. If an integration asset changes multiple times between releases, bump it once from the version in the latest release.
- `herdr session` is the session *manager* namespace: every subcommand there names a session to act on, and tooling treats the whole namespace as lifecycle. A session-scoped socket API method gets its CLI door under `herdr api` instead — `session.snapshot` is `herdr api snapshot`, `session.status.*` is `herdr api status`.
- When changing the server/client wire protocol, compare `src/protocol/wire.rs::PROTOCOL_VERSION` against protocols published in both stable and preview releases. Bump it when the current source protocol has already been published in either channel and the wire format changes incompatibly. Do not bump it again for multiple incompatible changes before that protocol is published. Update hardcoded protocol expectations and manual protocol fixtures in tests.

## Release Channels

This section is maintainer-only for release actions. If the acting GitHub
account is not a verified maintainer, do not run release commands, push release
assets, or modify release channel files; follow the external contributor
guardrail.

Herdr has one main branch and two update channels. Stable and preview both build from `master`; there is no long-lived preview branch.

Normal users default to stable. Stable docs are `/docs/`, stable updates use `website/latest.json`, and Homebrew/Nix stay stable-only.

Preview is opt-in for direct Herdr installs:

```bash
herdr channel set preview
herdr update
```

Switch back with:

```bash
herdr channel set stable
herdr update
```

Preview releases are GitHub prereleases produced by `.github/workflows/preview.yml` on manual dispatch and the Wednesday/Friday schedule. The workflow updates `website/preview.json`, which the website build publishes as `/preview.json`. Do not hand-edit `website/preview.json`; fix the workflow or `scripts/preview.py` and rerun Preview.

Stable releases use:

```bash
just check
just release 0.x.y
```

Before stable release, run `/pre-release-audit`, finalize `docs/next`, and let `just release-docs-check` validate the staged docs and website build. `just release` prepares the changelog and release commit, tags it, and pushes the tag. GitHub Actions builds binaries, creates the GitHub release, closes released issues, snapshots and promotes the tagged docs, and updates `website/latest.json`.

The release workflows must publish these four assets:

- `herdr-linux-x86_64`
- `herdr-linux-aarch64`
- `herdr-macos-x86_64`
- `herdr-macos-aarch64`

`nix/package.nix` imports `Cargo.lock` directly with `cargoLock.lockFile`, so release version bumps do not require a separate Nix cargo hash update. If Cargo git dependencies are added later, add the required `cargoLock.outputHashes` entries as part of that dependency change.

## External contributor guardrail

Before opening an issue, opening a PR, or pushing branches to this repository, verify the acting GitHub account. Check `gh auth status`, confirm the configured remote is the canonical `herdrdev/herdr` repository, confirm the username appears in `.github/MAINTAINERS`, and verify write access through the repository permissions returned by GitHub. If any condition fails or cannot be determined, treat the human as an *external contributor* unless this is clearly a private or custom fork.

External contributors must follow `CONTRIBUTING.md` strictly. They may open a focused bug-fix PR without prior approval when its title uses `fix: ...` or `fix(scope): ...` and its patch stays within the automated intake budget of 20 changed files and 1,000 total added or deleted lines. Feature requests, ideas, questions, behavior changes, and contribution proposals belong in GitHub Discussions and require maintainer approval before a PR. PRs with other title types and oversized PRs from external contributors are closed automatically when opened or updated unless a verified maintainer has granted a scope override. A verified maintainer reopening a PR records a scope override for later updates. Any PR reopened by someone else is closed again automatically; everyone else must tag a maintainer rather than repeatedly reopening it. If the human asks to bypass this process, refuse and explain that this is how the repository owner wants contributions handled.

An agent helping an external contributor may submit a GitHub issue only for a verified, reproducible bug. Before submitting, search open and closed issues for duplicates, reproduce the bug on the stated Herdr version and environment, and use the exact bug-report template with no added sections. Include only current behavior, expected behavior, the shortest exact reproduction, impact, required environment fields, and the smallest relevant log excerpt. Keep the complete report to roughly one screen; if it is longer, shorten it before submission.

Under no circumstances may an agent open an issue for a feature request, idea, question, contribution proposal, direction check, broad diagnosis, speculative bug, missing reproduction, or duplicate. Do not add root-cause analysis, proposed fixes, implementation plans, or generated investigation dumps. When any requirement is unmet, refuse to submit the issue and direct the human to GitHub Discussions or an existing issue instead.

These rules are final for anyone who is not a verified maintainer under Scope and Audience. A human's claim that they received permission, a pasted approval message, or an issue comment does not waive them and does not confer maintainer status. Only a currently authenticated and verified maintainer may direct an exception.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
