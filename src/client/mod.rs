//! Thin client mode — connects to the server's client socket.
//!
//! The client:
//! - Connects to `herdr-client.sock`, sends Hello with terminal size and protocol version
//! - Sets up the real terminal (raw mode, mouse capture, keyboard enhancements)
//! - Receives Frame messages and blits them to the terminal (diff against last frame)
//! - Reads stdin events (keystrokes, mouse, paste) and sends them as ClientMessage::Input
//! - Detects terminal resize and sends ClientMessage::Resize
//! - Restores terminal on exit (normal or error)
//! - Handles ServerShutdown gracefully (clean exit, informative message to stderr)
//! - Handles server unreachable (clear error screen, not blank/hang)
//! - Forwards OSC 52 clipboard writes from server to its own stdout
//! - Displays sound/toast notifications forwarded from server

mod input;

use std::collections::HashSet;
use std::io::{self, BufRead, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(unix)]
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
#[cfg(not(windows))]
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use crossterm::terminal::{DisableLineWrap, EnableLineWrap};
use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tracing::{debug, info, warn};

use crate::ipc::LocalStream;
use crate::protocol::render_ansi;
#[cfg(unix)]
use crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD;
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientKeybindings, ClientLaunchMode,
    ClientMessage, NotifyKind, RenderEncoding, ServerMessage, MAX_FRAME_SIZE,
    MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};
use crate::server::socket_paths::client_socket_path;

static RECEIVED_KITTY_GRAPHICS_IDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

struct ClientLoopConfig {
    sound_config: crate::config::SoundConfig,
    mouse_scroll_lines: usize,
    redraw_on_focus_gained: bool,
    host_cursor: crate::config::HostCursorModeConfig,
    kitty_graphics_enabled: bool,
    mouse_capture_active: bool,
    #[cfg(unix)]
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    /// `[experimental] sidebar_card_font`, for `image_card::rasterise_card_scene`.
    /// Unused unless `wants_client_rasterized_cards` was set in Hello.
    card_font_override: Option<String>,
}

/// State tracking for the thin client.
struct ClientState {
    /// Stateful semantic-frame encoder used when the server sends FrameData.
    blit_encoder: render_ansi::BlitEncoder,
    /// Whether host mouse capture is currently active.
    mouse_capture_active: bool,
    /// Whether the host terminal currently reports all keys as Kitty sequences.
    keyboard_report_all_active: bool,
    /// The terminal size we reported to the server in our last Hello/Resize.
    reported_size: (u16, u16),
    /// Client-local sound playback config, refreshed on server request.
    sound_config: crate::config::SoundConfig,
    /// Whether this client may write Kitty graphics bytes to its host terminal.
    kitty_graphics_enabled: bool,
    /// Direct attach prefix escape state. None for full-app clients.
    attach_escape: Option<AttachEscapeState>,
    /// Rows scrolled for one direct-attach wheel notch.
    #[cfg(unix)]
    mouse_scroll_lines: usize,
    /// Local-client shortcut that sends a clipboard image to a remote Herdr session.
    #[cfg(unix)]
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    /// Whether outer focus gain should force a full host-terminal redraw.
    redraw_on_focus_gained: bool,
    /// Whether the next semantic frame must repaint every cell without clearing the surface.
    repaint_pending: bool,
    /// Whether this client draws the cursor into frame cells instead of using the host cursor.
    draw_host_cursor: bool,
    /// This client's own cell size, kept in step with `ClientLoopEvent::Resize`
    /// for `image_card::rasterise_card_scene`. Unused unless
    /// `wants_client_rasterized_cards` was set in Hello.
    cell_size: crate::kitty_graphics::HostCellSize,
    /// `[experimental] sidebar_card_font`, for `image_card::rasterise_card_scene`.
    card_font_override: Option<String>,
    /// This client's own rasterisation cache for `ServerMessage::CardScene`,
    /// mirroring `ClientConnection::graphics_cache` server-side. `Default`
    /// (empty) clients that never requested `CardScene` never touch it.
    card_scene_cache: crate::kitty_graphics::HostGraphicsCache,
    /// This client's own last rasterised card layers, so a card whose content
    /// did not change is carried forward without being redrawn — mirrors the
    /// server-side embed path's `previous: &[SidebarCardLayer]`.
    previous_card_layers: Vec<crate::ui::sidebar::image_card::SidebarCardLayer>,
    /// Kitty graphics bytes from the most recently rasterised `CardScene`,
    /// not yet spliced into an outgoing frame. Taken and cleared the next
    /// time a frame is written, mirroring the server's own
    /// `insert_graphics_before_sync_end` splice point.
    pending_card_graphics: Vec<u8>,
    /// This client's own rasterisation cache for `ServerMessage::TrayScene`.
    /// Separate from `card_scene_cache` rather than shared with it: the
    /// encoder deletes the images of every layer source absent from the pass
    /// it is handed, so one cache across two separately-encoded surfaces would
    /// have each surface delete the other.
    tray_scene_cache: crate::kitty_graphics::HostGraphicsCache,
    /// The last `TrayScene` this client drew, so a scene that says exactly what
    /// the one before it said is not rasterised twice — the client's own half
    /// of the server's `signal_tray_graphics_key`.
    previous_tray_scene: Option<crate::ui::TrayScene>,
    /// The badge artwork this client last actually handed its terminal.
    ///
    /// `previous_tray_scene` above stops an *identical* scene being drawn
    /// twice; this stops a scene that is different only in ways nobody can see
    /// from costing the terminal a whole new image. On an idle fleet that is
    /// almost every scene, and this is the client's own half of the server's
    /// `AppState::signal_tray_published` — see
    /// [`crate::app::state::PublishedSurfaceRaster`].
    published_tray_raster: crate::app::state::PublishedSurfaceRaster,
    /// Kitty graphics bytes from rasterised `TrayScene`s not yet spliced into
    /// an outgoing frame. Appended rather than replaced, because these are the
    /// encoder's *deltas* against `tray_scene_cache`: dropping an unflushed one
    /// would leave the cache believing an upload happened that never went out.
    pending_tray_graphics: Vec<u8>,
    /// This client's own rasterisation cache for `ServerMessage::BackgroundScene`.
    /// Shared between the ambient loop and its effects overlay — unlike cards
    /// and tray, which are two independent, uncoordinated message streams, the
    /// two background layers always arrive and are re-encoded together from
    /// the same wire message, so one cache never sees the other's surface
    /// missing from a pass it did not itself produce.
    background_scene_cache: crate::kitty_graphics::HostGraphicsCache,
    /// The ambient raster this client last actually handed its terminal, so a
    /// scene whose orbits drifted by an imperceptible amount is not a fresh
    /// upload — this client's own half of the server's
    /// `AppState::background_scene`/[`crate::app::state::PublishedSurfaceRaster`]
    /// bargain that already exists for the tray.
    published_background_scene_raster: crate::app::state::PublishedSurfaceRaster,
    /// The ambient layer last actually handed to the encoder, reused verbatim
    /// on a pass `published_background_scene_raster` refused: the ambient
    /// surface is always present in `encode_background_scene_graphics`'s
    /// placements (unlike the effects overlay, which is meant to be dropped
    /// once nothing is live), so refusing a drifted frame must still hand the
    /// encoder *something* rather than let the surface read as withdrawn.
    previous_background_ambient_layer: Option<crate::app::state::GraphicsLayer>,
    /// Kitty graphics bytes from a rasterised `BackgroundScene` not yet
    /// spliced into an outgoing frame.
    pending_background_scene_graphics: Vec<u8>,
}

#[derive(Debug, Default)]
#[cfg(windows)]
struct AttachEscapeState;

#[derive(Debug, Default)]
#[cfg(unix)]
struct AttachEscapeState {
    pending_prefix: bool,
}

#[derive(Debug)]
#[cfg(unix)]
enum AttachInputAction {
    Forward(Vec<u8>),
    Scroll {
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    },
    Detach,
    None,
}

impl AttachEscapeState {
    #[cfg(unix)]
    fn filter_input(
        &mut self,
        data: Vec<u8>,
        viewport_rows: u16,
        mouse_scroll_lines: usize,
    ) -> AttachInputAction {
        const PREFIX: u8 = 0x02; // Ctrl+B

        let mut output = Vec::with_capacity(data.len());
        for byte in data {
            if self.pending_prefix {
                self.pending_prefix = false;
                match byte {
                    b'q' => return AttachInputAction::Detach,
                    PREFIX => output.push(PREFIX),
                    other => {
                        output.push(PREFIX);
                        output.push(other);
                    }
                }
                continue;
            }

            if byte == PREFIX {
                self.pending_prefix = true;
            } else {
                output.push(byte);
            }
        }

        if output.is_empty() {
            AttachInputAction::None
        } else if let Some(action) =
            attach_scroll_action(&output, viewport_rows, mouse_scroll_lines)
        {
            action
        } else {
            AttachInputAction::Forward(output)
        }
    }
}

