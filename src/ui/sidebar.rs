mod tokens;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use self::tokens::{ResolvedToken, ResolvedTokenKind, SpaceTokenContext};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{state_dot, state_label, state_label_color};
use super::text::{display_width, middle_elide, truncate_end};
use crate::app::agent_view::AgentViewHidden;
use crate::app::relation_signal::RelationSignalPhase;
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::config::SidebarTokenEmphasis;
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: Option<String>,
    pub agent_kind_label: Option<String>,
    pub agent: Option<crate::detect::Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub last_agent_state_change_at: Option<std::time::Instant>,
    pub state_labels: std::collections::HashMap<String, String>,
    pub tokens: std::collections::HashMap<String, String>,
    /// This pane's own handle, the thing another pane's `owner` token names.
    pub agent_name: Option<String>,
    /// The `owner` metadata token, naming the pane that owns this one.
    pub owner: Option<String>,
    /// Depth in the ownership tree, stamped by
    /// [`crate::app::agent_tree::arrange_agent_tree`]. 0 is a root.
    pub depth: u8,
    /// Where this pane sits in the ownership tree.
    pub relation: crate::app::agent_tree::AgentRelation,
    /// Whether this is the last child at its own depth, picking `└` over `├`.
    pub is_last_child: bool,
    /// For each ancestor level, whether that level still has a sibling below,
    /// which is what decides between a `│` continuation and blank space.
    pub ancestors_continue: Vec<bool>,
}

#[cfg(test)]
impl AgentPanelEntry {
    /// Minimal entry for tree and layout tests, which care only about the
    /// name/owner pair and the fields the arranger stamps.
    pub(crate) fn test_new(agent_name: &str) -> Self {
        Self {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::alloc(),
            primary_label: String::new(),
            primary_tab_label: None,
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: Some(agent_name.to_string()),
            agent_kind_label: None,
            agent: None,
            state: AgentState::Unknown,
            seen: true,
            last_agent_state_change_seq: None,
            last_agent_state_change_at: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
            agent_name: Some(agent_name.to_string()),
            owner: None,
            depth: 0,
            relation: crate::app::agent_tree::AgentRelation::FirstMate,
            is_last_child: true,
            ancestors_continue: Vec::new(),
        }
    }
}

/// The sidebar's one content column, everything left of the divider bar.
///
/// The sidebar is a single panel: the Spaces tree owns the whole column. There
/// is no second section and so no section divider to size — the Agents panel it
/// used to separate no longer exists, because every mate, worker and sub agent
/// is a row in the one tree below.
pub(crate) fn sidebar_content_rect(area: Rect) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return Rect::default();
    }
    content
}

/// Header label candidates for an active view, widest first.
///
/// A view that hides rows has to say so even in an 18-column surface. Every
/// candidate keeps the hidden count; only the owner name and the word "hidden"
/// are given up as space runs out.
fn agent_view_label_candidates(app: &AppState, hidden: AgentViewHidden) -> Vec<String> {
    let Some(tier) = app.agent_views.active_tier() else {
        return Vec::new();
    };
    let owner = tier.label();
    let name = app
        .agent_views
        .active()
        .and_then(|view| view.label.as_deref());
    let qualified = match name {
        Some(name) => format!("{owner}:{name}"),
        None => owner.to_string(),
    };
    let short = name.unwrap_or(owner).to_string();

    let mut candidates = if hidden.any() {
        let mark = if hidden.hidden_blocked > 0 { " !" } else { "" };
        let count = hidden.hidden;
        vec![
            format!("{qualified} · {count} hidden{mark}"),
            format!("{short} · {count} hidden{mark}"),
            format!("{short} ·{count}{mark}"),
            format!("{count} hidden{mark}"),
            format!("{count}{mark}"),
        ]
    } else {
        vec![qualified, short]
    };
    candidates.dedup();
    candidates
}

/// Label for the mobile switcher's agents section title.
pub(crate) fn mobile_agents_title(app: &AppState, hidden: AgentViewHidden) -> String {
    let Some(label) = agent_view_label_candidates(app, hidden).into_iter().next() else {
        return "agents".to_string();
    };
    format!("agents · {label}")
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_and_hidden_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> (Vec<AgentPanelEntry>, AgentViewHidden) {
    agent_panel_entries_and_hidden_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_and_hidden_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> (Vec<AgentPanelEntry>, AgentViewHidden) {
    let mut entries = collect_agent_panel_entries_with_runtimes(app, terminal_runtimes);
    // Classify against the whole fleet first: a view that filters on `relation`
    // has to see what each pane is, not what is left after it filtered.
    crate::app::agent_tree::classify_agent_relations(&mut entries);
    let hidden = crate::app::agent_view::apply_agent_view(app, &mut entries);
    // After filtering and sorting, so a hidden parent re-parents its children
    // onto the nearest survivor instead of leaving a connector pointing at a
    // row that is not on screen.
    crate::app::agent_tree::arrange_agent_tree(&mut entries);
    (entries, hidden)
}

pub(crate) fn all_agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    collect_agent_panel_entries_with_runtimes(app, None)
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_and_hidden_with_runtimes(app, terminal_runtimes).0
}

fn collect_agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    app.workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| {
                    let show_tab = multi_tab
                        || ws
                            .tabs
                            .get(detail.tab_idx)
                            .is_some_and(|tab| !tab.is_auto_named());
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: show_tab.then_some(detail.tab_label),
                        pane_label: detail.pane_label,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        agent_label: Some(detail.agent_label),
                        agent_kind_label: detail.agent_kind_label,
                        agent: detail.agent,
                        state: detail.state,
                        seen: detail.seen,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,
                        last_agent_state_change_at: detail.last_agent_state_change_at,
                        state_labels: detail.state_labels,
                        owner: detail
                            .tokens
                            .get(crate::app::agent_tree::OWNER_TOKEN)
                            .map(|owner| owner.trim().to_string())
                            .filter(|owner| !owner.is_empty()),
                        tokens: detail.tokens,
                        agent_name: detail.agent_name,
                        depth: 0,
                        relation: crate::app::agent_tree::AgentRelation::FirstMate,
                        is_last_child: true,
                        ancestors_continue: Vec::new(),
                    }
                })
        })
        .collect()
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

/// Space rows roll a terminal title up from the same pane the state icon
/// describes, so an icon and a title on one row always talk about one pane.
/// Skipped entirely when no Space row asks for a title.
fn space_terminal_title(
    app: &AppState,
    ws: &crate::workspace::Workspace,
) -> crate::workspace::AggregateTerminalTitle {
    if !app.sidebar_spaces.uses_terminal_title() {
        return crate::workspace::AggregateTerminalTitle::default();
    }
    ws.aggregate_terminal_title(&app.terminals)
}

/// Elapsed time for the pane that decides this Space row's state icon.
///
/// Skipped entirely when no Space row asks for an age, the same way
/// [`space_terminal_title`] is skipped: resolving the winning pane walks every
/// pane in the workspace, and an unconfigured sidebar should not pay for it.
fn space_state_age(
    app: &AppState,
    ws: &crate::workspace::Workspace,
) -> Option<std::time::Duration> {
    if !app.sidebar_spaces.uses_state_age() {
        return None;
    }
    ws.aggregate_state_changed_at(&app.terminals)
        .map(|at| app.state_age_now.saturating_duration_since(at))
}

fn workspace_row_height(app: &AppState, ws: &crate::workspace::Workspace, indented: bool) -> u16 {
    let (state, seen) = ws.aggregate_state(&app.terminals);
    let label = if indented {
        grouped_child_display_label(
            &ws.display_name_from_terminals(&app.terminals),
            ws.branch().as_deref(),
            ws.custom_name.is_some(),
        )
    } else {
        ws.display_name_from_terminals(&app.terminals)
    };
    let token_values = ws.metadata_tokens.values();
    let terminal_title = space_terminal_title(app, ws);
    tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: &label,
            branch: ws.branch().as_deref(),
            state_text: state_label(state, seen),
            state_age: space_state_age(app, ws),
            ahead_behind: ws.git_ahead_behind(),
            terminal_title: terminal_title.raw.as_deref(),
            terminal_title_stripped: terminal_title.stripped.as_deref(),
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
    .len()
    .max(1)
    .min(u16::MAX as usize) as u16
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
) -> u16 {
    workspace_row_height(app, workspace, indented).min(body_height)
}

fn workspace_entry_gap(
    app: &AppState,
    entries: &[WorkspaceListEntry],
    entry_idx: usize,
    indented: bool,
) -> u16 {
    if entry_idx + 1 < entries.len()
        && !(indented && next_entry_is_indented_workspace(entries, entry_idx))
    {
        app.sidebar_spaces.row_gap
    } else {
        0
    }
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

/// Aggregate state of a collapsed group, over the group's rows only.
///
/// Workspaces that share the key but are not group members (a second workspace
/// opened in the same main checkout) keep their own visible row, so folding
/// their state into the parent would double-report it.
///
/// State and elapsed time are resolved in one pass so the parent row cannot
/// show one member's dot beside another member's clock.
fn space_aggregate_state_and_age(
    app: &AppState,
    key: &str,
) -> (AgentState, bool, Option<std::time::Duration>) {
    let group = app.worktree_space_group(key);
    let wants_age = app.sidebar_spaces.uses_state_age();
    app.workspaces
        .iter()
        .enumerate()
        .filter(|(ws_idx, ws)| {
            ws.worktree_space().is_some_and(|space| space.key == key)
                && group.as_ref().is_none_or(|group| group.contains(*ws_idx))
        })
        .map(|(_, ws)| {
            let (state, seen) = ws.aggregate_state(&app.terminals);
            let age = wants_age
                .then(|| ws.aggregate_state_changed_at(&app.terminals))
                .flatten()
                .map(|at| app.state_age_now.saturating_duration_since(at));
            (state, seen, age)
        })
        .max_by_key(|(state, seen, _)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true, None))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    let group = app.worktree_space_group(&space.key)?;
    (group.parent_idx == ws_idx).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

pub(crate) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

/// One row of the sidebar's single tree.
///
/// Spaces and owned agent panes are rows of the same list on purpose: a second
/// mate is a Space and its workers are panes, and the captain's tree runs
/// straight through that boundary. Every row carries the same three drawing
/// facts so one prefix function serves both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace {
        ws_idx: usize,
        /// Worktree-group child. A styling fact, not a depth: it also shortens
        /// the label and suppresses git detail.
        indented: bool,
        depth: u8,
        ancestors_continue: Vec<bool>,
        is_last_child: bool,
    },
    /// An agent pane that named an owner. Indexes [`sidebar_agent_entries`].
    Agent {
        entry_idx: usize,
        depth: u8,
        ancestors_continue: Vec<bool>,
        is_last_child: bool,
    },
}

impl WorkspaceListEntry {
    pub(crate) fn depth(&self) -> u8 {
        match self {
            Self::Workspace { depth, .. } | Self::Agent { depth, .. } => *depth,
        }
    }

    pub(crate) fn ancestors_continue(&self) -> &[bool] {
        match self {
            Self::Workspace {
                ancestors_continue, ..
            }
            | Self::Agent {
                ancestors_continue, ..
            } => ancestors_continue,
        }
    }

