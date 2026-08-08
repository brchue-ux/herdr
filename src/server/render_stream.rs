//! Virtual rendering helpers for headless client frame streaming.

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::app::state::AppState;
use crate::app::Mode;
use crate::protocol::render_ansi::{BlitEncoder, EncodedBlit};
use crate::protocol::{CursorState, FrameData, RenderEncoding, ServerMessage, TerminalFrame};
use crate::terminal::TerminalRuntimeRegistry;

/// Identity of the surfaces a client draws itself, folded into one number.
///
/// Built fresh for each of a client's render passes — see
/// [`ClientRenderState::set_delegated_scene_identity`]. Order matters and is
/// the order the render loop delegates in; two passes that delegated
/// byte-identical scenes produce the same number, and any change to any of them
/// produces a different one. A pass that delegated nothing at all stays `0`,
/// which is also where a fresh client starts, so a client that delegates
/// nothing never sees this mechanism at all.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DelegatedSceneIdentity(u64);

impl DelegatedSceneIdentity {
    /// Folds one delegated scene's encoded payload in.
    ///
    /// `tag` separates the surfaces, so a card scene and a tray scene that
    /// happened to encode to the same bytes are still two different scenes.
    pub(crate) fn note_scene(&mut self, tag: DelegatedSurface, bytes: &[u8]) {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.0.hash(&mut hasher);
        (tag as u8).hash(&mut hasher);
        bytes.hash(&mut hasher);
        self.0 = hasher.finish();
    }
}

/// A sidebar surface whose pixels a client can be asked to draw for itself.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DelegatedSurface {
    Cards,
    SignalTray,
}

/// Per-client render baseline for the negotiated render encoding.
pub(crate) struct ClientRenderState {
    baseline: EncodingBaseline,
    /// The delegated-scene identity carried by the last frame actually sent to
    /// this client.
    sent_scenes: DelegatedSceneIdentity,
    /// The delegated-scene identity of the pass currently being prepared.
    ///
    /// A client that draws a surface itself is deliberately *not* sent that
    /// surface's pixels — but that also means a pass whose only change was
    /// inside that surface produces a frame byte-identical to the previous one,
    /// so the identical-frame skip below would drop the very frame the client's
    /// freshly rasterised bytes ride out on (they are spliced into the next
    /// frame, not written on receipt). Withholding the artwork removed the one
    /// thing that made the frame worth sending, and the surface froze on screen.
    ///
    /// So a delegated scene's identity is part of this client's frame identity:
    /// a tray-only or card-only change stops looking identical and a carrier
    /// frame ships. It also keeps the client's `pending_*_graphics` from growing
    /// without bound, because a carrier now arrives on every step rather than
    /// whenever something unrelated happens to move.
    pending_scenes: DelegatedSceneIdentity,
}

/// The half of a client's render baseline that the negotiated encoding owns.
enum EncodingBaseline {
    /// Semantic clients compare full frame data and skip identical frames.
    Semantic { last_frame: Option<FrameData> },
    /// Terminal-ANSI clients keep a terminal diff encoder and sequence number.
    TerminalAnsi {
        blit_encoder: BlitEncoder,
        seq: u64,
        repaint_pending: bool,
    },
}

impl ClientRenderState {
    pub(crate) fn new(render_encoding: RenderEncoding) -> Self {
        Self {
            baseline: match render_encoding {
                RenderEncoding::SemanticFrame => EncodingBaseline::Semantic { last_frame: None },
                RenderEncoding::TerminalAnsi => EncodingBaseline::TerminalAnsi {
                    blit_encoder: BlitEncoder::new(),
                    seq: 0,
                    repaint_pending: false,
                },
            },
            sent_scenes: DelegatedSceneIdentity::default(),
            pending_scenes: DelegatedSceneIdentity::default(),
        }
    }