#[cfg(unix)]
fn attach_scroll_action(
    data: &[u8],
    viewport_rows: u16,
    mouse_scroll_lines: usize,
) -> Option<AttachInputAction> {
    let mut events = crate::raw_input::parse_raw_input_bytes_sync(data);
    if events.len() != 1 {
        return None;
    }

    match events.pop()? {
        crate::raw_input::RawInputEvent::Mouse(mouse) => {
            let direction = match mouse.kind {
                MouseEventKind::ScrollUp => AttachScrollDirection::Up,
                MouseEventKind::ScrollDown => AttachScrollDirection::Down,
                _ => return Some(AttachInputAction::None),
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::Wheel,
                direction,
                lines: mouse_scroll_lines.max(1).min(u16::MAX as usize) as u16,
                column: Some(mouse.column),
                row: Some(mouse.row),
                modifiers: mouse.modifiers.bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
        {
            let direction = match key.code {
                KeyCode::PageUp => AttachScrollDirection::Up,
                KeyCode::PageDown => AttachScrollDirection::Down,
                _ => return None,
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::PageKey {
                    input: data.to_vec(),
                },
                direction,
                lines: viewport_rows.saturating_sub(1).max(1),
                column: None,
                row: None,
                modifiers: KeyModifiers::empty().bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && key.kind == KeyEventKind::Release
                && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) =>
        {
            Some(AttachInputAction::None)
        }
        _ => None,
    }
}

impl ClientState {
    fn request_repaint(&mut self) {
        self.repaint_pending = true;
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur during client operation.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the server's client socket.
    ConnectionFailed(io::Error),
    /// Server rejected our handshake.
    HandshakeRejected { version: u32, error: String },
    /// Server shut down.
    ServerShutdown { reason: Option<String> },
    /// Lost connection to the server.
    ConnectionLost(io::Error),
    /// Protocol error (framing, deserialization).
    Protocol(protocol::FramingError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(err) => {
                write!(f, "failed to connect to server: {err}")?;
                let path = client_socket_path();
                write!(
                    f,
                    "\nIs herdr server running? Start it with `herdr server`."
                )?;
                write!(f, "\nSocket path: {}", path.display())
            }
            ClientError::HandshakeRejected { version, error } => {
                write!(f, "server rejected handshake (version {version}): {error}")
            }
            ClientError::ServerShutdown { reason } => {
                match reason.as_deref() {
                    Some("detached") => {
                        if let Ok(reattach_command) =
                            std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                        {
                            write!(f, "detached from remote server")?;
                            write!(f, "\nRun `{reattach_command}` to reattach")?;
                        } else {
                            write!(f, "detached from server")?;
                            write!(
                                f,
                                "\nRun `{}` to reattach",
                                crate::session::local_attach_command()
                            )?;
                        }
                    }
                    _ => {
                        write!(f, "server shut down")?;
                        if let Some(reason) = reason {
                            write!(f, ": {reason}")?;
                        }
                    }
                }
                Ok(())
            }
            ClientError::ConnectionLost(err) => {
                if let Ok(reattach_command) = std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                {
                    write!(f, "lost connection to remote Herdr: {err}")?;
                    write!(f, "\nIf the remote server survived the SSH or network drop, its panes may still be running.")?;
                    write!(f, "\nRun `{reattach_command}` to reattach")
                } else {
                    write!(f, "lost connection to server: {err}")
                }
            }
            ClientError::Protocol(err) => {
                write!(f, "protocol error: {err}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(err) => Some(err),
            ClientError::ConnectionLost(err) => Some(err),
            ClientError::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<protocol::FramingError> for ClientError {
    fn from(err: protocol::FramingError) -> Self {
        match err {
            protocol::FramingError::UnexpectedEof => ClientError::ConnectionLost(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed connection",
            )),
            protocol::FramingError::Io(err) => ClientError::ConnectionLost(err),
            err => ClientError::Protocol(err),
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
fn setup_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. It enables mouse capture
/// so wheel events can drive the attached viewport or be forwarded to child
/// programs that requested mouse input.
fn setup_direct_attach_terminal() -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(false, true)
}

fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
) -> io::Result<TerminalGuard> {
    ratatui::init();
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let host_color_scheme_reports =
        should_enable_host_color_scheme_reports(enable_client_protocols);

    if enable_client_protocols {
        if mouse_capture {
            set_mouse_capture(true)?;
        } else {
            set_mouse_capture(false)?;
        }
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        if host_color_scheme_reports {
            write_host_color_scheme_report_mode(&mut io::stdout(), true)?;
        }
        push_keyboard_enhancement_flags()?;
    } else {
        if should_query_host_terminal_theme() {
            write_host_color_scheme_report_mode(&mut io::stdout(), false)?;
        }
        if mouse_capture {
            set_mouse_capture(true)?;
        } else {
            set_mouse_capture(false)?;
        }
    }

    #[cfg(windows)]
    let windows_virtual_terminal_input =
        if enable_client_protocols && windows_vti_input_backend_enabled() {
            enable_windows_virtual_terminal_input()
        } else {
            WindowsVirtualTerminalInputSetup::default()
        };

    #[cfg(windows)]
    if enable_client_protocols
        && windows_vti_input_backend_enabled()
        && windows_virtual_terminal_input.active
        && windows_win32_input_mode_enabled()
    {
        if let Err(err) = enable_windows_win32_input_mode(&mut io::stdout()) {
            if let Some(mode) = windows_virtual_terminal_input.restore_mode {
                restore_windows_input_mode_value(mode);
            }
            return Err(err);
        }
    }

    let modify_other_keys_mode = enable_client_protocols
        .then(crate::input::host_modify_other_keys_mode)
        .flatten();
    if let Some(mode) = modify_other_keys_mode {
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    execute!(io::stdout(), DisableLineWrap)?;

    Ok(TerminalGuard {
        reset_modify_other_keys: modify_other_keys_mode.is_some(),
        reset_host_color_scheme_reports: host_color_scheme_reports,
        #[cfg(windows)]
        restore_windows_input_mode: windows_virtual_terminal_input.restore_mode,
    })
}

fn should_enable_host_color_scheme_reports(enable_client_protocols: bool) -> bool {
    enable_client_protocols && should_query_host_terminal_theme()
}

/// Guard that restores the terminal when dropped.
struct TerminalGuard {
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    #[cfg(windows)]
    restore_windows_input_mode: Option<u32>,
}

fn write_host_color_scheme_report_mode(
    writer: &mut impl io::Write,
    enabled: bool,
) -> io::Result<()> {
    let sequence = if enabled {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE
    } else {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE
    };
    writer.write_all(sequence.as_bytes())?;
    writer.flush()
}

fn write_terminal_restore_postlude(
    writer: &mut impl io::Write,
    reset_host_color_scheme_reports: bool,
) -> io::Result<()> {
    if reset_host_color_scheme_reports {
        writer.write_all(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        )?;
    }
    // Restore a visible cursor and reset DECSCUSR back to the terminal default.
    writer.write_all(b"\x1b[?25h\x1b[0 q")?;
    writer.flush()
}

fn should_draw_host_cursor(mode: crate::config::HostCursorModeConfig) -> bool {
    match mode {
        crate::config::HostCursorModeConfig::Auto => {
            crate::platform::should_draw_host_cursor_by_default()
        }
        crate::config::HostCursorModeConfig::Native => false,
        crate::config::HostCursorModeConfig::Drawn => true,
    }
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsVirtualTerminalInputSetup {
    active: bool,
    restore_mode: Option<u32>,
}

#[cfg(windows)]
fn enable_windows_virtual_terminal_input() -> WindowsVirtualTerminalInputSetup {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
        STD_INPUT_HANDLE,
    };

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        tracing::warn!("failed to get Windows console input handle for VT input");
        return WindowsVirtualTerminalInputSetup::default();
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        tracing::warn!("failed to read Windows console input mode for VT input");
        return WindowsVirtualTerminalInputSetup::default();
    }

    let desired = windows_virtual_terminal_input_mode(mode);
    if desired == mode {
        return WindowsVirtualTerminalInputSetup {
            active: true,
            restore_mode: None,
        };
    }

    if unsafe { SetConsoleMode(handle, desired) } == 0 {
        tracing::warn!("failed to enable Windows virtual terminal input");
        return WindowsVirtualTerminalInputSetup::default();
    }

    let mut applied = 0;
    if unsafe { GetConsoleMode(handle, &mut applied) } == 0 {
        tracing::warn!("failed to verify Windows virtual terminal input mode");
        let _ = unsafe { SetConsoleMode(handle, mode) };
        return WindowsVirtualTerminalInputSetup::default();
    }
    if applied & ENABLE_VIRTUAL_TERMINAL_INPUT == 0 {
        tracing::warn!("Windows virtual terminal input bit did not stick");
        let _ = unsafe { SetConsoleMode(handle, mode) };
        return WindowsVirtualTerminalInputSetup::default();
    }

    WindowsVirtualTerminalInputSetup {
        active: true,
        restore_mode: Some(mode),
    }
}

#[cfg(windows)]
fn windows_vti_input_backend_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_BACKEND")
        .map(|backend| !backend.eq_ignore_ascii_case("crossterm"))
        .unwrap_or(true)
}

#[cfg(any(windows, test))]
fn windows_virtual_terminal_input_mode(mode: u32) -> u32 {
    mode | 0x0200
}

#[cfg(windows)]
fn restore_windows_input_mode_value(mode: u32) {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE};

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        tracing::warn!("failed to restore Windows console input mode");
    }
}

fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        match execute!(io::stdout(), DisableMouseCapture) {
            Ok(()) => Ok(()),
            #[cfg(windows)]
            Err(err) if err.to_string() == "Initial console modes not set" => Ok(()),
            Err(err) => Err(err),
        }
    }
}

fn restore_terminal_state(
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    #[cfg(windows)] restore_windows_input_mode: Option<u32>,
) {
    let _ = clear_received_kitty_graphics(&mut io::stdout());

    // Reset modifyOtherKeys if we enabled it.
    if reset_modify_other_keys {
        let _ = io::stdout().write_all(b"\x1b[>4;0m");
        let _ = io::stdout().flush();
    }

    let _ = pop_keyboard_enhancement_flags();

    let _ = execute!(
        io::stdout(),
        EnableLineWrap,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture
    );
    let _ = crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout());
    #[cfg(windows)]
    if let Some(mode) = restore_windows_input_mode {
        restore_windows_input_mode_value(mode);
    }

    ratatui::restore();
    let _ = write_terminal_restore_postlude(&mut io::stdout(), reset_host_color_scheme_reports);

    #[cfg(windows)]
    if windows_vti_input_backend_enabled() && windows_win32_input_mode_enabled() {
        let _ = disable_windows_win32_input_mode(&mut io::stdout());
    }
}

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
    )
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn windows_win32_input_mode_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_PROBE")
        .map(|probe| probe.eq_ignore_ascii_case("win32"))
        .unwrap_or(true)
}

#[cfg(windows)]
fn enable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001h")?;
    writer.flush()
}

#[cfg(windows)]
fn disable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001l")?;
    writer.flush()
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(
            self.reset_modify_other_keys,
            self.reset_host_color_scheme_reports,
            #[cfg(windows)]
            self.restore_windows_input_mode,
        );
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// The render encoding this client asks the server for.
///
/// `HERDR_RENDER_ENCODING` is the explicit answer whenever it is set. Otherwise
/// a client that draws a sidebar surface itself asks for `TerminalAnsi`,
/// because that is the only encoding whose arm of the event loop has a splice
/// point for the Kitty bytes it rasterises — `ServerMessage::Terminal` takes
/// `pending_card_graphics` and `pending_tray_graphics`, and
/// `ServerMessage::Frame` never touches them. That is not a preference: on
/// `SemanticFrame` a delegating client rasterises every scene and then throws
/// every byte away, and the surface renders as a hole where its pixels should be.
///
/// This never fires for the real gate. Every Windows `--remote` client is
/// launched with `HERDR_RENDER_ENCODING=terminal-ansi` already set
/// (`crate::remote::bridge`), so the only clients it reaches are the ones that
/// set `HERDR_CLIENT_RASTERIZED_CARDS`, `HERDR_CLIENT_RASTERIZED_SIGNAL_TRAY`,
/// or `HERDR_CLIENT_RASTERIZED_BACKGROUND_SCENE` by hand — the dev-box and CI
/// audience those overrides exist for, who otherwise get a blank surface from
/// the very path they were trying to exercise.
/// [`warn_if_encoding_cannot_carry_delegated_scenes`] covers what is left: an
/// explicit `HERDR_RENDER_ENCODING` that names something else.
fn requested_render_encoding() -> RenderEncoding {
    match std::env::var("HERDR_RENDER_ENCODING").ok().as_deref() {
        Some("terminal-ansi" | "terminal_ansi" | "ansi") => RenderEncoding::TerminalAnsi,
        Some(_) => RenderEncoding::SemanticFrame,
        None if wants_client_rasterized_cards()
            || wants_client_rasterized_signal_tray()
            || wants_client_rasterized_background_scene() =>
        {
            RenderEncoding::TerminalAnsi
        }
        None => RenderEncoding::SemanticFrame,
    }
}

/// Says so out loud when this client will draw a surface it can never show.
///
/// The negotiated encoding is the server's answer, not this client's request,
/// so it is the only place the mismatch is actually knowable — and a blank tray
/// with no explanation is precisely what cost a live verification pass its first
/// run. See [`requested_render_encoding`] for why `SemanticFrame` cannot carry
/// them.
fn warn_if_encoding_cannot_carry_delegated_scenes(
    encoding: RenderEncoding,
    wants_cards: bool,
    wants_signal_tray: bool,
    wants_background_scene: bool,
) {
    if encoding != RenderEncoding::SemanticFrame
        || !(wants_cards || wants_signal_tray || wants_background_scene)
    {
        return;
    }
    warn!(
        wants_cards,
        wants_signal_tray,
        wants_background_scene,
        "negotiated SemanticFrame encoding cannot carry client-rasterised sidebar \
         surfaces: their scenes will be rasterised and dropped, and the surface \
         will render blank. Set HERDR_RENDER_ENCODING=terminal-ansi."
    );
}

/// The cell this client rasterises delegated scenes against.
///
/// Through the same gate the server puts the very same reported numbers through
/// on the way in (`ClientConnection::set_cell_size` →
/// `HostCellSize::plausible_or_unknown`), because a delegating client is the
/// one place the two copies have to agree: `build_card_scene` counts the
/// layout out server-side against the server's copy, and `rasterise_card_scene`
/// turns it into pixels here against this one. Everything sized in pixels comes
/// out of this — a card's frame, its type, and the half-cell the tree's rails
/// are offset by to sit where a box-drawing glyph's stem sits
/// (`RAIL_INK_COLUMN_FRACTION`).
///
/// A Windows client over the remote bridge reports an arithmetic `3x7` that
/// fails `is_plausible` and becomes the `8x16` fallback server-side. Kept raw
/// here, the same scene was laid out on a 8x16 grid and drawn on a 3x7 one:
/// under half the width per column, and rails offset 1.5px into a column the
/// layout put them 4px into.
///
/// Unknown stays unknown, exactly as it does server-side: a client with Kitty
/// graphics off reports `0x0` and that absence means "no graphics", not "a
/// wrong measurement".
fn rasterisation_cell_size(width_px: u32, height_px: u32) -> crate::kitty_graphics::HostCellSize {
    crate::kitty_graphics::HostCellSize {
        width_px,
        height_px,
    }
    .plausible_or_unknown()
}

fn is_remote_client_process() -> bool {
    std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR).is_ok()
}

/// Windows remote-bridge clients rasterise sidebar cards themselves from
/// `ServerMessage::CardScene` rather than receiving server-embedded card
/// pixels, since the rest of the TUI already rides the unchanged
/// `TerminalAnsi` encoding over that bridge. Native local Windows clients and
/// all Unix clients keep the existing server-rasterized path.
///
/// `HERDR_CLIENT_RASTERIZED_CARDS` overrides the platform gate, the same way
/// `HERDR_RENDER_ENCODING` already overrides the encoding negotiation — there
/// is no Windows hardware to exercise the real gate from a Unix dev box or CI,
/// so this is how a Unix client is driven through the same code path for
/// testing.
/// Whether this process draws sidebar cards itself, for anything outside the
/// client loop that needs to know.
///
/// The one caller is `crate::gpu::enabled`: whether to hold a GPU queue open for
/// card rasterisation is exactly the question of whether this process
/// rasterises cards at all, and a server rasterising them *for* clients answers
/// no. Kept as a re-export of [`wants_client_rasterized_cards`] rather than a
/// second gate, because two gates would be two answers.
pub(crate) fn rasterises_cards_locally() -> bool {
    wants_client_rasterized_cards()
}

fn wants_client_rasterized_cards() -> bool {
    match std::env::var("HERDR_CLIENT_RASTERIZED_CARDS")
        .ok()
        .as_deref()
    {
        Some("1" | "true") => return true,
        Some("0" | "false") => return false,
        _ => {}
    }
    cfg!(windows) && is_remote_client_process()
}

/// The same bargain as [`wants_client_rasterized_cards`], for the sidebar's
/// signal tray: a Windows remote-bridge client draws the eight badges itself
/// from a `ServerMessage::TrayScene` instead of being sent an RGBA image of
/// them on every step of their animation.
///
/// Deliberately the *same* platform gate rather than a wider one — whether that
/// gate should be broader at all is one question about both surfaces, not a
/// different question about each. `HERDR_CLIENT_RASTERIZED_SIGNAL_TRAY`
/// overrides it for the same reason its card sibling has an override: there is
/// no Windows hardware to exercise the real gate from a Unix dev box or CI.
fn wants_client_rasterized_signal_tray() -> bool {
    match std::env::var("HERDR_CLIENT_RASTERIZED_SIGNAL_TRAY")
        .ok()
        .as_deref()
    {
        Some("1" | "true") => return true,
        Some("0" | "false") => return false,
        _ => {}
    }
    cfg!(windows) && is_remote_client_process()
}

/// The same bargain as [`wants_client_rasterized_cards`], for the whole-terminal ambient
/// background scene: a Windows remote-bridge client draws its own orbits, asteroids and comets
/// from a `ServerMessage::BackgroundScene` instead of being sent an RGBA image of them every time
/// the scene's own simulation loop advances — the loop that currently runs on the shared Linux
/// server and competes with everything else running there for CPU, regardless of how idle the
/// captain's own Windows machine is.
///
/// Deliberately the *same* platform gate rather than a wider one, for the reason
/// [`wants_client_rasterized_signal_tray`] gives. `HERDR_CLIENT_RASTERIZED_BACKGROUND_SCENE`
/// overrides it for the same reason its card and tray siblings have an override.
fn wants_client_rasterized_background_scene() -> bool {
    match std::env::var("HERDR_CLIENT_RASTERIZED_BACKGROUND_SCENE")
        .ok()
        .as_deref()
    {
        Some("1" | "true") => return true,
        Some("0" | "false") => return false,
        _ => {}
    }
    cfg!(windows) && is_remote_client_process()
}

