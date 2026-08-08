use super::AgentPanelEntry;
use crate::config::{
    AgentSidebarToken, AgentsSidebarConfig, SidebarTokenStyle, SpaceSidebarToken,
    SpacesSidebarConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedToken {
    pub kind: ResolvedTokenKind,
    pub style: SidebarTokenStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedTokenKind {
    StateIcon,
    StateText(String),
    StateAge(String),
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
    TerminalTitle(String),
    Branch(String),
    GitStatus {
        ahead: usize,
        behind: usize,
    },
    GitDirty(crate::workspace::GitDirtyCounts),
    PullRequests {
        open: usize,
        review_requested: usize,
    },
    /// The 5-hour (session) quota window, already formatted by
    /// [`crate::quota::format_readout`]: `session 42%, resets in 2h`.
    QuotaSession(String),
    /// The 7-day (weekly) quota window, already formatted the same way:
    /// `week 15%, resets in 2d`. A distinct variant from `QuotaSession`
    /// rather than one `Quota(Window, String)` so the two can never be drawn
    /// with the same styling by accident — see the readability requirement
    /// this pair exists to satisfy.
    QuotaWeekly(String),
    /// This Space's quality streak, already decayed to now and banded:
    /// `streak 23.8 steady`. See [`crate::quality_streak`].
    ///
    /// The band travels beside the text rather than being re-derived from it,
    /// because it is what picks the colour the flame draws in and a renderer
    /// re-parsing a word out of a formatted line is a second source of truth.
    Streak {
        band: crate::quality_streak::FlameBand,
        text: String,
    },
    Custom(String),
}

impl ResolvedToken {
    fn new(kind: ResolvedTokenKind, style: SidebarTokenStyle) -> Self {
        Self { kind, style }
    }

    #[cfg(test)]
    pub(super) fn unstyled(kind: ResolvedTokenKind) -> Self {
        Self::new(kind, SidebarTokenStyle::default())
    }
}

pub(super) fn agent_rows(
    config: &AgentsSidebarConfig,
    entry: &AgentPanelEntry,
    state_text: &str,
    state_age: Option<std::time::Duration>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows_for_agent(entry.agent)
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        AgentSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        AgentSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(state_text.to_string()))
                        }
                        // Elides when the runtime has no stamp, exactly like a
                        // missing custom token. A row that cannot say how long
                        // is better off saying nothing than saying `0s`.
                        AgentSidebarToken::StateAge => state_age
                            .map(crate::state_age::format)
                            .map(ResolvedTokenKind::StateAge),
                        AgentSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(entry.primary_label.clone()))
                        }
                        AgentSidebarToken::Tab => {
                            entry.primary_tab_label.clone().map(ResolvedTokenKind::Tab)
                        }
                        AgentSidebarToken::Pane => {
                            entry.pane_label.clone().map(ResolvedTokenKind::Pane)
                        }
                        AgentSidebarToken::Agent => {
                            entry.agent_label.clone().map(ResolvedTokenKind::Agent)
                        }
                        AgentSidebarToken::TerminalTitle => entry
                            .terminal_title
                            .clone()
                            .map(ResolvedTokenKind::TerminalTitle),
                        AgentSidebarToken::TerminalTitleStripped => entry
                            .terminal_title_stripped
                            .clone()
                            .map(ResolvedTokenKind::TerminalTitle),
                        AgentSidebarToken::Custom(name) => entry
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        AgentSidebarToken::Styled { .. } => None,
                    }?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(super) struct SpaceTokenContext<'a> {
    pub workspace: &'a str,
    pub branch: Option<&'a str>,
    pub state_text: &'a str,
    /// How long the pane behind this row's state icon has held that state.
    pub state_age: Option<std::time::Duration>,
    pub ahead_behind: Option<(usize, usize)>,
    pub dirty: Option<crate::workspace::GitDirtyCounts>,
    pub pull_requests: Option<crate::forge::PullRequestCounts>,
    /// Terminal titles of the pane that also decides this space's state icon.
    pub terminal_title: Option<&'a str>,
    pub terminal_title_stripped: Option<&'a str>,
    pub tokens: &'a std::collections::HashMap<String, String>,
    pub suppress_git_details: bool,
    /// The clock `quota_session`/`quota_weekly` reset countdowns are rendered
    /// against — [`crate::app::state::AppState::wall_now`], threaded through
    /// rather than read here so resolving a row stays a pure function of its
    /// inputs.
    pub wall_now: std::time::SystemTime,
}