    pub(crate) fn is_last_child(&self) -> bool {
        match self {
            Self::Workspace { is_last_child, .. } | Self::Agent { is_last_child, .. } => {
                *is_last_child
            }
        }
    }
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    if workspace_list_entries(app).is_empty() {
        0
    } else {
        requested.min(workspace_list_bottom_start(app, ws_area))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, false)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true)
}

/// The expanded list with the fleet's agent rows dropped, leaving Spaces only.
///
/// The mobile switcher lists Spaces and agents as separate blocks, so it wants
/// the Space rows on their own rather than the interleaved tree the sidebar
/// draws.
pub(crate) fn workspace_list_spaces_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    let mut entries = workspace_list_entries_expanded(app);
    entries.retain(|entry| matches!(entry, WorkspaceListEntry::Workspace { .. }));
    entries
}

/// A Space and the worktree children that always travel with it.
///
/// The worktree group is a fact about checkouts, not about ownership, so it is
/// resolved before the ownership walk and re-emitted inside the block. That
/// keeps the two kinds of nesting from competing for the same parent.
struct SpaceBlock {
    parent_idx: usize,
    children: Vec<usize>,
}

/// The handle an `owner` token uses to name this Space.
///
/// It is the Space's own label, because that is already what a fleet writes
/// into `owner` — `firstmate`, `2ndmate-explore` — so there is no second naming
/// scheme to keep in sync. Resolved without terminal runtimes so the tree's
/// shape cannot change with a terminal title.
fn space_tree_name(app: &AppState, ws_idx: usize) -> Option<String> {
    let ws = app.workspaces.get(ws_idx)?;
    let name = ws.display_name_from(&app.terminals, &TerminalRuntimeRegistry::new());
    (!name.trim().is_empty()).then_some(name)
}

/// Who this Space says owns it, from its own `owner` metadata token.
///
/// A Space publishes this with `workspace report-metadata --token owner=...`,
/// the same token a pane uses. Nothing declares it by default, so a fleet that
/// publishes nothing gets the flat list it has always had.
fn space_owner(app: &AppState, ws_idx: usize) -> Option<String> {
    let ws = app.workspaces.get(ws_idx)?;
    ws.metadata_tokens
        .values()
        .get(crate::app::agent_tree::OWNER_TOKEN)
        .map(|owner| owner.trim().to_string())
        .filter(|owner| !owner.is_empty())
}

/// The agent panes that belong in the sidebar tree: the ones that named an
/// owner.
///
/// A pane with no owner is deliberately absent. In a fleet where the mates are
/// Spaces, a mate's own pane declares no owner, so drawing every agent pane
/// would draw each mate twice — once as its Space row and again as a child of
/// itself. Owning is what makes a pane part of somebody's tree.
///
/// No Agents view is applied. This is the only place a worker is drawn now, and
/// a filter here could hide the whole fleet from the only panel that shows it.
pub(crate) fn sidebar_agent_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    let mut entries = all_agent_panel_entries(app);
    // Classify against the whole fleet before dropping the unowned rows, so a
    // pane's relation still reflects where it really sits.
    crate::app::agent_tree::classify_agent_relations(&mut entries);
    if matches!(
        app.agent_panel_sort,
        crate::app::state::AgentPanelSort::Priority
    ) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(workspace_attention_priority(entry.state, entry.seen)),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }
    entries.retain(|entry| entry.owner.is_some());
    entries
}

/// Arrange `blocks` and the owned agent panes into one tree, then flatten it.
///
/// Both kinds of node go through [`crate::app::agent_tree::arrange_owner_tree`]
/// together, so a worker nests under its second mate's Space by exactly the
/// same rule that nests a second mate under the first mate.
fn arrange_space_tree(
    app: &AppState,
    blocks: &[SpaceBlock],
    agents: &[AgentPanelEntry],
) -> Vec<WorkspaceListEntry> {
    let space_names: Vec<Option<String>> = blocks
        .iter()
        .map(|block| space_tree_name(app, block.parent_idx))
        .collect();
    let space_owners: Vec<Option<String>> = blocks
        .iter()
        .map(|block| space_owner(app, block.parent_idx))
        .collect();

    let mut nodes: Vec<crate::app::agent_tree::OwnedNode<'_>> = space_names
        .iter()
        .zip(&space_owners)
        .map(|(name, owner)| crate::app::agent_tree::OwnedNode {
            name: name.as_deref(),
            owner: owner.as_deref(),
        })
        .collect();
    nodes.extend(
        agents
            .iter()
            .map(|entry| crate::app::agent_tree::OwnedNode {
                name: entry.agent_name.as_deref(),
                owner: entry.owner.as_deref(),
            }),
    );

    let placements = crate::app::agent_tree::arrange_owner_tree(&nodes);
    // A block's worktree children draw as siblings of whatever it owns, so the
    // last worktree child is only `└` when nothing owned follows it. In a
    // depth-first flattening a node owns children exactly when the next row is
    // one level deeper.
    let owns_children: Vec<bool> = placements
        .iter()
        .enumerate()
        .map(|(position, placement)| {
            placements
                .get(position + 1)
                .is_some_and(|next| next.depth == placement.depth.saturating_add(1))
        })
        .collect();

    let mut entries = Vec::with_capacity(placements.len());
    for (position, placement) in placements.into_iter().enumerate() {
        match blocks.get(placement.index) {
            Some(block) => {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: block.parent_idx,
                    indented: false,
                    depth: placement.depth,
                    ancestors_continue: placement.ancestors_continue.clone(),
                    is_last_child: placement.is_last_child && block.children.is_empty(),
                });
                let mut child_ancestors = placement.ancestors_continue.clone();
                child_ancestors.push(!placement.is_last_child);
                let last = block.children.len().saturating_sub(1);
                for (child_position, child_idx) in block.children.iter().enumerate() {
                    entries.push(WorkspaceListEntry::Workspace {
                        ws_idx: *child_idx,
                        indented: true,
                        depth: placement.depth.saturating_add(1),
                        ancestors_continue: child_ancestors.clone(),
                        is_last_child: child_position == last && !owns_children[position],
                    });
                }
            }
            None => entries.push(WorkspaceListEntry::Agent {
                entry_idx: placement.index - blocks.len(),
                depth: placement.depth,
                ancestors_continue: placement.ancestors_continue,
                is_last_child: placement.is_last_child,
            }),
        }
    }
    entries
}

fn workspace_list_entries_inner(app: &AppState, force_expanded: bool) -> Vec<WorkspaceListEntry> {
    let mut groups_by_key =
        std::collections::HashMap::<String, crate::app::state::WorktreeSpaceGroup>::new();
    for ws in app.workspaces.iter() {
        let Some(space) = ws.worktree_space() else {
            continue;
        };
        if groups_by_key.contains_key(&space.key) {
            continue;
        }
        if let Some(group) = app.worktree_space_group(&space.key) {
            groups_by_key.insert(space.key.clone(), group);
        }
    }

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut blocks: Vec<SpaceBlock> = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        // A workspace only joins a tree when it is that tree's main-checkout
        // parent or one of its linked worktrees. Anything else - including a
        // second workspace opened in the same main checkout - keeps its own
        // top-level row.
        let Some((space, group)) = ws.worktree_space().and_then(|space| {
            groups_by_key
                .get(&space.key)
                .filter(|group| group.contains(ws_idx))
                .map(|group| (space, group))
        }) else {
            blocks.push(SpaceBlock {
                parent_idx: ws_idx,
                children: Vec::new(),
            });
            continue;
        };

        if !emitted_groups.insert(space.key.clone()) {
            continue;
        }

        let collapsed = !force_expanded && app.collapsed_space_keys.contains(&space.key);
        let children = if collapsed {
            visible_group_idx
                .filter(|idx| group.children.contains(idx))
                .into_iter()
                .collect()
        } else {
            group.children.clone()
        };
        blocks.push(SpaceBlock {
            parent_idx: group.parent_idx,
            children,
        });
    }

    arrange_space_tree(app, &blocks, &sidebar_agent_entries(app))
}

pub(crate) fn workspace_list_rect(area: Rect) -> Rect {
    sidebar_content_rect(area)
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

/// Drawn height of one tree row, whichever kind it is.
///
/// Space rows and agent rows keep their own configured token rows, so the two
/// keep the heights their `[ui.sidebar.spaces]` / `[ui.sidebar.agents]` blocks
/// ask for even though they are now one list.
fn list_entry_height(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
    body_height: u16,
) -> u16 {
    match entry {
        WorkspaceListEntry::Workspace {
            ws_idx, indented, ..
        } => app
            .workspaces
            .get(*ws_idx)
            .map(|ws| workspace_row_height_in_body(app, ws, *indented, body_height))
            .unwrap_or(0),
        WorkspaceListEntry::Agent { entry_idx, .. } => agents
            .get(*entry_idx)
            .map(|entry| agent_entry_height_in_body(app, entry, body_height))
            .unwrap_or(0),
    }
}

/// Gap after one tree row. Each kind keeps its own `row_gap`; the compact
/// worktree-group packing is unchanged.
fn list_entry_gap(app: &AppState, entries: &[WorkspaceListEntry], entry_idx: usize) -> u16 {
    match entries.get(entry_idx) {
        Some(WorkspaceListEntry::Workspace { indented, .. }) => {
            workspace_entry_gap(app, entries, entry_idx, *indented)
        }
        Some(WorkspaceListEntry::Agent { .. }) => agent_entry_gap(app, entry_idx, entries.len()),
        None => 0,
    }
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = list_entry_height(app, &agents, entry, body.height);
        if row_height == 0 {
            continue;
        }
        if used_rows.saturating_add(row_height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(row_height);
        visible += 1;
        used_rows = used_rows
            .saturating_add(list_entry_gap(app, &entries, entry_idx))
            .min(body.height);
    }
    visible
}

fn workspace_list_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = workspace_list_body_rect(area, false);
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (entry_idx, entry) in entries.iter().enumerate().rev() {
        let row_height = list_entry_height(app, &agents, entry, body.height);
        if row_height == 0 {
            continue;
        }
        let needed = row_height.saturating_add(list_entry_gap(app, &entries, entry_idx));
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        start = entry_idx;
    }
    start.min(entries.len().saturating_sub(1))
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let max_scroll = workspace_list_bottom_start(app, area);
    let scroll = app.workspace_scroll.min(max_scroll);
    let viewport_rows = workspace_list_visible_count(app, area, scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

fn resolved_agent_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| state_label(entry.state, entry.seen));
    let state_age = entry
        .last_agent_state_change_at
        .map(|at| app.state_age_now.saturating_duration_since(at));
    tokens::agent_rows(&app.sidebar_agents, entry, label, state_age)
}

pub(crate) fn agent_entry_height_in_body(
    app: &AppState,
    entry: &AgentPanelEntry,
    body_height: u16,
) -> u16 {
    (resolved_agent_rows(app, entry)
        .len()
        .max(1)
        .min(u16::MAX as usize) as u16)
        .min(body_height)
}

