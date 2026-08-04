mod notifications;
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
use super::text::{display_width, display_width_u16, middle_elide, truncate_end};
use crate::app::agent_view::AgentViewHidden;
use crate::app::relation_signal::{RelationSignalKind, RelationSignalPhase};
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

/// The bottom row of the workspace panel, kept clear of the tree for the `new`
/// button and the collapse toggle.
const WORKSPACE_SECTION_FOOTER_ROWS: u16 = 1;

/// One row above the tree, drawn empty.
///
/// The `spaces` title and the second-mate drop-down that used to share this
/// space are gone, but the row itself is not free to reclaim: the tree's
/// drop-slot grid anchors "drop before this Space" on the row *above* a card,
/// so a tree flush to the panel's top edge has no row that means "above
/// everything" and cannot be reordered to first position. It is also the row
/// the usage readout is proposed to occupy.
const WORKSPACE_SECTION_HEADER_ROWS: u16 = 1;

/// Columns a state mark occupies, whatever state it is reporting.
///
/// The layout has to know how wide a row's icon is before it knows which icon
/// the row will draw — the height a row reserves is decided from the tokens
/// alone, one pass before the palette and the aggregate state are resolved.
/// `state_marks_are_one_column_wide` is the check that keeps this honest.
const STATE_MARK_WIDTH: usize = 1;

/// Cloneable because a row that has left is still drawn while it leaves: the
/// runtime keeps the last pass's rows in
/// [`crate::app::state::AppState::sidebar_tree_row_memory`] so a pane closing
/// does not take the only copy of its row with it.
#[derive(Clone)]
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
    /// Whether this is the last row drawn in its own column, picking `└` over
    /// `├`. See [`crate::app::agent_tree::Placement::is_last_child`] for how a
    /// row past [`crate::app::agent_tree::MAX_DISPLAY_DEPTH`] counts here.
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

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
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

fn workspace_row_height(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    indented: bool,
    content_width: usize,
) -> u16 {
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
    let rows = tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: &label,
            branch: ws.branch().as_deref(),
            state_text: state_label(state, seen),
            state_age: space_state_age(app, ws),
            ahead_behind: ws.git_ahead_behind(),
            dirty: ws.git_dirty(),
            pull_requests: ws.pull_requests(),
            terminal_title: terminal_title.raw.as_deref(),
            terminal_title_stripped: terminal_title.stripped.as_deref(),
            tokens: &token_values,
            suppress_git_details: indented,
        },
    );
    fold_token_lines(rows, content_width, None)
        .len()
        .max(1)
        .min(u16::MAX as usize) as u16
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
    content_width: usize,
) -> u16 {
    workspace_row_height(app, workspace, indented, content_width).min(body_height)
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
    workspace_list_entries_inner(app, false, &app.tree_root)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true, &app.tree_root)
}

/// The whole fleet's tree, whatever the sidebar is currently rooted on.
///
/// Reordering asks where a Space sits among *all* the roots, which is a fact
/// about the session rather than about this viewer's current view. Answering it
/// from a re-rooted tree would move a Space to a position derived from the two
/// or three rows that happen to be on screen.
pub(crate) fn workspace_list_entries_whole_fleet(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true, &crate::app::tree_view::TreeRoot::Fleet)
}

/// The tree name a row answers to, which is the handle an `owner` token uses.
///
/// `None` for a row that never named itself; such a row can be drawn and can be
/// owned, but nothing can be rooted on it because there is no name to hold.
fn entry_tree_handle(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
) -> Option<String> {
    match entry {
        // A worktree child is a checkout of its parent Space, not a node of the
        // ownership tree, so it is not a root anything can be re-rooted on.
        WorkspaceListEntry::Workspace { indented: true, .. } => None,
        WorkspaceListEntry::Workspace { ws_idx, .. } => space_tree_name(app, *ws_idx),
        WorkspaceListEntry::Agent { entry_idx, .. } => agents
            .get(*entry_idx)
            .and_then(|entry| entry.agent_name.clone()),
    }
}

/// The tree name one drawn row answers to, for a caller that has an entry but
/// not the agent list it was built against.
pub(crate) fn sidebar_tree_handle(app: &AppState, entry: &WorkspaceListEntry) -> Option<String> {
    entry_tree_handle(app, &sidebar_agent_entries(app), entry)
}

/// Cut `entries` down to the subtree `root` names, re-depthed onto rank 0.
///
/// The selected node takes the position the fleet's root held and its own
/// children take the position the second mates held — by being drawn there, not
/// by travelling there. A root that no longer names a row leaves the tree
/// untouched, so a mate whose Space is closed under an open view degrades back
/// to the whole fleet instead of blanking the panel.
fn re_root_entries(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entries: Vec<WorkspaceListEntry>,
    root: &crate::app::tree_view::TreeRoot,
) -> Vec<WorkspaceListEntry> {
    let handles: Vec<Option<String>> = entries
        .iter()
        .map(|entry| entry_tree_handle(app, agents, entry))
        .collect();
    let rows = entries
        .iter()
        .zip(&handles)
        .map(|(entry, handle)| (entry.depth(), handle.as_deref()));
    let Some(kept) = crate::app::tree_view::rooted_rows(rows, root) else {
        return entries;
    };

    kept.into_iter()
        .enumerate()
        .filter_map(|(position, row)| {
            let mut entry = entries.get(row.index)?.clone();
            let trimmed = usize::from(row.trimmed_levels);
            let (depth, ancestors, is_last_child) = match &mut entry {
                WorkspaceListEntry::Workspace {
                    depth,
                    ancestors_continue,
                    is_last_child,
                    ..
                }
                | WorkspaceListEntry::Agent {
                    depth,
                    ancestors_continue,
                    is_last_child,
                    ..
                } => (depth, ancestors_continue, is_last_child),
            };
            *depth = row.depth;
            // The rail is indexed by absolute level, so dropping the levels
            // above the new root is what keeps a child's `│` under its own
            // parent rather than under a column that is no longer drawn.
            let cut = trimmed.min(ancestors.len());
            ancestors.drain(..cut);
            if position == 0 {
                // The new root is the only row in its column.
                *is_last_child = true;
            }
            Some(entry)
        })
        .collect()
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
///
/// Rows that are still *leaving* are re-inserted by [`rows_with_departing`], so
/// the group a worker belonged to contracts when its exit finishes rather than
/// the instant its pane closes.
pub(crate) fn sidebar_agent_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    rows_with_departing(app, sidebar_agent_live_entries(app))
}

/// The rows a pane that exists right now would draw.
///
/// Live membership and nothing else: this is what the runtime publishes to the
/// animation engine, so a row that has left this list is exactly a row the
/// engine should start dismounting.
pub(crate) fn sidebar_agent_live_entries(app: &AppState) -> Vec<AgentPanelEntry> {
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

/// Put the rows that are mid-exit back among the live ones.
///
/// A sidebar group grows and shrinks because its rows arrive and leave, and a
/// row that leaves has to still be somewhere to be seen leaving. The rows the
/// last loop pass drew are kept in
/// [`crate::app::state::AppState::sidebar_tree_row_memory`]; a remembered row
/// whose pane is gone comes back here for exactly as long as the engine still
/// has a dismount to play for it, at the index it was standing in — so it
/// fades out of its own place in its own second mate's group instead of
/// jumping to the end of it first.
///
/// It still carries its `owner`, so the tree walk nests it under the same
/// second mate it always had: one group contracts and the others do not move.
///
/// With no row exit configured, memory is never published and this is the
/// identity function — an unconfigured Herdr draws exactly the rows it always
/// did.
pub(crate) fn rows_with_departing(
    app: &AppState,
    mut live: Vec<AgentPanelEntry>,
) -> Vec<AgentPanelEntry> {
    if app.sidebar_tree_row_memory.is_empty() {
        return live;
    }
    let present: std::collections::HashSet<crate::layout::PaneId> =
        live.iter().map(|entry| entry.pane_id).collect();
    for (position, remembered) in app.sidebar_tree_row_memory.iter().enumerate() {
        if present.contains(&remembered.pane_id) {
            continue;
        }
        // The engine is the authority on whether this row has anything left to
        // play: it drops an element the moment its dismount finishes, and it
        // holds none at all when nothing is configured to animate.
        if app
            .anim
            .frame(&crate::anim::ElementId::agent_row(remembered.pane_id), None)
            .is_none()
        {
            continue;
        }
        live.insert(position.min(live.len()), remembered.clone());
    }
    live
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

fn workspace_list_entries_inner(
    app: &AppState,
    force_expanded: bool,
    root: &crate::app::tree_view::TreeRoot,
) -> Vec<WorkspaceListEntry> {
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

    let agents = sidebar_agent_entries(app);
    let entries = arrange_space_tree(app, &blocks, &agents);
    if root.is_fleet() {
        return entries;
    }
    re_root_entries(app, &agents, entries, root)
}

pub(crate) fn workspace_list_rect(area: Rect) -> Rect {
    sidebar_content_rect(area)
}

/// The tree owns every row of the panel between the empty header row and the
/// footer the `new` button and the collapse toggle sit on.
pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0
        || area.height <= WORKSPACE_SECTION_HEADER_ROWS + WORKSPACE_SECTION_FOOTER_ROWS
    {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(WORKSPACE_SECTION_FOOTER_ROWS);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

/// Columns one tree row spends on rails and connector before its first token.
///
/// The single place the prefix is measured. The layout subtracts it to decide
/// how many lines a row needs and the renderer subtracts it again to decide how
/// much each line may draw, so the two cannot disagree about how much room the
/// row had - which is the whole reason a row's height may depend on its width
/// at all.
fn tree_prefix_width(depth: u8, indented: bool, row_index: usize) -> usize {
    use crate::app::agent_tree::display_depth;
    if indented {
        // A worktree child brings its own legacy connector, so the ownership
        // rails above it are laid down first and its connector sits after them.
        3 * display_depth(depth.saturating_sub(1)) as usize + if row_index == 0 { 6 } else { 8 }
    } else {
        let depth = display_depth(depth) as usize;
        match (depth, row_index) {
            (0, 0) => 1,
            (0, _) => 3,
            (_, 0) => 3 * depth + 1,
            (_, _) => 3 * depth + 3,
        }
    }
}

/// The panel width every fold decision is measured against.
///
/// Deliberately the narrow one, as though the scrollbar were always showing.
/// Whether it *is* showing depends on how tall the rows are, so a fold that
/// measured the real width would be feeding its own input: one folded row frees
/// a line, which can retire the scrollbar, which widens the panel, which folds
/// another row. Measuring against the narrow width costs at most one column of
/// slack and makes the layout a fixed point.
fn row_fold_width(list_area: Rect) -> u16 {
    workspace_list_body_rect(list_area, true).width
}

/// Columns a row has for its tokens once its prefix and any trailing control
/// are taken out.
fn row_content_width(fold_width: u16, depth: u8, indented: bool, trailing_width: usize) -> usize {
    (fold_width as usize)
        .saturating_sub(tree_prefix_width(depth, indented, 0))
        .saturating_sub(trailing_width)
}

/// Columns reserved at the right edge of a Space row's first line for the
/// worktree group chevron, which is drawn over the row rather than laid out in
/// it.
fn space_trailing_width(app: &AppState, ws_idx: usize, indented: bool) -> usize {
    2 * usize::from(!indented && workspace_parent_group_state(app, ws_idx).is_some())
}

/// Columns row 0 of this entry gives up to a worker-summary badge.
///
/// Zero for every row that owns no worker with a published summary, which is
/// every row in a fleet that never calls `pane report-metadata --token summary`.
fn entry_badge_width(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
    fold_width: u16,
) -> usize {
    let Some(name) = entry_tree_name(app, agents, entry) else {
        return 0;
    };
    let count = crate::app::worker_summary::summary_count_for_owner(agents, &name);
    if count == 0 {
        return 0;
    }
    usize::from(worker_summary_badge_width(count, fold_width))
}

/// Columns one tree row has for its tokens, from the entry alone.
///
/// Every control drawn *over* the row rather than laid out in it - the worktree
/// chevron, the worker-summary badge - is subtracted here, because the fold is
/// only allowed to buy a row back, never to spend a character.
fn list_entry_content_width(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
    fold_width: u16,
) -> usize {
    let badge = entry_badge_width(app, agents, entry, fold_width);
    match entry {
        WorkspaceListEntry::Workspace {
            ws_idx, indented, ..
        } => row_content_width(
            fold_width,
            entry.depth(),
            *indented,
            space_trailing_width(app, *ws_idx, *indented) + badge,
        ),
        WorkspaceListEntry::Agent { .. } => {
            row_content_width(fold_width, entry.depth(), false, badge)
        }
    }
}

/// The reserved row above the tree, the session status draws on.
///
/// It is the panel's own content column narrowed to a single row, so a status
/// that fits here fits the tree below it and nothing on it can reach the
/// divider bar. Empty whenever the panel is too short to hold a tree at all:
/// the row exists to sit above cards, and with no cards there is no "above".
pub(crate) fn workspace_list_header_rect(area: Rect) -> Rect {
    if area.width == 0
        || area.height <= WORKSPACE_SECTION_HEADER_ROWS + WORKSPACE_SECTION_FOOTER_ROWS
    {
        return Rect::default();
    }
    Rect::new(area.x, area.y, area.width, WORKSPACE_SECTION_HEADER_ROWS)
}

/// Below this many columns the header row stays empty.
///
/// A status elided down to a glyph and an ellipsis is not a shorter readout,
/// it is a different one that happens to look like text, and the row reads
/// better blank than wrong.
const MIN_SESSION_STATUS_WIDTH: u16 = 6;

/// Columns between the signal bar and whatever shares its row.
const HEADER_ROW_GAP: u16 = 2;

/// Draw the panel's reserved header row: the fleet signal bar, then the session
/// status in whatever is left.
///
/// The bar takes its columns from the left and the status right-aligns in the
/// remainder, so the two never overlap and neither has to know the other's
/// content. The bar is measured first on purpose: it is a fixed readout whose
/// positions a reader learns, while the status is arbitrary publisher text that
/// already knows how to elide and how to drop when it cannot be read.
fn render_header_row(app: &AppState, frame: &mut Frame, area: Rect) {
    let header = workspace_list_header_rect(area);
    if header.height == 0 {
        return;
    }

    let bar_width = notifications::fleet_signal_bar_width(app, header.width);
    if bar_width > 0 {
        notifications::render_fleet_signal_bar(
            app,
            frame,
            Rect::new(header.x, header.y, bar_width, header.height),
        );
    }

    let mut taken = bar_width.saturating_add(if bar_width > 0 { HEADER_ROW_GAP } else { 0 });

    // The way out of a re-rooted tree sits after the bar, never before it. The
    // bar is a permanent readout whose positions a reader learns, and a control
    // that comes and goes with the current view must not shift it.
    let breadcrumb = sidebar_tree_breadcrumb_rect(app, area);
    if breadcrumb.width > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                TREE_BREADCRUMB_LABEL,
                Style::default().fg(app.palette.accent),
            )),
            breadcrumb,
        );
        taken = taken
            .saturating_add(breadcrumb.width)
            .saturating_add(HEADER_ROW_GAP);
    }

    let remaining = header.width.saturating_sub(taken);
    render_session_status(
        app,
        frame,
        Rect::new(
            header.x.saturating_add(taken),
            header.y,
            remaining,
            header.height,
        ),
    );
}