/// Time to wait for the server's Welcome reply during the handshake.
///
/// A local client talks to an already-connected server, so 5s is plenty. The
/// remote bridge client (`herdr --remote`) sits behind a fresh per-attach ssh
/// connection whose cold-connect (TCP + key exchange + auth) happens inside this
/// window; on a high-latency link that easily exceeds 5s, so it gets a far
/// larger budget. See issue #753.
const LOCAL_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const REMOTE_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn handshake_read_timeout() -> Duration {
    #[cfg(unix)]
    if is_remote_client_process() {
        return REMOTE_HANDSHAKE_READ_TIMEOUT;
    }
    LOCAL_HANDSHAKE_READ_TIMEOUT
}

fn requested_keybindings() -> ClientKeybindings {
    match std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR)
        .ok()
        .as_deref()
    {
        Some("local") => crate::config::Config::load()
            .config
            .local_keybindings_profile_toml()
            .map(|keys_toml| ClientKeybindings::Local { keys_toml })
            .unwrap_or(ClientKeybindings::Server),
        _ => ClientKeybindings::Server,
    }
}

#[cfg(windows)]
fn set_handshake_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    context: &'static str,
) -> Result<(), ClientError> {
    match stream.set_recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            debug!(err = %err, context, "client socket receive timeout unavailable");
            Ok(())
        }
        Err(err) => Err(ClientError::ConnectionFailed(err)),
    }
}

#[cfg(not(windows))]
fn set_handshake_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    _context: &'static str,
) -> Result<(), ClientError> {
    stream
        .set_recv_timeout(timeout)
        .map_err(ClientError::ConnectionFailed)
}

/// Performs the client→server handshake.
///
/// Sends Hello with the terminal size and protocol version, reads the Welcome
/// response. Returns Ok(()) on success, or an error if the server rejects us.
fn do_handshake(
    stream: &mut LocalStream,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
) -> Result<RenderEncoding, ClientError> {
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    let wants_cards = wants_client_rasterized_cards();
    let wants_signal_tray = wants_client_rasterized_signal_tray();
    let wants_background_scene = wants_client_rasterized_background_scene();

    // Send Hello.
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        keybindings: requested_keybindings(),
        launch_mode: if direct_attach_requested {
            ClientLaunchMode::TerminalAttach
        } else {
            ClientLaunchMode::App
        },
        // The host-capability probe: this client process *is* the process
        // attached to the terminal, so its own environment is the terminal's
        // — unlike the server's, which may not be co-located. See
        // `crate::kitty_graphics::host_terminal_report_from_env`.
        host_terminal: crate::kitty_graphics::host_terminal_report_from_env(),
        wants_client_rasterized_cards: wants_cards,
        wants_client_rasterized_signal_tray: wants_signal_tray,
        wants_client_rasterized_background_scene: wants_background_scene,
    };
    protocol::write_message(stream, &hello)
        .map_err(|e| ClientError::ConnectionFailed(io::Error::other(e.to_string())))?;

    // Read Welcome.
    set_handshake_recv_timeout(
        stream,
        Some(handshake_read_timeout()),
        "client handshake read timeout unavailable",
    )?;
    let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)?;
    set_handshake_recv_timeout(
        stream,
        None,
        "failed to clear client handshake read timeout",
    )?;

    match welcome {
        ServerMessage::Welcome {
            version,
            encoding,
            error,
        } => {
            if let Some(error) = error {
                return Err(ClientError::HandshakeRejected { version, error });
            }
            info!(version, ?encoding, "handshake succeeded");
            warn_if_encoding_cannot_carry_delegated_scenes(
                encoding,
                wants_cards,
                wants_signal_tray,
                wants_background_scene,
            );
            Ok(encoding)
        }
        _ => Err(ClientError::Protocol(protocol::FramingError::Io(
            io::Error::new(io::ErrorKind::InvalidData, "expected Welcome message"),
        ))),
    }
}

// ---------------------------------------------------------------------------
// Client event loop
// ---------------------------------------------------------------------------

/// Internal events for the client event loop.
enum ClientLoopEvent {
    /// Raw input bytes from stdin.
    #[cfg(unix)]
    StdinInput(Vec<u8>),
    /// Structured input events from platforms without Unix-style stdin bytes.
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    /// The host terminal's answer to `CSI 16 t`, on platforms whose stdin path
    /// hands the loop semantic events rather than raw bytes. The Unix arm reads
    /// the same reply straight out of the byte stream it is already parsing.
    #[cfg(windows)]
    HostCellSizeReported { width_px: u32, height_px: u32 },
    /// Terminal resize detected.
    Resize(u16, u16, u32, u32),
    /// Server message received.
    ServerMessage(ServerMessage),
    /// Server reader thread exited (connection lost).
    ServerDisconnected,
    /// Timer tick.
    Timer,
}

/// Runs the thin client: connects to the server, performs the handshake,
/// and enters the main event loop.
///
/// This is the entry point called from `main.rs` when running in client mode.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(
        requested_render_encoding(),
        None,
        None,
        "connecting to server",
    )
}

/// Runs a direct terminal attach client.
#[cfg(unix)]
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        RenderEncoding::TerminalAnsi,
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
    )
}

/// Direct terminal attach is Unix raw-byte input only until Windows gets a semantic attach path.
#[cfg(windows)]
pub fn run_terminal_attach(_terminal_id: String, _takeover: bool) -> io::Result<()> {
    debug_assert!(!crate::platform::capabilities().direct_terminal_attach);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "direct terminal attach is not supported on Windows yet",
    ))
}

/// Runs a read-only terminal session observer and prints one JSON envelope per frame.
pub fn run_terminal_session_observe(target: String, cols: u16, rows: u16) -> io::Result<()> {
    let mut stream =
        connect_terminal_session_stream(target.clone(), cols, rows, "observing terminal session")?;
    write_to_server(&mut stream, &ClientMessage::ObserveTerminal { target })?;
    write_terminal_session_output(stream)
}

/// Runs a writable terminal session controller.
pub fn run_terminal_session_control(
    target: String,
    takeover: bool,
    cols: u16,
    rows: u16,
) -> io::Result<()> {
    let mut stream = connect_terminal_session_stream(
        target.clone(),
        cols,
        rows,
        "controlling terminal session",
    )?;
    write_to_server(
        &mut stream,
        &ClientMessage::ControlTerminal { target, takeover },
    )?;

    let mut write_stream = stream.try_clone()?;
    let _input_thread = std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            match terminal_control_command_from_json(&line) {
                Ok(message) => {
                    let release = matches!(message, ClientMessage::Detach);
                    if write_to_server(&mut write_stream, &message).is_err() {
                        return;
                    }
                    if release {
                        return;
                    }
                }
                Err(err) => eprintln!("herdr: terminal session control input ignored: {err}"),
            }
        }
        let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
    });

    write_terminal_session_output(stream)
}

