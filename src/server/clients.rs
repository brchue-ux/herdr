use std::collections::HashMap;
use std::path::PathBuf;

use crate::protocol::RenderEncoding;
use crate::server::client_transport::ClientWriter;
use crate::server::render_stream::{ClientRenderState, VirtualRenderer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientConnectionMode {
    App,
    TerminalAttach { terminal_id: String },
    TerminalObserve { terminal_id: String },
}

pub(crate) type RenderTarget = (
    u64,
    (u16, u16),
    crate::kitty_graphics::HostCellSize,
    bool,
    ClientConnectionMode,
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DeferredRender {
    #[default]
    None,
    Graphics,
    Full,
}

/// A connected client tracked by the server.
pub(crate) struct ClientConnection {
    /// Whether this connection is the full app client or a direct terminal attach.
    pub(crate) mode: ClientConnectionMode,
    /// True after the handshake for clients that will switch into direct terminal attach mode.
    pub(crate) pending_terminal_attach: bool,
    /// Client-local app keybindings. None means use the server's keybindings.
    pub(crate) keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
    /// The client's terminal size after clamping.
    pub(crate) terminal_size: (u16, u16),
    /// Pixel size of one client terminal cell, after the plausibility gate.
    ///
    /// Private on purpose: this is the one field on the connection that arrives
    /// as arithmetic rather than a measurement, so every way in goes through
    /// [`ClientConnection::set_cell_size`] and nothing downstream has to
    /// re-check it.
    cell_size: crate::kitty_graphics::HostCellSize,
    /// Last known host terminal default colors for this client.
    pub(crate) host_terminal_theme: crate::terminal_theme::TerminalTheme,
    /// Last known host terminal appearance for this client.
    pub(crate) host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    /// True when appearance came from an explicit host color-scheme report.
    pub(crate) host_terminal_appearance_explicit: bool,
    /// Last reported focus state for this client's outer terminal.
    pub(crate) outer_terminal_focus: Option<bool>,
    /// This client's own host terminal, classified from whichever source has
    /// spoken: the terminal's own XTVERSION answer once it arrives, and until
    /// then the attach-time environment probe
    /// (`crate::protocol::HostTerminalReport`). See
    /// [`ClientConnection::update_host_terminal_identity_from_events`].
    pub(crate) host_terminal_kind: crate::kitty_graphics::HostTerminalKind,
    /// What this client's terminal called itself, if it answered XTVERSION.
    /// `None` for a terminal that does not implement the query — which is the
    /// case the environment probe still has to cover.
    pub(crate) host_terminal_identity: Option<crate::host_terminal_identity::HostTerminalIdentity>,
    /// Whether this client positively established that it shares a
    /// filesystem with its terminal.
    pub(crate) host_graphics_is_local: bool,
    /// Whether this client's real outer terminal confirmed Kitty Graphics
    /// Protocol support. Monotonic: once true, stays true for the connection.
    pub(crate) kitty_graphics_capability_confirmed: bool,
    /// Whether this client requested `ServerMessage::CardScene` instead of
    /// server-embedded sidebar card pixels. Set once from the client's Hello
    /// and never changes for the lifetime of the connection.
    pub(crate) wants_client_rasterized_cards: bool,
    /// Whether this client requested `ServerMessage::TrayScene` instead of
    /// server-embedded signal-tray badge pixels. Set once from the client's
    /// Hello and never changes for the lifetime of the connection.
    pub(crate) wants_client_rasterized_signal_tray: bool,
    /// Stateful parser for app-client input split across transport reads.
    pub(crate) raw_input: crate::raw_input::RawInputFramer,
    /// Monotonic activity stamp used to choose the fallback foreground client.
    pub(crate) last_activity: u64,
    /// Render baseline for the negotiated client encoding.
    pub(crate) render_state: ClientRenderState,
    /// This client's own ratatui terminal, kept across frames.
    ///
    /// Per client rather than per server: two clients at different sizes
    /// sharing one would resize it every frame, and a resize is a full clear.
    pub(crate) renderer: VirtualRenderer,
    /// Client-local host Kitty graphics cache.
    pub(crate) graphics_cache: crate::kitty_graphics::HostGraphicsCache,
    /// Whether this client's own last rendered frame published sidebar card layers.
    ///
    /// Set from `ViewState::sidebar_card_layers_published` at the moment this
    /// client's own full-render pass produced it, next to `graphics_cache`
    /// because both describe what that pass actually did. The retained
    /// graphics path does not recompute the view per client — it reuses
    /// whatever pass wrote `AppState::view` last, which `render_targets`
    /// sorts to the foreground — so it reads this per-client copy to tell
    /// whether that shared view still matches what *this* client's own last
    /// pass published, rather than assuming it does.
    pub(crate) sidebar_card_layers_published: bool,
    /// Whether the next graphics frame must clear and rebuild host-side Kitty state.
    pub(crate) graphics_surface_reset_pending: bool,
    /// Whether an ordinary render was skipped because the render channel was full.
    pub(crate) render_pending: bool,
    /// Whether a pane-graphics-only render was skipped because the channel was full.
    pane_graphics_render_pending: bool,
    /// Last host mouse capture mode sent to this client.
    pub(crate) host_mouse_capture_active: Option<bool>,
    /// Last Kitty report-all mode sent to this client's host terminal.
    pub(crate) host_keyboard_report_all_active: Option<bool>,
    /// Temporary files staged from this client's local clipboard image pastes.
    pub(crate) staged_clipboard_files: Vec<PathBuf>,
    /// Channels for sending framed ServerMessage data to the client writer thread.
    pub(crate) writer: Option<ClientWriter>,
}

impl ClientConnection {
    #[cfg(test)]
    pub(crate) fn new(
        terminal_size: (u16, u16),
        cell_size: crate::kitty_graphics::HostCellSize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        outer_terminal_focus: Option<bool>,
        last_activity: u64,
        render_encoding: RenderEncoding,
        writer: Option<ClientWriter>,
    ) -> Self {
        Self::new_with_mode(
            ClientConnectionMode::App,
            None,
            terminal_size,
            cell_size,
            host_terminal_theme,
            outer_terminal_focus,
            last_activity,
            render_encoding,
            false,
            writer,
        )
    }

    pub(crate) fn new_with_mode(
        mode: ClientConnectionMode,
        keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
        terminal_size: (u16, u16),
        cell_size: crate::kitty_graphics::HostCellSize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        outer_terminal_focus: Option<bool>,
        last_activity: u64,
        render_encoding: RenderEncoding,
        pending_terminal_attach: bool,
        writer: Option<ClientWriter>,
    ) -> Self {
        Self {
            mode,
            pending_terminal_attach,
            keybindings,
            terminal_size,
            cell_size: cell_size.plausible_or_unknown(),
            host_terminal_appearance: host_terminal_theme
                .background
                .map(crate::terminal_theme::RgbColor::inferred_appearance),
            host_terminal_appearance_explicit: false,
            host_terminal_theme,
            outer_terminal_focus,
            host_terminal_kind: crate::kitty_graphics::HostTerminalKind::default(),
            host_terminal_identity: None,
            host_graphics_is_local: false,
            kitty_graphics_capability_confirmed: false,
            wants_client_rasterized_cards: false,
            wants_client_rasterized_signal_tray: false,
            raw_input: crate::raw_input::RawInputFramer::default(),
            last_activity,
            render_state: ClientRenderState::new(render_encoding),
            renderer: VirtualRenderer::default(),
            graphics_cache: crate::kitty_graphics::HostGraphicsCache::default(),
            sidebar_card_layers_published: false,
            graphics_surface_reset_pending: false,
            render_pending: false,
            pane_graphics_render_pending: false,
            host_mouse_capture_active: None,
            host_keyboard_report_all_active: None,
            staged_clipboard_files: Vec::new(),
            writer,
        }
    }

    pub(crate) fn request_repaint(&mut self) {
        self.render_state.request_repaint();
        self.pane_graphics_render_pending = false;
    }

    /// Pixel size of one of this client's terminal cells.
    ///
    /// Either unknown or believable; never an arithmetic artefact of a stale
    /// pty pixel width.
    pub(crate) fn cell_size(&self) -> crate::kitty_graphics::HostCellSize {
        self.cell_size
    }

    /// Records the cell size a client reported, gated on being a cell a
    /// terminal could actually be drawing in.
    ///
    /// A wrong-but-nonzero cell is worse than no cell: `is_known` says yes to
    /// it and every pixel surface downstream then rasterises into a space that
    /// does not exist, which the terminal rescales away where no test can see
    /// it. The gate lives here rather than at the readers so a new reader
    /// cannot forget it.
    pub(crate) fn set_cell_size(&mut self, cell_size: crate::kitty_graphics::HostCellSize) {
        self.cell_size = cell_size.plausible_or_unknown();
    }

    /// Classifies and records this client's attach-time host-capability
    /// probe (`crate::protocol::HostTerminalReport`).
    ///
    /// Classification lives in `crate::kitty_graphics::host_terminal_kind_for_env`
    /// / `host_graphics_locality_for_env` — the same pure rule the monolithic
    /// (`--no-session`) path applies to its own process environment — so a
    /// client's reported facts and a live env read are judged identically.
    ///
    /// This is the *fallback* source for the terminal kind. It runs at Hello,
    /// before the terminal has had time to answer XTVERSION, so it is what
    /// herdr goes on until the reply arrives — and all there ever is for a
    /// terminal that does not implement the query. Once the terminal has named
    /// itself, its own answer stands and this cannot demote it back to an
    /// environment guess.
    pub(crate) fn set_host_terminal(&mut self, report: &crate::protocol::HostTerminalReport) {
        let env_kind = crate::kitty_graphics::host_terminal_kind_for_env(
            report.term_program.as_deref(),
            report.term.as_deref(),
            report.kitty_window_id_set,
        );
        if self.host_terminal_identity.is_none() {
            self.host_terminal_kind = env_kind;
        }
        self.host_graphics_is_local = report.is_local;
        tracing::info!(
            term_program = ?report.term_program,
            term = ?report.term,
            kitty_window_id_set = report.kitty_window_id_set,
            is_local = report.is_local,
            classified_kind = ?self.host_terminal_kind,
            "client host-capability probe classified"
        );
    }

    /// Adopts an XTVERSION answer from this client's input stream, replacing
    /// whatever the environment guessed. Returns whether the terminal kind
    /// changed as a result.
    ///
    /// The answer came down the pty rather than out of an environment
    /// variable, which is the whole point: an SSH-attached client's
    /// environment describes the machine herdr runs on, not the terminal it
    /// draws on, and that is why every remote client used to classify `Other`
    /// however real its terminal was. See `crate::host_terminal_identity`.
    pub(crate) fn update_host_terminal_identity_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let Some(identity) = events.iter().rev().find_map(|event| match event {
            crate::raw_input::RawInputEvent::HostTerminalIdentity(identity) => Some(identity),
            _ => None,
        }) else {
            return false;
        };
        if self.host_terminal_identity.as_ref() == Some(identity) {
            return false;
        }
        let kind = crate::kitty_graphics::host_terminal_kind_for_identity(identity.name());
        let previous_kind = self.host_terminal_kind;
        tracing::info!(
            name = identity.name(),
            version = identity.version().unwrap_or("unreported"),
            classified_kind = ?kind,
            ?previous_kind,
            "client host terminal identified in band"
        );
        self.host_terminal_identity = Some(identity.clone());
        self.host_terminal_kind = kind;
        previous_kind != kind
    }

    pub(crate) fn deferred_render(&self) -> DeferredRender {
        if self.render_pending {
            DeferredRender::Full
        } else if self.pane_graphics_render_pending {
            DeferredRender::Graphics
        } else {
            DeferredRender::None
        }
    }

    pub(crate) fn clear_deferred_render(&mut self) {
        self.render_pending = false;
        self.pane_graphics_render_pending = false;
    }

    pub(crate) fn defer_full_render(&mut self) {
        self.render_pending = true;
        self.pane_graphics_render_pending = false;
    }

    pub(crate) fn defer_pane_graphics_render(&mut self) {
        if !self.render_pending {
            self.pane_graphics_render_pending = true;
        }
    }

    pub(crate) fn take_deferred_render(&mut self) -> DeferredRender {
        let deferred = self.deferred_render();
        self.clear_deferred_render();
        deferred
    }

    pub(crate) fn is_full_app_client(&self) -> bool {
        matches!(self.mode, ClientConnectionMode::App) && !self.pending_terminal_attach
    }

    /// Which of Herdr's own drawn surfaces this client is sent as pixels.
    ///
    /// The inverse of what it asked to rasterise itself: a surface it draws
    /// from a scene message is one this client's frames must not also carry.
    pub(crate) fn embedded_surfaces(&self) -> crate::kitty_graphics::EmbeddedSurfaces {
        crate::kitty_graphics::EmbeddedSurfaces {
            cards: !self.wants_client_rasterized_cards,
            signal_tray: !self.wants_client_rasterized_signal_tray,
        }
    }

    pub(crate) fn request_semantic_redraw_after_input(&mut self) {
        self.render_state.reset_semantic_input_baseline();
    }

    pub(crate) fn update_host_theme_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let mut next_theme = self.host_terminal_theme;
        let mut changed = false;
        for event in events {
            match event {
                crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                    next_theme = next_theme.with_color(*kind, *color);
                    if matches!(kind, crate::terminal_theme::DefaultColorKind::Background)
                        && !self.host_terminal_appearance_explicit
                    {
                        changed |=
                            self.set_host_appearance(Some(color.inferred_appearance()), false);
                    }
                }
                crate::raw_input::RawInputEvent::HostPaletteColors { colors } => {
                    for &(index, color) in colors {
                        next_theme = next_theme.with_palette_color(index, color);
                    }
                }
                crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                    changed |= self.set_host_appearance(Some(*appearance), true);
                }
                _ => {}
            }
        }

        if next_theme != self.host_terminal_theme {
            self.host_terminal_theme = next_theme;
            changed = true;
        }
        changed
    }

    fn set_host_appearance(
        &mut self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
        explicit: bool,
    ) -> bool {
        if self.host_terminal_appearance_explicit && !explicit {
            return false;
        }
        if self.host_terminal_appearance == appearance
            && self.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        self.host_terminal_appearance = appearance;
        self.host_terminal_appearance_explicit = explicit;
        true
    }

    /// Returns `true` if this client's capability flag flipped to confirmed.
    pub(crate) fn update_kitty_graphics_capability_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        if self.kitty_graphics_capability_confirmed {
            return false;
        }
        let confirmed = events.iter().any(|event| {
            matches!(
                event,
                crate::raw_input::RawInputEvent::KittyGraphicsCapability(true)
            )
        });
        if confirmed {
            self.kitty_graphics_capability_confirmed = true;
        }
        confirmed
    }

    pub(crate) fn update_outer_focus_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> Option<bool> {
        let next_focus = events
            .iter()
            .filter_map(|event| match event {
                crate::raw_input::RawInputEvent::OuterFocusGained => Some(true),
                crate::raw_input::RawInputEvent::OuterFocusLost => Some(false),
                _ => None,
            })
            .next_back()?;

        self.outer_terminal_focus = Some(next_focus);
        Some(next_focus)
    }
}