/// What the way back out of a re-rooted tree is labelled.
///
/// The captain's own word for the view a mate's subtree was entered from. It
/// names the destination rather than the current position because the row
/// immediately below already says where you are.
const TREE_BREADCRUMB_LABEL: &str = "◂ main";

/// The label for the control that leaves the current view, when there is one.
///
/// `None` in the fleet view: there is nothing above it, so the header row stays
/// exactly as empty as it was.
fn sidebar_tree_breadcrumb(app: &AppState) -> Option<&'static str> {
    (!app.tree_root.is_fleet()).then_some(TREE_BREADCRUMB_LABEL)
}

/// Where that control is drawn and hit-tested.
///
/// On the reserved header row, which is the one row above the tree and
/// therefore the only place a "go up" control can sit without taking a column
/// away from a row. It follows the fleet signal bar rather than preceding it,
/// so a control that comes and goes with the current view never shifts a
/// permanent readout. `area` is the panel's own list rect.
pub(crate) fn sidebar_tree_breadcrumb_rect(app: &AppState, area: Rect) -> Rect {
    let Some(label) = sidebar_tree_breadcrumb(app) else {
        return Rect::default();
    };
    let header = workspace_list_header_rect(area);
    if header.height == 0 {
        return Rect::default();
    }
    // Measured against the same allocation `render_header_row` walks, so the
    // control is hit-tested exactly where it was drawn however many columns the
    // signal bar took first.
    let bar = notifications::fleet_signal_bar_width(app, header.width);
    let offset = bar.saturating_add(if bar > 0 { HEADER_ROW_GAP } else { 0 });
    let width = display_width(label) as u16;
    if header.width.saturating_sub(offset) < width {
        return Rect::default();
    }
    Rect::new(header.x.saturating_add(offset), header.y, width, 1)
}

/// Draw the session status in the columns of the header row left over for it.
///
/// Right-aligned on purpose. Every row below it is left-aligned and carries
/// the indent guides and connectors that spell out ownership, so a left-hung
/// status would read as another root of the tree; against the right edge it
/// reads as panel chrome instead, and it lands in the same column the
/// scrollbar track occupies further down rather than inventing a new one.
///
/// Nothing is drawn when no status is set, which leaves the row exactly as
/// empty as it was before the slot existed.
fn render_session_status(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(status) = app.session_status.as_deref() else {
        return;
    };
    if area.width < MIN_SESSION_STATUS_WIDTH || area.height == 0 {
        return;
    }
    let text = truncate_end(status, usize::from(area.width));
    frame.render_widget(
        Paragraph::new(Span::styled(
            text,
            Style::default().fg(app.palette.overlay0),
        ))
        .alignment(Alignment::Right),
        area,
    );
}