pub(crate) fn agent_entry_gap(app: &AppState, entry_idx: usize, entry_count: usize) -> u16 {
    if entry_idx + 1 < entry_count {
        app.sidebar_agents.row_gap
    } else {
        0
    }
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (Vec<crate::app::state::WorkspaceCardArea>, Vec<()>) {
    let ws_area = workspace_list_rect(area);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = list_entry_height(app, &agents, entry, body.height);
        if row_height == 0 {
            continue;
        }
        if row_y.saturating_add(row_height) > body_bottom {
            break;
        }
        let (ws_idx, indented, agent) = match entry {
            WorkspaceListEntry::Workspace {
                ws_idx, indented, ..
            } => (*ws_idx, *indented, None),
            WorkspaceListEntry::Agent { entry_idx, .. } => {
                let Some(detail) = agents.get(*entry_idx) else {
                    continue;
                };
                (
                    detail.ws_idx,
                    false,
                    Some(crate::app::state::AgentCardTarget {
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                    }),
                )
            }
        };
        cards.push(crate::app::state::WorkspaceCardArea {
            ws_idx,
            rect: Rect::new(body.x, row_y, body.width, row_height),
            indented,
            entry_idx,
            agent,
        });
        row_y = row_y
            .saturating_add(row_height)
            .saturating_add(list_entry_gap(app, &entries, entry_idx))
            .min(body_bottom);
    }

    (cards, headers)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

pub(crate) fn workspace_group_chevron_rect(card: &crate::app::state::WorkspaceCardArea) -> Rect {
    if card.rect.width == 0 || card.rect.height == 0 {
        return Rect::default();
    }

    Rect::new(
        card.rect.x + card.rect.width.saturating_sub(1),
        card.rect.y,
        1,
        1,
    )
}

/// The collapsed sidebar's one content column.
///
/// Collapsed is the same single panel as expanded, just narrower: the whole
/// column is the Spaces glance.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> Rect {
    sidebar_content_rect(area)
}

/// Collapsed sidebar: a one-cell-per-Space glance down the whole column.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let ws_area = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_slots(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
) -> Vec<(crate::app::state::WorkspaceDropTarget, u16)> {
    if area.height == 0 || cards.is_empty() {
        return Vec::new();
    }
    let list_bottom = area.y + area.height.saturating_sub(1);
    let entries = workspace_list_entries(app);
    let entry_position = |ws_idx| {
        entries.iter().position(|entry| {
            matches!(
                entry,
                WorkspaceListEntry::Workspace {
                    ws_idx: entry_ws_idx,
                    ..
                } if *entry_ws_idx == ws_idx
            )
        })
    };

    let block_root_at = |entry_idx: usize| {
        entries[..=entry_idx]
            .iter()
            .rev()
            .find_map(|entry| match entry {
                WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                    ..
                } => Some(*ws_idx),
                _ => None,
            })
    };

    let mut slots = Vec::new();
    let mut previous_root = None;
    // Agent rows share the card list but are not reorderable Spaces, so they
    // anchor nothing: a drop above a worker would name a workspace the pointer
    // is not over.
    for card in cards.iter().filter(|card| card.agent.is_none()) {
        let Some(entry_idx) = entry_position(card.ws_idx) else {
            continue;
        };
        let Some(root_idx) = block_root_at(entry_idx) else {
            continue;
        };
        if previous_root == Some(root_idx) {
            continue;
        }
        previous_root = Some(root_idx);
        if let Some(row) = card.rect.y.checked_sub(1).filter(|row| *row < list_bottom) {
            slots.push((
                crate::app::state::WorkspaceDropTarget::Before(root_idx),
                row,
            ));
        }
    }

    let Some(last) = cards.iter().filter(|card| card.agent.is_none()).next_back() else {
        return slots;
    };
    let Some(last_entry_idx) = entry_position(last.ws_idx) else {
        return slots;
    };
    let next_entry = entries.get(last_entry_idx.saturating_add(1));
    if matches!(
        next_entry,
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    ) {
        return slots;
    }
    let target = match next_entry {
        Some(WorkspaceListEntry::Workspace { ws_idx, .. }) => {
            crate::app::state::WorkspaceDropTarget::Before(*ws_idx)
        }
        // A row that is not a Space cannot name a drop position, so the drag
        // lands at the end rather than silently anchoring on a worker.
        Some(WorkspaceListEntry::Agent { .. }) | None => {
            crate::app::state::WorkspaceDropTarget::End
        }
    };
    let row = last.rect.y.saturating_add(last.rect.height);
    if row < list_bottom
        && slots
            .last()
            .is_none_or(|(last_target, _)| *last_target != target)
    {
        slots.push((target, row));
    }
    slots
}

pub(crate) fn workspace_drop_indicator_row(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    target: crate::app::state::WorkspaceDropTarget,
) -> Option<u16> {
    workspace_drop_slots(app, cards, area)
        .into_iter()
        .find_map(|(candidate, row)| (candidate == target).then_some(row))
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    render_workspace_list(
        app,
        terminal_runtimes,
        frame,
        sidebar_content_rect(area),
        is_navigating,
    );
    render_sidebar_toggle(app, frame, area, false, p);
}

fn resolved_token_spans(
    resolved: &[ResolvedToken],
    state_icon: (&str, Style),
    state_text_style: Style,
    workspace_style: Style,
    secondary_style: Style,
    custom_style: Style,
    p: &Palette,
    animation_tick: u32,
    max_width: usize,
) -> Vec<Span<'static>> {
    let fixed_widths = resolved
        .iter()
        .map(|token| match &token.kind {
            ResolvedTokenKind::StateIcon => display_width(state_icon.0),
            // Fixed, not flexible: at two to four columns there is nothing to
            // reclaim, and a truncated age is a wrong age - `4` out of `47m`
            // reads as four of something. It shares the fixed lane with the
            // state icon and the git counters for the same reason.
            ResolvedTokenKind::StateAge(text) => display_width(text),
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                usize::from(*ahead > 0) * display_width(&format!("↑{ahead}"))
                    + usize::from(*behind > 0) * display_width(&format!("↓{behind}"))
                    + usize::from(*ahead > 0 && *behind > 0)
            }
            _ => 0,
        })
        .collect::<Vec<_>>();
    let flexible_widths = resolved
        .iter()
        .map(|token| match &token.kind {
            ResolvedTokenKind::StateText(text)
            | ResolvedTokenKind::Workspace(text)
            | ResolvedTokenKind::Tab(text)
            | ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::TerminalTitle(text)
            | ResolvedTokenKind::Branch(text)
            | ResolvedTokenKind::Custom(text) => display_width(text),
            _ => 0,
        })
        .collect::<Vec<_>>();
    // Every token at its full width, with decorated separators. When even this
    // fits there is nothing to reclaim and the row renders exactly as before.
    let natural_width = fixed_widths.iter().chain(&flexible_widths).sum::<usize>()
        + resolved
            .windows(2)
            .map(|pair| display_width(tokens::separator(&pair[0], &pair[1])))
            .sum::<usize>();
    let compact_separators = natural_width > max_width;
    let separator_for = |previous: &ResolvedToken, current: &ResolvedToken| {
        if compact_separators {
            tokens::compact_separator(previous, current)
        } else {
            tokens::separator(previous, current)
        }
    };
    let minimum_width = |active: &[bool]| {
        let indices = active
            .iter()
            .enumerate()
            .filter_map(|(index, active)| active.then_some(index))
            .collect::<Vec<_>>();
        let content = indices
            .iter()
            .map(|index| fixed_widths[*index] + usize::from(flexible_widths[*index] > 0))
            .sum::<usize>();
        let separators = indices
            .windows(2)
            .map(|pair| display_width(separator_for(&resolved[pair[0]], &resolved[pair[1]])))
            .sum::<usize>();
        content + separators
    };
    let mut active = resolved.iter().map(|_| true).collect::<Vec<_>>();
    if minimum_width(&active) > max_width {
        for (index, width) in flexible_widths.iter().enumerate() {
            if *width > 0 {
                active[index] = false;
            }
        }
        for index in (0..resolved.len()).rev() {
            if flexible_widths[index] == 0 {
                continue;
            }
            active[index] = true;
            if minimum_width(&active) > max_width {
                active[index] = false;
            }
        }
    }
    let visible_indices = active
        .iter()
        .enumerate()
        .filter_map(|(index, active)| active.then_some(index))
        .collect::<Vec<_>>();
    let separator_width = visible_indices
        .windows(2)
        .map(|pair| display_width(separator_for(&resolved[pair[0]], &resolved[pair[1]])))
        .sum::<usize>();
    let fixed_width = visible_indices
        .iter()
        .map(|index| fixed_widths[*index])
        .sum::<usize>();
    let mut budgets = flexible_widths
        .iter()
        .enumerate()
        .map(|(index, width)| usize::from(active[index] && *width > 0))
        .collect::<Vec<_>>();
    let minimum = budgets.iter().sum::<usize>();
    let mut remaining = max_width
        .saturating_sub(separator_width + fixed_width)
        .saturating_sub(minimum);
    while remaining > 0 {
        let mut grew = false;
        for (budget, width) in budgets.iter_mut().zip(&flexible_widths) {
            if *budget > 0 && *budget < *width {
                *budget += 1;
                remaining -= 1;
                grew = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut spans = Vec::new();
    for (position, index) in visible_indices.iter().copied().enumerate() {
        let token = &resolved[index];
        if position > 0 {
            let previous = &resolved[visible_indices[position - 1]];
            spans.push(Span::styled(
                separator_for(previous, token),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ));
        }
        match &token.kind {
            // Identity tokens (workspace/tab/pane/agent/branch) elide in the
            // middle; the prose tokens (state text, terminal title, custom
            // metadata) keep truncating from the end. Prose is written to be
            // read front to back, so its head is the part worth keeping. Names
            // are not: siblings routinely share a long prefix - `2ndmate-`,
            // `issue/`, `feature/` - and end-truncation spends the whole budget
            // redrawing the one part every row already agrees on. Eliding the
            // middle keeps both anchors, so what survives is what actually tells
            // two rows apart.
            ResolvedTokenKind::StateIcon => {
                spans.push(Span::styled(
                    state_icon.0.to_string(),
                    apply_token_style(state_icon.1, token.style, p, animation_tick),
                ));
            }
            ResolvedTokenKind::StateText(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(state_text_style, token.style, p, animation_tick),
                ));
            }
            // Drawn in the state's own colour, dimmed. The age qualifies the
            // state rather than competing with it, and dimming is how the row
            // says so without a second hue. It is not an alarm: nothing about
            // the styling changes as the number grows, because the runtime has
            // no evidence that a long state is a bad one.
            ResolvedTokenKind::StateAge(text) => {
                spans.push(Span::styled(
                    text.clone(),
                    apply_token_style(
                        state_text_style.add_modifier(Modifier::DIM),
                        token.style,
                        p,
                        animation_tick,
                    ),
                ));
            }
            ResolvedTokenKind::Workspace(text) => {
                spans.push(Span::styled(
                    middle_elide(text, budgets[index]),
                    apply_token_style(workspace_style, token.style, p, animation_tick),
                ));
            }
            ResolvedTokenKind::Tab(text)
            | ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::Branch(text) => {
                spans.push(Span::styled(
                    middle_elide(text, budgets[index]),
                    apply_token_style(secondary_style, token.style, p, animation_tick),
                ));
            }
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                if *ahead > 0 {
                    spans.push(Span::styled(
                        format!("↑{ahead}"),
                        apply_token_style(
                            Style::default().fg(p.green),
                            token.style,
                            p,
                            animation_tick,
                        ),
                    ));
                }
                if *ahead > 0 && *behind > 0 {
                    spans.push(Span::styled(
                        " ",
                        apply_token_style(Style::default(), token.style, p, animation_tick),
                    ));
                }
                if *behind > 0 {
                    spans.push(Span::styled(
                        format!("↓{behind}"),
                        apply_token_style(
                            Style::default().fg(p.red),
                            token.style,
                            p,
                            animation_tick,
                        ),
                    ));
                }
            }
            ResolvedTokenKind::TerminalTitle(text) | ResolvedTokenKind::Custom(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(custom_style, token.style, p, animation_tick),
                ));
            }
        }
    }
    spans
}

