use ratatui::{layout::Rect, Frame};

use super::panes::{compute_pane_infos, render_panes, resize_tab_panes};
use crate::app::state::ViewState;
use crate::app::{AppState, Mode};
use crate::layout::{PaneInfo, SplitBorder};
use crate::protocol::CursorState;
use crate::terminal::TerminalRuntimeRegistry;

pub(crate) struct TabSurfaceLayout {
    pub(crate) pane_infos: Vec<PaneInfo>,
    pub(crate) split_borders: Vec<SplitBorder>,
}

#[derive(Clone, Copy)]
pub(crate) struct TabSurfaceView<'a> {
    pub(crate) pane_infos: &'a [PaneInfo],
    pub(crate) split_borders: &'a [SplitBorder],
}

impl ViewState {
    pub(crate) fn tab_surface(&self) -> TabSurfaceView<'_> {
        TabSurfaceView {
            pane_infos: &self.pane_infos,
            split_borders: &self.split_borders,
        }
    }
}

pub(crate) fn compute_tab_surface(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let split_borders = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| {
            if ws.zoomed {
                Vec::new()
            } else {
                ws.layout.splits(area)
            }
        })
        .unwrap_or_default();
    let pane_infos = compute_pane_infos(app, terminal_runtimes, area, resize_panes, cell_size);

    TabSurfaceLayout {
        pane_infos,
        split_borders,
    }
}

pub(crate) fn resize_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    tab: &crate::workspace::Tab,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    resize_tab_panes(app, terminal_runtimes, tab, area, cell_size);
}

pub(crate) fn render_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
    frame: &mut Frame,
) {
    render_panes(
        app,
        terminal_runtimes,
        frame,
        surface.pane_infos,
        surface.split_borders,
    );
}

pub(crate) fn tab_surface_hyperlinks(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
) -> Vec<((u16, u16), String, String)> {
    let Some(ws_idx) = app.active else {
        return Vec::new();
    };
    if app.workspaces.get(ws_idx).is_none() {
        return Vec::new();
    }

    let mut links = Vec::new();
    for info in surface.pane_infos {
        if let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        {
            let triview = runtime.last_claude_triview_layout();
            links.extend(
                runtime
                    .visible_hyperlinks(info.inner_rect)
                    .into_iter()
                    .filter_map(|((x, y), uri, id)| {
                        Some(((x, project_pane_row(triview, info.inner_rect, y)?), uri, id))
                    }),
            );
        }
    }
    links
}

/// Where a grid-derived pane row was actually **drawn** in `inner_rect`.
///
/// `visible_hyperlinks` and `cursor_state` both read the live grid and offset
/// it by the pane's own origin, which is right whenever a pane row is the grid
/// row of the same index. A Claude triview pane whose log zone has taken rows
/// off the top of the transcript is the one case where it is not: the
/// transcript and composer are drawn `transcript_skip` rows higher than they
/// sit in the grid. Without this the frame cursor lands `transcript_skip` rows
/// below the composer it belongs to — inside the command-log zone, which reads
/// as a second input line hidden under the log.
///
/// `None` when the split did not draw that grid row at all (a cropped composer
/// border, or a transcript row the log zone scrolled off the top).
fn project_pane_row(
    triview: Option<crate::pane::ClaudeTriviewLayout>,
    inner_rect: Rect,
    row: u16,
) -> Option<u16> {
    let Some(layout) = triview else {
        return Some(row);
    };
    let grid_row = row.checked_sub(inner_rect.y)?;
    layout
        .pane_row_for_grid_row(grid_row)
        .map(|pane_row| inner_rect.y + pane_row)
}