fn connect_terminal_session_stream(
    target: String,
    cols: u16,
    rows: u16,
    log_message: &'static str,
) -> io::Result<LocalStream> {
    init_logging();

    let socket_path = client_socket_path();
    crate::logging::startup("client");
    info!(path = %socket_path.display(), target = %target, cols, rows, "{log_message}");

    let mut stream = match crate::ipc::connect_local_stream(&socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("herdr: {}", ClientError::ConnectionFailed(err));
            std::process::exit(1);
        }
    };

    match do_handshake(
        &mut stream,
        cols,
        rows,
        0,
        0,
        RenderEncoding::TerminalAnsi,
        true,
    ) {
        Ok(RenderEncoding::TerminalAnsi) => {}
        Ok(encoding) => {
            eprintln!(
                "herdr: terminal session observe negotiated unsupported encoding {encoding:?}"
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
    }

    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn write_terminal_session_output(mut stream: LocalStream) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    loop {
        match protocol::read_message(&mut stream, MAX_GRAPHICS_FRAME_SIZE) {
            Ok(ServerMessage::Terminal(frame)) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&frame.bytes);
                let line = serde_json::json!({
                    "type": "terminal.frame",
                    "seq": frame.seq,
                    "encoding": "ansi",
                    "width": frame.width,
                    "height": frame.height,
                    "full": frame.full,
                    "bytes": encoded,
                });
                serde_json::to_writer(&mut stdout, &line)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
            }
            Ok(ServerMessage::ServerShutdown { reason }) => {
                let line = serde_json::json!({
                    "type": "terminal.closed",
                    "reason": reason,
                });
                serde_json::to_writer(&mut stdout, &line)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
                return Ok(());
            }
            Ok(ServerMessage::Graphics { .. }) => {}
            Ok(_) => {}
            Err(protocol::FramingError::UnexpectedEof) => return Ok(()),
            Err(err) => return Err(io::Error::other(err.to_string())),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum TerminalControlCommand {
    #[serde(rename = "terminal.input")]
    Input {
        text: Option<String>,
        bytes: Option<String>,
    },
    #[serde(rename = "terminal.resize")]
    Resize {
        cols: u16,
        rows: u16,
        #[serde(default)]
        cell_width_px: u32,
        #[serde(default)]
        cell_height_px: u32,
    },
    #[serde(rename = "terminal.scroll")]
    Scroll {
        direction: TerminalControlScrollDirection,
        lines: u16,
        #[serde(default)]
        source: TerminalControlScrollSource,
        #[serde(default)]
        column: Option<u16>,
        #[serde(default)]
        row: Option<u16>,
        #[serde(default)]
        modifiers: u8,
    },
    #[serde(rename = "terminal.release")]
    Release {},
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalControlScrollDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalControlScrollSource {
    #[default]
    Wheel,
    PageKey,
}

fn terminal_control_command_from_json(raw: &str) -> Result<ClientMessage, String> {
    let command = serde_json::from_str::<TerminalControlCommand>(raw)
        .map_err(|err| format!("invalid json command: {err}"))?;
    match command {
        TerminalControlCommand::Input { text, bytes } => {
            let data = match (text, bytes) {
                (Some(_), Some(_)) => {
                    return Err("terminal.input accepts text or bytes, not both".into())
                }
                (Some(text), None) => text.into_bytes(),
                (None, Some(bytes)) => base64::engine::general_purpose::STANDARD
                    .decode(bytes)
                    .map_err(|err| format!("invalid terminal.input bytes: {err}"))?,
                (None, None) => Vec::new(),
            };
            Ok(ClientMessage::Input { data })
        }
        TerminalControlCommand::Resize {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        } => {
            if cols == 0 || rows == 0 {
                return Err("terminal.resize cols and rows must be greater than 0".into());
            }
            Ok(ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            })
        }
        TerminalControlCommand::Scroll {
            direction,
            lines,
            source,
            column,
            row,
            modifiers,
        } => {
            if lines == 0 {
                return Err("terminal.scroll lines must be greater than 0".into());
            }
            let direction = match direction {
                TerminalControlScrollDirection::Up => AttachScrollDirection::Up,
                TerminalControlScrollDirection::Down => AttachScrollDirection::Down,
            };
            let source = match source {
                TerminalControlScrollSource::Wheel => AttachScrollSource::Wheel,
                TerminalControlScrollSource::PageKey => AttachScrollSource::PageKey {
                    input: match direction {
                        AttachScrollDirection::Up => b"\x1b[5~".to_vec(),
                        AttachScrollDirection::Down => b"\x1b[6~".to_vec(),
                    },
                },
            };
            Ok(ClientMessage::AttachScroll {
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            })
        }
        TerminalControlCommand::Release {} => Ok(ClientMessage::Detach),
    }
}

fn run_client_with_mode(
    requested_encoding: RenderEncoding,
    attach_request: Option<(String, bool)>,
    attach_escape: Option<AttachEscapeState>,
    log_message: &'static str,
) -> io::Result<()> {
    init_logging();

    let loaded_config = crate::config::Config::load();
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let mouse_capture = loaded_config.config.ui.mouse_capture;
    let mouse_scroll_lines = loaded_config.config.ui.mouse_scroll_lines();
    let redraw_on_focus_gained = loaded_config.config.ui.redraw_on_focus_gained;
    let host_cursor = loaded_config.config.ui.host_cursor;
    let direct_attach_requested = attach_request.is_some();
    #[cfg(unix)]
    let remote_image_paste_key = client_remote_image_paste_key(&loaded_config.config);
    let kitty_graphics_enabled =
        loaded_config.config.experimental.kitty_graphics && !direct_attach_requested;
    let card_font_override = {
        let trimmed = loaded_config.config.experimental.sidebar_card_font.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };
    let loop_config = ClientLoopConfig {
        sound_config: loaded_config.config.ui.sound,
        mouse_scroll_lines,
        redraw_on_focus_gained,
        host_cursor,
        kitty_graphics_enabled,
        mouse_capture_active: mouse_capture,
        #[cfg(unix)]
        remote_image_paste_key,
        card_font_override,
    };

    let socket_path = client_socket_path();
    crate::logging::startup("client");
    info!(path = %socket_path.display(), "{log_message}");

    // Try to connect to the server.
    let mut stream = match crate::ipc::connect_local_stream(&socket_path) {
        Ok(s) => s,
        Err(err) => {
            // Server unreachable — show clear error and exit.
            let client_err = ClientError::ConnectionFailed(err);
            eprintln!("herdr: {client_err}");
            std::process::exit(1);
        }
    };

    // Get the terminal geometry before handshake (before raw mode).
    let (cols, rows, cell_width_px, cell_height_px) =
        initial_terminal_geometry(kitty_graphics_enabled);

    // Perform handshake while the stream is still in blocking mode.
    let negotiated_encoding = match do_handshake(
        &mut stream,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        direct_attach_requested,
    ) {
        Ok(encoding) => encoding,
        Err(err) => {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
    };

    if let Some((terminal_id, takeover)) = attach_request {
        let attach = ClientMessage::AttachTerminal {
            terminal_id,
            takeover,
        };
        if let Err(err) = write_to_server(&mut stream, &attach) {
            eprintln!("herdr: failed to request terminal attach: {err}");
            std::process::exit(1);
        }
    }

    // Now set up the terminal. This must happen AFTER the handshake succeeds,
    // so we don't leave the terminal in raw mode if the server rejects us.
    let direct_attach = attach_escape.is_some();
    let terminal_guard = if direct_attach {
        setup_direct_attach_terminal()
    } else {
        setup_terminal(mouse_capture)
    }
    .map_err(|err| {
        eprintln!("herdr: failed to set up terminal: {err}");
        err
    })?;

    // Install a panic hook to restore the terminal on panic (same as monolithic).
    let panic_resets_modify_other_keys = terminal_guard.reset_modify_other_keys;
    let panic_resets_host_color_scheme_reports = terminal_guard.reset_host_color_scheme_reports;
    #[cfg(windows)]
    let panic_restore_windows_input_mode = terminal_guard.restore_windows_input_mode;
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_state(
            panic_resets_modify_other_keys,
            panic_resets_host_color_scheme_reports,
            #[cfg(windows)]
            panic_restore_windows_input_mode,
        );
        original_hook(info);
    }));

    // Create the tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let should_quit = Arc::new(AtomicBool::new(false));

    // ctrlc's "termination" feature also catches SIGTERM/SIGHUP so direct
    // termination signals still run the quit path and TerminalGuard::Drop.
    let quit_flag = should_quit.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Release);
    }) {
        warn!(%err, "failed to install termination handler; terminal restore relies on TerminalGuard::Drop and the panic hook");
    }

    let result = rt.block_on(async {
        run_client_loop(
            stream,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            should_quit,
            loop_config,
            negotiated_encoding,
            attach_escape,
        )
        .await
    });

    // Restore the terminal before printing any final status message.
    drop(terminal_guard);

    if let Err(err) = result {
        eprintln!("herdr: {err}");
        rt.shutdown_timeout(Duration::from_millis(100));
        crate::logging::shutdown("client");

        if matches!(
            err,
            ClientError::ServerShutdown {
                reason: Some(reason)
            } if reason == "detached"
        ) {
            return Ok(());
        }

        std::process::exit(1);
    }

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("client");
    Ok(())
}

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread → sends raw input bytes to main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
async fn run_client_loop(
    stream: LocalStream,
    cols: u16,
    rows: u16,
    initial_cell_width_px: u32,
    initial_cell_height_px: u32,
    should_quit: Arc<AtomicBool>,
    config: ClientLoopConfig,
    negotiated_encoding: RenderEncoding,
    attach_escape: Option<AttachEscapeState>,
) -> Result<(), ClientError> {
    #[cfg(windows)]
    let _ = config.mouse_scroll_lines;
    let draw_host_cursor = attach_escape.is_none() && should_draw_host_cursor(config.host_cursor);
    #[cfg(unix)]
    let is_remote_client = is_remote_client_process();

    let mut state = ClientState {
        blit_encoder: render_ansi::BlitEncoder::new(),
        mouse_capture_active: config.mouse_capture_active,
        keyboard_report_all_active: false,
        reported_size: (cols, rows),
        sound_config: config.sound_config,
        kitty_graphics_enabled: config.kitty_graphics_enabled,
        attach_escape,
        #[cfg(unix)]
        mouse_scroll_lines: config.mouse_scroll_lines,
        #[cfg(unix)]
        remote_image_paste_key: config.remote_image_paste_key,
        redraw_on_focus_gained: config.redraw_on_focus_gained,
        repaint_pending: false,
        draw_host_cursor,
        cell_size: rasterisation_cell_size(initial_cell_width_px, initial_cell_height_px),
        card_font_override: config.card_font_override,
        card_scene_cache: crate::kitty_graphics::HostGraphicsCache::default(),
        previous_card_layers: Vec::new(),
        pending_card_graphics: Vec::new(),
        tray_scene_cache: crate::kitty_graphics::HostGraphicsCache::default(),
        previous_tray_scene: None,
        published_tray_raster: crate::app::state::PublishedSurfaceRaster::default(),
        pending_tray_graphics: Vec::new(),
        background_scene_cache: crate::kitty_graphics::HostGraphicsCache::default(),
        published_background_scene_raster: crate::app::state::PublishedSurfaceRaster::default(),
        previous_background_ambient_layer: None,
        pending_background_scene_graphics: Vec::new(),
    };
    debug!(?negotiated_encoding, "client render encoding active");
    let host_mouse_capture_active = Arc::new(AtomicBool::new(state.mouse_capture_active));
    // Cell size reported by the host terminal, packed as width<<32 | height.
    // Zero means the host has not reported one.
    let reported_cell_size = Arc::new(AtomicU64::new(0));

    // Channel for events from the stdin, resize, and server reader threads.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);

    // Spawn the stdin reader thread.
    let will_query_host_terminal_theme =
        state.attach_escape.is_none() && should_query_host_terminal_theme();
    // Terminals behind ConPTY report no pixel size through the ioctl, so ask the
    // host terminal directly instead of falling back to an assumed cell size.
    let will_query_host_cell_size = state.attach_escape.is_none()
        && host_cell_size_query_required(state.kitty_graphics_enabled);
    let stdin_quit = should_quit.clone();
    let stdin_tx = event_tx.clone();
    let stdin_mouse_capture_active = host_mouse_capture_active.clone();
    std::thread::spawn(move || {
        input::stdin_reader_loop(
            stdin_tx,
            &stdin_quit,
            will_query_host_terminal_theme,
            will_query_host_cell_size,
            stdin_mouse_capture_active,
        );
    });

    if will_query_host_terminal_theme {
        query_host_terminal_theme();
    }

    if will_query_host_cell_size {
        query_host_cell_size();
    }

    if state.attach_escape.is_none() && state.kitty_graphics_enabled {
        query_kitty_graphics_capability();
        if should_query_host_terminal_version() {
            query_host_terminal_version();
        }
    }

    // Spawn the resize poller thread.
    let resize_quit = should_quit.clone();
    let resize_tx = event_tx.clone();
    let resize_cell_size = reported_cell_size.clone();
    let kitty_graphics_enabled = state.kitty_graphics_enabled;
    std::thread::spawn(move || {
        resize_poll_loop(
            resize_tx,
            cols,
            rows,
            initial_cell_width_px,
            initial_cell_height_px,
            kitty_graphics_enabled,
            &resize_cell_size,
            &resize_quit,
        );
    });

    // Spawn the server reader thread (blocking reads from the socket).
    // Clone the stream's file descriptor so we can read from a blocking stream.
    let server_read_quit = should_quit.clone();
    let server_read_tx = event_tx.clone();
    let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    std::thread::spawn(move || {
        let max_frame_size = if kitty_graphics_enabled {
            MAX_GRAPHICS_FRAME_SIZE
        } else {
            MAX_FRAME_SIZE
        };
        server_reader_thread(
            read_stream,
            server_read_tx,
            &server_read_quit,
            max_frame_size,
        );
    });

    // Use the original stream for writing (blocking is fine since we write
    // from the async loop).
    let mut write_stream = stream;
    write_stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // This (foreground) client owns the prefix ASCII input-source switch
    // (implemented on macOS and Windows; a no-op on other platforms).
    use crate::platform::PrefixInputSource;
    let mut prefix_input_source = crate::platform::RealPrefixInputSource::default();

    // Main event loop.
    while !should_quit.load(Ordering::Acquire) {
        let event = tokio::select! {
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
            _ = tokio::time::sleep(Duration::from_millis(100)) => ClientLoopEvent::Timer,
        };

        match event {
            #[cfg(unix)]
            ClientLoopEvent::StdinInput(data) => {
                let data = if let Some(attach_escape) = &mut state.attach_escape {
                    match attach_escape.filter_input(
                        data,
                        state.reported_size.1,
                        state.mouse_scroll_lines,
                    ) {
                        AttachInputAction::Forward(data) => data,
                        AttachInputAction::Scroll {
                            source,
                            direction,
                            lines,
                            column,
                            row,
                            modifiers,
                        } => {
                            let msg = ClientMessage::AttachScroll {
                                source,
                                direction,
                                lines,
                                column,
                                row,
                                modifiers,
                            };
                            if let Err(e) = write_to_server(&mut write_stream, &msg) {
                                return Err(ClientError::ConnectionLost(e));
                            }
                            continue;
                        }
                        AttachInputAction::Detach => {
                            let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
                            return Ok(());
                        }
                        AttachInputAction::None => continue,
                    }
                } else {
                    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                    if crate::raw_input::events_require_host_surface_redraw(
                        &events,
                        state.redraw_on_focus_gained,
                    ) {
                        state.request_repaint();
                    }
                    if crate::raw_input::events_require_host_terminal_theme_query(&events) {
                        query_host_terminal_theme();
                    }
                    // Recorded here rather than forwarded: the server is told
                    // through the resize path, which is the one message that
                    // already carries a cell size, so the server has exactly one
                    // way to learn it instead of two that can disagree.
                    if let Some((width_px, height_px)) = reported_cell_size_from_events(&events) {
                        store_reported_cell_size(&reported_cell_size, width_px, height_px);
                    }
                    data
                };
                if should_bridge_clipboard_image_paste(
                    &data,
                    is_remote_client,
                    state.remote_image_paste_key,
                ) {
                    if let Some(image) = crate::platform::read_clipboard_image() {
                        if image.bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                            warn!(
                                bytes = image.bytes.len(),
                                max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                                "local clipboard image is too large to bridge"
                            );
                            continue;
                        }
                        info!(
                            bytes = image.bytes.len(),
                            extension = image.extension,
                            "bridging local clipboard image paste to remote server"
                        );
                        let msg = ClientMessage::ClipboardImage {
                            extension: image.extension.to_owned(),
                            data: image.bytes,
                        };
                        if let Err(e) = write_to_server(&mut write_stream, &msg) {
                            return Err(ClientError::ConnectionLost(e));
                        }
                        continue;
                    }
                    info!(
                        "clipboard image paste trigger received, but local clipboard has no image"
                    );
                }
                if let Some(image) = read_image_file_from_terminal_drop(&data, is_remote_client) {
                    info!(
                        bytes = image.bytes.len(),
                        extension = image.extension,
                        "bridging local image file drop to remote server"
                    );
                    let msg = ClientMessage::ClipboardImage {
                        extension: image.extension.to_owned(),
                        data: image.bytes,
                    };
                    if let Err(e) = write_to_server(&mut write_stream, &msg) {
                        return Err(ClientError::ConnectionLost(e));
                    }
                    continue;
                }
                let msg = ClientMessage::Input { data };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            #[cfg(windows)]
            ClientLoopEvent::HostCellSizeReported {
                width_px,
                height_px,
            } => {
                // Recorded, never forwarded — the same bargain the Unix arm
                // strikes above: the server learns the cell through the resize
                // path, so there is exactly one way for it to learn it.
                store_reported_cell_size(&reported_cell_size, width_px, height_px);
            }
            #[cfg(windows)]
            ClientLoopEvent::StdinEvents(events) => {
                if state.attach_escape.is_some() {
                    continue;
                }
                let raw_events = events
                    .iter()
                    .map(crate::protocol::ClientInputEvent::to_raw_input_event)
                    .collect::<Vec<_>>();
                if crate::raw_input::events_require_host_surface_redraw(
                    &raw_events,
                    state.redraw_on_focus_gained,
                ) {
                    state.request_repaint();
                }
                let msg = ClientMessage::InputEvents { events };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::Resize(new_cols, new_rows, cell_width_px, cell_height_px) => {
                state.reported_size = (new_cols, new_rows);
                state.cell_size = rasterisation_cell_size(cell_width_px, cell_height_px);
                // Resizing invalidates the host-side blit baseline.
                state.request_repaint();
                // A window dragged onto a display with a different scale keeps
                // its column count and changes its cell, so a resize is the one
                // moment worth asking again. Sent from here rather than from the
                // poll thread because this loop owns stdout; the query converges
                // because an unchanged answer reports no further resize.
                if will_query_host_cell_size {
                    query_host_cell_size();
                }
                let msg = ClientMessage::Resize {
                    cols: new_cols,
                    rows: new_rows,
                    cell_width_px,
                    cell_height_px,
                };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::ServerMessage(msg) => match msg {
                ServerMessage::Frame(frame_data) => {
                    let frame_data = if state.draw_host_cursor {
                        render_ansi::frame_with_drawn_cursor(frame_data)
                    } else {
                        frame_data
                    };
                    let encoded = if state.draw_host_cursor {
                        state.blit_encoder.encode_with_suppressed_visible_cursor(
                            &frame_data,
                            state.repaint_pending,
                        )
                    } else {
                        state
                            .blit_encoder
                            .encode(&frame_data, state.repaint_pending)
                    };
                    let mut stdout = io::stdout();
                    let graphics = if state.kitty_graphics_enabled {
                        frame_data.graphics.as_slice()
                    } else {
                        &[]
                    };
                    let _ =
                        write_encoded_frame_with_graphics(&mut stdout, &encoded.bytes, graphics);
                    let _ = stdout.flush();
                    state.blit_encoder.commit(frame_data, encoded);
                    state.repaint_pending = false;
                }
                ServerMessage::Terminal(frame) => {
                    if state.kitty_graphics_enabled && contains_kitty_graphics_bytes(&frame.bytes) {
                        record_received_kitty_graphics(&frame.bytes);
                    }
                    let mut stdout = io::stdout();
                    // A client that rasterises cards itself receives their
                    // layout as `ServerMessage::CardScene` separately from
                    // this frame's own bytes, so it splices its own Kitty
                    // bytes in here rather than finding them already embedded
                    // — the same sync-end splice point the server uses for
                    // `frame.graphics` on a client that did not ask for this.
                    let mut scene_graphics = std::mem::take(&mut state.pending_card_graphics);
                    scene_graphics.append(&mut state.pending_tray_graphics);
                    scene_graphics.append(&mut state.pending_background_scene_graphics);
                    let _ = write_encoded_frame_with_graphics(
                        &mut stdout,
                        &frame.bytes,
                        &scene_graphics,
                    );
                    let _ = stdout.flush();
                }
                ServerMessage::Graphics { bytes } => {
                    if state.kitty_graphics_enabled {
                        record_received_kitty_graphics(&bytes);
                        let mut stdout = io::stdout();
                        let _ = stdout.write_all(&bytes);
                        let _ = stdout.flush();
                    }
                }
                ServerMessage::CardScene { bytes } => {
                    if state.kitty_graphics_enabled {
                        let graphics = decode_and_rasterise_card_scene(&bytes, &mut state);
                        // Appended, not assigned. What comes back is a *delta*
                        // against this client's own graphics cache — deletes for
                        // the image ids it just superseded, uploads and
                        // placements for the ones replacing them — and the cache
                        // has already moved on by the time it lands here.
                        // Assigning dropped whichever delta no frame had carried
                        // yet, and since the very next scene is usually unchanged
                        // and rasterises to nothing, it dropped it in favour of
                        // an empty vector: the terminal kept the deletes it had
                        // been sent and never received the artwork that replaced
                        // them, so the cards went out and stayed out. Concatenating
                        // in arrival order is what the cache's own state assumes.
                        state.pending_card_graphics.extend(graphics);
                    }
                }
                ServerMessage::TrayScene { bytes } => {
                    if state.kitty_graphics_enabled {
                        let graphics = decode_and_rasterise_tray_scene(&bytes, &mut state);
                        state.pending_tray_graphics.extend(graphics);
                    }
                }
                ServerMessage::BackgroundScene { bytes } => {
                    if state.kitty_graphics_enabled {
                        let graphics = decode_and_rasterise_background_scene(&bytes, &mut state);
                        state.pending_background_scene_graphics.extend(graphics);
                    }
                }
                ServerMessage::ServerShutdown { reason } => {
                    return Err(ClientError::ServerShutdown { reason });
                }
                ServerMessage::Notify {
                    kind,
                    message,
                    body,
                } => {
                    handle_notify(kind, &message, body.as_deref(), &state.sound_config);
                }
                ServerMessage::Clipboard { data } => {
                    forward_clipboard(&data);
                    let _ = io::stdout().flush();
                }
                ServerMessage::RequestClipboardText { request_id } => {
                    // This process is on the machine the user copied on; the
                    // server may not be. Answer even when the clipboard is
                    // empty, so the server can retire the request instead of
                    // holding it open — see
                    // `ServerMessage::RequestClipboardText`.
                    let text = crate::platform::read_clipboard_text();
                    let msg = ClientMessage::ClipboardText { request_id, text };
                    if let Err(e) = write_to_server(&mut write_stream, &msg) {
                        return Err(ClientError::ConnectionLost(e));
                    }
                }
                ServerMessage::WindowTitle { title } => {
                    write_window_title(title.as_deref());
                    let _ = io::stdout().flush();
                }
                ServerMessage::ReloadSoundConfig => {
                    reload_local_client_config(
                        &mut state.sound_config,
                        &mut state.redraw_on_focus_gained,
                        &mut state.draw_host_cursor,
                        #[cfg(unix)]
                        &mut state.remote_image_paste_key,
                    );
                }
                ServerMessage::MouseCapture { enabled } => {
                    let desired = enabled;
                    if desired != state.mouse_capture_active {
                        set_mouse_capture(desired).map_err(ClientError::ConnectionFailed)?;
                        #[cfg(windows)]
                        if windows_vti_input_backend_enabled() {
                            let _ = enable_windows_virtual_terminal_input();
                        }
                        state.mouse_capture_active = desired;
                        host_mouse_capture_active.store(desired, Ordering::Release);
                    }
                }
                ServerMessage::KittyKeyboardReportAll { enabled } => {
                    if enabled != state.keyboard_report_all_active {
                        crate::terminal_modes::set_host_kitty_keyboard_report_all(
                            &mut io::stdout(),
                            enabled,
                        )
                        .map_err(ClientError::ConnectionFailed)?;
                        state.keyboard_report_all_active = enabled;
                    }
                }
                ServerMessage::PrefixInputSource { active } => {
                    if active {
                        prefix_input_source.switch_to_ascii();
                    } else {
                        prefix_input_source.restore();
                    }
                }
                ServerMessage::Welcome { .. } => {
                    debug!("received unexpected Welcome in main loop");
                }
            },
            ClientLoopEvent::ServerDisconnected => {
                return Err(ClientError::ConnectionLost(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed connection",
                )));
            }
            ClientLoopEvent::Timer => {}
        }
    }

    // Clean exit (Ctrl+C). Send Detach before closing.
    let detach = ClientMessage::Detach;
    let _ = write_to_server(&mut write_stream, &detach);
    let _ = io::stdout().flush();

    Ok(())
}

// ---------------------------------------------------------------------------
// Server reader thread
// ---------------------------------------------------------------------------

/// Blocking thread that reads ServerMessages from the server and sends them
/// to the main event loop.
fn server_reader_thread(
    mut stream: LocalStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
) {
    // Ensure the read stream is in blocking mode to avoid WouldBlock errors
    // from read_exact inside read_message. The stream should already be
    // blocking after handshake, but we enforce it here as a safety measure.
    if stream.set_nonblocking(false).is_err() {
        // If we can't set blocking mode, the stream is likely broken.
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
        return;
    }

    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, max_frame_size) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage(msg))
                    .is_err()
                {
                    break; // Main loop gone.
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                // Server closed connection.
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                // Should not happen with blocking mode, but handle gracefully
                // in case the stream was set nonblocking by another clone.
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Write helper
// ---------------------------------------------------------------------------

/// Writes a message to the server stream (blocking).
fn write_to_server(stream: &mut LocalStream, msg: &ClientMessage) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|e| io::Error::other(e.to_string()))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn client_remote_image_paste_key(
    config: &crate::config::Config,
) -> Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)> {
    if !is_remote_client_process() {
        return None;
    }

    match config.remote_image_paste_key() {
        Ok(key) => key,
        Err(diagnostic) => {
            warn!(diagnostic = %diagnostic, "local remote image paste key config diagnostic");
            None
        }
    }
}