pub(super) fn space_rows(
    config: &SpacesSidebarConfig,
    context: SpaceTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        SpaceSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        SpaceSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(context.state_text.to_string()))
                        }
                        SpaceSidebarToken::StateAge => context
                            .state_age
                            .map(crate::state_age::format)
                            .map(ResolvedTokenKind::StateAge),
                        SpaceSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(context.workspace.to_string()))
                        }
                        SpaceSidebarToken::Branch if !context.suppress_git_details => context
                            .branch
                            .map(|branch| ResolvedTokenKind::Branch(branch.to_string())),
                        SpaceSidebarToken::Branch => None,
                        SpaceSidebarToken::GitStatus if !context.suppress_git_details => context
                            .ahead_behind
                            .filter(|(ahead, behind)| *ahead > 0 || *behind > 0)
                            .map(|(ahead, behind)| ResolvedTokenKind::GitStatus { ahead, behind }),
                        SpaceSidebarToken::GitStatus => None,
                        // A clean tree renders nothing at all rather than a zero:
                        // this counter exists to say work is outstanding, and
                        // "nothing outstanding" is best said with silence.
                        SpaceSidebarToken::GitDirty if !context.suppress_git_details => context
                            .dirty
                            .filter(|dirty| !dirty.is_clean())
                            .map(ResolvedTokenKind::GitDirty),
                        SpaceSidebarToken::GitDirty => None,
                        SpaceSidebarToken::PullRequests if !context.suppress_git_details => context
                            .pull_requests
                            .filter(|counts| counts.open > 0)
                            .map(|counts| ResolvedTokenKind::PullRequests {
                                open: counts.open,
                                review_requested: counts.review_requested,
                            }),
                        SpaceSidebarToken::PullRequests => None,
                        SpaceSidebarToken::QuotaSession => context
                            .tokens
                            .get(crate::quota::SESSION_TOKEN)
                            .and_then(|raw| crate::quota::parse(raw))
                            .map(|readout| {
                                ResolvedTokenKind::QuotaSession(crate::quota::format_readout(
                                    "session",
                                    &readout,
                                    context.wall_now,
                                ))
                            }),
                        SpaceSidebarToken::QuotaWeekly => context
                            .tokens
                            .get(crate::quota::WEEKLY_TOKEN)
                            .and_then(|raw| crate::quota::parse(raw))
                            .map(|readout| {
                                ResolvedTokenKind::QuotaWeekly(crate::quota::format_readout(
                                    "week",
                                    &readout,
                                    context.wall_now,
                                ))
                            }),
                        // Decayed here, on this render, from the instant the
                        // publisher stamped into the token — never from a
                        // counter Herdr keeps, which would have stood still
                        // while Herdr was stopped and redrawn a stale score
                        // at full heat. See [`crate::quality_streak`].
                        SpaceSidebarToken::Streak => context
                            .tokens
                            .get(crate::quality_streak::STREAK_TOKEN)
                            .and_then(|raw| crate::quality_streak::parse(raw))
                            .map(|readout| {
                                let half_lives = crate::quality_streak::half_lives(
                                    context
                                        .tokens
                                        .get(crate::quality_streak::HALF_LIFE_TOKEN)
                                        .map(String::as_str),
                                );
                                let value = crate::quality_streak::decayed(
                                    readout,
                                    half_lives,
                                    context.wall_now,
                                );
                                let band = crate::quality_streak::FlameBand::of(value);
                                ResolvedTokenKind::Streak {
                                    band,
                                    text: crate::quality_streak::format_readout(value, band),
                                }
                            }),
                        SpaceSidebarToken::TerminalTitle => context
                            .terminal_title
                            .map(|title| ResolvedTokenKind::TerminalTitle(title.to_string())),
                        SpaceSidebarToken::TerminalTitleStripped => context
                            .terminal_title_stripped
                            .map(|title| ResolvedTokenKind::TerminalTitle(title.to_string())),
                        SpaceSidebarToken::Custom(name) => context
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        SpaceSidebarToken::Styled { .. } => None,
                    }?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

