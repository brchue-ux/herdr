use std::time::Instant;

use crossterm::terminal;

use super::{
    background_update_check_enabled, App, AUTO_UPDATE_CHECK_INTERVAL, MIN_RENDER_INTERVAL,
    RESIZE_POLL_INTERVAL, SELECTION_AUTOSCROLL_INTERVAL,
};
/// The life the failure spider is given: climb in, rest pulsing, retreat back
/// down once the card it is on clears.
///
/// Mirrors [`crate::app::card_wash::CardWashes::lifecycle`] — a `Lifecycle` is
/// cheap enough to build fresh on every pass rather than cached, and building
/// it beside its one call site keeps the mount/idle/dismount stages next to
/// the reasoning for them. Unlike a card wash, this element does have a
/// dismount: a wash is an event with nothing left to be once it settles, but
/// a spider that has climbed to a card it is still sitting on has to leave the
/// same way it arrived, not vanish.
/// Everything the background scene's ambient loop depends on, folded into one number: the
/// screen's pixel dimensions and the fleet tree's own shape (which bodies exist, their kind,
/// lifecycle stage and severity — a `Vec<TreeNode>`'s field values, since the type itself has no
/// `Hash`).
fn background_scene_key(
    nodes: &[crate::solar_system::TreeNode],
    area: ratatui::layout::Rect,
    cell: crate::kitty_graphics::HostCellSize,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (area.width, area.height).hash(&mut hasher);
    (cell.width_px, cell.height_px).hash(&mut hasher);
    nodes.len().hash(&mut hasher);
    for node in nodes {
        node.parent.hash(&mut hasher);
        (node.kind as u8).hash(&mut hasher);
        node.stage.hash(&mut hasher);
        node.severity.hash(&mut hasher);
        // A project that grew is a body that has to be redrawn at its new radius; leaving `size`
        // out of the key would cache the scene at whatever size it first saw.
        node.size.hash(&mut hasher);
        // ...and the same for the streak, which decides a mate's ring width and brightness, its
        // gas swell, and — through the size ranking it shares with `size` — nothing else. A
        // streak that climbed a band has to reach the picture, so it belongs in the key even
        // though it is quantized to the nearest expression step rather than a raw score.
        node.streak.to_bits().hash(&mut hasher);
        // The drawn wear step, not the raw revolution count: the count advances every tick and the
        // step does not, so hashing the count would rebake the whole loop on every pass forever.
        // This is exactly why `OrbitWear` is quantized at all.
        node.wear.to_bits().hash(&mut hasher);
        // A mote is a permanent mark on the track, so a new one is a new picture. The count is
        // already bounded by what the scene draws, and each one costs a rebake by construction —
        // which is the honest price of "every mote traces to one event".
        node.motes.hash(&mut hasher);
        node.mote_share.to_bits().hash(&mut hasher);
        // The caption is drawn into the baked frames, so a renamed Space is a new picture. Left out
        // of the key, a rename would keep showing the old name until something else moved.
        node.label.as_str().hash(&mut hasher);
    }
    hasher.finish()
}

/// One coherent whole-scene snapshot being rendered away from the loop that owns every pane.
///
/// Frames, layout, and row identity travel together so the effects overlay never targets body
/// positions from a different bake. The receiver is polled; the app/server loop never waits for
/// the worker.
pub(crate) struct BackgroundSceneBake {
    input: BackgroundSceneBakeInput,
    result_rx: std::sync::mpsc::Receiver<Result<Vec<Vec<u8>>, ()>>,
}

struct BackgroundSceneBakeInput {
    key: u64,
    width: u32,
    height: u32,
    identity: Vec<crate::anim::CardRow>,
    layout: crate::solar_system::SceneLayout,
}

/// Replacing the root frame re-arms terminal playback at phase zero. A replacement therefore
/// gets at least one complete animation loop before another bake may replace it.
const BACKGROUND_SCENE_REBAKE_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(crate::app::background_scene::LOOP_DURATION_MS);

struct FinishedBackgroundSceneBake {
    input: BackgroundSceneBakeInput,
    frames: Vec<Vec<u8>>,
}

/// `pub(crate)` so the pixel card's own tests mount the *real* lifecycle
/// rather than a copy of it: a spider whose test fixture had its own stage
/// table would keep passing after this one changed shape.
pub(crate) fn failure_spider_lifecycle() -> crate::anim::Lifecycle {
    use crate::anim::behaviour::{names, FAILURE_SPIDER_CLIMB_PERIOD};
    use crate::anim::{Lifecycle, Stage};

    Lifecycle::still()
        .with_mount(Stage::new(
            names::FAILURE_SPIDER_CLIMB,
            FAILURE_SPIDER_CLIMB_PERIOD,
        ))
        .with_idle(names::FAILURE_SPIDER_PULSE)
        .with_dismount(Stage::new(
            names::FAILURE_SPIDER_CLIMB,
            FAILURE_SPIDER_CLIMB_PERIOD,
        ))
}

fn retain_custom_command_after_wait(
    pid: u32,
    result: std::io::Result<Option<std::process::ExitStatus>>,
) -> bool {
    match result {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
        Err(err) => {
            tracing::warn!(pid, err = %err, "failed to reap detached custom command");
            false
        }
    }
}

impl App {
    pub(crate) fn reap_finished_custom_commands(&mut self) {
        self.detached_custom_command_children
            .retain_mut(|child| retain_custom_command_after_wait(child.id(), child.try_wait()));
    }

    pub(crate) fn shutdown_terminal_runtime(&mut self, terminal_id: crate::terminal::TerminalId) {
        let target = super::TerminalInputTarget {
            terminal_id: terminal_id.clone(),
        };
        self.release_input_target_headless(&target);
        self.state.pane_unread.remove(&terminal_id);
        if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
    }

    pub(crate) fn shutdown_detached_terminal_runtimes(&mut self) {
        let terminal_ids = std::mem::take(&mut self.state.terminal_runtime_shutdowns);
        for terminal_id in terminal_ids {
            self.shutdown_terminal_runtime(terminal_id);
        }
    }

    pub(crate) fn drain_api_requests(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.api_rx.try_recv() {
            changed |= self.handle_api_request_message(msg);
            self.shutdown_detached_terminal_runtimes();
        }
        changed
    }

    pub(super) fn handle_api_request_message(
        &mut self,
        msg: crate::api::ApiRequestMessage,
    ) -> bool {
        let previous_mode = self.state.mode;
        let mut changed = self.expire_due_metadata(Instant::now());
        changed |= crate::api::request_changes_ui(&msg.request);
        let skip_default_workspace = matches!(
            &msg.request.method,
            crate::api::schema::Method::ServerStop(_)
                | crate::api::schema::Method::ServerLiveHandoff(_)
        );
        if matches!(
            &msg.request.method,
            crate::api::schema::Method::WorktreeCreate(_)
                | crate::api::schema::Method::WorktreeRemove(_)
        ) {
            self.drain_all_internal_events();
            let deferred_changed =
                self.handle_deferred_worktree_api_request(msg.request, msg.respond_to);
            if !skip_default_workspace {
                changed |= self.ensure_default_workspace();
            }
            self.sync_prefix_input_source(previous_mode);
            return changed | deferred_changed;
        }
        let response = self.handle_api_request(msg.request);
        if !skip_default_workspace {
            changed |= self.ensure_default_workspace();
        }
        let _ = msg.respond_to.send(response);
        self.sync_prefix_input_source(previous_mode);
        changed
    }

    pub(super) async fn handle_raw_input_batch(
        &mut self,
        first: crate::raw_input::RawInputEvent,
    ) -> bool {
        let mut changed = self.handle_raw_input_event(first).await;

        while let Some(rx) = self.input_rx.as_mut() {
            match rx.try_recv() {
                Ok(event) => changed |= self.handle_raw_input_event(event).await,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.input_rx = None;
                    break;
                }
            }
        }