/// Style for one cell of a child row's branch-line connector.
///
/// A relation signal only ever changes a cell's *style*. The symbol, the cell
/// count, and every width the layout was computed from stay exactly what they
/// would be with no signal at all, so a row whose signal was skipped, cut
/// short, or never scheduled draws the same characters in the same columns as
/// one that never had a signal.
/// Indent and connector for one Agents-panel row.
///
/// Deliberately the same vocabulary as the Spaces panel's child cards —
/// `├─ `/`└─ ` on the first row, a `│` continuation while a level still has
/// siblings below — because the user sees both panels at once and a second
/// branch glyph would read as a second meaning. Returns the spans and the
/// columns they consume, which the caller subtracts from the token budget.
///
/// The Spaces panel indents a child by three columns before its connector; the
/// Agents panel starts one column in rather than three, so the same shape costs
/// `3 * depth` columns here instead of `3 + 3 * depth`. Continuation rows sit
/// two columns further right than their first row in both panels, which is what
/// keeps wrapped text aligned under the name rather than under the state dot.
/// Vertical rails for `levels` ancestor levels, drawn before a row's own
/// prefix. A level shows `│` while that ancestor still has a sibling below it.
///
/// Zero levels draws nothing, which is what keeps a fleet that declares no
/// ownership rendering byte-identically to before the tree existed.
fn ancestor_rail(
    levels: u8,
    ancestors_continue: &[bool],
    p: &Palette,
) -> (Vec<Span<'static>>, usize) {
    let levels = levels.min(crate::app::agent_tree::MAX_DISPLAY_DEPTH);
    let mut spans = Vec::new();
    for level in 1..=levels {
        if ancestors_continue
            .get(level as usize)
            .copied()
            .unwrap_or(false)
        {
            spans.push(Span::styled("│", Style::default().fg(p.overlay0)));
            spans.push(Span::raw("  "));
        } else {
            spans.push(Span::raw("   "));
        }
    }
    (spans, 3 * levels as usize)
}