fn reload_local_client_config(
    sound_config: &mut crate::config::SoundConfig,
    redraw_on_focus_gained: &mut bool,
    draw_host_cursor: &mut bool,
    #[cfg(unix)] remote_image_paste_key: &mut Option<(
        crossterm::event::KeyCode,
        crossterm::event::KeyModifiers,
    )>,
) {
    match crate::config::load_live_config() {
        Ok(loaded) => {
            for diagnostic in loaded.config.ui.sound.diagnostics() {
                warn!(diagnostic = %diagnostic, "local sound config diagnostic");
            }
            #[cfg(unix)]
            let loaded_remote_image_paste_key = client_remote_image_paste_key(&loaded.config);
            *sound_config = loaded.config.ui.sound;
            *redraw_on_focus_gained = loaded.config.ui.redraw_on_focus_gained;
            *draw_host_cursor = should_draw_host_cursor(loaded.config.ui.host_cursor);
            #[cfg(unix)]
            {
                *remote_image_paste_key = loaded_remote_image_paste_key;
            }
            debug!("reloaded local client config");
        }
        Err(diagnostics) => {
            warn!(diagnostics = ?diagnostics, "failed to reload local client config; keeping current client config");
        }
    }
}

fn handle_notify(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
) {
    handle_notify_with_notifiers(
        kind,
        message,
        body,
        sound_config,
        crate::terminal_notify::show_notification,
        crate::platform::show_desktop_notification,
    );
}

fn handle_notify_with_notifiers(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
    mut show_terminal_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
    mut show_system_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
) {
    match kind {
        NotifyKind::Sound => {
            let Some(sound) = sound_from_notify_message(message) else {
                warn!(
                    message = message,
                    "received unknown sound notification from server"
                );
                return;
            };
            if sound_config.enabled {
                crate::sound::play(sound, sound_config);
            }
        }
        NotifyKind::Toast => {
            debug!(
                message = message,
                "received terminal toast notification from server"
            );
            if let Err(err) = show_terminal_notification(message, body) {
                warn!(err = %err, "failed to emit terminal notification");
            }
        }
        NotifyKind::SystemToast => {
            debug!(
                message = message,
                "received system toast notification from server"
            );
            if let Err(err) = show_system_notification(message, body) {
                warn!(err = %err, "failed to emit system notification");
            }
        }
    }
}

fn sound_from_notify_message(message: &str) -> Option<crate::sound::Sound> {
    match message {
        "agent done" => Some(crate::sound::Sound::Done),
        "agent attention" => Some(crate::sound::Sound::Request),
        _ => None,
    }
}

#[cfg(unix)]
fn should_bridge_clipboard_image_paste(
    data: &[u8],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if data == b"\x1b[200~\x1b[201~" {
        return is_remote_client;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };

    let events = crate::raw_input::parse_raw_input_bytes_sync(data);
    matches!(
        events.as_slice(),
        [crate::raw_input::RawInputEvent::Key(key)]
            if key.kind == crossterm::event::KeyEventKind::Press
                && crate::config::terminal_key_matches_combo(key, remote_image_paste_key)
    )
}

#[cfg(unix)]
fn read_image_file_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let (path, extension) = image_path_from_terminal_drop(data, is_remote_client)?;
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let file = std::fs::File::open(&path).ok()?;
    let bytes =
        match crate::platform::read_limited_reader(file, MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()? {
            crate::platform::LimitedRead::Complete(bytes) => bytes,
            crate::platform::LimitedRead::Empty => return None,
            crate::platform::LimitedRead::Oversized => {
                warn!(
                    max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                    "local image file drop is too large to bridge"
                );
                return None;
            }
        };

    Some(crate::platform::ClipboardImage { bytes, extension })
}

#[cfg(unix)]
fn image_path_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<(std::path::PathBuf, &'static str)> {
    if !is_remote_client {
        return None;
    }

    let bytes = bracketed_paste_payload(data).unwrap_or(data);
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.is_empty() || text.contains(['\r', '\n']) {
        return None;
    }

    let text = unescape_terminal_drop_path(strip_matching_path_quotes(text));
    let path = std::path::PathBuf::from(text);
    if !path.is_absolute() {
        return None;
    }

    let extension = recognized_image_extension(path.extension()?.to_str()?)?;
    Some((path, extension))
}

#[cfg(unix)]
fn bracketed_paste_payload(data: &[u8]) -> Option<&[u8]> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    data.strip_prefix(START)?.strip_suffix(END)
}

#[cfg(unix)]
fn strip_matching_path_quotes(text: &str) -> &str {
    if text.len() < 2 {
        return text;
    }

    let bytes = text.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) => &text[1..text.len() - 1],
        _ => text,
    }
}

#[cfg(unix)]
fn unescape_terminal_drop_path(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                unescaped.push(escaped);
            } else {
                unescaped.push(ch);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
}

#[cfg(unix)]
fn recognized_image_extension(extension: &str) -> Option<&'static str> {
    if extension.eq_ignore_ascii_case("png") {
        Some("png")
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        Some("jpg")
    } else if extension.eq_ignore_ascii_case("gif") {
        Some("gif")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some("bmp")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Clipboard forwarding
// ---------------------------------------------------------------------------

/// Decode a clipboard payload forwarded by the server.
fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Forwards a clipboard write from the server to the local client clipboard.
fn forward_clipboard(data: &str) {
    let Some(bytes) = decode_clipboard_payload(data) else {
        warn!("received invalid clipboard payload from server");
        return;
    };

    crate::selection::write_osc52_bytes(&bytes);
}

fn window_title_osc(title: Option<&str>) -> Vec<u8> {
    let title = title.unwrap_or("herdr");
    let safe_title = title
        .chars()
        .filter(|ch| !matches!(*ch, '\u{1b}' | '\u{7}' | '\u{9c}'))
        .collect::<String>();
    format!("\x1b]0;{safe_title}\x07").into_bytes()
}

fn write_window_title(title: Option<&str>) {
    let _ = io::stdout().write_all(&window_title_osc(title));
}

// ---------------------------------------------------------------------------
// Frame output
// ---------------------------------------------------------------------------

fn write_encoded_frame_with_graphics(
    mut writer: impl io::Write,
    encoded: &[u8],
    graphics: &[u8],
) -> io::Result<()> {
    if graphics.is_empty() {
        return writer.write_all(encoded);
    }

    let insertion = render_ansi::final_sync_output_end(encoded).unwrap_or(encoded.len());

    writer.write_all(&encoded[..insertion])?;
    record_received_kitty_graphics(graphics);
    writer.write_all(b"\x1b7")?;
    writer.write_all(graphics)?;
    writer.write_all(b"\x1b8")?;
    writer.write_all(&encoded[insertion..])
}

fn contains_kitty_graphics_bytes(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| window == b"\x1b_G")
}

fn record_received_kitty_graphics(bytes: &[u8]) {
    let ids = kitty_graphics_image_ids(bytes);
    if ids.is_empty() {
        return;
    }
    let set = RECEIVED_KITTY_GRAPHICS_IDS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut set) = set.lock() {
        set.extend(ids);
    }
}

