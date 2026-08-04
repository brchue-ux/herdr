//! Opening, answering and closing the notification tray's popover.
//!
//! The tray's badges are hit-tested straight out of the sidebar's own layout
//! ([`crate::ui::sidebar::tray`]), so nothing here caches a rect. What it owns
//! is the *authority*: which of the popover's buttons is allowed to run
//! anything, and what running it actually does.
//!
//! ## The one rule
//!
//! Every acting path in this file goes through
//! [`crate::app::signal_tray::TrayBadge::command`], which returns `None` for
//! every badge whose action is a jump and for every badge whose refusal has
//! fired. There is deliberately no second route: a click cannot reach a
//! `git pull --rebase` that the badge refused, because the refusal is checked
//! by the same function that produces the command rather than beside it.
//!
//! Git runs on a background thread and reports back through
//! [`crate::events::AppEvent::SignalTrayCommandFinished`], so a slow remote
//! cannot pin the app loop, and the popover shows what actually happened rather
//! than assuming it worked.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::app::fleet_signals::FleetSignal;
use crate::app::signal_tray::{self, SignalTrayPopup, TrayCommand, TrayOutcome, TrayTarget};
use crate::app::state::{AppState, Mode};
use crate::app::App;
use crate::events::AppEvent;
use crate::ui::signal_tray_popup::Button;

use super::modal::leave_modal;

impl AppState {
    /// Open a badge's popover, pointed at its first item.
    pub(crate) fn open_signal_tray_popup(&mut self, signal: FleetSignal) {
        // Opening *is* the acknowledgement: the escalation exists to say "this
        // happened while you were looking away", and you are no longer looking
        // away.
        self.signal_tray.acknowledge(signal);
        self.signal_tray.popup = Some(SignalTrayPopup {
            signal,
            item: 0,
            legend: false,
            outcome: None,
        });
        self.mode = Mode::SignalTray;
    }

    /// Open the legend, which is what the `···` button's popover contains.
    pub(crate) fn open_signal_tray_legend(&mut self) {
        self.signal_tray.popup = Some(SignalTrayPopup {
            // Anchored on the menu button rather than on a badge; the signal is
            // only carried so one type covers both contents.
            signal: FleetSignal::ALL[0],
            item: 0,
            legend: true,
            outcome: None,
        });
        self.mode = Mode::SignalTray;
    }

    /// Point the open popover at the next thing its badge covers.
    fn cycle_signal_tray_item(&mut self) {
        let count = signal_tray::resolve(self)
            .badge(
                self.signal_tray
                    .popup
                    .as_ref()
                    .map_or(FleetSignal::ALL[0], |popup| popup.signal),
            )
            .items
            .len()
            .max(1);
        if let Some(popup) = self.signal_tray.popup.as_mut() {
            popup.item = (popup.item + 1) % count;
            // A new item is a new question: an outcome from the last one would
            // be read as belonging to this one.
            popup.outcome = None;
        }
    }
}

impl App {
    /// Close the popover and hand the mode back.
    pub(crate) fn close_signal_tray_popup(&mut self) {
        self.state.signal_tray.popup = None;
        leave_modal(&mut self.state);
    }

