use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use super::{App, GIT_REMOTE_STATUS_REFRESH_INTERVAL, GIT_REPO_DISCOVERY_REFRESH_INTERVAL};
use crate::events::AppEvent;
use crate::workspace::{GitStatusCacheEntry, GitStatusRefreshDemand, WorkspaceGitStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshItem {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
    cache_key_hint: Option<PathBuf>,
    /// Whether this item is the diff pane's own target — see
    /// [`App::diff_pane_target`]. Carried per-item rather than matched by
    /// `workspace_id` alone, because the diff target's cwd (the focused
    /// pane's worktree) can now differ from the same Space's sidebar
    /// identity item (its first tab's root pane), landing the two as
    /// separate items that must not be confused for one another.
    is_diff_target: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshTarget {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
    is_diff_target: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshJob {
    cache_key: PathBuf,
    cached: Option<GitStatusCacheEntry>,
    targets: Vec<WorkspaceGitRefreshTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshOutput {
    results: Vec<WorkspaceGitStatus>,
    cache_updates: Vec<(PathBuf, GitStatusCacheEntry)>,
}

impl App {
    pub(crate) fn start_git_status_refresh_if_due(&mut self, now: Instant) {
        let Some(deadline) = self.git_refresh_deadline() else {
            return;
        };

        if now < deadline {
            return;
        }

        let refresh_repo_discovery = self.git_identity_refresh_requested
            || now.saturating_duration_since(self.last_git_repo_discovery_refresh)
                >= GIT_REPO_DISCOVERY_REFRESH_INTERVAL;
        let workspaces = self.workspace_git_refresh_items(refresh_repo_discovery);

        if workspaces.is_empty() {
            self.last_git_remote_status_refresh = now;
            self.git_identity_refresh_requested = false;
            return;
        }

        self.git_refresh_in_flight = true;
        let event_tx = self.event_tx.clone();
        let cache = self.git_status_cache.clone();
        let mut demand = self.git_refresh_demand();
        if self.git_identity_refresh_requested {
            demand.branch = true;
        }
        self.git_identity_refresh_requested = false;
        if refresh_repo_discovery {
            self.last_git_repo_discovery_refresh = now;
        }
        std::thread::spawn(move || {
            let output =
                refresh_workspace_git_statuses_with_cache_and_demand(workspaces, &cache, demand);
            let _ = event_tx.blocking_send(AppEvent::GitStatusRefreshed {
                results: output.results,
                cache_updates: output.cache_updates,
            });
        });
    }

    pub(crate) fn request_git_identity_refresh(&mut self, now: Instant) {
        self.git_identity_refresh_requested = true;
        self.mark_git_status_refresh_due(now);
    }

    pub(crate) fn mark_git_status_refresh_due(&mut self, now: Instant) {
        self.git_status_cache
            .retain(|_, entry| entry.fingerprint.is_some());
        if self.git_refresh_in_flight {
            self.git_refresh_due_after_in_flight = true;
            return;
        }
        self.last_git_remote_status_refresh = now
            .checked_sub(GIT_REMOTE_STATUS_REFRESH_INTERVAL)
            .unwrap_or(now);
        self.git_refresh_due_after_in_flight = false;
    }

    pub(crate) fn git_refresh_deadline(&self) -> Option<Instant> {
        (!self.git_refresh_in_flight
            && !self.state.workspaces.is_empty()
            && (self.git_identity_refresh_requested
                || !self.git_refresh_demand().is_empty()
                || self.diff_pane_target().is_some()))
        .then_some(self.last_git_remote_status_refresh + GIT_REMOTE_STATUS_REFRESH_INTERVAL)
    }

    /// The workspace and cwd the diff pane is currently asking for, if any —
    /// the active Space's *focused pane*, and only while the pane is
    /// actually visible (the fixed three-zone layout, or the folded popup
    /// fallback).
    ///
    /// Deliberately the focused pane's own cwd, not
    /// [`crate::workspace::Workspace::resolved_identity_cwd_from`] (the
    /// Space's sidebar identity, fixed to its first tab's root pane): a
    /// Space can hold several workers, each its own tab in its own worktree,
    /// and the diff pane is supposed to follow whichever one is focused, not
    /// always the first one created. Switching tabs or split panes inside
    /// the same Space therefore can change what the diff pane shows even
    /// though the Space's own label and branch stay put.
    ///
    /// Not folded into the shared [`GitStatusRefreshDemand`] the rest of this
    /// module computes: that demand is applied uniformly across every
    /// distinct repo among *all* open workspaces (see
    /// `refresh_workspace_git_statuses_with_cache_and_demand`), which is
    /// correct for `dirty`/`branch`/`ahead_behind` (every Space's sidebar row
    /// wants its own), but would make every open Space in the fleet pay for a
    /// `git diff` invocation whenever only one Space's diff pane is open.
    /// Kept separate so only the focused pane's own target ever demands
    /// `diff`.
    pub(crate) fn diff_pane_target(&self) -> Option<(String, PathBuf)> {
        let visible = !self.state.view.diff_area.is_empty() || self.state.diff_popup_open;
        if !visible {
            return None;
        }
        let ws = self
            .state
            .active
            .and_then(|idx| self.state.workspaces.get(idx))?;
        let cwd = ws.focused_pane_cwd_from(&self.state.terminals, &self.terminal_runtimes)?;
        Some((ws.id.clone(), cwd))
    }

    fn git_refresh_demand(&self) -> GitStatusRefreshDemand {
        let mut demand = GitStatusRefreshDemand::default();
        for token in self.state.sidebar_spaces.rows.iter().flatten() {
            match token.parts().0 {
                crate::config::SpaceSidebarToken::Branch => demand.branch = true,
                crate::config::SpaceSidebarToken::GitStatus => demand.ahead_behind = true,
                crate::config::SpaceSidebarToken::GitDirty => demand.dirty = true,
                _ => {}
            }
        }
        // A configured fleet pulse arms the same counts, so it carries the same
        // demand. Without this its `push` and `sync` slots would be drawn over
        // counts nothing ever refreshed and could never go live.
        if self.state.sidebar_notifications.enabled {
            let signals = crate::app::fleet_signals::FleetSignalDemand::for_all_signals();
            demand.dirty |= signals.git_dirty;
            demand.ahead_behind |= signals.git_ahead_behind;
        }
        // The tray demands strictly more: its `sync` refuses on a dirty tree,
        // and a refusal that cannot see the tree is a refusal that always
        // fires. See `FleetSignalDemand::for_tray`.
        if crate::ui::signal_tray_active(&self.state) {
            let signals = crate::app::fleet_signals::FleetSignalDemand::for_tray();
            demand.dirty |= signals.git_dirty;
            demand.ahead_behind |= signals.git_ahead_behind;
        }
        demand
    }

    fn workspace_git_refresh_items(
        &self,
        refresh_repo_discovery: bool,
    ) -> Vec<WorkspaceGitRefreshItem> {
        let mut items: Vec<WorkspaceGitRefreshItem> = self
            .state
            .workspaces
            .iter()
            .filter_map(|ws| {
                let cwd =
                    ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)?;
                let cache_key_hint = (!refresh_repo_discovery && ws.cached_identity_cwd == cwd)
                    .then(|| ws.cached_git_status_key.clone());
                Some(WorkspaceGitRefreshItem {
                    workspace_id: ws.id.clone(),
                    resolved_identity_cwd: cwd,
                    cache_key_hint,
                    is_diff_target: false,
                })
            })
            .collect();

        if let Some((workspace_id, cwd)) = self.diff_pane_target() {
            match items
                .iter_mut()
                .find(|item| item.workspace_id == workspace_id && item.resolved_identity_cwd == cwd)
            {
                // The focused pane's cwd matches this Space's own sidebar
                // identity item already (the common case: focus is on the
                // Space's first tab) — mark it rather than adding a
                // duplicate that would just re-dedupe into the same job.
                Some(item) => item.is_diff_target = true,
                // A different worker's worktree: its own item, so it gets
                // its own cache key and — unless some other open Space
                // happens to share that exact repo — its own `git diff` job.
                None => items.push(WorkspaceGitRefreshItem {
                    workspace_id,
                    resolved_identity_cwd: cwd,
                    cache_key_hint: None,
                    is_diff_target: true,
                }),
            }
        }

        items
    }
}

fn deduplicate_git_refresh_items(
    items: Vec<WorkspaceGitRefreshItem>,
    cache: &HashMap<PathBuf, GitStatusCacheEntry>,
) -> Vec<WorkspaceGitRefreshJob> {
    let mut indexes = HashMap::<PathBuf, usize>::new();
    let mut jobs = Vec::<WorkspaceGitRefreshJob>::new();

    for item in items {
        let cache_key = item.cache_key_hint.unwrap_or_else(|| {
            crate::workspace::git_status_cache_key(&item.resolved_identity_cwd)
                .unwrap_or_else(|| item.resolved_identity_cwd.clone())
        });
        let target = WorkspaceGitRefreshTarget {
            workspace_id: item.workspace_id,
            resolved_identity_cwd: item.resolved_identity_cwd,
            is_diff_target: item.is_diff_target,
        };
        if let Some(&index) = indexes.get(&cache_key) {
            jobs[index].targets.push(target);
            continue;
        }

        let cached = cache.get(&cache_key).cloned();
        indexes.insert(cache_key.clone(), jobs.len());
        jobs.push(WorkspaceGitRefreshJob {
            cache_key,
            cached,
            targets: vec![target],
        });
    }

    jobs
}

fn refresh_workspace_git_statuses_with_cache_and_demand(
    items: Vec<WorkspaceGitRefreshItem>,
    cache: &HashMap<PathBuf, GitStatusCacheEntry>,
    demand: GitStatusRefreshDemand,
) -> WorkspaceGitRefreshOutput {
    let mut results = Vec::new();
    let mut cache_updates = Vec::new();

    for job in deduplicate_git_refresh_items(items, cache) {
        // `diff` is the one demand bit decided per job rather than shared:
        // every other bit is wanted for every open Space's sidebar row, but a
        // diff pane shows only the focused pane's own diff, so only the job
        // that actually carries the diff target ever pays for `git diff` —
        // see `App::diff_pane_target`.
        let job_demand = GitStatusRefreshDemand {
            diff: demand.diff || job.targets.iter().any(|target| target.is_diff_target),
            ..demand
        };
        let (snapshot, cache_entry) = crate::workspace::git_status_snapshot_for_cwd_with_demand(
            &job.cache_key,
            job.cached.as_ref(),
            job_demand,
        );
        if let Some(cache_entry) = cache_entry {
            cache_updates.push((job.cache_key.clone(), cache_entry));
        }
        results.extend(job.targets.into_iter().map(move |target| {
            snapshot.clone().into_workspace_status(
                target.workspace_id,
                target.resolved_identity_cwd,
                job.cache_key.clone(),
                job_demand,
            )
        }));
    }

    WorkspaceGitRefreshOutput {
        results,
        cache_updates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn git_refresh_deduplicates_workspaces_with_same_cache_key() {
        let repo =
            std::env::temp_dir().join(format!("herdr-git-refresh-dedupe-{}", std::process::id()));
        let nested = repo.join("nested");
        let other = repo.join("other");
        std::fs::create_dir_all(&nested).expect("create nested dir");
        std::fs::create_dir_all(&other).expect("create other dir");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("init")
            .output()
            .expect("run git init");

        let output = refresh_workspace_git_statuses_with_cache_and_demand(
            vec![
                WorkspaceGitRefreshItem {
                    workspace_id: "one".into(),
                    resolved_identity_cwd: nested.clone(),
                    cache_key_hint: None,
                    is_diff_target: false,
                },
                WorkspaceGitRefreshItem {
                    workspace_id: "two".into(),
                    resolved_identity_cwd: other.clone(),
                    cache_key_hint: None,
                    is_diff_target: false,
                },
            ],
            &HashMap::new(),
            GitStatusRefreshDemand::ALL,
        );

        assert_eq!(output.cache_updates.len(), 1);
        assert_eq!(
            output.cache_updates[0].0,
            std::fs::canonicalize(&repo).expect("canonical repo path")
        );
        assert_eq!(output.results.len(), 2);
        assert_eq!(output.results[0].workspace_id, "one");
        assert_eq!(output.results[0].resolved_identity_cwd, nested);
        assert_eq!(output.results[1].workspace_id, "two");
        assert_eq!(output.results[1].resolved_identity_cwd, other);

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn git_refresh_item_collection_does_not_discover_uncached_cwd() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = std::env::temp_dir().join(format!("herdr-uncached-cwd-{}", std::process::id()));
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resolved_identity_cwd, cwd);
        assert_eq!(items[0].cache_key_hint, None);
    }

    #[test]
    fn git_refresh_item_collection_reuses_matching_cached_key() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = PathBuf::from("/repo/deep/nested");
        let cache_key = PathBuf::from("/repo");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_git_status_key = cache_key.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cache_key_hint, Some(cache_key));
    }

    #[test]
    fn periodic_repo_discovery_ignores_cached_key_hints() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = PathBuf::from("/repo/deep/nested");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_git_status_key = PathBuf::from("/repo");
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(true);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cache_key_hint, None);
    }

    #[test]
    fn cwd_identity_refresh_runs_once_without_sidebar_git_tokens() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        let now = Instant::now();

        app.request_git_identity_refresh(now);

        assert!(app.git_refresh_deadline().is_some());
        app.start_git_status_refresh_if_due(now);
        assert!(app.git_refresh_in_flight);
        assert!(!app.git_identity_refresh_requested);
    }

    #[test]
    fn due_git_refresh_does_not_start_without_sidebar_consumer() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        let now = Instant::now();
        app.last_git_remote_status_refresh = now - GIT_REMOTE_STATUS_REFRESH_INTERVAL;

        app.start_git_status_refresh_if_due(now);

        assert!(!app.git_refresh_in_flight);
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn git_refresh_demand_matches_sidebar_rows() {
        let cases = [
            (
                crate::config::SpaceSidebarToken::Workspace,
                GitStatusRefreshDemand::default(),
            ),
            (
                crate::config::SpaceSidebarToken::Branch,
                GitStatusRefreshDemand {
                    branch: true,
                    ahead_behind: false,
                    dirty: false,
                    diff: false,
                },
            ),
            (
                crate::config::SpaceSidebarToken::GitStatus,
                GitStatusRefreshDemand {
                    branch: false,
                    ahead_behind: true,
                    dirty: false,
                    diff: false,
                },
            ),
        ];

        for (token, expected) in cases {
            let mut config = crate::config::Config::default();
            config.ui.sidebar.spaces.rows = vec![vec![token.clone()]];
            let mut app = test_app(&config);
            app.state.workspaces.push(Workspace::test_new("test"));

            assert_eq!(app.git_refresh_demand(), expected, "token: {token:?}");
            assert_eq!(
                app.git_refresh_deadline().is_some(),
                !expected.is_empty(),
                "token: {token:?}"
            );
        }
    }

    #[test]
    fn unnamed_linked_worktree_does_not_force_periodic_branch_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        let mut child = Workspace::test_new("test");
        child.custom_name = None;
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo".into(),
            label: "repo".into(),
            repo_root: "/repo".into(),
            checkout_path: "/repo-worktree".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces.push(child);

        assert_eq!(app.git_refresh_deadline(), None);
    }

    #[test]
    fn custom_named_linked_worktree_does_not_require_branch_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        let mut child = Workspace::test_new("custom");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo".into(),
            label: "repo".into(),
            repo_root: "/repo".into(),
            checkout_path: "/repo-worktree".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces.push(child);

        assert_eq!(app.git_refresh_deadline(), None);
    }

    #[test]
    fn headless_deadline_can_suppress_git_refresh_timer() {
        // Explicitly configured with a git consumer. The default Space rows are
        // the body registers now — see `crate::ui::sidebar::body_register` — so
        // a default fleet demands no git refresh at all, which is
        // `due_git_refresh_does_not_start_without_sidebar_consumer`'s subject
        // rather than this one's.
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Branch]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        let now = Instant::now();
        app.last_git_remote_status_refresh = now - GIT_REMOTE_STATUS_REFRESH_INTERVAL;

        assert_eq!(
            app.next_headless_loop_deadline_with_git_refresh(now, false, false),
            None
        );
        assert_eq!(
            app.next_headless_loop_deadline_with_git_refresh(now, false, true),
            Some(now)
        );
    }

    #[test]
    fn explicit_git_refresh_invalidates_cached_non_git_results() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = std::env::temp_dir().join(format!("herdr-git-miss-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let (_, entry) = crate::workspace::git_status_snapshot_for_cwd_with_demand(
            &cwd,
            None,
            GitStatusRefreshDemand::ALL,
        );
        app.git_status_cache
            .insert(cwd.clone(), entry.expect("non-Git cache entry"));

        app.mark_git_status_refresh_due(Instant::now());

        assert!(app.git_status_cache.is_empty());
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn git_refresh_due_request_survives_in_flight_refresh() {
        // A git consumer on the rows, for the reason
        // `headless_deadline_can_suppress_git_refresh_timer` gives.
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Branch]];
        let mut app = test_app(&config);
        let now = Instant::now();
        app.git_refresh_in_flight = true;

        app.mark_git_status_refresh_due(now);
        assert!(app.git_refresh_due_after_in_flight);

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            results: Vec::new(),
            cache_updates: Vec::new(),
        });

        assert!(!app.git_refresh_in_flight);
        assert!(!app.git_refresh_due_after_in_flight);
        assert_eq!(app.git_refresh_deadline(), None);

        app.state.workspaces.push(Workspace::test_new("test"));
        let deadline = app
            .git_refresh_deadline()
            .expect("refresh should be due once a workspace exists");
        assert!(deadline <= Instant::now());
    }

    #[test]
    fn diff_pane_target_is_none_when_the_pane_is_neither_shown_nor_popped_open() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("one"));
        app.state.active = Some(0);

        assert_eq!(app.diff_pane_target(), None);
    }

    #[test]
    fn diff_pane_target_is_the_active_workspaces_focused_pane_when_the_zone_is_shown() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("one"));
        app.state.workspaces.push(Workspace::test_new("two"));
        app.state.active = Some(1);
        app.state.view.diff_area = ratatui::layout::Rect::new(100, 0, 100, 20);

        let (workspace_id, cwd) = app.diff_pane_target().expect("diff target while visible");
        assert_eq!(workspace_id, app.state.workspaces[1].id);
        assert_eq!(cwd, app.state.workspaces[1].identity_cwd);
    }

    #[test]
    fn diff_pane_target_is_the_active_workspaces_focused_pane_when_the_popup_is_open_while_folded()
    {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("one"));
        app.state.active = Some(0);
        app.state.view.diff_area = ratatui::layout::Rect::default();
        app.state.diff_popup_open = true;

        let (workspace_id, cwd) = app
            .diff_pane_target()
            .expect("diff target while popped open");
        assert_eq!(workspace_id, app.state.workspaces[0].id);
        assert_eq!(cwd, app.state.workspaces[0].identity_cwd);
    }

    /// The captain's own stated expectation: switching to a different
    /// worker's pane in the same Space — a second tab in its own worktree —
    /// changes what the diff pane targets, even though the Space's own
    /// sidebar identity (label, branch) stays pinned to its first tab.
    #[test]
    fn diff_pane_target_follows_the_focused_tab_not_the_spaces_first_tab() {
        let mut app = test_app(&crate::config::Config::default());
        let mut ws = Workspace::test_new("one");

        let first_terminal_id = ws.tabs[0]
            .terminal_id(ws.tabs[0].root_pane)
            .unwrap()
            .clone();
        app.state.terminals.insert(
            first_terminal_id.clone(),
            crate::terminal::TerminalState::new(
                first_terminal_id,
                PathBuf::from("/repo/first-worker"),
            ),
        );

        let second_tab_idx = ws.test_add_tab(Some("second worker"));
        let second_root_pane = ws.tabs[second_tab_idx].root_pane;
        let second_terminal_id = ws.tabs[second_tab_idx]
            .terminal_id(second_root_pane)
            .unwrap()
            .clone();
        app.state.terminals.insert(
            second_terminal_id.clone(),
            crate::terminal::TerminalState::new(
                second_terminal_id,
                PathBuf::from("/repo/second-worker"),
            ),
        );
        ws.active_tab = second_tab_idx;

        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.diff_area = ratatui::layout::Rect::new(100, 0, 100, 20);

        let (workspace_id, cwd) = app.diff_pane_target().expect("diff target while visible");
        assert_eq!(workspace_id, app.state.workspaces[0].id);
        assert_eq!(cwd, PathBuf::from("/repo/second-worker"));
        assert_ne!(
            app.state.workspaces[0]
                .resolved_identity_cwd_from(&app.state.terminals, &app.terminal_runtimes),
            Some(cwd),
            "the Space's own sidebar identity must stay on the first tab regardless of focus"
        );
    }

    /// A diff pane shows only the active Space's diff, so `git diff` must run
    /// for that Space's job and no other — never for every open Space in the
    /// fleet just because one diff pane happens to be visible somewhere.
    #[test]
    fn diff_demand_is_scoped_to_only_the_targeted_workspaces_job() {
        let repo_one = std::env::temp_dir().join(format!(
            "herdr-git-refresh-diff-scope-one-{}",
            std::process::id()
        ));
        let repo_two = std::env::temp_dir().join(format!(
            "herdr-git-refresh-diff-scope-two-{}",
            std::process::id()
        ));
        for repo in [&repo_one, &repo_two] {
            std::fs::create_dir_all(repo).expect("create repo dir");
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .arg("init")
                .output()
                .expect("git init");
        }

        let output = refresh_workspace_git_statuses_with_cache_and_demand(
            vec![
                WorkspaceGitRefreshItem {
                    workspace_id: "one".into(),
                    resolved_identity_cwd: repo_one.clone(),
                    cache_key_hint: None,
                    is_diff_target: false,
                },
                WorkspaceGitRefreshItem {
                    workspace_id: "two".into(),
                    resolved_identity_cwd: repo_two.clone(),
                    cache_key_hint: None,
                    is_diff_target: true,
                },
            ],
            &HashMap::new(),
            GitStatusRefreshDemand::default(),
        );

        let one = output
            .results
            .iter()
            .find(|r| r.workspace_id == "one")
            .unwrap();
        let two = output
            .results
            .iter()
            .find(|r| r.workspace_id == "two")
            .unwrap();
        assert!(!one.demand.diff, "workspace one was not the diff target");
        assert!(two.demand.diff, "workspace two was the diff target");

        let _ = std::fs::remove_dir_all(repo_one);
        let _ = std::fs::remove_dir_all(repo_two);
    }

    fn test_app(config: &crate::config::Config) -> super::super::App {
        super::super::App::new(
            config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }
}