/// Draw one owned agent pane as a row of the Spaces tree.
///
/// It uses the same connector maths as every other row and its own
/// `[ui.sidebar.agents]` token layout, so a worker reads as a branch of its
/// mate rather than as a visitor from somewhere else.
fn render_agent_row(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    entries: &[WorkspaceListEntry],
    agents: &[AgentPanelEntry],
    list_bottom: u16,
) {
    let Some(entry) = entries.get(card.entry_idx) else {
        return;
    };
    let WorkspaceListEntry::Agent { entry_idx, .. } = entry else {
        return;
    };
    let Some(detail) = agents.get(*entry_idx) else {
        return;
    };

    let p = &app.palette;
    let rows = resolved_agent_rows(app, detail);
    let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
    let label_color = state_label_color(detail.state, detail.seen, p);
    let row_style = if is_active {
        Style::default().bg(p.surface_dim)
    } else {
        Style::default()
    };
    let name_style = if is_active {
        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
    };
    let status_style = if is_active {
        Style::default().fg(label_color)
    } else {
        Style::default().fg(label_color).add_modifier(Modifier::DIM)
    };
    let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
    let state_icon = state_dot(detail.state, detail.seen, p);

    for (row_index, resolved) in rows.iter().enumerate() {
        let row_y = card.rect.y + row_index as u16;
        if row_index as u16 >= card.rect.height || row_y >= list_bottom {
            break;
        }
        let (mut spans, prefix_width) = agent_row_prefix(
            entry.depth(),
            entry.is_last_child(),
            entry.ancestors_continue(),
            row_index,
            p,
        );
        spans.extend(resolved_token_spans(
            resolved,
            state_icon,
            status_style,
            name_style,
            agent_style,
            agent_style,
            p,
            app.animation_tick,
            (card.rect.width as usize).saturating_sub(prefix_width),
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );
    }
}

fn agent_row_prefix(
    depth: u8,
    is_last_child: bool,
    ancestors_continue: &[bool],
    row_index: usize,
    p: &Palette,
) -> (Vec<Span<'static>>, usize) {
    let depth = depth.min(crate::app::agent_tree::MAX_DISPLAY_DEPTH);
    if depth == 0 {
        // Byte-identical to the pre-tree panel, so a fleet with no declared
        // ownership draws exactly as it did before.
        return if row_index == 0 {
            (vec![Span::raw(" ")], 1)
        } else {
            (vec![Span::raw("   ")], 3)
        };
    }

    let line_style = Style::default().fg(p.overlay0);
    let mut spans = vec![Span::raw(" ")];

    // One spacer per intermediate ancestor level; level 0 is the root column,
    // which carries no connector of its own.
    for level in 1..depth {
        let open = ancestors_continue
            .get(level as usize)
            .copied()
            .unwrap_or(false);
        if open {
            spans.push(Span::styled("│", line_style));
            spans.push(Span::raw("  "));
        } else {
            spans.push(Span::raw("   "));
        }
    }

    if row_index == 0 {
        let connector = if is_last_child { "└─ " } else { "├─ " };
        for glyph in connector.chars() {
            spans.push(Span::styled(glyph.to_string(), line_style));
        }
        (spans, 3 * depth as usize + 1)
    } else {
        if is_last_child {
            spans.push(Span::raw("   "));
        } else {
            spans.push(Span::styled("│", line_style));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::raw("  "));
        (spans, 3 * depth as usize + 3)
    }
}

fn connector_cell_style(
    base: Style,
    phase: Option<RelationSignalPhase>,
    cell: u8,
    p: &Palette,
) -> Style {
    match phase.and_then(|phase| phase.connector_cell()) {
        // A block, not a coloured glyph: one of the three connector cells is a
        // blank, and a foreground colour on a blank has nothing to ink.
        Some(lit) if lit == cell => Style::default().fg(p.panel_bg).bg(p.accent),
        _ => base,
    }
}

/// Emphasis on a row's state icon while a signal is sitting on it.
///
/// The icon's own colour becomes the block, rather than being replaced by the
/// accent, because that colour *is* the agent state. A decoration is never
/// allowed to overwrite the information underneath it.
fn arrived_state_icon_style(base: Style, phase: Option<RelationSignalPhase>, p: &Palette) -> Style {
    if !phase.is_some_and(|phase| phase.is_at_state_icon()) {
        return base;
    }
    let ink = base.fg.unwrap_or(p.text);
    base.fg(p.panel_bg).bg(ink).add_modifier(Modifier::BOLD)
}

/// Frames in each half of one pulse cycle. The full cycle is twice this, so at
/// `ANIMATION_INTERVAL` the pulse breathes over about 1.6 seconds.
const PULSE_HALF_CYCLE_FRAMES: u32 = 8;
/// Blend fraction toward the panel background at the dimmest point of a pulse.
/// Deliberately partial: the token stays readable at its trough.
const PULSE_MAX_FADE: f32 = 0.6;

/// Triangle ramp from `0.0` at the pulse peak to `PULSE_MAX_FADE` at its
/// trough. Tick `0` is the peak, so a freshly armed pulse starts out looking
/// exactly like the same token without emphasis.
fn pulse_fade(tick: u32) -> f32 {
    let period = PULSE_HALF_CYCLE_FRAMES * 2;
    let phase = tick % period;
    let distance_from_peak = phase.min(period - phase);
    PULSE_MAX_FADE * distance_from_peak as f32 / PULSE_HALF_CYCLE_FRAMES as f32
}

fn rgb_parts(color: ratatui::style::Color) -> Option<(u8, u8, u8)> {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

fn blend_channel(from: u8, to: u8, mix: f32) -> u8 {
    (f32::from(from) + (f32::from(to) - f32::from(from)) * mix).round() as u8
}

/// Ramps `fg` toward `bg` by `mix`. This is how emphasis animates: a brightness
/// ramp we draw ourselves, never SGR blink.
fn blend_toward(fg: ratatui::style::Color, bg: ratatui::style::Color, mix: f32) -> Option<Style> {
    let (fr, fg_, fb) = rgb_parts(fg)?;
    let (br, bg_, bb) = rgb_parts(bg)?;
    Some(Style::default().fg(ratatui::style::Color::Rgb(
        blend_channel(fr, br, mix),
        blend_channel(fg_, bg_, mix),
        blend_channel(fb, bb, mix),
    )))
}

fn apply_token_style(
    mut style: Style,
    patch: crate::config::SidebarTokenStyle,
    p: &Palette,
    animation_tick: u32,
) -> Style {
    if let Some(fg) = patch.fg {
        style = style.fg(fg.ratatui());
    }
    if let Some(bg) = patch.bg {
        style = style.bg(bg.ratatui());
    }
    if let Some(bold) = patch.bold {
        style = if bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style.remove_modifier(Modifier::BOLD)
        };
    }
    if let Some(dim) = patch.dim {
        style = if dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style.remove_modifier(Modifier::DIM)
        };
    }
    if let Some(italic) = patch.italic {
        style = if italic {
            style.add_modifier(Modifier::ITALIC)
        } else {
            style.remove_modifier(Modifier::ITALIC)
        };
    }
    if let Some(underline) = patch.underline {
        style = if underline {
            style.add_modifier(Modifier::UNDERLINED)
        } else {
            style.remove_modifier(Modifier::UNDERLINED)
        };
    }
    if let Some(reverse) = patch.reverse {
        style = if reverse {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style.remove_modifier(Modifier::REVERSED)
        };
    }
    match patch.emphasis {
        Some(SidebarTokenEmphasis::Pulse) => {
            let fade = pulse_fade(animation_tick);
            let target = patch.bg.map_or(p.panel_bg, |bg| bg.ratatui());
            if let Some(faded) = style.fg.and_then(|fg| blend_toward(fg, target, fade)) {
                style = style.patch(faded);
            }
        }
        Some(SidebarTokenEmphasis::None) | None => {}
    }
    style
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            drop_target: Some(drop_target),
            ..
        }) => workspace_drop_indicator_row(app, &app.view.workspace_card_areas, area, *drop_target),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);

    for card in cards {
        if card.agent.is_some() {
            render_agent_row(app, frame, card, &entries, &agents, list_bottom);
            continue;
        }
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let (own_depth, own_ancestors, own_is_last) = match entries.get(card.entry_idx) {
            Some(entry) => (
                entry.depth(),
                entry.ancestors_continue().to_vec(),
                entry.is_last_child(),
            ),
            None => (0, Vec::new(), true),
        };
        let own_ancestors = own_ancestors.as_slice();
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let display_label = if card.indented {
            grouped_child_display_label(&label, ws.branch().as_deref(), ws.custom_name.is_some())
        } else {
            label
        };
        let parent_group = (!card.indented)
            .then(|| workspace_parent_group_state(app, i))
            .flatten();
        let is_last_child = card.indented
            && entries
                .iter()
                .position(|entry| {
                    matches!(
                        entry,
                        WorkspaceListEntry::Workspace { ws_idx, .. } if *ws_idx == i
                    )
                })
                .is_none_or(|entry_idx| !next_entry_is_indented_workspace(&entries, entry_idx));
        let (display_state, display_seen, display_state_age) = parent_group
            .as_ref()
            .filter(|(_, collapsed)| *collapsed)
            .map(|(key, _)| space_aggregate_state_and_age(app, key))
            .unwrap_or_else(|| (agg_state, agg_seen, space_state_age(app, ws)));
        let state_icon = state_dot(display_state, display_seen, p);
        let signal_phase = app.workspace_relation_signal_phase(i);
        let state_text_style = Style::default()
            .fg(state_label_color(display_state, display_seen, p))
            .add_modifier(Modifier::DIM);
        let branch_style = Style::default().fg(if selected || is_active {
            p.mauve
        } else {
            p.overlay0
        });
        let token_values = ws.metadata_tokens.values();
        let terminal_title = space_terminal_title(app, ws);
        let rows = tokens::space_rows(
            &app.sidebar_spaces,
            SpaceTokenContext {
                workspace: &display_label,
                branch: ws.branch().as_deref(),
                state_text: state_label(display_state, display_seen),
                state_age: display_state_age,
                ahead_behind: ws.git_ahead_behind(),
                terminal_title: terminal_title.raw.as_deref(),
                terminal_title_stripped: terminal_title.stripped.as_deref(),
                tokens: &token_values,
                suppress_git_details: card.indented,
            },
        );

        for (row_index, resolved) in rows.iter().enumerate() {
            if row_index as u16 >= row_height || row_y + row_index as u16 >= list_bottom {
                break;
            }
            // The branch line only exists on a child card's first row, so that
            // is the only row a signal can travel and the only row it damages.
            let row_signal_phase = (row_index == 0).then_some(signal_phase).flatten();
            let mut spans = Vec::new();
            // Only a worktree child needs a rail drawn for it: it brings its
            // own legacy connector, so the ownership levels above it have to be
            // laid down first. A Space row that is not a worktree child gets
            // its whole prefix, rails included, from `agent_row_prefix` below.
            // At depth 0 - a fleet that declares no `owner` anywhere - this is
            // empty and the row draws exactly as it always has.
            let (mut spans_prefix, rail_width) = ancestor_rail(
                if card.indented {
                    own_depth.saturating_sub(1)
                } else {
                    0
                },
                own_ancestors,
                p,
            );
            spans.append(&mut spans_prefix);
            let prefix_width = rail_width
                + if card.indented {
                    spans.push(Span::raw("   "));
                    if row_index == 0 {
                        let connector = if is_last_child { "└─ " } else { "├─ " };
                        let connector_style = Style::default().fg(p.overlay0);
                        for (cell, glyph) in connector.chars().enumerate() {
                            spans.push(Span::styled(
                                glyph.to_string(),
                                connector_cell_style(
                                    connector_style,
                                    row_signal_phase,
                                    cell as u8,
                                    p,
                                ),
                            ));
                        }
                        6
                    } else if is_last_child {
                        spans.push(Span::raw("     "));
                        8
                    } else {
                        spans.push(Span::styled("│", Style::default().fg(p.overlay0)));
                        spans.push(Span::raw("    "));
                        8
                    }
                } else if own_depth > 0 {
                    // An owned Space takes the same connector a worker does:
                    // the tree runs through the Space/pane boundary, so it must
                    // not change shape at it.
                    let (mut owned, width) =
                        agent_row_prefix(own_depth, own_is_last, own_ancestors, row_index, p);
                    spans.append(&mut owned);
                    width
                } else if row_index == 0 {
                    spans.push(Span::raw(" "));
                    1
                } else {
                    spans.push(Span::raw("   "));
                    3
                };
            let trailing_width = if row_index == 0 && parent_group.is_some() {
                2usize
            } else {
                0
            };
            spans.extend(resolved_token_spans(
                resolved,
                (
                    state_icon.0,
                    arrived_state_icon_style(state_icon.1, row_signal_phase, p),
                ),
                state_text_style,
                name_style,
                branch_style,
                branch_style,
                p,
                app.animation_tick,
                (card.rect.width as usize).saturating_sub(prefix_width + trailing_width),
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(card.rect.x, row_y + row_index as u16, card.rect.width, 1),
            );
        }

        if let Some((_, collapsed)) = parent_group {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    if collapsed { "▸" } else { "▾" },
                    Style::default().fg(p.accent),
                )),
                workspace_group_chevron_rect(card),
            );
        }
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("menu", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled("menu", Style::default().fg(p.overlay0))])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, workspace::Workspace};
    use ratatui::{backend::TestBackend, Terminal};

    /// A first mate owning a second mate owning a worker, rendered at the
    /// width the captain actually runs.
    /// The captain's fleet in miniature: the mates are Spaces, the worker is a
    /// pane inside its mate's Space. Both kinds of ownership are published the
    /// same way, with an `owner` metadata token.
    fn owned_fleet_sidebar_rows(width: u16) -> Vec<String> {
        let mut app = crate::app::state::AppState::test_new();
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let worker_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![Workspace::test_new("firstmate"), second_mate];
        app.ensure_test_terminals();
        app.active = Some(0);
        // One row each, so a row carries exactly one entity and its connector.
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        // The second mate's Space names the first mate's Space as its owner.
        app.workspaces[1].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            now,
        );

        let worker_terminal = app.workspaces[1].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&worker_terminal).unwrap();
        terminal.set_agent_name("worker".to_string());
        terminal.state = AgentState::Idle;
        // ...and the worker's pane names the second mate's Space as its owner.
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([(
                "owner".to_string(),
                Some("2ndmate-explore".to_string()),
            )]),
            None,
            now,
        );

        let area = Rect::new(0, 0, width, 20);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..20).map(|row| row_text(buffer, row, width)).collect()
    }

    #[test]
    fn a_worker_draws_beneath_its_second_mate_on_a_twenty_six_wide_sidebar() {
        let rows = owned_fleet_sidebar_rows(26);
        let screen = rows.join("\n");

        let row_of = |name: &str| {
            rows.iter()
                .position(|row| row.contains(name))
                .unwrap_or_else(|| panic!("{name} row missing:\n{screen}"))
        };
        let first_row = row_of("firstmate");
        let second_row = row_of("2ndmate-explore");
        let worker_row = row_of("worker");

        // Ownership order top to bottom: rank 0, rank 1, rank 2.
        assert!(
            first_row < second_row && second_row < worker_row,
            "the fleet is not in ownership order:\n{screen}"
        );

        // The first mate is the root, so it carries no connector.
        assert!(
            !rows[first_row].contains('└') && !rows[first_row].contains('├'),
            "the first mate should not be indented:\n{screen}"
        );

        let connector_indent = |row: usize| {
            rows[row]
                .find(['└', '├'])
                .unwrap_or_else(|| panic!("no connector on row {row}:\n{screen}"))
        };

        // The worker hangs one level deeper than its second mate, and the tree
        // does not change shape at the Space/pane boundary.
        assert!(
            connector_indent(worker_row) > connector_indent(second_row),
            "the worker is not nested under its second mate:\n{screen}"
        );

        // The name still survives the indent at the captain's width, whole and
        // directly after its connector rather than elided away by it.
        assert!(
            rows[worker_row].contains("└─ worker"),
            "the worker name was indented off the panel:\n{screen}"
        );
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        (0..width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn find_symbol_x(buffer: &ratatui::buffer::Buffer, row: u16, width: u16, symbol: &str) -> u16 {
        (0..width)
            .find(|x| buffer[(*x, row)].symbol() == symbol)
            .unwrap_or_else(|| {
                panic!(
                    "missing symbol {symbol:?} in row {}",
                    row_text(buffer, row, width)
                )
            })
    }

    #[test]
    fn default_space_workspace_style_tracks_active_state() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let first_row = app.view.workspace_card_areas[0].rect.y;
        let second_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let active = buffer[(find_symbol_x(buffer, first_row, 25, "o"), first_row)].style();
        assert_eq!(active.fg, Some(app.palette.text));
        assert!(active.add_modifier.contains(Modifier::BOLD));
        assert!(!active.add_modifier.contains(Modifier::DIM));
        assert_eq!(active.bg, Some(app.palette.surface_dim));

        let inactive = buffer[(find_symbol_x(buffer, second_row, 25, "t"), second_row)].style();
        assert_eq!(inactive.fg, Some(app.palette.subtext0));
        assert!(!inactive
            .add_modifier
            .intersects(Modifier::BOLD | Modifier::DIM));
        assert_eq!(inactive.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn space_terminal_title_row_renders_the_active_pane_title() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.spaces]
rows = [["state_icon", "workspace"], ["terminal_title_stripped"]]
"#,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let pane_terminal = app.terminals.get_mut(&terminal_id).unwrap();
        pane_terminal.detected_agent = Some(Agent::Claude);
        pane_terminal.state = AgentState::Working;
        pane_terminal.set_terminal_title(Some("⠋ shipping the token".into()));

        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let titled = app.view.workspace_card_areas[0].rect;
        let untitled = app.view.workspace_card_areas[1].rect;
        let mut renderer = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        renderer
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = renderer.backend().buffer();

        let content_width = area.width - 1;
        assert_eq!(titled.height, 2, "the title row is laid out");
        assert!(row_text(buffer, titled.y, content_width).contains("one"));
        assert_eq!(
            row_text(buffer, titled.y + 1, content_width),
            "   shipping the token"
        );

        assert_eq!(untitled.height, 1, "a space with no title keeps one row");
        assert!(row_text(buffer, untitled.y, content_width).contains("two"));
    }

    #[test]
    fn space_without_terminal_title_renders_no_placeholder_row() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.spaces]
rows = [["workspace"], ["terminal_title"], ["terminal_title_stripped"]]
"#,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];

        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let card = app.view.workspace_card_areas[0].rect;
        let mut renderer = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        renderer
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = renderer.backend().buffer();

        let content_width = area.width - 1;
        assert_eq!(card.height, 1);
        assert_eq!(row_text(buffer, card.y, content_width), " one");
        assert_eq!(row_text(buffer, card.y + 1, content_width), "");
    }

    #[test]
    fn space_occurrence_style_applies_without_styling_separator() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.spaces]