/// Drawn height of one tree row, whichever kind it is.
///
/// Space rows and agent rows keep their own configured token rows, so the two
/// keep the heights their `[ui.sidebar.spaces]` / `[ui.sidebar.agents]` blocks
/// ask for even though they are now one list - minus whatever those rows can
/// fold onto one line at this width.
fn list_entry_height(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
    body_height: u16,
    fold_width: u16,
) -> u16 {
    let content_width = list_entry_content_width(app, agents, entry, fold_width);
    match entry {
        WorkspaceListEntry::Workspace {
            ws_idx, indented, ..
        } => app
            .workspaces
            .get(*ws_idx)
            .map(|ws| workspace_row_height_in_body(app, ws, *indented, body_height, content_width))
            .unwrap_or(0),
        WorkspaceListEntry::Agent { entry_idx, .. } => agents
            .get(*entry_idx)
            .map(|entry| agent_entry_height_in_body(app, entry, body_height, content_width))
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

    let fold_width = row_fold_width(area);
    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = list_entry_height(app, &agents, entry, body.height, fold_width);
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
    let fold_width = row_fold_width(area);
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (entry_idx, entry) in entries.iter().enumerate().rev() {
        let row_height = list_entry_height(app, &agents, entry, body.height, fold_width);
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
    content_width: usize,
) -> u16 {
    (fold_token_lines(resolved_agent_rows(app, entry), content_width, None)
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
    let fold_width = row_fold_width(ws_area);
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = list_entry_height(app, &agents, entry, body.height, fold_width);
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

/// The glyph marking "these workers reported back".
const WORKER_SUMMARY_BADGE_GLYPH: &str = "▤";

/// What the badge prints for `count` finished workers.
///
/// Two digits is the widest it ever gets, so the badge cannot eat an
/// unbounded slice of a 26-wide sidebar however large a mate's crew grows.
pub(crate) fn worker_summary_badge_label(count: usize) -> String {
    if count > 9 {
        format!("{WORKER_SUMMARY_BADGE_GLYPH}9+")
    } else {
        format!("{WORKER_SUMMARY_BADGE_GLYPH}{count}")
    }
}

/// The clickable summary badge on a second mate's row.
///
/// It sits one cell left of [`workspace_group_chevron_rect`], which stays
/// reserved whether or not this Space draws a chevron, so the two controls can
/// never land on the same cell as a Space gains or loses worktree children.
/// That also keeps the badge at offset 2 or more from the divider column, well
/// outside the one-cell grab band in
/// [`crate::app::AppState::sidebar_divider_grab_at`], so unlike the chevron it
/// needs no carve-out there.
///
/// The rect is one cell wider than the glyphs: a two-character badge is a mean
/// mouse target, and the pad column is dead space on the row anyway.
pub(crate) fn worker_summary_badge_rect(
    card: &crate::app::state::WorkspaceCardArea,
    count: usize,
) -> Rect {
    if card.rect.height == 0 {
        return Rect::default();
    }
    let width = worker_summary_badge_width(count, card.rect.width);
    if width == 0 {
        return Rect::default();
    }
    Rect::new(
        card.rect.x + card.rect.width.saturating_sub(1 + width),
        card.rect.y,
        width,
        1,
    )
}

/// Columns a badge for `count` workers takes on a row `width` wide, or 0 when
/// the row is too narrow to carry one.
///
/// The badge is drawn *over* row 0 rather than laid out in it, so the fold has
/// to reserve it exactly as the renderer does - a merged line the layout sized
/// without it would be elided by a control the layout never saw. Both read this
/// one function so they cannot drift.
fn worker_summary_badge_width(count: usize, width: u16) -> u16 {
    let label_width = display_width_u16(&worker_summary_badge_label(count));
    let badge = label_width.saturating_add(1);
    // Needs the badge, the reserved chevron cell, and something left over for
    // the row's own name; below that the row is better off with just a name.
    if width <= badge + 1 || label_width == 0 {
        return 0;
    }
    badge
}

/// The tree handle this card's row answers to, the name a worker's `owner`
/// token would have to spell to nest under it.
///
/// Spaces and agent panes name themselves differently — a Space by its label, a
/// pane by `agent rename` — so this is the one place that difference is
/// resolved, and both kinds of row become eligible for a badge by the same
/// rule.
fn card_tree_name(
    app: &AppState,
    entries: &[WorkspaceListEntry],
    agents: &[AgentPanelEntry],
    card: &crate::app::state::WorkspaceCardArea,
) -> Option<String> {
    entry_tree_name(app, agents, entries.get(card.entry_idx)?)
}

/// The same handle, resolved from the entry alone.
///
/// The layout has entries but no cards yet, and it has to know whether a row
/// earns a badge before it can decide how wide that row's content is.
fn entry_tree_name(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
) -> Option<String> {
    match entry {
        WorkspaceListEntry::Workspace { ws_idx, .. } => space_tree_name(app, *ws_idx),
        WorkspaceListEntry::Agent { entry_idx, .. } => agents.get(*entry_idx)?.agent_name.clone(),
    }
}

/// The badge this card should draw, if any: the mate's handle and how many of
/// its workers have reported back.
///
/// A row earns a badge purely by owning workers that published a summary, so
/// the badge appears exactly where the tree already groups those workers. It is
/// deliberately not gated on `relation`: the scoping handle is the ownership
/// edge itself, and gating on a derived depth would make the badge vanish the
/// moment a fleet nests one level deeper than the display cap.
pub(crate) fn worker_summary_badge(
    app: &AppState,
    entries: &[WorkspaceListEntry],
    agents: &[AgentPanelEntry],
    card: &crate::app::state::WorkspaceCardArea,
) -> Option<(String, usize)> {
    let name = card_tree_name(app, entries, agents, card)?;
    let count = crate::app::worker_summary::summary_count_for_owner(agents, &name);
    (count > 0).then_some((name, count))
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

    let Some(last) = cards.iter().rfind(|card| card.agent.is_none()) else {
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

/// Rows of grip marking on the divider, and the shortest divider that gets one.
///
/// The grip is the divider's only at-rest affordance, so it has to read as a
/// deliberate marking rather than as a rendering artefact: three rows is the
/// smallest run that does, and below eight rows those three would be most of
/// the bar and stop reading as a grip at all.
const DIVIDER_GRIP_ROWS: u16 = 3;
const DIVIDER_GRIP_MIN_HEIGHT: u16 = 8;

/// The rows the divider's grip marking covers, centred on the bar.
///
/// Empty when the sidebar is too short to spare the rows.
fn sidebar_divider_grip_rows(area: Rect) -> std::ops::Range<u16> {
    if area.height < DIVIDER_GRIP_MIN_HEIGHT {
        return 0..0;
    }
    let start = area.y + (area.height - DIVIDER_GRIP_ROWS) / 2;
    start..start + DIVIDER_GRIP_ROWS
}

/// The sidebar's vertical bar, drawn so it says it can be dragged.
///
/// Three states, quietest first. At rest the bar is the separator colour it has
/// always been and only the grip rows lift to `overlay0` — a colour step, no
/// glyph change, so nothing that reads the sidebar as text sees anything new.
/// Hovering the grab band lifts the whole bar and turns the grip heavy, which
/// is the moment the divider has to look grabbable. A live drag holds that same
/// hovered look wherever the pointer has been dragged to.
///
/// The hover state mirrors [`crate::app::AppState::sidebar_divider_grab_at`]
/// exactly (see `track_sidebar_divider_hover`), so the bar can never light up
/// on a cell where a press would be swallowed by a scrollbar track, a worktree
/// chevron, or the collapse toggle.
fn render_sidebar_divider(app: &AppState, frame: &mut Frame, area: Rect, is_navigating: bool) {
    let p = &app.palette;
    let active = app.sidebar_divider_hover;
    let bar_style = if active {
        Style::default().fg(p.overlay1)
    } else if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let grip_style = if active {
        Style::default().fg(p.accent)
    } else if is_navigating {
        Style::default().fg(p.text)
    } else {
        Style::default().fg(p.overlay0)
    };
    let grip_symbol = if active { "┃" } else { "│" };

    let grip = sidebar_divider_grip_rows(area);
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        let is_grip = grip.contains(&y);
        buf[(sep_x, y)].set_symbol(if is_grip { grip_symbol } else { "│" });
        buf[(sep_x, y)].set_style(if is_grip { grip_style } else { bar_style });
    }
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    render_sidebar_divider(app, frame, area, is_navigating);

    render_workspace_list(
        app,
        terminal_runtimes,
        frame,
        sidebar_content_rect(area),
        is_navigating,
    );
    render_sidebar_toggle(app, frame, area, false, p);
}

/// Columns a token spends that no budget is allowed to reclaim.
///
/// The state icon, the elapsed time and the git counters share this lane: at
/// two to four columns there is nothing to give back, and a truncated age is a
/// wrong age - `4` out of `47m` reads as four of something.
fn fixed_token_width(token: &ResolvedToken, state_icon_width: usize) -> usize {
    match &token.kind {
        ResolvedTokenKind::StateIcon => state_icon_width,
        ResolvedTokenKind::StateAge(text) => display_width(text),
        ResolvedTokenKind::GitStatus { ahead, behind } => {
            usize::from(*ahead > 0) * display_width(&format!("↑{ahead}"))
                + usize::from(*behind > 0) * display_width(&format!("↓{behind}"))
                + usize::from(*ahead > 0 && *behind > 0)
        }
        // Fixed for the same reason as the ahead/behind counters: a truncated
        // count is a wrong count, and there is nothing to reclaim from two or
        // three columns anyway.
        ResolvedTokenKind::GitDirty(dirty) => {
            let parts = tokens::git_dirty_parts(*dirty);
            let separators = parts.len().saturating_sub(1);
            parts
                .iter()
                .map(|(_, text)| display_width(text))
                .sum::<usize>()
                + separators
        }
        ResolvedTokenKind::PullRequests { open, .. } => {
            display_width(&tokens::pull_requests_text(*open))
        }
        _ => 0,
    }
}

/// Columns a token would spend on text it is willing to give back.
fn flexible_token_width(token: &ResolvedToken) -> usize {
    match &token.kind {
        ResolvedTokenKind::StateText(text)
        | ResolvedTokenKind::Workspace(text)
        | ResolvedTokenKind::Tab(text)
        | ResolvedTokenKind::Pane(text)
        | ResolvedTokenKind::Agent(text)
        | ResolvedTokenKind::TerminalTitle(text)
        | ResolvedTokenKind::Branch(text)
        | ResolvedTokenKind::Custom(text) => display_width(text),
        _ => 0,
    }
}

/// Columns one line needs to draw every token whole, with the decorated
/// separators it uses while it still fits.
///
/// This is the measure the fold decision is made with, and it is deliberately
/// the *un*compacted one: a line the layout judged to fit is a line the
/// renderer draws having given nothing up, so folding can never be the reason a
/// name is elided.
fn natural_line_width(line: &[ResolvedToken]) -> usize {
    line.iter()
        .map(|token| fixed_token_width(token, STATE_MARK_WIDTH) + flexible_token_width(token))
        .sum::<usize>()
        + line
            .windows(2)
            .map(|pair| display_width(tokens::separator(&pair[0], &pair[1])))
            .sum::<usize>()
}

/// Width of `previous` and `next` drawn as one line, separator included.
fn merged_line_width(previous: &[ResolvedToken], next: &[ResolvedToken]) -> Option<usize> {
    let joint = display_width(tokens::separator(previous.last()?, next.first()?));
    Some(natural_line_width(previous) + joint + natural_line_width(next))
}

/// Lay a row's configured token lines out against the width the row actually
/// has.
///
/// A sidebar layout is written as a stack of lines, but a stack is a decision
/// made for the narrowest sidebar it will ever be drawn in. Widen the panel and
/// that decision stops paying: a Space spends two rows saying what fits on one,
/// and the tree runs off the bottom of the panel long before it runs out of
/// width. So consecutive lines are merged while the merged line still draws
/// every token whole. The trade is one-way by construction - folding buys back
/// a row and can never cost a character - which is what lets the same layout
/// serve an 18-column panel and a 54-column one without the user retuning it.
///
/// `max_lines` is the height the layout reserved for this row. Anything past it
/// is merged whether or not it fits, because a row that overflowed its
/// reservation used to lose its tail outright; an elided tail says more than a
/// missing one.
fn fold_token_lines(
    lines: Vec<Vec<ResolvedToken>>,
    content_width: usize,
    max_lines: Option<u16>,
) -> Vec<Vec<ResolvedToken>> {
    let mut folded: Vec<Vec<ResolvedToken>> = Vec::with_capacity(lines.len());
    for line in lines {
        let fits = folded
            .last()
            .and_then(|previous| merged_line_width(previous, &line))
            .is_some_and(|width| width <= content_width);
        match folded.last_mut().filter(|_| fits) {
            Some(previous) => previous.extend(line),
            None => folded.push(line),
        }
    }

    if let Some(max) = max_lines.map(usize::from).filter(|max| *max > 0) {
        while folded.len() > max {
            let Some(tail) = folded.pop() else { break };
            let Some(previous) = folded.last_mut() else {
                folded.push(tail);
                break;
            };
            previous.extend(tail);
        }
    }

    folded
}

fn resolved_token_spans(
    resolved: &[ResolvedToken],
    state_icon: (&str, Style),
    state_text_style: Style,
    workspace_style: Style,
    secondary_style: Style,
    custom_style: Style,
    p: &Palette,
    anim: &RowAnimation<'_>,
    max_width: usize,
) -> Vec<Span<'static>> {
    let fixed_widths = resolved
        .iter()
        .map(|token| fixed_token_width(token, display_width(state_icon.0)))
        .collect::<Vec<_>>();
    let flexible_widths = resolved
        .iter()
        .map(flexible_token_width)
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
                push_token_span(
                    &mut spans,
                    state_icon.0.to_string(),
                    state_icon.1,
                    token.style,
                    anim,
                );
            }
            ResolvedTokenKind::StateText(text) => {
                push_token_span(
                    &mut spans,
                    truncate_end(text, budgets[index]),
                    state_text_style,
                    token.style,
                    anim,
                );
            }
            // Drawn in the state's own colour, dimmed. The age qualifies the
            // state rather than competing with it, and dimming is how the row
            // says so without a second hue. It is not an alarm: nothing about
            // the styling changes as the number grows, because the runtime has
            // no evidence that a long state is a bad one.
            ResolvedTokenKind::StateAge(text) => {
                push_token_span(
                    &mut spans,
                    text.clone(),
                    state_text_style.add_modifier(Modifier::DIM),
                    token.style,
                    anim,
                );
            }
            ResolvedTokenKind::Workspace(text) => {
                push_token_span(
                    &mut spans,
                    middle_elide(text, budgets[index]),
                    workspace_style,
                    token.style,
                    anim,
                );
            }
            ResolvedTokenKind::Tab(text)
            | ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::Branch(text) => {
                push_token_span(
                    &mut spans,
                    middle_elide(text, budgets[index]),
                    secondary_style,
                    token.style,
                    anim,
                );
            }
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                if *ahead > 0 {
                    push_token_span(
                        &mut spans,
                        format!("↑{ahead}"),
                        Style::default().fg(p.green),
                        token.style,
                        anim,
                    );
                }
                if *ahead > 0 && *behind > 0 {
                    push_token_span(
                        &mut spans,
                        " ".to_string(),
                        Style::default(),
                        token.style,
                        anim,
                    );
                }
                if *behind > 0 {
                    push_token_span(
                        &mut spans,
                        format!("↓{behind}"),
                        Style::default().fg(p.red),
                        token.style,
                        anim,
                    );
                }
            }
            ResolvedTokenKind::GitDirty(dirty) => {
                for (index, (lane, text)) in tokens::git_dirty_parts(*dirty).into_iter().enumerate()
                {
                    if index > 0 {
                        push_token_span(
                            &mut spans,
                            " ".to_string(),
                            Style::default(),
                            token.style,
                            anim,
                        );
                    }
                    let color = match lane {
                        tokens::DirtyLane::Staged => p.green,
                        tokens::DirtyLane::Unstaged => p.yellow,
                        // Untracked files are the weakest claim on attention of
                        // the three, so they get the muted colour rather than a
                        // third bright one competing with the real edits.
                        tokens::DirtyLane::Untracked => p.overlay1,
                    };
                    push_token_span(
                        &mut spans,
                        text,
                        Style::default().fg(color),
                        token.style,
                        anim,
                    );
                }
            }
            ResolvedTokenKind::PullRequests {
                open,
                review_requested,
            } => {
                // A PR that named the viewer as a reviewer is the one figure here
                // that implies an action, so it is the only one emphasised.
                let color = if *review_requested > 0 {
                    p.peach
                } else {
                    p.blue
                };
                push_token_span(
                    &mut spans,
                    tokens::pull_requests_text(*open),
                    Style::default().fg(color),
                    token.style,
                    anim,
                );
            }
            ResolvedTokenKind::TerminalTitle(text) | ResolvedTokenKind::Custom(text) => {
                push_token_span(
                    &mut spans,
                    truncate_end(text, budgets[index]),
                    custom_style,
                    token.style,
                    anim,
                );
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
    let levels = crate::app::agent_tree::display_depth(levels);
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
    fold_width: u16,
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
    let rows = fold_token_lines(
        resolved_agent_rows(app, detail),
        list_entry_content_width(app, agents, entry, fold_width),
        Some(card.rect.height),
    );
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
    let summary_badge = worker_summary_badge(app, entries, agents, card);
    // This row's own life, not its Space's: a worker arrives when it starts and
    // leaves when it finishes, which is what makes its second mate's group grow
    // and shrink around it.
    let row_anim = RowAnimation::for_agent_row(app, detail.pane_id);

    for (row_index, resolved) in rows.iter().enumerate() {
        let row_y = card.rect.y + row_index as u16;
        if row_index as u16 >= card.rect.height || row_y >= list_bottom {
            break;
        }
        // Only row 0 carries the badge, so only row 0 gives up the width.
        let trailing_width = if row_index == 0 {
            summary_badge
                .as_ref()
                .map(|(_, count)| usize::from(worker_summary_badge_rect(card, *count).width))
                .unwrap_or(0)
        } else {
            0
        };
        let (mut spans, prefix_width) = agent_row_prefix(
            entry.depth(),
            entry.is_last_child(),
            entry.ancestors_continue(),
            row_index,
            p,
            // The Agents panel lists panes, and a relation signal is keyed on a
            // workspace, so there is nothing here for a charge to belong to.
            None,
        );
        animate_row_spans(&mut spans, &row_anim);
        spans.extend(resolved_token_spans(
            resolved,
            state_icon,
            status_style,
            name_style,
            agent_style,
            agent_style,
            p,
            &row_anim,
            (card.rect.width as usize).saturating_sub(prefix_width + trailing_width),
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );
    }

    if let Some((owner, count)) = &summary_badge {
        render_worker_summary_badge(app, frame, card, agents, owner, *count, list_bottom);
    }
}

/// Draw the summary badge at the right edge of a mate's first row.
///
/// Accent while at least one of those workers has finished without being
/// looked at — the same "done" distinction the row's own state dot already
/// makes — and muted once they have all been seen. The colour is the whole
/// signal: it is a static style, never a pulse, so the badge reads the same in
/// a still capture as it does on screen and does not wait on an animation
/// primitive Herdr has not settled yet.
fn render_worker_summary_badge(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    agents: &[AgentPanelEntry],
    owner: &str,
    count: usize,
    list_bottom: u16,
) {
    let rect = worker_summary_badge_rect(card, count);
    if rect.width == 0 || rect.y >= list_bottom {
        return;
    }
    let fresh = crate::app::worker_summary::summaries_for_owner(agents, owner)
        .iter()
        .any(|summary| summary.is_unseen_finish());
    let style = if fresh {
        Style::default()
            .fg(app.palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.overlay0)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(worker_summary_badge_label(count), style))
            .alignment(Alignment::Right),
        rect,
    );
}

fn agent_row_prefix(
    depth: u8,
    is_last_child: bool,
    ancestors_continue: &[bool],
    row_index: usize,
    p: &Palette,
    charge: Option<&ConnectorCharge<'_>>,
) -> (Vec<Span<'static>>, usize) {
    let depth = crate::app::agent_tree::display_depth(depth);
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
        push_connector_spans(&mut spans, is_last_child, charge, line_style);
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

/// The `├─ ` / `└─ ` connector of a child row's first line, charge and all.
///
/// One function for both panels' connectors so a charge cannot run one shape in
/// the Spaces tree and another in the owned-Space tree; the sidebar already
/// insists those be the same three glyphs, and this is what keeps them the same
/// three glyphs while something is travelling them.
fn push_connector_spans(
    spans: &mut Vec<Span<'static>>,
    is_last_child: bool,
    charge: Option<&ConnectorCharge<'_>>,
    base: Style,
) {
    let connector = if is_last_child { "└─ " } else { "├─ " };
    for (cell, settled) in connector.chars().enumerate() {
        let (glyph, style) = connector_cell(charge, cell as u16, settled, base);
        spans.push(Span::styled(glyph.to_string(), style));
    }
}

/// The colour vocabulary: what each kind of relation signal reads as.
///
/// Palette roles rather than literal hues, so the vocabulary follows whatever
/// theme is in force — including onto a light background — instead of being
/// written down twice and drifting. The four are chosen to be separable by hue
/// alone, because motion is a poor channel for category: telling a completion
/// from a failure should not require watching which way an 800 ms animation
/// went.
///
/// `Transfer` deliberately keeps the accent the connector charge has always
/// used, so the one signal that already existed does not change meaning when
/// the vocabulary arrives around it.
fn relation_signal_color(kind: RelationSignalKind, p: &Palette) -> ratatui::style::Color {
    match kind {
        RelationSignalKind::Transfer => p.accent,
        RelationSignalKind::Completed => p.green,
        RelationSignalKind::Failed => p.red,
        // The quiet one, and the only one that must not compete for attention:
        // "this branch stopped" is the least urgent thing a fleet can say.
        RelationSignalKind::Idle => p.overlay1,
    }
}

/// Which named behaviour a kind travels with.
///
/// Only two, because motion here separates *urgency*, not category — colour
/// does category. Something happening crackles; something stopping drifts.
fn relation_signal_behaviour(kind: RelationSignalKind) -> &'static str {
    use crate::anim::behaviour::names;
    match kind {
        RelationSignalKind::Idle => names::RELATION_DRIFT,
        RelationSignalKind::Transfer
        | RelationSignalKind::Completed
        | RelationSignalKind::Failed => names::RELATION_CHARGE,
    }
}

/// One row's relation charge, resolved once and then asked about each cell.
///
/// Built only when a signal is actually travelling this row, so the ordinary
/// case is a `None` costing exactly what the sidebar cost before charges
/// existed. The route is the three connector cells plus the state icon, in draw
/// order, which is why nothing here has to know which way the charge is going.
struct ConnectorCharge<'a> {
    behaviour: &'a crate::anim::behaviour::Behaviour,
    progress: f32,
    ink: crate::anim::cell::InkPalette,
    /// Kept so the state icon can resolve *its own* colour the same way every
    /// other colour in Herdr is resolved — against the palette the host
    /// terminal actually reported, never a static table.
    host: &'a crate::terminal_theme::TerminalTheme,
}

impl<'a> ConnectorCharge<'a> {
    fn new(app: &'a AppState, base: Style, phase: Option<RelationSignalPhase>) -> Option<Self> {
        let phase = phase?;
        let behaviour = app
            .anim
            .catalogue()
            .get(relation_signal_behaviour(phase.kind))?;
        let signal = crate::ui::color::resolve_color_rgb(
            relation_signal_color(phase.kind, &app.palette),
            &app.host_terminal_theme,
        )?;
        Some(Self {
            behaviour,
            progress: phase.progress,
            ink: crate::anim::cell::InkPalette::resolve(
                base,
                &app.palette,
                &app.host_terminal_theme,
            )
            .with_signal(signal),
            host: &app.host_terminal_theme,
        })
    }

    fn route() -> crate::anim::cell::CellExtent {
        crate::anim::cell::CellExtent::row(u16::from(crate::app::relation_signal::SIGNAL_STOPS))
    }

    fn paint(&self, cell: u16) -> crate::anim::cell::CellPaint {
        self.behaviour.cell(
            crate::anim::cell::CellPos::col(cell),
            Self::route(),
            self.progress,
            crate::anim::behaviour::DriveInputs::default(),
            self.ink,
        )
    }

    /// How strongly the charge reaches one cell, for a call site that draws its
    /// own thing rather than taking the paint whole.
    fn strength(&self, cell: u16) -> f32 {
        self.behaviour.strength(
            crate::anim::cell::CellPos::col(cell),
            Self::route(),
            self.progress,
        )
    }
}

/// One cell of a child row's branch-line connector, charge and all.
///
/// The connector is pure decoration — three glyphs that spell out ownership the
/// indent already showed — so this is the one place in the sidebar that honours
/// a behaviour's glyph offer. `glyph_over` still refuses anything that is not
/// the same width, so the cell count and every column stay exactly what they
/// would be with no signal at all.
fn connector_cell(
    charge: Option<&ConnectorCharge<'_>>,
    cell: u16,
    settled: char,
    base: Style,
) -> (char, Style) {
    let Some(charge) = charge else {
        return (settled, base);
    };
    let paint = charge.paint(cell);
    (
        paint.glyph_over(settled),
        paint.text_style(base, charge.ink),
    )
}

/// Emphasis on a row's state icon as a charge reaches it.
///
/// The icon's own colour becomes the block, rather than the signal's, because
/// that colour *is* the agent state — and its glyph is never substituted for
/// the same reason. This is where the amended rule earns its keep: a connector
/// cell may now change shape because it carries no information, and this cell
/// may not because it carries all of the row's.
///
/// The crossfade is continuous rather than a switch, so an arriving charge
/// flows onto the icon instead of the icon blinking on at the last stop.
fn arrived_state_icon_style(
    base: Style,
    charge: Option<&ConnectorCharge<'_>>,
    p: &Palette,
) -> Style {
    let Some(charge) = charge else {
        return base;
    };
    let amount = charge.strength(u16::from(crate::app::relation_signal::CONNECTOR_CELLS));
    if amount <= 0.0 {
        return base;
    }
    let Some(ink) = crate::ui::color::resolve_color_rgb(base.fg.unwrap_or(p.text), charge.host)
    else {
        return base;
    };
    let surface = charge.ink.surface;
    let style = base
        .fg(rgb_color(crate::ui::color::mix_rgb(ink, surface, amount)))
        .bg(rgb_color(crate::ui::color::mix_rgb(surface, ink, amount)));
    if amount >= 0.5 {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn rgb_color(rgb: crate::ui::color::Rgb) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2)
}

/// Draw the whole tree's own arrival or departure over the rows already drawn.
///
/// The view is one element of the animation engine and the rows underneath are
/// others, so this composes with them rather than replacing them: a worker
/// spawning while the panel is coming apart still plays its own arrival, and a
/// row that finished arriving mid-switch is not snapped back. Nothing here
/// moves a row — the transition is carried entirely by how present each cell
/// is, which is the whole reason a re-root never slides anything.
///
/// Every cell the tree occupies is reached, connectors and scrollbar included,
/// because a view that dissolved its text and left its rails behind would read
/// as the panel breaking rather than as the view leaving.
fn render_tree_view_transition(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    body: Rect,
    list_bottom: u16,
) {
    use crate::anim::cell::{CellExtent, CellPos, InkPalette};
    use crate::ui::color::{mix_rgb, resolve_color_rgb};

    let Some(view) = app
        .anim
        .frame(&crate::app::tree_view::view_element(), None)
        .filter(|view| view.behaviour.is_some())
    else {
        return;
    };
    if body.height == 0 || area.width == 0 {
        return;
    }
    let body_top = body.y;
    let height = list_bottom
        .min(body.y + body.height)
        .saturating_sub(body_top);
    if height == 0 {
        return;
    }

    let extent = CellExtent::new(area.width, height);
    let panel_bg = resolve_color_rgb(app.palette.panel_bg, &app.host_terminal_theme);
    let buf = frame.buffer_mut();
    for row in 0..height {
        let y = body_top + row;
        for col in 0..area.width {
            let x = area.x + col;
            let base = buf[(x, y)].style();
            let ink = InkPalette::resolve(base, &app.palette, &app.host_terminal_theme);
            let paint = view.cell(CellPos::new(col, row), extent, ink);
            let mut style = paint.text_style(base, ink);
            // A highlighted row's own background is part of the view too. It is
            // taken out on the engine's coverage rather than on a second ramp
            // of this call site's own, so the block and the text it holds leave
            // together.
            if paint.coverage < 1.0 {
                if let (Some(from), Some(to)) = (
                    base.bg
                        .and_then(|bg| resolve_color_rgb(bg, &app.host_terminal_theme)),
                    panel_bg,
                ) {
                    let (r, g, b) = mix_rgb(from, to, 1.0 - paint.coverage.clamp(0.0, 1.0));
                    style = style.bg(ratatui::style::Color::Rgb(r, g, b));
                }
            }
            buf[(x, y)].set_style(style);
        }
    }
}

/// One row's place in its own lifecycle, carried through the token pass.
///
/// The row owns the life; each token names its own idle behaviour through its
/// configured `emphasis`. That split is what lets one row's tokens animate
/// differently while still sharing one arrival — and why the *row's* arrival
/// always wins over a token's emphasis, which is a rule the engine enforces
/// rather than this pass.
struct RowAnimation<'a> {
    anim: &'a crate::anim::Animator,
    id: Option<crate::anim::ElementId>,
    palette: &'a Palette,
    host: &'a crate::terminal_theme::TerminalTheme,
}

impl<'a> RowAnimation<'a> {
    fn for_workspace(app: &'a AppState, workspace_id: Option<&str>) -> Self {
        Self {
            anim: &app.anim,
            id: workspace_id.map(crate::anim::ElementId::workspace_row),
            palette: &app.palette,
            host: &app.host_terminal_theme,
        }
    }

    /// A worker or sub agent row's own life, not its Space's.
    ///
    /// The distinction is the whole point of this task: a worker's row arrives
    /// when that worker starts and leaves when it finishes, while the Space it
    /// runs in was there before and stays after. Borrowing the Space's element
    /// gave every row in a group one shared arrival that had already happened,
    /// so nothing under a second mate could ever be seen appearing.
    fn for_agent_row(app: &'a AppState, pane_id: crate::layout::PaneId) -> Self {
        Self {
            anim: &app.anim,
            id: Some(crate::anim::ElementId::agent_row(pane_id)),
            palette: &app.palette,
            host: &app.host_terminal_theme,
        }
    }

    /// The frame this token should draw with, or `None` when nothing is
    /// animating it — which is the settled path every unconfigured Herdr takes.
    fn frame(
        &self,
        patch: crate::config::SidebarTokenStyle,
    ) -> Option<crate::anim::ElementFrame<'a>> {
        let idle = patch
            .emphasis
            .and_then(crate::config::SidebarTokenEmphasis::behaviour);
        let frame = self.anim.frame(self.id.as_ref()?, idle)?;
        frame.behaviour.is_some().then_some(frame)
    }

    /// The row's own arrival or departure, ignoring every steady behaviour.
    ///
    /// The tree connector belongs to the row rather than to any token on it, so
    /// it arrives and leaves with the row and never picks up a token's idle
    /// emphasis: a `├─` that pulsed because the branch name beside it was
    /// configured to would be drawing attention to scaffolding.
    fn lifecycle_frame(&self) -> Option<crate::anim::ElementFrame<'a>> {
        let frame = self.anim.frame(self.id.as_ref()?, None)?;
        (frame.behaviour.is_some()
            && matches!(
                frame.phase,
                crate::anim::Phase::Mount | crate::anim::Phase::Dismount
            ))
        .then_some(frame)
    }
}