    pub(crate) fn handle_signal_tray_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_signal_tray_popup(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.state.cycle_signal_tray_item();
            }
            // Enter is the escape hatch the popover advertises: it opens the
            // thing rather than answering it, which is the safe default for a
            // key that gets pressed by reflex.
            KeyCode::Enter => self.press_signal_tray_button(Button::Open),
            KeyCode::Char('y') => self.press_signal_tray_button(Button::Yes),
            KeyCode::Char('n') => self.press_signal_tray_button(Button::No),
            _ => {}
        }
    }

    /// Returns whether the event was consumed by the open popover.
    pub(crate) fn handle_signal_tray_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode != Mode::SignalTray || self.state.signal_tray.popup.is_none() {
            return false;
        }
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return true;
        }

        if let Some(button) =
            crate::ui::signal_tray_popup::button_at(&self.state, mouse.column, mouse.row)
        {
            self.press_signal_tray_button(button);
            return true;
        }

        // A second click on the same badge closes, the way the tray's own
        // affordance should: the badge is a toggle, not a one-way door.
        if let Some(signal) = crate::ui::signal_tray_badge_at(&self.state, mouse.column, mouse.row)
        {
            let same = self
                .state
                .signal_tray
                .popup
                .as_ref()
                .is_some_and(|popup| !popup.legend && popup.signal == signal);
            if same {
                self.close_signal_tray_popup();
            } else {
                self.state.open_signal_tray_popup(signal);
            }
            return true;
        }
        if crate::ui::signal_tray_menu_at(&self.state, mouse.column, mouse.row) {
            let was_legend = self
                .state
                .signal_tray
                .popup
                .as_ref()
                .is_some_and(|popup| popup.legend);
            if was_legend {
                self.close_signal_tray_popup();
            } else {
                self.state.open_signal_tray_legend();
            }
            return true;
        }

        // Click away: a popover is something you glance at, so getting out of
        // it must never need aim.
        if !crate::ui::signal_tray_popup::contains(&self.state, mouse.column, mouse.row) {
            self.close_signal_tray_popup();
        }
        true
    }

    /// Act on one of the popover's buttons.
    ///
    /// The badge and the item both come out of the popover's own view rather
    /// than being resolved a second time here, so a button acts on exactly what
    /// the popover was showing when it was pressed. That matters most when the
    /// item index is stale — a worker finishing under an open popup shrinks its
    /// badge's item list, and the view is where that is normalised.
    fn press_signal_tray_button(&mut self, button: Button) {
        let Some(view) = crate::ui::signal_tray_popup::view(&self.state) else {
            return;
        };
        if self
            .state
            .signal_tray
            .popup
            .as_ref()
            .is_none_or(|popup| popup.legend)
        {
            return;
        }
        let badge = view.badge;
        let item_index = view.item;

        match button {
            Button::Next => self.state.cycle_signal_tray_item(),
            Button::Open => {
                let Some(item) = badge.item(item_index) else {
                    return;
                };
                let target = item.target.clone();
                self.close_signal_tray_popup();
                self.jump_to_tray_target(target);
            }
            // Every acting button asks the badge for its command, and the badge
            // is what enforces the refusal. A button whose command is `None`
            // does nothing rather than falling through to something else.
            Button::Yes | Button::No => {
                let yes = matches!(button, Button::Yes);
                if let Some(command) = badge.command(&self.state, item_index, yes) {
                    self.run_tray_command(command);
                }
            }
            Button::Run | Button::Sweep => {
                if let Some(command) = badge.command(&self.state, item_index, true) {
                    self.run_tray_command(command);
                }
            }
        }
    }

    /// Go to the thing the popover was pointed at.
    fn jump_to_tray_target(&mut self, target: TrayTarget) {
        match target {
            TrayTarget::Pane { ws_idx, pane_id } => {
                self.focus_pane_internal_via_api(ws_idx, pane_id);
            }
            TrayTarget::Checkout { ws_idx } => self.focus_workspace_idx_via_api(ws_idx),
            TrayTarget::Summaries { owner } => self.state.open_worker_summaries(owner),
            TrayTarget::Url(url) => {
                if url.is_empty() {
                    return;
                }
                if let Err(err) = crate::platform::open_url(&url) {
                    tracing::warn!(%url, %err, "could not open the forge page for a tray badge");
                }
            }
        }
    }

    /// Run one in-place act.
    ///
    /// The two that only touch Herdr's own state run here and now. The two that
    /// run git go to a background thread: a remote that has gone slow must not
    /// be able to pin the app loop, and a `git push` that failed has to say so
    /// rather than being assumed to have worked.
    fn run_tray_command(&mut self, command: TrayCommand) {
        match command {
            TrayCommand::Answer {
                ws_idx,
                pane_id,
                yes,
            } => {
                let answer = if yes { "y" } else { "n" };
                let outcome = match self.send_tray_answer(ws_idx, pane_id, answer) {
                    Ok(()) => TrayOutcome {
                        ok: true,
                        message: format!("sent \"{answer}\""),
                    },
                    Err(message) => TrayOutcome { ok: false, message },
                };
                self.state.set_tray_outcome(outcome);
            }
            TrayCommand::MarkAllSeen => {
                let cleared = self.state.mark_every_pane_seen();
                self.state.set_tray_outcome(TrayOutcome {
                    ok: true,
                    message: format!("marked {cleared} pane{} seen", plural(cleared)),
                });
            }
            // Both go through `TrayCommand::argv`, which is also what the
            // popover printed — so what runs and what was confirmed are the
            // same list, produced once.
            command @ (TrayCommand::Push { .. } | TrayCommand::Sync { .. }) => {
                let ws_idx = match &command {
                    TrayCommand::Push { ws_idx, .. } | TrayCommand::Sync { ws_idx, .. } => *ws_idx,
                    _ => return,
                };
                if let Some(args) = command.argv() {
                    self.spawn_tray_git(ws_idx, args);
                }
            }
        }
    }

    /// Write the answer into the pane, through the same encoder the send-keys
    /// API uses so nothing here can put bytes on a PTY that the API would have
    /// rejected.
    fn send_tray_answer(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        answer: &str,
    ) -> Result<(), String> {
        let runtime = self
            .lookup_runtime_sender(ws_idx, pane_id)
            .ok_or_else(|| "that pane is gone".to_string())?;
        let encoded = crate::app::api_helpers::encode_api_keys(
            runtime,
            &[answer.to_string(), "enter".to_string()],
        )
        .map_err(|key| format!("cannot send {key}"))?;
        for bytes in encoded {
            runtime
                .try_send_bytes(bytes::Bytes::from(bytes))
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    /// Run `git` in a Space's checkout, off the app loop.
    ///
    /// Only ever reached with an argv the popover has already printed in full.
    fn spawn_tray_git(&mut self, ws_idx: usize, args: Vec<String>) {
        let Some(workspace) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let cwd = workspace.cached_identity_cwd.clone();
        let signal = self
            .state
            .signal_tray
            .popup
            .as_ref()
            .map_or(FleetSignal::Push, |popup| popup.signal);
        self.state.set_tray_outcome(TrayOutcome {
            ok: true,
            message: format!("running git {}…", args.join(" ")),
        });

        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let output = crate::noninteractive_process::command("git")
                .arg("-C")
                .arg(&cwd)
                .args(&args)
                .output();
            let (ok, message) = match output {
                Ok(output) if output.status.success() => (true, first_line(&output.stdout, "done")),
                Ok(output) => (false, first_line(&output.stderr, "git refused")),
                Err(err) => (false, err.to_string()),
            };
            let _ = event_tx.blocking_send(AppEvent::SignalTrayCommandFinished {
                signal_name: signal.name(),
                ok,
                message,
            });
        });
    }
}