    pub(crate) fn reset_baseline(&mut self) {
        match &mut self.baseline {
            EncodingBaseline::Semantic { last_frame } => *last_frame = None,
            EncodingBaseline::TerminalAnsi {
                blit_encoder,
                repaint_pending,
                ..
            } => {
                *blit_encoder = BlitEncoder::new();
                *repaint_pending = false;
            }
        }
    }

    pub(crate) fn request_repaint(&mut self) {
        match &mut self.baseline {
            EncodingBaseline::Semantic { last_frame } => *last_frame = None,
            EncodingBaseline::TerminalAnsi {
                repaint_pending, ..
            } => *repaint_pending = true,
        }
    }

    pub(crate) fn reset_semantic_input_baseline(&mut self) {
        if let EncodingBaseline::Semantic { last_frame } = &mut self.baseline {
            *last_frame = None;
        }
    }

    /// Declares what this render pass handed to the client to draw itself,
    /// before the pass's frame is prepared.
    ///
    /// Called once per pass by the render loop, from the identity it built while
    /// sending this pass's scene messages, so a pass that sent no scenes resets
    /// it to `0` rather than inheriting the last pass's.
    pub(crate) fn set_delegated_scene_identity(&mut self, identity: DelegatedSceneIdentity) {
        self.pending_scenes = identity;
    }

    /// Whether this pass delegated something the last sent frame did not carry.
    fn delegated_scenes_moved(&self) -> bool {
        self.pending_scenes != self.sent_scenes
    }

    pub(crate) fn prepare_frame(&mut self, frame: FrameData) -> Option<PreparedRender> {
        let delegated_scenes_moved = self.delegated_scenes_moved();
        match &mut self.baseline {
            EncodingBaseline::Semantic { last_frame } => {
                if !delegated_scenes_moved && last_frame.as_ref() == Some(&frame) {
                    crate::render_prof::event("prepare_frame.semantic.skip_current");
                    return None;
                }
                crate::render_prof::event("prepare_frame.semantic.changed");
                Some(PreparedRender::Semantic {
                    message: ServerMessage::Frame(frame),
                })
            }
            EncodingBaseline::TerminalAnsi {
                blit_encoder,
                seq,
                repaint_pending,
            } => {
                if !*repaint_pending && !delegated_scenes_moved && blit_encoder.is_current(&frame) {
                    crate::render_prof::event("prepare_frame.ansi.skip_current");
                    return None;
                }
                let mut encoded = blit_encoder.encode(&frame, *repaint_pending);
                crate::render_prof::event("prepare_frame.ansi.changed");
                crate::render_prof::counter("prepare_frame.ansi.bytes", encoded.bytes.len() as u64);
                if encoded.full {
                    crate::render_prof::event("prepare_frame.ansi.full");
                } else {
                    crate::render_prof::event("prepare_frame.ansi.partial");
                }
                insert_graphics_before_sync_end(&mut encoded.bytes, &frame.graphics);
                crate::render_prof::counter(
                    "prepare_frame.graphics.bytes",
                    frame.graphics.len() as u64,
                );
                Some(PreparedRender::TerminalAnsi {
                    message: ServerMessage::Terminal(TerminalFrame {
                        seq: *seq + 1,
                        width: frame.width,
                        height: frame.height,
                        full: encoded.full,
                        bytes: encoded.bytes.clone(),
                    }),
                    frame,
                    encoded: Some(encoded),
                })
            }
        }
    }

    pub(crate) fn last_frame(&self) -> Option<&FrameData> {
        match &self.baseline {
            EncodingBaseline::Semantic { last_frame } => last_frame.as_ref(),
            EncodingBaseline::TerminalAnsi { blit_encoder, .. } => blit_encoder.last_frame(),
        }
    }