pub(crate) fn tab_surface_cursor(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
) -> Option<CursorState> {
    if app.mode != Mode::Terminal {
        return None;
    }

    let ws_idx = app.active?;
    let info = surface.pane_infos.iter().find(|info| info.is_focused)?;
    if !app.pane_exposes_host_cursor(ws_idx, info.id) {
        return None;
    }
    let runtime = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let scrolled_back = super::panes::pane_is_scrolled_back(runtime);
    let reveal = app.reveal_hidden_cursor_for_cjk_ime
        && (!app.cjk_ime_agent_filter_configured || {
            let detected = app
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.terminal_id(info.id))
                .and_then(|terminal_id| app.terminals.get(terminal_id))
                .and_then(|terminal| terminal.detected_agent);
            detected.is_some_and(|agent| app.cjk_ime_agents.contains(&agent))
        });

    let triview = runtime.last_claude_triview_layout();
    if let Some(cursor) = runtime
        .cursor_state(info.inner_rect, true)
        .and_then(|cursor| {
            Some(crate::pane::TerminalCursorState {
                y: project_pane_row(triview, info.inner_rect, cursor.y)?,
                ..cursor
            })
        })
    {
        let visible = if reveal {
            !scrolled_back
        } else {
            cursor.visible && !scrolled_back
        };
        Some(CursorState {
            x: cursor.x,
            y: cursor.y,
            visible,
            shape: if reveal && visible {
                app.cjk_ime_cursor_shape
            } else {
                cursor.shape
            },
        })
    } else if reveal && !scrolled_back {
        Some(CursorState {
            x: info.inner_rect.x,
            y: info.inner_rect.y,
            visible: true,
            shape: app.cjk_ime_cursor_shape,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Direction;
    use ratatui::Terminal;

    /// Claude Code v2.1.241's real screen shape at `cols` x `rows`: a
    /// transcript floating above a full-width rule pair around a `\u{276F} `
    /// composer, then the two footer rows it pins to the literal floor.
    fn claude_screen_bytes(cols: usize, rows: usize) -> Vec<u8> {
        let rule = "\u{2500}".repeat(cols);
        let mut lines: Vec<String> = vec![
            "\u{276F} run the checks".to_string(),
            String::new(),
            "\u{25CF} Running the crate's unit tests".to_string(),
            "  \u{23BF}  $ cargo nextest run --lib".to_string(),
            String::new(),
            "\u{25CF} All green.".to_string(),
        ];
        lines.resize(rows - 5, String::new());
        lines.push(rule.clone());
        lines.push("\u{276F} typed input".to_string());
        lines.push(rule);
        lines.push("  \u{2839} Sonnet 5 5% $0.09".to_string());
        lines.push("  \u{23F5}\u{23F5} accept edits on".to_string());
        let mut bytes = lines.join("\r\n").into_bytes();
        // Park the caret where Claude Code parks it: at the end of the
        // composer's own text, three rows off the floor. Without this the
        // cursor is wherever the last write left it, and the projection under
        // test would never be asked about the composer at all.
        bytes.extend_from_slice(
            format!(
                "\x1b[{};{}H",
                rows - 3,
                "\u{276F} typed input".chars().count() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    /// The captain's bug, at the seam it broke: with commands in the pane's
    /// log the split shifts the composer up, and the frame cursor was still
    /// being reported on the composer's *grid* row — `transcript_skip` rows
    /// further down, inside the command-log zone. On screen that reads as a
    /// second input line hiding behind the command output, with the caret
    /// moving in it as you type.
    ///
    /// Asserted against the row the composer was actually drawn on in this
    /// frame's own buffer, not against the layout's arithmetic, so a split
    /// that moved would move the expectation with it.
    #[tokio::test]
    async fn the_frame_cursor_follows_a_shifted_claude_composer() {
        let cols: u16 = 60;
        let rows: u16 = 20;

        let mut workspace = Workspace::test_new("claude");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        // A placeholder, only so the view resolves; the real screen is written
        // at the size the pane's own chrome leaves it.
        workspace.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(cols, rows, b""),
        );

        let mut app = AppState::test_new();
        let mut terminal_state =
            crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal_state.detected_agent = Some(crate::detect::Agent::Claude);
        app.terminals.insert(terminal_id, terminal_state);
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        for command in ["cargo nextest run", "git status", "ls -la"] {
            app.pane_command_log.record(pane_id, command.to_string());
        }

        let registry = TerminalRuntimeRegistry::default();
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        let area = Rect::new(0, 0, cols, rows);
        crate::ui::compute_view_without_resizing_panes(&mut app, &registry, area);
        let inner = app
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("the focused pane")
            .inner_rect;
        // The agent draws to the pane it was given, so the fixture is written
        // at exactly that size: a grid taller than the pane would be read from
        // its own bottom and every row index would be off by the difference.
        app.workspaces[0].insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                inner.width,
                inner.height,
                &claude_screen_bytes(inner.width as usize, inner.height as usize),
            ),
        );
        crate::ui::compute_view_without_resizing_panes(&mut app, &registry, area);
        terminal
            .draw(|frame| {
                render_tab_surface(&app, &registry, app.view.tab_surface(), frame);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row_text = |y: u16| -> String {
            (0..cols)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        let composer_row = (0..rows)
            .find(|&y| row_text(y).starts_with('\u{276F}') && row_text(y).contains("typed input"))
            .expect("the composer body is on screen");
        // The split really did shift: the log zone is drawn below the
        // composer, so a test that passed by accident on an unshifted pane is
        // ruled out.
        assert!(
            (composer_row + 1..rows).any(|y| row_text(y).starts_with('\u{25CF}')),
            "no command-log zone was drawn, so nothing was shifted"
        );

        let cursor = tab_surface_cursor(&app, &registry, app.view.tab_surface())
            .expect("a focused Claude pane in terminal mode owns the host cursor");
        assert_eq!(
            cursor.y,
            composer_row,
            "the caret is on {:?}, not on the composer it belongs to",
            row_text(cursor.y)
        );
    }

    #[tokio::test]
    async fn explicit_surface_layout_drives_render_cursor_and_hyperlinks() {
        let uri = "https://example.com/surface";
        let mut workspace = Workspace::test_new("shell-workspace");
        let left = workspace.tabs[0].root_pane;
        let right = workspace.test_split(Direction::Horizontal);
        workspace.insert_test_runtime(
            left,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                20,
                8,
                format!("\x1b]8;;{uri}\x1b\\LEFT\x1b]8;;\x1b\\").as_bytes(),
            ),
        );
        workspace.insert_test_runtime(
            right,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 8, b"RIGHT"),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        let full_area = Rect::new(0, 0, 106, 20);
        crate::ui::compute_view(&mut app, full_area);
        let area = app.view.terminal_area;
        assert_eq!(area, Rect::new(26, 1, 80, 19));
        let surface = compute_tab_surface(
            &mut app,
            &TerminalRuntimeRegistry::new(),
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        assert_eq!(surface.pane_infos.len(), 2);
        assert!(!surface.split_borders.is_empty());

        app.view.terminal_area = Rect::new(9, 8, 7, 6);
        app.view.pane_infos.clear();
        app.view.split_borders.clear();

        let surface_view = TabSurfaceView {
            pane_infos: &surface.pane_infos,
            split_borders: &surface.split_borders,
        };
        let mut terminal =
            Terminal::new(TestBackend::new(full_area.width, full_area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_tab_surface(&app, &TerminalRuntimeRegistry::new(), surface_view, frame)
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("LEFT"), "surface: {rendered:?}");
        assert!(rendered.contains("RIGHT"), "surface: {rendered:?}");
        assert!(!rendered.contains("shell-workspace"));

        let links = tab_surface_hyperlinks(&app, &TerminalRuntimeRegistry::new(), surface_view);
        assert!(links
            .iter()
            .any(|(_, symbol, link)| { symbol == "L" && link == uri }));
        assert!(tab_surface_cursor(&app, &TerminalRuntimeRegistry::new(), surface_view,).is_some());
    }

    fn full_app_frame(app: &mut AppState, area: Rect) -> crate::protocol::FrameData {
        let (buffer, cursor) = crate::server::render_stream::render_virtual(app, area, true);
        let hyperlinks =
            crate::server::render_stream::visible_hyperlinks(app, &TerminalRuntimeRegistry::new());
        crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            &buffer,
            cursor,
            &hyperlinks,
        )
    }

    fn frame_digest(frame: &crate::protocol::FrameData) -> String {
        use sha2::{Digest, Sha256};

        let encoded = bincode::serde::encode_to_vec(frame, bincode::config::standard()).unwrap();
        format!("{:x}", Sha256::digest(encoded))
    }

    fn full_app_characterization_state(uri: &str) -> AppState {
        let mut workspace = Workspace::test_new("characterization");
        workspace.identity_cwd = std::path::PathBuf::from("characterization");
        workspace.cached_git_branch = None;
        workspace.cached_git_ahead_behind = None;
        workspace.cached_git_space = None;
        workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(0);
        let left = workspace.tabs[0].root_pane;
        let right = workspace.test_split(Direction::Horizontal);
        workspace.insert_test_runtime(
            left,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                40,
                10,
                format!("\x1b]8;;{uri}\x1b\\LINK\x1b]8;;\x1b\\").as_bytes(),
            ),
        );
        workspace.insert_test_runtime(
            right,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 10, b"RIGHT\r\nPANE"),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app
    }

    #[tokio::test]
    async fn desktop_full_app_semantic_frame_is_characterized() {
        let uri = "https://example.com/full-app";
        let mut app = full_app_characterization_state(uri);
        let frame = full_app_frame(&mut app, Rect::new(0, 0, 106, 20));

        assert_eq!((frame.width, frame.height), (106, 20));
        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 26, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 0, 80, 1));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 80, 19));
        assert_eq!(app.view.pane_infos.len(), 2);
        assert!(!app.view.split_borders.is_empty());
        assert!(frame.cursor.is_some());
        assert_eq!(frame.hyperlinks, vec![uri.to_owned()]);
        // The Agents panel is gone and the Spaces tree now owns the whole
        // sidebar column, which moved this digest: the separator row, the
        // panel header and every agent row below them are no longer drawn, and
        // the Spaces list runs to the bottom instead of stopping at the split.
        // It moved again when the `spaces` title and the second-mate drop-down
        // were removed: the header row is still there but is now empty, so
        // every tree row draws one row higher than it did.
        // It moved once more when the sidebar divider grew an at-rest grip:
        // three rows in the middle of the bar now carry a lighter foreground.
        // The glyphs are untouched, so this is purely a style move.
        // It moved again when the state alphabet became ASCII (`state_mark`):
        // every state glyph on this frame is a different character, and a pane
        // that is not an agent now draws a blank where it used to draw `·`.
        // Geometry is untouched — the mark still occupies exactly one cell.
        // It moved a final time when a Space's default rows became the body
        // registers (`crate::ui::sidebar::body_register`): line two of every
        // Space row is now `<body type> · <N> files · <N> moons` where it was
        // the branch and its ahead/behind counts, and line three is the orbit
        // register where there was no line three.
        assert_eq!(
            frame_digest(&frame),
            "4517627a4666195d6af36e7a5efda57d287d9f6fecf253ae595be996a08c0224"
        );
    }

    #[tokio::test]
    async fn mobile_full_app_semantic_frame_is_characterized() {
        let mut app = full_app_characterization_state("https://example.com/mobile");
        app.mode = Mode::Navigate;
        let frame = full_app_frame(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!((frame.width, frame.height), (44, 20));
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 44, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(frame.cursor, None);
        assert_eq!(
            frame_digest(&frame),
            "eab35c713421e44d9672e664b4ebbab2340a521ff657cbcaf394f84008fda5a8"
        );
    }

    /// The mobile switcher's tab rows carry a rolled-up agent state dot, which
    /// moved the mobile frame digest. Turning the decoration off must reproduce
    /// the pre-decoration frame exactly, so the digest above differs from this
    /// one by the dot and nothing else.
    #[tokio::test]
    async fn mobile_frame_without_tab_state_dots_is_unchanged() {
        let mut app = full_app_characterization_state("https://example.com/mobile");
        app.mode = Mode::Navigate;
        app.show_tab_state_dots = crate::config::TabDecorationConfig::Never;
        let frame = full_app_frame(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(
            frame_digest(&frame),
            "b71abf761edf09b82d730df08edf2eebb057e529aebd03e7a6a53e4a5ea0a7c8"
        );
    }
}