pub(crate) fn events_include_interaction(events: &[crate::raw_input::RawInputEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            crate::raw_input::RawInputEvent::Key(_)
                | crate::raw_input::RawInputEvent::Text(_)
                | crate::raw_input::RawInputEvent::Mouse(_)
                | crate::raw_input::RawInputEvent::Paste(_)
                | crate::raw_input::RawInputEvent::OuterFocusGained
        )
    })
}

pub(crate) fn latest_app_client(clients: &HashMap<u64, ClientConnection>) -> Option<u64> {
    clients
        .iter()
        .filter(|(_, client)| client.is_full_app_client())
        .max_by_key(|(_, client)| client.last_activity)
        .map(|(&client_id, _)| client_id)
}

pub(crate) fn terminal_stream_client_ids(
    clients: &HashMap<u64, ClientConnection>,
    terminal_id: &str,
) -> Vec<u64> {
    clients
        .iter()
        .filter_map(|(&client_id, client)| match &client.mode {
            ClientConnectionMode::TerminalAttach {
                terminal_id: attached,
            }
            | ClientConnectionMode::TerminalObserve {
                terminal_id: attached,
            } if attached == terminal_id => Some(client_id),
            _ => None,
        })
        .collect()
}

pub(crate) fn render_targets(
    clients: &HashMap<u64, ClientConnection>,
    foreground_client_id: Option<u64>,
) -> Vec<RenderTarget> {
    let mut targets: Vec<RenderTarget> = clients
        .iter()
        .filter(|(_, client)| {
            client.writer.is_some()
                && (client.is_full_app_client()
                    || matches!(
                        client.mode,
                        ClientConnectionMode::TerminalAttach { .. }
                            | ClientConnectionMode::TerminalObserve { .. }
                    ))
        })
        .map(|(&client_id, client)| {
            (
                client_id,
                client.terminal_size,
                client.cell_size(),
                foreground_client_id == Some(client_id),
                client.mode.clone(),
            )
        })
        .collect();

    targets.sort_by_key(|(client_id, _, _, is_foreground, _)| (*is_foreground, *client_id));
    targets
}