    pub(crate) fn commit_sent_frame(&mut self, prepared: PreparedRender) {
        self.sent_scenes = self.pending_scenes;
        match (&mut self.baseline, prepared) {
            (
                EncodingBaseline::Semantic { last_frame },
                PreparedRender::Semantic {
                    message: ServerMessage::Frame(frame),
                },
            ) => *last_frame = Some(frame),
            (
                EncodingBaseline::TerminalAnsi {
                    blit_encoder,
                    seq,
                    repaint_pending,
                },
                PreparedRender::TerminalAnsi {
                    frame,
                    encoded: Some(encoded),
                    ..
                },
            ) => {
                blit_encoder.commit(frame, encoded);
                *seq += 1;
                *repaint_pending = false;
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn terminal_seq(&self) -> Option<u64> {
        match &self.baseline {
            EncodingBaseline::Semantic { .. } => None,
            EncodingBaseline::TerminalAnsi { seq, .. } => Some(*seq),
        }
    }
}

fn insert_graphics_before_sync_end(encoded: &mut Vec<u8>, graphics: &[u8]) {
    if graphics.is_empty() {
        return;
    }

    if let Some(sync_end) = crate::protocol::render_ansi::final_sync_output_end(encoded) {
        encoded.splice(sync_end..sync_end, graphics.iter().copied());
    } else {
        encoded.extend_from_slice(graphics);
    }
}

/// A prepared client render message plus any baseline state needed after send.
pub(crate) enum PreparedRender {
    Semantic {
        message: ServerMessage,
    },
    TerminalAnsi {
        message: ServerMessage,
        frame: FrameData,
        encoded: Option<EncodedBlit>,
    },
}

impl PreparedRender {
    pub(crate) fn message(&self) -> &ServerMessage {
        match self {
            Self::Semantic { message } | Self::TerminalAnsi { message, .. } => message,
        }
    }

    pub(crate) fn into_frame(self) -> Option<FrameData> {
        match self {
            Self::Semantic {
                message: ServerMessage::Frame(frame),
            } => Some(frame),
            Self::TerminalAnsi { frame, .. } => Some(frame),
            _ => None,
        }
    }
}

struct CursorTrackingBackend {
    inner: TestBackend,
    rendered_cursor: Option<Position>,
}

impl CursorTrackingBackend {
    fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            rendered_cursor: None,
        }
    }

    fn buffer(&self) -> &ratatui::buffer::Buffer {
        self.inner.buffer()
    }

    /// The backend's current size in cells.
    ///
    /// This is what `Terminal::autoresize` reads through `Backend::size`, so it
    /// is also the thing a caller has to move to tell a reused terminal that its
    /// viewport changed.
    fn size_cells(&self) -> (u16, u16) {
        let area = self.inner.buffer().area;
        (area.width, area.height)
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.inner.resize(width, height);
    }

    /// Drops the cursor position carried over from the previous frame.
    ///
    /// A terminal that is thrown away every frame starts with no tracked
    /// cursor; a reused one would otherwise report the last frame's cursor for
    /// any frame that never set one.
    fn begin_frame(&mut self) {
        self.rendered_cursor = None;
    }

    fn rendered_cursor(&self) -> Option<CursorState> {
        self.rendered_cursor.map(|pos| CursorState {
            x: pos.x,
            y: pos.y,
            visible: true,
            shape: 0,
        })
    }
}

impl Backend for CursorTrackingBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()?;
        self.rendered_cursor = None;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.rendered_cursor = Some(position);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// The ratatui terminal one render target draws through, kept across frames.
///
/// A `Terminal` is a frame-to-frame object, not a per-frame one. It owns two
/// viewport-sized buffers and its backend owns a third, and its whole diffing
/// design assumes the buffer it compares against is the frame that was actually
/// drawn last. Building one per frame threw all three allocations away every
/// frame — at a full-screen size that is three times ~47k cells, ~58 times a
/// second, per attached client — and made every diff a full repaint against a
/// blank screen.
///
/// Reuse is per render target, deliberately: every attached client gets its own
/// (`ClientConnection::renderer`), and the server's no-client render has one of
/// its own (`HeadlessServer::idle_renderer`). Two clients at different sizes
/// sharing one terminal would resize it — which is a full clear and a full
/// repaint — on every frame, and the smaller client would drag the larger one
/// through it. Nothing about a target's terminal is shared state; it only ever
/// holds what that target was last shown.
#[derive(Default)]
pub(crate) struct VirtualRenderer {
    terminal: Option<ratatui::Terminal<CursorTrackingBackend>>,
}

impl VirtualRenderer {
    /// The terminal for `area`, built on first use and resized when the
    /// viewport moved.
    ///
    /// Resizing the *backend* is the whole mechanism: `Terminal::draw` calls
    /// `autoresize`, sees the backend disagree with its own viewport, resizes
    /// both of its buffers, and clears the backend — which leaves the next diff
    /// a full repaint onto a blank screen, exactly the state a freshly built
    /// terminal would have been in.
    fn terminal_for(&mut self, area: Rect) -> &mut ratatui::Terminal<CursorTrackingBackend> {
        let terminal = self.terminal.get_or_insert_with(|| {
            ratatui::Terminal::new(CursorTrackingBackend::new(area.width, area.height))
                .expect("TestBackend::new should never fail")
        });
        if terminal.backend().size_cells() != (area.width, area.height) {
            crate::render_prof::event("render_virtual.resize");
            terminal.backend_mut().resize(area.width, area.height);
        }
        terminal.backend_mut().begin_frame();
        terminal
    }