rows = [[{ token = "$hype", fg = "#abcdef", bold = true, dim = false }, "workspace"]]
"##,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        app.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("hype".into(), Some("HI".into()))]),
            None,
            std::time::Instant::now(),
        );

        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let row = app.view.workspace_card_areas[0].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let h = buffer[(find_symbol_x(buffer, row, 25, "H"), row)].style();
        let i = buffer[(find_symbol_x(buffer, row, 25, "I"), row)].style();
        let separator = buffer[(find_symbol_x(buffer, row, 25, "·"), row)].style();

        for style in [h, i] {
            assert_eq!(style.fg, Some(ratatui::style::Color::Rgb(0xab, 0xcd, 0xef)));
            assert!(style.add_modifier.contains(Modifier::BOLD));
            assert!(!style.add_modifier.contains(Modifier::DIM));
            assert_eq!(style.bg, Some(app.palette.surface_dim));
        }
        assert_eq!(separator.fg, Some(app.palette.overlay0));
        assert!(separator.add_modifier.contains(Modifier::DIM));
        assert!(!separator.add_modifier.contains(Modifier::BOLD));
        assert_eq!(separator.bg, Some(app.palette.surface_dim));
    }

    /// Renders one Space row whose sole token carries `style_table`, and returns
    /// the drawn style of the metadata value plus the app it rendered from.
    fn render_styled_space_token(
        style_table: &str,
        animation_tick: u32,
    ) -> (ratatui::style::Style, crate::app::state::AppState) {
        let config: crate::config::Config =
            toml::from_str(&format!("[ui.sidebar.spaces]\nrows = [[{style_table}]]\n"))
                .expect("styled space config");
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        app.animation_tick = animation_tick;
        app.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("dot".into(), Some("Z".into()))]),
            None,
            std::time::Instant::now(),
        );

        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let row = app.view.workspace_card_areas[0].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let style = buffer[(find_symbol_x(buffer, row, 25, "Z"), row)].style();
        (style, app)
    }

    #[test]
    fn static_token_attributes_render_on_the_sidebar() {
        let (style, _) = render_styled_space_token(
            r##"{ token = "$dot", fg = "#a6e3a1", bg = "#1e1e2e", italic = true, underline = true }"##,
            0,
        );

        assert_eq!(style.fg, Some(ratatui::style::Color::Rgb(0xa6, 0xe3, 0xa1)));
        assert_eq!(style.bg, Some(ratatui::style::Color::Rgb(0x1e, 0x1e, 0x2e)));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!style.add_modifier.contains(Modifier::REVERSED));

        let (reversed, _) = render_styled_space_token(r##"{ token = "$dot", reverse = true }"##, 0);
        assert!(reversed.add_modifier.contains(Modifier::REVERSED));

        let (cleared, _) = render_styled_space_token(
            r##"{ token = "$dot", italic = false, underline = false, reverse = false }"##,
            0,
        );
        assert!(!cleared
            .add_modifier
            .intersects(Modifier::ITALIC | Modifier::UNDERLINED | Modifier::REVERSED));
    }

    #[test]
    fn pulse_emphasis_ramps_the_token_color_toward_the_panel_background() {
        let pulse = r##"{ token = "$dot", fg = "#a6e3a1", emphasis = "pulse" }"##;
        let (peak, app) = render_styled_space_token(pulse, 0);
        let (trough, _) = render_styled_space_token(pulse, PULSE_HALF_CYCLE_FRAMES);
        let (mid, _) = render_styled_space_token(pulse, PULSE_HALF_CYCLE_FRAMES / 2);
        let (returned, _) = render_styled_space_token(pulse, PULSE_HALF_CYCLE_FRAMES * 2);
        let (calm, _) = render_styled_space_token(r##"{ token = "$dot", fg = "#a6e3a1" }"##, 0);

        // The peak of the pulse is the token's own configured color, so a
        // freshly armed pulse starts out indistinguishable from a calm token.
        assert_eq!(peak.fg, calm.fg);
        assert_eq!(returned.fg, calm.fg);

        let Some(ratatui::style::Color::Rgb(pr, pg, pb)) = peak.fg else {
            panic!("expected rgb peak, got {:?}", peak.fg);
        };
        let Some(ratatui::style::Color::Rgb(mr, mg, mb)) = mid.fg else {
            panic!("expected rgb midpoint, got {:?}", mid.fg);
        };
        let Some(ratatui::style::Color::Rgb(tr, tg, tb)) = trough.fg else {
            panic!("expected rgb trough, got {:?}", trough.fg);
        };
        let Some(ratatui::style::Color::Rgb(br, bg, bb)) = Some(app.palette.panel_bg) else {
            panic!("expected rgb panel background");
        };

        // Monotonic ramp from the token color toward the panel background.
        for (peak_c, mid_c, trough_c, bg_c) in
            [(pr, mr, tr, br), (pg, mg, tg, bg), (pb, mb, tb, bb)]
        {
            let distance = |value: u8| i32::from(value).abs_diff(i32::from(bg_c));
            assert!(
                distance(trough_c) < distance(mid_c) && distance(mid_c) < distance(peak_c),
                "channel did not ramp toward the panel background: \
                 peak {peak_c} mid {mid_c} trough {trough_c} bg {bg_c}"
            );
        }
        // Still readable at the trough: a partial fade, not a disappearing act.
        assert_ne!(trough.fg, Some(app.palette.panel_bg));

        // Emphasis animates by redrawing color, never with SGR blink.
        for style in [peak, mid, trough] {
            assert!(!style
                .add_modifier
                .intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK));
        }
    }

    #[test]
    fn pulse_fade_is_a_symmetric_ramp_peaking_at_tick_zero() {
        assert_eq!(pulse_fade(0), 0.0);
        assert_eq!(pulse_fade(PULSE_HALF_CYCLE_FRAMES), PULSE_MAX_FADE);
        assert_eq!(pulse_fade(PULSE_HALF_CYCLE_FRAMES * 2), 0.0);
        for offset in 1..PULSE_HALF_CYCLE_FRAMES {
            assert_eq!(
                pulse_fade(PULSE_HALF_CYCLE_FRAMES - offset),
                pulse_fade(PULSE_HALF_CYCLE_FRAMES + offset),
                "ramp is not symmetric at offset {offset}"
            );
        }
        const { assert!(PULSE_MAX_FADE < 1.0, "a pulse must never fully vanish") };
    }

    #[test]
    fn calm_configurations_render_identically_as_the_clock_advances() {
        let render_at = |tick: u32| {
            let mut app = crate::app::state::AppState::test_new();
            app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
            app.active = Some(0);
            app.mode = Mode::Terminal;
            app.animation_tick = tick;
            app.ensure_test_terminals();

            let area = Rect::new(0, 0, 26, 20);
            app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
            let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            terminal.backend().buffer().clone()
        };

        let baseline = render_at(0);
        for tick in [1, PULSE_HALF_CYCLE_FRAMES, 12345] {
            assert_eq!(
                render_at(tick),
                baseline,
                "default sidebar changed at animation tick {tick}"
            );
        }
    }

    #[test]
    fn occurrence_foreground_flattens_composite_git_status_colors() {
        let config: crate::config::Config = toml::from_str(
            r##"[ui.sidebar.spaces]
rows = [[{ token = "git_status", fg = "#123456" }]]
"##,
        )
        .unwrap();
        let spans = resolved_token_spans(
            &[ResolvedToken {
                kind: ResolvedTokenKind::GitStatus {
                    ahead: 2,
                    behind: 1,
                },
                style: config.ui.sidebar.spaces.rows[0][0].parts().1,
            }],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &crate::app::state::AppState::test_new().palette,
            0,
            20,
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "↑2 ↓1"
        );
        assert!(spans
            .iter()
            .all(|span| { span.style.fg == Some(ratatui::style::Color::Rgb(0x12, 0x34, 0x56)) }));
    }

    #[test]
    fn oversized_space_layout_is_clipped_to_the_section_body() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]; 6];
        // A six-row layout in a five-row body, so the clip is what is under
        // test rather than the row count.
        let area = Rect::new(0, 0, 20, 8);
        let workspace_area = workspace_list_rect(area);
        let body = workspace_list_body_rect(workspace_area, false);

        let metrics = workspace_list_scroll_metrics(&app, workspace_area);
        let (cards, _) = compute_workspace_list_areas(&app, area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 0);
        assert_eq!(cards[0].rect.height, body.height);
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_and_hidden_from(&app, &runtime_registry).0;
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    #[test]
    fn workspace_list_truncates_cjk_branch_without_panic() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("feature/中文-分支-644".into());
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            entry_idx: 0,
            agent: None,
            ws_idx: 0,
            rect: Rect::new(0, 1, 15, 2),
            indented: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(15, 6)).expect("test terminal");
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_workspace_list(&app, &runtimes, frame, Rect::new(0, 0, 15, 6), false)
            })
            .expect("workspace list should render");
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            repo_name: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn desktop_worktree_tree_aligns_parents_and_marks_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];
        app.sidebar_spaces.rows = vec![vec![
            crate::config::SpaceSidebarToken::StateIcon,
            crate::config::SpaceSidebarToken::Workspace,
        ]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cards = &app.view.workspace_card_areas;
        let parent_name_x = find_symbol_x(buffer, cards[0].rect.y, cards[0].rect.width, "m");
        let plain_name_x = find_symbol_x(buffer, cards[3].rect.y, cards[3].rect.width, "n");
        assert_eq!(parent_name_x, plain_name_x);
        assert_eq!(buffer[(cards[1].rect.x + 3, cards[1].rect.y)].symbol(), "├");
        assert_eq!(buffer[(cards[2].rect.x + 3, cards[2].rect.y)].symbol(), "└");
        assert_eq!(
            buffer[(cards[0].rect.x + cards[0].rect.width - 1, cards[0].rect.y)].symbol(),
            "▾"
        );
    }

    /// A three-space worktree tree — one parent, two indented children — with a
    /// relation signal of `kind` advanced to `stop` on the first child.
    ///
    /// Returns the rendered buffer alongside the app, so a caller can compare
    /// two stops, or a signalled render against an unsignalled one, cell by
    /// cell.
    fn render_signalled_tree(
        kind: Option<crate::app::relation_signal::RelationSignalKind>,
        stop: u8,
    ) -> (ratatui::buffer::Buffer, AppState) {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];
        app.sidebar_spaces.rows = vec![vec![
            crate::config::SpaceSidebarToken::StateIcon,
            crate::config::SpaceSidebarToken::Workspace,
        ]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);

        if let Some(kind) = kind {
            let now = std::time::Instant::now();
            let carrier = app.workspaces[1].id.clone();
            app.relation_signals
                .accept("firstmate", None, kind, carrier, None, now)
                .expect("a fresh row always accepts its first signal");
            // Walk the clock to the requested stop the same way the runtime
            // tick does, rather than reaching into the signal's internals.
            let step = crate::app::relation_signal::DEFAULT_SIGNAL_TTL
                / u32::from(crate::app::relation_signal::SIGNAL_STOPS);
            app.relation_signals
                .advance(now + step * u32::from(stop) + std::time::Duration::from_millis(1));
        }

        let list_area = workspace_list_rect(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (buffer, app)
    }

    fn changed_cells(
        before: &ratatui::buffer::Buffer,
        after: &ratatui::buffer::Buffer,
    ) -> Vec<(u16, u16)> {
        let area = before.area;
        assert_eq!(area, after.area, "frames must share a geometry to diff");
        (area.y..area.y + area.height)
            .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
            .filter(|(x, y)| {
                before[(*x, *y)].symbol() != after[(*x, *y)].symbol()
                    || before[(*x, *y)].style() != after[(*x, *y)].style()
            })
            .collect()
    }

    #[test]
    fn a_signal_frame_damages_only_the_branch_line_of_the_row_it_travels() {
        use crate::app::relation_signal::{RelationSignalKind, CONNECTOR_CELLS, SIGNAL_STOPS};

        // The whole route: from no signal, through every stop, back to none.
        let mut frames = vec![render_signalled_tree(None, 0).0];
        for stop in 0..SIGNAL_STOPS {
            frames.push(render_signalled_tree(Some(RelationSignalKind::Transfer), stop).0);
        }
        frames.push(render_signalled_tree(None, 0).0);

        let (_, app) = render_signalled_tree(None, 0);
        let child = app.view.workspace_card_areas[1].rect;
        // Three connector cells sit after a three-column indent, and the state
        // icon is the first token drawn after them.
        let first = child.x + 3;
        let last = first + u16::from(CONNECTOR_CELLS);

        let mut moved = 0;
        for pair in frames.windows(2) {
            let changed = changed_cells(&pair[0], &pair[1]);
            for (x, y) in &changed {
                assert_eq!(
                    *y, child.y,
                    "a signal must not touch any row but the one it travels"
                );
                assert!(
                    (first..=last).contains(x),
                    "a signal must not touch any column but its branch line and state icon; \
                     changed x={x} outside {first}..={last}"
                );
            }
            moved += changed.len();
        }
        assert!(moved > 0, "the signal has to actually draw something");
    }

    #[test]
    fn a_row_draws_the_same_characters_whether_or_not_its_signal_ever_runs() {
        use crate::app::relation_signal::{RelationSignalKind, SIGNAL_STOPS};

        let (calm, _) = render_signalled_tree(None, 0);
        for kind in [RelationSignalKind::Transfer, RelationSignalKind::Completed] {
            for stop in 0..SIGNAL_STOPS {
                let (signalled, _) = render_signalled_tree(Some(kind), stop);
                for y in calm.area.y..calm.area.y + calm.area.height {
                    for x in calm.area.x..calm.area.x + calm.area.width {
                        assert_eq!(
                            calm[(x, y)].symbol(),
                            signalled[(x, y)].symbol(),
                            "a signal changed a character at ({x}, {y}); it may only change style, \
                             so that a skipped or interrupted signal cannot leave a row wrong"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_transfer_and_a_completion_run_the_branch_line_in_opposite_directions() {
        use crate::app::relation_signal::{RelationSignalKind, CONNECTOR_CELLS};

        let (_, app) = render_signalled_tree(None, 0);
        let child = app.view.workspace_card_areas[1].rect;
        let accent = app.palette.accent;
        let lit_cell = |kind, stop| {
            let (buffer, _) = render_signalled_tree(Some(kind), stop);
            (0..=CONNECTOR_CELLS).find(|cell| {
                buffer[(child.x + 3 + u16::from(*cell), child.y)].style().bg == Some(accent)
            })
        };

        let inbound: Vec<Option<u8>> = (0..CONNECTOR_CELLS)
            .map(|stop| lit_cell(RelationSignalKind::Transfer, stop))
            .collect();
        let outbound: Vec<Option<u8>> = (1..=CONNECTOR_CELLS)
            .map(|stop| lit_cell(RelationSignalKind::Completed, stop))
            .collect();

        assert_eq!(inbound, vec![Some(0), Some(1), Some(2)]);
        assert_eq!(outbound, vec![Some(2), Some(1), Some(0)]);
    }

    #[test]
    fn an_arriving_signal_emphasises_the_state_icon_without_recolouring_it() {
        use crate::app::relation_signal::{RelationSignalKind, CONNECTOR_CELLS, SIGNAL_STOPS};

        // A transfer lands on the icon at its last stop; a completion starts
        // there, so both directions are covered by the same assertion.
        for (kind, arrival) in [
            (RelationSignalKind::Transfer, SIGNAL_STOPS - 1),
            (RelationSignalKind::Completed, 0),
        ] {
            let (buffer, app) = render_signalled_tree(Some(kind), arrival);
            let child = app.view.workspace_card_areas[1].rect;
            let icon = &buffer[(child.x + 3 + u16::from(CONNECTOR_CELLS), child.y)];

            let workspace = &app.workspaces[1];
            let (state, seen) = workspace.aggregate_state(&app.terminals);
            let (expected_symbol, expected_style) = state_dot(state, seen, &app.palette);

            assert_eq!(icon.symbol(), expected_symbol);
            assert_eq!(
                icon.style().bg,
                expected_style.fg,
                "the arrival block has to be the state's own colour, because that colour is the \
                 state and a decoration may not overwrite it"
            );
            assert!(icon.style().add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn desktop_tree_indents_derived_worktree_children_like_flow_created_ones() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            derived_space_member("demo", "/lab/demo", false),
            derived_space_member("feature-a", "/lab/demo-feature-a", true),
            derived_space_member("feature-b", "/lab/demo-feature-b", true),
            Workspace::test_new("home"),
        ];
        app.sidebar_spaces.rows = vec![vec![
            crate::config::SpaceSidebarToken::StateIcon,
            crate::config::SpaceSidebarToken::Workspace,
        ]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cards = &app.view.workspace_card_areas;
        assert_eq!(cards.len(), 4);
        // The main checkout and the non-repository row share the parent column.
        let parent_name_x = find_symbol_x(buffer, cards[0].rect.y, cards[0].rect.width, "d");
        let plain_name_x = find_symbol_x(buffer, cards[3].rect.y, cards[3].rect.width, "h");
        assert_eq!(parent_name_x, plain_name_x);
        // Both linked worktrees render as indented children of that checkout.
        assert_eq!(buffer[(cards[1].rect.x + 3, cards[1].rect.y)].symbol(), "├");
        assert_eq!(buffer[(cards[2].rect.x + 3, cards[2].rect.y)].symbol(), "└");
        assert_eq!(
            buffer[(cards[0].rect.x + cards[0].rect.width - 1, cards[0].rect.y)].symbol(),
            "▾"
        );
    }

    #[test]
    fn desktop_worktree_connector_uses_full_list_at_viewport_boundary() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        // Two body rows: the third child is a real entry that is off screen,
        // which is the whole point - the connector has to read the full list,
        // not the cards that happened to fit.
        let area = Rect::new(0, 0, 30, 5);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        assert_eq!(app.view.workspace_card_areas.len(), 2);
        let list_area = workspace_list_rect(area);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let child = app.view.workspace_card_areas[1];
        assert_eq!(
            terminal.backend().buffer()[(child.rect.x + 3, child.rect.y)].symbol(),
            "├"
        );
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.sidebar_spaces.row_gap = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn space_row_gap_preserves_compact_worktree_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 2;

        let (spacious, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert_eq!(
            spacious[1].rect.y,
            spacious[0].rect.y + spacious[0].rect.height + 2
        );
        assert_eq!(
            spacious[2].rect.y,
            spacious[1].rect.y + spacious[1].rect.height
        );
        assert_eq!(
            spacious[3].rect.y,
            spacious[2].rect.y + spacious[2].rect.height + 2
        );
        let spacious_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 7));
        assert_eq!(spacious_metrics.viewport_rows, 2);
        assert_eq!(spacious_metrics.max_offset_from_bottom, 2);

        app.sidebar_spaces.row_gap = 0;
        let (packed, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert!(packed
            .windows(2)
            .all(|pair| pair[1].rect.y == pair[0].rect.y + pair[0].rect.height));
        let packed_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 7));
        assert_eq!(packed_metrics.viewport_rows, 4);
        assert_eq!(packed_metrics.max_offset_from_bottom, 0);
    }

    #[test]
    fn packed_workspace_drag_indicator_overlays_an_internal_boundary() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area);
        let indicator_row = workspace_drop_indicator_row(
            &app,
            &app.view.workspace_card_areas,
            list_area,
            crate::app::state::WorkspaceDropTarget::Before(2),
        )
        .unwrap();
        assert_eq!(indicator_row, app.view.workspace_card_areas[1].rect.y);
        app.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::WorkspaceReorder {
                source_ws_idx: 0,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::Before(2)),
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[(list_area.x, indicator_row)].symbol(),
            "─"
        );
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(grouping(&entries), vec![(0, false), (1, false),]);
    }

    #[test]
    fn compact_space_group_scroll_clamps_when_all_entries_fit() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(app.workspace_scroll, 0);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[2].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (1, true),]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (2, true), (1, false),]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (1, false),]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (2, true), (1, false),]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (1, false),]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (1, true),]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(grouping(&workspace_list_entries(&app)), vec![(0, false)]);
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![(0, false), (1, true),]
        );
    }

    /// A member of the `repo-key` space with an explicit linked flag, so tests
    /// can build a checkout that has more than one workspace open in it.
    fn space_member(name: &str, checkout: &str, linked: bool) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            checkout_path: std::path::PathBuf::from(checkout),
            is_linked_worktree: linked,
        });
        ws
    }

    /// Which Space a row draws and whether it is a worktree child. Connector
    /// depth is layout the tree walk derives from the whole list, so grouping
    /// tests compare identity rather than pinning the drawing to a shape they
    /// are not about.
    fn grouping(entries: &[WorkspaceListEntry]) -> Vec<(usize, bool)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                WorkspaceListEntry::Workspace {
                    ws_idx, indented, ..
                } => Some((*ws_idx, *indented)),
                WorkspaceListEntry::Agent { .. } => None,
            })
            .collect()
    }

    fn top_level(ws_idx: usize) -> (usize, bool) {
        (ws_idx, false)
    }

    fn child(ws_idx: usize) -> (usize, bool) {
        (ws_idx, true)
    }

    /// A member of the `repo-key` space whose membership was derived from its
    /// own directory rather than recorded by Herdr's worktree flow.
    fn derived_space_member(
        name: &str,
        checkout: &str,
        linked: bool,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.derived_worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            checkout_path: std::path::PathBuf::from(checkout),
            is_linked_worktree: linked,
        });
        ws
    }

    #[test]
    fn derived_worktree_members_group_and_indent_under_their_main_checkout() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            derived_space_member("main", "/repo/herdr", false),
            derived_space_member("issue", "/repo/herdr-issue", true),
            derived_space_member("review", "/repo/herdr-review", true),
            Workspace::test_new("home"),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), child(1), child(2), top_level(3)]
        );
        assert_eq!(
            app.worktree_space_group("repo-key")
                .map(|group| group.parent_idx),
            Some(0)
        );
    }

    #[test]
    fn derived_and_flow_created_members_of_one_repo_share_a_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            derived_space_member("main", "/repo/herdr", false),
            space_member("flow-issue", "/repo/herdr-issue", true),
            derived_space_member("derived-review", "/repo/herdr-review", true),
        ];

        let group = app
            .worktree_space_group("repo-key")
            .expect("derived and flow-created members form one group");
        assert_eq!(group.parent_idx, 0);
        assert_eq!(group.children, vec![1, 2]);
        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), child(1), child(2)]
        );
    }

    #[test]
    fn a_flow_created_membership_is_not_replaced_by_the_derived_one() {
        let mut app = AppState::test_new();
        let mut child_ws = space_member("issue", "/repo/herdr-issue", true);
        // A stale derivation naming a different repo must stay invisible while
        // the flow-recorded membership is present.
        child_ws.derived_worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "other-repo-key".into(),
            label: "other".into(),
            repo_root: std::path::PathBuf::from("/repo/other"),
            checkout_path: std::path::PathBuf::from("/repo/other"),
            is_linked_worktree: false,
        });
        app.workspaces = vec![space_member("main", "/repo/herdr", false), child_ws];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), child(1)]
        );
    }

    #[test]
    fn a_derived_non_repository_workspace_stays_ungrouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            derived_space_member("main", "/repo/herdr", false),
            derived_space_member("issue", "/repo/herdr-issue", true),
            Workspace::test_new("home"),
        ];

        assert!(app.workspaces[2].worktree_space().is_none());
        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), child(1), top_level(2)]
        );
    }

    #[test]
    fn a_second_derived_main_checkout_leaves_exactly_one_group_parent() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            derived_space_member("mainA", "/repo/herdr", false),
            derived_space_member("issue", "/repo/herdr-issue", true),
            derived_space_member("mainB", "/repo/herdr", false),
        ];

        let group = app.worktree_space_group("repo-key").expect("one group");
        assert_eq!(group.parent_idx, 0);
        assert_eq!(group.children, vec![1]);
        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), child(1), top_level(2)]
        );
    }

    #[test]
    fn second_main_checkout_workspace_keeps_its_own_top_level_row() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            space_member("mainA", "/repo/herdr", false),
            space_member("issue", "/repo/herdr-issue", true),
            space_member("mainB", "/repo/herdr", false),
            space_member("review", "/repo/herdr-review", true),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), child(1), child(3), top_level(2)]
        );
    }

    #[test]
    fn only_the_group_parent_carries_the_collapse_chevron() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            space_member("mainA", "/repo/herdr", false),
            space_member("issue", "/repo/herdr-issue", true),
            space_member("mainB", "/repo/herdr", false),
        ];

        assert_eq!(
            workspace_parent_group_state(&app, 0),
            Some(("repo-key".to_string(), false))
        );
        assert_eq!(workspace_parent_group_state(&app, 1), None);
        assert_eq!(workspace_parent_group_state(&app, 2), None);
    }

    #[test]
    fn collapsed_group_ignores_a_selected_peer_main_checkout_workspace() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            space_member("mainA", "/repo/herdr", false),
            space_member("issue", "/repo/herdr-issue", true),
            space_member("mainB", "/repo/herdr", false),
        ];
        app.mode = Mode::Navigate;
        app.selected = 2;
        app.active = Some(2);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), top_level(2)]
        );
    }

    #[test]
    fn main_checkout_members_without_a_linked_worktree_do_not_form_a_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            space_member("mainA", "/repo/herdr", false),
            space_member("mainB", "/repo/herdr", false),
        ];

        assert_eq!(
            grouping(&workspace_list_entries(&app)),
            vec![top_level(0), top_level(1)]
        );
        assert_eq!(workspace_parent_group_state(&app, 0), None);
    }

    #[test]
    fn desktop_tree_never_indents_a_second_main_checkout_workspace() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            space_member("mainA", "/repo/herdr", false),
            space_member("issue", "/repo/herdr-issue", true),
            space_member("mainB", "/repo/herdr", false),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cards = &app.view.workspace_card_areas;
        assert_eq!(cards[2].ws_idx, 2);
        assert!(!cards[2].indented);
        assert_eq!(buffer[(cards[1].rect.x + 3, cards[1].rect.y)].symbol(), "└");
        let parent_name_x = find_symbol_x(buffer, cards[0].rect.y, cards[0].rect.width, "m");
        let peer_name_x = find_symbol_x(buffer, cards[2].rect.y, cards[2].rect.width, "m");
        assert_eq!(peer_name_x, parent_name_x);
    }

    /// A two-row layout in both panels where every row mixes builtin tokens with
    /// inline-styled custom `$tokens`.
    ///
    /// This is the shape a live sidebar was reported to have lost - second row
    /// gone, rows reading as unstyled text - so it is pinned here in both
    /// panels at once. A row keeps its line as long as one token on it
    /// resolves, and an inline `fg`/`bold` reaches the drawn cell; both hold
    /// whether the value comes from a builtin token or from published metadata.
    ///
    /// The elision this does NOT cover is the intended one: a row whose only
    /// tokens are custom values that were never published has nothing to draw
    /// and is dropped, which looks identical to the row having disappeared.
    /// `missing_custom_tokens_elide_rows_and_separators` in `tokens.rs` owns
    /// that case.
    const TWO_ROW_STYLED_CONFIG: &str = r##"