/// Fold a row's arrival or departure into spans that are already styled.
///
/// For the parts of a row that are not tokens — the tree connectors — so a row
/// growing into a group or leaving it moves as one thing rather than as text
/// that fades beside scaffolding that blinks in and out.
fn animate_row_spans(spans: &mut [Span<'static>], anim: &RowAnimation<'_>) {
    use crate::anim::cell::{CellExtent, CellPos, InkPalette};

    let Some(frame) = anim.lifecycle_frame() else {
        return;
    };
    let total: u16 = spans
        .iter()
        .map(|span| crate::ui::text::display_width_u16(&span.content))
        .sum();
    if total == 0 {
        return;
    }
    let extent = CellExtent::row(total);
    let mut col = 0u16;
    for span in spans {
        let width = crate::ui::text::display_width_u16(&span.content);
        let ink = InkPalette::resolve(span.style, anim.palette, anim.host);
        span.style = frame
            .cell(CellPos::col(col), extent, ink)
            .text_style(span.style, ink);
        col = col.saturating_add(width);
    }
}

/// Draw one token's text with the row's animation reaching each of its cells.
///
/// Emits exactly one span when the animation is uniform across the token — the
/// common case, and the only shape this pass produced before the engine existed
/// — and one span per cell only when a behaviour genuinely differs cell to
/// cell. A token nothing is animating takes the first branch and is byte-for-
/// byte what it always was.
fn push_token_span(
    spans: &mut Vec<Span<'static>>,
    text: String,
    base: Style,
    patch: crate::config::SidebarTokenStyle,
    anim: &RowAnimation<'_>,
) {
    let style = apply_token_style(base, patch);
    push_animated_span(
        spans,
        text,
        style,
        anim.frame(patch),
        anim.palette,
        anim.host,
    );
}

/// Draw one span of text with an animation frame reaching each of its cells.
///
/// The one place a sidebar surface turns an [`crate::anim::ElementFrame`] into
/// spans. Emits exactly one span when the frame is uniform across the text — the
/// common case, and the only shape this pass produced before the engine existed
/// — and one span per cell only when a behaviour genuinely differs cell to cell.
/// Text nothing is animating takes the first branch and is byte-for-byte what it
/// always was.
pub(super) fn push_animated_span(
    spans: &mut Vec<Span<'static>>,
    text: String,
    style: Style,
    frame: Option<crate::anim::ElementFrame<'_>>,
    palette: &Palette,
    host: &crate::terminal_theme::TerminalTheme,
) {
    use crate::anim::cell::{CellExtent, CellPos, InkPalette};

    let width = crate::ui::text::display_width_u16(&text);
    let Some(frame) = frame.filter(|_| width > 0) else {
        spans.push(Span::styled(text, style));
        return;
    };

    let extent = CellExtent::row(width);
    let ink = InkPalette::resolve(style, palette, host);
    if frame.is_uniform() {
        let paint = frame.cell(CellPos::col(0), extent, ink);
        spans.push(Span::styled(text, paint.text_style(style, ink)));
        return;
    }

    let mut col = 0u16;
    for ch in text.chars() {
        let paint = frame.cell(CellPos::col(col), extent, ink);
        spans.push(Span::styled(ch.to_string(), paint.text_style(style, ink)));
        col = col.saturating_add(unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16);
    }
}

/// Apply a token's own static styling. Animation is layered on top of this by
/// [`push_token_span`], never folded into it: the settled style has to stay
/// recoverable, because that is what every frame returns to.
fn apply_token_style(mut style: Style, patch: crate::config::SidebarTokenStyle) -> Style {
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
    let fold_width = row_fold_width(area);

    render_header_row(app, frame, area);

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);

    for card in cards {
        if card.agent.is_some() {
            render_agent_row(app, frame, card, &entries, &agents, list_bottom, fold_width);
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
        let summary_badge = worker_summary_badge(app, &entries, &agents, card);
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
                dirty: ws.git_dirty(),
                pull_requests: ws.pull_requests(),
                terminal_title: terminal_title.raw.as_deref(),
                terminal_title_stripped: terminal_title.stripped.as_deref(),
                tokens: &token_values,
                suppress_git_details: card.indented,
            },
        );
        // The same call the layout made when it decided this row's height, so
        // the reserved height and the drawn lines cannot disagree about the
        // chevron or the badge.
        let content_width = entries
            .get(card.entry_idx)
            .map(|entry| list_entry_content_width(app, &agents, entry, fold_width))
            .unwrap_or_else(|| row_content_width(fold_width, own_depth, card.indented, 0));
        let rows = fold_token_lines(rows, content_width, Some(row_height));

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
            // Resolved once per row rather than per cell: the charge's colour
            // and its behaviour are the same for every cell of the route, and
            // only its position along that route differs.
            let connector_style = Style::default().fg(p.overlay0);
            let row_charge = ConnectorCharge::new(app, connector_style, row_signal_phase);
            let prefix_width = rail_width
                + if card.indented {
                    spans.push(Span::raw("   "));
                    if row_index == 0 {
                        push_connector_spans(
                            &mut spans,
                            is_last_child,
                            row_charge.as_ref(),
                            connector_style,
                        );
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
                    // not change shape at it — and, for the same reason, a
                    // charge has to run it too. A fleet that declares ownership
                    // with `owner` tokens is the shape the sidebar sketch
                    // actually describes, so this is the path most signals take.
                    let (mut owned, width) = agent_row_prefix(
                        own_depth,
                        own_is_last,
                        own_ancestors,
                        row_index,
                        p,
                        row_charge.as_ref(),
                    );
                    spans.append(&mut owned);
                    width
                } else if row_index == 0 {
                    spans.push(Span::raw(" "));
                    1
                } else {
                    spans.push(Span::raw("   "));
                    3
                };
            // Row 0 keeps the chevron cell clear, and the badge's own width on
            // top of it, so a mate's name is truncated instead of being drawn
            // under either control.
            let trailing_width = if row_index == 0 {
                usize::from(parent_group.is_some()) * 2
                    + summary_badge
                        .as_ref()
                        .map(|(_, count)| {
                            usize::from(worker_summary_badge_rect(card, *count).width)
                        })
                        .unwrap_or(0)
            } else {
                0
            };
            spans.extend(resolved_token_spans(
                resolved,
                (
                    state_icon.0,
                    arrived_state_icon_style(state_icon.1, row_charge.as_ref(), p),
                ),
                state_text_style,
                name_style,
                branch_style,
                branch_style,
                p,
                &RowAnimation::for_workspace(app, Some(ws.id.as_str())),
                (card.rect.width as usize).saturating_sub(prefix_width + trailing_width),
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(card.rect.x, row_y + row_index as u16, card.rect.width, 1),
            );
        }

        if let Some((owner, count)) = &summary_badge {
            render_worker_summary_badge(app, frame, card, &agents, owner, *count, list_bottom);
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

    // Last, over everything the tree drew: the view's own life is a property of
    // the whole panel, not of any one row in it.
    render_tree_view_transition(
        app,
        frame,
        area,
        workspace_list_body_rect(area, scrollbar_rect.is_some()),
        list_bottom,
    );

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

    /// The same miniature fleet, but the worker has finished and published a
    /// summary the way a real one does — `pane report-metadata --token
    /// summary=...`, which lands in the pane's metadata tokens.
    ///
    /// Returns the built state so a test can hit-test the badge as well as
    /// read the rows back.
    fn summary_fleet(
        width: u16,
        summary: Option<&str>,
    ) -> (crate::app::state::AppState, Vec<String>) {
        let mut app = crate::app::state::AppState::test_new();
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let worker_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![Workspace::test_new("firstmate"), second_mate];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
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
        let mut tokens = std::collections::HashMap::from([(
            "owner".to_string(),
            Some("2ndmate-explore".to_string()),
        )]);
        if let Some(summary) = summary {
            tokens.insert("summary".to_string(), Some(summary.to_string()));
        }
        terminal.metadata_tokens.patch(tokens, None, now);

        let area = Rect::new(0, 0, width, 20);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = (0..20).map(|row| row_text(buffer, row, width)).collect();
        (app, rows)
    }

    /// The row that owns the finished worker grows a badge, and it is the
    /// mate's row rather than the worker's own.
    #[test]
    fn a_second_mate_with_a_finished_worker_draws_a_summary_badge() {
        let (_, rows) = summary_fleet(26, Some("rebased and green"));
        let screen = rows.join("\n");
        let mate_row = rows
            .iter()
            .find(|row| row.contains("2ndmate-explore"))
            .unwrap_or_else(|| panic!("no mate row in:\n{screen}"));
        assert!(
            mate_row.contains(WORKER_SUMMARY_BADGE_GLYPH),
            "the mate's row carries no summary badge:\n{screen}"
        );
        assert_eq!(
            screen.matches(WORKER_SUMMARY_BADGE_GLYPH).count(),
            1,
            "the badge is drawn on more than the owning mate's row:\n{screen}"
        );
    }

    /// No summary published, no badge. The affordance only exists once there
    /// is something behind it to read.
    #[test]
    fn a_mate_whose_workers_published_nothing_draws_no_badge() {
        let (_, rows) = summary_fleet(26, None);
        let screen = rows.join("\n");
        assert!(
            !screen.contains(WORKER_SUMMARY_BADGE_GLYPH),
            "a badge was drawn with no summary to open:\n{screen}"
        );
    }

    /// The badge must not move the tree. Everything left of the badge cells -
    /// every connector, every indent, every name - has to render exactly as it
    /// did before a summary existed, so #27/#28's tree behaviour is untouched.
    #[test]
    fn publishing_a_summary_does_not_disturb_the_tree_it_hangs_off() {
        let (app, with) = summary_fleet(26, Some("rebased and green"));
        let (_, without) = summary_fleet(26, None);

        let mate_card = app
            .view
            .workspace_card_areas
            .iter()
            .find(|card| {
                matches!(
                    workspace_list_entries(&app).get(card.entry_idx),
                    Some(WorkspaceListEntry::Workspace { ws_idx, .. }) if *ws_idx == 1
                )
            })
            .expect("the mate has a card");
        let badge = worker_summary_badge_rect(mate_card, 1);
        assert!(badge.width > 0, "the badge has no rect to reserve");

        for (row_idx, (a, b)) in with.iter().zip(without.iter()).enumerate() {
            let a: String = a.chars().take(usize::from(badge.x)).collect();
            let b: String = b.chars().take(usize::from(badge.x)).collect();
            assert_eq!(
                a,
                b,
                "row {row_idx} changed left of the badge:\n{}\n{}",
                with.join("\n"),
                without.join("\n")
            );
        }
    }

    /// The badge never lands on the chevron cell or inside the divider's
    /// one-cell grab band, whether or not a scrollbar is pushing the cards
    /// left.
    #[test]
    fn the_badge_clears_the_chevron_cell_and_the_divider_grab_band() {
        let (app, _) = summary_fleet(26, Some("rebased and green"));
        for card in &app.view.workspace_card_areas {
            let badge = worker_summary_badge_rect(card, 1);
            if badge.width == 0 {
                continue;
            }
            let chevron = workspace_group_chevron_rect(card);
            assert!(
                badge.x + badge.width <= chevron.x,
                "badge {badge:?} overlaps chevron {chevron:?}"
            );
            let divider_col = app.view.sidebar_rect.x + app.view.sidebar_rect.width - 1;
            assert!(
                badge.x + badge.width - 1 < divider_col - 1,
                "badge {badge:?} reaches into the divider grab band at {divider_col}"
            );
        }
    }

    /// A crowded mate still gets a bounded badge rather than one that grows
    /// with the crew.
    #[test]
    fn the_badge_label_is_capped_at_two_characters_of_count() {
        assert_eq!(worker_summary_badge_label(3), "▤3");
        assert_eq!(worker_summary_badge_label(9), "▤9");
        assert_eq!(worker_summary_badge_label(10), "▤9+");
        assert_eq!(worker_summary_badge_label(400), "▤9+");
    }

    /// A sidebar too narrow to hold a name beside the badge draws no badge,
    /// rather than a badge with nothing left of it.
    #[test]
    fn a_too_narrow_row_drops_the_badge_instead_of_the_name() {
        let card = crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 0, 4, 1),
            indented: false,
            entry_idx: 0,
            agent: None,
        };
        assert_eq!(worker_summary_badge_rect(&card, 1), Rect::default());
    }

    /// Draws the sidebar and reads the bar column back out of the buffer as
    /// `(symbol, foreground)` per row, which is the only place the divider's
    /// affordance actually exists.
    fn drawn_divider_column(hovered: bool) -> Vec<(String, ratatui::style::Color)> {
        let area = Rect::new(0, 0, 26, 20);
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        app.sidebar_divider_hover = hovered;

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bar_x = area.x + area.width - 1;
        (area.y..area.y + area.height)
            .map(|y| {
                let cell = &buffer[(bar_x, y)];
                (cell.symbol().to_string(), cell.fg)
            })
            .collect()
    }

    /// At rest the divider still reads as a plain separator to anything that
    /// scrapes the sidebar as text: the grip is a colour step only. It is a
    /// real step, though, and it is a short centred run rather than the whole
    /// bar.
    #[test]
    fn the_resting_divider_marks_a_centred_grip_with_colour_alone() {
        let column = drawn_divider_column(false);
        let grip = sidebar_divider_grip_rows(Rect::new(0, 0, 26, 20));

        assert!(
            column.iter().all(|(symbol, _)| symbol == "│"),
            "the resting divider changed glyph somewhere: {column:?}"
        );

        let grip_colours: Vec<_> = column[grip.start as usize..grip.end as usize]
            .iter()
            .map(|(_, fg)| *fg)
            .collect();
        let bar_colour = column[0].1;
        assert!(
            grip_colours.iter().all(|fg| *fg != bar_colour),
            "the grip is the same colour as the bar, so nothing marks it: {column:?}"
        );
        assert_eq!(
            grip_colours.len(),
            usize::from(DIVIDER_GRIP_ROWS),
            "the grip should be a short run, not the whole bar"
        );
        assert!(
            grip.start > 0 && grip.end < 20,
            "the grip should be centred, not flush against an end: {grip:?}"
        );
    }

    /// Hovering is the moment the divider has to look grabbable, so both the
    /// bar and the grip change and the grip picks up a heavier glyph.
    #[test]
    fn hovering_lifts_the_whole_divider_and_thickens_the_grip() {
        let resting = drawn_divider_column(false);
        let hovered = drawn_divider_column(true);
        let grip = sidebar_divider_grip_rows(Rect::new(0, 0, 26, 20));

        assert_ne!(
            resting[0].1, hovered[0].1,
            "the bar did not change colour on hover"
        );
        for y in grip.clone() {
            assert_eq!(
                hovered[y as usize].0, "┃",
                "the grip did not thicken on hover at row {y}"
            );
        }
        for (y, (symbol, _)) in hovered.iter().enumerate() {
            if !grip.contains(&(y as u16)) {
                assert_eq!(symbol, "│", "row {y} thickened outside the grip");
            }
        }
    }

    /// Three grip rows out of a five-row sidebar is not a grip, it is a
    /// differently coloured divider, so short sidebars get none.
    #[test]
    fn a_short_sidebar_draws_no_grip_at_all() {
        assert!(
            sidebar_divider_grip_rows(Rect::new(0, 0, 26, DIVIDER_GRIP_MIN_HEIGHT - 1)).is_empty()
        );
        assert!(
            !sidebar_divider_grip_rows(Rect::new(0, 0, 26, DIVIDER_GRIP_MIN_HEIGHT)).is_empty()
        );
    }

    /// The branch every fixture Space reports, so the second configured Space
    /// line has content at a fixed width on any checkout.
    const FIXTURE_BRANCH: &str = "fm/herdr-dynamic-sidebar-width";

    /// A realistic captain fleet rendered with the *default* sidebar layout
    /// (two token rows per Space and per agent), so the dump shows what a user
    /// who never edited `[ui.sidebar]` actually sees.
    fn default_layout_fleet_rows(width: u16, height: u16) -> Vec<String> {
        default_layout_fleet(width, height, None).1
    }

    /// The same fleet, optionally with one worker having published a summary so
    /// its owning mate earns a badge over row 0.
    fn default_layout_fleet(
        width: u16,
        height: u16,
        summary: Option<&str>,
    ) -> (crate::app::state::AppState, Vec<String>) {
        let mut app = crate::app::state::AppState::test_new();
        let mut explore = Workspace::test_new("2ndmate-explore");
        let worker_a = explore.test_split(ratatui::layout::Direction::Vertical);
        let worker_b = explore.test_split(ratatui::layout::Direction::Vertical);
        let root_pane = *explore.tabs[0]
            .panes
            .keys()
            .find(|pane| **pane != worker_a && **pane != worker_b)
            .expect("original pane present");
        app.workspaces = vec![
            Workspace::test_new("firstmate"),
            explore,
            Workspace::test_new("2ndmate-homeautomation"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);

        // `Workspace::test_new` resolves `cached_git_branch` from the real
        // checkout, so an unpinned fixture inherits whatever branch the tree
        // happens to be on - and renders one line per Space instead of two on a
        // detached HEAD. That is not hypothetical: `actions/checkout` builds a
        // pull request from a detached `refs/pull/N/merge`, and a rebase detaches
        // too. Pin it so the fold is measured against a fixed layout.
        for workspace in app.workspaces.iter_mut() {
            workspace.cached_git_branch = Some(FIXTURE_BRANCH.to_string());
        }

        let now = std::time::Instant::now();
        for idx in [1usize, 2] {
            app.workspaces[idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                now,
            );
        }

        for (pane, name, state) in [
            (
                root_pane,
                "herdr-dynamic-sidebar-width",
                AgentState::Working,
            ),
            (worker_a, "herdr-divider-grab", AgentState::Blocked),
            (worker_b, "wall-panel-narrowing", AgentState::Idle),
        ] {
            let terminal_id = app.workspaces[1].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app
                .terminals
                .get_mut(&terminal_id)
                .expect("every test pane has a terminal");
            terminal.set_agent_name(name.to_string());
            terminal.state = state;
            let mut tokens = std::collections::HashMap::from([(
                "owner".to_string(),
                Some("2ndmate-explore".to_string()),
            )]);
            if let (Some(summary), true) = (summary, pane == worker_b) {
                tokens.insert("summary".to_string(), Some(summary.to_string()));
            }
            terminal.metadata_tokens.patch(tokens, None, now);
        }

        let area = Rect::new(0, 0, width, height);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = (0..height)
            .map(|row| row_text(buffer, row, width))
            .collect();
        (app, rows)
    }

    /// The rows of the tree, in order, with the sidebar divider and the blank
    /// tail below the last row dropped.
    fn tree_rows(rows: &[String]) -> Vec<String> {
        rows.iter()
            .skip(usize::from(WORKSPACE_SECTION_HEADER_ROWS))
            .map(|row| row.strip_suffix('│').unwrap_or(row).to_string())
            .take_while(|row| !row.trim().is_empty())
            .collect()
    }

    /// Every state mark is one column, which is what lets [`STATE_MARK_WIDTH`]
    /// stand in for the real icon while a row's height is being decided - one
    /// pass before the palette and the aggregate state are resolved.
    #[test]
    fn state_marks_are_one_column_wide() {
        for state in [
            AgentState::Blocked,
            AgentState::Working,
            AgentState::Idle,
            AgentState::Unknown,
        ] {
            for seen in [true, false] {
                assert_eq!(
                    display_width(crate::ui::status::state_mark(state, seen)),
                    STATE_MARK_WIDTH,
                    "{state:?} seen={seen} does not fit the assumed mark width"
                );
            }
        }
    }

    /// [`tree_prefix_width`] is what the layout subtracts before deciding how
    /// many lines a row needs; [`agent_row_prefix`] is what the renderer
    /// actually draws. A disagreement would let a row fold on a budget it does
    /// not have, so they are checked against each other directly.
    #[test]
    fn the_measured_prefix_matches_the_drawn_one() {
        let p = Palette::catppuccin();
        for depth in 0u8..5 {
            for row_index in [0usize, 1] {
                for is_last_child in [true, false] {
                    let ancestors = vec![true; depth as usize + 1];
                    let (_, drawn) =
                        agent_row_prefix(depth, is_last_child, &ancestors, row_index, &p, None);
                    assert_eq!(
                        drawn,
                        tree_prefix_width(depth, false, row_index),
                        "depth {depth} row {row_index} last={is_last_child}"
                    );
                }
            }
        }
    }

    /// The narrow end is the regression bar: at the widths the captain runs a
    /// sidebar at today, every row still spends the lines its layout asked for
    /// and the tree keeps its connectors.
    #[test]
    fn a_narrow_sidebar_still_stacks_every_configured_line() {
        for width in [18u16, 22, 26, 30, 36, 44] {
            let rows = default_layout_fleet_rows(width, 24);
            let screen = rows.join("\n");
            let tree = tree_rows(&rows);

            // Three Spaces and three workers, two configured lines each.
            assert_eq!(
                tree.len(),
                12,
                "a row folded at {width} columns, where its layout does not fit on one line:\n{screen}"
            );
            // Nesting and connectors are untouched by the fold machinery.
            assert!(
                tree[0].contains("firstmate") && !tree[0].contains(['├', '└']),
                "the root grew a connector at {width}:\n{screen}"
            );
            assert!(
                tree[2].contains("├─ ") && tree[10].contains("└─ "),
                "the second mates lost their connectors at {width}:\n{screen}"
            );
            assert!(
                tree[4].contains("│  ├─ ") && tree[8].contains("│  └─ "),
                "the workers lost their rail or connector at {width}:\n{screen}"
            );
        }
    }

    /// The whole point: widen the sidebar and a row gives a line back rather
    /// than sitting in a stack sized for a panel half as wide.
    #[test]
    fn a_wide_sidebar_folds_a_rows_configured_lines_onto_one() {
        let rows = default_layout_fleet_rows(70, 24);
        let screen = rows.join("\n");
        let tree = tree_rows(&rows);

        // Six entities, one line each, where twelve lines were spent before.
        assert_eq!(
            tree.len(),
            6,
            "the tree did not fold at 70 columns:\n{screen}"
        );
        assert!(
            tree[0].contains("firstmate") && tree[0].contains("fm/herdr-dynamic-sidebar-width"),
            "the first mate's branch did not join its name:\n{screen}"
        );
        assert!(
            tree[3].contains("├─ ") && tree[3].contains("herdr-divider-grab"),
            "a folded worker lost its connector:\n{screen}"
        );
        assert!(
            tree[4].contains("│  └─ ") && tree[4].contains("wall-panel-narrowing"),
            "the last worker lost its rail or its closing connector:\n{screen}"
        );
    }

    /// Folding is only ever allowed to buy a row back, never to spend a
    /// character: a line the layout judged foldable is a line that draws whole.
    #[test]
    fn folding_never_elides_what_it_folded() {
        for width in [46u16, 54, 62, 70, 90] {
            let rows = default_layout_fleet_rows(width, 24);
            let screen = rows.join("\n");
            for row in tree_rows(&rows) {
                // A line carrying both configured tokens is a folded one.
                if row.contains(" · ") && row.contains("herdr") {
                    assert!(
                        !row.contains('…'),
                        "a folded line was elided at {width}:\n{screen}"
                    );
                }
            }
        }
    }

    /// A row that earns a summary badge has to fold against the width the badge
    /// leaves it, not the width it would have had alone.
    ///
    /// The badge is painted over row 0 after the tokens are drawn, so a fold
    /// that reserved only the chevron would merge two lines that then get
    /// elided to fit under it - the one character the fold promises never to
    /// spend. #34 added the badge and this pins it against the fold.
    #[test]
    fn a_folded_row_never_runs_under_its_summary_badge() {
        // Swept densely on purpose. The window where an unreserved badge does
        // damage is only as wide as the badge itself - the merged line has to
        // fit the row but not the row minus the badge - so a handful of sampled
        // widths steps straight over it.
        let mut ever_folded = false;
        for width in 40u16..=90 {
            let (_, rows) = default_layout_fleet(width, 24, Some("rebased and green"));
            let screen = rows.join("\n");
            let badged = tree_rows(&rows)
                .into_iter()
                .find(|row| row.contains(WORKER_SUMMARY_BADGE_GLYPH))
                .unwrap_or_else(|| panic!("no badge drawn at {width}:\n{screen}"));

            // The badge keeps a pad cell of its own, folded or not. Text in it
            // means the row was laid out against a width the badge had already
            // taken.
            let badge_at = badged
                .find(WORKER_SUMMARY_BADGE_GLYPH)
                .expect("the row was found by this glyph");
            assert!(
                badged[..badge_at].ends_with(' '),
                "row text ran into the badge's pad at {width} columns:\n{screen}"
            );

            // The branch reaching row 0 is the tell that this row folded.
            if !badged.contains(FIXTURE_BRANCH) {
                continue;
            }
            ever_folded = true;
            // Folding may only ever buy a row back. A budget that had to
            // compact the decorated separator down to a bare space, or elide,
            // means the fold spent characters to fit under the badge.
            assert!(
                badged.contains(" · "),
                "the badged row folded and then had to compact its separator to \
                 fit under the badge at {width} columns:\n{screen}"
            );
            assert!(
                !badged.contains('…'),
                "the badged row folded and was then elided under its badge at \
                 {width} columns:\n{screen}"
            );
        }
        assert!(
            ever_folded,
            "no width folded the badged row, so nothing was actually exercised"
        );
    }

    /// Resizing has to be monotonic to feel like resizing: no width may cost
    /// the tree rows that a narrower one could show.
    #[test]
    fn widening_the_sidebar_never_costs_the_tree_a_row() {
        let mut previous = usize::MAX;
        for width in 18u16..=90 {
            let lines = tree_rows(&default_layout_fleet_rows(width, 24)).len();
            assert!(
                lines <= previous,
                "widening to {width} columns grew the tree from {previous} lines to {lines}"
            );
            previous = lines;
        }
    }

    /// A row that cannot have every line it asked for used to lose the tail
    /// outright. It is folded onto what it does have instead, so the token
    /// budget elides it rather than the layout dropping it.
    #[test]
    fn a_row_squeezed_below_its_line_count_keeps_its_tail() {
        // Four body rows for three Spaces and three workers: nothing has room
        // for a second line.
        let rows = default_layout_fleet_rows(54, 6);
        let screen = rows.join("\n");
        let tree = tree_rows(&rows);

        assert!(!tree.is_empty(), "nothing rendered:\n{screen}");
        assert!(
            tree[0].contains("firstmate") && tree[0].contains("fm/herdr-"),
            "the first mate's second line was dropped instead of folded:\n{screen}"
        );
    }

    /// A first mate owning three second mates, drawn with whatever session
    /// status is in force. `None` is the default state, in which nothing has
    /// published a status and the header row has nothing to draw.
    fn mate_fleet_sidebar_rows(width: u16, status: Option<&str>) -> Vec<String> {
        let mut app = crate::app::state::AppState::test_new();
        let now = std::time::Instant::now();
        app.workspaces = vec![
            Workspace::test_new("firstmate"),
            Workspace::test_new("2ndmate-explore"),
            Workspace::test_new("2ndmate-build"),
            Workspace::test_new("2ndmate-homeauto"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.session_status = status.map(str::to_string);
        for idx in 1..app.workspaces.len() {
            app.workspaces[idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                now,
            );
        }

        let area = Rect::new(0, 0, width, 20);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..area.height)
            .map(|row| row_text(buffer, area.y + row, area.width))
            .collect()
    }

    /// The header row is empty until something publishes a session status.
    /// Neither the `spaces` title nor the second-mate drop-down beside it is
    /// drawn any more - both were removed once every mate rendered live in the
    /// tree and could be reached by clicking it - but the row itself stays,
    /// because the tree's drop-slot grid needs a row above the first card.
    #[test]
    fn the_header_row_is_empty_above_the_tree_until_a_status_is_set() {
        let rows = mate_fleet_sidebar_rows(26, None);
        let screen = rows.join("\n");

        // Only the sidebar divider draws on the header row; nothing else does.
        assert!(
            rows[0].chars().all(|ch| ch.is_whitespace() || ch == '│'),
            "the header row is not empty:\n{screen}"
        );
        assert!(
            rows[1].contains("firstmate"),
            "the tree does not start below the header row:\n{screen}"
        );
        assert!(
            !screen.contains("spaces"),
            "the removed header title is still drawn:\n{screen}"
        );
        assert!(
            !screen.contains('▾'),
            "the removed second-mate selector is still drawn:\n{screen}"
        );
        // The mates still render live in the tree, which is why the drop-down
        // is not needed to reach them.
        for mate in ["2ndmate-explore", "2ndmate-build", "2ndmate-homeauto"] {
            assert!(screen.contains(mate), "{mate} missing from:\n{screen}");
        }
    }

    /// A published status lands on that same row, right-aligned, and the tree
    /// below it does not move.
    #[test]
    fn a_session_status_draws_on_the_header_row_above_the_first_mate() {
        let status = "7d 62% 5h 18%";
        let rows = mate_fleet_sidebar_rows(26, Some(status));
        let screen = rows.join("\n");

        assert!(
            rows[0].contains(status),
            "the status is not on the header row:\n{screen}"
        );
        assert!(
            !rows[0].contains('…'),
            "the status was elided at a width it fits:\n{screen}"
        );
        // Right-aligned: the last content column, one left of the divider bar.
        let divider = usize::from(26u16 - 1);
        assert_eq!(
            rows[0].char_indices().nth(divider).map(|(_, ch)| ch),
            Some('│'),
            "the status overran the divider column:\n{screen}"
        );
        assert!(
            rows[0].trim_end_matches('│').ends_with(status),
            "the status is not right-aligned:\n{screen}"
        );

        // Setting a status is not allowed to disturb the tree it sits above.
        let without = mate_fleet_sidebar_rows(26, None);
        assert_eq!(
            rows[1..],
            without[1..],
            "the tree moved when a status was set:\n{screen}"
        );
    }

    /// Clearing it puts the row back exactly as it was.
    #[test]
    fn clearing_the_session_status_empties_the_header_row_again() {
        let set = mate_fleet_sidebar_rows(26, Some("7d 62% 5h 18%"));
        let cleared = mate_fleet_sidebar_rows(26, None);

        assert_ne!(
            set[0], cleared[0],
            "the status never drew in the first place"
        );
        assert!(
            cleared[0].chars().all(|ch| ch.is_whitespace() || ch == '│'),
            "the header row did not go back to empty:\n{}",
            cleared.join("\n")
        );
    }

    /// At the narrowest sidebar the status gives ground instead of taking it:
    /// it elides inside its own row, stays clear of the divider bar, and the
    /// tree keeps every column it had.
    #[test]
    fn a_long_session_status_elides_on_a_narrow_sidebar() {
        let width = 18;
        let rows = mate_fleet_sidebar_rows(width, Some("7d 62% · 5h 18% · $41.20"));
        let screen = rows.join("\n");

        assert_eq!(
            display_width(&rows[0]),
            usize::from(width),
            "the header row is not exactly one sidebar wide:\n{screen}"
        );
        assert!(
            rows[0].contains('…'),
            "the status did not elide at 18 columns:\n{screen}"
        );
        assert!(
            rows[0].starts_with("7d"),
            "eliding dropped the leading window:\n{screen}"
        );
        let divider = usize::from(width - 1);
        assert_eq!(
            rows[0].char_indices().nth(divider).map(|(_, ch)| ch),
            Some('│'),
            "the elided status overran the divider column:\n{screen}"
        );

        let without = mate_fleet_sidebar_rows(width, None);
        assert_eq!(
            rows[1..],
            without[1..],
            "the tree reflowed to make room for the status:\n{screen}"
        );
    }

    /// Narrower than a status can say anything, the row goes back to empty
    /// rather than showing a stub the reader would have to guess at.
    #[test]
    fn a_session_status_is_dropped_when_the_sidebar_is_too_narrow_to_read_it() {
        let rows = mate_fleet_sidebar_rows(5, Some("7d 62% 5h 18%"));

        assert!(
            rows[0].chars().all(|ch| ch.is_whitespace() || ch == '│'),
            "a stub status drew at 5 columns:\n{}",
            rows.join("\n")
        );
    }

    /// Renders a sidebar with the fleet signal bar switched on, optionally with
    /// two of its signals driven live, and returns the drawn rows.
    ///
    /// `dirty` is driven through the workspace's own cached Git counts and
    /// `blocked` through a pane's detected agent state, which are the same two
    /// facts the tree below the bar already reads — so the test drives real
    /// state rather than reaching into the bar.
    fn signal_bar_rows(width: u16, dirty: bool, blocked: bool) -> Vec<String> {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_notifications.enabled = true;

        if dirty {
            app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
                staged: 0,
                unstaged: 1,
                untracked: 0,
            });
        }
        if blocked {
            let terminal_id = app
                .terminals
                .keys()
                .next()
                .expect("a terminal exists")
                .clone();
            app.terminals
                .get_mut(&terminal_id)
                .expect("terminal exists")
                .state = crate::detect::AgentState::Blocked;
        }

        // The app loop is what publishes the live set; a render-only test has
        // to stand in for it or nothing would ever be mounted.
        let now = std::time::Instant::now();
        let lifecycle = app.sidebar_notifications.lifecycle();
        let live: Vec<_> = crate::app::fleet_signals::FleetSignals::resolve(&app)
            .animation_membership()
            .collect();
        app.anim
            .observe(now, crate::anim::Family::Named, &lifecycle, live);
        // Past the arrival, so a live slot is drawn in its steady state rather
        // than mid-fade.
        app.anim
            .advance(now + std::time::Duration::from_millis(600));

        let area = Rect::new(0, 0, width, 12);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..area.height)
            .map(|row| row_text(buffer, area.y + row, area.width))
            .collect()
    }

    /// Foreground each signal's mark is drawn in on the header row.
    ///
    /// Keyed by the mark rather than by column so the assertion does not have
    /// to know which tier the width picked.
    fn signal_bar_mark_colors(
        width: u16,
        dirty: bool,
        blocked: bool,
    ) -> std::collections::HashMap<String, Option<ratatui::style::Color>> {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_notifications.enabled = true;
        // Colour, not motion, is what this reads; a still live slot keeps its
        // own colour and makes the assertion independent of the frame clock.
        app.sidebar_notifications.emphasis = crate::config::SidebarTokenEmphasis::None;
        app.sidebar_notifications.enter = crate::config::SidebarTokenEmphasis::None;

        if dirty {
            app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
                staged: 0,
                unstaged: 1,
                untracked: 0,
            });
        }
        if blocked {
            let terminal_id = app
                .terminals
                .keys()
                .next()
                .expect("a terminal exists")
                .clone();
            app.terminals
                .get_mut(&terminal_id)
                .expect("terminal exists")
                .state = crate::detect::AgentState::Blocked;
        }

        let area = Rect::new(0, 0, width, 12);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let mut colors = std::collections::HashMap::new();
        for column in 0..area.width {
            let cell = &buffer[(column, 0)];
            colors
                .entry(cell.symbol().to_string())
                .or_insert(cell.style().fg);
        }
        colors
    }

    /// Resting slots are the panel's muted grey; a live one is its own colour.
    /// The change from one to the other is what the bar is for.
    #[test]
    fn a_live_signal_leaves_the_resting_grey_for_its_own_colour() {
        let palette = crate::app::state::AppState::test_new().palette;
        let width = notifications::Tier::Marks.width() + 2;

        let resting = signal_bar_mark_colors(width, false, false);
        for signal in crate::app::fleet_signals::FleetSignal::ALL {
            assert_eq!(
                resting.get(signal.mark()).copied().flatten(),
                Some(palette.overlay0),
                "{signal:?} is not grey on a resting bar"
            );
        }

        let alerting = signal_bar_mark_colors(width, true, true);
        assert_eq!(
            alerting.get("~").copied().flatten(),
            Some(palette.green),
            "uncommitted work did not colour its own slot"
        );
        assert_eq!(
            alerting.get("◉").copied().flatten(),
            Some(palette.red),
            "a blocked agent did not colour its own slot"
        );
        // And the six that are still quiet have not moved.
        assert_eq!(
            alerting.get("⋔").copied().flatten(),
            Some(palette.overlay0),
            "a quiet slot changed colour because a different signal went live"
        );
    }

    /// The bar's whole promise: at rest it is still there, and it says what the
    /// eight things are.
    #[test]
    fn a_quiet_fleet_still_draws_all_eight_signals_named() {
        let width = notifications::Tier::Named.width() + 4;
        let rows = signal_bar_rows(width, false, false);
        let screen = rows.join("\n");

        for signal in crate::app::fleet_signals::FleetSignal::ALL {
            assert!(
                rows[0].contains(signal.name()),
                "{signal:?} is not named on a resting bar:\n{screen}"
            );
            assert!(
                rows[0].contains(signal.mark()),
                "{signal:?} has no mark on a resting bar:\n{screen}"
            );
        }
    }

    /// Going live changes a slot's colour and its motion, never its text. That
    /// is what lets a reader learn where a signal sits: the bar cannot reflow
    /// as alerts come and go.
    #[test]
    fn a_signal_going_live_never_moves_the_bar() {
        for width in [
            notifications::Tier::Named.width() + 4,
            notifications::Tier::Marks.width() + 2,
            notifications::Tier::Tight.width() + 1,
        ] {
            let resting = signal_bar_rows(width, false, false);
            let alerting = signal_bar_rows(width, true, true);
            assert_eq!(
                resting[0], alerting[0],
                "the bar's text moved at {width} columns when two signals went live"
            );
        }
    }

    /// The narrow ladder: names go first, then the gaps, and all eight marks
    /// survive to the floor.
    #[test]
    fn a_narrow_sidebar_keeps_every_signal_and_drops_the_names() {
        for (width, tier) in [
            (notifications::Tier::Marks.width(), "marks"),
            (notifications::Tier::Tight.width(), "tight"),
        ] {
            let rows = signal_bar_rows(width + 1, false, false);
            let screen = rows.join("\n");
            for signal in crate::app::fleet_signals::FleetSignal::ALL {
                assert!(
                    rows[0].contains(signal.mark()),
                    "{signal:?} vanished from the {tier} tier at {width} columns:\n{screen}"
                );
                assert!(
                    !rows[0].contains(signal.name()),
                    "{signal:?} still drew its name in the {tier} tier:\n{screen}"
                );
            }
        }
    }

    /// A real 26-column sidebar - the default width - still shows all eight.
    #[test]
    fn the_default_sidebar_width_holds_the_whole_bar() {
        let rows = signal_bar_rows(26, false, false);
        let screen = rows.join("\n");
        assert!(
            notifications::Tier::widest_fitting(25).is_some(),
            "the default sidebar cannot hold the bar at all"
        );
        for signal in crate::app::fleet_signals::FleetSignal::ALL {
            assert!(
                rows[0].contains(signal.mark()),
                "{signal:?} is missing at the default 26 columns:\n{screen}"
            );
        }
    }

    /// Off by default: an unconfigured Herdr draws the header row exactly as it
    /// did before the bar existed.
    #[test]
    fn the_bar_is_not_drawn_until_it_is_configured_on() {
        let without = mate_fleet_sidebar_rows(26, None);
        assert!(
            without[0].chars().all(|ch| ch.is_whitespace() || ch == '│'),
            "something drew on the header row of an unconfigured Herdr:\n{}",
            without.join("\n")
        );
    }

    /// The bar takes the left of the row and the status keeps the right, so
    /// turning the bar on never writes over a published status.
    #[test]
    fn the_bar_and_the_session_status_share_the_header_row_without_overlapping() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_notifications.enabled = true;
        app.session_status = Some("62%".to_string());

        let area = Rect::new(0, 0, 40, 12);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let row = row_text(terminal.backend().buffer(), 0, area.width);

        assert!(
            row.starts_with('●'),
            "the bar is not hung on the left: {row}"
        );
        assert!(
            row.trim_end_matches('│').ends_with("62%"),
            "the status is not right-aligned beside the bar: {row}"
        );

        // Both measured in columns: the bar's last slot has to end before the
        // status begins, or one has been drawn over the other.
        let columns: Vec<char> = row.chars().collect();
        let last_slot = columns
            .iter()
            .position(|ch| *ch == '⋔')
            .expect("the bar's last slot is missing");
        let status_start = columns
            .windows(3)
            .position(|window| window == ['6', '2', '%'])
            .expect("the status is missing");
        assert!(
            status_start > last_slot,
            "the status overlapped the bar: {row}"
        );
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

    /// A fleet with a rank past the display cap: `worker` owns `sub`, and
    /// `worker2` is a genuine sibling of `worker` below it. `sub` has nowhere
    /// deeper to draw, so it shares `worker`'s column.
    fn capped_fleet_sidebar_rows(width: u16) -> Vec<String> {
        let mut app = crate::app::state::AppState::test_new();
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let sub_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        let worker2_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        let worker_pane = *second_mate.tabs[0]
            .panes
            .keys()
            .find(|pane| **pane != sub_pane && **pane != worker2_pane)
            .expect("the original pane is still present");
        app.workspaces = vec![Workspace::test_new("firstmate"), second_mate];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        app.workspaces[1].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            now,
        );

        for (pane, name, owner) in [
            (worker_pane, "worker", "2ndmate-explore"),
            (sub_pane, "sub", "worker"),
            (worker2_pane, "worker2", "2ndmate-explore"),
        ] {
            let terminal_id = app.workspaces[1].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app
                .terminals
                .get_mut(&terminal_id)
                .expect("every test pane has a terminal");
            terminal.set_agent_name(name.to_string());
            terminal.state = AgentState::Idle;
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([("owner".to_string(), Some(owner.to_string()))]),
                None,
                now,
            );
        }

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

    /// A row clamped at the display cap used to draw `└`, closing a column that
    /// its parent's own later siblings still had rows in - so the flattened run
    /// showed two `└` and the clamped row read as the end of the branch.
    #[test]
    fn a_row_clamped_at_the_display_cap_does_not_close_its_parents_column() {
        let rows = capped_fleet_sidebar_rows(26);
        let screen = rows.join("\n");

        let row_of = |name: &str| {
            rows.iter()
                .position(|row| row.contains(name))
                .unwrap_or_else(|| panic!("{name} row missing:\n{screen}"))
        };
        let connector_indent = |row: usize| {
            rows[row]
                .find(['└', '├'])
                .unwrap_or_else(|| panic!("no connector on row {row}:\n{screen}"))
        };

        // `worker` is drawn above `worker2`, so the first match is the right one.
        let worker = row_of("worker");
        let sub = row_of("sub");
        let worker2 = row_of("worker2");
        assert!(
            worker < sub && sub < worker2,
            "the fleet is not in ownership order:\n{screen}"
        );

        // The cap is not extended: every rank-2 row shares one column.
        assert_eq!(
            connector_indent(sub),
            connector_indent(worker),
            "the clamped row was indented past the cap:\n{screen}"
        );
        assert_eq!(
            connector_indent(worker2),
            connector_indent(worker),
            "a true sibling left the shared column:\n{screen}"
        );

        // Exactly one row closes that column, and it is the genuinely last one.
        assert!(
            rows[sub].contains("├─ sub"),
            "the clamped row closed a column that continues below it:\n{screen}"
        );
        assert!(
            rows[worker].contains("├─ worker"),
            "the parent of a clamped row closed its own column:\n{screen}"
        );
        assert!(
            rows[worker2].contains("└─ worker2"),
            "the last row in the column did not close it:\n{screen}"
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

        // Narrow enough that `◐ one · shipping the token` does not fit on one
        // line, so the two configured lines stay stacked and the title row is
        // a row of its own to assert on.
        let area = Rect::new(0, 0, 26, 20);
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

    /// Runs the animation engine forward `elapsed_ms` for every workspace row,
    /// standing in for the app loop a render-only test does not have.
    fn advance_row_animation(app: &mut crate::app::state::AppState, elapsed_ms: u64) {
        let start = std::time::Instant::now();
        let lifecycle = app.sidebar_row_lifecycle();
        let live: Vec<_> = app
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    crate::anim::ElementId::workspace_row(&workspace.id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(start, crate::anim::Family::WorkspaceRow, &lifecycle, live);
        app.anim
            .advance(start + std::time::Duration::from_millis(elapsed_ms));
    }

    /// Renders one Space row whose sole token carries `style_table`, and returns
    /// the drawn style of the metadata value plus the app it rendered from.
    fn render_styled_space_token(
        style_table: &str,
        elapsed_ms: u64,
    ) -> (ratatui::style::Style, crate::app::state::AppState) {
        let config: crate::config::Config =
            toml::from_str(&format!("[ui.sidebar.spaces]\nrows = [[{style_table}]]\n"))
                .expect("styled space config");
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        advance_row_animation(&mut app, elapsed_ms);
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
        // The pulse loops over 1600ms, so its trough is half of that in.
        const HALF_CYCLE_MS: u64 = 800;
        let pulse = r##"{ token = "$dot", fg = "#a6e3a1", emphasis = "pulse" }"##;
        let (peak, app) = render_styled_space_token(pulse, 0);
        let (trough, _) = render_styled_space_token(pulse, HALF_CYCLE_MS);
        let (mid, _) = render_styled_space_token(pulse, HALF_CYCLE_MS / 2);
        let (returned, _) = render_styled_space_token(pulse, HALF_CYCLE_MS * 2);
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
    fn calm_configurations_render_identically_as_the_clock_advances() {
        let render_at = |elapsed_ms: u64| {
            let mut app = crate::app::state::AppState::test_new();
            app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
            app.active = Some(0);
            app.mode = Mode::Terminal;
            advance_row_animation(&mut app, elapsed_ms);
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
        for elapsed_ms in [100, 800, 1_234_567] {
            assert_eq!(
                render_at(elapsed_ms),
                baseline,
                "default sidebar changed {elapsed_ms}ms into the clock"
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
            &RowAnimation::for_workspace(&crate::app::state::AppState::test_new(), None),
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
        // Names long enough that no two of the six lines fit on one, so the
        // layout still asks for all six rows at this width.
        app.workspaces = vec![
            Workspace::test_new("space-one"),
            Workspace::test_new("space-two"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]; 6];
        // A six-row layout in a five-row body, so the clip is what is under
        // test rather than the row count.
        let area = Rect::new(0, 0, 20, 7);
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
        let entries = agent_panel_entries_with_runtimes(&app, Some(&runtime_registry));
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
    /// relation signal of `kind` advanced to `position` on the first child.
    ///
    /// `position` counts the charge's own sub-cell steps, in
    /// `0..=SIGNAL_POSITIONS`; the last of them is past the signal's expiry, so
    /// asking for it renders the row exactly as it settles once nothing is
    /// travelling it.
    ///
    /// Returns the rendered buffer alongside the app, so a caller can compare
    /// two positions, or a signalled render against an unsignalled one, cell by
    /// cell.
    fn render_signalled_tree(
        kind: Option<crate::app::relation_signal::RelationSignalKind>,
        position: u16,
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
            // Walk the clock to the requested position the same way the runtime
            // tick does, rather than reaching into the signal's internals.
            let step = crate::app::relation_signal::DEFAULT_SIGNAL_TTL
                / u32::from(crate::app::relation_signal::SIGNAL_POSITIONS);
            app.relation_signals
                .advance(now + step * u32::from(position) + std::time::Duration::from_millis(1));
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

    /// How far a cell's foreground has been pulled away from its settled ink.
    ///
    /// Stands in for "how lit is this cell": the charge tints the connector's
    /// own overlay colour toward the signal's, so the distance rises and falls
    /// exactly as the charge passes. Reading it off the rendered buffer rather
    /// than off the behaviour is the point — this is what the terminal got.
    fn lit(cell: &ratatui::buffer::Cell, settled: ratatui::style::Color) -> f32 {
        let rgb = crate::ui::color::color_to_rgb;
        match (cell.style().fg.and_then(rgb), rgb(settled)) {
            (Some(now), Some(base)) => {
                let apart = |a: u8, b: u8| (f32::from(a) - f32::from(b)).abs();
                apart(now.0, base.0) + apart(now.1, base.1) + apart(now.2, base.2)
            }
            _ => 0.0,
        }
    }

    /// The connector cell the charge is brightest on, ties going to the left.
    ///
    /// `None` when the charge is not on the connector at all — it has not
    /// arrived yet, or it has already run past the state icon. The route is
    /// four cells and only three of them are connector, so both directions
    /// spend part of their travel entirely off it; a caller comparing frames
    /// has to skip those rather than read a tie as a position.
    fn peak_cell(
        buffer: &ratatui::buffer::Buffer,
        at: (u16, u16),
        settled: ratatui::style::Color,
    ) -> Option<u16> {
        let mut peak = (0u16, 0.0f32);
        for cell in 0..u16::from(crate::app::relation_signal::CONNECTOR_CELLS) {
            let amount = lit(&buffer[(at.0 + cell, at.1)], settled);
            if amount > peak.1 {
                peak = (cell, amount);
            }
        }
        (peak.1 > 0.0).then_some(peak.0)
    }

    #[test]
    fn a_signal_frame_damages_only_the_branch_line_of_the_row_it_travels() {
        use crate::app::relation_signal::{RelationSignalKind, CONNECTOR_CELLS, SIGNAL_POSITIONS};

        // The whole route: from no signal, through every position, back to none.
        let mut frames = vec![render_signalled_tree(None, 0).0];
        for position in 0..SIGNAL_POSITIONS {
            frames.push(render_signalled_tree(Some(RelationSignalKind::Transfer), position).0);
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

    /// Every kind, so the vocabulary is exercised rather than just the two that
    /// existed before it.
    const EVERY_SIGNAL_KIND: [crate::app::relation_signal::RelationSignalKind; 4] = {
        use crate::app::relation_signal::RelationSignalKind as K;
        [K::Transfer, K::Completed, K::Failed, K::Idle]
    };

    #[test]
    fn a_charge_may_reshape_a_connector_cell_but_never_move_a_column() {
        // This replaces a rule that said a signal may change a cell's *style*
        // only. Everything that rule was protecting is asserted here in full:
        // nothing outside the connector's own three decorative glyphs is ever
        // reshaped, no substitute is a different width, and an expired signal
        // leaves the row byte-identical. What it also forbade — and no longer
        // does — is those three glyphs taking the shape of the charge running
        // through them, which is the only way a discharge can be drawn at all.
        use crate::app::relation_signal::{CONNECTOR_CELLS, SIGNAL_POSITIONS};

        let (calm, app) = render_signalled_tree(None, 0);
        let child = app.view.workspace_card_areas[1].rect;
        let connector = child.x + 3;
        let icon = connector + u16::from(CONNECTOR_CELLS);

        let mut reshaped = 0usize;
        for kind in EVERY_SIGNAL_KIND {
            // The last position is past expiry, so the loop ends by checking
            // that the row really does come all the way back.
            for position in 0..=SIGNAL_POSITIONS {
                let (signalled, _) = render_signalled_tree(Some(kind), position);
                for y in calm.area.y..calm.area.y + calm.area.height {
                    for x in calm.area.x..calm.area.x + calm.area.width {
                        let before = calm[(x, y)].symbol();
                        let after = signalled[(x, y)].symbol();
                        if before == after {
                            continue;
                        }
                        assert!(
                            position < SIGNAL_POSITIONS,
                            "{kind:?} left ({x}, {y}) as {after:?} after expiring; a signal that \
                             was skipped, cut short, or never drawn has to leave no trace"
                        );
                        assert!(
                            y == child.y && (connector..icon).contains(&x),
                            "{kind:?} reshaped ({x}, {y}), which is not one of the connector's \
                             own decorative cells: {before:?} -> {after:?}"
                        );
                        assert_eq!(
                            display_width(before),
                            display_width(after),
                            "{kind:?} changed the width of ({x}, {y}): {before:?} -> {after:?}"
                        );
                        reshaped += 1;
                    }
                }
            }
        }
        assert!(reshaped > 0, "the crackle has to actually take a shape");
    }

    #[test]
    fn a_transfer_and_a_completion_run_the_branch_line_in_opposite_directions() {
        use crate::app::relation_signal::RelationSignalKind;

        use crate::app::relation_signal::SIGNAL_POSITIONS;

        let (_, app) = render_signalled_tree(None, 0);
        let child = app.view.workspace_card_areas[1].rect;
        let settled = app.palette.overlay0;
        // Every frame the charge is genuinely on the connector, in order. The
        // two directions light it over different stretches of their travel, so
        // filtering on "is it lit" is what compares like with like.
        let track = |kind| -> Vec<u16> {
            (0..SIGNAL_POSITIONS)
                .filter_map(|position| {
                    let (buffer, _) = render_signalled_tree(Some(kind), position);
                    peak_cell(&buffer, (child.x + 3, child.y), settled)
                })
                .collect()
        };
        let inbound = track(RelationSignalKind::Transfer);
        let outbound = track(RelationSignalKind::Completed);

        assert!(
            inbound.windows(2).all(|pair| pair[1] >= pair[0]),
            "a transfer runs toward the icon and never doubles back: {inbound:?}"
        );
        assert!(
            outbound.windows(2).all(|pair| pair[1] <= pair[0]),
            "a completion runs toward the trunk and never doubles back: {outbound:?}"
        );
        assert!(
            inbound.first() < inbound.last() && outbound.first() > outbound.last(),
            "and both actually travel: {inbound:?} / {outbound:?}"
        );
    }

    #[test]
    fn each_kind_of_signal_draws_in_its_own_colour() {
        // The point of the vocabulary: a reader should be able to tell a
        // completion from a failure without timing an 800ms animation.
        use crate::app::relation_signal::CONNECTOR_CELLS;

        let (_, app) = render_signalled_tree(None, 0);
        let child = app.view.workspace_card_areas[1].rect;

        let mut inks = Vec::new();
        for kind in EVERY_SIGNAL_KIND {
            // Half-way along, where every kind has its charge on the connector.
            let (buffer, _) = render_signalled_tree(Some(kind), 14);
            let brightest = (0..u16::from(CONNECTOR_CELLS))
                .map(|cell| buffer[(child.x + 3 + cell, child.y)].style().fg)
                .max_by_key(|fg| {
                    fg.and_then(crate::ui::color::color_to_rgb)
                        .map(|(r, g, b)| u32::from(r) + u32::from(g) + u32::from(b))
                        .unwrap_or(0)
                })
                .flatten()
                .unwrap_or_else(|| panic!("{kind:?} lit no connector cell"));
            inks.push((kind, brightest));
        }

        for (index, (kind, ink)) in inks.iter().enumerate() {
            for (other_kind, other) in &inks[index + 1..] {
                assert_ne!(
                    ink, other,
                    "{kind:?} and {other_kind:?} are indistinguishable; the vocabulary only \
                     works if the categories do not collide"
                );
            }
        }
    }

    /// The blend fraction from `from` to `to` that produced `observed`, if all
    /// three channels agree on one.
    ///
    /// `None` when `observed` is not on that line at all, which is how a test
    /// catches a decoration having *replaced* a colour rather than having faded
    /// toward it.
    fn mix_fraction(
        from: crate::ui::color::Rgb,
        to: crate::ui::color::Rgb,
        observed: crate::ui::color::Rgb,
    ) -> Option<f32> {
        let channel = |a: u8, b: u8, seen: u8| {
            (a != b).then(|| (f32::from(seen) - f32::from(a)) / (f32::from(b) - f32::from(a)))
        };
        let found = [
            channel(from.0, to.0, observed.0),
            channel(from.1, to.1, observed.1),
            channel(from.2, to.2, observed.2),
        ];
        let found: Vec<f32> = found.into_iter().flatten().collect();
        let first = *found.first()?;
        found
            .iter()
            .all(|at| (at - first).abs() < 0.05 && (-0.02..=1.02).contains(at))
            .then_some(first)
    }

    #[test]
    fn an_arriving_signal_emphasises_the_state_icon_without_recolouring_it() {
        use crate::app::relation_signal::{CONNECTOR_CELLS, SIGNAL_POSITIONS};

        for kind in EVERY_SIGNAL_KIND {
            let mut reached = 0.0f32;
            for position in 0..SIGNAL_POSITIONS {
                let (buffer, app) = render_signalled_tree(Some(kind), position);
                let child = app.view.workspace_card_areas[1].rect;
                let icon = &buffer[(child.x + 3 + u16::from(CONNECTOR_CELLS), child.y)];

                let workspace = &app.workspaces[1];
                let (state, seen) = workspace.aggregate_state(&app.terminals);
                let (expected_symbol, expected_style) = state_dot(state, seen, &app.palette);
                let rgb = crate::ui::color::color_to_rgb;
                let ink = expected_style
                    .fg
                    .and_then(rgb)
                    .expect("a state dot always has a colour");
                let surface = rgb(app.palette.panel_bg).expect("the panel has a background");

                // The icon's glyph carries the agent's state, so unlike the
                // connector's own decorative cells it is never reshaped.
                assert_eq!(
                    icon.symbol(),
                    expected_symbol,
                    "{kind:?} at {position} reshaped the state icon"
                );

                // And the block it fades into is the state's *own* colour,
                // never the signal's. That colour is the state; a decoration
                // may emphasise what it decorates but not overwrite it.
                let mix = match icon.style().bg.and_then(rgb) {
                    None => 0.0,
                    Some(bg) => mix_fraction(surface, ink, bg).unwrap_or_else(|| {
                        panic!(
                            "{kind:?} at {position} painted the icon {bg:?}, which is not \
                             anywhere between the panel and the state's own colour"
                        )
                    }),
                };
                reached = reached.max(mix);
            }
            // Not 1.0: the charge is sampled at its own discrete positions, so
            // its peak lands near the icon rather than exactly on it. What is
            // being checked is that it genuinely arrives rather than fading out
            // somewhere along the connector.
            assert!(
                reached > 0.85,
                "{kind:?} never reached the state icon; it only got to {reached}"
            );
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
        let area = Rect::new(0, 0, 30, 4);
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
        let spacious_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 6));
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

        // One body row, so the metric has to come from the display entries
        // rather than the three raw workspaces. At 30 columns each entry folds
        // its name and branch onto that single line.
        let ws_area = Rect::new(0, 0, 30, 3);
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

        // Narrow enough that `● Doing · Ctx · one` does not fit on one line, so
        // the styled row and the name row stay stacked.
        let area = Rect::new(0, 0, 20, 24);
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
        let app = crate::app::state::AppState::test_new();
        resolved_token_spans(
            resolved,
            ("●", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &app.palette,
            &RowAnimation::for_workspace(&app, None),
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

    /// A two-mate fleet: `firstmate` owns `2nd-a` and `2nd-b`, and each second
    /// mate owns one worker pane. Enough shape that re-rooting has both a
    /// subtree to keep and a sibling subtree to leave behind.
    fn re_root_fleet() -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        let mut mate_a = Workspace::test_new("2nd-a");
        let worker_a = mate_a.test_split(ratatui::layout::Direction::Vertical);
        let mut mate_b = Workspace::test_new("2nd-b");
        let worker_b = mate_b.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![Workspace::test_new("firstmate"), mate_a, mate_b];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        for ws_idx in [1, 2] {
            app.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                now,
            );
        }
        for (ws_idx, pane, name, owner) in [
            (1usize, worker_a, "worker-a", "2nd-a"),
            (2, worker_b, "worker-b", "2nd-b"),
        ] {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).expect("test terminal");
            terminal.set_agent_name(name.to_string());
            terminal.state = AgentState::Idle;
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([("owner".to_string(), Some(owner.to_string()))]),
                None,
                now,
            );
        }
        app
    }

    /// The shape of the drawn list: one `(depth, label)` per row.
    fn tree_shape(app: &crate::app::state::AppState) -> Vec<(u8, String)> {
        let agents = sidebar_agent_entries(app);
        workspace_list_entries(app)
            .iter()
            .map(|entry| {
                let label = match entry {
                    WorkspaceListEntry::Workspace { ws_idx, .. } => app.workspaces[*ws_idx]
                        .display_name_from(&app.terminals, &TerminalRuntimeRegistry::new()),
                    WorkspaceListEntry::Agent { entry_idx, .. } => {
                        agents[*entry_idx].agent_name.clone().unwrap_or_default()
                    }
                };
                (entry.depth(), label)
            })
            .collect()
    }

    #[test]
    fn the_fleet_view_holds_every_mate_and_worker() {
        let app = re_root_fleet();
        assert_eq!(
            tree_shape(&app),
            vec![
                (0, "firstmate".to_string()),
                (1, "2nd-a".to_string()),
                (2, "worker-a".to_string()),
                (1, "2nd-b".to_string()),
                (2, "worker-b".to_string()),
            ]
        );
    }

    /// The captain's rule: the selected second mate takes the position the
    /// first mate held, its workers take the position the second mates held,
    /// and everything else is simply not there.
    #[test]
    fn selecting_a_second_mate_re_roots_the_tree_onto_it() {
        let mut app = re_root_fleet();
        app.tree_root = crate::app::tree_view::TreeRoot::Node("2nd-a".to_string());

        assert_eq!(
            tree_shape(&app),
            vec![(0, "2nd-a".to_string()), (1, "worker-a".to_string())]
        );
    }

    /// A re-rooted row is drawn in the root column, not merely renumbered: a
    /// mate still carrying a `├─` would be a row that had moved rank without
    /// moving column.
    #[test]
    fn the_new_root_draws_in_the_root_column_with_no_connector() {
        let mut app = re_root_fleet();
        app.tree_root = crate::app::tree_view::TreeRoot::Node("2nd-a".to_string());

        let width = 26;
        let area = Rect::new(0, 0, width, 20);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..20).map(|row| row_text(buffer, row, width)).collect();

        let root_row = rows
            .iter()
            .find(|row| row.contains("2nd-a"))
            .expect("the new root is drawn");
        assert!(
            !root_row.contains('├') && !root_row.contains('└'),
            "a re-rooted mate kept a child connector: {root_row:?}"
        );
        let worker_row = rows
            .iter()
            .find(|row| row.contains("worker-a"))
            .expect("its worker is drawn");
        assert!(
            worker_row.contains('└'),
            "the worker lost its connector: {worker_row:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("2nd-b")),
            "the sibling subtree was not left behind: {rows:?}"
        );
    }

    /// The way back out is a control on the one row above the tree, so it
    /// costs no column of a panel that has none to spare.
    #[test]
    fn a_re_rooted_tree_offers_a_way_back_on_the_header_row() {
        let mut app = re_root_fleet();
        let area = Rect::new(0, 0, 26, 20);
        app.view.sidebar_rect = area;

        assert_eq!(sidebar_tree_breadcrumb(&app), None);
        assert_eq!(
            sidebar_tree_breadcrumb_rect(&app, workspace_list_rect(area)),
            Rect::default()
        );

        app.tree_root = crate::app::tree_view::TreeRoot::Node("2nd-a".to_string());
        let rect = sidebar_tree_breadcrumb_rect(&app, workspace_list_rect(area));
        assert!(sidebar_tree_breadcrumb(&app).is_some());
        assert_eq!(
            rect.y,
            workspace_list_rect(area).y,
            "it sits above the tree"
        );
        assert!(rect.width > 0);
    }

    /// A mate whose Space is closed while its own view is open must not blank
    /// the panel.
    #[test]
    fn a_root_that_left_the_fleet_falls_back_to_the_whole_tree() {
        let mut app = re_root_fleet();
        app.tree_root = crate::app::tree_view::TreeRoot::Node("gone".to_string());

        assert_eq!(tree_shape(&app).len(), 5);
    }

    /// Reordering asks where a Space sits among every root, which is a fact
    /// about the session rather than about this viewer's current view.
    #[test]
    fn reordering_still_sees_the_whole_fleet_from_inside_a_mate_view() {
        let mut app = re_root_fleet();
        app.tree_root = crate::app::tree_view::TreeRoot::Node("2nd-a".to_string());

        assert_eq!(workspace_list_entries_whole_fleet(&app).len(), 5);
    }

    /// The switch does not swap the layout until the outgoing view has come
    /// apart, which is the one instant at which nothing appears to move.
    #[test]
    fn a_view_switch_holds_the_old_layout_until_the_panel_is_empty() {
        let mut app = re_root_fleet();
        let now = std::time::Instant::now();

        assert!(app.select_tree_root(
            crate::app::tree_view::TreeRoot::Node("2nd-a".to_string()),
            now
        ));
        assert_eq!(app.tree_root, crate::app::tree_view::TreeRoot::Fleet);
        assert_eq!(tree_shape(&app).len(), 5, "the old view is still drawn");
        assert_eq!(
            app.anim
                .frame(&crate::app::tree_view::view_element(), None)
                .expect("the view is animating")
                .phase,
            crate::anim::Phase::Dismount,
            "and it is on its way out"
        );

        let commit = app
            .next_tree_view_commit_deadline()
            .expect("the loop is told when to finish the switch");
        assert!(!app.advance_tree_view(commit - std::time::Duration::from_millis(1)));
        assert_eq!(tree_shape(&app).len(), 5);

        assert!(app.advance_tree_view(commit));
        assert_eq!(
            app.tree_root,
            crate::app::tree_view::TreeRoot::Node("2nd-a".to_string())
        );
        assert_eq!(tree_shape(&app).len(), 2);
        assert_eq!(
            app.anim
                .frame(&crate::app::tree_view::view_element(), None)
                .expect("the new view is animating")
                .phase,
            crate::anim::Phase::Mount,
            "the view being arrived at materializes"
        );
        assert_eq!(app.next_tree_view_commit_deadline(), None);
    }

    /// With the transition configured off the swap is immediate, and nothing
    /// is left animating.
    #[test]
    fn an_instant_switch_adopts_the_root_without_an_element() {
        let mut app = re_root_fleet();
        app.sidebar_animation.view_switch = crate::config::SidebarTokenEmphasis::None;

        assert!(app.select_tree_root(
            crate::app::tree_view::TreeRoot::Node("2nd-a".to_string()),
            std::time::Instant::now()
        ));
        assert_eq!(tree_shape(&app).len(), 2);
        assert_eq!(app.next_tree_view_commit_deadline(), None);
        assert!(app.anim.is_empty());
    }

    /// Publish the live agent rows the way `App::observe_agent_rows` does, for
    /// a test that has no app loop to do it.
    fn publish_agent_rows(
        app: &mut crate::app::state::AppState,
        now: std::time::Instant,
        lifecycle: &crate::anim::Lifecycle,
    ) {
        let rows: Vec<_> = sidebar_agent_live_entries(app)
            .iter()
            .map(|entry| {
                (
                    crate::anim::ElementId::agent_row(entry.pane_id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(now, crate::anim::Family::AgentRow, lifecycle, rows);
    }

    /// The hard requirement: a worker that spawns while the panel is coming
    /// apart is still its own arrival. Two independent lives, one engine —
    /// neither queued behind the other, and neither cancelling it.
    #[test]
    fn a_worker_arriving_mid_switch_animates_while_the_view_is_leaving() {
        let mut app = re_root_fleet();
        app.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::Dissolve;
        let now = std::time::Instant::now();

        // Settle the fleet that already exists, so the new worker is the only
        // thing that can be arriving. Same membership the runtime publishes.
        let lifecycle = app.sidebar_row_lifecycle();
        publish_agent_rows(&mut app, now, &lifecycle);
        let settled = now + std::time::Duration::from_secs(1);
        app.anim.advance(settled);

        // The switch starts...
        assert!(app.select_tree_root(
            crate::app::tree_view::TreeRoot::Node("2nd-a".to_string()),
            settled
        ));

        // ...and a second worker spawns under the mate that is still on screen.
        let spawned = app.workspaces[1].test_split(ratatui::layout::Direction::Vertical);
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[1].tabs[0].panes[&spawned]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("test terminal");
        terminal.set_agent_name("worker-a2".to_string());
        terminal.state = AgentState::Idle;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("2nd-a".to_string()))]),
            None,
            settled,
        );

        let mid = settled + std::time::Duration::from_millis(20);
        let spawned_id = crate::anim::ElementId::agent_row(spawned);
        assert!(
            sidebar_agent_live_entries(&app)
                .iter()
                .any(|entry| entry.pane_id == spawned),
            "a worker spawning mid-switch is still live membership"
        );
        publish_agent_rows(&mut app, mid, &lifecycle);

        assert_eq!(
            app.anim
                .frame(&spawned_id, None)
                .expect("the new worker is tracked")
                .phase,
            crate::anim::Phase::Mount,
            "the fleet event was not swallowed by the transition"
        );
        assert_eq!(
            app.anim
                .frame(&crate::app::tree_view::view_element(), None)
                .expect("the view is still animating")
                .phase,
            crate::anim::Phase::Dismount,
            "and the transition was not cancelled by the fleet event"
        );
        assert_eq!(
            app.next_tree_view_commit_deadline(),
            Some(settled + std::time::Duration::from_millis(220)),
            "nor was it restarted"
        );
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