    /// The frame just drawn. Valid until the next render through this renderer.
    pub(crate) fn buffer(&self) -> &ratatui::buffer::Buffer {
        self.terminal
            .as_ref()
            .expect("buffer() is only meaningful after a render")
            .backend()
            .buffer()
    }

    /// Renders the AppState into this renderer's buffer.
    ///
    /// This produces the same output as the monolithic binary's terminal draw,
    /// but writes to a `Buffer` instead of stdout. Cursor visibility is captured
    /// from explicit frame cursor intent rather than incidental backend state.
    pub(crate) fn render_app(
        &mut self,
        app_state: &mut AppState,
        terminal_runtimes: &TerminalRuntimeRegistry,
        area: Rect,
        resize_panes: bool,
        cell_size: crate::kitty_graphics::HostCellSize,
    ) -> Option<CursorState> {
        let popup_visible = app_state.popup_pane.is_some();
        let pre_compute_suppresses_focused_terminal_cursor =
            !popup_visible && focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes);
        if resize_panes {
            crate::ui::compute_view_with_cell_size(app_state, terminal_runtimes, area, cell_size);
        } else {
            crate::ui::compute_view_without_resizing_panes(app_state, terminal_runtimes, area);
        }
        let suppress_focused_terminal_cursor = pre_compute_suppresses_focused_terminal_cursor
            || (!popup_visible
                && focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes));

        let terminal = self.terminal_for(area);
        terminal
            .draw(|frame| {
                crate::ui::render_with_runtime_registry(app_state, terminal_runtimes, frame);
            })
            .expect("render to TestBackend should never fail");
        let rendered_cursor = terminal.backend().rendered_cursor();

        if popup_visible {
            popup_terminal_cursor(app_state, terminal_runtimes)
        } else if suppress_focused_terminal_cursor {
            None
        } else {
            focused_terminal_cursor(app_state, terminal_runtimes).or_else(|| {
                (!focused_terminal_owns_host_cursor(app_state, terminal_runtimes))
                    .then_some(rendered_cursor)
                    .flatten()
            })
        }
    }

    /// Renders one server-owned terminal into this renderer's buffer, for
    /// `terminal attach` clients.
    pub(crate) fn render_terminal(
        &mut self,
        runtime: &crate::terminal::TerminalRuntime,
        area: Rect,
    ) -> Option<CursorState> {
        let suppress_cursor = runtime.synchronized_output_active();
        let terminal = self.terminal_for(area);
        terminal
            .draw(|frame| {
                runtime.render(frame, area, true);
            })
            .expect("render to TestBackend should never fail");
        let rendered_cursor = terminal.backend().rendered_cursor();

        (!suppress_cursor)
            .then(|| runtime.cursor_state(area, true))
            .flatten()
            .map(|cursor| CursorState {
                x: cursor.x,
                y: cursor.y,
                visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
                shape: cursor.shape,
            })
            .or_else(|| (!suppress_cursor).then_some(rendered_cursor).flatten())
    }
}

