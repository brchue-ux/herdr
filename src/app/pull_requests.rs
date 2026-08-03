//! Background refresh of open pull request counts per Space.
//!
//! Deliberately a separate loop from [`super::git_refresh`]: that one reads a
//! handful of files out of `.git` and can afford to tick every 1.5s, while this
//! one crosses the network and answers to a rate limit. Both are demand-gated on
//! the sidebar actually asking for the token, so a user who configures neither
//! pays for neither.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use super::{App, PULL_REQUEST_REFRESH_INTERVAL};
use crate::events::AppEvent;
use crate::forge::{ForgeAuth, PullRequestCounts, RepoSlug};

/// Open pull request counts resolved for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePullRequests {
    pub workspace_id: String,
    pub remote_url: Option<String>,
    pub counts: Option<PullRequestCounts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PullRequestRefreshItem {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
}

impl App {
    pub(crate) fn start_pull_request_refresh_if_due(&mut self, now: Instant) {
        let Some(deadline) = self.pull_request_refresh_deadline() else {
            return;
        };
        if now < deadline {
            return;
        }

        let items = self.pull_request_refresh_items();
        if items.is_empty() {
            self.last_pull_request_refresh = now;
            return;
        }

        self.pull_request_refresh_in_flight = true;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let results = refresh_pull_requests(items);
            let _ = event_tx.blocking_send(AppEvent::PullRequestsRefreshed { results });
        });
    }

    pub(crate) fn pull_request_refresh_deadline(&self) -> Option<Instant> {
        (!self.pull_request_refresh_in_flight
            && !self.state.workspaces.is_empty()
            && self.pull_requests_are_rendered())
        .then_some(self.last_pull_request_refresh + PULL_REQUEST_REFRESH_INTERVAL)
    }

    fn pull_requests_are_rendered(&self) -> bool {
        self.state
            .sidebar_spaces
            .rows
            .iter()
            .flatten()
            .any(|token| {
                matches!(
                    token.parts().0,
                    crate::config::SpaceSidebarToken::PullRequests
                )
            })
    }

    fn pull_request_refresh_items(&self) -> Vec<PullRequestRefreshItem> {
        self.state
            .workspaces
            .iter()
            .filter_map(|ws| {
                Some(PullRequestRefreshItem {
                    workspace_id: ws.id.clone(),
                    resolved_identity_cwd: ws.resolved_identity_cwd_from(
                        &self.state.terminals,
                        &self.terminal_runtimes,
                    )?,
                })
            })
            .collect()
    }
}