/// Which lane of uncommitted work a `git_dirty` component counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirtyLane {
    Staged,
    Unstaged,
    Untracked,
}

/// The pieces a `git_dirty` token draws, in order, zero lanes omitted.
///
/// Width measurement and painting both read this one function, so the two can
/// never disagree about how wide the token is. The marks are git's own porcelain
/// vocabulary rather than invented glyphs, so they survive any font and read
/// correctly to anyone who has run `git status`.
pub(super) fn git_dirty_parts(dirty: crate::workspace::GitDirtyCounts) -> Vec<(DirtyLane, String)> {
    [
        (DirtyLane::Staged, '+', dirty.staged),
        (DirtyLane::Unstaged, '~', dirty.unstaged),
        (DirtyLane::Untracked, '?', dirty.untracked),
    ]
    .into_iter()
    .filter(|(_, _, count)| *count > 0)
    .map(|(lane, mark, count)| (lane, format!("{mark}{count}")))
    .collect()
}

/// The text a `pull_requests` token draws.
///
/// Spelled `pr` rather than drawn as a glyph on purpose: a misread counter is
/// silent, and there is no established one-cell mark for "pull request" the way
/// there is for ahead/behind.
pub(super) fn pull_requests_text(open: usize) -> String {
    format!("pr{open}")
}

pub(super) fn separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    // An elapsed time straight after the state it belongs to is not a second
    // value on the row, it is the rest of the same phrase: `working 47m`. A
    // middot there reads as two facts and costs two columns to say so. After
    // anything else the age is its own fact and keeps the decorated separator.
    let age_qualifies_the_state = matches!(previous.kind, ResolvedTokenKind::StateText(_))
        && matches!(current.kind, ResolvedTokenKind::StateAge(_));
    if matches!(previous.kind, ResolvedTokenKind::StateIcon)
        || matches!(
            current.kind,
            ResolvedTokenKind::GitStatus { .. }
                | ResolvedTokenKind::GitDirty(_)
                | ResolvedTokenKind::PullRequests { .. }
        )
        || age_qualifies_the_state
    {
        " "
    } else {
        " · "
    }
}