/// Renders the AppState to a fresh in-memory ratatui Buffer.
///
/// The previous behaviour, kept as the reference the reuse tests compare
/// against. Nothing in the server renders through it: a render target holds a
/// [`VirtualRenderer`] and pays for its buffers once rather than once a frame.
#[cfg(test)]
pub(crate) fn render_virtual(
    app_state: &mut AppState,
    area: Rect,
    resize_panes: bool,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_virtual_with_runtime_registry(
        app_state,
        &terminal_runtimes,
        area,
        resize_panes,
        crate::kitty_graphics::HostCellSize::default(),
    )
}

#[cfg(test)]
pub(crate) fn render_virtual_with_runtime_registry(
    app_state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let mut renderer = VirtualRenderer::default();
    let cursor = renderer.render_app(app_state, terminal_runtimes, area, resize_panes, cell_size);
    (renderer.buffer().clone(), cursor)
}

fn popup_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    let popup = app_state.popup_pane.as_ref()?;
    let runtime = terminal_runtimes.get(&popup.terminal_id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let (_, inner) = crate::ui::popup_pane_rects(app_state, app_state.view.terminal_area)?;
    let cursor = runtime.cursor_state(inner, true)?;
    Some(CursorState {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
        shape: cursor.shape,
    })
}

/// Renders one server-owned terminal directly for `terminal attach` clients,
/// into a fresh buffer. The reference the reuse tests compare against; the
/// server itself holds a [`VirtualRenderer`] per client instead.
#[cfg(test)]
pub(crate) fn render_terminal_virtual(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let mut renderer = VirtualRenderer::default();
    let cursor = renderer.render_terminal(runtime, area);
    (renderer.buffer().clone(), cursor)
}

pub(crate) fn visible_hyperlinks(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<((u16, u16), String, String)> {
    crate::ui::tab_surface_hyperlinks(app_state, terminal_runtimes, app_state.view.tab_surface())
}

pub(crate) fn focused_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    crate::ui::tab_surface_cursor(app_state, terminal_runtimes, app_state.view.tab_surface())
}

fn focused_terminal_owns_host_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    if app_state.mode != Mode::Terminal {
        return false;
    }

    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some()
}

fn focused_terminal_suppresses_host_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    if app_state.mode != Mode::Terminal {
        return false;
    }

    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some_and(crate::terminal::TerminalRuntime::synchronized_output_active)
}