[ui.sidebar.spaces]
rows = [
  ["state_icon", { token = "$doing", fg = "#e0def4" }, { token = "$context", fg = "#c4a7e7", bold = true }],
  ["workspace"],
]

[ui.sidebar.agents]
rows = [
  ["state_icon", "terminal_title_stripped", { token = "$context", fg = "#c4a7e7", bold = true }],
  [{ token = "$project", fg = "#9ccfd8" }],
]
"##;

    fn rose_pine(r: u8, g: u8, b: u8) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(r, g, b)
    }

    /// One space and one agent carrying every custom token the config names.
    fn app_with_two_row_styled_config() -> crate::app::state::AppState {
        let config: crate::config::Config =
            toml::from_str(TWO_ROW_STYLED_CONFIG).expect("two-row styled sidebar config");
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_agents = config.ui.sidebar.agents;
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let pane_terminal = app.terminals.get_mut(&terminal_id).unwrap();
        pane_terminal.detected_agent = Some(Agent::Claude);
        pane_terminal.state = AgentState::Working;
        pane_terminal.set_terminal_title(Some("⠋ Wiring".into()));
        pane_terminal.metadata_tokens.patch(
            std::collections::HashMap::from([
                ("context".into(), Some("Ctx".into())),
                ("project".into(), Some("Proj".into())),
            ]),
            None,
            std::time::Instant::now(),
        );
        app.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([
                ("doing".into(), Some("Doing".into())),
                ("context".into(), Some("Ctx".into())),
            ]),
            None,
            std::time::Instant::now(),
        );
        app
    }

    #[test]
    fn two_row_styled_space_config_renders_both_rows_with_inline_styles() {
        let mut app = app_with_two_row_styled_config();

        let area = Rect::new(0, 0, 40, 24);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let card = app.view.workspace_card_areas[0].rect;
        let mut renderer = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        renderer
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = renderer.backend().buffer();

        assert_eq!(card.height, 2, "both configured Space rows are laid out");
        let first = row_text(buffer, card.y, card.width);
        let second = row_text(buffer, card.y + 1, card.width);
        assert!(first.contains("Doing"), "first Space row: {first:?}");
        assert!(first.contains("Ctx"), "first Space row: {first:?}");
        assert!(
            second.contains("one"),
            "second Space row is missing: {second:?}"
        );

        let doing = buffer[(find_symbol_x(buffer, card.y, card.width, "D"), card.y)].style();
        assert_eq!(doing.fg, Some(rose_pine(0xe0, 0xde, 0xf4)));
        let context = buffer[(find_symbol_x(buffer, card.y, card.width, "C"), card.y)].style();
        assert_eq!(context.fg, Some(rose_pine(0xc4, 0xa7, 0xe7)));
        assert!(context.add_modifier.contains(Modifier::BOLD));
    }

    /// Render one configured row at `max_width` and return the drawn text, so a
    /// narrow row can be asserted on the way the captain actually reads it.
    fn narrow_row(resolved: &[tokens::ResolvedToken], max_width: usize) -> String {
        let palette = crate::app::state::Palette::catppuccin();
        resolved_token_spans(
            resolved,
            ("●", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &palette,
            0,
            max_width,
        )
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
    }

    /// The captain's Spaces row 1: `state_icon`, `$doing`, `$context`. At their
    /// sidebar width a grouped child gets 16 columns for the whole row.
    fn doing_row(doing: &str, context: &str) -> Vec<tokens::ResolvedToken> {
        vec![
            tokens::ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
            tokens::ResolvedToken::unstyled(ResolvedTokenKind::Custom(doing.to_string())),
            tokens::ResolvedToken::unstyled(ResolvedTokenKind::Custom(context.to_string())),
        ]
    }

    #[test]
    fn overflowing_row_spends_the_middot_padding_on_the_flexible_token() {
        // 16 columns is what a grouped child row has at sidebar_width 23.
        let row = narrow_row(&doing_row("Own herding runtime work", "9%"), 16);

        // " · " costs three columns to draw one glyph of decoration; on a row that
        // has to truncate anyway those go back to $doing.
        assert_eq!(row, "● Own herdin… 9%");
        assert_eq!(display_width(&row), 16);
    }

    #[test]
    fn row_that_fits_keeps_its_middot() {
        // Widen the same row until everything fits and the decoration is free.
        let row = narrow_row(&doing_row("Own herding runtime work", "9%"), 40);

        assert_eq!(row, "● Own herding runtime work · 9%");
    }

    #[test]
    fn sibling_names_keep_the_part_that_tells_them_apart() {
        // Row 2 of a grouped child is a bare `workspace` token with 14 columns.
        let rendered = ["2ndmate-explore", "2ndmate-wallpanel", "2ndmate-scraper"].map(|name| {
            narrow_row(
                &[tokens::ResolvedToken::unstyled(
                    ResolvedTokenKind::Workspace(name.to_string()),
                )],
                14,
            )
        });

        // End-truncation spent the budget redrawing the shared `2ndmate-` prefix
        // and cut exactly where the names start to differ.
        assert_eq!(
            rendered,
            ["2ndmat…explore", "2ndmat…llpanel", "2ndmat…scraper"]
        );
        for row in &rendered {
            assert!(display_width(row) <= 14, "{row:?}");
        }
    }

    #[test]
    fn prose_tokens_still_truncate_from_the_end() {
        // A terminal title is written to be read front to back, so its head is
        // the part worth keeping - it must not pick up the name treatment.
        let row = narrow_row(
            &[tokens::ResolvedToken::unstyled(
                ResolvedTokenKind::TerminalTitle("Rebuild fitness RPG progression".to_string()),
            )],
            14,
        );

        assert_eq!(row, "Rebuild fitne…");
    }
}