/// The separator to draw when the row cannot fit its content anyway.
///
/// `" · "` spends three columns to draw one glyph of decoration. That is a fine
/// trade while a row still fits, and a bad one the moment those columns are
/// coming out of a token that is already being truncated: a grouped child row
/// in a 23-column sidebar has 16 columns to work with, so the middot and its
/// padding are nearly a fifth of the row and come straight out of the one token
/// the reader cares about. On an overflowing row the dot is dropped and its two
/// padding columns go back to the flexible tokens; rows that fit keep the dot
/// and render exactly as before.
pub(super) fn compact_separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    match separator(previous, current) {
        " · " => " ",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentSidebarToken, SpaceSidebarToken};
    use crate::detect::AgentState;

    #[test]
    fn an_elapsed_time_reads_as_part_of_the_state_it_qualifies() {
        let state = ResolvedToken::unstyled(ResolvedTokenKind::StateText("working".into()));
        let age = ResolvedToken::unstyled(ResolvedTokenKind::StateAge("47m".into()));
        let doing = ResolvedToken::unstyled(ResolvedTokenKind::Custom("doing".into()));

        // `working 47m` is one phrase; `working · 47m` reads as two facts and
        // spends two more columns saying it.
        assert_eq!(separator(&state, &age), " ");
        assert_eq!(compact_separator(&state, &age), " ");

        // Anywhere else the age is a value like any other and keeps the dot.
        assert_eq!(separator(&doing, &age), " · ");
        assert_eq!(separator(&age, &doing), " · ");
    }

    #[test]
    fn a_state_age_token_elides_when_the_runtime_has_no_stamp() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::StateAge,
            ]],
            ..Default::default()
        };

        // No stamp: the row must not invent `0s`, which would read as a state
        // that just changed when in fact nothing is known about when it did.
        assert_eq!(
            agent_rows(&config, &entry, "working", None),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::StateText(
                "working".into()
            ))]]
        );

        assert_eq!(
            agent_rows(
                &config,
                &entry,
                "working",
                Some(std::time::Duration::from_secs(47 * 60))
            ),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateText("working".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::StateAge("47m".into())),
            ]]
        );
    }

    /// The whole point of the feature: two rows in the same state, told apart.
    #[test]
    fn minute_one_and_minute_ninety_render_differently() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::StateAge]],
            ..Default::default()
        };
        let render = |secs| {
            agent_rows(
                &config,
                &entry,
                "working",
                Some(std::time::Duration::from_secs(secs)),
            )
        };
        assert_ne!(render(60), render(90 * 60));
    }

    #[test]
    fn a_space_row_state_age_follows_the_same_missing_stamp_rule() {
        let tokens = std::collections::HashMap::new();
        let config = SpacesSidebarConfig {
            rows: vec![vec![
                SpaceSidebarToken::StateText,
                SpaceSidebarToken::StateAge,
            ]],
            ..Default::default()
        };
        let context = |state_age| SpaceTokenContext {
            workspace: "repo",
            branch: None,
            state_text: "blocked",
            state_age,
            ahead_behind: None,
            dirty: None,
            pull_requests: None,
            terminal_title: None,
            terminal_title_stripped: None,
            tokens: &tokens,
            suppress_git_details: false,
            wall_now: std::time::SystemTime::UNIX_EPOCH,
        };

        assert_eq!(
            space_rows(&config, context(None)),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::StateText(
                "blocked".into()
            ))]]
        );
        assert_eq!(
            space_rows(
                &config,
                context(Some(std::time::Duration::from_secs(3 * 60 * 60)))
            )[0][1],
            ResolvedToken::unstyled(ResolvedTokenKind::StateAge("3h".into()))
        );
    }

    fn outstanding_work_context<'a>(
        tokens: &'a std::collections::HashMap<String, String>,
        dirty: Option<crate::workspace::GitDirtyCounts>,
        pull_requests: Option<crate::forge::PullRequestCounts>,
        suppress_git_details: bool,
    ) -> SpaceTokenContext<'a> {
        SpaceTokenContext {
            workspace: "repo",
            branch: None,
            state_text: "idle",
            state_age: None,
            ahead_behind: None,
            dirty,
            pull_requests,
            terminal_title: None,
            terminal_title_stripped: None,
            tokens,
            suppress_git_details,
            wall_now: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    fn outstanding_work_config() -> SpacesSidebarConfig {
        SpacesSidebarConfig {
            rows: vec![vec![
                SpaceSidebarToken::GitDirty,
                SpaceSidebarToken::PullRequests,
            ]],
            ..Default::default()
        }
    }

    #[test]
    fn nothing_outstanding_renders_no_row_at_all() {
        // A clean tree and an empty queue must not draw `+0` and `pr0`; the row
        // exists to say work is waiting, so silence is the correct rendering.
        let tokens = std::collections::HashMap::new();
        let rows = space_rows(
            &outstanding_work_config(),
            outstanding_work_context(
                &tokens,
                Some(crate::workspace::GitDirtyCounts::default()),
                Some(crate::forge::PullRequestCounts::default()),
                false,
            ),
        );

        assert!(rows.is_empty(), "clean state should render nothing");
    }

    #[test]
    fn outstanding_work_renders_both_counters() {
        let tokens = std::collections::HashMap::new();
        let dirty = crate::workspace::GitDirtyCounts {
            staged: 1,
            unstaged: 2,
            untracked: 3,
        };
        let rows = space_rows(
            &outstanding_work_config(),
            outstanding_work_context(
                &tokens,
                Some(dirty),
                Some(crate::forge::PullRequestCounts {
                    open: 4,
                    draft: 1,
                    review_requested: 2,
                    ..Default::default()
                }),
                false,
            ),
        );

        assert_eq!(
            rows,
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::GitDirty(dirty)),
                ResolvedToken::unstyled(ResolvedTokenKind::PullRequests {
                    open: 4,
                    review_requested: 2,
                }),
            ]]
        );
    }

    #[test]
    fn dirty_parts_omit_empty_lanes_and_keep_gits_own_marks() {
        assert_eq!(
            git_dirty_parts(crate::workspace::GitDirtyCounts {
                staged: 0,
                unstaged: 2,
                untracked: 0,
            }),
            vec![(DirtyLane::Unstaged, "~2".to_string())]
        );
        assert_eq!(
            git_dirty_parts(crate::workspace::GitDirtyCounts {
                staged: 1,
                unstaged: 2,
                untracked: 3,
            }),
            vec![
                (DirtyLane::Staged, "+1".to_string()),
                (DirtyLane::Unstaged, "~2".to_string()),
                (DirtyLane::Untracked, "?3".to_string()),
            ]
        );
    }

    #[test]
    fn a_grouped_child_row_suppresses_both_counters() {
        // Indented worktree children already inherit the parent's Git details;
        // repeating them per child would spend scarce columns saying the same
        // thing, and the pull request count is a whole-repository fact anyway.
        let tokens = std::collections::HashMap::new();
        let rows = space_rows(
            &outstanding_work_config(),
            outstanding_work_context(
                &tokens,
                Some(crate::workspace::GitDirtyCounts {
                    staged: 1,
                    unstaged: 0,
                    untracked: 0,
                }),
                Some(crate::forge::PullRequestCounts {
                    open: 2,
                    draft: 0,
                    review_requested: 0,
                    ..Default::default()
                }),
                true,
            ),
        );

        assert!(rows.is_empty());
    }

    #[test]
    fn counters_hug_the_token_before_them_like_the_ahead_behind_pair() {
        let workspace = ResolvedToken::unstyled(ResolvedTokenKind::Workspace("repo".into()));
        let dirty = ResolvedToken::unstyled(ResolvedTokenKind::GitDirty(
            crate::workspace::GitDirtyCounts {
                staged: 1,
                unstaged: 0,
                untracked: 0,
            },
        ));
        let pulls = ResolvedToken::unstyled(ResolvedTokenKind::PullRequests {
            open: 2,
            review_requested: 0,
        });

        assert_eq!(separator(&workspace, &dirty), " ");
        assert_eq!(separator(&dirty, &pulls), " ");
    }

    #[test]
    fn compaction_only_drops_the_middot_never_the_icon_gap() {
        let icon = ResolvedToken::unstyled(ResolvedTokenKind::StateIcon);
        let doing = ResolvedToken::unstyled(ResolvedTokenKind::Custom("doing".into()));
        let context = ResolvedToken::unstyled(ResolvedTokenKind::Custom("9%".into()));

        // The icon already sits one space from its row; there is nothing there
        // to reclaim and squeezing it would glue the dot to the text.
        assert_eq!(separator(&icon, &doing), " ");
        assert_eq!(compact_separator(&icon, &doing), " ");

        // Between two value tokens the middot is pure decoration.
        assert_eq!(separator(&doing, &context), " · ");
        assert_eq!(compact_separator(&doing, &context), " ");
    }

    fn entry() -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            primary_label: "repo".into(),
            primary_tab_label: None,
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: Some("pi".into()),
            agent_kind_label: Some("pi".into()),
            agent: Some(crate::detect::Agent::Pi),
            state: AgentState::Working,
            seen: true,
            last_agent_state_change_seq: None,
            last_agent_state_change_at: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
            agent_name: None,
            owner: None,
            depth: 0,
            relation: crate::app::agent_tree::AgentRelation::FirstMate,
            is_last_child: true,
            ancestors_continue: Vec::new(),
            delegated_in_space: false,
        }
    }

    #[test]
    fn missing_custom_tokens_elide_rows_and_separators() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Custom("missing".into()),
                ],
                vec![AgentSidebarToken::Custom("missing".into())],
                vec![AgentSidebarToken::Agent],
            ],
            ..Default::default()
        };

        let rows = agent_rows(&config, &entry, "working", None);

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::StateIcon)]
        );
        assert_eq!(
            rows[1],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "pi".into()
            ))]
        );
    }

    #[test]
    fn state_text_and_arbitrary_values_are_independent_tokens() {
        let mut entry = entry();
        entry
            .tokens
            .insert("summary".into(), "reviewing auth".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Custom("summary".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "deep in the mines", None),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateText("deep in the mines".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("reviewing auth".into())),
            ]]
        );
    }

    #[test]
    fn terminal_title_builtins_are_distinct_from_custom_tokens() {
        let mut entry = entry();
        entry.terminal_title = Some("⠋ raw title".into());
        entry.terminal_title_stripped = Some("raw title".into());
        entry
            .tokens
            .insert("terminal_title".into(), "custom title".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "working", None),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("⠋ raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("custom title".into())),
            ]]
        );
    }

    #[test]
    fn known_agent_override_replaces_default_rows() {
        let mut config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::Workspace]],
            ..Default::default()
        };
        config
            .rows_by_agent
            .insert("pi".into(), vec![vec![AgentSidebarToken::Agent]]);
        let mut pi = entry();
        pi.agent_label = Some("renamed pi".into());

        assert_eq!(
            agent_rows(&config, &pi, "working", None),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "renamed pi".into()
            ))]]
        );

        pi.agent = None;
        assert_eq!(
            agent_rows(&config, &pi, "working", None),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                "repo".into()
            ))]]
        );
    }

    #[test]
    fn grouped_children_suppress_all_builtin_git_details() {
        let config = SpacesSidebarConfig::default();

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    state_age: None,
                    workspace: "feature",
                    branch: Some("worktree/feature"),
                    state_text: "idle",
                    ahead_behind: Some((2, 1)),
                    dirty: None,
                    pull_requests: None,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: true,
                    wall_now: std::time::SystemTime::UNIX_EPOCH,
                },
            ),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Workspace("feature".into())),
            ]]
        );
    }

    #[test]
    fn space_terminal_title_tokens_resolve_raw_and_stripped_values() {
        let config = SpacesSidebarConfig {
            rows: vec![
                vec![SpaceSidebarToken::Workspace],
                vec![
                    SpaceSidebarToken::TerminalTitle,
                    SpaceSidebarToken::TerminalTitleStripped,
                ],
            ],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    state_age: None,
                    workspace: "repo",
                    branch: None,
                    state_text: "working",
                    ahead_behind: None,
                    dirty: None,
                    pull_requests: None,
                    terminal_title: Some("⠋ running tests"),
                    terminal_title_stripped: Some("running tests"),
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: false,
                    wall_now: std::time::SystemTime::UNIX_EPOCH,
                },
            ),
            vec![
                vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                    "repo".into()
                ))],
                vec![
                    ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle(
                        "⠋ running tests".into()
                    )),
                    ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle(
                        "running tests".into()
                    )),
                ],
            ]
        );
    }

    #[test]
    fn missing_space_terminal_titles_elide_their_row() {
        let config = SpacesSidebarConfig {
            rows: vec![
                vec![SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::TerminalTitleStripped],
            ],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    state_age: None,
                    workspace: "repo",
                    branch: None,
                    state_text: "unknown",
                    ahead_behind: None,
                    dirty: None,
                    pull_requests: None,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: false,
                    wall_now: std::time::SystemTime::UNIX_EPOCH,
                },
            ),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                "repo".into()
            ))]]
        );
    }

    #[test]
    fn grouped_space_children_keep_terminal_titles() {
        let config = SpacesSidebarConfig {
            rows: vec![vec![
                SpaceSidebarToken::Workspace,
                SpaceSidebarToken::Branch,
                SpaceSidebarToken::TerminalTitleStripped,
            ]],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    state_age: None,
                    workspace: "feature",
                    branch: Some("worktree/feature"),
                    state_text: "working",
                    ahead_behind: None,
                    dirty: None,
                    pull_requests: None,
                    terminal_title: Some("⠋ running tests"),
                    terminal_title_stripped: Some("running tests"),
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: true,
                    wall_now: std::time::SystemTime::UNIX_EPOCH,
                },
            ),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::Workspace("feature".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("running tests".into())),
            ]]
        );
    }

    #[test]
    fn workspace_custom_token_can_replace_git_specific_details() {
        let tokens = std::collections::HashMap::from([("jj_status".into(), "2 changes".into())]);
        let config = SpacesSidebarConfig {
            rows: vec![vec![SpaceSidebarToken::Custom("jj_status".into())]],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    state_age: None,
                    workspace: "repo",
                    branch: None,
                    state_text: "idle",
                    ahead_behind: None,
                    dirty: None,
                    pull_requests: None,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    tokens: &tokens,
                    suppress_git_details: false,
                    wall_now: std::time::SystemTime::UNIX_EPOCH,
                },
            ),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Custom(
                "2 changes".into()
            ))]]
        );
    }

    /// 2026-08-08T00:00:00Z, cross-checked against
    /// `date -u -d 2026-08-08T00:00:00Z +%s`.
    const WALL_NOW_SECS: u64 = 1_786_147_200;

    fn streak_rows(tokens: &std::collections::HashMap<String, String>) -> Vec<Vec<ResolvedToken>> {
        let config = SpacesSidebarConfig {
            rows: vec![vec![SpaceSidebarToken::Streak]],
            ..Default::default()
        };
        space_rows(
            &config,
            SpaceTokenContext {
                workspace: "repo",
                branch: None,
                state_text: "idle",
                state_age: None,
                ahead_behind: None,
                dirty: None,
                pull_requests: None,
                terminal_title: None,
                terminal_title_stripped: None,
                tokens,
                suppress_git_details: false,
                wall_now: std::time::SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(WALL_NOW_SECS),
            },
        )
    }

    fn streak_tokens(
        published_days_ago: f64,
        score: f64,
    ) -> std::collections::HashMap<String, String> {
        let published_at = WALL_NOW_SECS - (published_days_ago * 86_400.0) as u64;
        std::collections::HashMap::from([
            ("streak".into(), format!("{score:.2}@{published_at}")),
            ("streak_hl".into(), "5/10".into()),
        ])
    }

    /// The whole point of the token carrying its own instant: a score
    /// published two days ago has to arrive two days colder on the first frame
    /// after a cold start, with nothing having ticked in between.
    #[test]
    fn a_streak_is_decayed_at_read_time_so_a_cold_start_never_redraws_it_hot() {
        assert_eq!(
            streak_rows(&streak_tokens(0.0, 45.0)),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Streak {
                band: crate::quality_streak::FlameBand::Hot,
                text: "streak 45.0 hot".into(),
            })]],
            "read at the instant it was published, a score is itself"
        );
        assert_eq!(
            streak_rows(&streak_tokens(2.0, 45.0)),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Streak {
                band: crate::quality_streak::FlameBand::Steady,
                text: "streak 34.1 steady".into(),
            })]],
            "two days of a five-day half-life took the same token out of `hot`"
        );
        assert_eq!(
            streak_rows(&streak_tokens(20.0, 45.0)),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Streak {
                band: crate::quality_streak::FlameBand::Ember,
                text: "streak 2.8 ember".into(),
            })]],
            "and a fortnight of silence is an ember, not a fire"
        );
    }

    /// Every band a row can be in, reached through the real token path.
    #[test]
    fn every_flame_band_is_reachable_from_a_published_token() {
        let banded = |score: f64| match &streak_rows(&streak_tokens(0.0, score))[0][0].kind {
            ResolvedTokenKind::Streak { band, text } => (*band, text.clone()),
            other => panic!("expected a streak token, got {other:?}"),
        };
        use crate::quality_streak::FlameBand;
        assert_eq!(banded(-4.0), (FlameBand::Cold, "streak -4.0 cold".into()));
        assert_eq!(banded(4.0), (FlameBand::Ember, "streak 4.0 ember".into()));
        assert_eq!(banded(12.0), (FlameBand::Low, "streak 12.0 low".into()));
        assert_eq!(
            banded(24.0),
            (FlameBand::Steady, "streak 24.0 steady".into())
        );
        assert_eq!(banded(41.0), (FlameBand::Hot, "streak 41.0 hot".into()));
    }

    /// A Space nobody publishes a streak for draws no streak, and a malformed
    /// one draws nothing rather than an undecayed number.
    #[test]
    fn a_row_with_no_usable_streak_token_draws_no_streak() {
        assert!(streak_rows(&std::collections::HashMap::new()).is_empty());
        assert!(streak_rows(&std::collections::HashMap::from([(
            "streak".into(),
            "23.75".into()
        )]))
        .is_empty());
    }

    /// The half-lives are a knob, not the fact: a publisher that omits them
    /// still gets a correctly decayed readout.
    #[test]
    fn a_missing_half_life_token_falls_back_rather_than_blanking_the_readout() {
        let mut tokens = streak_tokens(5.0, 40.0);
        tokens.remove("streak_hl");
        assert_eq!(
            streak_rows(&tokens),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Streak {
                band: crate::quality_streak::FlameBand::Steady,
                text: "streak 20.0 steady".into(),
            })]],
            "five days with no published half-life is one default win half-life"
        );
    }

    #[test]
    fn quota_windows_resolve_from_their_own_tokens_and_read_as_two_distinct_facts() {
        let tokens = std::collections::HashMap::from([
            ("quota_5h".into(), "42@2026-08-08T02:00:00Z".into()),
            ("quota_7d".into(), "15@2026-08-10T00:00:00Z".into()),
        ]);
        let config = SpacesSidebarConfig {
            rows: vec![
                vec![SpaceSidebarToken::QuotaSession],
                vec![SpaceSidebarToken::QuotaWeekly],
            ],
            ..Default::default()
        };
        // 2026-08-08T00:00:00Z, cross-checked against
        // `date -u -d 2026-08-08T00:00:00Z +%s`: 2h before the session window
        // resets and exactly 2 days before the weekly one does.
        let wall_now =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_786_147_200);

        let rows = space_rows(
            &config,
            SpaceTokenContext {
                workspace: "repo",
                branch: None,
                state_text: "idle",
                state_age: None,
                ahead_behind: None,
                dirty: None,
                pull_requests: None,
                terminal_title: None,
                terminal_title_stripped: None,
                tokens: &tokens,
                suppress_git_details: false,
                wall_now,
            },
        );

        assert_eq!(
            rows,
            vec![
                vec![ResolvedToken::unstyled(ResolvedTokenKind::QuotaSession(
                    "session 42%, resets in 2h".into()
                ))],
                vec![ResolvedToken::unstyled(ResolvedTokenKind::QuotaWeekly(
                    "week 15%, resets in 2d".into()
                ))],
            ],
            "the two windows must carry different label words (session vs week) \
             and different resolved-token variants, so no downstream renderer \
             can draw them identically",
        );
    }

    #[test]
    fn a_quota_row_elides_when_its_own_token_is_absent() {
        let tokens = std::collections::HashMap::from([(
            "quota_5h".into(),
            "42@2026-08-08T02:00:00Z".into(),
        )]);
        let config = SpacesSidebarConfig {
            rows: vec![
                vec![SpaceSidebarToken::QuotaSession],
                vec![SpaceSidebarToken::QuotaWeekly],
            ],
            ..Default::default()
        };

        let rows = space_rows(
            &config,
            SpaceTokenContext {
                workspace: "repo",
                branch: None,
                state_text: "idle",
                state_age: None,
                ahead_behind: None,
                dirty: None,
                pull_requests: None,
                terminal_title: None,
                terminal_title_stripped: None,
                tokens: &tokens,
                suppress_git_details: false,
                wall_now: std::time::SystemTime::UNIX_EPOCH,
            },
        );

        assert_eq!(
            rows.len(),
            1,
            "the weekly row has no token published, so it renders nothing"
        );
    }
}