        changed
    }

    async fn execute_repeat_plan(
        &mut self,
        lease_key: super::input::InputLeaseKey,
        key: crate::input::TerminalKey,
        plan: super::input::RepeatPlan,
    ) -> bool {
        match plan {
            super::input::RepeatPlan::Forwarded(target) => {
                if !self.forward_terminal_key_to_target(&target, key).await {
                    self.input_leases.remove(&lease_key);
                }
                true
            }
            super::input::RepeatPlan::Reprocess {
                context,
                repetitions,
                tracked,
            } => {
                let key = key
                    .with_kind(crossterm::event::KeyEventKind::Repeat)
                    .with_repeat_count(1);
                let mut forwarded_target = None;
                for _ in 0..repetitions {
                    if let Some(target) = &forwarded_target {
                        if !self
                            .forward_terminal_key_to_target(target, key.clone())
                            .await
                        {
                            self.input_leases.remove(&lease_key);
                            break;
                        }
                        continue;
                    }
                    let current_context = self.terminal_input_context();
                    if !self.input_leases.reprocess_allowed(
                        lease_key,
                        &context,
                        current_context.as_ref(),
                        tracked,
                    ) {
                        break;
                    }
                    if let Some(target) = self.handle_key(key.clone()).await {
                        if tracked {
                            self.input_leases.insert_forwarded(
                                lease_key,
                                target.clone(),
                                key.clone(),
                            );
                            forwarded_target = Some(target);
                        }
                    }
                }
                true
            }
            super::input::RepeatPlan::Ignore => false,
        }
    }

    pub(super) async fn handle_raw_input_event(
        &mut self,
        event: crate::raw_input::RawInputEvent,
    ) -> bool {
        let previous_mode = self.state.mode;
        let changed = match event {
            crate::raw_input::RawInputEvent::Key(key) => {
                let lease_key = super::input::InputLeaseKey::new(super::LOCAL_INPUT_SOURCE, &key);
                let key = self.input_leases.normalize_press(&lease_key, key);
                match key.kind {
                    crossterm::event::KeyEventKind::Press => {
                        let initial_context = self.terminal_input_context();
                        let target = self.handle_key(key.clone()).await;
                        let resulting_context = self.terminal_input_context();
                        let plan = self.input_leases.complete_press(
                            lease_key,
                            &key,
                            initial_context.as_ref(),
                            resulting_context.as_ref(),
                            target,
                        );
                        self.execute_repeat_plan(lease_key, key, plan).await;
                        true
                    }
                    crossterm::event::KeyEventKind::Repeat => {
                        let current_context = self.terminal_input_context();
                        let plan = self.input_leases.plan_repeat(
                            lease_key,
                            &key,
                            current_context.as_ref(),
                        );
                        self.execute_repeat_plan(lease_key, key, plan).await
                    }
                    crossterm::event::KeyEventKind::Release => {
                        if let Some(lease) = self.input_leases.remove_forwarded(&lease_key) {
                            let _ = self
                                .forward_terminal_key_to_target(&lease.target, key)
                                .await;
                        }
                        false
                    }
                }
            }
            crate::raw_input::RawInputEvent::Text(text) => {
                self.handle_text_commit(text.into_string()).await;
                true
            }
            crate::raw_input::RawInputEvent::Paste(text) => {
                self.handle_paste(text).await;
                true
            }
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                let changes_view = !matches!(mouse.kind, crossterm::event::MouseEventKind::Moved)
                    || self.state.mode.mouse_motion_changes_view();
                let divider_hover_before = self.state.sidebar_divider_hover;
                let divider_detent_before = self.state.sidebar_divider_detent;
                if self.state.popup_pane.is_some() || self.state.mouse_capture {
                    self.handle_mouse(mouse);
                } else {
                    self.state
                        .handle_pane_mouse_only(&self.terminal_runtimes, mouse);
                }
                // The divider's hover state is drawn, so a motion that crosses
                // into or out of its grab band is not the render-neutral motion
                // the mode-level check above assumes. The detent is drawn too,
                // and it is the one case where the width deliberately does not
                // change, so nothing else would ask for the repaint.
                changes_view
                    || self.state.sidebar_divider_hover != divider_hover_before
                    || self.state.sidebar_divider_detent != divider_detent_before
            }
            crate::raw_input::RawInputEvent::OuterFocusGained => {
                self.send_outer_focus_event(crate::ghostty::FocusEvent::Gained);
                if self.state.redraw_on_focus_gained {
                    self.request_repaint();
                }
                self.state.outer_terminal_focus = Some(true);
                self.state.mark_active_tab_seen();
                true
            }
            crate::raw_input::RawInputEvent::OuterFocusLost => {
                self.release_input_source(super::LOCAL_INPUT_SOURCE).await;
                self.send_outer_focus_event(crate::ghostty::FocusEvent::Lost);
                self.state.outer_terminal_focus = Some(false);
                false
            }
            crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                self.update_host_terminal_theme(kind, color)
            }
            crate::raw_input::RawInputEvent::HostPaletteColors { colors } => {
                self.update_host_terminal_palette_colors(&colors)
            }
            crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                self.query_host_terminal_theme();
                self.set_host_terminal_appearance(appearance, true)
            }
            // Cell size reports are consumed by the thin client, not the runtime.
            crate::raw_input::RawInputEvent::HostCellSizeReport { .. } => false,
            crate::raw_input::RawInputEvent::KittyGraphicsCapability(confirmed) => {
                self.update_kitty_graphics_capability(confirmed)
            }
            crate::raw_input::RawInputEvent::HostTerminalIdentity(identity) => {
                self.update_host_terminal_identity(&identity)
            }
            crate::raw_input::RawInputEvent::Unsupported => false,
        };
        self.sync_prefix_input_source(previous_mode);
        self.shutdown_detached_terminal_runtimes();
        changed
    }

    fn handle_resize_poll(&mut self) -> bool {
        let Ok(size) = terminal::size() else {
            return false;
        };
        if self.last_terminal_size != Some(size) {
            self.last_terminal_size = Some(size);
            return true;
        }
        false
    }

    pub(crate) fn handle_scheduled_tasks(&mut self, now: Instant, geometry_dirty: bool) -> bool {
        let mut changed = false;
        let mut resized = false;

        self.refresh_state_age_clock(now);

        if now >= self.next_resize_poll {
            resized = self.handle_resize_poll();
            changed |= resized;
            self.next_resize_poll = now + RESIZE_POLL_INTERVAL;
        }

        if self
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.config_diagnostic_deadline = None;
            self.state.config_diagnostic = None;
            changed = true;
        }

        if self.toast_deadline.is_some_and(|deadline| now >= deadline) {
            self.toast_deadline = None;
            self.state.toast = None;
            changed = true;
        }

        if self
            .state
            .next_pending_agent_notification_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let previous_toast = self.state.toast.clone();
            let mut deliveries = self.state.drain_due_agent_notifications(now);
            if !deliveries.is_empty() {
                self.refresh_agent_notification_delivery_contexts(&mut deliveries);
                self.emit_delayed_client_local_agent_notifications(&deliveries);
                self.sync_toast_deadline(previous_toast);
                changed = true;
            }
        }

        if self
            .state
            .next_managed_agent_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let panes = self.state.reconcile_managed_agents_at(now);
            if !panes.is_empty() {
                for (ws_idx, pane_id) in panes {
                    self.emit_pane_updated(ws_idx, pane_id);
                }
                self.schedule_session_save();
                changed = true;
            }
        }

        if self
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.copy_feedback_deadline = None;
            self.state.copy_feedback = None;
            changed = true;
        }

        // The app's own loop is by definition its viewer.
        changed |= self.advance_animations(now, true);

        // Nothing to mutate: `state_age_now` already moved with the clock.
        // Reaching the deadline only means a drawn age is now stale, so the
        // one thing to do is ask for a repaint. Cleared here and re-armed by
        // the `sync_state_age_timer` at the end of this pass, so a fired
        // deadline can never sit in the past and spin the loop.
        if self
            .next_state_age_tick
            .is_some_and(|deadline| now >= deadline)
        {
            self.next_state_age_tick = None;
            changed = true;
        }
        changed |= self.advance_relation_signals(now, true);
        changed |= self.sample_pane_activity(now);
        changed |= self.observe_pane_unread(now);
        // Its own terminal is its own viewer, so this Herdr rasterises its
        // own badges — the delegated path is the server's, and only when every
        // client attached to it draws them instead.
        changed |= self.observe_signal_tray(now, false);
        changed |= self.observe_sidebar_particle_field();
        // Before the scene, so a corner drawn this pass carries this pass's sample rather than
        // the previous one's.
        changed |= self.observe_machine_register(now);
        // After the toast has been raised or cleared by this pass's events, so
        // the stream records what herdr actually said rather than what it said
        // last time round.
        changed |= self.observe_status_feed(now);
        // Before the scene, so a rebake this pass draws this pass's wear and this pass's motes.
        changed |= self.observe_orbit_tracks(now);
        changed |= self.observe_ambient_motes();
        changed |= self.observe_background_scene(now);
        // The app's own loop is by definition its viewer, same as `advance_animations` above.
        changed |= self.observe_background_effects(now, true);

        if self
            .selection_autoscroll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.tick_selection_autoscroll(now);
            changed = true;
        }

        changed |= self.clear_due_selection_highlight(now);

        self.start_git_status_refresh_if_due(now);
        self.start_pull_request_refresh_if_due(now);

        if self
            .next_auto_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_auto_update_check();
        }

        if self
            .next_agent_manifest_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_agent_manifest_update_check();
        }

        if self
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.start_background_session_save();
        }

        changed |= self.expire_due_metadata(now);

        if geometry_dirty || resized {
            self.pending_agent_resume_deadline = None;
        } else {
            self.sync_pending_agent_resume_deadline(now);
            changed |= self.start_pending_agent_resumes(self.pending_agent_resume_due(now));
        }
        self.sync_state_age_timer(now, true);
        changed
    }

    /// Moves live relation signals to the stop they are due at `now` and drops
    /// the ones that have expired.
    ///
    /// Runs on every loop iteration, not only while something is being drawn,
    /// so a signal always dies on schedule. What it gates is the *repaint*: the
    /// caller only learns that something changed when the change lands on a row
    /// the sidebar actually laid out, before or after this advance. A signal
    /// aimed at a collapsed sidebar, a mobile layout, or a row scrolled out of
    /// view therefore costs its own expiry and no frames at all.
    ///
    /// `has_viewers` is false when no client is rendering the app, so a detached
    /// server does not paint a travel nobody is looking at — the same rule the
    /// animation clock already follows.
    pub(crate) fn advance_relation_signals(&mut self, now: Instant, has_viewers: bool) -> bool {
        if self.state.relation_signals.is_empty() {
            return false;
        }
        let damaged_before = has_viewers && self.state.relation_signal_damage();
        let advanced = self.state.relation_signals.advance(now);
        advanced && (damaged_before || (has_viewers && self.state.relation_signal_damage()))
    }

    /// Take one work-volume reading from every live terminal and advance the
    /// activity signal.
    ///
    /// Runs on every loop iteration — the same rule relation signals follow —
    /// and deliberately *not* gated on whether anyone is looking. The level is a
    /// runtime fact about the session, published on the API the same way a
    /// pane's scroll position is, so a detached server answering `pane get`
    /// must report the truth rather than a number frozen at whatever it was
    /// when the last client left. What that costs is bounded by the sampler
    /// itself: once every pane has settled at zero it asks for no deadline, so
    /// a Herdr with nothing happening still parks indefinitely.
    pub(crate) fn sample_pane_activity(&mut self, now: Instant) -> bool {
        let changed = self
            .state
            .pane_activity
            .observe(now, self.terminal_runtimes.output_byte_counts());
        self.next_activity_sample = self.state.pane_activity.next_deadline(now);
        changed
    }

    /// Flip a backgrounded pane's `seen` bit to unread the moment new PTY
    /// content arrives on it — agent-detected or not, and regardless of
    /// `AgentState`. See `crate::app::pane_unread` for why this is a
    /// leading-edge latch rather than a debounce.
    ///
    /// Self-throttled to the tracker's own ~300ms cadence internally, so
    /// calling this on every loop pass (the same way `sample_pane_activity`
    /// and `observe_signal_tray` are already called) costs nothing extra in
    /// between polls. No dedicated wake deadline is armed for it: PTY output
    /// on any pane already requests a render via `RenderSignal::request_pty`
    /// regardless of that pane's visibility, which is what wakes this loop in
    /// time to catch it.
    ///
    /// A thin wrapper: the actual baseline diffing and `seen` flip live on
    /// `AppState`, testable with fake readings and no PTYs, the same split
    /// `sample_pane_activity` uses for `PaneActivityMap`. This layer's job is
    /// publishing the API events a silent state mutation wouldn't otherwise
    /// produce — `AppState::observe_pane_unread` has no `event_hub` to push
    /// through.
    pub(crate) fn observe_pane_unread(&mut self, now: Instant) -> bool {
        let flipped = self
            .state
            .observe_pane_unread(now, self.terminal_runtimes.detection_content_seq_counts());
        for (ws_idx, pane_id) in &flipped {
            self.publish_pane_unread_latch(*ws_idx, *pane_id);
        }
        !flipped.is_empty()
    }

    /// Publish for one pane that just latched unread: `PaneUpdated` always,
    /// so `pane.get`/subscribers see the new `unread` bit, and
    /// `PaneAgentStatusChanged` when the fused status actually moved. Only an
    /// `Idle` pane's `agent_status` depends on `seen` at all (see
    /// `pane_agent_status`) — a `Working`/`Blocked`/`Unknown` pane going
    /// unread changes `PaneInfo.unread` but not `agent_status`, so this stays
    /// silent on that event for those, matching the additive-only API design.
    fn publish_pane_unread_latch(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(terminal_state) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
            .map(|terminal| terminal.state)
        else {
            return;
        };
        // `observe_pane_unread` only ever flips `seen` from true to false, so
        // the "previous" fused status is always what `seen: true` would give.
        let previous_status = super::api_helpers::pane_agent_status(terminal_state, true);
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return;
        };
        self.emit_pane_updated(ws_idx, pane_id);
        if previous_status != pane.agent_status {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentStatusChanged,
                data: crate::api::schema::EventData::PaneAgentStatusChanged {
                    pane_id: pane.pane_id,
                    workspace_id: pane.workspace_id,
                    agent_status: pane.agent_status,
                    agent: pane.agent,
                    title: pane.title,
                    display_agent: pane.display_agent,
                    state_labels: pane.state_labels,
                },
            });
        }
    }

    /// Publish the sidebar rows that exist right now and move every animated
    /// element to where it is due at `now`.
    ///
    /// Membership is what drives arrivals and departures, so a workspace being
    /// created or closed is all it takes for its row to mount or retire — no
    /// call site has to remember to announce either.
    ///
    /// `has_viewers` is false when no client is rendering the app. A detached
    /// server then forgets every element rather than animating something nobody
    /// is looking at: the engine holds presentation state only, so dropping it
    /// costs nothing but the arrivals a client that was not attached could not
    /// have seen anyway.
    pub(crate) fn advance_animations(&mut self, now: Instant, has_viewers: bool) -> bool {
        let tree = has_viewers && self.state.sidebar_animation_active();
        let signals = has_viewers && self.state.fleet_signal_animation_active();
        let badges = has_viewers && self.state.signal_tray_animation_active();
        let washes = has_viewers
            && self.state.sidebar_cards.wash
            && self.state.sidebar_card_animation_active();
        // The view switch is gated on neither: a re-root *is* the behaviour
        // rather than a decoration on a row, and one already in flight has to
        // be able to finish even when nothing else in the panel is animating.
        let switching = has_viewers && self.state.tree_view_switch_active();
        // A command ack is an event a screen-detection scan fired moments ago,
        // not a configured feature — unlike every family above it, it has no
        // "is this switched on" flag of its own to gate on, only "is one
        // currently live". `tree` still folds in below so a card breathing for
        // an unrelated reason does not make this recompute `live` twice.
        let acks_pending = has_viewers && self.state.sidebar_cmd_acks.any_live();

        // The failure spider is the same shape of exception for the same
        // reason: a core failure signal, not a decorative animation toggle,
        // so unlike `tree`/`signals`/`badges` it is not gated on any
        // `[ui.sidebar.animation]` config — it has to climb on an
        // unconfigured Herdr. Its membership is read eagerly, ahead of the
        // cheap-exit check below, because that check would otherwise forget a
        // spider that has nothing else in the panel keeping the loop alive —
        // the same reason `switching` is in the check though it has no
        // feature gate either. `Animator::has_any` covers the falling edge:
        // once a card clears, its row drops out of `spider_members` on this
        // very pass, and the spider still needs the pass after that, and the
        // one after, to finish retreating back down the trunk.
        let spider_live = has_viewers && !self.state.sidebar_collapsed;
        let spider_members: Members = if spider_live {
            self.failing_card_rows(&crate::ui::sidebar_agent_live_entries(&self.state))
                .into_iter()
                .map(|row| {
                    (
                        crate::anim::ElementId::failure_spider(row),
                        crate::anim::behaviour::DriveInputs::default(),
                    )
                })
                .collect()
        } else {
            Members::new()
        };
        let spiders = spider_live
            && (!spider_members.is_empty()
                || self.state.anim.has_any(crate::anim::Family::FailureSpider));

        if !tree && !signals && !switching && !badges && !acks_pending && !spiders {
            let forgotten = self.state.anim.forget_all();
            let remembered = !self.state.sidebar_tree_row_memory.is_empty();
            self.state.sidebar_tree_row_memory.clear();
            // The card states go with the elements. Keeping them across a host
            // that has stopped drawing would make the first frame back wash
            // every card whose state moved while nobody was looking, and
            // animating history is exactly what `Animator::forget_all` exists
            // to prevent.
            let washed = self.state.sidebar_card_washes.forget_all();
            let acked = self.state.sidebar_cmd_acks.forget_all();
            return forgotten || remembered || washed || acked;
        }

        // Adopting a due root comes first, because it is what decides which
        // rows this pass draws.
        let switch_changed = self.state.advance_tree_view(now);

        // Every family is published on every pass, with an empty set when its
        // own feature is switched off, so turning one off retires exactly its
        // elements and leaves the others alone. Reconciling is per-family, so
        // the families can never evict each other. The view switch is not one of
        // them: it is a singleton driven by enter/leave, in its own family for
        // exactly that reason.
        type Members = Vec<(crate::anim::ElementId, crate::anim::behaviour::DriveInputs)>;
        // Families whose publisher also knows which of the lifecycle's
        // alternates each element is playing — see `crate::anim::Member`.
        type SelectedMembers = Vec<crate::anim::Member>;

        let lifecycle = self.state.sidebar_row_lifecycle();
        let spaces: SelectedMembers = if tree {
            crate::ui::sidebar_space_row_members(&self.state)
        } else {
            SelectedMembers::new()
        };
        let spaces_changed =
            self.state
                .anim
                .observe(now, crate::anim::Family::WorkspaceRow, &lifecycle, spaces);
        let agents_changed = self.observe_agent_rows(now, &lifecycle, tree, washes);
        let acks_changed = self.observe_cmd_acks(now, tree || acks_pending);

        // One segment per row with a gap still open beneath it — see
        // `sidebar_trunk_segment_members`. Its own lifecycle rather than the
        // rows' own: a segment's arrival is `row_enter`/`row_exit` timed the
        // same as a row's, but it declares none of a row's idle behaviours,
        // so a card's own pulse or glow cannot leak onto a rail through this
        // path.
        let trunk_lifecycle = self.state.sidebar_trunk_lifecycle();
        let trunk_members: Members = if tree {
            crate::ui::sidebar_trunk_segment_members(&self.state)
        } else {
            Members::new()
        };
        let trunk_changed = self.state.anim.observe(
            now,
            crate::anim::Family::TrunkSegment,
            &trunk_lifecycle,
            trunk_members,
        );

        let signal_lifecycle = self.state.sidebar_notifications.lifecycle();
        let signal_members: Members = if signals {
            crate::app::fleet_signals::FleetSignals::resolve(&self.state)
                .animation_membership()
                .collect()
        } else {
            Members::new()
        };
        let signals_changed = self.state.anim.observe(
            now,
            crate::anim::Family::Named,
            &signal_lifecycle,
            signal_members,
        );

        // The tray's eight badges. Published whole rather than filtered to the
        // live ones — see `TrayReading::animation_membership` — so the family
        // is either all eight or none, and a badge going quiet changes which
        // behaviour it plays rather than whether it exists.
        let badge_lifecycle = crate::app::signal_tray::BadgeState::lifecycle();
        let badge_members: SelectedMembers = if badges {
            crate::app::signal_tray::resolve(&self.state)
                .animation_membership()
                .collect()
        } else {
            SelectedMembers::new()
        };
        let badges_changed = self.state.anim.observe(
            now,
            crate::anim::Family::TrayBadge,
            &badge_lifecycle,
            badge_members,
        );

        let spider_lifecycle = failure_spider_lifecycle();
        let spiders_changed = self.state.anim.observe(
            now,
            crate::anim::Family::FailureSpider,
            &spider_lifecycle,
            spider_members,
        );

        switch_changed
            || spaces_changed
            || agents_changed
            || trunk_changed
            || acks_changed
            || signals_changed
            || badges_changed
            || spiders_changed
    }

    /// Every card that currently owns an open defect, as the row identity the
    /// failure spider mounts under.
    ///
    /// Resolved through [`crate::app::lifecycle::row_signal`], the same one
    /// function [`crate::ui::sidebar::render_failure_spiders`] draws from, so a
    /// card is marked here exactly when it is drawn marked. A row is marked
    /// when the fleet published a `sev` severity for it, or — with nothing
    /// published — when detection reads it as failed; a published `sev=-` says
    /// the defect is closed and unmounts the marker even on a failed row. See
    /// [`crate::quality_streak::defect_mark`], which owns that rule.
    /// `live` is the tree's own agent rows, handed in rather than gathered
    /// again, because [`Self::observe_agent_rows`] already computes the same
    /// list when the tree is being drawn; the one extra computation this
    /// causes when the tree is *not* being drawn (no row animation
    /// configured) is the cost of the spider working on an unconfigured
    /// Herdr, and it is bounded by the fleet's own size rather than by
    /// anything else in the panel.
    fn failing_card_rows(
        &self,
        live: &[crate::ui::sidebar::AgentPanelEntry],
    ) -> Vec<crate::anim::CardRow> {
        let mut rows = Vec::new();
        for workspace in &self.state.workspaces {
            let (state, _seen) = workspace.aggregate_state(&self.state.terminals);
            let signal =
                crate::app::lifecycle::row_signal(&workspace.metadata_tokens.values(), state);
            if signal.defect.is_some() {
                rows.push(crate::anim::CardRow::Space(workspace.id.clone()));
            }
        }
        for entry in live {
            let signal = crate::app::lifecycle::row_signal(&entry.tokens, entry.state);
            if signal.defect.is_some() {
                rows.push(crate::anim::CardRow::Agent(entry.pane_id));
            }
        }
        rows
    }

    /// Publish the owned agent rows that exist right now, so each second mate's
    /// group grows and shrinks on its own.
    ///
    /// Every worker and sub agent row is its own element, keyed by pane id, so
    /// a pane opening under one second mate mounts exactly one row and a pane
    /// closing dismounts exactly one — the other second mates' groups are not
    /// even looked at. That is the whole of "independently": there is no group
    /// object to rebuild, only rows that arrive and leave under the owner they
    /// already name.
    ///
    /// The rows are then remembered, because a departing row's pane is gone
    /// from the session and the tree could not otherwise draw it for the length
    /// of its exit. Only worth remembering when an exit is configured; without
    /// one the engine retires a departed row on the spot and memory would be a
    /// copy nobody reads.
    ///
    /// `tree` is false when the fleet signals are the only reason this pass is
    /// running at all. The rows are then published as an empty set rather than
    /// skipped: a family that is not being animated should say so, not go
    /// unmentioned.
    ///
    /// The `!tree` half of the memory check below cannot fire today, and is
    /// here so that it fails safe if that ever changes. Both features are gated
    /// on the sidebar being expanded, so a bar-only pass implies nothing at all
    /// is configured to animate the tree, which means no exit stage, which the
    /// `dismount.is_none()` half already catches. Let the two gates diverge —
    /// a bar that animates while collapsed, say — and this pass would reach a
    /// configured exit with no rows behind it, and a departed row's memory
    /// would never be dropped. `the_two_animation_gates_are_collapsed_together`
    /// is what pins the coupling this rests on.
    fn observe_agent_rows(
        &mut self,
        now: Instant,
        lifecycle: &crate::anim::Lifecycle,
        tree: bool,
        washes: bool,
    ) -> bool {
        let live = if tree {
            crate::ui::sidebar_agent_live_entries(&self.state)
        } else {
            Vec::new()
        };
        let washes_changed = self.observe_card_washes(now, &live, washes);
        let rows = crate::ui::sidebar_agent_row_members(&self.state, &live);
        let changed = self
            .state
            .anim
            .observe(now, crate::anim::Family::AgentRow, lifecycle, rows);
        if lifecycle.dismount.is_none() || !tree {
            let remembered = !self.state.sidebar_tree_row_memory.is_empty();
            self.state.sidebar_tree_row_memory.clear();
            return changed || remembered || washes_changed;
        }
        // Observed first, so a row that has just left is already dismounting and
        // survives this refresh; one whose exit finished is not, and is dropped.
        let drawn = crate::ui::rows_with_departing(&self.state, live);
        let moved = drawn.len() != self.state.sidebar_tree_row_memory.len();
        self.state.sidebar_tree_row_memory = drawn;
        changed || moved || washes_changed
    }

    /// Publish the state washes crossing the tree's cards right now.
    ///
    /// Reads the same live entries the rows were published from rather than
    /// gathering its own, because the two must agree about which cards exist: a
    /// wash on a row the tree is not drawing is a sweep nobody sees, and one
    /// missing from a row it *is* drawing is a state change that arrives with
    /// no announcement.
    ///
    /// Space cards are folded in beside the agent rows because the tree draws
    /// both as cards — a first mate's card changes state exactly as a worker's
    /// does, and a Space's state is the aggregate its own row already shows.
    ///
    /// `live_washes` is false when the wash is switched off or the pixel path is
    /// not drawing cards. The family is still published, as an empty set, for
    /// the reason the comment above `Members` gives: a family that is not being
    /// animated should say so rather than go unmentioned, so turning the wash
    /// off retires exactly its elements and touches nothing else.
    fn observe_card_washes(
        &mut self,
        now: Instant,
        live: &[crate::ui::sidebar::AgentPanelEntry],
        live_washes: bool,
    ) -> bool {
        let members = if live_washes {
            let cards: Vec<_> = self
                .state
                .workspaces
                .iter()
                .map(|workspace| {
                    (
                        crate::anim::CardRow::Space(workspace.id.clone()),
                        workspace.aggregate_state(&self.state.terminals).0,
                    )
                })
                .chain(
                    live.iter()
                        .map(|entry| (crate::anim::CardRow::Agent(entry.pane_id), entry.state)),
                )
                .collect();
            let window = self.state.sidebar_cards.wash_duration();
            self.state.sidebar_card_washes.observe(now, window, cards)
        } else {
            self.state.sidebar_card_washes.forget_all();
            Vec::new()
        };
        let lifecycle =
            crate::app::card_wash::CardWashes::lifecycle(self.state.sidebar_cards.wash_duration());
        self.state
            .anim
            .observe(now, crate::anim::Family::CardWash, &lifecycle, members)
    }

    /// Publish the command-acknowledgement markers live right now, and prune
    /// the ones this module no longer has any reason to remember.
    ///
    /// `active` is false when neither the tree nor a pending marker gives this
    /// pass a reason to look — see [`Self::advance_animations`]'s own
    /// `acks_pending` — in which case the live agent rows are not even fetched:
    /// there is nothing to prune against that a card leaving the tree could
    /// not already tell [`crate::app::cmd_ack::CmdAcks::observe`] by simply not
    /// being in `live_rows`.
    fn observe_cmd_acks(&mut self, now: Instant, active: bool) -> bool {
        let live_rows: Vec<crate::anim::CardRow> = if active {
            crate::ui::sidebar_agent_live_entries(&self.state)
                .into_iter()
                .map(|entry| crate::anim::CardRow::Agent(entry.pane_id))
                .collect()
        } else {
            Vec::new()
        };
        let active_window = crate::anim::behaviour::CMD_ACK_MOUNT_PERIOD
            + crate::anim::behaviour::CMD_ACK_HOLD_PERIOD;
        // Has to outlast `active_window` by at least the dismount stage's own
        // duration — see `CmdAcks::observe`'s own doc on why, or the sidebar
        // stops drawing a marker mid-fade.
        let retain_window = active_window + crate::anim::behaviour::CMD_ACK_DISMOUNT_PERIOD;
        let members =
            self.state
                .sidebar_cmd_acks
                .observe(now, active_window, retain_window, live_rows);
        let lifecycle = crate::app::cmd_ack::CmdAcks::lifecycle(
            crate::anim::behaviour::CMD_ACK_MOUNT_PERIOD,
            crate::anim::behaviour::CMD_ACK_DISMOUNT_PERIOD,
        );
        self.state
            .anim
            .observe(now, crate::anim::Family::CmdAck, &lifecycle, members)
    }

    /// Fold the fleet's current state into the notification tray.
    ///
    /// Three things happen here, and all three are mutations, which is exactly
    /// why they are here and not in the renderer:
    ///
    /// 1. **Escalation.** Whether a badge is merely lit or is demanding
    ///    attention is a *transition*, so it needs the previous reading to
    ///    compare against. [`crate::app::signal_tray::SignalTrayState::observe`]
    ///    is the only place that comparison is made.
    /// 2. **The question.** A blocked agent's screen lives behind a terminal
    ///    lock the pure renderer cannot take, so the text is snapshotted into
    ///    state on a clock — and only while the tray is on and something is
    ///    blocked, so a Herdr with a quiet fleet pays nothing.
    /// 3. **The artwork.** Eight badges rasterised from scratch is not a
    ///    per-frame cost, so it is redone only when the states, the grid or the
    ///    cell size move.
    ///
    /// `client_rasterized` says every viewer draws the badges itself from a
    /// `ServerMessage::TrayScene`, in which case step three is the one thing
    /// that does not happen here — see [`Self::refresh_signal_tray_graphics`].
    ///
    /// Returns whether anything a frame would show has changed.
    pub(crate) fn observe_signal_tray(&mut self, now: Instant, client_rasterized: bool) -> bool {
        let delegation_changed =
            self.state.signal_tray_graphics_client_rasterized != client_rasterized;
        self.state.signal_tray_graphics_client_rasterized = client_rasterized;
        if !crate::ui::signal_tray_active(&self.state) {
            let had = self.state.signal_tray_graphics.take().is_some();
            self.state.signal_tray_graphics_key = 0;
            self.state.signal_tray_published.forget();
            return had;
        }
        if delegation_changed {
            // Whichever way it moved, this process is no longer the one whose
            // raster the terminal is showing.
            self.state.signal_tray_published.forget();
        }

        // A tray that just changed sides has to repaint whichever way it moved:
        // the marks come off the grid when the client takes over, and back onto
        // it when the last such client leaves.
        let mut changed = delegation_changed;
        changed |= self.refresh_blocked_questions(now);
        changed |= self
            .state
            .signal_tray
            .observe(crate::app::signal_tray::magnitudes(&self.state));
        changed |= self.refresh_signal_tray_graphics(client_rasterized);
        changed
    }

    /// Snapshot what every blocked pane is showing.
    ///
    /// Wholesale, so a pane that stopped being blocked stops having a
    /// remembered question: a stale question under a live badge would be worse
    /// than none at all.
    fn refresh_blocked_questions(&mut self, now: Instant) -> bool {
        if !self.state.signal_tray.questions_are_due(now) {
            return false;
        }
        let blocked: Vec<(crate::layout::PaneId, crate::terminal::TerminalId)> = self
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .filter_map(|(pane_id, pane)| {
                let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                matches!(terminal.state, crate::detect::AgentState::Blocked)
                    .then(|| (*pane_id, pane.attached_terminal_id.clone()))
            })
            .collect();

        let questions = blocked
            .into_iter()
            .filter_map(|(pane_id, terminal_id)| {
                let runtime = self.terminal_runtimes.get(&terminal_id)?;
                let lines = crate::app::signal_tray::SignalTrayState::question_lines(
                    &runtime.detection_text(),
                );
                (!lines.is_empty()).then_some((pane_id, lines))
            })
            .collect();

        self.state.signal_tray.set_questions(now, questions);
        // The snapshot only reaches a frame through an open `ask` popup, so a
        // refresh nobody is looking at is not a reason to repaint.
        self.state
            .signal_tray
            .popup
            .as_ref()
            .is_some_and(|popup| popup.signal == crate::app::fleet_signals::FleetSignal::Ask)
    }

    /// Redraw the tray's badge artwork when what it was drawn for has moved.
    ///
    /// `client_rasterized` means every viewer draws the badges itself from a
    /// `ServerMessage::TrayScene`, so this stops at the key: rasterising eight
    /// badges for nobody is the whole cost this exists to move off the server,
    /// and the key is still what tells the loop a new scene is worth sending.
    ///
    /// The key moving is what makes a *raster* worth taking; whether that
    /// raster is worth an *upload* is a second question, and
    /// `AppState::signal_tray_published` is what answers it. A resting tray
    /// moves the key on every badge frame for a change of a fraction of one
    /// 8-bit level — see [`crate::app::state::PublishedSurfaceRaster`].
    fn refresh_signal_tray_graphics(&mut self, client_rasterized: bool) -> bool {
        // The fleet's cell, not the foreground client's: this artwork is one
        // image every attached viewer is placed a copy of, so a viewer whose
        // cell differs would be shown a crop of a raster built for someone
        // else's grid. See `AppState::shared_raster_cell_size`.
        let cell = self.state.shared_raster_cell_size();
        // `host_paints_pixel_surfaces` and not `kitty_graphics_enabled` alone:
        // this artwork is what `tray::artwork_covers_grid` stands the fallback
        // marks down for, so building it on a host the delivery gate will not
        // send it to draws the tray as a hole. See the predicate's own doc.
        if !self.state.host_paints_pixel_surfaces() || !cell.is_known() {
            let had = self.state.signal_tray_graphics.take().is_some();
            self.state.signal_tray_graphics_key = 0;
            self.state.signal_tray_published.forget();
            return had;
        }

        let key = self.signal_tray_graphics_key(cell);
        if client_rasterized {
            let had = self.state.signal_tray_graphics.take().is_some();
            let moved = key != self.state.signal_tray_graphics_key;
            self.state.signal_tray_graphics_key = key;
            if had {
                self.state.signal_tray_published.forget();
            }
            return had || moved;
        }
        if key == self.state.signal_tray_graphics_key && self.state.signal_tray_graphics.is_some() {
            return false;
        }

        let Some((_, image)) =
            crate::ui::signal_tray_image(&self.state, cell.width_px, cell.height_px)
        else {
            let had = self.state.signal_tray_graphics.take().is_some();
            self.state.signal_tray_graphics_key = 0;
            self.state.signal_tray_published.forget();
            return had;
        };

        self.state.signal_tray_graphics_key = key;
        // Drawn, and within a couple of levels of the artwork already on
        // screen: the terminal keeps what it has rather than being handed the
        // whole surface again.
        if !self
            .state
            .signal_tray_published
            .accept(image.width, image.height, &image.pixels)
            && self.state.signal_tray_graphics.is_some()
        {
            return false;
        }
        let layer = crate::ui::signal_tray_graphics_layer(
            image,
            self.state.host_terminal_kind,
            self.state.host_graphics_is_local,
        );
        let had = self.state.signal_tray_graphics.is_some();
        if layer.is_none() {
            // Nothing was published, so nothing is on screen to compare the
            // next raster against.
            self.state.signal_tray_published.forget();
        }
        self.state.signal_tray_graphics = layer;
        had || self.state.signal_tray_graphics.is_some()
    }

    /// Everything the artwork depends on, folded into one number.
    fn signal_tray_graphics_key(&self, cell: crate::kitty_graphics::HostCellSize) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let grid = crate::ui::signal_tray_graphics_rect(&self.state);
        (grid.x, grid.y, grid.width, grid.height).hash(&mut hasher);
        (cell.width_px, cell.height_px).hash(&mut hasher);
        for badge in crate::app::signal_tray::resolve(&self.state).badges() {
            badge.state.hash(&mut hasher);
        }
        // Where every badge is in its animation. Without this the artwork is
        // rasterised once per state change and then holds still, which is
        // exactly the shape of a badge that looks animated in the code and is
        // frozen on the screen.
        crate::ui::signal_tray_motion_fingerprint(&self.state).hash(&mut hasher);
        format!("{:?}", self.state.palette.peach).hash(&mut hasher);
        format!("{:?}", self.state.host_terminal_theme.background).hash(&mut hasher);
        hasher.finish()
    }

    /// (Re-)generate the sidebar's ambient particle-field wash when its size has moved.
    ///
    /// Unlike the tray's badges, this loop is not free to regenerate — it is a whole animation
    /// sequence — so it is redone only when [`Self::sidebar_particle_field_key`] moves (a
    /// resize), never once per tick. Once generated, uploading and arming playback is
    /// `kitty_graphics`'s job (`GraphicsLayer::animation`); this only owns producing the pixels.
    pub(crate) fn observe_sidebar_particle_field(&mut self) -> bool {
        // Shared with every viewer, so sized by the fleet's cell rather than
        // the foreground client's — see `AppState::shared_raster_cell_size`.
        let cell = self.state.shared_raster_cell_size();
        if !self.state.sidebar_particle_field_active() || !cell.is_known() {
            let had = self.state.sidebar_particle_field.take().is_some();
            self.state.sidebar_particle_field_key = 0;
            return had;
        }

        let key = self.sidebar_particle_field_key(cell);
        if key == self.state.sidebar_particle_field_key
            && self.state.sidebar_particle_field.is_some()
        {
            return false;
        }

        let Some(generated) = crate::ui::sidebar::particle_background::image(
            &self.state,
            cell.width_px,
            cell.height_px,
        ) else {
            let had = self.state.sidebar_particle_field.take().is_some();
            self.state.sidebar_particle_field_key = 0;
            return had;
        };

        self.state.sidebar_particle_field_key = key;
        self.state.sidebar_particle_field = Some(
            crate::app::state::GraphicsLayer::new(
                crate::api::schema::PaneGraphicsFormat::Rgba,
                generated.width,
                generated.height,
                generated.root,
                crate::api::schema::PaneGraphicsPlacementParams {
                    viewport_col: 0,
                    viewport_row: 0,
                    grid_cols: 0,
                    grid_rows: 0,
                    // Under any text the sidebar draws over it, over the cell background: an
                    // ambient backdrop rather than something that could obscure a row.
                    z: -1,
                },
            )
            .with_animation(crate::app::state::GraphicsAnimation {
                frame_gap_ms: crate::ui::sidebar::particle_background::FRAME_GAP_MS,
                frames: generated.extra_frames,
            }),
        );
        true
    }

    /// Everything the wash's pixels depend on, folded into one number.
    fn sidebar_particle_field_key(&self, cell: crate::kitty_graphics::HostCellSize) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let area = crate::ui::sidebar_particle_field_rect(&self.state);
        (area.width, area.height).hash(&mut hasher);
        (cell.width_px, cell.height_px).hash(&mut hasher);
        hasher.finish()
    }

    /// (Re-)generate the whole-terminal solar-system scene's ambient loop when the fleet's shape
    /// or the screen size has moved.
    ///
    /// Mirrors [`Self::observe_sidebar_particle_field`] exactly: a whole animation sequence is
    /// not free to regenerate, so it is redone only when [`Self::background_scene_key`] moves,
    /// never once per tick. See `src/app/background_scene.rs` for why the scene's *placement*
    /// (`AppState::background_scene_layout`) is cached alongside the rendered loop rather than
    /// only the encoded bytes: the effects overlay needs real body positions to draw against.
    /// Sample the host machine's own state, on the register's own cadence.
    ///
    /// Gated on [`crate::machine_register::MachineRegister::is_due`] rather than run per tick:
    /// this is three small `/proc` reads, but it is on the tick loop, and the tick loop is one of
    /// the multiplicative paths this project's own performance rule names. At a two-second cadence
    /// the whole register costs less than a thousandth of a percent of a core; per tick it would
    /// be filesystem I/O in a loop that is not allowed any.
    ///
    /// Sampled whether or not the scene is drawing, because the register is a runtime fact the
    /// session API publishes rather than a detail of one client's picture — and a corner that only
    /// had history for as long as the scene had been switched on would be a worse readout than one
    /// that is simply always current.
    pub(crate) fn observe_machine_register(&mut self, now: Instant) -> bool {
        if !self.state.machine_register.is_due(now) {
            return false;
        }
        let counters = crate::platform::read_machine_counters();
        self.state.machine_register.sample(counters, now);
        // Deliberately *not* `moved || …`. A sample landing is not by itself a reason to repaint:
        // the numbers reach a viewer only through the drawn corner, and the API reports them on
        // demand. Returning `true` here would arm a repaint every two seconds forever, on a
        // terminal where nothing had changed — which is what this returns `false` for.
        self.observe_machine_corner(now)
    }

    /// Record whatever herdr is currently saying into its own status stream.
    ///
    /// One hook rather than twenty. There are twenty places in this codebase
    /// that raise a toast, and a stream appended to at each of them would be
    /// nineteen places to forget; this watches the one field they all write. See
    /// [`crate::app::status_feed`].
    ///
    /// Runs on every tick rather than on the register's cadence, because a toast
    /// can be raised and replaced between two two-second samples and the stream
    /// exists precisely so that neither of them is lost. The work is one
    /// equality check against the toast already held, which is what makes that
    /// affordable on the tick loop.
    pub(crate) fn observe_status_feed(&mut self, now: Instant) -> bool {
        let toast = self.state.toast.clone();
        self.state.status_feed.observe(toast.as_ref(), now)
    }

    /// (Re-)draw the machine register's corner readout.
    ///
    /// Its own small graphics surface rather than a pass over the ambient scene or the effects
    /// overlay, and that is a cost decision rather than a tidiness one. The register moves every
    /// two seconds; re-baking the scene's 36-frame ambient loop at that cadence would cost more
    /// CPU than the whole rest of this feature, and folding it into the whole-screen effects
    /// overlay would make every machine sample repaint the entire terminal. A corner box is a few
    /// tens of thousands of pixels.
    ///
    /// Drawn whenever the scene is drawing — it is part of the same surface family and shares its
    /// gate — and cleared the moment it is not.
    fn observe_machine_corner(&mut self, now: Instant) -> bool {
        let rect = self.state.machine_corner_rect();
        let cell = self.state.shared_raster_cell_size();
        if !self.state.background_scene_active()
            || rect.width == 0
            || rect.height == 0
            || !cell.is_known()
        {
            self.state.machine_corner_key = 0;
            self.state.machine_corner_rgba = None;
            return self.state.machine_corner_layer.take().is_some();
        }

        let register = &self.state.machine_register;
        // F21: absent is absent. A stalled or unsupported register draws nothing at all rather
        // than holding its last picture on screen as though it were current.
        if register.absence(now).is_some() {
            self.state.machine_corner_key = 0;
            self.state.machine_corner_rgba = None;
            return self.state.machine_corner_layer.take().is_some();
        }

        // Everything the drawn corner depends on, folded into one number — the same shape
        // `background_scene_key` uses, and for the same reason: rendering on a timer and diffing
        // the PNG afterwards would pay the render either way.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (rect.width, rect.height).hash(&mut hasher);
            (cell.width_px, cell.height_px).hash(&mut hasher);
            register.generation().hash(&mut hasher);
            hasher.finish().max(1)
        };
        if key == self.state.machine_corner_key && self.state.machine_corner_layer.is_some() {
            return false;
        }

        let corner = crate::solar_system::MachineCorner {
            grooves: crate::machine_register::Quantity::ALL
                .iter()
                .map(|q| register.series(*q).history().collect())
                .collect(),
            cores: register.cores().iter().map(|core| core.current()).collect(),
        };

        let width = u32::from(rect.width) * cell.width_px;
        let height = u32::from(rect.height) * cell.height_px;
        let rgba = crate::solar_system::machine_corner_frame(&corner, width, height);
        let png = crate::solar_system::encode_rgba_png(width, height, &rgba);
        if png.is_empty() {
            self.state.machine_corner_rgba = None;
            return self.state.machine_corner_layer.take().is_some();
        }

        self.state.machine_corner_key = key;
        // Kept for the legibility pass, which needs pixels rather than a PNG to decide the
        // foreground for the cells this surface covers.
        self.state.machine_corner_rgba = Some(rgba);
        self.state.machine_corner_layer = Some(crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Png,
            width,
            height,
            png,
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 0,
                viewport_row: 0,
                grid_cols: 0,
                grid_rows: 0,
                // Above the effects overlay: this is a readout rather than part of the scene, and
                // a comet crossing behind it must not take it with it.
                z: -1,
            },
        ));
        true
    }

    /// Advance every body's orbit-track wear, and forget the bodies that have left.
    ///
    /// Runs every tick, and is cheap enough to: it is one multiply and one comparison per body.
    /// What it deliberately does *not* do is report a change every tick — the revolution counts
    /// advance continuously and the drawn wear steps do not, so it returns `true` only when a
    /// track has actually deepened enough to look different. Returning the former would rebake
    /// the whole ambient loop on every pass forever.
    ///
    /// Sampled whether or not the scene is currently drawing, because a track is a fact about how
    /// long a body has been in the fleet rather than about how long it has been *looked at*: a
    /// scene switched on after an hour's work should show the hour.
    pub(crate) fn observe_orbit_tracks(&mut self, now: Instant) -> bool {
        let (nodes, identity) = crate::app::background_scene::tree_nodes(&self.state);
        let rates: Vec<(&crate::anim::CardRow, f32)> = identity
            .iter()
            .zip(&nodes)
            .map(|(row, node)| (row, node.kind.revolutions_per_loop(node.size)))
            .collect();
        let mut tracks = std::mem::take(&mut self.state.orbit_tracks);
        let moved = tracks.advance(rates.into_iter(), now);
        self.state.orbit_tracks = tracks;
        moved
    }

    /// Consume every body's new work and emit its ambient motes.
    ///
    /// Fed from `AppState::pane_activity`'s lifetime output-byte counters, which is the per-body
    /// work register herdr actually holds — see `AmbientMotes` for why that is what the tier
    /// counts and what it would count instead if a command counter ever landed.
    ///
    /// A body only earns motes for work done *while it was being watched*: a pane already an hour
    /// into a build when the scene first sees it starts from wherever its counter is, rather than
    /// studding its whole orbit in one pass for work nobody was watching.
    pub(crate) fn observe_ambient_motes(&mut self) -> bool {
        let counts = crate::app::background_scene::ambient_mote_inputs(&self.state);
        let mut motes = std::mem::take(&mut self.state.ambient_motes);
        let emitted = motes.consume(counts.iter().map(|(row, bytes)| (row, *bytes)));
        self.state.ambient_motes = motes;
        emitted
    }

    pub(crate) fn observe_background_scene(&mut self, now: Instant) -> bool {
        let finished = self.take_finished_background_scene_bake();

        if !self.state.background_scene_active() {
            self.background_scene_deferred_bake_at = None;
            return self.clear_background_scene();
        }

        let area = self.state.screen_rect();
        // Shared with every viewer, so sized by the fleet's cell rather than
        // the foreground client's — see `AppState::shared_raster_cell_size`.
        let cell = self.state.shared_raster_cell_size();
        if area.width == 0 || area.height == 0 || !cell.is_known() {
            self.background_scene_deferred_bake_at = None;
            return self.clear_background_scene();
        }

        let (nodes, identity) = crate::app::background_scene::tree_nodes(&self.state);
        let key = background_scene_key(&nodes, area, cell);
        let width = u32::from(area.width) * cell.width_px;
        let height = u32::from(area.height) * cell.height_px;
        let layout = crate::solar_system::build_layout(&nodes, width, height);
        if layout.is_empty() {
            // No fleet to mirror yet — nothing ambient to draw.
            self.background_scene_deferred_bake_at = None;
            return self.clear_background_scene();
        }

        let mut changed = false;
        if let Some(finished) = finished {
            // Content may advance while 36 frames are rendering. Installing that coherent
            // snapshot gives the terminal a usable loop; the key mismatch below queues the latest
            // snapshot behind the rate floor. Pixel geometry is different: installing old-sized
            // frames after a resize would place the entire layer incorrectly, so those are
            // discarded.
            if finished.input.width == width && finished.input.height == height {
                changed |= self.adopt_baked_background_scene(finished, now);
            }
        }

        if key == self.state.background_scene_key && self.state.background_scene.is_some() {
            self.background_scene_deferred_bake_at = None;
            return changed;
        }

        // The current layer remains live while its replacement renders. There is only ever one
        // worker owned by this App, including across disable/resize churn.
        if self.background_scene_bake.is_some() {
            self.background_scene_deferred_bake_at = None;
            return changed;
        }

        if let Some(next) = self
            .background_scene_next_bake_at
            .filter(|next| now < *next)
        {
            self.background_scene_deferred_bake_at = Some(next);
            return changed;
        }

        self.background_scene_deferred_bake_at = None;
        self.spawn_background_scene_bake(key, width, height, identity, layout, now);
        changed
    }

    fn spawn_background_scene_bake(
        &mut self,
        key: u64,
        width: u32,
        height: u32,
        identity: Vec<crate::anim::CardRow>,
        layout: crate::solar_system::SceneLayout,
        now: Instant,
    ) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let bake_layout = layout.clone();
        let notify = std::sync::Arc::clone(&self.render_notify);
        let worker = std::thread::Builder::new()
            .name("herdr-scene-bake".into())
            .spawn(move || {
                let frames = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::solar_system::loop_frames_png(
                        &bake_layout,
                        crate::solar_system::FRAME_COUNT,
                    )
                }))
                .map_err(|_| ());
                let _ = tx.send(frames);
                // `Notify` retains a permit, so finishing between select passes is not a lost
                // wake. Failure wakes too, allowing the loop to retire the broken request.
                notify.notify_one();
            });

        self.background_scene_next_bake_at = Some(now + BACKGROUND_SCENE_REBAKE_INTERVAL);
        match worker {
            Ok(_) => {
                self.background_scene_bake = Some(BackgroundSceneBake {
                    input: BackgroundSceneBakeInput {
                        key,
                        width,
                        height,
                        identity,
                        layout,
                    },
                    result_rx: rx,
                });
            }
            Err(err) => {
                tracing::warn!(error = %err, "could not spawn background scene bake");
                self.background_scene_deferred_bake_at = self.background_scene_next_bake_at;
            }
        }
    }

    fn take_finished_background_scene_bake(&mut self) -> Option<FinishedBackgroundSceneBake> {
        let received = match self.background_scene_bake.as_ref() {
            Some(bake) => bake.result_rx.try_recv(),
            None => return None,
        };
        let result = match received {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(()),
        };
        let bake = self.background_scene_bake.take()?;
        match result {
            Ok(frames) => Some(FinishedBackgroundSceneBake {
                input: bake.input,
                frames,
            }),
            Err(()) => {
                tracing::warn!("background scene bake ended without frames");
                None
            }
        }
    }

    fn adopt_baked_background_scene(
        &mut self,
        bake: FinishedBackgroundSceneBake,
        now: Instant,
    ) -> bool {
        let mut frames = bake.frames.into_iter();
        let Some(root) = frames.next() else {
            tracing::warn!("background scene bake produced no frames");
            return false;
        };

        self.state.background_scene_key = bake.input.key;
        self.state.background_scene_generated_at = Some(now);
        self.state.background_scene_identity = bake.input.identity;
        self.state.background_scene = Some(
            crate::app::state::GraphicsLayer::new(
                crate::api::schema::PaneGraphicsFormat::Png,
                bake.input.width,
                bake.input.height,
                root,
                crate::api::schema::PaneGraphicsPlacementParams {
                    viewport_col: 0,
                    viewport_row: 0,
                    grid_cols: 0,
                    grid_rows: 0,
                    // Above the cell background, below text — a full backdrop for live panes,
                    // not confined to cells a pane left default
                    // (`data/decisions/2026-08-07-terminal-background-visual-execution-round1.md`,
                    // firstmate home). One step below the effects overlay's own `z`, so a comet
                    // or crater always draws over the ambient scene rather than under it.
                    z: -2,
                },
            )
            .with_animation(crate::app::state::GraphicsAnimation {
                frame_gap_ms: crate::app::background_scene::FRAME_GAP_MS,
                frames: frames.collect(),
            }),
        );
        self.state.background_scene_layout = Some(bake.input.layout);
        // A successful swap restarts phase zero, so it owns a fresh whole-loop floor even if the
        // worker itself took longer than the floor established when it started.
        self.background_scene_next_bake_at = Some(now + BACKGROUND_SCENE_REBAKE_INTERVAL);
        true
    }

    fn clear_background_scene(&mut self) -> bool {
        let had = self.state.background_scene.take().is_some();
        self.state.background_scene_key = 0;
        self.state.background_scene_layout = None;
        self.state.background_scene_identity.clear();
        self.state.background_scene_generated_at = None;
        had
    }

    fn next_background_scene_bake_deadline(&self) -> Option<Instant> {
        if self.background_scene_bake.is_some() {
            return None;
        }
        self.background_scene_deferred_bake_at
    }

    /// Wait for a queued bake only in tests that need the finished pixels. Production never calls
    /// this path; its app/server loop keeps servicing panes until the worker notifies it.
    #[cfg(test)]
    pub(crate) fn settle_background_scene(&mut self, now: Instant) -> bool {
        let mut changed = self.observe_background_scene(now);
        let Some(bake) = self.background_scene_bake.take() else {
            return changed;
        };
        let result = bake
            .result_rx
            .recv_timeout(std::time::Duration::from_secs(60));
        match result {
            Ok(Ok(frames)) => {
                changed |= self.adopt_baked_background_scene(
                    FinishedBackgroundSceneBake {
                        input: bake.input,
                        frames,
                    },
                    now,
                );
            }
            Ok(Err(())) | Err(_) => panic!("background scene bake did not finish"),
        }
        changed
    }

    /// (Re-)generate the background scene's event-driven overlay: in-flight asteroids, fading
    /// craters, travelling comets.
    ///
    /// `has_viewers` is false when no client is rendering the app — a detached server then
    /// forgets every live effect rather than animating something nobody is looking at, mirroring
    /// [`Self::advance_animations`]'s own `Animator::forget_all` gate. Regenerated on every pass
    /// while something is live (unlike the ambient loop above, this is small and
    /// bounding-box-limited — see `src/solar_system.rs`'s own module doc), and left entirely
    /// absent whenever nothing is: fade-clean, state-derived persistence, not an accumulator.
    pub(crate) fn observe_background_effects(&mut self, now: Instant, has_viewers: bool) -> bool {
        if !self.state.background_scene_active() {
            let forgot = self.state.background_effects.forget_all();
            let had = self.state.background_effects_layer.take().is_some();
            let had_legibility = self.state.background_legibility.take().is_some();
            return forgot || had || had_legibility;
        }

        if !has_viewers {
            let forgot = self.state.background_effects.forget_all();
            let had = self.state.background_effects_layer.take().is_some();
            let had_legibility = self.state.background_legibility.take().is_some();
            return forgot || had || had_legibility;
        }

        let Some(layout) = self.state.background_scene_layout.clone() else {
            return false;
        };
        let identity = self.state.background_scene_identity.clone();

        let mut effects_state = std::mem::take(&mut self.state.background_effects);
        crate::app::background_scene::spawn_new_effects(
            &self.state,
            &mut effects_state,
            &identity,
            now,
        );
        let generated_at = self.state.background_scene_generated_at.unwrap_or(now);
        let phase = crate::app::background_scene::phase_at(generated_at, now);
        let live_before = effects_state.is_live();
        let effects = crate::app::background_scene::advance_and_build_effects(
            &mut effects_state,
            &identity,
            Some((&layout, phase)),
            now,
        );
        let live_after = effects_state.is_live();
        self.state.background_effects = effects_state;

        // Resample per-cell text legibility every pass too — not gated on `live_after`, since an
        // ambient body alone (the sun, an orbiting planet) can sit under static text with no
        // transient effect ever spawning. `background_legibility::observe` gates its own heavier
        // resampling work to a coarser cadence internally (see its own doc), so calling it here
        // every tick is cheap on the passes it declines to do anything.
        //
        // Sampled against the same cell the scene under it was rasterised at,
        // not the foreground client's, or the samples would be read out of a
        // grid the layout was never built for.
        let cell = self.state.shared_raster_cell_size();
        // The machine corner is a third surface over the same cells, so the legibility decision
        // for the cells it covers has to be made against it — and only for those cells. Its origin
        // is relative to the scene's own grid, which starts at the screen rect's origin.
        let screen = self.state.screen_rect();
        let corner_rect = self.state.machine_corner_rect();
        let corner = self.state.machine_corner_rgba.as_ref().and_then(|rgba| {
            (corner_rect.width > 0 && corner_rect.height > 0).then(|| {
                crate::solar_system::CornerLayer {
                    rgba,
                    width: u32::from(corner_rect.width) * cell.width_px,
                    height: u32::from(corner_rect.height) * cell.height_px,
                    col: u32::from(corner_rect.x.saturating_sub(screen.x)),
                    row: u32::from(corner_rect.y.saturating_sub(screen.y)),
                }
            })
        });
        let legibility_changed = crate::app::background_legibility::observe(
            &mut self.state.background_legibility,
            &layout,
            phase,
            &effects,
            corner,
            cell.width_px,
            cell.height_px,
            now,
        );

        if !live_after {
            let had = self.state.background_effects_layer.take().is_some();
            return live_before || had || legibility_changed;
        }

        let frame = crate::solar_system::effects_frame_png(&layout, &effects, phase);
        self.state.background_effects_layer = Some(crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Png,
            layout.width(),
            layout.height(),
            frame,
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 0,
                viewport_row: 0,
                grid_cols: 0,
                grid_rows: 0,
                z: -1,
            },
        ));
        true
    }

    /// Move the clock the sidebar renders elapsed times against.
    ///
    /// Deliberately separate from arming the repaint deadline. This runs every
    /// loop iteration so that a frame drawn for any other reason already shows
    /// current ages; re-arming on the same schedule would push the deadline
    /// past `now` on every pass and it would never fire.
    pub(crate) fn refresh_state_age_clock(&mut self, now: Instant) {
        self.state.state_age_now = now;
        self.state.wall_now = std::time::SystemTime::now();
    }

    /// Arm the deadline that forces a repaint when nothing else would.
    ///
    /// Called after the scheduled-task pass has had its chance to fire the
    /// previous deadline, so the new one is always computed from a `now` at or
    /// past it. `has_viewers` is false when no client is rendering, so a
    /// detached server does not wake up to keep a number current that nobody
    /// is reading.
    pub(crate) fn sync_state_age_timer(&mut self, now: Instant, has_viewers: bool) {
        self.state.state_age_now = now;
        self.state.wall_now = std::time::SystemTime::now();
        self.next_state_age_tick = has_viewers
            .then(|| self.state.next_sidebar_state_age_tick(now))
            .flatten();
    }

    /// The same advance, under the frame floor a server without a local
    /// terminal is configured to run at.
    ///
    /// A headless server is drawing for a remote client over a socket, so the
    /// floor is there to trade smoothness for wakes; the behaviours keep their
    /// own periods, which is what stops the animation running at a different
    /// *speed* there rather than merely at a coarser step. The floor comes from
    /// `[advanced] headless_animation_interval_ms` and defaults below every
    /// declared tier, so out of the box it takes nothing away.
    ///
    /// # What the floor actually reaches
    ///
    /// Only [`crate::anim::Engine::next_deadline`], which is one candidate
    /// among many in `next_headless_loop_deadline_with_git_refresh`.
    /// [`crate::anim::Engine::advance`] is never floored, so while any element
    /// is animating `handle_scheduled_tasks_headless` reports a change on every
    /// loop pass, `needs_render` stays true, and `last_render_at +
    /// MIN_RENDER_INTERVAL` is always the smaller deadline. The animation
    /// deadline is therefore never the minimum and the floor is never reached.
    ///
    /// Measured, not reasoned: `tests/frame_floor_lab.rs` runs a real server
    /// and a real client at floors of 16, 200 and 1000 ms and gets 58 fps from
    /// all three, against 0 fps with nothing animating. The hard-coded 200 ms
    /// this setting replaced was inert on a live server for the same reason.
    /// The floor becomes load-bearing only once the loop stops free-running.
    pub(crate) fn advance_headless_animations(&mut self, now: Instant, has_viewers: bool) -> bool {
        let floor = self.state.headless_animation_interval;
        self.state.anim.set_frame_floor(floor);
        self.advance_animations(now, has_viewers)
    }

    /// Clears temporary copied-token highlights, such as after double-click copy.
    pub(crate) fn clear_due_selection_highlight(&mut self, now: Instant) -> bool {
        if self
            .selection_highlight_clear_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return false;
        }

        self.selection_highlight_clear_deadline = None;
        if self
            .state
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_in_progress())
        {
            self.state.clear_selection();
            return true;
        }
        false
    }

    pub(crate) fn sync_agent_metadata_deadline(&mut self) {
        self.agent_metadata_deadline = self.state.next_agent_metadata_expiry();
    }

    pub(crate) fn expire_due_metadata(&mut self, now: Instant) -> bool {
        let Some(deadline) = self
            .agent_metadata_deadline
            .filter(|deadline| now >= *deadline)
        else {
            return false;
        };
        self.expire_metadata_at(deadline, now);
        true
    }

    pub(crate) fn expire_metadata_at(&mut self, deadline: Instant, now: Instant) {
        let previous_toast = self.state.toast.clone();
        for update in self.state.expire_agent_metadata_at(deadline, now) {
            self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
            self.emit_pane_state_update(&update);
        }
        let (panes, workspaces) = self.state.expire_metadata_tokens(now);
        let expired_any_token = !panes.is_empty() || !workspaces.is_empty();
        for (ws_idx, pane_id) in panes {
            self.emit_pane_updated(ws_idx, pane_id);
        }
        for ws_idx in workspaces {
            self.emit_workspace_token_updated(ws_idx);
        }
        if expired_any_token {
            // The session file holds absolute deadlines, so a sweep that is
            // never saved is not incorrect — a restore would drop the same
            // tokens. Saving anyway keeps the file honest about what is live.
            self.state.mark_session_dirty();
        }
        self.sync_agent_metadata_deadline();
    }

    pub(crate) fn tick_selection_autoscroll(&mut self, now: Instant) {
        let Some(autoscroll) = self.state.selection_autoscroll.clone() else {
            // Self-heal: state cleared but deadline leaked
            self.selection_autoscroll_deadline = None;
            return;
        };

        // Selection must still be in progress for autoscroll to continue
        let Some(pane_id) = self.state.selection.as_ref().map(|s| s.pane_id) else {
            self.stop_selection_autoscroll();
            return;
        };
        if !self
            .state
            .selection
            .as_ref()
            .is_some_and(|s| s.is_dragging())
        {
            self.stop_selection_autoscroll();
            return;
        }

        // Rect-change detection: if inner_rect changed since drag, stop
        let current_rect = self
            .state
            .pane_info_by_id(pane_id)
            .map(|info| info.inner_rect);
        if current_rect != Some(autoscroll.inner_rect) {
            self.stop_selection_autoscroll();
            return;
        }

        // Scrollback boundary detection via ScrollMetrics — fail-closed if unavailable
        let Some(metrics) = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)
        else {
            self.stop_selection_autoscroll();
            return;
        };
        match autoscroll.direction {
            crate::app::state::SelectionAutoscrollDirection::Up => {
                let at_top = metrics.offset_from_bottom >= metrics.max_offset_from_bottom;
                if at_top {
                    self.stop_selection_autoscroll();
                    return;
                }
                self.state
                    .scroll_pane_up(&self.terminal_runtimes, pane_id, 1);
            }
            crate::app::state::SelectionAutoscrollDirection::Down => {
                let at_bottom = metrics.offset_from_bottom == 0;
                if at_bottom {
                    self.stop_selection_autoscroll();
                    return;
                }
                self.state
                    .scroll_pane_down(&self.terminal_runtimes, pane_id, 1);
            }
        }

        // Extend selection cursor to last known mouse position
        self.state.update_selection_cursor(
            &self.terminal_runtimes,
            pane_id,
            autoscroll.last_mouse_screen_col,
            autoscroll.last_mouse_screen_row,
        );

        // Reschedule
        self.selection_autoscroll_deadline = Some(now + SELECTION_AUTOSCROLL_INTERVAL);
    }

    pub(crate) fn stop_selection_autoscroll(&mut self) {
        self.state.stop_selection_autoscroll_state();
        self.selection_autoscroll_deadline = None;
    }

    pub(crate) fn can_render_now(&self, now: Instant) -> bool {
        match self.last_render_at {
            Some(last_render_at) => now.duration_since(last_render_at) >= MIN_RENDER_INTERVAL,
            None => true,
        }
    }

    pub(crate) fn run_auto_update_check(&mut self) {
        if !background_update_check_enabled(self.no_session, self.update_version_check_enabled) {
            self.next_auto_update_check = None;
            return;
        }

        self.next_auto_update_check = self
            .state
            .update_available
            .is_none()
            .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        if self.state.update_available.is_some() {
            return;
        }

        let update_tx = self.event_tx.clone();
        std::thread::spawn(move || crate::update::auto_update(update_tx));
    }

    pub(crate) fn run_agent_manifest_update_check(&mut self) {
        if !background_update_check_enabled(self.no_session, self.update_manifest_check_enabled) {
            self.next_agent_manifest_update_check = None;
            return;
        }

        self.next_agent_manifest_update_check = Some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        let manifest_update_tx = self.event_tx.clone();
        std::thread::spawn(move || crate::detect::manifest_update::auto_update(manifest_update_tx));
    }

    pub(crate) fn next_loop_deadline(&self, now: Instant, needs_render: bool) -> Option<Instant> {
        self.next_loop_deadline_with_resize_poll(now, needs_render, true, true)
    }

    pub(crate) fn next_headless_loop_deadline_with_git_refresh(
        &self,
        now: Instant,
        needs_render: bool,
        include_git_refresh: bool,
    ) -> Option<Instant> {
        self.next_loop_deadline_with_resize_poll(now, needs_render, false, include_git_refresh)
    }

    fn next_loop_deadline_with_resize_poll(
        &self,
        now: Instant,
        needs_render: bool,
        include_resize_poll: bool,
        include_git_refresh: bool,
    ) -> Option<Instant> {
        let render_deadline = if needs_render {
            self.last_render_at
                .map(|last_render_at| last_render_at + MIN_RENDER_INTERVAL)
                .filter(|deadline| *deadline > now)
        } else {
            None
        };

        [
            include_resize_poll.then_some(self.next_resize_poll),
            self.config_diagnostic_deadline,
            self.toast_deadline,
            self.state.next_pending_agent_notification_deadline(),
            self.state.next_managed_agent_deadline(),
            self.copy_feedback_deadline,
            self.state.anim.next_deadline(now),
            self.state.pane_resize_reflow.next_deadline(now),
            self.state.next_tree_view_commit_deadline(),
            self.next_state_age_tick,
            self.next_activity_sample,
            self.state.relation_signals.next_deadline(),
            include_git_refresh
                .then(|| self.git_refresh_deadline())
                .flatten(),
            include_git_refresh
                .then(|| self.pull_request_refresh_deadline())
                .flatten(),
            self.next_auto_update_check,
            self.next_agent_manifest_update_check,
            self.agent_metadata_deadline,
            self.pending_agent_resume_deadline,
            self.session_save_deadline,
            self.selection_autoscroll_deadline,
            self.selection_highlight_clear_deadline,
            self.next_background_scene_bake_deadline(),
            render_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn drain_internal_events(&mut self) -> bool {
        self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT)
            .1
    }

    pub(crate) fn drain_all_internal_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            let (had_event, batch_changed) =
                self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT);
            changed |= batch_changed;
            if !had_event {
                break;
            }
        }
        changed
    }

    fn drain_internal_events_up_to(&mut self, limit: usize) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_prefix_sync(ev);
        }
        (had_event, changed)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::app::state;
    use crate::app::ANIMATION_INTERVAL;
    use crate::workspace::Workspace;

    #[test]
    fn interrupted_custom_command_wait_keeps_child_for_retry() {
        let interrupted = std::io::Error::new(std::io::ErrorKind::Interrupted, "test interrupt");

        assert!(retain_custom_command_after_wait(42, Err(interrupted)));
    }

    fn pulse_space_rows() -> Vec<Vec<crate::config::SpaceSidebarToken>> {
        let config: crate::config::Config = toml::from_str(
            "[ui.sidebar.spaces]\nrows = [[{ token = \"workspace\", emphasis = \"pulse\" }]]\n",
        )
        .expect("pulse space config");
        config.ui.sidebar.spaces.rows
    }

    /// Where the first workspace's row has reached in its own loop.
    fn row_position(app: &super::super::App) -> f32 {
        let id = crate::anim::ElementId::workspace_row(&app.state.workspaces[0].id);
        app.state
            .anim
            .frame(&id, None)
            .expect("the row is tracked")
            .progress
    }

    /// The switch has a deadline of its own, and the loop has to arm it: a
    /// panel mid-dissolve with nothing else animating would otherwise park with
    /// the outgoing view half gone and the incoming one never drawn.
    #[test]
    fn a_view_switch_arms_the_loop_with_nothing_else_animating() {
        let (mut app, _) = test_app_with_pane();
        let now = Instant::now();
        assert_eq!(app.state.anim.next_deadline(now), None);

        assert!(app.state.select_tree_root(
            crate::app::tree_view::TreeRoot::Node("2nd-a".to_string()),
            now
        ));
        let commit = app
            .state
            .next_tree_view_commit_deadline()
            .expect("a switch is in flight");
        assert!(app
            .next_loop_deadline(now, false)
            .is_some_and(|deadline| deadline <= commit));

        assert!(app.handle_scheduled_tasks(commit, false));
        assert_eq!(
            app.state.tree_root,
            crate::app::tree_view::TreeRoot::Node("2nd-a".to_string())
        );
        assert_eq!(app.state.next_tree_view_commit_deadline(), None);
    }

    #[test]
    fn calm_sidebar_never_arms_the_animation_clock() {
        let (mut app, _) = test_app_with_pane();
        let now = Instant::now();

        assert!(!app.handle_scheduled_tasks(now, false));
        assert_eq!(
            app.state.anim.next_deadline(now),
            None,
            "an unconfigured Herdr must not wake up to animate"
        );
        assert!(app.state.anim.is_empty(), "and must track nothing at all");
        assert_eq!(
            app.next_loop_deadline(now, false),
            Some(app.next_resize_poll)
        );
    }

    /// A trunk segment mounts, settles, and retracts on its own clock — driven
    /// by the same `row_enter`/`row_exit` config a row's own arrival reads,
    /// but tracked as its own element rather than riding the row's.
    ///
    /// `fleet_app` gives `2ndmate-left` a sibling, `2ndmate-right`, so the
    /// ancestor column beside `2ndmate-left`'s own two workers is still open —
    /// one segment per worker row, both at that same level, since the column
    /// only closes once every one of `2ndmate-left`'s rows has been passed.
    /// Unpublishing `2ndmate-right`'s owner closes that gap without touching
    /// either worker's own row at all, which is the case this test exists to
    /// pin: a segment's life is not a side effect of its row's.
    #[test]
    fn a_trunk_segment_mounts_settles_and_retracts_on_its_own_clock() {
        let (mut app, _left_second, _right_only) = fleet_app("wipe");
        app.state.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::Wipe;
        app.state.sidebar_animation.row_enter_ms = 200;
        let now = Instant::now();

        let members = crate::ui::sidebar_trunk_segment_members(&app.state);
        assert_eq!(
            members.len(),
            2,
            "both of 2ndmate-left's worker rows pass the still-open column \
             beside 2ndmate-right: {members:?}"
        );
        let segment_id = members[0].0.clone();

        assert!(app.handle_scheduled_tasks(now, false));
        assert_eq!(
            app.state
                .anim
                .frame(&segment_id, None)
                .expect("the segment is tracked from its first pass")
                .phase,
            crate::anim::Phase::Mount,
        );

        let settled = now + Duration::from_millis(250);
        assert!(app.handle_scheduled_tasks(settled, false));
        assert_eq!(
            app.state
                .anim
                .frame(&segment_id, None)
                .expect("still tracked")
                .phase,
            crate::anim::Phase::Idle,
            "past its mount duration the segment has settled, same as any row",
        );

        // Closing the gap: `2ndmate-right` no longer names the first mate as
        // its owner, so `2ndmate-left` becomes the only — and therefore last
        // — child, and the ancestor column `left-worker-1` stood beside closes.
        app.state.workspaces[2].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), None)]),
            None,
            settled,
        );
        assert!(app.handle_scheduled_tasks(settled, false));
        assert_eq!(
            app.state
                .anim
                .frame(&segment_id, None)
                .expect("still drawable mid-retract")
                .phase,
            crate::anim::Phase::Dismount,
        );

        let gone = settled + Duration::from_millis(250);
        app.handle_scheduled_tasks(gone, false);
        assert!(
            app.state.anim.frame(&segment_id, None).is_none(),
            "the segment is gone once its retract finishes"
        );
    }

    /// The failure spider climbs, rests, and retreats on a fleet with nothing
    /// else configured to animate at all — the core requirement it exists
    /// for, since it is a failure signal rather than a decorative toggle.
    #[test]
    fn a_failing_card_mounts_a_spider_with_nothing_else_configured() {
        let (mut app, _pane_id) = test_app_with_pane();
        let now = Instant::now();

        assert!(!app.advance_animations(now, true));
        assert!(
            app.state.anim.is_empty(),
            "a healthy fleet tracks nothing at all"
        );

        app.state.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([(
                "lifecycle".to_string(),
                Some("failed".to_string()),
            )]),
            None,
            now,
        );
        let row = crate::anim::ElementId::failure_spider(crate::anim::CardRow::Space(
            app.state.workspaces[0].id.clone(),
        ));

        assert!(app.advance_animations(now, true));
        assert_eq!(
            app.state
                .anim
                .frame(&row, None)
                .expect("mounted the frame the card started failing")
                .phase,
            crate::anim::Phase::Mount,
        );

        let settled =
            now + crate::anim::behaviour::FAILURE_SPIDER_CLIMB_PERIOD + Duration::from_millis(50);
        assert!(app.advance_animations(settled, true));
        assert_eq!(
            app.state
                .anim
                .frame(&row, None)
                .expect("still tracked")
                .phase,
            crate::anim::Phase::Idle,
            "past its climb duration the spider has arrived and rests",
        );

        // Clearing the failure: the card stops being failing, but the spider
        // has to retreat rather than vanish on the spot.
        app.state.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("lifecycle".to_string(), None)]),
            None,
            settled,
        );
        assert!(app.advance_animations(settled, true));
        assert_eq!(
            app.state
                .anim
                .frame(&row, None)
                .expect("still drawable mid-retreat")
                .phase,
            crate::anim::Phase::Dismount,
        );

        let gone = settled
            + crate::anim::behaviour::FAILURE_SPIDER_CLIMB_PERIOD
            + Duration::from_millis(50);
        app.advance_animations(gone, true);
        assert!(
            app.state.anim.frame(&row, None).is_none(),
            "gone once the retreat finishes"
        );
    }

    /// A published defect severity marks a card the fleet is still working on.
    ///
    /// The case the two channels exist for — *running, but in serious trouble*
    /// — and the one detection can never reach on its own: a pane happily
    /// producing output looks exactly like a pane happily producing output
    /// whether or not somebody has an open bug against what it produced.
    #[test]
    fn a_published_defect_marks_a_card_that_is_not_failing() {
        let (mut app, _pane_id) = test_app_with_pane();
        let now = Instant::now();

        assert!(!app.advance_animations(now, true));
        let row = crate::anim::ElementId::failure_spider(crate::anim::CardRow::Space(
            app.state.workspaces[0].id.clone(),
        ));

        app.state.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([
                ("lifecycle".to_string(), Some("running".to_string())),
                ("sev".to_string(), Some("S2".to_string())),
            ]),
            None,
            now,
        );
        assert!(app.advance_animations(now, true));
        assert_eq!(
            app.state
                .anim
                .frame(&row, None)
                .expect("a running card with an open defect is marked")
                .phase,
            crate::anim::Phase::Mount,
        );
    }

    /// `sev=-` is the fleet stating the defect is closed, and it unmounts the
    /// marker even from a card detection still reads as failed.
    ///
    /// Publication is the ceiling: whether a bug is still open is a fact only
    /// the fleet holds, and a failed last task with a closed defect is a real
    /// state rather than a contradiction.
    #[test]
    fn a_closed_defect_retreats_the_marker_off_a_failed_card() {
        let (mut app, _pane_id) = test_app_with_pane();
        let now = Instant::now();

        assert!(!app.advance_animations(now, true));
        let row = crate::anim::ElementId::failure_spider(crate::anim::CardRow::Space(
            app.state.workspaces[0].id.clone(),
        ));

        app.state.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([(
                "lifecycle".to_string(),
                Some("failed".to_string()),
            )]),
            None,
            now,
        );
        assert!(app.advance_animations(now, true));
        assert!(
            app.state.anim.frame(&row, None).is_some(),
            "an unrated failure is still marked, the way it always was"
        );

        app.state.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("sev".to_string(), Some("-".to_string()))]),
            None,
            now,
        );
        assert!(app.advance_animations(now, true));
        assert_eq!(
            app.state
                .anim
                .frame(&row, None)
                .expect("still drawable mid-retreat")
                .phase,
            crate::anim::Phase::Dismount,
            "a closed defect retreats the marker rather than leaving it resting",
        );
    }

    #[test]
    fn a_pulse_token_arms_the_clock_and_moves_the_row() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        let now = Instant::now();

        // The first pass is what brings the row into existence, so it reports a
        // change of its own; the clock it arms is the cheap interval, which is
        // exactly what the hand-rolled pulse cost before the engine.
        assert!(app.handle_scheduled_tasks(now, false));
        let armed = app
            .state
            .anim
            .next_deadline(now)
            .expect("a pulse arms the clock");
        assert_eq!(armed, now + ANIMATION_INTERVAL);

        // Nothing to do before the deadline.
        let before = row_position(&app);
        assert!(!app.handle_scheduled_tasks(now, false));
        assert_eq!(row_position(&app), before);

        // At the deadline the row has moved and the frame is marked dirty.
        assert!(app.handle_scheduled_tasks(armed, false));
        assert_ne!(row_position(&app), before);
        assert_eq!(
            app.state.anim.next_deadline(armed),
            Some(armed + ANIMATION_INTERVAL),
            "the clock reschedules itself"
        );
    }

    fn state_age_space_rows() -> Vec<Vec<crate::config::SpaceSidebarToken>> {
        let config: crate::config::Config =
            toml::from_str("[ui.sidebar.spaces]\nrows = [[\"state_text\", \"state_age\"]]\n")
                .expect("state_age space config");
        config.ui.sidebar.spaces.rows
    }

    /// Give the app's one terminal a state stamped `held_for` ago.
    fn stamp_state_age(app: &mut super::super::App, now: Instant, held_for: Duration) {
        app.state.ensure_test_terminals();
        let terminal = app
            .state
            .terminals
            .values_mut()
            .next()
            .expect("test app has a terminal");
        terminal.last_agent_state_change_at = Some(now - held_for);
    }

    #[test]
    fn a_sidebar_with_no_state_age_token_never_arms_the_repaint_clock() {
        let (mut app, _) = test_app_with_pane();
        let now = Instant::now();
        stamp_state_age(&mut app, now, Duration::from_secs(5));

        app.sync_state_age_timer(now, true);

        assert_eq!(app.next_state_age_tick, None);
        assert!(!app.handle_scheduled_tasks(now, false));
    }

    /// The cost argument for the feature: the wake-up interval follows the
    /// token's resolution, so an old state is nearly free to keep current.
    #[test]
    fn the_repaint_interval_coarsens_as_the_state_ages() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = state_age_space_rows();
        let now = Instant::now();

        stamp_state_age(&mut app, now, Duration::from_secs(5));
        app.sync_state_age_timer(now, true);
        let seconds_old = app
            .next_state_age_tick
            .expect("a fresh state arms the clock");
        assert_eq!(seconds_old, now + Duration::from_secs(1));

        stamp_state_age(&mut app, now, Duration::from_secs(2 * 60 * 60));
        app.sync_state_age_timer(now, true);
        assert_eq!(
            app.next_state_age_tick,
            Some(now + Duration::from_secs(60 * 60)),
            "a two-hour-old state costs one wake-up an hour, not 3600"
        );
    }

    #[test]
    fn reaching_the_repaint_deadline_asks_for_a_redraw() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = state_age_space_rows();
        let now = Instant::now();
        stamp_state_age(&mut app, now, Duration::from_secs(5));

        app.sync_state_age_timer(now, true);
        let armed = app.next_state_age_tick.expect("arms");
        assert!(app
            .next_loop_deadline(now, false)
            .is_some_and(|d| d <= armed));

        // Before the deadline the drawn text is still correct.
        assert!(!app.handle_scheduled_tasks(now, false));
        // At it, the age has changed and the frame is dirty.
        assert!(app.handle_scheduled_tasks(armed, false));
    }

    #[test]
    fn a_state_with_no_timestamp_leaves_the_repaint_clock_unarmed() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = state_age_space_rows();
        app.state.ensure_test_terminals();
        let now = Instant::now();

        // A configured token with nothing to draw must not cost a wake-up.
        app.sync_state_age_timer(now, true);
        assert_eq!(app.next_state_age_tick, None);
    }

    #[test]
    fn a_detached_server_does_not_repaint_ages_nobody_is_reading() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = state_age_space_rows();
        let now = Instant::now();
        stamp_state_age(&mut app, now, Duration::from_secs(5));

        app.sync_state_age_timer(now, false);
        assert_eq!(app.next_state_age_tick, None);

        app.sync_state_age_timer(now, true);
        assert!(app.next_state_age_tick.is_some());
    }

    #[test]
    fn collapsing_the_sidebar_disarms_the_repaint_clock() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = state_age_space_rows();
        let now = Instant::now();
        stamp_state_age(&mut app, now, Duration::from_secs(5));

        app.sync_state_age_timer(now, true);
        assert!(app.next_state_age_tick.is_some());

        app.state.sidebar_collapsed = true;
        app.sync_state_age_timer(now, true);
        assert_eq!(app.next_state_age_tick, None);
    }

    /// Arms a relation signal on the app's only workspace and reports the stop
    /// length, so a test can walk the clock the way the loop does.
    fn arm_relation_signal(app: &mut super::super::App, now: Instant) -> Duration {
        let carrier = app.state.workspaces[0].id.clone();
        app.state
            .relation_signals
            .accept(
                "firstmate",
                None,
                crate::app::relation_signal::RelationSignalKind::Transfer,
                crate::app::relation_signal::CarrierId::Workspace(carrier),
                None,
                now,
            )
            .expect("a fresh row always accepts its first signal");
        crate::app::relation_signal::DEFAULT_SIGNAL_TTL
            / u32::from(crate::app::relation_signal::SIGNAL_STOPS)
    }

    fn lay_out_the_only_workspace(app: &mut super::super::App) {
        app.state.view.workspace_card_areas = vec![state::WorkspaceCardArea {
            ws_idx: 0,
            rect: ratatui::layout::Rect::new(0, 0, 30, 2),
            worktree_child: true,
            entry_idx: 0,
            agent: None,
            card_frame: None,
            motion_cells: (0, 0),
            arriving: false,
            drawn_card: true,
        }];
    }

    #[test]
    fn a_signal_on_a_laid_out_row_repaints_once_per_stop_and_once_more_to_clear() {
        let (mut app, _) = test_app_with_pane();
        let now = Instant::now();
        lay_out_the_only_workspace(&mut app);
        let step = arm_relation_signal(&mut app, now);

        assert_eq!(
            app.next_loop_deadline(now, false)
                .map(|deadline| deadline <= now + step),
            Some(true),
            "the loop has to wake for the next stop, not for the resize poll"
        );

        let mut repaints = 0;
        for stop in 1..=u32::from(crate::app::relation_signal::SIGNAL_STOPS) {
            if app.handle_scheduled_tasks(now + step * stop + Duration::from_millis(1), false) {
                repaints += 1;
            }
        }
        // Three moves along the route plus the frame that puts the row back the
        // way it was. No animation clock is running, and nothing else in this
        // app is dirty, so every one of those frames is the signal's own.
        assert_eq!(
            repaints,
            u32::from(crate::app::relation_signal::SIGNAL_STOPS)
        );
        assert!(app.state.relation_signals.is_empty());
        assert!(!app.handle_scheduled_tasks(
            now + crate::app::relation_signal::DEFAULT_SIGNAL_TTL * 2,
            false
        ));
    }

    #[test]
    fn a_signal_on_a_row_that_was_never_laid_out_expires_without_a_single_repaint() {
        let (mut app, _) = test_app_with_pane();
        let now = Instant::now();
        // No `workspace_card_areas`: what a collapsed sidebar, a mobile layout,
        // a collapsed parent group, and a row scrolled past the end of the list
        // all look like from here.
        assert!(app.state.view.workspace_card_areas.is_empty());
        let step = arm_relation_signal(&mut app, now);

        for stop in 1..=u32::from(crate::app::relation_signal::SIGNAL_STOPS) {
            assert!(
                !app.handle_scheduled_tasks(now + step * stop + Duration::from_millis(1), false),
                "a signal nobody can see must never cost a frame"
            );
        }
        assert!(
            app.state.relation_signals.is_empty(),
            "and it still has to die on schedule, or a row could be stranded \
             mid-travel the moment it scrolls back into view"
        );
    }

    #[test]
    fn collapsing_the_sidebar_disarms_the_animation_clock() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        let now = Instant::now();

        app.handle_scheduled_tasks(now, false);
        assert!(app.state.anim.next_deadline(now).is_some());

        // The collapsed sidebar draws its own compact layout with no configured
        // token rows, so there is nothing left to animate.
        app.state.sidebar_collapsed = true;
        app.handle_scheduled_tasks(now, false);
        assert_eq!(app.state.anim.next_deadline(now), None);
        assert!(app.state.anim.is_empty());
    }

    #[test]
    fn detached_server_with_no_viewers_does_not_animate() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        let now = Instant::now();

        app.advance_headless_animations(now, false);
        assert_eq!(app.state.anim.next_deadline(now), None);

        app.advance_headless_animations(now, true);
        assert!(app.state.anim.next_deadline(now).is_some());

        // And a client leaving puts it straight back to sleep rather than
        // leaving a row mid-loop for nobody.
        app.advance_headless_animations(now, false);
        assert_eq!(app.state.anim.next_deadline(now), None);
        assert!(app.state.anim.is_empty());
    }

    /// The whole lifecycle in one pass: an alert going live mounts its element,
    /// the alert clearing retires it, and nothing has to remember to tear it
    /// down.
    #[test]
    fn a_signal_going_live_mounts_an_element_and_clearing_retires_it() {
        use crate::app::fleet_signals::FleetSignal;

        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_notifications.enabled = true;
        let now = Instant::now();

        // A quiet fleet: the bar is drawn, but nothing in it is animating, so
        // the engine holds nothing and the loop arms no deadline.
        app.advance_animations(now, true);
        assert!(app.state.anim.is_empty());
        assert_eq!(app.state.anim.next_deadline(now), None);

        app.state.workspaces[0].cached_git_ahead_behind = Some((1, 0));
        app.advance_animations(now, true);
        assert!(
            app.state
                .anim
                .frame(&FleetSignal::Push.element_id(), None)
                .is_some(),
            "unpushed work did not bring its own element into existence"
        );
        assert!(
            app.state
                .anim
                .frame(&FleetSignal::Ask.element_id(), None)
                .is_none(),
            "a quiet signal was given an element"
        );

        // Pushing the work clears the alert. The element leaves on its own
        // because it simply stopped being in the published membership set.
        app.state.workspaces[0].cached_git_ahead_behind = Some((0, 0));
        app.advance_animations(now + std::time::Duration::from_secs(2), true);
        assert!(app.state.anim.is_empty());
    }

    /// The two families share one engine and must not evict each other.
    #[test]
    fn signal_elements_and_row_elements_do_not_retire_one_another() {
        use crate::app::fleet_signals::FleetSignal;

        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        app.state.sidebar_notifications.enabled = true;
        app.state.workspaces[0].cached_git_ahead_behind = Some((1, 0));
        let now = Instant::now();

        app.advance_animations(now, true);
        let row = crate::anim::ElementId::workspace_row(&app.state.workspaces[0].id);
        assert!(app.state.anim.frame(&row, None).is_some());
        assert!(app
            .state
            .anim
            .frame(&FleetSignal::Push.element_id(), None)
            .is_some());

        // Switching the bar off must retire its slots and leave the row alone.
        app.state.sidebar_notifications.enabled = false;
        app.advance_animations(now + std::time::Duration::from_secs(2), true);
        assert!(
            app.state.anim.frame(&row, None).is_some(),
            "the sidebar row was retired by a change to a different family"
        );
        assert!(app
            .state
            .anim
            .frame(&FleetSignal::Push.element_id(), None)
            .is_none());
    }

    #[test]
    fn the_headless_clock_runs_at_the_behaviours_own_tier_by_default() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        let now = Instant::now();

        app.advance_headless_animations(now, true);

        assert_eq!(
            app.state.anim.next_deadline(now),
            Some(now + ANIMATION_INTERVAL),
            "the default floor is finer than every declared tier, so a pulse on \
             a headless server is clocked by the pulse rather than by the floor"
        );
        assert!(
            app.state.headless_animation_interval <= Duration::from_millis(16),
            "60 fps is the floor the default has to clear, not a target to approach"
        );
    }

    #[test]
    fn the_headless_clock_honours_a_configured_floor() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        // Coarser than the pulse's own 100 ms tier, so the floor is what the
        // deadline can only be coming from.
        let floor = ANIMATION_INTERVAL * 3;
        app.state.headless_animation_interval = floor;
        let now = Instant::now();

        app.advance_headless_animations(now, true);

        assert_eq!(app.state.anim.next_deadline(now), Some(now + floor));
    }

    #[test]
    fn a_configured_floor_can_only_coarsen_a_behaviour_never_sharpen_it() {
        let (mut app, _) = test_app_with_pane();
        app.state.sidebar_spaces.rows = pulse_space_rows();
        // Finer than the pulse asked for. The floor must not promote it.
        app.state.headless_animation_interval = Duration::from_millis(1);
        let now = Instant::now();

        app.advance_headless_animations(now, true);

        assert_eq!(
            app.state.anim.next_deadline(now),
            Some(now + ANIMATION_INTERVAL)
        );
    }

    fn test_app_with_pane() -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        (app, pane_id)
    }

    /// A tray with graphics available, its badges reachable by the app loop.
    fn tray_app() -> super::super::App {
        let (mut app, _) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.sidebar_signal_tray.enabled = true;
        app.state.kitty_graphics_enabled = true;
        // The other half of `AppState::host_paints_pixel_surfaces`: without the
        // host's answer to the capability probe this is a tray that draws its
        // character marks, and none of the artwork below it is built.
        app.state.kitty_graphics_capability_confirmed = true;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 30, 40);
        app
    }

    /// A host that never answered the capability probe draws the marks, not a
    /// hole.
    ///
    /// The gap PR #101's own body named and left: "the character fallbacks are
    /// gated on different predicates than the pixels ... so when the two
    /// disagree a surface renders as a **hole** rather than degrading to text."
    /// This is that disagreement, and it is not an edge case — it is the
    /// permanent state of every terminal that does not speak the Kitty Graphics
    /// Protocol, because such a terminal never answers
    /// `query_kitty_graphics_capability` and so never confirms.
    ///
    /// Before the fix: `refresh_signal_tray_graphics` asked only
    /// `kitty_graphics_enabled`, rasterised the badges, and
    /// `tray::artwork_covers_grid` saw artwork and stood the eight marks down —
    /// while both delivery gates (`server::headless`'s per-client pass and
    /// `App::run`'s own paint) required `kitty_graphics_capability_confirmed`
    /// too and encoded nothing at all. Eight empty slots under a live header.
    ///
    /// Asserted end to end rather than on the predicate, because the predicate
    /// agreeing with itself is exactly what the bug looked like: the app loop
    /// runs, then the real renderer draws, then the delivery conjunction is
    /// evaluated as the two gates spell it.
    #[test]
    fn a_host_that_never_confirmed_graphics_draws_the_marks_and_not_a_hole() {
        use crate::app::fleet_signals::FleetSignal;
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = tray_app();
        app.state.kitty_graphics_capability_confirmed = false;
        assert!(
            app.state.kitty_graphics_enabled && app.state.host_cell_size.is_known(),
            "the opt-in and the cell are what make this the interesting state"
        );

        app.observe_signal_tray(std::time::Instant::now(), false);

        let area = app.state.view.sidebar_rect;
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("backend");
        terminal
            .draw(|frame| {
                crate::ui::sidebar::tray::render(
                    &app.state,
                    frame,
                    crate::ui::sidebar::sidebar_content_rect(area),
                )
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let marks = FleetSignal::ALL
            .into_iter()
            .filter(|signal| {
                (0..area.height)
                    .any(|y| (0..area.width).any(|x| buffer[(x, y)].symbol() == signal.mark()))
            })
            .count();

        // The delivery conjunction, spelled as `server::headless` and
        // `App::run` spell it. Both now read the same predicate the fallback
        // does, so this is the fact the marks were suppressed for.
        let pixels_delivered =
            app.state.host_paints_pixel_surfaces() && app.state.host_cell_size.is_known();
        assert!(
            !pixels_delivered,
            "no client can be sent graphics without a confirmed capability, so \
             this fixture is not the divergent state"
        );
        assert!(
            app.state.signal_tray_graphics.is_none(),
            "artwork was rasterised for a host no delivery gate will send it to"
        );
        assert_eq!(
            marks,
            FleetSignal::COUNT,
            "the tray drew {marks} of {} marks with no pixels coming: a blank \
             hole where the fallback belongs",
            FleetSignal::COUNT
        );

        // And the moment the probe is answered the tray changes sides, so this
        // is a fallback and not a feature switch.
        app.state.kitty_graphics_capability_confirmed = true;
        app.observe_signal_tray(std::time::Instant::now(), false);
        assert!(
            app.state.signal_tray_graphics.is_some(),
            "a host that confirmed the capability was left on the marks"
        );
    }

    /// A fleet whose cards are drawn as pixels, so the card animations have
    /// something to happen to.
    fn card_app() -> super::super::App {
        let (mut app, _) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.sidebar_card_shapes = true;
        app.state.kitty_graphics_enabled = true;
        app.state.kitty_graphics_capability_confirmed = true;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 40, 40);
        app
    }

    fn particle_field_app() -> super::super::App {
        let (mut app, _) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.kitty_graphics_enabled = true;
        app.state.sidebar_particle_field_enabled = true;
        // An opaque ambient wash is only ever handed to a host that draws one
        // where it was placed — see `HostTerminalKind::draws_ambient_wash`.
        app.state.host_terminal_kind = crate::kitty_graphics::HostTerminalKind::Kitty;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 24, 30);
        app
    }

    /// A fleet whose whole-terminal background scene has a screen to cover and
    /// a host willing to draw it under the text.
    fn background_scene_app() -> super::super::App {
        let (mut app, _) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.kitty_graphics_enabled = true;
        app.state.persistent_background_enabled = true;
        app.state.host_terminal_kind = crate::kitty_graphics::HostTerminalKind::Kitty;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 40, 30);
        app.state.view.terminal_area = ratatui::layout::Rect::new(40, 0, 80, 30);
        app
    }

    /// The scene is a full-surface, fully opaque image whose entire safety is
    /// the host honouring `z=-2` — "above the cell background, below text". On
    /// a host that draws it anyway at the top of the stack the same bytes
    /// erase every glyph on screen, which is the failure this gate exists to
    /// make unreachable rather than to apologise for afterwards.
    #[test]
    // The refused set is down to one member since Rio was allowed on the
    // patched build, but it is still a *set* — the next unmeasured terminal
    // joins this array rather than restructuring the test — so the loop stays.
    #[allow(clippy::single_element_loop)]
    fn the_background_scene_is_refused_on_a_host_that_does_not_draw_an_ambient_wash() {
        // One kind rather than the list this used to walk: Rio moved to the
        // drawing side of the gate when it was measured (#127), leaving
        // `Other` — every terminal nobody has measured — as the whole of the
        // refusal, which is the case that actually has to hold.
        let kind = crate::kitty_graphics::HostTerminalKind::Other;
        let mut app = background_scene_app();
        app.state.host_terminal_kind = kind;
        let now = Instant::now();
        assert!(
            !app.observe_background_scene(now),
            "{kind:?} was handed a scene it has not been measured to draw below text"
        );
        assert!(app.state.background_scene.is_none());
        assert!(app.state.background_scene_layout.is_none());
    }

    /// The other half of the same gate: the terminal this was designed and
    /// measured against still gets the scene, in the band it was specified in.
    #[test]
    fn the_background_scene_is_generated_below_text_on_a_host_that_draws_it() {
        let mut app = background_scene_app();
        assert!(app.settle_background_scene(Instant::now()));
        let layer = app
            .state
            .background_scene
            .as_ref()
            .expect("kitty gets the scene");
        assert!(
            layer.render.z < 0,
            "the scene must be placed in the below-text band, not over the UI"
        );
        assert!(
            layer
                .animation
                .as_ref()
                .is_some_and(|a| !a.frames.is_empty()),
            "the scene must carry its loop, not be a single frozen frame"
        );
    }

    #[test]
    fn the_background_scene_bake_is_submitted_without_blocking_for_frames() {
        let mut app = background_scene_app();
        let now = Instant::now();

        assert!(
            !app.observe_background_scene(now),
            "submitting work must not claim that pixels already changed"
        );
        assert!(app.background_scene_bake.is_some());
        assert!(app.state.background_scene.is_none());
        assert!(app.state.background_scene_layout.is_none());
    }

    #[test]
    fn a_changed_scene_waits_one_loop_without_arming_an_idle_spin() {
        let mut app = background_scene_app();
        let now = Instant::now();
        assert!(app.settle_background_scene(now));
        assert_eq!(app.next_background_scene_bake_deadline(), None);

        app.state.workspaces[0].custom_name = Some("changed-scene-key".to_string());
        let before_floor = now + std::time::Duration::from_millis(1);
        assert!(!app.observe_background_scene(before_floor));
        assert!(app.background_scene_bake.is_none());
        assert_eq!(
            app.next_background_scene_bake_deadline(),
            Some(now + BACKGROUND_SCENE_REBAKE_INTERVAL)
        );

        assert!(!app.observe_background_scene(now + BACKGROUND_SCENE_REBAKE_INTERVAL));
        assert!(app.background_scene_bake.is_some());
        assert_eq!(app.next_background_scene_bake_deadline(), None);
    }

    #[test]
    fn disabling_a_scene_does_not_orphan_its_worker_and_start_another() {
        let mut app = background_scene_app();
        let now = Instant::now();
        assert!(!app.observe_background_scene(now));
        assert!(app.background_scene_bake.is_some());

        app.state.persistent_background_enabled = false;
        assert!(!app.observe_background_scene(now));
        assert!(
            app.background_scene_bake.is_some(),
            "the owned worker must remain tracked until its result can be discarded"
        );

        app.state.persistent_background_enabled = true;
        assert!(!app.observe_background_scene(now));
        assert!(app.background_scene_bake.is_some());
    }

    /// A client attaching from a different terminal replaces
    /// `host_terminal_kind` (`sync_foreground_client_state`), so a scene
    /// generated for one host has to be retired when the next one cannot draw
    /// it — otherwise the already-uploaded image outlives the fact that made
    /// it safe.
    #[test]
    fn a_generated_scene_is_retired_when_the_host_stops_being_one_that_draws_it() {
        let mut app = background_scene_app();
        let now = Instant::now();
        assert!(app.settle_background_scene(now));
        assert!(app.state.background_scene.is_some());

        app.state.host_terminal_kind = crate::kitty_graphics::HostTerminalKind::Other;
        assert!(
            app.observe_background_scene(now + std::time::Duration::from_millis(1)),
            "retiring the scene did not report a change to redraw"
        );
        assert!(app.state.background_scene.is_none());
        assert_eq!(app.state.background_scene_key, 0);
    }

    /// The sidebar wash is opaque too, in the same band, so it answers to the
    /// same fact about the host.
    #[test]
    fn the_sidebar_particle_field_is_refused_on_a_host_that_does_not_draw_an_ambient_wash() {
        let mut app = particle_field_app();
        app.state.host_terminal_kind = crate::kitty_graphics::HostTerminalKind::Other;
        assert!(!app.observe_sidebar_particle_field());
        assert!(app.state.sidebar_particle_field.is_none());
    }

    fn set_first_pane_state(app: &mut super::super::App, state: crate::detect::AgentState) {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        if let Some(terminal) = app.state.terminals.get_mut(&terminal_id) {
            terminal.state = state;
        }
    }

    fn live_washes(app: &super::super::App) -> usize {
        app.state
            .workspaces
            .iter()
            .filter(|workspace| {
                app.state
                    .sidebar_card_washes
                    .live(&crate::anim::CardRow::Space(workspace.id.clone()))
                    .is_some()
            })
            .count()
    }

    /// The whole plumbing, through the loop the app really runs: a card's state
    /// moves, one wash mounts in its own family, it plays, and it retires when
    /// its window closes.
    ///
    /// The families are checked to coexist for the same reason the tray's are.
    /// A wash published as `Family::AgentRow` or `Family::WorkspaceRow` would be
    /// swept by the row reconciliation it shares a pass with; published as
    /// `Named` the fleet signals' own pass would retire it mid-sweep.
    #[test]
    fn a_state_change_mounts_one_wash_and_it_retires_when_its_window_closes() {
        let mut app = card_app();
        if !app.state.sidebar_card_animation_active() {
            // No proportional face on this machine: nothing draws pixel cards,
            // so nothing publishes their animation either. The same skip every
            // other pixel-card test takes.
            return;
        }
        let now = Instant::now();
        set_first_pane_state(&mut app, crate::detect::AgentState::Idle);
        app.advance_animations(now, true);
        assert_eq!(
            live_washes(&app),
            0,
            "a card arriving is not a card changing"
        );

        set_first_pane_state(&mut app, crate::detect::AgentState::Working);
        app.advance_animations(now, true);
        assert_eq!(live_washes(&app), 1);

        let window = app.state.sidebar_cards.wash_duration();
        let id = crate::anim::ElementId::CardWash(
            app.state
                .sidebar_card_washes
                .live(&crate::anim::CardRow::Space(
                    app.state.workspaces[0].id.clone(),
                ))
                .expect("the wash is live"),
        );
        let frame = app
            .state
            .anim
            .frame(&id, None)
            .expect("the wash is tracked");
        assert_eq!(frame.phase, crate::anim::Phase::Mount);
        assert!(frame.behaviour.is_some(), "the sweep is playing nothing");

        // Half way: still mounting, and further through than it was.
        app.advance_animations(now + window / 2, true);
        let mid = app
            .state
            .anim
            .frame(&id, None)
            .expect("the wash is still tracked")
            .progress;
        assert!(mid > 0.3 && mid < 0.9, "the sweep is at {mid:.3} half way");

        // And past its window it is gone from both the memory and the engine.
        app.advance_animations(now + window + Duration::from_millis(50), true);
        assert_eq!(live_washes(&app), 0);
        assert!(app.state.anim.frame(&id, None).is_none());
    }

    /// Turning the wash off retires exactly its elements and leaves the rows
    /// alone, which is what publishing an empty set for a switched-off family
    /// buys.
    #[test]
    fn switching_the_wash_off_retires_it_without_touching_the_rows() {
        let mut app = card_app();
        if !app.state.sidebar_card_animation_active() {
            return;
        }
        let now = Instant::now();
        set_first_pane_state(&mut app, crate::detect::AgentState::Idle);
        app.advance_animations(now, true);
        set_first_pane_state(&mut app, crate::detect::AgentState::Working);
        app.advance_animations(now, true);
        assert_eq!(live_washes(&app), 1);

        app.state.sidebar_cards.wash = false;
        app.advance_animations(now + Duration::from_millis(20), true);
        assert_eq!(live_washes(&app), 0);
        let row = crate::anim::ElementId::workspace_row(&app.state.workspaces[0].id);
        assert!(
            app.state.anim.frame(&row, None).is_some(),
            "retiring the wash family took the workspace row with it"
        );
    }

    /// The reason the badges are their own family.
    ///
    /// The fleet signals reconcile `Family::Named` against the ones that are
    /// *live*, and a resting badge is not live. Published under the same family
    /// the tray's eight would be retired by the bar's own pass every frame —
    /// they would mount, be told to leave, and never move. This asserts the two
    /// coexist: the bar publishing nothing must leave all eight badges standing.
    #[test]
    fn the_signal_bar_cannot_retire_the_trays_badges() {
        let mut app = tray_app();
        // The bar is on with nothing lit, which is the case that would evict.
        app.state.sidebar_notifications.enabled = true;
        assert!(
            !crate::app::fleet_signals::FleetSignals::resolve(&app.state).any_live(),
            "the fixture lit a signal, which is not the case under test"
        );

        let now = Instant::now();
        app.advance_animations(now, true);
        app.advance_animations(now + Duration::from_millis(120), true);

        for signal in crate::app::fleet_signals::FleetSignal::ALL {
            assert!(
                app.state
                    .anim
                    .frame(&signal.badge_element_id(), None)
                    .is_some(),
                "{signal:?} was retired by another family's reconciliation"
            );
        }
    }

    /// Turning the tray's animation off retires exactly its elements, and the
    /// artwork stops asking the loop for frames.
    #[test]
    fn switching_badge_motion_off_retires_the_badges() {
        let mut app = tray_app();
        let now = Instant::now();
        app.advance_animations(now, true);
        assert!(!app.state.anim.is_empty());

        app.state.sidebar_signal_tray.animate = false;
        app.advance_animations(now + Duration::from_millis(20), true);
        for signal in crate::app::fleet_signals::FleetSignal::ALL {
            assert!(
                app.state
                    .anim
                    .frame(&signal.badge_element_id(), None)
                    .is_none(),
                "{signal:?} kept animating after motion was switched off"
            );
        }
    }

    /// The artwork's cache key follows the animation, so the app loop keeps
    /// re-rasterising while a badge is moving — and stops when it settles.
    #[test]
    fn a_moving_badge_makes_the_app_loop_redraw_the_artwork() {
        let mut app = tray_app();
        app.state.workspaces[0].cached_git_ahead_behind = Some((3, 0));
        let now = Instant::now();
        app.advance_animations(now, true);
        assert!(
            app.observe_signal_tray(now, false),
            "the tray drew nothing at all"
        );

        let first = app.state.signal_tray_graphics_key;
        app.advance_animations(now + Duration::from_millis(400), true);
        assert!(
            app.observe_signal_tray(now + Duration::from_millis(400), false),
            "the artwork did not follow the animation"
        );
        assert_ne!(first, app.state.signal_tray_graphics_key);

        // Asked again at the same instant, nothing has moved and nothing is
        // redrawn: the badge costs a raster per frame it moves, not per pass.
        assert!(!app.refresh_signal_tray_graphics(false));
    }

    /// A resting tray costs the terminal one upload, not one per badge frame.
    ///
    /// The key follows the animation — that is what
    /// `a_moving_badge_makes_the_app_loop_redraw_the_artwork` pins, and it is
    /// right — so the artwork is *rasterised* on every badge frame. Whether
    /// each of those is worth a whole-surface upload is the second question,
    /// and at rest the answer is almost always no: the breath moves the pixels
    /// by a fraction of one 8-bit level.
    ///
    /// Measured on a real client before this existed: 20 uploads a second at
    /// 328x128, 3.28 MiB/s, on a fleet with nothing happening.
    #[test]
    fn a_resting_tray_publishes_once_and_then_holds() {
        const FRAMES: u64 = 40;
        let mut app = tray_app();
        let now = Instant::now();
        app.advance_animations(now, true);
        assert!(
            app.observe_signal_tray(now, false),
            "the tray drew nothing at all"
        );
        let published = app
            .state
            .signal_tray_graphics
            .clone()
            .expect("a resting tray still has artwork");

        let mut on_screen = published.data_fingerprint;
        let mut republished = 0;
        for step in 1..=FRAMES {
            let at = now + Duration::from_millis(step * 50);
            app.advance_animations(at, true);
            app.observe_signal_tray(at, false);
            let current = app
                .state
                .signal_tray_graphics
                .as_ref()
                .expect("the tray lost its artwork while resting")
                .data_fingerprint;
            if current != on_screen {
                republished += 1;
                on_screen = current;
            }
        }
        assert!(
            republished * 3 < FRAMES,
            "a resting tray handed the terminal {republished} of {FRAMES} frames"
        );
    }

    /// And a badge that actually lights is published at once, not on the
    /// tolerance's clock.
    #[test]
    fn a_badge_lighting_up_reaches_the_terminal_immediately() {
        let mut app = tray_app();
        let now = Instant::now();
        app.advance_animations(now, true);
        app.observe_signal_tray(now, false);
        let resting = app
            .state
            .signal_tray_graphics
            .clone()
            .expect("a resting tray still has artwork");

        app.state.workspaces[0].cached_git_ahead_behind = Some((3, 0));
        let at = now + Duration::from_millis(50);
        app.advance_animations(at, true);
        assert!(
            app.observe_signal_tray(at, false),
            "a badge lit and the tray reported nothing"
        );
        assert_ne!(
            app.state
                .signal_tray_graphics
                .as_ref()
                .expect("artwork")
                .data_fingerprint,
            resting.data_fingerprint,
            "a badge lit and the terminal was left showing the resting artwork"
        );
    }

    /// When every viewer draws the badges itself, the app loop stops drawing
    /// them — but does not stop *watching* them. The key still has to follow
    /// the animation, because it is what tells the loop a new scene is worth
    /// sending, and a tray that stopped reporting change would ship one scene
    /// and then hold still.
    #[test]
    fn a_delegated_tray_tracks_the_animation_without_rasterising_it() {
        let mut app = tray_app();
        app.state.workspaces[0].cached_git_ahead_behind = Some((3, 0));
        let now = Instant::now();
        app.advance_animations(now, true);

        assert!(app.observe_signal_tray(now, true), "nothing was observed");
        assert!(
            app.state.signal_tray_graphics.is_none(),
            "the server rasterised badge pixels no client was going to be sent"
        );
        assert!(
            app.state.signal_tray_graphics_client_rasterized,
            "the renderer was not told the badges are coming from elsewhere"
        );
        // And there is still a scene to send, saying the same thing the
        // artwork would have said.
        assert!(crate::ui::build_signal_tray_scene(&app.state).is_some());

        let first = app.state.signal_tray_graphics_key;
        app.advance_animations(now + Duration::from_millis(400), true);
        assert!(
            app.observe_signal_tray(now + Duration::from_millis(400), true),
            "a moving badge did not register as a change worth a new scene"
        );
        assert_ne!(first, app.state.signal_tray_graphics_key);
        assert!(app.state.signal_tray_graphics.is_none());

        // The last such client leaving hands the tray back: the pixels return
        // and the frame that carries them is asked for.
        assert!(
            app.observe_signal_tray(now + Duration::from_millis(400), false),
            "handing the tray back did not repaint it"
        );
        assert!(
            app.state.signal_tray_graphics.is_some(),
            "the server never resumed drawing a tray nobody else was drawing"
        );
        assert!(!app.state.signal_tray_graphics_client_rasterized);
    }

    #[test]
    fn sidebar_particle_field_generates_once_and_holds_still_on_a_repeat_pass() {
        let mut app = particle_field_app();

        assert!(
            app.observe_sidebar_particle_field(),
            "an enabled, sized wash generated nothing"
        );
        let layer = app
            .state
            .sidebar_particle_field
            .as_ref()
            .expect("wash generated");
        assert!(layer
            .animation
            .as_ref()
            .is_some_and(|a| !a.frames.is_empty()));
        let key = app.state.sidebar_particle_field_key;

        // Nothing about the sidebar moved, so the whole loop is not regenerated: generating a
        // full animation sequence per tick is exactly the per-frame cost this transport exists
        // to avoid paying.
        assert!(!app.observe_sidebar_particle_field());
        assert_eq!(key, app.state.sidebar_particle_field_key);
    }

    #[test]
    fn sidebar_particle_field_regenerates_when_the_sidebar_is_resized() {
        let mut app = particle_field_app();
        assert!(app.observe_sidebar_particle_field());
        let first_key = app.state.sidebar_particle_field_key;

        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 30, 30);
        assert!(
            app.observe_sidebar_particle_field(),
            "a resize did not regenerate the wash"
        );
        assert_ne!(first_key, app.state.sidebar_particle_field_key);
    }

    #[test]
    fn sidebar_particle_field_disabled_generates_nothing() {
        let mut app = particle_field_app();
        app.state.sidebar_particle_field_enabled = false;
        assert!(!app.observe_sidebar_particle_field());
        assert!(app.state.sidebar_particle_field.is_none());
    }

    #[test]
    fn sidebar_particle_field_retires_when_turned_off_after_generating() {
        let mut app = particle_field_app();
        assert!(app.observe_sidebar_particle_field());
        assert!(app.state.sidebar_particle_field.is_some());

        app.state.sidebar_particle_field_enabled = false;
        assert!(
            app.observe_sidebar_particle_field(),
            "turning the wash off did not report a change to redraw"
        );
        assert!(app.state.sidebar_particle_field.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_metrics_unavailable() {
        // Without a runtime, pane_scroll_metrics returns None.
        // Fail-closed: stop autoscroll instead of rescheduling forever.
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        // Drag to a different cell so it becomes Dragging
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // Should stop because no runtime metrics available
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_done() {
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        // Create a selection that is already finished (not in progress)
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        // Drag to a different cell so it becomes visible, then finish
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        sel.finish(); // now it's Done, not in progress
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_cleared() {
        let (mut app, _pane_id) = test_app_with_pane();
        let now = Instant::now();
        app.state.selection = None;
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_anchored() {
        // Anchored (click, no drag) should not keep the timer running.
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        app.state.selection = Some(crate::selection::Selection::anchor(pane_id, 0, 0, None));
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    /// Creates an app with a real TerminalRuntime (no PTY) so scroll_metrics
    /// returns meaningful data. Uses test_with_scrollback_bytes.
    fn test_app_with_runtime(
        cols: u16,
        rows: u16,
        bytes: &[u8],
    ) -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(cols, rows, 0, bytes);
        ws.tabs[0].runtimes.insert(pane_id, runtime);
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, cols, rows),
            inner_rect: ratatui::layout::Rect::new(0, 0, cols, rows),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        (app, pane_id)
    }

    #[tokio::test]
    async fn tick_selection_autoscroll_stops_at_scrollback_top() {
        // Create a runtime with no scrollback content — we're already at
        // the top (offset_from_bottom == max_offset_from_bottom).
        let (mut app, pane_id) = test_app_with_runtime(80, 24, &[]);
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 5, 5, None);
        sel.drag(0, 0, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Up,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 0,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // At scrollback top, can't scroll further up — should stop
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn tick_selection_autoscroll_stops_at_scrollback_bottom() {
        // Create a runtime with no scrollback content — we're already at
        // the bottom (offset_from_bottom == 0).
        let (mut app, pane_id) = test_app_with_runtime(80, 24, &[]);
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // At scrollback bottom, can't scroll further down — should stop
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn passive_mouse_motion_does_not_request_monolithic_render() {
        let (mut app, _) = test_app_with_pane();
        app.state.mode = crate::app::Mode::Terminal;
        let motion = || {
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Moved,
                column: 10,
                row: 5,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };

        assert!(!app.handle_raw_input_event(motion()).await);
        app.state.mode = crate::app::Mode::GlobalMenu;
        assert!(app.handle_raw_input_event(motion()).await);
    }

    #[tokio::test]
    async fn raw_input_batch_does_not_start_pending_agent_resume_before_render() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane should have a terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(
            app.handle_raw_input_batch(crate::raw_input::RawInputEvent::HostDefaultColor {
                kind: crate::terminal_theme::DefaultColorKind::Foreground,
                color: crate::terminal_theme::RgbColor {
                    r: 220,
                    g: 220,
                    b: 220,
                },
            })
            .await
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "raw input can mutate active geometry; pending resumes must wait for render to refresh pane_infos"
        );
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
    }

    #[tokio::test]
    async fn scheduled_tasks_do_not_start_pending_agent_resume_when_geometry_dirty() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane should have a terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert!(!app.handle_scheduled_tasks(Instant::now(), true));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(app.pending_agent_resume_deadline.is_none());
    }

    /// Two second mates, each owning its own workers, under one first mate.
    ///
    /// The shape the captain's fleet actually has, and the only shape in which
    /// "only that group moved" means anything.
    fn fleet_app(
        exit: &str,
    ) -> (
        super::super::App,
        crate::layout::PaneId,
        crate::layout::PaneId,
    ) {
        let config: crate::config::Config = toml::from_str(&format!(
            "[ui.sidebar.animation]\nrow_exit = \"{exit}\"\nrow_exit_ms = 200\n"
        ))
        .expect("row exit config");
        let mut app = super::super::App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );

        let mut left = Workspace::test_new("2ndmate-left");
        let left_second = left.test_split(ratatui::layout::Direction::Vertical);
        let left_first = left.tabs[0]
            .panes
            .keys()
            .copied()
            .find(|pane| *pane != left_second)
            .expect("the original pane is still present");
        let right = Workspace::test_new("2ndmate-right");
        let right_only = right.tabs[0].root_pane;

        app.state.workspaces = vec![Workspace::test_new("firstmate"), left, right];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();

        let now = Instant::now();
        for ws_idx in [1, 2] {
            app.state.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                now,
            );
        }
        for (ws_idx, pane, name, owner) in [
            (1, left_first, "left-worker-1", "2ndmate-left"),
            (1, left_second, "left-worker-2", "2ndmate-left"),
            (2, right_only, "right-worker", "2ndmate-right"),
        ] {
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane)
                .cloned()
                .expect("every test pane has a terminal");
            let terminal = app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("every test pane has a terminal");
            terminal.set_agent_name(name.to_string());
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([("owner".to_string(), Some(owner.to_string()))]),
                None,
                now,
            );
        }

        (app, left_second, right_only)
    }

    /// The agent rows the sidebar tree would draw, by agent name, in order.
    fn tree_worker_names(app: &super::super::App) -> Vec<String> {
        let agents = crate::ui::sidebar_agent_entries(&app.state);
        crate::ui::workspace_list_entries(&app.state)
            .into_iter()
            .filter_map(|entry| match entry {
                crate::ui::WorkspaceListEntry::Agent { entry_idx, .. } => agents
                    .get(entry_idx)
                    .and_then(|agent| agent.agent_name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_worker_leaving_contracts_only_its_own_second_mates_group() {
        let (mut app, leaving, _) = fleet_app("fade");
        let now = Instant::now();
        app.advance_animations(now, true);
        // `left-worker-2` was created second and so entered at the head of its
        // mate's group - see `crate::ui::sidebar::enter_at_head`. That makes the
        // departing row below the *first* one here, which is the harder case:
        // its place has to be held from index zero.
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-2", "left-worker-1", "right-worker"],
            "the fleet did not start in ownership order"
        );

        // The worker finishes: its pane closes and it is gone from the session.
        // `close_pane` reports whether the *workspace* emptied, not whether it
        // did anything, so the pane going is what to assert on.
        app.state.workspaces[1].close_pane(leaving);
        assert!(app.state.workspaces[1].pane_state(leaving).is_none());
        app.advance_animations(now + Duration::from_millis(10), true);

        // It is still drawn, still in its own place, under its own second mate.
        // The other second mate's group never entered into it.
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-2", "left-worker-1", "right-worker"],
            "the row was dropped before it could be seen leaving"
        );

        // And when the exit finishes, that group - and only that group -
        // contracts.
        app.advance_animations(now + Duration::from_millis(400), true);
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-1", "right-worker"],
            "the group did not contract once the exit was over"
        );
        assert_eq!(
            app.state.sidebar_tree_row_memory.len(),
            2,
            "a finished exit must not be remembered"
        );
    }

    #[test]
    fn without_a_configured_exit_a_finished_worker_is_gone_on_the_next_frame() {
        let (mut app, leaving, _) = fleet_app("none");
        let now = Instant::now();
        // Nothing is configured to animate at all, so the engine tracks nothing
        // and there is nothing to remember.
        app.advance_animations(now, true);
        assert!(app.state.sidebar_tree_row_memory.is_empty());

        app.state.workspaces[1].close_pane(leaving);
        app.advance_animations(now + Duration::from_millis(10), true);
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-1", "right-worker"],
            "an unconfigured Herdr must drop a closed worker's row at once"
        );
    }

    /// The combination neither feature was tested in on its own: a fleet with
    /// both the fleet signals and row exits switched on.
    ///
    /// The bar's slots and the tree's rows are different families sharing one
    /// engine, so the risk is that publishing one disturbs the other. A group
    /// must still grow, hold a departing row for its exit, and contract, with
    /// the bar live throughout.
    #[test]
    fn a_group_still_contracts_while_the_signal_bar_is_running() {
        use crate::app::fleet_signals::FleetSignal;

        let (mut app, leaving, _) = fleet_app("fade");
        app.state.sidebar_notifications.enabled = true;
        // Something for the bar to actually light up, so its family is
        // populated rather than trivially empty.
        app.state.workspaces[1].cached_git_ahead_behind = Some((1, 0));
        let now = Instant::now();

        app.advance_animations(now, true);
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-2", "left-worker-1", "right-worker"],
            "the fleet did not start in ownership order with the bar on"
        );
        assert!(
            app.state
                .anim
                .frame(&FleetSignal::Push.element_id(), None)
                .is_some(),
            "the bar is not actually running, so this proves nothing"
        );

        app.state.workspaces[1].close_pane(leaving);
        app.advance_animations(now + Duration::from_millis(10), true);
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-2", "left-worker-1", "right-worker"],
            "a live fleet signal cost the departing row its exit"
        );
        assert!(!app.state.sidebar_tree_row_memory.is_empty());

        app.advance_animations(now + Duration::from_millis(400), true);
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-1", "right-worker"],
            "the group did not contract with the bar on"
        );
        // And the bar is still there, untouched by the tree's comings and goings.
        assert!(app
            .state
            .anim
            .frame(&FleetSignal::Push.element_id(), None)
            .is_some());
    }

    /// A pass running for the bar alone still leaves the tree in a clean state:
    /// nothing remembered, nothing drawn that has no reason to be there.
    #[test]
    fn a_bar_only_pass_leaves_no_tree_rows_behind() {
        let (mut app, leaving, _) = fleet_app("fade");
        app.state.sidebar_notifications.enabled = true;
        let now = Instant::now();

        app.advance_animations(now, true);
        app.state.workspaces[1].close_pane(leaving);
        app.advance_animations(now + Duration::from_millis(10), true);
        assert!(!app.state.sidebar_tree_row_memory.is_empty());

        // The tree's animation goes away mid-exit; the bar keeps the pass alive.
        app.state.sidebar_animation.row_exit = crate::config::SidebarTokenEmphasis::None;
        app.state.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::None;
        assert!(!app.state.sidebar_animation_active());
        assert!(app.state.fleet_signal_animation_active());

        app.advance_animations(now + Duration::from_millis(20), true);
        assert!(
            app.state.sidebar_tree_row_memory.is_empty(),
            "a row mid-exit was left in memory when only the bar was running"
        );
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-1", "right-worker"],
            "the departed row was still being drawn with nothing animating it"
        );
    }

    /// Collapsing the sidebar switches the tree's animation and the fleet signals
    /// off together.
    ///
    /// `observe_agent_rows` leans on this: it is what makes "the bar is the
    /// only reason this pass is running" imply "nothing is configured to
    /// animate the tree", and so imply that there is no exit stage to hold a
    /// departed row's memory open. Break the coupling and that guard starts
    /// carrying real weight.
    #[test]
    fn the_two_animation_gates_are_collapsed_together() {
        let (mut app, _, _) = fleet_app("fade");
        app.state.sidebar_notifications.enabled = true;
        assert!(app.state.sidebar_animation_active());
        assert!(app.state.fleet_signal_animation_active());

        app.state.sidebar_collapsed = true;
        assert!(
            !app.state.sidebar_animation_active() && !app.state.fleet_signal_animation_active(),
            "one animation gate survived a collapse the other did not"
        );
    }

    #[test]
    fn a_detached_server_forgets_the_rows_it_was_remembering() {
        let (mut app, leaving, _) = fleet_app("fade");
        let now = Instant::now();
        app.advance_animations(now, true);
        app.state.workspaces[1].close_pane(leaving);
        app.advance_animations(now + Duration::from_millis(10), true);
        assert!(!app.state.sidebar_tree_row_memory.is_empty());

        // Nobody is looking, so there is no exit to play and nothing to hold a
        // departed pane's row for.
        app.advance_animations(now + Duration::from_millis(20), false);
        assert!(app.state.sidebar_tree_row_memory.is_empty());
        assert_eq!(
            tree_worker_names(&app),
            vec!["left-worker-1", "right-worker"]
        );
    }
}
