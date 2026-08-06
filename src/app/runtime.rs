use std::time::Instant;

use crossterm::terminal;

use super::{
    background_update_check_enabled, App, AUTO_UPDATE_CHECK_INTERVAL, MIN_RENDER_INTERVAL,
    RESIZE_POLL_INTERVAL, SELECTION_AUTOSCROLL_INTERVAL,
};
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
        changed |= self.observe_signal_tray(now);

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
        if !tree && !signals && !switching && !badges {
            let forgotten = self.state.anim.forget_all();
            let remembered = !self.state.sidebar_tree_row_memory.is_empty();
            self.state.sidebar_tree_row_memory.clear();
            // The card states go with the elements. Keeping them across a host
            // that has stopped drawing would make the first frame back wash
            // every card whose state moved while nobody was looking, and
            // animating history is exactly what `Animator::forget_all` exists
            // to prevent.
            let washed = self.state.sidebar_card_washes.forget_all();
            return forgotten || remembered || washed;
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

        let lifecycle = self.state.sidebar_row_lifecycle();
        let spaces: Members = if tree {
            self.state
                .workspaces
                .iter()
                .map(|workspace| {
                    (
                        crate::anim::ElementId::workspace_row(&workspace.id),
                        crate::anim::behaviour::DriveInputs {
                            activity: self.state.workspace_activity_level(workspace),
                        },
                    )
                })
                .collect()
        } else {
            Members::new()
        };
        let spaces_changed =
            self.state
                .anim
                .observe(now, crate::anim::Family::WorkspaceRow, &lifecycle, spaces);
        let agents_changed = self.observe_agent_rows(now, &lifecycle, tree, washes);

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
        let badge_members: Members = if badges {
            crate::app::signal_tray::resolve(&self.state)
                .animation_membership()
                .collect()
        } else {
            Members::new()
        };
        let badges_changed = self.state.anim.observe(
            now,
            crate::anim::Family::TrayBadge,
            &badge_lifecycle,
            badge_members,
        );

        switch_changed || spaces_changed || agents_changed || signals_changed || badges_changed
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
    /// `tree` is false when the signal bar is the only reason this pass is
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
        let rows: Vec<_> = live
            .iter()
            .map(|entry| {
                (
                    crate::anim::ElementId::agent_row(entry.pane_id),
                    crate::anim::behaviour::DriveInputs {
                        activity: self.state.pane_activity_level(entry.ws_idx, entry.pane_id),
                    },
                )
            })
            .collect();
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
    /// Returns whether anything a frame would show has changed.
    pub(crate) fn observe_signal_tray(&mut self, now: Instant) -> bool {
        if !crate::ui::signal_tray_active(&self.state) {
            let had = self.state.signal_tray_graphics.take().is_some();
            self.state.signal_tray_graphics_key = 0;
            return had;
        }

        let mut changed = self.refresh_blocked_questions(now);
        changed |= self
            .state
            .signal_tray
            .observe(crate::app::signal_tray::magnitudes(&self.state));
        changed |= self.refresh_signal_tray_graphics();
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
    fn refresh_signal_tray_graphics(&mut self) -> bool {
        if !self.state.kitty_graphics_enabled || !self.state.host_cell_size.is_known() {
            let had = self.state.signal_tray_graphics.take().is_some();
            self.state.signal_tray_graphics_key = 0;
            return had;
        }

        let cell = self.state.host_cell_size;
        let key = self.signal_tray_graphics_key(cell);
        if key == self.state.signal_tray_graphics_key && self.state.signal_tray_graphics.is_some() {
            return false;
        }

        let Some((_, image)) =
            crate::ui::signal_tray_image(&self.state, cell.width_px, cell.height_px)
        else {
            let had = self.state.signal_tray_graphics.take().is_some();
            self.state.signal_tray_graphics_key = 0;
            return had;
        };

        self.state.signal_tray_graphics_key = key;
        self.state.signal_tray_graphics = Some(crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            image.width,
            image.height,
            image.pixels,
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 0,
                viewport_row: 0,
                grid_cols: 0,
                grid_rows: 0,
                // Over the text. The badges *are* the tray; the fallback marks
                // underneath them are what a host with no graphics gets, and on
                // a host with graphics they are meant to be covered.
                z: 0,
            },
        ));
        true
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

    /// Move the clock the sidebar renders elapsed times against.
    ///
    /// Deliberately separate from arming the repaint deadline. This runs every
    /// loop iteration so that a frame drawn for any other reason already shows
    /// current ages; re-arming on the same schedule would push the deadline
    /// past `now` on every pass and it would never fire.
    pub(crate) fn refresh_state_age_clock(&mut self, now: Instant) {
        self.state.state_age_now = now;
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
                carrier,
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
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 30, 40);
        app
    }

    /// A fleet whose cards are drawn as pixels, so the card animations have
    /// something to happen to.
    fn card_app() -> super::super::App {
        let (mut app, _) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.sidebar_card_shapes = true;
        app.state.kitty_graphics_enabled = true;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 40, 40);
        app
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
    /// `Named` the fleet signal bar would retire it mid-sweep.
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
    /// The signal bar reconciles `Family::Named` against the signals that are
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
        assert!(app.observe_signal_tray(now), "the tray drew nothing at all");

        let first = app.state.signal_tray_graphics_key;
        app.advance_animations(now + Duration::from_millis(400), true);
        assert!(
            app.observe_signal_tray(now + Duration::from_millis(400)),
            "the artwork did not follow the animation"
        );
        assert_ne!(first, app.state.signal_tray_graphics_key);

        // Asked again at the same instant, nothing has moved and nothing is
        // redrawn: the badge costs a raster per frame it moves, not per pass.
        assert!(!app.refresh_signal_tray_graphics());
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
    /// both the signal bar and row exits switched on.
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
            "a live signal bar cost the departing row its exit"
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

    /// Collapsing the sidebar switches the tree's animation and the signal bar
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