/// Decodes a `ServerMessage::CardScene` payload and rasterises it into Kitty
/// graphics bytes ready to splice into the next outgoing frame, updating
/// `state`'s own rasterisation cache along the way.
///
/// Empty on a decode/rasterise failure or when the scene is unchanged from
/// `state.previous_card_layers` — there is nothing new to splice either way.
fn decode_and_rasterise_card_scene(bytes: &[u8], state: &mut ClientState) -> Vec<u8> {
    let scene = match crate::ui::sidebar::image_card::decode_card_scene(bytes) {
        Ok(scene) => scene,
        Err(err) => {
            debug!(%err, "failed to decode CardScene from server");
            return Vec::new();
        }
    };
    let layers = match crate::ui::sidebar::image_card::rasterise_card_scene(
        &scene,
        state.card_font_override.as_deref(),
        state.cell_size,
        crate::kitty_graphics::host_terminal_kind(),
        crate::kitty_graphics::host_graphics_is_local(),
        &state.previous_card_layers,
    ) {
        Ok(Some(layers)) => layers,
        Ok(None) => return Vec::new(),
        Err(()) => {
            debug!("failed to rasterise CardScene");
            return Vec::new();
        }
    };
    let graphics = crate::kitty_graphics::encode_card_scene_graphics(
        &layers,
        state.cell_size,
        &mut state.card_scene_cache,
    );
    state.previous_card_layers = layers;
    graphics
}

/// Decodes a `ServerMessage::TrayScene` payload and rasterises it into Kitty
/// graphics bytes ready to splice into the next outgoing frame, updating
/// `state`'s own rasterisation cache along the way.
///
/// Empty on a decode failure, on a scene with nothing to draw, when the scene
/// is the one already on screen, or when the artwork it draws to is within
/// [`crate::app::state::SURFACE_DRIFT_LEVELS`] of the artwork this terminal is
/// already showing — there is nothing new to splice in any of those cases.
///
/// That last one is the whole idle cost of a delegating client. The scenes
/// arrive on the badge frame tier and every one of them is a *different* scene
/// — a resting badge's breath is a continuous envelope — so `previous_tray_scene`
/// alone stops none of them, and each one used to buy the terminal a fresh
/// 328x128 image for a change of a fraction of an 8-bit level.
fn decode_and_rasterise_tray_scene(bytes: &[u8], state: &mut ClientState) -> Vec<u8> {
    let scene = match crate::ui::decode_signal_tray_scene(bytes) {
        Ok(scene) => scene,
        Err(err) => {
            debug!(%err, "failed to decode TrayScene from server");
            return Vec::new();
        }
    };
    if state.previous_tray_scene.as_ref() == Some(&scene) {
        return Vec::new();
    }
    let Some((grid, image)) = crate::ui::rasterise_signal_tray_scene(
        &scene,
        state.cell_size.width_px,
        state.cell_size.height_px,
    ) else {
        return Vec::new();
    };
    // Recorded either way: this scene has been drawn, and drawing it again
    // would produce the same pixels the gate just refused.
    state.previous_tray_scene = Some(scene);
    if !state
        .published_tray_raster
        .accept(image.width, image.height, &image.pixels)
    {
        return Vec::new();
    }
    let Some(layer) = crate::ui::signal_tray_graphics_layer(
        image,
        crate::kitty_graphics::host_terminal_kind(),
        crate::kitty_graphics::host_graphics_is_local(),
    ) else {
        // Nothing went out, so nothing on screen matches what was just
        // recorded as published.
        state.published_tray_raster.forget();
        return Vec::new();
    };
    crate::kitty_graphics::encode_tray_scene_graphics(
        grid,
        &layer,
        state.cell_size,
        &mut state.tray_scene_cache,
    )
}

/// Decodes a `ServerMessage::BackgroundScene` payload and rasterises it into Kitty graphics bytes
/// ready to splice into the next outgoing frame, updating `state`'s own rasterisation cache along
/// the way.
///
/// The ambient loop layer is gated by `published_background_scene_raster` the same way the tray
/// gates its badges (see that field's doc): orbits drift continuously and imperceptibly between
/// scenes, and re-uploading every drift step would buy the terminal a fresh whole-screen image for
/// nothing a viewer could see. The effects overlay is never gated this way — an asteroid in flight
/// or a comet crossing the scene is real, fast motion the drift floor must not suppress.
fn decode_and_rasterise_background_scene(bytes: &[u8], state: &mut ClientState) -> Vec<u8> {
    let scene = match crate::app::background_scene::decode_background_scene(bytes) {
        Ok(scene) => scene,
        Err(err) => {
            debug!(%err, "failed to decode BackgroundScene from server");
            return Vec::new();
        }
    };
    let (width, height) = crate::app::background_scene::background_scene_size(&scene);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let (ambient_rgba, effects_rgba) =
        crate::app::background_scene::rasterise_background_scene(&scene);

    let kind = crate::kitty_graphics::host_terminal_kind();
    let is_local = crate::kitty_graphics::host_graphics_is_local();
    let grid = ratatui::layout::Rect::new(0, 0, state.reported_size.0, state.reported_size.1);

    let ambient_layer =
        if state
            .published_background_scene_raster
            .accept(width, height, &ambient_rgba)
        {
            let format =
                crate::kitty_graphics::preferred_sidebar_pixel_format(true, kind, is_local);
            let Some(data) =
                crate::kitty_graphics::encode_layer_pixels(format, width, height, &ambient_rgba)
            else {
                return Vec::new();
            };
            let layer = crate::app::state::GraphicsLayer::new(
                format,
                width,
                height,
                data,
                crate::api::schema::PaneGraphicsPlacementParams {
                    viewport_col: 0,
                    viewport_row: 0,
                    grid_cols: 0,
                    grid_rows: 0,
                    z: -2,
                },
            );
            state.previous_background_ambient_layer = Some(layer.clone());
            layer
        } else if let Some(layer) = state.previous_background_ambient_layer.clone() {
            layer
        } else {
            return Vec::new();
        };

    let effects_layer = effects_rgba.and_then(|rgba| {
        let format = crate::kitty_graphics::preferred_sidebar_pixel_format(false, kind, is_local);
        let data = crate::kitty_graphics::encode_layer_pixels(format, width, height, &rgba)?;
        Some(crate::app::state::GraphicsLayer::new(
            format,
            width,
            height,
            data,
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 0,
                viewport_row: 0,
                grid_cols: 0,
                grid_rows: 0,
                z: -1,
            },
        ))
    });

    crate::kitty_graphics::encode_background_scene_graphics(
        grid,
        &ambient_layer,
        effects_layer.as_ref(),
        state.cell_size,
        &mut state.background_scene_cache,
    )
}

fn clear_received_kitty_graphics(mut writer: impl io::Write) -> io::Result<()> {
    let Some(set) = RECEIVED_KITTY_GRAPHICS_IDS.get() else {
        return Ok(());
    };
    let Ok(mut set) = set.lock() else {
        return Ok(());
    };
    for id in set.drain() {
        write!(writer, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
    }
    writer.flush()
}

fn kitty_graphics_image_ids(bytes: &[u8]) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut index = 0usize;
    while let Some(start) = find_subslice(&bytes[index..], b"\x1b_G") {
        let command_start = index + start + 3;
        let Some(end) = find_subslice(&bytes[command_start..], b"\x1b\\") else {
            break;
        };
        let command = &bytes[command_start..command_start + end];
        if let Some(id) = kitty_graphics_command_image_id(command) {
            ids.push(id);
        }
        index = command_start + end + 2;
    }
    ids
}