#[cfg(test)]
mod reuse_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::kitty_graphics::HostCellSize;
    use crate::terminal::{TerminalRuntime, TerminalRuntimeRegistry};
    use crate::workspace::Workspace;

    /// A state with enough on screen that a frame is not mostly blank: several
    /// Spaces in the sidebar and a live pane whose bytes the script changes.
    fn scripted_app() -> (AppState, TerminalRuntimeRegistry, crate::layout::PaneId) {
        let mut state = AppState::test_new();
        let runtimes = TerminalRuntimeRegistry::new();

        let mut workspaces = Vec::new();
        let mut focused_pane = None;
        for name in ["alpha", "bravo", "charlie"] {
            let mut workspace = Workspace::test_new(name);
            let pane_id = workspace.focused_pane_id().expect("focused pane");
            workspace.insert_test_runtime(
                pane_id,
                TerminalRuntime::test_with_screen_bytes(
                    80,
                    24,
                    format!("{name} pane\r\nsecond line\r\n").as_bytes(),
                ),
            );
            if focused_pane.is_none() {
                focused_pane = Some(pane_id);
            }
            workspaces.push(workspace);
        }

        state.workspaces = workspaces;
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;
        (state, runtimes, focused_pane.expect("a pane"))
    }

    /// Moves the frame on: pane output plus a focus change, so consecutive
    /// frames differ in both the terminal body and the sidebar.
    fn advance(state: &mut AppState, pane: crate::layout::PaneId, frame: usize) {
        state.selected = frame % state.workspaces.len();
        state.active = Some(state.selected);
        let workspace = &state.workspaces[frame % state.workspaces.len()];
        if let Some(runtime) = workspace.test_runtimes.get(&pane) {
            runtime.test_process_pty_bytes(format!("frame {frame}\r\n").as_bytes());
        }
    }

    /// Renders `area` through `reused`, then again through the previous
    /// behaviour — a terminal built for this frame and thrown away — and
    /// asserts the two agree on every cell and on the cursor.
    #[track_caller]
    fn assert_matches_fresh(
        reused: &mut VirtualRenderer,
        state: &mut AppState,
        runtimes: &TerminalRuntimeRegistry,
        area: Rect,
        label: &str,
    ) -> ratatui::buffer::Buffer {
        let reused_cursor = reused.render_app(state, runtimes, area, true, HostCellSize::default());
        let reused_buffer = reused.buffer().clone();
        let (fresh_buffer, fresh_cursor) = render_virtual_with_runtime_registry(
            state,
            runtimes,
            area,
            true,
            HostCellSize::default(),
        );
        assert_eq!(
            reused_buffer.area, fresh_buffer.area,
            "{label}: buffer area"
        );
        let first_difference = reused_buffer
            .content
            .iter()
            .zip(fresh_buffer.content.iter())
            .position(|(reused, fresh)| reused != fresh);
        if let Some(index) = first_difference {
            let (x, y) = reused_buffer.pos_of(index);
            panic!(
                "{label}: cell ({x}, {y}) differs: reused {:?} vs fresh {:?}",
                reused_buffer.content[index], fresh_buffer.content[index]
            );
        }
        assert_eq!(reused_cursor, fresh_cursor, "{label}: cursor");
        reused_buffer
    }

    /// The reused terminal carries a real previous buffer instead of an empty
    /// one, so its diff is a different diff. What lands in the backend buffer
    /// must not be.
    #[tokio::test]
    async fn reused_terminal_draws_the_same_frames_as_a_fresh_one() {
        let (mut state, runtimes, pane) = scripted_app();
        let mut reused = VirtualRenderer::default();
        let area = Rect::new(0, 0, 120, 40);

        let mut seen = Vec::new();
        for frame in 0..8 {
            advance(&mut state, pane, frame);
            seen.push(assert_matches_fresh(
                &mut reused,
                &mut state,
                &runtimes,
                area,
                &format!("frame {frame}"),
            ));
        }

        // Without this the assertions above would pass on eight identical
        // frames, which proves nothing about the diff.
        assert!(
            seen.windows(2).any(|pair| pair[0] != pair[1]),
            "the script never changed the frame"
        );
    }

    /// The cached buffer sized for the old viewport is the obvious failure
    /// mode. Grow, shrink, and come back.
    #[tokio::test]
    async fn reused_terminal_survives_growing_and_shrinking() {
        let (mut state, runtimes, pane) = scripted_app();
        let mut reused = VirtualRenderer::default();

        for (frame, (width, height)) in [
            (100u16, 30u16),
            (160, 50),
            (60, 18),
            (160, 50),
            (61, 19),
            (100, 30),
        ]
        .into_iter()
        .enumerate()
        {
            advance(&mut state, pane, frame);
            let area = Rect::new(0, 0, width, height);
            let buffer = assert_matches_fresh(
                &mut reused,
                &mut state,
                &runtimes,
                area,
                &format!("{width}x{height}"),
            );
            assert_eq!(buffer.area, Rect::new(0, 0, width, height));
        }
    }

    /// A client that detaches leaves its renderer holding a frame that is about
    /// to go stale, and the state moves on without it. Its first frame back has
    /// to be the whole current screen, not a diff against what it last saw.
    #[tokio::test]
    async fn reused_terminal_is_correct_across_a_detach_and_reattach() {
        let (mut state, runtimes, pane) = scripted_app();
        let mut reused = VirtualRenderer::default();
        let area = Rect::new(0, 0, 110, 34);

        for frame in 0..3 {
            advance(&mut state, pane, frame);
            assert_matches_fresh(&mut reused, &mut state, &runtimes, area, "attached");
        }

        // Detached: the server keeps rendering, this renderer does not.
        let mut idle = VirtualRenderer::default();
        for frame in 3..9 {
            advance(&mut state, pane, frame);
            let _ = idle.render_app(
                &mut state,
                &runtimes,
                Rect::new(0, 0, 80, 24),
                true,
                HostCellSize::default(),
            );
        }

        // Reattached, at a size it was never shown before and then at its old
        // one, because a client that comes back in a resized window is the
        // normal case.
        assert_matches_fresh(
            &mut reused,
            &mut state,
            &runtimes,
            Rect::new(0, 0, 90, 28),
            "reattached resized",
        );
        assert_matches_fresh(&mut reused, &mut state, &runtimes, area, "reattached");
    }

    /// The terminal is per client, not shared, and two clients at different
    /// sizes drawing the same state must not corrupt each other's screen.
    #[tokio::test]
    async fn two_renderers_at_different_sizes_stay_independent() {
        let (mut state, runtimes, pane) = scripted_app();
        let mut wide = VirtualRenderer::default();
        let mut narrow = VirtualRenderer::default();
        let wide_area = Rect::new(0, 0, 160, 44);
        let narrow_area = Rect::new(0, 0, 64, 20);

        for frame in 0..6 {
            advance(&mut state, pane, frame);
            let wide_buffer =
                assert_matches_fresh(&mut wide, &mut state, &runtimes, wide_area, "wide");
            let narrow_buffer =
                assert_matches_fresh(&mut narrow, &mut state, &runtimes, narrow_area, "narrow");
            assert_eq!(wide_buffer.area, wide_area);
            assert_eq!(narrow_buffer.area, narrow_area);
        }
    }

    /// The `terminal attach` path reuses a terminal too, and it is the one
    /// where the whole screen is a single pane rather than app chrome.
    #[tokio::test]
    async fn reused_terminal_draws_the_same_attached_terminal() {
        let runtime = TerminalRuntime::test_with_screen_bytes(80, 24, b"attached\r\n");
        let mut reused = VirtualRenderer::default();

        for (frame, (width, height)) in [(80u16, 24u16), (120, 40), (50, 16), (80, 24)]
            .into_iter()
            .enumerate()
        {
            runtime.test_process_pty_bytes(format!("line {frame}\r\n").as_bytes());
            let area = Rect::new(0, 0, width, height);
            let reused_cursor = reused.render_terminal(&runtime, area);
            let reused_buffer = reused.buffer().clone();
            let (fresh_buffer, fresh_cursor) = render_terminal_virtual(&runtime, area);
            assert_eq!(
                reused_buffer, fresh_buffer,
                "frame {frame} at {width}x{height}"
            );
            assert_eq!(reused_cursor, fresh_cursor, "frame {frame} cursor");
        }
    }

    /// What reusing the terminal actually bought, measured rather than
    /// reasoned about.
    ///
    /// Ignored by default because it prints tables and times things; run it
    /// with `cargo test --release --bin herdr render_terminal_reuse_cost --
    /// --ignored --nocapture`. Release matters: a debug build spends so much
    /// longer inside `compute_view` that the allocation it removes disappears
    /// into the noise.
    ///
    /// Both columns run the identical `compute_view` and the identical widget
    /// pass — the only difference is whether the ratatui `Terminal` and its
    /// backend are built for this frame or kept from the last one — so the
    /// delta between them is the terminal, and nothing else.
    ///
    /// Two scripts, because a server frame is not one thing. `moving` changes
    /// the pane's output and the focused Space every frame, which is a fleet
    /// under load. `settled` redraws the same screen, which is what the
    /// 58 fps animation loop does most of the time and is where a per-frame
    /// allocation is pure waste.
    #[tokio::test]
    #[ignore = "measurement, not an assertion: run with --ignored --nocapture"]
    async fn render_terminal_reuse_cost() {
        const RUNS: usize = 200;

        fn quantiles(mut samples: Vec<f64>) -> (f64, f64, f64, f64) {
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let at = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
            (at(0.5), at(0.9), at(0.99), samples[samples.len() - 1])
        }

        println!("\n{RUNS} frames per cell, release, one client\n");
        for (script, moving) in [("moving", true), ("settled", false)] {
            println!(
                "{script:>7} | cells  |            fresh terminal per frame |                    reused terminal | saved"
            );
            println!(
                "  size  |        |  med ms  p90 ms  p99 ms   max ms |  med ms  p90 ms  p99 ms   max ms | med ms"
            );
            println!(
                "--------+--------+-----------------------------------+-----------------------------------+-------"
            );
            for (width, height) in [(100u16, 30u16), (200, 60), (390, 120)] {
                let area = Rect::new(0, 0, width, height);

                let (mut state, runtimes, pane) = scripted_app();
                let mut fresh = Vec::with_capacity(RUNS);
                for frame in 0..RUNS {
                    if moving {
                        advance(&mut state, pane, frame);
                    }
                    let started = std::time::Instant::now();
                    let _ = render_virtual_with_runtime_registry(
                        &mut state,
                        &runtimes,
                        area,
                        true,
                        HostCellSize::default(),
                    );
                    fresh.push(started.elapsed().as_secs_f64() * 1000.0);
                }

                let (mut state, runtimes, pane) = scripted_app();
                let mut renderer = VirtualRenderer::default();
                let mut reused = Vec::with_capacity(RUNS);
                for frame in 0..RUNS {
                    if moving {
                        advance(&mut state, pane, frame);
                    }
                    let started = std::time::Instant::now();
                    renderer.render_app(&mut state, &runtimes, area, true, HostCellSize::default());
                    // The caller reads the buffer; the fresh path clones it, so
                    // the reused path has to be charged for a read of it too.
                    std::hint::black_box(renderer.buffer().area);
                    reused.push(started.elapsed().as_secs_f64() * 1000.0);
                }

                let (fm, fp90, fp99, fmax) = quantiles(fresh);
                let (rm, rp90, rp99, rmax) = quantiles(reused);
                println!(
                    "{:>3}x{:<3} | {:>6} | {fm:>7.2} {fp90:>7.2} {fp99:>7.2} {fmax:>8.2} | \
                     {rm:>7.2} {rp90:>7.2} {rp99:>7.2} {rmax:>8.2} | {:>6.2}",
                    width,
                    height,
                    u32::from(width) * u32::from(height),
                    fm - rm,
                );
            }
            println!();
        }
    }

    /// The point of the change: the terminal is built once and kept.
    #[tokio::test]
    async fn reused_terminal_is_built_once() {
        let (mut state, runtimes, pane) = scripted_app();
        let mut reused = VirtualRenderer::default();
        assert!(reused.terminal.is_none(), "nothing is built until a render");

        let area = Rect::new(0, 0, 100, 30);
        reused.render_app(&mut state, &runtimes, area, true, HostCellSize::default());
        let first = reused.buffer().content.as_ptr();

        for frame in 0..5 {
            advance(&mut state, pane, frame);
            reused.render_app(&mut state, &runtimes, area, true, HostCellSize::default());
            assert_eq!(
                reused.buffer().content.as_ptr(),
                first,
                "the backend buffer was reallocated on frame {frame}"
            );
        }
    }
}