impl AppState {
    /// Record what an in-place act reported, for the popover to print.
    ///
    /// A no-op when the popover has already been closed: an outcome that
    /// arrived after the captain walked away has nowhere to be shown, and
    /// re-opening the popover to show it would be the tray taking the screen
    /// for something he stopped asking about.
    pub(crate) fn set_tray_outcome(&mut self, outcome: TrayOutcome) {
        if let Some(popup) = self.signal_tray.popup.as_mut() {
            popup.outcome = Some(outcome);
        }
    }

    /// Clear the unseen mark on every pane that has one.
    ///
    /// The `review` badge's sweep. Not aimed at any single pane on purpose: it
    /// is the act of saying "I have looked at the list", and doing it one row
    /// at a time is what the tree is already for.
    pub(crate) fn mark_every_pane_seen(&mut self) -> usize {
        let mut cleared = 0;
        for workspace in &mut self.workspaces {
            for tab in &mut workspace.tabs {
                for pane in tab.panes.values_mut() {
                    if !pane.seen {
                        pane.seen = true;
                        cleared += 1;
                    }
                }
            }
        }
        cleared
    }
}

/// The first useful line of a subprocess's output, for the popover to print.
fn first_line(bytes: &[u8], fallback: &str) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(72).collect())
        .unwrap_or_else(|| fallback.to_string())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input::app_for_mouse_test;

    fn tray_app() -> App {
        let mut app = app_for_mouse_test();
        app.state.sidebar_signal_tray.enabled = true;
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 42, 60);
        app.state.view.terminal_area = ratatui::layout::Rect::new(43, 0, 117, 60);
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app
    }

    #[test]
    fn opening_a_badge_acknowledges_its_escalation() {
        let mut app = tray_app();
        app.state
            .signal_tray
            .observe(escalated(FleetSignal::Ask, 2));
        assert_eq!(
            signal_tray::resolve(&app.state)
                .badge(FleetSignal::Ask)
                .state,
            crate::app::signal_tray::BadgeState::Idle,
            "no blocked pane, so nothing is lit"
        );

        app.state.open_signal_tray_popup(FleetSignal::Ask);
        assert_eq!(app.state.mode, Mode::SignalTray);
        // Opening is the acknowledgement: looking at it is what it was asking
        // for.
        app.state.open_signal_tray_popup(FleetSignal::Ask);
        assert!(app.state.signal_tray.popup.is_some());
    }

    fn escalated(signal: FleetSignal, count: usize) -> [usize; FleetSignal::COUNT] {
        let mut counts = [0usize; FleetSignal::COUNT];
        let index = FleetSignal::ALL
            .iter()
            .position(|candidate| *candidate == signal)
            .unwrap_or(0);
        counts[index] = count;
        counts
    }

    #[test]
    fn esc_closes_and_drops_the_popup() {
        let mut app = tray_app();
        app.state.open_signal_tray_popup(FleetSignal::Push);
        app.handle_signal_tray_key(KeyEvent::from(KeyCode::Esc));
        assert_ne!(app.state.mode, Mode::SignalTray);
        assert!(app.state.signal_tray.popup.is_none());
    }

    /// A second click on the same badge closes it. The badge is a toggle: a
    /// popover you can only get out of by aiming somewhere else is a trap.
    #[test]
    fn a_second_click_on_the_same_badge_closes_the_popover() {
        let mut app = tray_app();
        app.state.workspaces[0].cached_git_ahead_behind = Some((2, 0));
        let area = crate::ui::sidebar_content_rect(app.state.view.sidebar_rect);
        let tray = crate::ui::sidebar::tray::tray_rect(&app.state, area);
        let grid = crate::ui::sidebar::tray::grid_rect(tray);
        let push_index = FleetSignal::ALL
            .iter()
            .position(|s| *s == FleetSignal::Push)
            .expect("push is one of the eight");
        let slot = crate::ui::sidebar::tray::slot_rect(grid, push_index);

        app.state.open_signal_tray_popup(FleetSignal::Push);
        let consumed = app.handle_signal_tray_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: slot.x,
            row: slot.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(consumed);
        assert!(app.state.signal_tray.popup.is_none());
    }

    /// Clicking away closes, without aim.
    #[test]
    fn clicking_away_closes_the_popover() {
        let mut app = tray_app();
        app.state.open_signal_tray_popup(FleetSignal::Push);
        app.handle_signal_tray_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 150,
            row: 55,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(app.state.signal_tray.popup.is_none());
    }

    /// The sweep clears every unseen finish, and says how many.
    #[test]
    fn the_review_sweep_clears_every_unseen_pane() {
        let mut app = tray_app();
        for tab in &mut app.state.workspaces[0].tabs {
            for pane in tab.panes.values_mut() {
                pane.seen = false;
            }
        }
        let cleared = app.state.mark_every_pane_seen();
        assert!(cleared > 0);
        assert_eq!(
            app.state.mark_every_pane_seen(),
            0,
            "the sweep is idempotent"
        );
    }

    /// The safety property, at the input layer: a refused `sync` must not run
    /// even when the button is pressed by some other path.
    #[test]
    fn pressing_run_on_a_refused_sync_does_nothing() {
        let mut app = tray_app();
        app.state.workspaces[0].cached_git_ahead_behind = Some((0, 3));
        app.state.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
            staged: 0,
            unstaged: 1,
            untracked: 0,
        });
        app.state.workspaces[0].cached_git_branch = Some("feature".into());
        app.state.open_signal_tray_popup(FleetSignal::Sync);

        app.press_signal_tray_button(Button::Run);
        // Nothing ran, so nothing reported. An outcome here would mean a
        // refused command had been started.
        assert_eq!(
            app.state
                .signal_tray
                .popup
                .as_ref()
                .and_then(|popup| popup.outcome.as_ref()),
            None
        );
    }

    #[test]
    fn cycling_moves_to_the_next_item_and_drops_the_last_outcome() {
        let mut app = tray_app();
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("two"));
        app.state.ensure_test_terminals();
        app.state.workspaces[0].cached_git_ahead_behind = Some((1, 0));
        app.state.workspaces[1].cached_git_ahead_behind = Some((1, 0));

        app.state.open_signal_tray_popup(FleetSignal::Push);
        app.state.set_tray_outcome(TrayOutcome {
            ok: true,
            message: "old news".into(),
        });
        app.state.cycle_signal_tray_item();

        let popup = app.state.signal_tray.popup.as_ref().expect("still open");
        assert_eq!(popup.item, 1);
        assert_eq!(popup.outcome, None);
    }

    #[test]
    fn the_legend_toggles_from_its_own_button() {
        let mut app = tray_app();
        app.state.open_signal_tray_legend();
        assert!(app
            .state
            .signal_tray
            .popup
            .as_ref()
            .is_some_and(|popup| popup.legend));
        // The legend has no acting buttons, so pressing one is a no-op rather
        // than a panic.
        app.press_signal_tray_button(Button::Run);
        assert!(app.state.signal_tray.popup.is_some());
    }
}