/// Resolves every item's repository and fetches each distinct one exactly once.
///
/// Runs on its own thread. Two Spaces that are worktrees of the same repository
/// share one request, which is the whole reason the results are keyed by slug
/// rather than by workspace.
fn refresh_pull_requests(items: Vec<PullRequestRefreshItem>) -> Vec<WorkspacePullRequests> {
    let mut remote_urls = HashMap::<PathBuf, Option<String>>::new();
    let mut counts_by_slug = HashMap::<RepoSlug, Option<PullRequestCounts>>::new();
    let mut auth_by_host = HashMap::<String, Option<ForgeAuth>>::new();
    let mut results = Vec::with_capacity(items.len());

    for item in items {
        let remote_url = remote_urls
            .entry(item.resolved_identity_cwd.clone())
            .or_insert_with(|| {
                crate::workspace::git_remote_url_for_checkout(&item.resolved_identity_cwd)
            })
            .clone();

        let counts = remote_url
            .as_deref()
            .and_then(RepoSlug::parse)
            .and_then(|slug| {
                if let Some(cached) = counts_by_slug.get(&slug) {
                    return *cached;
                }
                let auth = auth_by_host
                    .entry(slug.host.clone())
                    .or_insert_with(|| crate::forge::resolve_auth(&slug.host))
                    .clone();
                let fetched = auth
                    .as_ref()
                    .and_then(|auth| crate::forge::fetch_pull_request_counts(&slug, auth));
                counts_by_slug.insert(slug, fetched);
                fetched
            });

        results.push(WorkspacePullRequests {
            workspace_id: item.workspace_id,
            remote_url,
            counts,
        });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn test_app(config: &crate::config::Config) -> App {
        App::new(
            config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn config_with_space_tokens(
        tokens: Vec<crate::config::SpaceSidebarToken>,
    ) -> crate::config::Config {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![tokens];
        config
    }

    #[test]
    fn no_refresh_without_a_sidebar_consumer() {
        let config = config_with_space_tokens(vec![crate::config::SpaceSidebarToken::Workspace]);
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));

        assert_eq!(app.pull_request_refresh_deadline(), None);

        app.start_pull_request_refresh_if_due(Instant::now());
        assert!(!app.pull_request_refresh_in_flight);
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn the_pull_requests_token_is_what_arms_the_refresh() {
        let config = config_with_space_tokens(vec![crate::config::SpaceSidebarToken::PullRequests]);
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));

        assert!(app.pull_request_refresh_deadline().is_some());
    }

    #[test]
    fn a_styled_pull_requests_token_still_arms_the_refresh() {
        // `parts()` unwraps the style wrapper; reading the outer token directly
        // would leave a styled row fetching nothing and rendering nothing.
        let config = config_with_space_tokens(vec![crate::config::SpaceSidebarToken::Styled {
            token: Box::new(crate::config::SpaceSidebarToken::PullRequests),
            style: crate::config::SidebarTokenStyle::default(),
        }]);
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));

        assert!(app.pull_request_refresh_deadline().is_some());
    }

    #[test]
    fn an_in_flight_refresh_is_not_started_again() {
        let config = config_with_space_tokens(vec![crate::config::SpaceSidebarToken::PullRequests]);
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        app.pull_request_refresh_in_flight = true;

        assert_eq!(app.pull_request_refresh_deadline(), None);
    }

    #[test]
    fn results_apply_to_the_workspace_they_name() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("test"));
        let workspace_id = app.state.workspaces[0].id.clone();
        app.pull_request_refresh_in_flight = true;

        app.handle_internal_event(AppEvent::PullRequestsRefreshed {
            results: vec![WorkspacePullRequests {
                workspace_id,
                remote_url: Some("git@github.com:owner/name.git".into()),
                counts: Some(PullRequestCounts {
                    open: 3,
                    draft: 1,
                    review_requested: 2,
                }),
            }],
        });

        assert!(!app.pull_request_refresh_in_flight);
        let counts = app.state.workspaces[0]
            .pull_requests()
            .expect("counts should be applied");
        assert_eq!(counts.open, 3);
        assert_eq!(counts.review_requested, 2);
    }

    #[test]
    fn a_failed_fetch_keeps_the_counts_already_on_screen() {
        // "We could not ask" is not "nothing is open". Blanking the row on a
        // dropped network would read as the captain's queue having emptied.
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("test"));
        let workspace_id = app.state.workspaces[0].id.clone();
        app.state.workspaces[0].cached_pull_requests = Some(PullRequestCounts {
            open: 2,
            draft: 0,
            review_requested: 1,
        });

        app.handle_internal_event(AppEvent::PullRequestsRefreshed {
            results: vec![WorkspacePullRequests {
                workspace_id,
                remote_url: Some("git@github.com:owner/name.git".into()),
                counts: None,
            }],
        });

        assert_eq!(
            app.state.workspaces[0].pull_requests().map(|c| c.open),
            Some(2)
        );
    }

    #[test]
    fn losing_the_remote_clears_the_counts() {
        // A checkout that no longer names a forge repository has no pull requests
        // to report, which is a different thing from a fetch that failed.
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("test"));
        let workspace_id = app.state.workspaces[0].id.clone();
        app.state.workspaces[0].cached_pull_requests = Some(PullRequestCounts {
            open: 2,
            draft: 0,
            review_requested: 1,
        });

        app.handle_internal_event(AppEvent::PullRequestsRefreshed {
            results: vec![WorkspacePullRequests {
                workspace_id,
                remote_url: None,
                counts: None,
            }],
        });

        assert_eq!(app.state.workspaces[0].pull_requests(), None);
    }
}