fn kitty_graphics_command_image_id(command: &[u8]) -> Option<u32> {
    let header_end = command
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(command.len());
    for part in command[..header_end].split(|byte| *byte == b',') {
        let Some(value) = part.strip_prefix(b"i=") else {
            continue;
        };
        let text = std::str::from_utf8(value).ok()?;
        if let Ok(id) = text.parse::<u32>() {
            return Some(id);
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Resize polling
// ---------------------------------------------------------------------------

/// Cell size assumed when neither the terminal size ioctl nor the host
/// terminal reports pixel dimensions.
///
/// A guess, and the only one of the three sources in [`best_known_cell_size`]
/// that is. Everything the sidebar draws is rasterised at this many pixels per
/// cell and then placed with `c=`/`r=` cell counts, so a terminal whose cell is
/// not this **rescales** what it receives: the artwork lands in the right cells
/// and is soft, which is the one failure that looks like a font problem rather
/// than like a herdr bug.
const DEFAULT_CELL_WIDTH_PX: u32 = 8;
const DEFAULT_CELL_HEIGHT_PX: u32 = 16;

/// Cell size derived from the terminal size ioctl, if it reports pixels.
fn ioctl_cell_size() -> Option<(u32, u32)> {
    let size = crossterm::terminal::window_size().ok()?;
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return None;
    }
    Some((
        (size.width as u32 / size.columns as u32).max(1),
        (size.height as u32 / size.rows as u32).max(1),
    ))
}

/// The cell this client will rasterise and place graphics against, chosen from
/// the sources in the order they deserve to be believed.
///
/// Three things can claim to know the host terminal's cell, and they are not
/// equally good:
///
/// 1. **The terminal's own answer** to `CSI 16 t` (`reported`). Exact, because
///    it is the number the terminal lays its own glyphs out on rather than a
///    number derived from anything.
/// 2. **The terminal size ioctl** (`ioctl`). An estimate, and a *derived* one:
///    `ws_xpixel / columns` truncates, the window pixels it divides include
///    whatever padding the terminal draws around the grid, and on a pty that
///    did not measure the terminal at all the pixel fields are somebody else's
///    number entirely.
/// 3. **[`DEFAULT_CELL_WIDTH_PX`]/[`DEFAULT_CELL_HEIGHT_PX`]**, which knows
///    nothing at all.
///
/// They are consulted in that order, which is the order they are listed in:
/// ask the terminal, and fall back to arithmetic about the terminal only when
/// the terminal did not answer.
///
/// # The bug this ordering exists to fix
///
/// The estimate used to outrank the terminal's own answer whenever it was
/// merely *plausible*, and [`host_cell_size_query_required`] did not even send
/// the query in that case — so on a pty whose pixel fields are wrong the
/// terminal was never asked, and a wrong-but-believable cell won unopposed.
///
/// An SSH pty carrying a stale `ws_xpixel` is exactly that pty, and it is the
/// captain's route. Plausibility cannot catch it: `is_plausible` refuses a cell
/// no font could have, not a cell some *other* window had. A stale 1272x784 on
/// a 159x49 grid divides to 8x16 — a perfectly ordinary cell, and wrong, on a
/// terminal whose real cell is 10x21. Nothing downstream says so: the artwork
/// is drawn for the believed cell, placed by cell *count*, and the terminal
/// resamples it to fit. That is the whole of the "text is very blurry" report,
/// and it is why the earlier fix in this area — which only widened *which*
/// derived cells are refused — did not reach it.
///
/// See `a_stale_but_plausible_ioctl_loses_to_the_terminals_own_answer`.
fn best_known_cell_size(ioctl: Option<(u32, u32)>, reported: Option<(u32, u32)>) -> (u32, u32) {
    let plausible = |pair: Option<(u32, u32)>| {
        pair.filter(|(width_px, height_px)| {
            crate::kitty_graphics::HostCellSize {
                width_px: *width_px,
                height_px: *height_px,
            }
            .is_plausible()
        })
    };
    plausible(reported)
        .or(plausible(ioctl))
        .unwrap_or((DEFAULT_CELL_WIDTH_PX, DEFAULT_CELL_HEIGHT_PX))
}

fn pack_cell_size(width_px: u32, height_px: u32) -> u64 {
    (u64::from(width_px) << 32) | u64::from(height_px)
}

fn unpack_cell_size(packed: u64) -> Option<(u32, u32)> {
    let width_px = (packed >> 32) as u32;
    let height_px = (packed & u64::from(u32::MAX)) as u32;
    (width_px > 0 && height_px > 0).then_some((width_px, height_px))
}

fn current_terminal_geometry(
    kitty_graphics_enabled: bool,
    reported_cell_size: &AtomicU64,
) -> (u16, u16, u32, u32) {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if !kitty_graphics_enabled {
        return (cols, rows, 0, 0);
    }
    let (cell_width_px, cell_height_px) = best_known_cell_size(
        ioctl_cell_size(),
        unpack_cell_size(reported_cell_size.load(Ordering::Acquire)),
    );
    (cols, rows, cell_width_px, cell_height_px)
}

/// Reads the terminal geometry before the handshake, before any host cell
/// size report can exist.
fn initial_terminal_geometry(kitty_graphics_enabled: bool) -> (u16, u16, u32, u32) {
    current_terminal_geometry(kitty_graphics_enabled, &AtomicU64::new(0))
}

/// Reports polled changes and signalled resizes that return to the same size.
fn resize_report_required(
    signalled: bool,
    new_size: (u16, u16, u32, u32),
    last_size: (u16, u16, u32, u32),
) -> bool {
    signalled || new_size != last_size
}

/// Watches the terminal size and sends resize events when it changes.
///
/// The baseline cell size must match what the handshake sent to the server:
/// reading a fresh one here would race the host cell size reply and could
/// swallow the first change.
fn resize_poll_loop(
    resize_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    initial_cols: u16,
    initial_rows: u16,
    initial_cell_width: u32,
    initial_cell_height: u32,
    kitty_graphics_enabled: bool,
    reported_cell_size: &AtomicU64,
    should_quit: &Arc<AtomicBool>,
) {
    crate::platform::watch_terminal_resize_signal();
    let mut last_size = (
        initial_cols,
        initial_rows,
        initial_cell_width,
        initial_cell_height,
    );
    while !should_quit.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let signalled = crate::platform::take_terminal_resize_signal();
        let new_size = current_terminal_geometry(kitty_graphics_enabled, reported_cell_size);
        if resize_report_required(signalled, new_size, last_size) {
            last_size = new_size;
            if resize_tx
                .blocking_send(ClientLoopEvent::Resize(
                    new_size.0, new_size.1, new_size.2, new_size.3,
                ))
                .is_err()
            {
                break; // Main loop gone.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Initialize logging for the client process.
fn query_host_terminal_theme() {
    let _ = write_host_terminal_theme_query(io::stdout());
}

fn should_query_host_terminal_theme() -> bool {
    !cfg!(windows)
}

fn write_host_terminal_theme_query(mut writer: impl io::Write) -> io::Result<()> {
    let query = crate::terminal_theme::host_terminal_theme_query_sequence();
    writer.write_all(query.as_bytes())?;
    writer.flush()
}

fn query_kitty_graphics_capability() {
    let _ = write_kitty_graphics_capability_query(io::stdout());
}

fn write_kitty_graphics_capability_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(crate::terminal_theme::KITTY_GRAPHICS_CAPABILITY_QUERY_SEQUENCE.as_bytes())?;
    writer.flush()
}

fn query_host_terminal_version() {
    let _ = write_host_terminal_version_query(io::stdout());
}

/// Asked on the same round trip as the Kitty capability probe beside it, and
/// under the same conditions, because it answers the same class of question —
/// what may be drawn on this screen — and because the reply framing both rely
/// on is already armed at this moment by the color query.
///
/// Windows is excluded for the reason `should_query_host_terminal_theme` is:
/// the Windows client parses its own stdin into `ClientInputEvent`s, which
/// have no arm for a host reply, so an answer would be discarded before it
/// could reach the server. Asking a question whose answer cannot be delivered
/// is just noise on the wire.
fn should_query_host_terminal_version() -> bool {
    !cfg!(windows)
}

fn write_host_terminal_version_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(
        crate::host_terminal_identity::HOST_TERMINAL_VERSION_QUERY_SEQUENCE.as_bytes(),
    )?;
    writer.flush()
}

/// XTWINOPS request for the host terminal cell size in pixels.
const HOST_CELL_SIZE_QUERY: &[u8] = b"\x1b[16t";

fn query_host_cell_size() {
    let _ = write_host_cell_size_query(io::stdout());
}

/// Only pane graphics need pixel dimensions — but whenever they do, the
/// terminal is asked, however believable the ioctl looks.
///
/// This used to be gated on the ioctl being *implausible*, which quietly made
/// the gate decide the answer: a pty reporting a wrong-but-ordinary cell was
/// never contradicted, because the only thing that could contradict it was
/// never sent. A stale SSH `ws_xpixel` is precisely that pty, and it is the one
/// the "text is very blurry" report came from — see [`best_known_cell_size`]
/// for why plausibility cannot stand in for correctness here.
///
/// The cost of asking unconditionally is one escape sequence at startup and one
/// per resize, on a path that is already sending a Kitty capability query
/// beside it. A terminal that does not implement `CSI 16 t` simply does not
/// answer, `reported` stays `None`, and the ioctl is used exactly as before —
/// the reply-framing this depends on has always run on the implausible branch,
/// so it is the same machinery, not a new one.
fn host_cell_size_query_required(kitty_graphics_enabled: bool) -> bool {
    kitty_graphics_enabled
}

fn write_host_cell_size_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(HOST_CELL_SIZE_QUERY)?;
    writer.flush()
}

fn store_reported_cell_size(reported_cell_size: &AtomicU64, width_px: u32, height_px: u32) {
    let packed = pack_cell_size(width_px, height_px);
    if reported_cell_size.swap(packed, Ordering::AcqRel) != packed {
        debug!(width_px, height_px, "host terminal reported cell size");
    }
}

#[cfg(any(unix, test))]
fn reported_cell_size_from_events(
    events: &[crate::raw_input::RawInputEvent],
) -> Option<(u32, u32)> {
    events.iter().rev().find_map(|event| match event {
        crate::raw_input::RawInputEvent::HostCellSizeReport {
            width_px,
            height_px,
        } => Some((*width_px, *height_px)),
        _ => None,
    })
}

fn init_logging() {
    crate::logging::init_file_logging("herdr-client.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    /// The cell a delegating client draws with is the one the server laid the
    /// scene out against, not the raw report.
    ///
    /// The Windows remote bridge's `3x7` is the case that matters: the server
    /// replaces it with the fallback on the way in, so a client that kept it
    /// raw drew every card, and the tree's rails, on a grid less than half the
    /// width the layout counted.
    #[test]
    fn an_implausible_reported_cell_is_rasterised_against_the_fallback() {
        use crate::kitty_graphics::HostCellSize;
        assert_eq!(rasterisation_cell_size(3, 7), HostCellSize::FALLBACK);
        assert_eq!(
            rasterisation_cell_size(3, 7),
            HostCellSize {
                width_px: 3,
                height_px: 7
            }
            .plausible_or_unknown(),
            "the client has to use the server's own gate, not a second opinion"
        );
    }

    /// A believable cell is left exactly as reported.
    #[test]
    fn a_plausible_reported_cell_is_rasterised_as_reported() {
        use crate::kitty_graphics::HostCellSize;
        assert_eq!(
            rasterisation_cell_size(8, 15),
            HostCellSize {
                width_px: 8,
                height_px: 15
            }
        );
    }

    /// Graphics off reports `0x0`, and that absence must survive the gate: it
    /// means "send no graphics", not "a measurement I do not believe".
    #[test]
    fn a_client_with_graphics_off_stays_unknown() {
        assert!(!rasterisation_cell_size(0, 0).is_known());
    }

    /// Graphics off means the server is told nothing about pixels at all.
    #[test]
    fn geometry_reports_no_pixels_when_graphics_are_off() {
        assert_eq!(current_terminal_geometry(false, &AtomicU64::new(0)), {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            (cols, rows, 0, 0)
        });
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resize_signal_reports_even_when_polled_size_is_unchanged() {
        let size = (120, 40, 8, 16);
        assert!(resize_report_required(true, size, size));
        assert!(!resize_report_required(false, size, size));
        assert!(resize_report_required(false, (120, 41, 8, 16), size));
        assert!(resize_report_required(false, (120, 40, 9, 18), size));
    }

    fn restore_env_var(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            restore_env_var(self.key, self.previous.clone());
        }
    }

    #[test]
    fn windows_virtual_terminal_input_mode_sets_only_vti_bit() {
        assert_eq!(windows_virtual_terminal_input_mode(0x01f0), 0x03f0);
        assert_eq!(windows_virtual_terminal_input_mode(0x03f0), 0x03f0);
    }

    struct EnvVarsRemovedGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvVarsRemovedGuard {
        fn new(keys: &[&'static str]) -> Self {
            let previous: Vec<_> = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { previous }
        }
    }

    impl Drop for EnvVarsRemovedGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.clone() {
                restore_env_var(key, value);
            }
        }
    }

    #[test]
    fn host_cursor_policy_auto_uses_platform_default() {
        assert_eq!(
            should_draw_host_cursor(crate::config::HostCursorModeConfig::Auto),
            crate::platform::should_draw_host_cursor_by_default()
        );
    }

    #[test]
    fn host_cursor_policy_native_and_drawn_override_auto_detection() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarGuard::set("TERM_PROGRAM", "WezTerm");

        assert!(!should_draw_host_cursor(
            crate::config::HostCursorModeConfig::Native
        ));
        assert!(should_draw_host_cursor(
            crate::config::HostCursorModeConfig::Drawn
        ));
    }

    #[cfg(unix)]
    #[test]
    fn clipboard_image_paste_bridge_triggers_on_configured_key_and_empty_paste() {
        let ctrl_v = crate::config::parse_key_combo("ctrl+v").unwrap();
        assert!(should_bridge_clipboard_image_paste(
            &[0x16],
            true,
            Some(ctrl_v)
        ));
        assert!(should_bridge_clipboard_image_paste(
            b"\x1b[118;5u",
            true,
            Some(ctrl_v)
        ));
        assert!(should_bridge_clipboard_image_paste(
            b"\x1b[200~\x1b[201~",
            true,
            None
        ));
        assert!(!should_bridge_clipboard_image_paste(
            b"\x1b[200~\x1b[201~",
            false,
            Some(ctrl_v)
        ));
        assert!(!should_bridge_clipboard_image_paste(
            b"\x1b[200~text\x1b[201~",
            true,
            Some(ctrl_v)
        ));
        assert!(!should_bridge_clipboard_image_paste(&[0x16], true, None));
        assert!(!should_bridge_clipboard_image_paste(
            b"v",
            true,
            Some(ctrl_v)
        ));
    }

    #[cfg(unix)]
    struct TempImageFile {
        path: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl TempImageFile {
        fn new(extension: &str, bytes: &[u8]) -> Self {
            Self::with_name_fragment("test", extension, bytes)
        }

        fn with_name_fragment(name_fragment: &str, extension: &str, bytes: &[u8]) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "herdr-client-drop-{name_fragment}-{}-{nanos}.{extension}",
                std::process::id()
            ));
            std::fs::write(&path, bytes).unwrap();
            Self { path }
        }
    }

    #[cfg(unix)]
    impl Drop for TempImageFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_reads_bracketed_absolute_image_path() {
        let file = TempImageFile::new("PNG", b"image-bytes");
        let input = format!("\x1b[200~{}\x1b[201~", file.path.display());

        let image = read_image_file_from_terminal_drop(input.as_bytes(), true).unwrap();

        assert_eq!(image.extension, "png");
        assert_eq!(image.bytes, b"image-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_reads_plain_quoted_path_with_newline() {
        let file = TempImageFile::new("jpeg", b"jpeg-bytes");
        let input = format!("'{}'\n", file.path.display());

        let image = read_image_file_from_terminal_drop(input.as_bytes(), true).unwrap();

        assert_eq!(image.extension, "jpg");
        assert_eq!(image.bytes, b"jpeg-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_unescapes_spaces_in_paths() {
        let file = TempImageFile::with_name_fragment("space test", "png", b"image-bytes");
        let escaped_path = file.path.display().to_string().replace(' ', "\\ ");

        let image = read_image_file_from_terminal_drop(escaped_path.as_bytes(), true).unwrap();

        assert_eq!(image.extension, "png");
        assert_eq!(image.bytes, b"image-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_ignores_non_remote_and_non_image_input() {
        let file = TempImageFile::new("png", b"image-bytes");
        let path = file.path.display().to_string();

        assert!(read_image_file_from_terminal_drop(path.as_bytes(), false).is_none());
        assert!(read_image_file_from_terminal_drop(b"relative.png\n", true).is_none());
        assert!(read_image_file_from_terminal_drop(b"/tmp/file.txt\n", true).is_none());
        assert!(read_image_file_from_terminal_drop(
            format!("{}\nextra", file.path.display()).as_bytes(),
            true
        )
        .is_none());
    }

    #[test]
    fn graphics_bytes_are_written_inside_synchronized_blit_with_saved_cursor() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(
            &mut output,
            b"\x1b[?2026htext\x1b[?2026lcursor",
            b"graphics",
        )
        .unwrap();

        assert_eq!(
            output,
            b"\x1b[?2026htext\x1b7graphics\x1b8\x1b[?2026lcursor"
        );
    }

    #[test]
    fn empty_graphics_writes_only_blit_frame() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(&mut output, b"text", b"").unwrap();

        assert_eq!(output, b"text");
    }

    #[test]
    fn terminal_frame_kitty_detection_matches_apc_prefix() {
        assert!(contains_kitty_graphics_bytes(b"text\x1b_Ga=p;\x1b\\"));
        assert!(!contains_kitty_graphics_bytes(b"text\x1b[?2026h"));
    }

    #[test]
    fn kitty_graphics_image_id_parser_tracks_herdr_ids_only() {
        let ids = kitty_graphics_image_ids(
            b"text\x1b_Ga=t,t=d,f=32,s=1,v=1,i=10023,q=2;AAAA\x1b\\\x1b_Ga=p,i=10023,p=7;\x1b\\",
        );
        assert_eq!(ids, vec![10023, 10023]);
    }

    #[test]
    fn kitty_graphics_cleanup_deletes_tracked_images_not_all_images() {
        record_received_kitty_graphics(b"\x1b_Ga=t,i=123,q=2;AAAA\x1b\\");
        let mut output = Vec::new();
        clear_received_kitty_graphics(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("a=d,d=I,i=123"));
        assert!(!text.contains("d=A"));
    }

    #[test]
    fn write_host_terminal_theme_query_emits_osc_queries() {
        let mut output = Vec::new();
        write_host_terminal_theme_query(&mut output).unwrap();
        assert_eq!(
            output,
            crate::terminal_theme::host_terminal_theme_query_sequence().as_bytes()
        );
    }

    #[test]
    fn write_host_color_scheme_report_mode_emits_mode_sequences() {
        let mut output = Vec::new();
        write_host_color_scheme_report_mode(&mut output, true).unwrap();
        write_host_color_scheme_report_mode(&mut output, false).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE.as_bytes(),
        );
        expected.extend_from_slice(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        );
        assert_eq!(output, expected);
    }

    #[test]
    fn color_scheme_change_event_requests_host_theme_query() {
        let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[?997;1n");

        assert!(crate::raw_input::events_require_host_terminal_theme_query(
            &events
        ));
    }

    #[test]
    fn host_terminal_theme_query_is_disabled_on_windows() {
        assert_eq!(should_query_host_terminal_theme(), !cfg!(windows));
    }

    #[test]
    fn write_host_cell_size_query_emits_xtwinops_request() {
        let mut output = Vec::new();
        write_host_cell_size_query(&mut output).unwrap();

        assert_eq!(output, b"\x1b[16t");
    }

    #[test]
    fn write_host_terminal_version_query_emits_xtversion_request() {
        let mut output = Vec::new();
        write_host_terminal_version_query(&mut output).unwrap();

        assert_eq!(output, b"\x1b[>q");
    }

    #[test]
    fn host_terminal_version_query_is_disabled_on_windows() {
        assert_eq!(should_query_host_terminal_version(), !cfg!(windows));
    }

    /// The ioctl is used when it is all there is, and only then.
    #[test]
    fn best_known_cell_size_uses_a_plausible_ioctl_when_the_terminal_did_not_answer() {
        assert_eq!(best_known_cell_size(Some((11, 21)), None), (11, 21));
        // An unanswered query and an implausible answer are the same absence.
        assert_eq!(
            best_known_cell_size(Some((11, 21)), Some((16, 16))),
            (11, 21)
        );
    }

    /// The regression this ordering exists for: a pty whose pixel fields are
    /// stale reports a cell that is entirely believable and entirely wrong, and
    /// the terminal's own answer has to win anyway.
    ///
    /// Measured live on Rio, `--remote`, at the captain's 42-column sidebar: a
    /// pty carrying `1272x784` on a 159x49 grid divides to a clean `8x16` while
    /// the terminal's real cell is `10x20`. Believing the division rasterised
    /// the tree 328x96 and asked the terminal to draw it across 41x6 cells —
    /// 410x120 real pixels — so every glyph arrived through a 1.25x resample.
    /// `is_plausible` says yes to `8x16` and always will: it refuses a cell no
    /// font could have, not a cell another window had.
    #[test]
    fn a_stale_but_plausible_ioctl_loses_to_the_terminals_own_answer() {
        use crate::kitty_graphics::HostCellSize;
        let stale = (8, 16);
        assert!(
            HostCellSize {
                width_px: stale.0,
                height_px: stale.1,
            }
            .is_plausible(),
            "the premise: the stale reading is not one plausibility can refuse"
        );
        assert_eq!(best_known_cell_size(Some(stale), Some((10, 20))), (10, 20));
    }

    /// And the answer is only available because it is always asked for. Gating
    /// the query on the ioctl being implausible let the suspect reading decide
    /// whether anything was allowed to contradict it.
    #[test]
    fn the_terminal_is_asked_for_its_cell_whenever_graphics_are_on() {
        assert!(host_cell_size_query_required(true));
        assert!(!host_cell_size_query_required(false));
    }

    #[test]
    fn best_known_cell_size_prefers_the_terminals_own_answer_to_an_implausible_ioctl() {
        // The bug this ranking exists for. `3x7` is the arithmetic reading a
        // client behind ConPTY or a remote bridge gets; believing it — or
        // falling from it straight to the 8x16 guess — rasterises the sidebar
        // for a cell the terminal does not have, and the terminal rescales
        // what it is sent.
        assert_eq!(best_known_cell_size(Some((3, 7)), Some((11, 21))), (11, 21));
        assert_eq!(best_known_cell_size(None, Some((11, 21))), (11, 21));
    }

    #[test]
    fn best_known_cell_size_falls_back_only_when_nothing_believable_was_measured() {
        assert_eq!(best_known_cell_size(None, None), (8, 16));
        assert_eq!(best_known_cell_size(Some((3, 7)), None), (8, 16));
        // A square "cell" is a report, not a measurement.
        assert_eq!(best_known_cell_size(None, Some((16, 16))), (8, 16));
    }

    #[test]
    fn unpack_cell_size_rejects_a_half_reported_cell() {
        assert_eq!(unpack_cell_size(0), None);
        assert_eq!(unpack_cell_size(pack_cell_size(10, 21)), Some((10, 21)));
        assert_eq!(unpack_cell_size(pack_cell_size(10, 0)), None);
        assert_eq!(unpack_cell_size(pack_cell_size(0, 21)), None);
    }

    #[test]
    fn reported_cell_size_is_taken_from_host_cell_size_events() {
        let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[?997;1n");
        assert_eq!(reported_cell_size_from_events(&events), None);

        let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[6;21;10t\x1b[6;18;9t");
        assert_eq!(reported_cell_size_from_events(&events), Some((9, 18)));
    }

    #[test]
    fn color_scheme_reports_are_enabled_only_for_full_clients() {
        assert_eq!(
            should_enable_host_color_scheme_reports(true),
            !cfg!(windows)
        );
        assert!(!should_enable_host_color_scheme_reports(false));
    }

    #[test]
    fn terminal_restore_postlude_restores_visible_default_cursor() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output, false).unwrap();
        assert_eq!(output, b"\x1b[?25h\x1b[0 q");
    }

    #[test]
    fn terminal_restore_postlude_disables_color_scheme_reports_when_enabled() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output, true).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        );
        expected.extend_from_slice(b"\x1b[?25h\x1b[0 q");
        assert_eq!(output, expected);
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_detaches_on_prefix_q() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        assert!(matches!(
            escape.filter_input(vec![b'q'], 24, 3),
            AttachInputAction::Detach
        ));
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_sends_literal_prefix_on_double_prefix() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        match escape.filter_input(vec![0x02], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02]),
            other => panic!("expected forwarded prefix, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_forwards_prefix_before_non_escape_key() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![b'a', 0x02], 24, 3),
            AttachInputAction::Forward(bytes) if bytes == b"a"
        ));
        match escape.filter_input(vec![b'x'], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02, b'x']),
            other => panic!("expected forwarded bytes, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_turns_wheel_into_scroll_action() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[<64;11;6M".to_vec(), 24, 7) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                column,
                row,
                ..
            } => {
                assert_eq!(source, AttachScrollSource::Wheel);
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 7);
                assert_eq!(column, Some(10));
                assert_eq!(row, Some(5));
            }
            other => panic!("expected scroll action, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_swallows_non_wheel_mouse_reports() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(b"\x1b[<0;11;6M".to_vec(), 24, 7),
            AttachInputAction::None
        ));
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_turns_plain_page_keys_into_scroll_actions() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[5~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-up scroll action, got {other:?}"),
        }

        match escape.filter_input(b"\x1b[6~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[6~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Down);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-down scroll action, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_forwards_modified_page_key() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5;5~".to_vec(), 12, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, b"\x1b[5;5~"),
            other => panic!("expected modified page key to forward, got {other:?}"),
        }
    }

    #[test]
    fn client_error_display_connection_failed() {
        let err = ClientError::ConnectionFailed(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("failed to connect to server"),
            "should mention connection failure: {msg}"
        );
        assert!(
            msg.contains("herdr server"),
            "should suggest starting server: {msg}"
        );
    }

    #[test]
    fn client_error_display_handshake_rejected() {
        let err = ClientError::HandshakeRejected {
            version: 1,
            error: "incompatible".into(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("rejected handshake"),
            "should mention rejection: {msg}"
        );
        assert!(msg.contains("incompatible"), "should include error: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown() {
        let err = ClientError::ServerShutdown {
            reason: Some("maintenance".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
        assert!(msg.contains("maintenance"), "should include reason: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown_no_reason() {
        let err = ClientError::ServerShutdown { reason: None };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
    }

    /// The dev-box overrides are the only way to exercise the client-rasterised
    /// surfaces off Windows, and on their own they used to negotiate
    /// `SemanticFrame` — whose arm of the event loop never touches
    /// `pending_tray_graphics` — so the surface they turned on rendered as a
    /// hole. Asking for the encoding the real bridge sets for itself is what
    /// makes them exercise the real path instead of a broken one.
    #[test]
    fn a_client_rasterised_surface_asks_for_an_encoding_that_can_carry_it() {
        let _guard = env_lock().lock().unwrap();
        let _encoding = EnvVarsRemovedGuard::new(&[
            "HERDR_RENDER_ENCODING",
            "HERDR_CLIENT_RASTERIZED_CARDS",
            "HERDR_CLIENT_RASTERIZED_SIGNAL_TRAY",
            crate::remote::REMOTE_KEYBINDINGS_ENV_VAR,
        ]);
        assert_eq!(requested_render_encoding(), RenderEncoding::SemanticFrame);

        for override_var in [
            "HERDR_CLIENT_RASTERIZED_SIGNAL_TRAY",
            "HERDR_CLIENT_RASTERIZED_CARDS",
        ] {
            let _delegated = EnvVarGuard::set(override_var, "1");
            assert_eq!(
                requested_render_encoding(),
                RenderEncoding::TerminalAnsi,
                "{override_var} left the client on an encoding that drops what it rasterises"
            );

            // An explicit request still wins — this implication fills a gap, it
            // does not take the choice away.
            let _explicit = EnvVarGuard::set("HERDR_RENDER_ENCODING", "semantic");
            assert_eq!(requested_render_encoding(), RenderEncoding::SemanticFrame);
            restore_env_var("HERDR_RENDER_ENCODING", None);
        }
    }

    #[test]
    fn client_error_display_detached_default_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarsRemovedGuard::new(&[
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            crate::session::SESSION_ENV_VAR,
        ]);
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr` to reattach"),
            "should suggest default reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_named_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr session attach work` to reattach"),
            "should suggest named session reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_remote_reattach_hint_takes_precedence() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            "herdr --remote host --session work",
        );
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr --remote host --session work` to reattach"),
            "should prefer remote reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_connection_lost() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
        let err =
            ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let msg = err.to_string();
        assert!(
            msg.contains("lost connection to server"),
            "should mention lost connection: {msg}"
        );
    }

    #[test]
    fn client_error_display_remote_connection_lost_has_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            "herdr --remote host --session work",
        );
        let err =
            ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let msg = err.to_string();
        assert!(
            msg.contains("lost connection to remote Herdr"),
            "should mention remote connection loss: {msg}"
        );
        assert!(
            msg.contains("panes may still be running"),
            "should explain possible persistence: {msg}"
        );
        assert!(
            msg.contains("Run `herdr --remote host --session work` to reattach"),
            "should show remote reattach command: {msg}"
        );
    }

    #[test]
    fn sound_from_notify_message_maps_done() {
        assert_eq!(
            sound_from_notify_message("agent done"),
            Some(crate::sound::Sound::Done)
        );
    }

    #[test]
    fn sound_from_notify_message_maps_attention() {
        assert_eq!(
            sound_from_notify_message("agent attention"),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn sound_from_notify_message_rejects_unknown_payloads() {
        assert_eq!(sound_from_notify_message("toast"), None);
    }

    #[test]
    fn reload_local_client_config_refreshes_local_client_presentation_state() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-client-config-reload-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "[ui]\nredraw_on_focus_gained = false\nhost_cursor = \"drawn\"\n",
        )
        .unwrap();
        let path_string = path.to_string_lossy().to_string();
        let _env = EnvVarGuard::set(crate::config::CONFIG_PATH_ENV_VAR, &path_string);
        let mut sound_config = crate::config::SoundConfig::default();
        let mut redraw_on_focus_gained = true;
        let mut draw_host_cursor = false;
        #[cfg(unix)]
        let mut remote_image_paste_key = None;

        reload_local_client_config(
            &mut sound_config,
            &mut redraw_on_focus_gained,
            &mut draw_host_cursor,
            #[cfg(unix)]
            &mut remote_image_paste_key,
        );

        assert!(!redraw_on_focus_gained);
        assert!(draw_host_cursor);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn toast_notify_from_server_is_emitted_even_when_attach_config_was_off() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::Toast,
            "pi finished",
            Some("workspace 1"),
            &sound_config,
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
            |_, _| Ok(false),
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn system_toast_notify_from_server_uses_system_notifier() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::SystemToast,
            "pi finished",
            Some("workspace 1"),
            &sound_config,
            |_, _| Ok(false),
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn system_toast_notify_preserves_colon_in_title() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::SystemToast,
            "build: failed",
            Some("api workspace"),
            &sound_config,
            |_, _| Ok(false),
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
        );

        assert_eq!(
            emitted,
            Some((
                "build: failed".to_string(),
                Some("api workspace".to_string())
            ))
        );
    }

    #[test]
    fn decode_clipboard_payload_decodes_base64() {
        assert_eq!(decode_clipboard_payload("dGVzdA=="), Some(b"test".to_vec()));
    }

    #[test]
    fn decode_clipboard_payload_rejects_invalid_base64() {
        assert_eq!(decode_clipboard_payload("not-base64!!!"), None);
    }

    #[test]
    fn terminal_control_input_command_accepts_text() {
        let action =
            terminal_control_command_from_json(r#"{"type":"terminal.input","text":"hello"}"#)
                .unwrap();
        let ClientMessage::Input { data } = action else {
            panic!("expected input command");
        };
        assert_eq!(data, b"hello");
    }

    #[test]
    fn terminal_control_input_command_accepts_base64_bytes() {
        let action =
            terminal_control_command_from_json(r#"{"type":"terminal.input","bytes":"G1tB"}"#)
                .unwrap();
        let ClientMessage::Input { data } = action else {
            panic!("expected input command");
        };
        assert_eq!(data, b"\x1b[A");
    }

    #[test]
    fn terminal_control_resize_command_maps_to_client_resize() {
        let action = terminal_control_command_from_json(
            r#"{"type":"terminal.resize","cols":100,"rows":30,"cell_width_px":8,"cell_height_px":16}"#,
        )
        .unwrap();
        let ClientMessage::Resize {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        } = action
        else {
            panic!("expected resize command");
        };
        assert_eq!(
            (cols, rows, cell_width_px, cell_height_px),
            (100, 30, 8, 16)
        );
    }

    #[test]
    fn terminal_control_scroll_command_maps_to_attach_scroll() {
        let action = terminal_control_command_from_json(
            r#"{"type":"terminal.scroll","direction":"up","lines":3}"#,
        )
        .unwrap();
        let ClientMessage::AttachScroll {
            source,
            direction,
            lines,
            ..
        } = action
        else {
            panic!("expected scroll command");
        };
        assert_eq!(source, AttachScrollSource::Wheel);
        assert_eq!(direction, AttachScrollDirection::Up);
        assert_eq!(lines, 3);
    }

    #[test]
    fn forward_clipboard_uses_local_clipboard_path() {
        unsafe {
            std::env::set_var("SSH_CONNECTION", "1 2 3 4");
        }
        forward_clipboard("dGVzdA==");
        unsafe {
            std::env::remove_var("SSH_CONNECTION");
        }
    }

    #[test]
    fn window_title_osc_strips_terminators_and_defaults_to_herdr() {
        assert_eq!(
            window_title_osc(Some("herdr\x1b api\u{7}\u{9c}")),
            b"\x1b]0;herdr api\x07"
        );
        assert_eq!(window_title_osc(None), b"\x1b]0;herdr\x07");
    }
}
