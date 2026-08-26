mod body_register;
mod card;
pub(crate) mod image_card;
pub(crate) mod motion;
mod notifications;
pub(crate) mod particle_background;
mod tokens;
pub(crate) mod tray;
pub(crate) mod tray_art;

pub(crate) use self::image_card::SidebarCardLayer;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use self::card::{Card, Pill, RowShell};
use self::tokens::{ResolvedToken, ResolvedTokenKind, SpaceTokenContext};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{state_icon, state_label, state_label_color};
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
    /// Who owns this pane: the published `owner` token if there is one, else
    /// the Space of the pane that created this one. See
    /// [`resolve_entry_owner`].
    pub owner: Option<String>,
    /// Whether a pane *in this same Space* asked Herdr to create this one.
    ///
    /// This is the membership test, not `owner`: it is true for every pane
    /// somebody in this Space delegated, including the ones whose owner did not
    /// resolve, and false for a pane that spun up a Space of its own. Stamped
    /// when the entry is built, so a row replayed from memory by
    /// [`rows_with_departing`] keeps the answer its Space gave it.
    pub delegated_in_space: bool,
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
            delegated_in_space: false,
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

/// What the active Agents view is holding back from the tree.
///
/// Resolved in [`crate::ui::compute_view`] and parked on
/// [`crate::app::state::ViewState::sidebar_view_hidden`], so the draw reads a
/// scalar instead of rebuilding the panel.
///
/// Answers early when no view is set, which is the unconfigured default: with
/// nothing to filter there is nothing to hold back, and this runs on a path
/// that already walks every pane of every tab of every Space. The rows are
/// still classified before filtering, because a view may filter on `relation`
/// and has to see what each pane *is*; only the tree arrangement is skipped,
/// since counting what was removed never needs the survivors placed.
pub(crate) fn agent_view_hidden(app: &AppState) -> AgentViewHidden {
    if app.agent_views.active().is_none() {
        return AgentViewHidden::default();
    }
    let mut entries = collect_agent_panel_entries_with_runtimes(app, None);
    crate::app::agent_tree::classify_agent_relations(&mut entries);
    crate::app::agent_view::apply_agent_view(app, &mut entries)
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
                    // Resolved before the detail is taken apart, because the
                    // rule reads two of its fields at once.
                    let owner = resolve_entry_owner(app, ws_idx, &detail);
                    let delegated_in_space = detail
                        .created_by
                        .as_ref()
                        .is_some_and(|origin| ws.id == origin.workspace_id);
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
                        owner,
                        state_labels: detail.state_labels,
                        delegated_in_space,
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
    worktree_child: bool,
    content_width: usize,
    shell: RowShell,
    body: Option<&body_register::BodyFacts>,
) -> u16 {
    let (state, seen) = ws.aggregate_state(&app.terminals);
    let label = if worktree_child {
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
            body,
            suppress_git_details: worktree_child,
            wall_now: app.wall_now,
        },
    );
    shell_row_height(
        shell_row_lines(rows, content_width, None, shell).len(),
        shell,
    )
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    worktree_child: bool,
    body_height: u16,
    content_width: usize,
    shell: RowShell,
    body: Option<&body_register::BodyFacts>,
) -> u16 {
    workspace_row_height(app, workspace, worktree_child, content_width, shell, body)
        .min(body_height)
}

fn workspace_entry_gap(app: &AppState, entries: &[WorkspaceListEntry], entry_idx: usize) -> u16 {
    // No gap anywhere inside a worktree group: not between two children, and
    // not between the parent and its first child either.
    if entry_idx + 1 < entries.len() && !next_entry_is_worktree_child(entries, entry_idx) {
        app.sidebar_spaces.row_gap
    } else {
        0
    }
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        // Unread shares the Idle-unread tier rather than inventing a new one:
        // output-scoped unread is agent-state-agnostic, so a `Working` or
        // `Unknown` pane with unseen content is exactly as attention-worthy
        // as an unseen `Idle` one.
        (AgentState::Working, false) | (AgentState::Idle, false) | (AgentState::Unknown, false) => {
            3
        }
        (AgentState::Working, true) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, true) => 0,
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
        /// This Space is a linked worktree of the Space that owns it.
        ///
        /// Purely a styling fact — it shortens the label, suppresses git
        /// detail, and says the row carries no group chevron of its own. It
        /// used to double as a second, parallel connector geometry, which put
        /// a worktree child's own connector in one column and its workers'
        /// rails in another; `depth` and `ancestors_continue` are now the only
        /// things any prefix is measured from, for every row alike.
        worktree_child: bool,
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

    /// The rank this row draws at: what it **is**, never who opened it.
    ///
    /// Deliberately not [`Self::depth`]. Depth says where a row *hangs* — it is
    /// the ownership edge, and it is what the connectors are measured from.
    /// Rank says what the row *is*, and it is what the card's size is measured
    /// from. They come apart in both directions, and for the same reason: a
    /// row's kind is the honest answer about what it is, and the depth of the
    /// chain that opened it is not.
    ///
    /// A worker the first mate opened directly hangs off the first mate,
    /// because that is genuinely who owns it, so it sits at depth 1 — the same
    /// level a second mate does. Reading its rank off that depth would make it
    /// a second mate purely because of who spawned it, and a second mate is a
    /// *card*: it would take a box of its own, at a mate's width, beside the
    /// mate that dispatched it. What it is instead is a row inside that mate's
    /// own box ([`crew_folds_into_its_space`]), which is the captain's rule and
    /// is available to it only as a worker.
    ///
    /// The rank still reaches the screen on its own where such a worker stands
    /// outside a box — a panel scrolled past its mate — and it is what
    /// [`card_frame_for`] measures the resulting card from.
    ///
    /// A mate nested under another mate is the mirror of it. A Space is what a
    /// mate *is*, so a second mate that has itself dispatched a second mate
    /// leaves a Space at depth 2 — and reading its rank off that depth would
    /// demote it to a worker purely because of who spawned it, drawing a
    /// persistent mate at exactly a one-off task's size. A mate is a Space
    /// wherever it hangs, which is what
    /// [`crate::app::agent_tree::AgentRelation::for_space`] says and
    /// [`crate::app::agent_tree::AgentRelation::from_depth`], which answers for
    /// panes, cannot.
    ///
    /// See [`rank_width_inset`] for what a rank is actually worth on screen,
    /// [`rank_residual_spans`] for how the rails reach a card whose rank and
    /// level disagree in either direction, and
    /// [`tie_workers_to_a_second_mate`] for the re-parenting that runs before
    /// this and keeps the worker fallback rare.
    pub(crate) fn rank(&self) -> crate::app::agent_tree::AgentRelation {
        match self {
            Self::Workspace { depth, .. } => {
                crate::app::agent_tree::AgentRelation::for_space(*depth)
            }
            Self::Agent { .. } => crate::app::agent_tree::AgentRelation::Worker,
        }
    }
}

/// Whether a Space's card is on screen carrying its own workers.
///
/// The crew list is a *card*: it exists exactly when a pixel card is actually
/// covering these rows, and not merely when the layout could size one. The two
/// come apart for a frame at a time — a panel whose cards have not been
/// published yet still draws its rows in characters — and a layout that had
/// already folded the workers into a box nothing was drawing would leave their
/// connectors standing inside a border that was not there. So this is
/// [`image_card::card_covers_row`], the same reading the character row stands
/// down on, and every part of the feature is gated on it: the heights, the gaps,
/// the stretched frame, and the rails.
fn crew_is_drawn(app: &AppState, fold_width: u16) -> bool {
    image_card::card_covers_row(app, fold_width)
}

/// Whether a worker is drawn inside its own mate's box rather than in a box of
/// its own.
///
/// **One rule at every level.** The captain settled it as a generalisation of
/// the first mate's own case: *"2nd mates' workers will function just like
/// firstmate's workers, only they show up in their respective 2nd
/// [mate's card]"*. Which box a worker belongs in is [`crew_head`]'s answer —
/// the nearest Space up its own parent chain — so nothing here has to know
/// which mate it is looking at, and a second mate's worker lands in the second
/// mate's card by the same walk that puts the first mate's in the first mate's.
/// A mate itself is unaffected: a Space is still its own card, still branch-
/// nested under whoever owns it.
///
/// True wherever the panel draws boxes at all, because a box is the whole of
/// the relation being drawn. Both renderers merge — the pixel card sets the
/// rows itself ([`image_card::crew`]) and the character card draws them under
/// the mate's own content ([`render_crew_row`]) — and below
/// [`card::MIN_FOLD_WIDTH`] a row is a styled line with no box to be inside of,
/// so every worker there keeps the row and the connector it always had.
fn crew_folds_into_its_space(fold_width: u16) -> bool {
    RowShell::for_fold_width(fold_width).is_card()
}

/// Columns a worker's row is stepped in inside the box it is drawn in.
///
/// The character twin of [`image_card::crew`]'s `INDENT_MUL`, and saturated the
/// same way: one step for a worker that reached this card through anybody at
/// all, and one step for anything deeper too. Past the first the indent has
/// stopped answering "did this come through somebody" and started eating a
/// 26-column sidebar.
const CREW_INDENT_COLS: u16 = 2;

fn crew_indent_cols(entries: &[WorkspaceListEntry], idx: usize) -> u16 {
    u16::from(crew_tier(entries, idx).unwrap_or(0).min(1)) * CREW_INDENT_COLS
}

/// The row that owns the row at `idx` in the drawn tree.
///
/// The nearest row above it at a shallower depth, which a depth-first walk makes
/// the parent by construction — [`crate::app::agent_tree::walk_tree`] emits every
/// child immediately after its parent and no shallower row can come between them.
fn tree_parent(entries: &[WorkspaceListEntry], idx: usize) -> Option<usize> {
    let depth = entries.get(idx)?.depth();
    (0..idx).rev().find(|row| entries[*row].depth() < depth)
}

/// The Space whose card the row at `idx` is drawn *inside*, if any.
///
/// A Space's card carries its workers in its own box now, under a dashed rule —
/// the captain's confirmed mockups. Which rows those are is not a new fact: the
/// tree walk has already put every worker under whatever owns it, so this is
/// simply "walk up the parents until a Space is reached". Nothing here reads a
/// token and nothing publishes one — see
/// [`crate::app::agent_tree::resolve_owner`], which is still the only thing that
/// decides who owns whom.
///
/// `None` when the walk runs out of parents without meeting a Space: a pane
/// standing at the root of the tree is a card in its own right, and so is
/// everything hanging off it. Following the *nearest Workspace above* instead of
/// the parent chain is what would break that — a root pane's own workers would
/// be folded into the card of a Space they are not inside.
pub(crate) fn crew_head(entries: &[WorkspaceListEntry], idx: usize) -> Option<usize> {
    if !matches!(entries.get(idx)?, WorkspaceListEntry::Agent { .. }) {
        return None;
    }
    let mut cursor = idx;
    // Bounded by the entry count: every step moves strictly up the list.
    for _ in 0..entries.len() {
        let parent = tree_parent(entries, cursor)?;
        match entries[parent] {
            WorkspaceListEntry::Workspace { .. } => return Some(parent),
            WorkspaceListEntry::Agent { .. } => cursor = parent,
        }
    }
    None
}

/// [`crew_head`], but only when that head is a row the panel is actually
/// drawing.
///
/// A card's box is drawn by its *head*, so a worker whose head has scrolled off
/// the top has nothing to be inside of: it goes back to being a card of its own
/// rather than becoming an invisible row under a border nobody drew. The panel's
/// own first drawn row is the whole test — the tree walk emits a head before
/// everything it owns, so a head at or after the scroll is on screen and one
/// before it is not.
pub(crate) fn drawn_crew_head(
    app: &AppState,
    entries: &[WorkspaceListEntry],
    idx: usize,
) -> Option<usize> {
    crew_head(entries, idx).filter(|head| *head >= app.workspace_scroll)
}

/// Which indent step the row at `idx` draws at inside its Space's card.
///
/// `0` for a worker the Space dispatched itself — its parent *is* the card it is
/// drawn in — and `1` for one that reached it through anybody at all. **`1` for
/// anything deeper too**: past the first step the indent has stopped answering
/// "did this come through somebody" and started eating a 26-column sidebar. The
/// same saturation [`crate::app::agent_tree::display_depth`] applies to the
/// tree's own columns, for the same reason.
pub(crate) fn crew_tier(entries: &[WorkspaceListEntry], idx: usize) -> Option<u8> {
    let head = crew_head(entries, idx)?;
    Some(u8::from(tree_parent(entries, idx) != Some(head)))
}

/// How many worker rows the Space at `head` carries inside its own card.
///
/// `0` for an `Agent` row — a worker dispatches nothing that draws under it —
/// and for a Space running nothing, which is the card every Space drew before
/// this existed.
pub(crate) fn crew_len(entries: &[WorkspaceListEntry], head: usize) -> usize {
    let Some(WorkspaceListEntry::Workspace { .. }) = entries.get(head) else {
        return 0;
    };
    let depth = entries[head].depth();
    entries[head.saturating_add(1)..]
        .iter()
        .take_while(|entry| {
            matches!(entry, WorkspaceListEntry::Agent { .. }) && entry.depth() > depth
        })
        .count()
}

/// Whether something hangs off the row at `idx`.
///
/// The next row one level deeper is exactly how the walk emits a subtree, so a
/// row opens a branch if and only if the row after it is drawn one column
/// further in. Measured in *drawn* depth — [`crate::app::agent_tree::display_depth`]
/// — because a row past the display cap shares its parent's column and hangs no
/// line of its own off it.
pub(crate) fn row_opens_a_branch(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    let drawn = |entry: &WorkspaceListEntry| crate::app::agent_tree::display_depth(entry.depth());
    entries
        .get(idx)
        .zip(entries.get(idx.saturating_add(1)))
        .is_some_and(|(row, next)| drawn(next) > drawn(row))
}

pub(crate) fn next_entry_is_worktree_child(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace {
            worktree_child: true,
            ..
        })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area);
    let body = workspace_list_body_rect(app, ws_area, false);
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

/// The stable identity a row draws its trunk segments and card wash under.
///
/// `None` for a row whose backing entry has already gone — `entry_idx`/`ws_idx`
/// briefly dangle between a pane closing and the next tree rebuild — which a
/// caller treats as "this row owns no segment right now" rather than an error.
fn entry_card_row(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
) -> Option<crate::anim::CardRow> {
    match entry {
        WorkspaceListEntry::Workspace { ws_idx, .. } => app
            .workspaces
            .get(*ws_idx)
            .map(|ws| crate::anim::CardRow::Space(ws.id.clone())),
        WorkspaceListEntry::Agent { entry_idx, .. } => agents
            .get(*entry_idx)
            .map(|entry| crate::anim::CardRow::Agent(entry.pane_id)),
    }
}

/// What one drawn row's colour channels are saying: its lifecycle stage, and
/// the open defect on it if there is one.
///
/// The renderer's door to [`crate::app::lifecycle::row_signal`], reading a row's
/// tokens and detected state from wherever this particular entry keeps them —
/// a Space from its own workspace, an agent row from the panel entry the tree
/// was built with. The app loop reaches the same function from the other side
/// when it decides which rows mount a marker at all.
fn entry_row_signal(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entry: &WorkspaceListEntry,
) -> Option<crate::app::lifecycle::RowSignal> {
    match entry {
        WorkspaceListEntry::Workspace { ws_idx, .. } => {
            let workspace = app.workspaces.get(*ws_idx)?;
            let (state, _seen) = workspace.aggregate_state(&app.terminals);
            Some(crate::app::lifecycle::row_signal(
                &workspace.metadata_tokens.values(),
                state,
            ))
        }
        WorkspaceListEntry::Agent { entry_idx, .. } => {
            let entry = agents.get(*entry_idx)?;
            Some(crate::app::lifecycle::row_signal(
                &entry.tokens,
                entry.state,
            ))
        }
    }
}

/// Which of a card's three breaths this Space row is on right now.
///
/// The publish-time half of [`image_card::breath_behaviour`]: the app loop needs
/// the same answer the renderer will reach, one pass earlier, so the engine can
/// step the row on that breath's tier instead of the fastest of the three the
/// row declares. Read from the same two facts the card reads — the row's
/// aggregate state and its severity token.
fn space_row_breath(app: &AppState, workspace: &crate::workspace::Workspace) -> &'static str {
    let (state, _) = workspace.aggregate_state(&app.terminals);
    let severity = crate::app::lifecycle::severity(
        workspace
            .metadata_tokens
            .values()
            .get(crate::app::lifecycle::SEVERITY_TOKEN)
            .map(String::as_str),
    );
    image_card::breath_behaviour(state, severity)
}

/// Which of a card's three breaths this Agent row is on right now.
///
/// See [`space_row_breath`]; the same two facts, read off the panel entry.
fn agent_row_breath(entry: &AgentPanelEntry) -> &'static str {
    let severity = crate::app::lifecycle::severity(
        entry
            .tokens
            .get(crate::app::lifecycle::SEVERITY_TOKEN)
            .map(String::as_str),
    );
    image_card::breath_behaviour(entry.state, severity)
}

/// Every Space row the app loop publishes to [`crate::anim::Animator`], with the
/// breath each one is playing.
pub(crate) fn sidebar_space_row_members(app: &AppState) -> Vec<crate::anim::Member> {
    app.workspaces
        .iter()
        .map(|workspace| crate::anim::Member {
            id: crate::anim::ElementId::workspace_row(&workspace.id),
            inputs: crate::anim::behaviour::DriveInputs {
                activity: app.workspace_activity_level(workspace),
            },
            playing: Some(space_row_breath(app, workspace)),
        })
        .collect()
}

/// Every Agent row the app loop publishes, with the breath each one is playing.
///
/// Takes the live entries the caller already gathered rather than gathering its
/// own, for the reason `observe_card_washes` gives: two readings of the tree in
/// one pass can disagree about which rows exist.
pub(crate) fn sidebar_agent_row_members(
    app: &AppState,
    live: &[AgentPanelEntry],
) -> Vec<crate::anim::Member> {
    live.iter()
        .map(|entry| crate::anim::Member {
            id: crate::anim::ElementId::agent_row(entry.pane_id),
            inputs: crate::anim::behaviour::DriveInputs {
                activity: app.pane_activity_level(entry.ws_idx, entry.pane_id),
            },
            playing: Some(agent_row_breath(entry)),
        })
        .collect()
}

/// Every trunk segment on screen right now: one per row with a gap still open
/// beneath it, at each ancestor column that gap belongs to.
///
/// This is what the app loop publishes to [`crate::anim::Animator`] so a
/// segment mounts the frame its gap opens and dismounts the frame it closes,
/// exactly mirroring the `│` cells [`agent_row_prefix`] and
/// [`card_rail_prefix`] already draw — a level counts as open here under
/// precisely the condition that makes those functions draw a rail glyph
/// there rather than blank space.
pub(crate) fn sidebar_trunk_segment_members(
    app: &AppState,
) -> Vec<(crate::anim::ElementId, crate::anim::behaviour::DriveInputs)> {
    let agents = sidebar_agent_entries(app);
    workspace_list_entries(app)
        .iter()
        .filter_map(|entry| entry_card_row(app, &agents, entry).map(|row| (row, entry.clone())))
        .flat_map(|(row, entry)| {
            let depth = crate::app::agent_tree::display_depth(entry.depth());
            let ancestors = entry.ancestors_continue().to_vec();
            (1..depth).filter_map(move |level| {
                let open = ancestors.get(level as usize).copied().unwrap_or(false);
                open.then(|| {
                    (
                        crate::anim::ElementId::trunk_segment(row.clone(), level),
                        crate::anim::behaviour::DriveInputs::default(),
                    )
                })
            })
        })
        .collect()
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
        // A worktree child names itself like any other Space. It is a node of
        // the ownership tree - workers hang off it by `owner` token - so it can
        // be re-rooted on, which for a fleet whose second mates are all linked
        // worktrees is the only thing there is to re-root on.
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
/// resolved before the ownership walk. It is carried *into* that walk rather
/// than emitted around it: a child is a node like any other, holding its parent
/// by index so the two kinds of nesting share one set of connectors instead of
/// competing for the same parent.
struct SpaceBlock {
    parent_idx: usize,
    children: Vec<usize>,
}

/// One Space's row in the tree, flattened out of its [`SpaceBlock`].
struct SpaceRow {
    ws_idx: usize,
    /// This Space is a linked worktree of the Space that owns it.
    ///
    /// A styling fact, not a depth — the depth is on the placement, the same as
    /// for every other row. It shortens the label, suppresses git detail, and
    /// says the row carries no worktree-group chevron of its own.
    worktree_child: bool,
    /// Index, within the same flat list, of the checkout this row was cut from.
    structural_parent: Option<usize>,
}

/// The handle an `owner` token uses to name this Space.
///
/// It is the Space's own label, because that is already what a fleet writes
/// into `owner` — `firstmate`, `2ndmate-explore` — so there is no second naming
/// scheme to keep in sync. Resolved without terminal runtimes so the tree's
/// shape cannot change with a terminal title.
pub(crate) fn space_tree_name(app: &AppState, ws_idx: usize) -> Option<String> {
    let ws = app.workspaces.get(ws_idx)?;
    let name = ws.display_name_from(&app.terminals, &TerminalRuntimeRegistry::new());
    (!name.trim().is_empty()).then_some(name)
}

/// Apply [`crate::app::agent_tree::resolve_owner`] to one pane of one Space.
///
/// The Space handle comes from [`space_tree_name`], the same function the Space
/// nodes are named with, so a derived owner always matches a real node rather
/// than a label that merely looks like one.
fn resolve_entry_owner(
    app: &AppState,
    ws_idx: usize,
    detail: &crate::workspace::aggregate::PaneDetail,
) -> Option<String> {
    let workspace_id = app.workspaces.get(ws_idx).map(|ws| ws.id.as_str())?;
    crate::app::agent_tree::resolve_owner(
        detail
            .tokens
            .get(crate::app::agent_tree::OWNER_TOKEN)
            .map(String::as_str),
        detail.created_by.as_ref(),
        workspace_id,
        space_tree_name(app, ws_idx).as_deref(),
    )
}

/// Who this Space says owns it, from its own `owner` metadata token.
///
/// A Space publishes this with `workspace report-metadata --token owner=...`,
/// the same token a pane uses. Nothing declares it by default, so a fleet that
/// publishes nothing gets the flat list it has always had.
pub(crate) fn space_owner(app: &AppState, ws_idx: usize) -> Option<String> {
    let ws = app.workspaces.get(ws_idx)?;
    ws.metadata_tokens
        .values()
        .get(crate::app::agent_tree::OWNER_TOKEN)
        .map(|owner| owner.trim().to_string())
        .filter(|owner| !owner.is_empty())
}

/// The agent panes that belong in the sidebar tree: the ones somebody owns, and
/// the ones somebody asked for.
///
/// A pane a *person* made is deliberately absent. In a fleet where the mates
/// are Spaces, a mate's own pane is opened from elsewhere and names no owner,
/// so drawing every agent pane would draw each mate twice — once as its Space
/// row and again as a child of itself. Belonging to somebody's tree is what
/// earns a row here.
///
/// But being *unplaceable* must not cost a pane its row — see
/// [`keeps_a_tree_row`], which is where that line is actually drawn.
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

/// Whether a pane has earned a row in the tree.
///
/// Membership is *delegation within this Space*, not a resolved owner. Those
/// two come apart in both directions, and each direction is a bug the other
/// predicate would cause:
///
/// - A pane delegated here whose owner did not resolve — the Space has no
///   usable label yet — still belongs, and draws at the root the way
///   [`crate::app::agent_tree::arrange_owner_tree`] already draws any owner it
///   cannot match. Requiring an owner here is what used to *delete* such a row,
///   which is how a run of merged pull requests went unseen. A flat row is a
///   bad tree; a missing row is a lie.
/// - A pane created from a *different* Space is a Space being spun up, not a
///   worker. It is already on screen as its own Space row, so admitting it here
///   too draws it twice — once as the Space, once as a root pane inside it.
///   That was observed live, not reasoned about: every second mate is created
///   from the first mate's Space, so this doubles the whole fleet.
///
/// An explicit `owner` token still admits a pane on its own, so a fleet that
/// publishes ownership by hand keeps exactly the rows it always had.
fn keeps_a_tree_row(entry: &AgentPanelEntry) -> bool {
    entry.owner.is_some() || entry.delegated_in_space
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
    enter_at_head(&mut entries);
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
    entries.retain(keeps_a_tree_row);
    entries
}

/// Where a card comes in: at the head of its parent's children, never the tail.
///
/// A new pane used to join the panel wherever the session happened to list it,
/// which is last — [`collect_agent_panel_entries_with_runtimes`] walks
/// workspaces and then panes in creation order, so a card arrived at the bottom
/// of its group and the row that was seen to open was the one furthest from the
/// row the user was looking at. Entry is the *top* of the group instead, so an
/// arrival happens where attention already is.
///
/// **Entry is not the sort, and this runs before it.** The sort still owns the
/// row from here — under `AgentPanelSort::Priority` it is free to move a
/// just-arrived card straight back down, and it must be, or a burst of new panes
/// would outrank a blocked one. What entry order decides is where a card sits
/// when the sort is indifferent: `sort_by_key` is stable, so a card the sort
/// ranks equal to its siblings keeps the head position this gave it. Under
/// `AgentPanelSort::Spaces` nothing sorts at all and entry order is the whole
/// answer, which is the mode where a card is actually watched arriving.
///
/// **"Highest allowed branch" is the parent's, not the panel's.** This only
/// establishes a *sibling* order; parentage is rebuilt afterwards from the
/// `owner` tokens by [`crate::app::agent_tree::arrange_owner_tree`], which
/// groups children by resolved parent and keeps whatever relative order it was
/// handed. A total order restricted to one parent's children is still that
/// order, so ordering the flat list newest-first puts the newest card first
/// inside every group at once and cannot lift a card out of the group that owns
/// it. Nothing but removal takes a card out of its parent.
///
/// Recency is [`crate::layout::PaneId`], which is a process-wide allocation
/// counter: a higher id is a pane that was created later, by construction. That
/// is why this needs no "is it new" flag and no remembered previous frame —
/// stating it as an ordering rule makes "every card entered at the head" an
/// invariant of the list rather than something one frame has to catch.
fn enter_at_head(entries: &mut [AgentPanelEntry]) {
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.pane_id.raw()));
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

/// Flatten `blocks` into one row per Space, each child pointing at the checkout
/// it was cut from.
///
/// This is what puts a worktree child *into* the ownership namespace. Emitting
/// it beside the walk instead — which is what this used to do — left it with no
/// entry in the name table `owner` tokens are matched against, so a fleet whose
/// second mates are linked worktrees of the first mate's repo could not nest a
/// single worker under one of them, however correct its tokens were.
fn space_rows(blocks: &[SpaceBlock]) -> Vec<SpaceRow> {
    let mut rows = Vec::new();
    for block in blocks {
        let parent_row = rows.len();
        rows.push(SpaceRow {
            ws_idx: block.parent_idx,
            worktree_child: false,
            structural_parent: None,
        });
        rows.extend(block.children.iter().map(|child_idx| SpaceRow {
            ws_idx: *child_idx,
            worktree_child: true,
            structural_parent: Some(parent_row),
        }));
    }
    rows
}

/// Hang a first-mate-opened worker off the second mate whose scope fits it.
///
/// The captain's rule, verbatim: *"if the firstmate opens a worker or subagent
/// it should always be tied to a secondmate."*
///
/// This is the tie, and it is all that is left of the rule. The captain's own
/// second clause — *"if there is no relevant secondmate have it create the card
/// as a sub agent as a branch under it"* — is superseded: a worker with no
/// fitting mate is now drawn *inside the first mate's own card* rather than as
/// a sub agent's card branched off it. See [`crew_folds_into_its_space`].
///
/// The re-parenting is unchanged either way. A worker with no fitting mate
/// keeps the first mate as its owner, because that is who genuinely opened it,
/// and it is [`WorkspaceListEntry::rank`] that stops the resulting depth from
/// promoting it to a mate. Moving it deeper instead would put it in a second
/// mate's column with no second mate above it to belong to.
///
/// **What "fits" means, and why it is this:** the Space the worker is actually
/// running in. A pane is *in* one checkout and one Space, and that is the scope
/// it is working on — so a worker running inside a second mate's Space belongs
/// to that mate whatever a token says about who spawned it. Nothing else
/// available is a scope: `owner` is provenance, and a name is a label.
///
/// It is expressed through [`OwnedNode::parent`] rather than by rewriting
/// `owner` for the same reason worktree membership is: it is a fact about where
/// the pane *is*, not a preference about who it answers to, and the parent
/// channel is the one that outranks the token. It joins the same cycle scan, so
/// it can stand a node up as a root but never strand it.
///
/// Narrow on purpose. It fires only for a worker whose owner resolves to a
/// Space at the **root** of the tree — a first mate — because that is the only
/// case the rule governs. A worker already under a second mate is untouched,
/// and so is one owned by another pane.
fn tie_workers_to_a_second_mate(
    rows: &[SpaceRow],
    names: &[Option<String>],
    agents: &[AgentPanelEntry],
    nodes: &mut [crate::app::agent_tree::OwnedNode<'_>],
) {
    let space_count = rows.len();
    if space_count == 0 || nodes.len() <= space_count {
        return;
    }
    let space_parents = crate::app::agent_tree::resolve_parents(&nodes[..space_count]);
    let root_of = |mut row: usize| {
        // Bounded by the node count: `resolve_parents` has already broken every
        // cycle, so this walk terminates.
        for _ in 0..space_count {
            match space_parents[row] {
                Some(parent) => row = parent,
                None => break,
            }
        }
        row
    };
    let depth_of = |row: usize| {
        let mut depth = 0usize;
        let mut cursor = space_parents[row];
        while let Some(parent) = cursor {
            depth += 1;
            cursor = space_parents[parent];
        }
        depth
    };

    // First writer wins, exactly as in the walk's own name table, so a name
    // resolved here can never point at a different row than the walk picks.
    let mut space_by_name: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for (idx, name) in names.iter().enumerate().take(space_count) {
        if let Some(name) = name {
            space_by_name.entry(name.as_str()).or_insert(idx);
        }
    }

    for (offset, node) in nodes[space_count..].iter_mut().enumerate() {
        let Some(agent) = agents.get(offset) else {
            continue;
        };
        let Some(owner_row) = node
            .owner
            .and_then(|owner| space_by_name.get(owner).copied())
        else {
            continue;
        };
        if depth_of(owner_row) != 0 {
            continue;
        }
        // A second mate of *this* first mate, running the Space this worker is
        // in. Two first mates in one panel is not the captain's fleet, but a
        // worker must not be re-homed across trees on a bare Space match.
        let fitting = (0..space_count).find(|row| {
            rows[*row].ws_idx == agent.ws_idx && depth_of(*row) == 1 && root_of(*row) == owner_row
        });
        if let Some(mate) = fitting {
            node.parent = Some(mate);
        }
    }
}

/// Which band of a parent's children a worker pane draws in, and which one a
/// Space draws in. Workers first: a mate's own directly-dispatched workers are
/// the rows that belong immediately beneath it.
///
/// Without this the order was decided by node index, and the node list is
/// Spaces then panes purely because that is how [`arrange_space_tree`] builds
/// it. So a first mate's own workers sorted below *every* second mate — and
/// because the walk is depth-first, below each of those mates' own workers too,
/// which put the first mate's most immediate rows at the very bottom of the
/// panel with three levels of somebody else's tree in between.
///
/// Stated as two bands rather than by reordering the node list, because the
/// node list's order is also its identity: [`arrange_space_tree`] maps a
/// placement back to a Space or a pane by comparing its index against the Space
/// count. Order and identity should not be the same fact.
const WORKER_SIBLING_GROUP: u8 = 0;
const SPACE_SIBLING_GROUP: u8 = 1;

/// Arrange `blocks` and the owned agent panes into one tree, then flatten it.
///
/// Every Space and every owned pane goes through
/// [`crate::app::agent_tree::arrange_owner_tree`] as a node, so a worker nests
/// under its second mate's Space by exactly the same rule that nests a second
/// mate under the first mate — including when that second mate is a linked
/// worktree, which is the shape a fleet run out of one repo actually has.
///
/// Within one parent, its worker panes draw before its Spaces — see
/// [`WORKER_SIBLING_GROUP`].
fn arrange_space_tree(
    app: &AppState,
    blocks: &[SpaceBlock],
    agents: &[AgentPanelEntry],
) -> Vec<WorkspaceListEntry> {
    let rows = space_rows(blocks);
    let names: Vec<Option<String>> = rows
        .iter()
        .map(|row| space_tree_name(app, row.ws_idx))
        .collect();
    // A worktree child publishes no owner into the walk: the checkout it hangs
    // off is already carried by `structural_parent`, and a token that could
    // move it would let a Space leave the group its repository puts it in.
    let owners: Vec<Option<String>> = rows
        .iter()
        .map(|row| {
            (!row.worktree_child)
                .then(|| space_owner(app, row.ws_idx))
                .flatten()
        })
        .collect();

    let mut nodes: Vec<crate::app::agent_tree::OwnedNode<'_>> = rows
        .iter()
        .zip(&names)
        .zip(&owners)
        .map(|((row, name), owner)| crate::app::agent_tree::OwnedNode {
            name: name.as_deref(),
            owner: owner.as_deref(),
            parent: row.structural_parent,
            sibling_group: SPACE_SIBLING_GROUP,
        })
        .collect();
    nodes.extend(
        agents
            .iter()
            .map(|entry| crate::app::agent_tree::OwnedNode {
                name: entry.agent_name.as_deref(),
                owner: entry.owner.as_deref(),
                parent: None,
                sibling_group: WORKER_SIBLING_GROUP,
            }),
    );
    tie_workers_to_a_second_mate(&rows, &names, agents, &mut nodes);

    crate::app::agent_tree::arrange_owner_tree(&nodes)
        .into_iter()
        .map(|placement| match rows.get(placement.index) {
            Some(row) => WorkspaceListEntry::Workspace {
                ws_idx: row.ws_idx,
                worktree_child: row.worktree_child,
                depth: placement.depth,
                ancestors_continue: placement.ancestors_continue,
                is_last_child: placement.is_last_child,
            },
            None => WorkspaceListEntry::Agent {
                entry_idx: placement.index - rows.len(),
                depth: placement.depth,
                ancestors_continue: placement.ancestors_continue,
                is_last_child: placement.is_last_child,
            },
        })
        .collect()
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
/// footer the `new` button and the collapse toggle sit on — less whatever the
/// notification tray has reserved at the foot.
///
/// The tray's rows come out here, in the one function every part of the tree's
/// geometry already goes through, rather than by shrinking the panel rect
/// upstream. Doing it upstream would move the footer the `new` button is
/// measured against and put a blank row above the tray; doing it here leaves
/// every other coordinate exactly where it was.
pub(crate) fn workspace_list_body_rect(app: &AppState, area: Rect, has_scrollbar: bool) -> Rect {
    let tray_rows = tray::reserved_rows(app, area);
    if area.width == 0
        || area.height <= WORKSPACE_SECTION_HEADER_ROWS + WORKSPACE_SECTION_FOOTER_ROWS + tray_rows
    {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y
        + area
            .height
            .saturating_sub(WORKSPACE_SECTION_FOOTER_ROWS)
            .saturating_sub(tray_rows);
    let body_height = footer_y.saturating_sub(body_y);
    Rect::new(
        area.x,
        body_y,
        workspace_list_body_width(area, has_scrollbar),
        body_height,
    )
}

/// The columns a tree row is drawn into, on a panel of this width.
///
/// Split out from [`workspace_list_body_rect`] because it is a pure function of
/// the panel's *width*, and some callers only have one. The notification tray
/// takes rows off the bottom of the panel and never a column off its side, so a
/// question about width has no business needing the app's state to answer it —
/// and `card_shell_min_sidebar_width` is exactly that question, asked of a
/// hypothetical panel that does not exist yet.
fn workspace_list_body_width(area: Rect, has_scrollbar: bool) -> u16 {
    area.width.saturating_sub(u16::from(has_scrollbar))
}

/// Columns one tree row spends on rails and connector before its first token.
///
/// The single place the prefix is measured. The layout subtracts it to decide
/// how many lines a row needs and the renderer subtracts it again to decide how
/// much each line may draw, so the two cannot disagree about how much room the
/// row had - which is the whole reason a row's height may depend on its width
/// at all.
///
/// Depth is the only input, for a Space, a worktree child and an owned pane
/// alike. A worktree child used to be measured from its own parallel geometry,
/// which is exactly how a second mate's connector ended up in a different
/// column from the workers railing off it.
fn tree_prefix_width(depth: u8, row_index: usize) -> usize {
    let depth = crate::app::agent_tree::display_depth(depth) as usize;
    match (depth, row_index) {
        (0, 0) => 1,
        (0, _) => 3,
        (_, 0) => 3 * depth + 1,
        (_, _) => 3 * depth + 3,
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
fn row_fold_width(app: &AppState, list_area: Rect) -> u16 {
    workspace_list_body_rect(app, list_area, true).width
}

#[cfg(test)]
mod fold_width_is_a_width {
    use super::*;

    /// The detent the sidebar drag sticks at is a property of the panel's
    /// width, and must not move when the tray is switched on: the tray takes
    /// rows off the bottom, never a column off the side. A regression here
    /// would make the drag stick at a different column depending on a setting
    /// that has nothing to do with width.
    #[test]
    fn the_card_shell_detent_does_not_move_when_the_tray_is_on() {
        let detent = card_shell_min_sidebar_width();
        for width in [detent - 1, detent, detent + 1, 60] {
            let area = Rect::new(0, 0, width, 40);
            let list = workspace_list_rect(area);

            let mut app = AppState::test_new();
            app.sidebar_signal_tray.enabled = false;
            let without = row_fold_width(&app, list);
            app.sidebar_signal_tray.enabled = true;
            let with = row_fold_width(&app, list);

            assert_eq!(with, without, "the tray changed the fold width at {width}");
            assert_eq!(
                with,
                workspace_list_body_width(list, true),
                "the fold width stopped agreeing with the width-only path at {width}"
            );
        }
    }
}

/// The narrowest sidebar that draws cards rather than bare lines.
///
/// Derived by asking the real geometry rather than by naming a number: the
/// panel's fold width is its own width less the content inset and the
/// scrollbar, and a change to either would otherwise leave a hardcoded
/// threshold pointing at the wrong column. The drag detent in
/// [`crate::app::AppState::set_manual_sidebar_width`] is anchored on this, so
/// the column the width sticks at is always the column the shell changes at.
pub(crate) fn card_shell_min_sidebar_width() -> u16 {
    // A tall probe rect, so the body is never the thing that limits the width.
    (1..=MAX_PROBE_SIDEBAR_WIDTH)
        .find(|width| {
            // The width-only path: the shell a panel draws depends on how many
            // columns a row gets, and the tray changes rows rather than columns.
            workspace_list_body_width(workspace_list_rect(Rect::new(0, 0, *width, 24)), true)
                >= card::MIN_FOLD_WIDTH
        })
        .unwrap_or(card::MIN_FOLD_WIDTH)
}

/// Upper bound for the [`card_shell_min_sidebar_width`] probe. Far wider than
/// any sidebar the bounds allow, so the search always terminates on the real
/// answer rather than on the limit.
const MAX_PROBE_SIDEBAR_WIDTH: u16 = 512;

/// Columns a row has for its tokens once its prefix, its rank's inset, its
/// shell and any trailing control are taken out.
fn row_content_width(
    fold_width: u16,
    depth: u8,
    rank: crate::app::agent_tree::AgentRelation,
    trailing_width: usize,
) -> usize {
    (fold_width as usize)
        .saturating_sub(usize::from(card_left_offset(depth, rank, fold_width)))
        .saturating_sub(usize::from(
            RowShell::for_fold_width(fold_width).chrome_cols(),
        ))
        .saturating_sub(trailing_width)
}

/// Columns per rank the card's right edge pulls in, so width reads as rank.
///
/// Three, because that is the number the tree already spends indenting one
/// level on the left; the right edge mirrors it rather than inventing a second
/// step size.
const RANK_INSET_STEP: u16 = 3;

/// How many steps in from the top rank a rank sits.
///
/// Not [`crate::app::agent_tree::MAX_DISPLAY_DEPTH`] by coincidence — the ranks
/// and the drawn columns are the same three levels — but it is spelled from the
/// rank so a change to the display cap cannot silently restripe the ladder.
fn rank_steps(rank: crate::app::agent_tree::AgentRelation) -> u16 {
    use crate::app::agent_tree::AgentRelation;
    match rank {
        AgentRelation::FirstMate => 0,
        AgentRelation::SecondMate => 1,
        AgentRelation::Worker => 2,
    }
}

/// Columns this rank gives up out of its card's width.
///
/// # Why it is spent on the left
///
/// A width difference between the ranks already existed and it did not read as
/// one. The left edge alone carried it — a card starts at its connector's
/// column, so each level was three columns narrower than its parent — but every
/// card still *ended* in the same column, and a reader comparing two cards
/// compares their right edges. So the captain saw ranks that were 7% apart and
/// no ladder: *"i just mean the sub agent width difference compared to
/// 2ndmate."*
///
/// The answer was a mirrored inset at the **right** edge, which doubled the step
/// to six columns a rank and made a staircase of the right edges. That staircase
/// is what the captain has since ruled out: a card inset on both sides is the
/// *symmetric* layout, and he wants every card aligned to the right side
/// instead. So the same six-column step is spent entirely on the left now —
/// [`card_left_offset`] — and the right edges go flush against the panel. The
/// ladder still reads, off the left edges, which is also the edge the tree's own
/// wires stand in.
///
/// **The width itself is unchanged.** Only the anchor moved, so the fold, the
/// pill reservation and the drag detent all see exactly the columns they saw
/// before.
///
/// # Why it is spent out of slack
///
/// [`card::MIN_FOLD_WIDTH`] is the width at which the deepest row stops fitting
/// a pill and a readable subtitle, and [`card_shell_min_sidebar_width`] turns
/// that into the column the sidebar drag detent sticks at. A ladder that spent
/// columns unconditionally would push the deepest card under that floor and
/// move the detent — a drag behaviour nobody asked to change — so the ladder is
/// paid for only out of what a panel has *above* the floor. At the floor the
/// step is zero and every rank draws exactly the width it always did; the full
/// mirror is in force from six columns of slack up, which the captain's sidebar
/// has eight of.
///
/// A pure function of `(rank, fold_width)`, like everything else the fold is
/// measured with, so the layout and the renderer cannot disagree about where a
/// card ends and no width decision feeds its own input.
fn rank_width_inset(rank: crate::app::agent_tree::AgentRelation, fold_width: u16) -> u16 {
    let steps = rank_steps(rank);
    if steps == 0 {
        return 0;
    }
    let deepest_steps = rank_steps(crate::app::agent_tree::AgentRelation::Worker);
    let slack = fold_width.saturating_sub(card::MIN_FOLD_WIDTH);
    let step = (slack / deepest_steps).min(RANK_INSET_STEP);
    step * steps
}

/// Columns one level of the ladder is worth at this fold — the amount every
/// tree column moves right, per level, to pay for right alignment.
///
/// Spelled as one step of [`rank_width_inset`] rather than as its own constant,
/// so the ladder the cards are sized by and the columns the wires stand in are
/// the same number by construction and cannot be tuned apart.
fn rank_step(fold_width: u16) -> u16 {
    rank_width_inset(
        crate::app::agent_tree::AgentRelation::SecondMate,
        fold_width,
    )
}

/// The column the tree's level-`depth` line stands in.
///
/// **This is the wire contract.** A card's left border, the `│` an open ancestor
/// continues at that level, and the `├`/`└` of every child hanging off it are
/// all this one column — which is what "the trunk is aligned with the first
/// mate" and "the branches are aligned with the second mates" mean. A child's
/// connector is therefore `tree_column(depth - 1)`, one level up from its own.
///
/// Right alignment is paid for here rather than by sliding cards off their
/// wires: every level moves right by [`rank_step`], so the whole tree — lines
/// included — widens together and the cards' right edges come out flush.
///
/// Read off the *level*, not off a row's rank, because a column is shared by
/// every row standing in it and a rank is one row's own. The two agree for
/// every worker under a mate, and they come apart both ways — a worker the
/// first mate opened is drawn narrower than its column, a mate nested under a
/// mate wider than its own. [`fit_rails_to_card`] is where a row's rails are
/// reconciled with the card they point at, whichever way round the two fell.
///
/// [`tree_prefix_columns`] is the same number per *row*, which is what the
/// renderer actually draws and the layout actually budgets against.
fn tree_column(depth: u8, fold_width: u16) -> u16 {
    let drawn = crate::app::agent_tree::display_depth(depth);
    tree_prefix_width(depth, 0) as u16
        + rank_width_inset(
            crate::app::agent_tree::AgentRelation::from_depth(drawn),
            fold_width,
        )
}

/// Columns one row of a level-`depth` node spends before its own content.
///
/// The right-aligned twin of [`tree_prefix_width`], which it replaces
/// everywhere the *drawn* prefix is measured. A continuation row still sits two
/// columns further right than its first row, so wrapped text aligns under the
/// name rather than under the state mark.
fn tree_prefix_columns(depth: u8, row_index: usize, fold_width: u16) -> usize {
    tree_column(depth, fold_width) as usize + if row_index == 0 { 0 } else { 2 }
}

/// The column a row's card starts in, measured from the left edge of the fold.
///
/// The one place the right-aligned layout is expressed, so the frame, the rails
/// and the connector cannot disagree about where a card begins. Its width is
/// simply everything left over — see [`card_frame_for`] — which is what puts
/// every right edge flush against the panel.
fn card_left_offset(
    depth: u8,
    rank: crate::app::agent_tree::AgentRelation,
    fold_width: u16,
) -> u16 {
    tree_prefix_width(depth, 0) as u16 + rank_width_inset(rank, fold_width)
}

/// The columns between where a row's own tree column ends and where its card
/// actually begins.
///
/// Zero for every row whose rank is the one its level implies, which is every
/// worker under a mate. It is non-zero for the row that is drawn *narrower*
/// than its level — a worker the first mate opened, which hangs at a second
/// mate's level and is drawn a sub agent's width — and there it is the last
/// stretch of that row's own branch, running on to the border it points at.
/// Blank on any other row of the same card, because nothing is travelling
/// there.
///
/// Zero as well for a row drawn *wider* than its level, which cannot be
/// expressed as columns to add: see [`fit_rails_to_card`], which is what every
/// row is actually measured with.
fn rank_residual_spans(
    depth: u8,
    rank: crate::app::agent_tree::AgentRelation,
    fold_width: u16,
    meets_a_card: bool,
    p: &Palette,
) -> Vec<Span<'static>> {
    let residual = card_left_offset(depth, rank, fold_width)
        .saturating_sub(tree_column(depth, fold_width)) as usize;
    if residual == 0 {
        return Vec::new();
    }
    // At the root there is no branch to carry: the row hangs off nothing, so
    // the columns are plain indentation rather than a line reaching anywhere.
    if depth == 0 || !meets_a_card {
        return vec![Span::raw(" ".repeat(residual))];
    }
    vec![Span::styled(
        "─".repeat(residual),
        Style::default().fg(p.overlay0),
    )]
}

/// Run a row's rails to the column its card actually begins in.
///
/// The one place a rank is reconciled with the level it hangs at, so no caller
/// has to know which way the two came apart. [`card_rail_prefix`] and
/// [`agent_row_prefix`] measure a row's rails to [`tree_column`] — the column
/// its *level* stands in — because a column is shared by every row standing in
/// it. A row's card begins at [`card_left_offset`], which is its own, and the
/// two disagree in both directions:
///
/// - Card further **right** than the column — a worker the first mate opened.
///   The branch carries on across the gap, which is [`rank_residual_spans`].
/// - Card further **left** than the column — a mate nested under a mate, drawn
///   a mate's width in a level whose ladder rung is a worker's. There are no
///   columns to add; the rails have to give back the ones the level claimed, or
///   they would be drawn *inside* the card they are pointing at.
///
/// Giving them back is a trim rather than a narrower prefix so that
/// `card_rail_prefix` stays a pure function of the level, which is what keeps
/// every row standing in a column aligned with every other.
fn fit_rails_to_card(
    spans: &mut Vec<Span<'static>>,
    depth: u8,
    rank: crate::app::agent_tree::AgentRelation,
    fold_width: u16,
    meets_a_card: bool,
    p: &Palette,
) {
    let column = tree_column(depth, fold_width);
    let card = card_left_offset(depth, rank, fold_width);
    if card >= column {
        spans.extend(rank_residual_spans(
            depth,
            rank,
            fold_width,
            meets_a_card,
            p,
        ));
    } else {
        trim_spans_right(spans, usize::from(column - card));
    }
}

/// Drop `columns` off the right-hand end of an already-built row prefix.
///
/// Rails are single-column glyphs and padding, so a span that has to be cut
/// part-way is cut by characters. Anything wider than one column per character
/// is dropped whole rather than split, because half of a wide glyph is not a
/// column of anything.
fn trim_spans_right(spans: &mut Vec<Span<'static>>, columns: usize) {
    let mut left = columns;
    while left > 0 {
        let Some(last) = spans.last_mut() else { return };
        let width = display_width(&last.content);
        let chars = last.content.chars().count();
        if width <= left || width != chars {
            left = left.saturating_sub(width);
            spans.pop();
            continue;
        }
        let kept: String = last.content.chars().take(chars - left).collect();
        last.content = kept.into();
        return;
    }
}

/// Columns reserved at the right edge of a Space row's first line for the
/// worktree group chevron, which is drawn over the row rather than laid out in
/// it.
fn space_trailing_width(app: &AppState, ws_idx: usize, worktree_child: bool) -> usize {
    2 * usize::from(!worktree_child && workspace_parent_group_state(app, ws_idx).is_some())
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
            ws_idx,
            worktree_child,
            ..
        } => row_content_width(
            fold_width,
            entry.depth(),
            entry.rank(),
            space_trailing_width(app, *ws_idx, *worktree_child) + badge,
        ),
        WorkspaceListEntry::Agent { .. } => {
            row_content_width(fold_width, entry.depth(), entry.rank(), badge)
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

/// Columns between the pulse row and whatever shares its row.
const HEADER_ROW_GAP: u16 = 2;

/// Draw the panel's reserved header row: the fleet pulse, then the session
/// status in whatever is left.
///
/// The pulse takes its columns from the left and the status right-aligns in the
/// remainder, so the two never overlap and neither has to know the other's
/// content. The pulse is measured first on purpose: it is a permanent readout
/// whose positions a reader learns, while the status is arbitrary publisher
/// text that already knows how to elide and how to drop when it cannot be read.
fn render_header_row(app: &AppState, frame: &mut Frame, area: Rect) {
    let header = workspace_list_header_rect(area);
    if header.height == 0 {
        return;
    }

    // Resolved once and reused for both the width and the draw. Reading the
    // fleet is a walk of every pane in every tab in every workspace, and this
    // row is not allowed to walk it twice per frame.
    let pulse = notifications::Pulse::resolve(app);
    let pulse_width = pulse.map_or(0, |pulse| pulse.width(header.width));
    if let (Some(pulse), true) = (pulse, pulse_width > 0) {
        pulse.render(
            app,
            frame,
            Rect::new(header.x, header.y, pulse_width, header.height),
        );
    }

    let mut taken = pulse_width.saturating_add(if pulse_width > 0 { HEADER_ROW_GAP } else { 0 });

    // The way out of a re-rooted tree sits after the pulse, never before it.
    // The pulse is a permanent readout whose positions a reader learns, and a
    // control that comes and goes with the current view must not shift it.
    let breadcrumb = breadcrumb_rect_after_pulse(app, area, pulse_width);
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

    // Next after the breadcrumb, for the same reason it sits there: both are
    // controls that come and go with the current view rather than permanent
    // readouts, so neither may shift the pulse a reader has learned to find.
    let notice = view_notice_rect_after(app, area, taken);
    if notice.width > 0 {
        if let Some(label) = tree_view_notice_label(app.view.sidebar_view_hidden) {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    label,
                    Style::default().fg(if app.view.sidebar_view_hidden.hidden_blocked > 0 {
                        app.palette.yellow
                    } else {
                        app.palette.overlay0
                    }),
                )),
                notice,
            );
            taken = taken
                .saturating_add(notice.width)
                .saturating_add(HEADER_ROW_GAP);
        }
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

/// Leads the view notice. The same funnel the Agents view grammar is named for,
/// so the row reads as "a view is filtering" rather than "the fleet is idle".
const TREE_VIEW_NOTICE_GLYPH: &str = "⧨ ";

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
/// away from a row. It follows the fleet pulse rather than preceding it,
/// so a control that comes and goes with the current view never shifts a
/// permanent readout. `area` is the panel's own list rect.
pub(crate) fn sidebar_tree_breadcrumb_rect(app: &AppState, area: Rect) -> Rect {
    let header = workspace_list_header_rect(area);
    // Reads the fleet for itself, which the render path must not do a second
    // time — see [`breadcrumb_rect_after_pulse`]. This entry point exists for
    // hit-testing a click, which happens once per event rather than once per
    // frame.
    let pulse = notifications::fleet_pulse_width(app, header.width);
    breadcrumb_rect_after_pulse(app, area, pulse)
}

/// The breadcrumb's rect given the columns the pulse row already took.
///
/// Split out so the render path, which has just measured the pulse, does not
/// have to walk every pane again to learn the same number. Measured against the
/// same allocation `render_header_row` walks, so the control is hit-tested
/// exactly where it was drawn.
/// What the header says when the active view is holding rows back.
///
/// `None` when it is holding nothing back, which leaves the row exactly as it
/// was before the slot existed. The blocked count is called out separately
/// because a blocked agent is waiting on the user: "5 hidden" is a filter doing
/// its job, and "5 hidden · 1 blocked" is somebody waiting behind it.
pub(crate) fn tree_view_notice_label(hidden: AgentViewHidden) -> Option<String> {
    if !hidden.any() {
        return None;
    }
    let mut label = format!("{}{} hidden", TREE_VIEW_NOTICE_GLYPH, hidden.hidden);
    if hidden.hidden_blocked > 0 {
        label.push_str(&format!(" · {} blocked", hidden.hidden_blocked));
    }
    Some(label)
}

/// The columns the view notice takes, or an empty rect when it does not fit.
///
/// Measured against the same header the breadcrumb is measured against, and it
/// yields the whole slot rather than truncating: a clipped count is a wrong
/// count, the same rule the git counters and the state age already follow.
fn view_notice_rect_after(app: &AppState, area: Rect, taken: u16) -> Rect {
    let Some(label) = tree_view_notice_label(app.view.sidebar_view_hidden) else {
        return Rect::default();
    };
    let header = workspace_list_header_rect(area);
    if header.height == 0 {
        return Rect::default();
    }
    let width = display_width(&label) as u16;
    if header.width.saturating_sub(taken) < width {
        return Rect::default();
    }
    Rect::new(header.x.saturating_add(taken), header.y, width, 1)
}

fn breadcrumb_rect_after_pulse(app: &AppState, area: Rect, pulse_width: u16) -> Rect {
    let Some(label) = sidebar_tree_breadcrumb(app) else {
        return Rect::default();
    };
    let header = workspace_list_header_rect(area);
    if header.height == 0 {
        return Rect::default();
    }
    let offset = pulse_width.saturating_add(if pulse_width > 0 { HEADER_ROW_GAP } else { 0 });
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
            Style::default().fg(app.sidebar_palette.overlay0),
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
    entries: &[WorkspaceListEntry],
    entry_idx: usize,
    body_height: u16,
    fold_width: u16,
    bodies: &body_register::BodyRegister,
) -> u16 {
    let Some(entry) = entries.get(entry_idx) else {
        return 0;
    };
    // The pixel card is a skin over this row's cells, so it does not get to
    // move the row — but it does get to say how many cells the row is, because
    // a card drawn shorter than its rect would leave a band of the character
    // card showing under it. Taken here, above the Space/agent split, because
    // both kinds of row are drawn by the same card: a mate is a Space and a
    // worker is a pane, and a tree that skinned one and not the other would be
    // two designs stacked on each other.
    //
    // It does not take the entry either: card height is uniform across every
    // rank by the captain's decision, and width is the rank signal. See
    // `image_card::BASE_HEIGHT_PX`.
    if let Some(rows) = image_card::row_height_cells(app, fold_width) {
        // A worker drawn inside its Space's card is still a *row*: it keeps its
        // own rect, so clicking it still selects its pane and the engine still
        // has something to open and close when it arrives and leaves. What it no
        // longer keeps is a card's height — it is two lines of type inside
        // somebody else's box.
        if crew_is_drawn(app, fold_width) && drawn_crew_head(app, entries, entry_idx).is_some() {
            return image_card::crew_row_cells(app, fold_width)
                .unwrap_or(rows)
                .min(body_height);
        }
        // The dashed rule is carried by the head and not by the first worker,
        // so every worker row is the same height — which is what lets a row
        // arriving anywhere in the list push the ones below it by exactly one
        // row's worth.
        let divider = if crew_is_drawn(app, fold_width) && crew_len(entries, entry_idx) > 0 {
            image_card::crew_divider_cells(app, fold_width).unwrap_or(0)
        } else {
            0
        };
        return rows.saturating_add(divider).min(body_height);
    }
    // A worker inside its mate's own box is two lines of type in somebody
    // else's border, so it reserves its lines and no chrome at all. The box
    // above it grows over them instead — see [`stretch_cards_over_their_crew`].
    if let Some(crew) = crew_row_layout(app, agents, entries, entry_idx, fold_width) {
        return crew.height().min(body_height);
    }
    let content_width = list_entry_content_width(app, agents, entry, fold_width);
    let shell = RowShell::for_fold_width(fold_width);
    match entry {
        WorkspaceListEntry::Workspace {
            ws_idx,
            worktree_child,
            ..
        } => app
            .workspaces
            .get(*ws_idx)
            .map(|ws| {
                workspace_row_height_in_body(
                    app,
                    ws,
                    *worktree_child,
                    body_height,
                    content_width,
                    shell,
                    bodies.get(&crate::anim::CardRow::Space(ws.id.clone())),
                )
            })
            .unwrap_or(0),
        WorkspaceListEntry::Agent { entry_idx, .. } => agents
            .get(*entry_idx)
            .map(|entry| agent_entry_height_in_body(app, entry, body_height, content_width, shell))
            .unwrap_or(0),
    }
}

/// A worker's row as it is laid out inside its own mate's box.
///
/// The layout and the renderer both resolve one of these, for the reason
/// [`shell_row_height`] exists: a row whose reserved height and drawn lines
/// disagree puts every card below it on the wrong row.
struct CrewRowLayout {
    /// Columns from the row's own left edge to where its type starts: the
    /// mate's card offset, the border and the pad inside it, then this row's
    /// own tier step.
    text_offset: u16,
    /// Columns it has for that type.
    text_width: u16,
    /// Whether it reached this card through another worker rather than straight
    /// from the mate whose box it is in. See [`crew_tier`].
    via_mate: bool,
    /// Whether this is the last worker in its mate's list, and so the row the
    /// box closes under.
    ///
    /// **It reserves a row for that rule.** A mate's own rect already pays for
    /// two chrome rows — the top border and the closing one — and when the box
    /// is stretched the closing rule *moves*, down past every worker, leaving
    /// the row it vacated to be the dashed rule between the mate and its crew
    /// ([`Card::render_crew_rule`]). Something has to pay for it where it
    /// landed, and the row it landed under is this one. Without it the box
    /// closes on the last worker's own last line and the rule is drawn over a
    /// name.
    closes_the_box: bool,
    /// Its folded token lines.
    lines: Vec<Vec<ResolvedToken>>,
}

impl CrewRowLayout {
    /// Rows this reserves: its lines, and the closing rule when it carries one.
    fn height(&self) -> u16 {
        shell_row_height(self.lines.len(), RowShell::Line)
            .saturating_add(u16::from(self.closes_the_box))
    }
}

/// How a worker's row is laid out inside its mate's box, or `None` when this
/// row is not drawn inside one.
///
/// Every measurement is taken off the *head's* card and not off the worker's
/// own rank: the row is standing in that border, so the columns it has are the
/// ones that border left it, less the step its tier costs. A width read off the
/// worker's own rank would be the width of the card it no longer draws.
///
/// Nothing is reserved for a control drawn over the row, because a crew row
/// carries none: the worker-summary badge belongs to the card, and the card is
/// its mate's. That is the pixel list's own rule — [`image_card::crew`] draws a
/// name, a status line and a marker and nothing else — and reserving columns
/// here for a badge no renderer draws would clip a worker's name to buy space
/// for nothing.
///
/// The lines are folded on the bare line's rules rather than stacked on the
/// card's, because a crew row *is* a line — the compact one-per-worker row the
/// mockups put under the rule, not a second card's worth of block set inside
/// the first.
fn crew_row_layout(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entries: &[WorkspaceListEntry],
    entry_idx: usize,
    fold_width: u16,
) -> Option<CrewRowLayout> {
    if !crew_folds_into_its_space(fold_width) {
        return None;
    }
    let head_idx = drawn_crew_head(app, entries, entry_idx)?;
    let head = entries.get(head_idx)?;
    let WorkspaceListEntry::Agent {
        entry_idx: agent_idx,
        ..
    } = entries.get(entry_idx)?
    else {
        return None;
    };
    let detail = agents.get(*agent_idx)?;
    let (left, width) = card_frame_columns(head, fold_width)?;
    let indent = crew_indent_cols(entries, entry_idx);
    let text_width = width
        .saturating_sub(card::CHROME_COLS)
        .saturating_sub(indent);
    Some(CrewRowLayout {
        text_offset: left.saturating_add(card::EDGE_COLS).saturating_add(indent),
        text_width,
        via_mate: indent > 0,
        closes_the_box: crew_head(entries, entry_idx.saturating_add(1)) != Some(head_idx),
        lines: shell_row_lines(
            resolved_agent_rows(app, detail),
            usize::from(text_width),
            None,
            RowShell::Line,
        ),
    })
}

/// The row a crew row has to keep free below it for the box to close.
///
/// A worker that ends its mate's list reserves the closing rule itself
/// ([`CrewRowLayout::closes_the_box`]). Every worker *before* it reserves
/// nothing, because the rule is not coming there — unless the panel runs out of
/// room and stops on one, and then it is: the box has to close under the last
/// worker the panel drew, and with nothing kept free the rule lands on that
/// worker's own last line.
///
/// So a placement walk inside a crew asks for one row more than the row itself
/// takes. Nothing consumes it while the list continues, and a list the panel
/// cannot finish stops one worker earlier — the same trade
/// [`a_card_is_drawn_whole_or_not_at_all`] already makes for a card.
///
/// Zero under a pixel card, which owes nothing here: its closing edge is drawn
/// *inside* the cells its own band already has, so a cut list needs no row of
/// anybody's to close over.
fn crew_closing_reserve(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entries: &[WorkspaceListEntry],
    entry_idx: usize,
    fold_width: u16,
) -> u16 {
    if crew_is_drawn(app, fold_width) {
        return 0;
    }
    crew_row_layout(app, agents, entries, entry_idx, fold_width)
        .map(|crew| u16::from(!crew.closes_the_box))
        .unwrap_or(0)
}

/// Gap after one tree row. Each kind keeps its own `row_gap`; the compact
/// worktree-group packing is unchanged.
fn list_entry_gap(
    app: &AppState,
    entries: &[WorkspaceListEntry],
    entry_idx: usize,
    fold_width: u16,
) -> u16 {
    // No gap anywhere *inside* a card: between a Space and the first worker in
    // its own box, or between two of them. The same rule a worktree group
    // already follows, for the same reason — a gap here would be a band of
    // panel showing through the middle of one border.
    //
    // Gated on there being a box at all, because a gap is only "inside" one if
    // one is drawn: below the card shell's own floor a row is a styled line,
    // and a worker there is still its own row with its own air around it.
    if crew_folds_into_its_space(fold_width)
        && crew_head(entries, entry_idx.saturating_add(1)).is_some()
    {
        return 0;
    }
    match entries.get(entry_idx) {
        Some(WorkspaceListEntry::Workspace { .. }) => workspace_entry_gap(app, entries, entry_idx),
        Some(WorkspaceListEntry::Agent { .. }) => agent_entry_gap(app, entry_idx, entries.len()),
        None => 0,
    }
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(app, area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let fold_width = row_fold_width(app, area);
    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    // Ranked once for the whole walk. See [`body_register`].
    let bodies = body_register::BodyRegister::resolve(app);
    for entry_idx in scroll..entries.len() {
        let row_height = list_entry_height(
            app,
            &agents,
            &entries,
            entry_idx,
            body.height,
            fold_width,
            &bodies,
        );
        if row_height == 0 {
            continue;
        }
        if used_rows
            .saturating_add(row_height)
            .saturating_add(crew_closing_reserve(
                app, &agents, &entries, entry_idx, fold_width,
            ))
            > body.height
        {
            break;
        }
        used_rows = used_rows.saturating_add(row_height);
        visible += 1;
        used_rows = used_rows
            .saturating_add(list_entry_gap(app, &entries, entry_idx, fold_width))
            .min(body.height);
    }
    visible
}

fn workspace_list_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = workspace_list_body_rect(app, area, false);
    let fold_width = row_fold_width(app, area);
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    let bodies = body_register::BodyRegister::resolve(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for entry_idx in (0..entries.len()).rev() {
        let row_height = list_entry_height(
            app,
            &agents,
            &entries,
            entry_idx,
            body.height,
            fold_width,
            &bodies,
        );
        if row_height == 0 {
            continue;
        }
        let needed =
            row_height.saturating_add(list_entry_gap(app, &entries, entry_idx, fold_width));
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
    let body = workspace_list_body_rect(app, area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

fn resolved_agent_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = agent_status_label(entry);
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
    shell: RowShell,
) -> u16 {
    shell_row_height(
        shell_row_lines(resolved_agent_rows(app, entry), content_width, None, shell).len(),
        shell,
    )
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
    let body = workspace_list_body_rect(app, ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let fold_width = row_fold_width(app, ws_area);
    // Whether a drawn card is what sets every row's height, resolved once for
    // the whole pass rather than per row: it is the same answer for all of them
    // (card height is uniform across every rank) and answering it costs a font
    // lookup. `list_entry_height` asks the same question again per row, which is
    // where the height itself comes from; this is only the fact that it did.
    let drawn_card = image_card::row_height_cells(app, fold_width).is_some();
    let nested = crew_folds_into_its_space(fold_width);
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    // Ranked once for the whole walk. See [`body_register`].
    let bodies = body_register::BodyRegister::resolve(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = list_entry_height(
            app,
            &agents,
            &entries,
            entry_idx,
            body.height,
            fold_width,
            &bodies,
        );
        if row_height == 0 {
            continue;
        }
        if row_y
            .saturating_add(row_height)
            .saturating_add(crew_closing_reserve(
                app, &agents, &entries, entry_idx, fold_width,
            ))
            > body_bottom
        {
            break;
        }
        let (ws_idx, worktree_child, agent) = match entry {
            WorkspaceListEntry::Workspace {
                ws_idx,
                worktree_child,
                ..
            } => (*ws_idx, *worktree_child, None),
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
        let rect = Rect::new(body.x, row_y, body.width, row_height);
        cards.push(crate::app::state::WorkspaceCardArea {
            ws_idx,
            rect,
            worktree_child,
            entry_idx,
            agent,
            // A worker drawn inside its mate's box opens no box of its own —
            // that is the whole of the change — so it is handed no frame to
            // draw one from. Its own columns are that mate's border's, resolved
            // where the row is drawn, by [`crew_row_layout`].
            //
            // Below the card shell's own floor there is no box to be inside of,
            // so a worker is still its own row with its own shell and nothing
            // about that panel changes.
            card_frame: (!nested || drawn_crew_head(app, &entries, entry_idx).is_none())
                .then(|| card_frame_for(rect, entry, fold_width))
                .flatten(),
            motion_cells: (0, 0),
            arriving: false,
            drawn_card,
        });
        row_y = row_y
            .saturating_add(row_height)
            .saturating_add(list_entry_gap(app, &entries, entry_idx, fold_width))
            .min(body_bottom);
    }

    if nested {
        stretch_cards_over_their_crew(app, &agents, &entries, &mut cards, fold_width);
    }
    (cards, headers)
}

/// Grow every Space's card frame down over the worker rows drawn inside it.
///
/// The rows themselves are unchanged — a crew row keeps its own rect, which is
/// what a click on a worker lands on and what the engine opens and closes when
/// it arrives. What changes is the *box*: one border around the Space and
/// everything it is running, rather than one border each.
///
/// **Every Space, not the first mate's.** The head is simply the last Space row
/// walked past, so a second mate's own workers stretch the second mate's box
/// and stop at the second mate's own border — which is the whole of the
/// captain's generalisation. See [`crew_folds_into_its_space`].
///
/// Measured off the crew rows that were actually laid out, so a list the panel
/// ran out of room part-way through closes its box at the last row it drew
/// instead of reaching past the bottom of the panel.
fn stretch_cards_over_their_crew(
    app: &AppState,
    agents: &[AgentPanelEntry],
    entries: &[WorkspaceListEntry],
    cards: &mut [crate::app::state::WorkspaceCardArea],
    fold_width: u16,
) {
    let mut head: Option<usize> = None;
    for index in 0..cards.len() {
        let entry_idx = cards[index].entry_idx;
        if drawn_crew_head(app, entries, entry_idx).is_some() {
            let Some(head) = head else {
                continue;
            };
            // A worker that ends its mate's list reserved the closing rule in
            // its own rect. One that does not did not — so where the panel
            // stops on it, the box closes on the row kept free below it.
            let bottom = cards[index]
                .rect
                .y
                .saturating_add(cards[index].rect.height)
                .saturating_add(crew_closing_reserve(
                    app, agents, entries, entry_idx, fold_width,
                ));
            if let Some(frame) = cards[head].card_frame.as_mut() {
                frame.height = bottom.saturating_sub(frame.y);
            }
            continue;
        }
        head = matches!(
            entries.get(entry_idx),
            Some(WorkspaceListEntry::Workspace { .. })
        )
        .then_some(index);
    }
}

/// Where this row's card shell stands, if the panel is wide enough to draw one.
///
/// The box begins in the column the row's connector points at — which is why
/// the `├──` runs into the card's left border rather than stopping beside the
/// name — and ends at the fold width less what its rank gives up. Both edges
/// are measured with the same functions the fold is, so the layout and the
/// renderer cannot disagree about which columns are frame.
///
/// The right edge is the panel's, for every row: the cards are right-aligned, so
/// a reader's eye runs down one straight edge instead of a staircase.
///
/// Both of the two questions a row answers are therefore spent on the *left*
/// edge, via [`card_left_offset`]: [`WorkspaceListEntry::depth`], because it is
/// where the row *hangs*, and [`WorkspaceListEntry::rank`], because it is what
/// the row *is*.
///
/// `head` is the row whose *border* this frame belongs to, which is the row
/// itself for everything that reaches here: every Space, and every worker on a
/// panel too narrow to draw boxes at all. A worker on a panel that does draw
/// them opens no box and is handed no frame — it is type inside its own mate's
/// border, and the columns it stands in come from [`card_frame_columns`] by way
/// of [`crew_row_layout`] instead.
fn card_frame_for(rect: Rect, head: &WorkspaceListEntry, fold_width: u16) -> Option<Rect> {
    let (left, width) = card_frame_columns(head, fold_width)?;
    // A box has to hold a border, a line and a rule; anything shorter is a
    // frame around nothing.
    (rect.height > card::CHROME_ROWS)
        .then(|| Rect::new(rect.x.saturating_add(left), rect.y, width, rect.height))
}

/// The columns a box drawn for `head` occupies: its left offset into the row,
/// and its width. `None` on a panel that draws no boxes at all.
fn card_frame_columns(head: &WorkspaceListEntry, fold_width: u16) -> Option<(u16, u16)> {
    if !RowShell::for_fold_width(fold_width).is_card() {
        return None;
    }
    let left = card_left_offset(head.depth(), head.rank(), fold_width);
    let width = fold_width.saturating_sub(left);
    (width > card::CHROME_COLS).then_some((left, width))
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

/// The glyph marking "these workers reported back".
const WORKER_SUMMARY_BADGE_GLYPH: &str = "▤";

/// What the badge prints for `count` finished workers, without the mark in
/// front of it.
///
/// Two digits is the widest it ever gets, so the badge cannot eat an
/// unbounded slice of a 26-wide sidebar however large a mate's crew grows.
///
/// Split out because the pixel card *draws* the mark instead of setting it —
/// [`WORKER_SUMMARY_BADGE_GLYPH`] is U+25A4 and the proportional faces a card
/// can be set in are not guaranteed to carry it — and a card that clamped the
/// count for itself would be a second opinion about what ten workers says.
pub(crate) fn worker_summary_count_label(count: usize) -> String {
    if count > 9 {
        "9+".to_string()
    } else {
        count.to_string()
    }
}

/// What the badge prints for `count` finished workers.
pub(crate) fn worker_summary_badge_label(count: usize) -> String {
    format!(
        "{WORKER_SUMMARY_BADGE_GLYPH}{}",
        worker_summary_count_label(count)
    )
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
        card.control_right().saturating_sub(1 + width),
        card.content_y(),
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

/// The tree handle a row answers to, the name a worker's `owner` token would
/// have to spell to nest under it.
///
/// Spaces and agent panes name themselves differently — a Space by its label, a
/// pane by `agent rename` — so this is the one place that difference is
/// resolved, and both kinds of row become eligible for a badge by the same
/// rule.
///
/// Resolved from the entry rather than from the card: the layout has entries
/// but no cards yet, and it has to know whether a row earns a badge before it
/// can decide how wide that row's content is.
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
    let entry = entries.get(card.entry_idx)?;
    worker_summary_badge_for_entry(app, entry, agents)
}

/// The same answer, resolved from the entry alone.
///
/// The pixel card's placement pass walks entries and cards together and already
/// holds the entry, so it asks here rather than looking the same entry up again
/// through `card.entry_idx`. One rule, reached two ways.
pub(crate) fn worker_summary_badge_for_entry(
    app: &AppState,
    entry: &WorkspaceListEntry,
    agents: &[AgentPanelEntry],
) -> Option<(String, usize)> {
    let name = entry_tree_name(app, agents, entry)?;
    let count = crate::app::worker_summary::summary_count_for_owner(agents, &name);
    (count > 0).then_some((name, count))
}

pub(crate) fn workspace_group_chevron_rect(card: &crate::app::state::WorkspaceCardArea) -> Rect {
    if card.rect.width == 0 || card.rect.height == 0 {
        return Rect::default();
    }

    Rect::new(
        card.control_right().saturating_sub(1),
        card.content_y(),
        1,
        1,
    )
}

/// The fill the panel is painted with, when the theme gives it one.
///
/// The single answer to "what colour is under the sidebar", asked by every pass
/// that composites inside the panel: the animated ink and the view-switch
/// dissolve here, the card shell's gradient and plates in
/// [`card`], the tray's engraved badges in [`tray`], and the pixel cards' bloom
/// in [`image_card`]. `render_sidebar` fills the panel with
/// `palette.sidebar_bg`, so that — and nothing else — is what a card's edge, a
/// badge's carve or a dissolving cell actually lands on.
///
/// `None` is the default: `Color::Reset` means "inherit the host", and the
/// panel then has no fill of its own. Each caller keeps its own answer for that
/// case, because what a pass should fall back to is a property of what it
/// draws, not of the panel — the ink wants a colour it can mix toward, the card
/// declines to tint what it cannot measure, and the tray falls back to the
/// canvas its marks were designed against.
pub(crate) fn panel_fill_rgb(
    p: &Palette,
    host: &crate::terminal_theme::TerminalTheme,
) -> Option<crate::ui::color::Rgb> {
    crate::ui::color::resolve_color_rgb(p.sidebar_bg, host)
}

/// The colour animated ink composites against inside the panel.
///
/// The panel's own fill first, then the RGB Herdr measured with OSC 11 — which
/// is what an unfilled panel is showing — and the panel background only for a
/// host that answered neither.
pub(crate) fn backdrop_rgb(app: &AppState) -> Option<crate::ui::color::Rgb> {
    let host = &app.host_terminal_theme;
    panel_fill_rgb(&app.palette, host)
        .or_else(|| host.background.map(crate::ui::color::terminal_theme_to_rgb))
        .or_else(|| crate::ui::color::resolve_color_rgb(app.palette.panel_bg, host))
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

    let p = &app.sidebar_palette;
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(p.sidebar_bg));
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
        let (icon, icon_style) = state_icon(agg_state, agg_seen, app.status_indicators, p);
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
                Span::styled(format!("{:<2}", visible_idx + 1), num_style),
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
                    worktree_child: false,
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
        Some(WorkspaceListEntry::Workspace {
            worktree_child: true,
            ..
        })
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
///
/// A fourth state sits on top of those three: while a drag is held at the
/// card/line shell boundary the whole bar goes heavy and accented, not just the
/// grip. The width has stopped following the pointer at that moment, and
/// without something to see, a divider that ignores the hand reads as a stuck
/// drag. Lighting the full height says the resistance is the boundary rather
/// than a fault, and it resolves the instant the detent commits.
fn render_sidebar_divider(app: &AppState, frame: &mut Frame, area: Rect, is_navigating: bool) {
    let p = &app.sidebar_palette;
    let active = app.sidebar_divider_hover;
    let detent = app.sidebar_divider_detent;
    let bar_style = if detent {
        Style::default().fg(p.accent)
    } else if active {
        Style::default().fg(p.overlay1)
    } else if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let grip_style = if active || detent {
        Style::default().fg(p.accent)
    } else if is_navigating {
        Style::default().fg(p.text)
    } else {
        Style::default().fg(p.overlay0)
    };
    let grip_symbol = if active || detent { "┃" } else { "│" };
    let bar_symbol = if detent { "┃" } else { "│" };

    let grip = sidebar_divider_grip_rows(area);
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        let is_grip = grip.contains(&y);
        buf[(sep_x, y)].set_symbol(if is_grip { grip_symbol } else { bar_symbol });
        buf[(sep_x, y)].set_style(if is_grip { grip_style } else { bar_style });
    }
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.sidebar_palette;
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(p.sidebar_bg));
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
        | ResolvedTokenKind::QuotaSession(text)
        | ResolvedTokenKind::QuotaWeekly(text)
        | ResolvedTokenKind::Streak { text, .. }
        | ResolvedTokenKind::BodyRegister(text)
        | ResolvedTokenKind::OrbitRegister(text)
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

    collapse_to_max_lines(folded, max_lines)
}

/// Merge a row's tail into the line above it until the row fits the height the
/// layout reserved for it.
///
/// Independent of whether the row folds by width, because the reason is
/// different: a row that overflowed its reservation used to lose its tail
/// outright, and an elided tail says more than a missing one.
fn collapse_to_max_lines(
    mut lines: Vec<Vec<ResolvedToken>>,
    max_lines: Option<u16>,
) -> Vec<Vec<ResolvedToken>> {
    if let Some(max) = max_lines.map(usize::from).filter(|max| *max > 0) {
        while lines.len() > max {
            let Some(tail) = lines.pop() else { break };
            let Some(previous) = lines.last_mut() else {
                lines.push(tail);
                break;
            };
            previous.extend(tail);
        }
    }

    lines
}

/// Lay a row's configured token lines out for the shell it is drawn in.
///
/// The bare line folds. The card does not, and must not: its content rows *are*
/// the card — the chip and the title on one, the subtitle and the status pill
/// on the next — so merging them would spend six columns of frame to produce a
/// bordered line, which is the one thing a card must not be. A bordered single
/// line of text is still a line of text.
///
/// `max_lines` applies either way: it is the height the layout reserved, and
/// the renderer is not free to draw past it.
fn shell_row_lines(
    lines: Vec<Vec<ResolvedToken>>,
    content_width: usize,
    max_lines: Option<u16>,
    shell: RowShell,
) -> Vec<Vec<ResolvedToken>> {
    match shell {
        RowShell::Line => fold_token_lines(lines, content_width, max_lines),
        RowShell::Card => collapse_to_max_lines(lines, max_lines),
    }
}

/// The rows a tree entry reserves: its content lines plus its shell's chrome.
///
/// The layout and the renderer both go through here, because a row whose
/// reserved height and drawn lines disagree puts every card below it on the
/// wrong row — the vertical twin of the failure [`tree_prefix_width`] guards
/// against.
fn shell_row_height(content_lines: usize, shell: RowShell) -> u16 {
    content_lines
        .max(1)
        .saturating_add(usize::from(shell.chrome_rows()))
        .min(u16::MAX as usize) as u16
}

/// The colour one flame band draws in.
///
/// Five distinct palette entries rather than one colour at five brightnesses,
/// for the same reason the band word is in the text: this readout is drawn as a
/// token span, which resolves against a palette and not against the colour
/// underneath it, so a brightness ramp here would be measured against a ground
/// it cannot see. The run is a temperature — a cold blue below zero, an ember
/// that is barely there, then yellow, orange and red as the streak climbs — and
/// red at the top is the reward this readout exists to give rather than the
/// alarm the same colour means elsewhere in the panel, which is why the word
/// `hot` is beside it.
fn streak_band_color(band: crate::quality_streak::FlameBand, p: &Palette) -> ratatui::style::Color {
    use crate::quality_streak::FlameBand;
    match band {
        FlameBand::Cold => p.blue,
        FlameBand::Ember => p.subtext0,
        FlameBand::Low => p.yellow,
        FlameBand::Steady => p.peach,
        FlameBand::Hot => p.red,
    }
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
            // The two quota windows draw in two different hues rather than
            // sharing `custom_style` on purpose: they are already two rows
            // and two label words apart, and a colour split is the third,
            // cheapest axis the readability requirement asks for - a reader
            // who cannot separate the words still separates the colour.
            ResolvedTokenKind::QuotaSession(text) => {
                push_token_span(
                    &mut spans,
                    truncate_end(text, budgets[index]),
                    Style::default().fg(p.blue),
                    token.style,
                    anim,
                );
            }
            ResolvedTokenKind::QuotaWeekly(text) => {
                push_token_span(
                    &mut spans,
                    truncate_end(text, budgets[index]),
                    Style::default().fg(p.mauve),
                    token.style,
                    anim,
                );
            }
            // Both register lines are drawn as plain caption text in the
            // panel's own dim ink, and deliberately so: the reference's tree
            // column is **one hue and everything else is brightness**, and a
            // line saying what a body is has no business introducing a second
            // colour to say it. See `body_register`.
            ResolvedTokenKind::BodyRegister(text) | ResolvedTokenKind::OrbitRegister(text) => {
                push_token_span(
                    &mut spans,
                    truncate_end(text, budgets[index]),
                    Style::default().fg(p.overlay1),
                    token.style,
                    anim,
                );
            }
            // The flame heats up through the palette's own warm run rather
            // than through the `anim::cell` intensity ramp the defect marker
            // uses: a token span is resolved with a palette and no knowledge
            // of the colour under it, and an intensity channel measured
            // against the wrong ground is worse than a plain colour. The band
            // *word* is in the text either way, which is what keeps the
            // readout legible on a monochrome theme and in a text pane read.
            ResolvedTokenKind::Streak { band, text } => {
                push_token_span(
                    &mut spans,
                    truncate_end(text, budgets[index]),
                    Style::default().fg(streak_band_color(*band, p)),
                    token.style,
                    anim,
                );
            }
        }
    }
    spans
}

/// Draw one owned agent pane as a row of the Spaces tree.
///
/// It uses the same connector maths as every other row and its own
/// `[ui.sidebar.agents]` token layout, so a worker reads as a branch of its
/// mate rather than as a visitor from somewhere else.
// The panel's two vertical bounds and its fold width are all the row's caller
// already knows and the row must not re-derive: measuring them twice is how a
// row's reserved height and its drawn lines come to disagree.
#[allow(clippy::too_many_arguments)]
fn render_agent_row(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    entries: &[WorkspaceListEntry],
    agents: &[AgentPanelEntry],
    list_top: u16,
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

    // A worker drawn inside its own mate's box is not a card and takes none of
    // what follows: no frame of its own, no glow, and no connector — the box it
    // is standing in *is* the relation, and a `├─` beside it would be a second,
    // weaker statement of what the border already says, drawn in the gutter of a
    // card it is not outside of.
    if let Some(crew) = crew_row_layout(app, agents, entries, card.entry_idx, fold_width) {
        // Except under a pixel card, which sets these rows in its own type —
        // see [`image_card::crew`]. Anything drawn here would be under it.
        if !crew_is_drawn(app, fold_width) {
            render_crew_row(app, frame, card, detail, &crew, list_top, list_bottom);
        }
        return;
    }

    let p = &app.sidebar_palette;
    let shell = RowShell::for_fold_width(fold_width);
    let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
    let label_color = state_label_color(detail.state, detail.seen, p);
    let card_shell = card
        .card_frame
        .and_then(|rect| Card::new(rect, label_color, is_active, p, &app.host_terminal_theme));
    // A transparent pixel card carries this row's ink itself, and anything drawn
    // under it would show through it rather than be covered by it. The shell is
    // still *constructed* — the row's content width, its rails and its prefix are
    // measured off it — it is just not drawn.
    let covered = card_shell.is_some() && image_card::shape_covers_row(app, fold_width);
    let motion = row_motion_cells(card, covered);
    let content_rows = card
        .rect
        .height
        .saturating_sub(if card_shell.is_some() {
            shell.chrome_rows()
        } else {
            0
        })
        .max(1);
    let rows = shell_row_lines(
        resolved_agent_rows(app, detail),
        list_entry_content_width(app, agents, entry, fold_width),
        Some(content_rows),
        shell,
    );
    // The card carries the row's background itself, as a glow: a flat highlight
    // painted over it would put out the light the card is lit by.
    let row_style = if is_active && card_shell.is_none() {
        Style::default().bg(p.surface_dim)
    } else {
        Style::default()
    };
    // A card's first content row is its title, and a title is the one thing on
    // the card that is not competing for attention with anything: the frame,
    // the chip and the glow already carry state, so the name gets full weight
    // whether or not this is the row the cursor is on. On the bare line it
    // still earns that weight by being the active pane.
    let name_style = if is_active || card_shell.is_some() {
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
    let mark = state_icon(detail.state, detail.seen, app.status_indicators, p);
    // On a card the mark is set in a chip. It is the same mark, padded and
    // plated — the alphabet is untouched, and a row with no `state_icon` token
    // configured gets no chip, exactly as it gets no mark today.
    let chip = card_shell.as_ref().map(|shell| shell.chip(mark.0));
    let state_icon = match &chip {
        Some((text, style)) => (text.as_str(), *style),
        None => mark,
    };
    let pill = card_shell
        .as_ref()
        .and_then(|shell| card_pill(shell, agent_status_label(detail), rows.len(), content_rows));
    let pill_reservation = pill
        .as_ref()
        .zip(card_shell.as_ref())
        .map(|(pill, shell)| usize::from(shell.pill_reservation(&pill.label)))
        .unwrap_or(0);
    let summary_badge = worker_summary_badge(app, entries, agents, card);
    let cmd_ack_row = crate::anim::CardRow::Agent(detail.pane_id);
    let cmd_ack_instances: Vec<u64> = app.sidebar_cmd_acks.live(&cmd_ack_row).collect();
    let cmd_ack_reserved = summary_badge
        .as_ref()
        .map(|(_, count)| worker_summary_badge_rect(card, *count).width)
        .unwrap_or(0);
    let cmd_ack_width =
        cmd_ack_strip_width(cmd_ack_instances.len(), card.rect.width, cmd_ack_reserved);
    // This row's own life, not its Space's: a worker arrives when it starts and
    // leaves when it finishes, which is what makes its second mate's group grow
    // and shrink around it.
    let row_anim = RowAnimation::for_agent_row(app, detail.pane_id);
    let trunk = TrunkRailPaint::new(
        app,
        Some(crate::anim::CardRow::Agent(detail.pane_id)),
        Style::default().fg(p.overlay0),
    );
    // A worker's own row is a pane, which is what makes it a carrier at all —
    // see `AppState::pane_relation_signal_phase`. Severity comes from the
    // pane's own tokens, the same way a Space's connector reads its own,
    // so the charge and the card it runs into cannot disagree about how bad
    // this row's trouble is.
    let signal_phase = app.pane_relation_signal_phase(detail.ws_idx, detail.pane_id);
    let row_severity = crate::app::lifecycle::severity(
        detail
            .tokens
            .get(crate::app::lifecycle::SEVERITY_TOKEN)
            .map(String::as_str),
    );
    let connector_style = Style::default().fg(p.overlay0);
    let top_charge = ConnectorCharge::new(app, connector_style, signal_phase, row_severity);

    if let Some(shell) = &card_shell {
        let (mut connector, _) = agent_row_prefix(
            entry.depth(),
            entry.is_last_child(),
            entry.ancestors_continue(),
            0,
            p,
            top_charge.as_ref(),
            true,
            trunk.as_ref(),
            fold_width,
        );
        let (mut above, _) = card_rail_prefix(
            entry.depth(),
            entry.is_last_child(),
            entry.ancestors_continue(),
            CardRailSegment::AboveConnector,
            p,
            trunk.as_ref(),
            fold_width,
        );
        let (mut below, _) = card_rail_prefix(
            entry.depth(),
            entry.is_last_child(),
            entry.ancestors_continue(),
            CardRailSegment::BelowConnector,
            p,
            trunk.as_ref(),
            fold_width,
        );
        // The rails are measured to the row's level; the card is placed by the
        // row's rank. Fit one to the other — the branch carries on across a gap
        // right-alignment opened, and gives columns back where the card starts
        // ahead of them. The rails above and below the connector are blank
        // either way, because nothing is travelling there.
        let rank = entry.rank();
        fit_rails_to_card(&mut connector, entry.depth(), rank, fold_width, true, p);
        fit_rails_to_card(&mut above, entry.depth(), rank, fold_width, false, p);
        fit_rails_to_card(&mut below, entry.depth(), rank, fold_width, false, p);
        if entry.depth() > 0 {
            connector.push(connector_joint_span(p));
        }
        animate_row_spans(&mut connector, &row_anim);
        animate_row_spans(&mut above, &row_anim);
        animate_row_spans(&mut below, &row_anim);
        render_card_border_rails(
            frame,
            card,
            connector,
            above,
            below,
            row_opens_a_branch(entries, card.entry_idx).then(|| branch_rail_span(p)),
            list_entry_gap(app, entries, card.entry_idx, fold_width),
            list_top,
            list_bottom,
            motion,
            image_card::row_arrival(app, card),
        );
        if !covered {
            shell.render_glow(frame, list_bottom);
        }
    }

    let last_content_row = rows.len().saturating_sub(1);
    // Which of the card's content rows the branch line lands on, counted from
    // the first. Zero for a character card and for every drawn card whose row
    // has a middle cell to be the middle of; one further down when the card
    // needed five cells. Resolved once, because the loop below and
    // `render_card_border_rails` above both draw into the same prefix columns
    // and a disagreement between them is two branch lines on one card.
    let connector_row = card.connector_y().saturating_sub(card.content_y());
    for (row_index, resolved) in rows.iter().enumerate() {
        let row_y = card.content_y() + row_index as u16;
        if row_index as u16 >= content_rows || row_y >= list_bottom {
            break;
        }
        // A row still crossing the panel draws nothing in characters: its own
        // rail would point at a card that has not arrived, and under a shape
        // this line carries only that rail anyway.
        if motion.0 != 0 {
            continue;
        }
        let Some(row_y) = moved_row(row_y, motion.1, list_top, list_bottom) else {
            continue;
        };
        // The branch line only exists on the card's first content row, so
        // that is the only row a signal can travel and the only row it
        // damages.
        let row_signal_phase = (row_index as u16 == connector_row)
            .then_some(signal_phase)
            .flatten();
        let row_charge = ConnectorCharge::new(app, connector_style, row_signal_phase, row_severity);
        // Only the first content row carries the badge, so only it gives up the
        // width; only the last carries the pill.
        let trailing_width = if row_index == 0 {
            summary_badge
                .as_ref()
                .map(|(_, count)| usize::from(worker_summary_badge_rect(card, *count).width))
                .unwrap_or(0)
                + usize::from(cmd_ack_width)
        } else {
            0
        } + if row_index == last_content_row {
            pill_reservation
        } else {
            0
        };
        let (mut spans, token_budget) = match &card_shell {
            Some(shell) => {
                // A content row draws its own prefix over the rail laid down
                // before it, so this is where the connector actually lands —
                // on `connector_row`, which is the card's name row unless the
                // drawn card's own middle falls further down.
                let (mut spans, _) = if row_index as u16 == connector_row {
                    agent_row_prefix(
                        entry.depth(),
                        entry.is_last_child(),
                        entry.ancestors_continue(),
                        0,
                        p,
                        row_charge.as_ref(),
                        true,
                        trunk.as_ref(),
                        fold_width,
                    )
                } else {
                    card_rail_prefix(
                        entry.depth(),
                        entry.is_last_child(),
                        entry.ancestors_continue(),
                        if (row_index as u16) < connector_row {
                            CardRailSegment::AboveConnector
                        } else {
                            CardRailSegment::BelowConnector
                        },
                        p,
                        trunk.as_ref(),
                        fold_width,
                    )
                };
                // The columns between where the rails stop and where the card
                // begins, before the border's own column below.
                let on_connector_row = row_index as u16 == connector_row;
                fit_rails_to_card(
                    &mut spans,
                    entry.depth(),
                    entry.rank(),
                    fold_width,
                    on_connector_row,
                    p,
                );
                // The frame's own column and the pad inside it. Blank, so the
                // border can be laid over the first of them once the row has
                // had its say — except on the connector row of a nested card,
                // where that column is where the branch meets the border.
                if on_connector_row && entry.depth() > 0 {
                    spans.push(connector_joint_span(p));
                } else {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::raw(" "));
                (spans, usize::from(shell.content_width()))
            }
            None => {
                let (mut spans, prefix_width) = agent_row_prefix(
                    entry.depth(),
                    entry.is_last_child(),
                    entry.ancestors_continue(),
                    row_index,
                    p,
                    row_charge.as_ref(),
                    false,
                    trunk.as_ref(),
                    fold_width,
                );
                // A bare row is right-aligned by the same offset its card would
                // have been, so a panel that folds mid-fleet does not restripe
                // the tree's left edges as it crosses the threshold.
                let gap_width = usize::from(rank_width_inset(entry.rank(), fold_width));
                fit_rails_to_card(
                    &mut spans,
                    entry.depth(),
                    entry.rank(),
                    fold_width,
                    false,
                    p,
                );
                (
                    spans,
                    (card.rect.width as usize)
                        .saturating_sub(prefix_width)
                        .saturating_sub(gap_width),
                )
            }
        };
        // On a card the whole first content row is the title, however the row's
        // tokens are configured: a worker's identity lands in the secondary
        // slot (`Agent`) as often as in the primary one, and a title that
        // changed weight depending on which token spelled it would read as two
        // different rows.
        let secondary_style = if card_shell.is_some() && row_index == 0 {
            name_style
        } else {
            agent_style
        };
        animate_row_spans(&mut spans, &row_anim);
        // Under a shape the prefix still draws — it is the tree's connector,
        // outside the card — but the row's own tokens do not: the pixel card has
        // already set this title, this chip and this tidbit in its own type.
        if !covered {
            spans.extend(resolved_token_spans(
                resolved,
                (
                    state_icon.0,
                    arrived_state_icon_style(state_icon.1, row_charge.as_ref(), p),
                ),
                status_style,
                name_style,
                secondary_style,
                secondary_style,
                p,
                &row_anim,
                token_budget.saturating_sub(trailing_width),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );
    }

    if let Some(shell) = &card_shell {
        if !covered {
            shell.render_frame(frame, list_bottom, pill.as_ref());
        }
    }

    if let Some((owner, count)) = &summary_badge {
        // Not suppressed under a shape any more — *relocated*. The pixel card
        // draws its own badge on its own right rail, in its own type, at the
        // cell this one's click target still names. See
        // `image_card::ControlRail`.
        if !covered {
            render_worker_summary_badge(app, frame, card, agents, owner, *count, list_bottom);
        }
    }

    if !cmd_ack_instances.is_empty() && !covered {
        render_cmd_acks(
            app,
            frame,
            card,
            &cmd_ack_row,
            &cmd_ack_instances,
            cmd_ack_reserved,
            cmd_ack_width,
            list_bottom,
        );
    }
}

/// Draw a worker inside the box its own mate already opened.
///
/// The character twin of [`image_card::crew`], and the same design read at the
/// scale a cell grid has: the mate's card carries its own block, then a dashed
/// rule ([`Card::render_crew_rule`]), then one compact row per worker. What a
/// row keeps is its mark, its name and its dim status line; what it gives up is
/// everything that said "separate card" — the border, the glow, the pill and the
/// connector.
///
/// The two tiers are the list's own, saturated at one step: a worker this mate
/// dispatched sits flush with the card's text column at full strength, and one
/// that reached it through another worker is stepped in by
/// [`CREW_INDENT_COLS`] and drawn dim. One step whichever chain it came down,
/// because the question the step answers is "did this come through somebody"
/// and not "through whom".
fn render_crew_row(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    detail: &AgentPanelEntry,
    crew: &CrewRowLayout,
    list_top: u16,
    list_bottom: u16,
) {
    if crew.text_width == 0 {
        return;
    }
    let text_x = card.rect.x.saturating_add(crew.text_offset);

    let p = &app.sidebar_palette;
    // A row that came through somebody is dimmed whole — the mark, the name and
    // the status line together — for the reason the pixel list dims its whole
    // row: a tier is one signal, and every mark on the row says the same thing.
    let dim = |style: Style| {
        if crew.via_mate {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    };
    let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
    let name_style = dim(Style::default()
        .fg(if is_active { p.text } else { p.subtext0 })
        .add_modifier(Modifier::BOLD));
    let detail_style = dim(Style::default().fg(p.overlay0).add_modifier(Modifier::DIM));
    let status_style = dim(Style::default().fg(state_label_color(detail.state, detail.seen, p)));
    // The row's own mark, undecorated. A chip is a *card's* plate and this row
    // has no card; the pixel list makes the same call, giving a worker a dot
    // where a card gets its plated mark.
    let mark = state_icon(detail.state, detail.seen, app.status_indicators, p);
    let row_anim = RowAnimation::for_agent_row(app, detail.pane_id);

    // A worker's command flashes follow it inside the box, for the reason its
    // failure marker does: a signal that stops being drawn the moment the design
    // changed is a regression nobody sees until they need it. They are still
    // drawn *over* the row's own last columns, so the first line gives them up
    // out of its token budget exactly as a card's row gives up the badge's.
    let cmd_ack_row = crate::anim::CardRow::Agent(detail.pane_id);
    let cmd_ack_instances: Vec<u64> = app.sidebar_cmd_acks.live(&cmd_ack_row).collect();
    let cmd_ack_width = cmd_ack_strip_width(cmd_ack_instances.len(), crew.text_width, 0);

    for (index, resolved) in crew.lines.iter().enumerate() {
        let y = card.rect.y.saturating_add(index as u16);
        if y >= card.rect.y.saturating_add(card.rect.height) || y >= list_bottom || y < list_top {
            break;
        }
        let trailing = if index == 0 { cmd_ack_width } else { 0 };
        let mut spans = resolved_token_spans(
            resolved,
            (mark.0, dim(mark.1)),
            status_style,
            name_style,
            detail_style,
            detail_style,
            p,
            &row_anim,
            usize::from(crew.text_width.saturating_sub(trailing)),
        );
        animate_row_spans(&mut spans, &row_anim);
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(text_x, y, crew.text_width, 1),
        );
    }

    if cmd_ack_width > 0 && card.rect.y < list_bottom && card.rect.y >= list_top {
        render_cmd_ack_strip(
            app,
            frame,
            &cmd_ack_row,
            &cmd_ack_instances,
            Rect::new(
                text_x + crew.text_width - cmd_ack_width,
                card.rect.y,
                cmd_ack_width,
                1,
            ),
        );
    }
}

/// The status readout a card repeats at its foot, or `None` when the card has
/// no room to say it twice.
///
/// A one-content-row card already spends that row on the name and whatever
/// controls are drawn over it; a pill there would be competing with the row's
/// own title for the same columns, and the chip on that row is already saying
/// the same thing in one cell.
fn card_pill(shell: &Card<'_>, label: &str, lines: usize, content_rows: u16) -> Option<Pill> {
    if lines < 2 || content_rows < 2 {
        return None;
    }
    let pill = Pill {
        label: label.to_string(),
    };
    (shell.pill_reservation(&pill.label) > 0).then_some(pill)
}

/// The state label this pane reports, publisher override included.
fn agent_status_label(entry: &AgentPanelEntry) -> &str {
    entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| state_label(entry.state, entry.seen))
}

/// Where a row is drawn relative to where the layout put it, in whole cells.
///
/// `(0, 0)` unless a pixel card was actually placed for this row on this pass:
/// `motion_cells` is the offset the *placement* took, so applying it to the
/// characters of a row whose card is not on screen would move a line away from
/// the thing it belongs to rather than with it.
fn row_motion_cells(card: &crate::app::state::WorkspaceCardArea, covered: bool) -> (i32, i32) {
    if covered {
        card.motion_cells
    } else {
        (0, 0)
    }
}

/// One of a row's rows, moved by the row's own motion offset, or `None` when
/// that puts it outside the list.
fn moved_row(y: u16, dy: i32, list_top: u16, list_bottom: u16) -> Option<u16> {
    let moved = i32::from(y).checked_add(dy)?;
    let moved = u16::try_from(moved).ok()?;
    (moved >= list_top && moved < list_bottom).then_some(moved)
}

/// Draw the tree rails beside a card's border rows.
///
/// A card's content rows carry their own rails in the line they render; its two
/// border rows have no line of their own, so the rails beside them are drawn
/// here. Without this the tree's vertical rules would break every time they
/// passed a card — which, at four rows an entity, is most of the panel.
///
/// `branch_rail` is the rail this card's *own children* hang off, and it is the
/// one rail that does not fit beside the card: a child's connector stands in the
/// column its parent's left border stands in — see
/// [`every_branch_starts_in_the_column_its_parents_border_stands_in`] — so the
/// line leaving a parent has to be drawn *in* that border column rather than to
/// the left of it. `Some` on a row something hangs off, and then only the rows
/// below the connector carry it: above the connector that column is the card's
/// own top corner, and the branch has not left yet.
///
/// `motion` is [`row_motion_cells`]. The rail travels with the card because the
/// two are one thing seen by two renderers: a connector left at the layout's
/// row while the card it points at is four rows higher is an arrow at empty
/// space. Both are whole-cell arithmetic over the same published offset, so
/// they cannot land a row apart. A row still travelling *sideways* draws no
/// rail at all — its card has not reached the panel yet.
// Three pre-rendered span runs, the gap under the card and the three bounds the
// rail is drawn between: every one of them is a separate fact about this one
// row, and grouping them into a struct would only move the list.
#[allow(clippy::too_many_arguments)]
fn render_card_border_rails(
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    connector: Vec<Span<'static>>,
    above: Vec<Span<'static>>,
    below: Vec<Span<'static>>,
    branch_rail: Option<Span<'static>>,
    trailing_gap: u16,
    list_top: u16,
    list_bottom: u16,
    motion: (i32, i32),
    circuit: motion::ArrivalCircuit,
) {
    let Some(shell_frame) = card.card_frame else {
        return;
    };
    let width = shell_frame.x.saturating_sub(card.rect.x);
    if width == 0 || shell_frame.height == 0 || motion.0 != 0 {
        return;
    }
    // One column wider than the rails beside the card, because this rail stands
    // *in* the card's own border column. Harmless under the character shell and
    // under the sheet, where the card's border and the sheet's backdrop are both
    // drawn over it afterwards; it is the shape shell — which draws no character
    // border at all — that would otherwise leave the line missing.
    // The rail that leaves this card downward, drawn **only below the card's own
    // bottom edge**.
    //
    // A18/A32: *a thin vertical rail drops from the parent pane's bottom edge —
    // never inside it, at any alpha*. It used to be pushed onto every row below
    // the connector, which put it in the card's own border column on the card's
    // own rows: a rail crossing the pane it is leaving. Below the bottom edge it
    // is in the gutter, where the tree's line belongs, and the child's own
    // prefix picks it up from the child's first row — so the line is continuous
    // with no gap even at `row_gap = 0`, and its overlap with every pane box is
    // exactly zero. See `no_rail_cell_lands_inside_a_pane_box`.
    let card_bottom = shell_frame.y.saturating_add(shell_frame.height);
    let below_width = width.saturating_add(u16::from(branch_rail.is_some()));
    let mut below_the_card = below.clone();
    if let Some(rail) = branch_rail {
        below_the_card.push(rail);
    }
    // Where the branch line meets this card. On a character card that is its
    // *name* — its first content row, not the corner of its box, because a rail
    // calibrated for one-line rows put it on the top border where it reads as an
    // arrow at a rectangle rather than as a line to a thing with a name. On a
    // drawn card it is the row the shape's own middle falls in, which is a
    // different row as soon as the card needs more than three cells. See
    // [`crate::app::state::WorkspaceCardArea::connector_y`].
    let connector_y = card.connector_y();
    // Every row from the card's top edge to the foot of the gap under it, so
    // the line is a line. Drawing only the first and last rows of a card left a
    // dash per card with the tree's own spacing showing through the breaks.
    let last_y = shell_frame
        .y
        .saturating_add(shell_frame.height)
        .saturating_add(trailing_gap);
    for y in shell_frame.y..last_y {
        let Some(drawn_y) = moved_row(y, motion.1, list_top, list_bottom) else {
            continue;
        };
        // The rail and the branch grow from their own fixed anchors — the
        // rail's top edge, the branch's own left edge at the rail — never a
        // translation of a finished line. See [`super::motion::ArrivalCircuit`].
        let (spans, cols) = if y == connector_y {
            let lit = ((connector.len() as f32) * circuit.tick).round() as usize;
            if lit == 0 {
                continue;
            }
            (connector[..lit.min(connector.len())].to_vec(), width)
        } else if y < connector_y {
            // Measured from the card's top edge down to the elbow, so the
            // rail's growth rate does not depend on how tall the card is.
            // Zero at `rail == 0.0`: the segment's fixed anchor is its own
            // top, and nothing below it is lit until growth reaches there.
            let run = f32::from(connector_y.saturating_sub(shell_frame.y));
            let grown = run * circuit.rail;
            let offset = f32::from(y.saturating_sub(shell_frame.y));
            if offset >= grown {
                continue;
            }
            (above.clone(), width)
        } else if circuit.card <= 0.0 {
            // Nothing below the elbow until the branch exists and the card
            // has begun to bloom: what runs below a card is the rail leaving
            // it toward its own children, and a card that does not exist yet
            // has none. Drawing it would be a line hanging under nothing.
            continue;
        } else if y >= card_bottom {
            (below_the_card.clone(), below_width)
        } else {
            (below.clone(), width)
        };
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(card.rect.x, drawn_y, cols, 1),
        );
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
        Style::default().fg(app.sidebar_palette.overlay0)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(worker_summary_badge_label(count), style))
            .alignment(Alignment::Right),
        rect,
    );
}

/// Max simultaneous command-ack glyphs one card's row draws at once.
///
/// The animation engine tracks every instance independently regardless of
/// this cap — see [`crate::app::cmd_ack::CmdAcks`]'s own header on why the
/// captain's call was one instance per detected command rather than a
/// coalesced counter. This is purely the scoping report's "soft legibility
/// damping" for a card busy enough that drawing every one of them would be
/// noise rather than signal: the newest instances win the row's limited
/// width, and the ones that lose the draw are still animating, just off
/// screen.
const CMD_ACK_MAX_VISIBLE: usize = 6;

/// The glyph drawn per acknowledged command.
///
/// Deliberately not the command text or its output — the scoping report's
/// §2/§4, unchanged by the captain's multiplicity answer — a short marker is
/// the whole content, and the output already lives in the pane's own
/// scrollback.
const CMD_ACK_GLYPH: char = '●';

/// Columns a strip of `instance_count` command-ack glyphs takes on a row
/// `row_width` wide with `reserved` columns already spoken for (the worker
/// summary badge, if the card has one), or `0` when there is nothing to draw
/// or no room left to draw it in.
fn cmd_ack_strip_width(instance_count: usize, row_width: u16, reserved: u16) -> u16 {
    let glyphs = instance_count.min(CMD_ACK_MAX_VISIBLE) as u16;
    if glyphs == 0 {
        return 0;
    }
    // Needs the glyphs themselves and something left over for the row's own
    // name and its chevron; below that the row is better off with just a
    // name, the same trade `worker_summary_badge_width` makes.
    let budget = row_width.saturating_sub(reserved + 2);
    glyphs.min(budget)
}

/// Where a command-ack strip of `width` columns sits, immediately left of
/// whatever else (the worker summary badge) has already reserved `reserved`
/// columns from the card's right edge.
fn cmd_ack_strip_rect(
    card: &crate::app::state::WorkspaceCardArea,
    reserved: u16,
    width: u16,
) -> Rect {
    if card.rect.height == 0 || width == 0 {
        return Rect::default();
    }
    Rect::new(
        card.control_right().saturating_sub(1 + reserved + width),
        card.content_y(),
        width,
        1,
    )
}

/// Draw up to [`CMD_ACK_MAX_VISIBLE`] command-acknowledgement markers at the
/// right edge of a card's first row.
///
/// Each glyph is its own animation element with its own settle clock — see
/// [`crate::app::cmd_ack::CmdAcks`] — so a burst of commands reads as several
/// independently ticking markers rather than one that restarted.
fn render_cmd_acks(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    row: &crate::anim::CardRow,
    instances: &[u64],
    reserved: u16,
    width: u16,
    list_bottom: u16,
) {
    let rect = cmd_ack_strip_rect(card, reserved, width);
    if rect.width == 0 || rect.y >= list_bottom {
        return;
    }
    render_cmd_ack_strip(app, frame, row, instances, rect);
}

/// The markers themselves, in a strip the caller has already placed.
///
/// Split out because a worker drawn inside its mate's box has no card of its
/// own to measure a strip against — its columns are that box's, resolved by
/// [`crew_row_layout`] — while the glyphs, their clocks and their ink are the
/// same on either row.
fn render_cmd_ack_strip(
    app: &AppState,
    frame: &mut Frame,
    row: &crate::anim::CardRow,
    instances: &[u64],
    rect: Rect,
) {
    let base = Style::default().fg(app.palette.accent);
    // The newest instances are the ones still worth a reader's attention when
    // the row cannot show every one of them.
    let visible = &instances[instances.len().saturating_sub(usize::from(rect.width))..];
    let mut spans = Vec::with_capacity(visible.len());
    for &seq in visible {
        let id = crate::anim::ElementId::CmdAck(crate::anim::CmdAck {
            row: row.clone(),
            seq,
        });
        push_animated_span(
            &mut spans,
            CMD_ACK_GLYPH.to_string(),
            base,
            app.anim.frame(&id, None),
            backdrop_rgb(app),
            &app.palette,
            &app.host_terminal_theme,
        );
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Indent and connector for one row's first column run, in either panel.
///
/// Deliberately the same vocabulary as the Spaces panel's child cards —
/// `├─ `/`└─ ` on the first row, a `│` continuation while a level still has
/// siblings below — because the user sees both panels at once and a second
/// branch glyph would read as a second meaning. Returns the spans and the
/// columns they consume, which the caller subtracts from the token budget.
///
/// Continuation rows sit two columns further right than their first row, which
/// is what keeps wrapped text aligned under the name rather than under the
/// state dot.
///
/// `meets_a_card` says whether a card's border stands in the column this prefix
/// ends at, which decides the connector's joint — see [`push_connector_spans`].
/// It changes no width: the prefix is [`tree_prefix_width`] either way, so the
/// layout does not have to know which shell the renderer chose.
fn agent_row_prefix(
    depth: u8,
    is_last_child: bool,
    ancestors_continue: &[bool],
    row_index: usize,
    p: &Palette,
    charge: Option<&ConnectorCharge<'_>>,
    meets_a_card: bool,
    trunk: Option<&TrunkRailPaint<'_>>,
    fold_width: u16,
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

    // Every level's run is widened by one rung of the ladder, which is what
    // walks the whole tree — wires and cards together — to the right.
    let step = usize::from(rank_step(fold_width));
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
            let (glyph, style) = trunk_rail_cell(trunk, level, line_style);
            spans.push(Span::styled(glyph.to_string(), style));
            spans.push(Span::raw(" ".repeat(2 + step)));
        } else {
            spans.push(Span::raw(" ".repeat(3 + step)));
        }
    }

    if row_index == 0 {
        push_connector_spans(
            &mut spans,
            is_last_child,
            charge,
            line_style,
            meets_a_card,
            step,
        );
        (spans, tree_column(depth, fold_width) as usize)
    } else {
        if is_last_child {
            spans.push(Span::raw(" ".repeat(3 + step)));
        } else {
            spans.push(Span::styled("│", line_style));
            spans.push(Span::raw(" ".repeat(2 + step)));
        }
        spans.push(Span::raw("  "));
        (spans, tree_prefix_columns(depth, 1, fold_width))
    }
}

/// Where one row of a card sits relative to the row its connector points at.
///
/// A one-line row has one rail to draw and no choice to make. A card is four or
/// more rows tall, and its own level's rail means different things above and
/// below the connector: above, the line is still travelling down from the
/// parent and always continues; below, it continues only if this card has a
/// sibling under it. Drawing the same rail on every row of a card is what made
/// a last child's line stop at the top of the card and a middle child's line
/// run past the bottom of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardRailSegment {
    /// Rows between the previous row's rail and this card's connector.
    AboveConnector,
    /// Rows after the connector: the card's remaining rows, and the gap under
    /// it up to the next sibling's first row.
    BelowConnector,
}

/// The rails a card's row carries, cut back to the column the card's own left
/// border stands in.
///
/// A bare row's continuation lines sit two columns further right than its first
/// line, so wrapped text aligns under the name rather than under the state
/// mark. A card does not indent: its left border stands in the connector's own
/// column on every row, and the frame — not whitespace — is what holds the
/// content in. So every row of a card, border row and content row alike, is
/// preceded by exactly `tree_prefix_width(depth, 0)` columns, which is what
/// lets the layout keep measuring one prefix per row.
fn card_rail_prefix(
    depth: u8,
    is_last_child: bool,
    ancestors_continue: &[bool],
    segment: CardRailSegment,
    p: &Palette,
    trunk: Option<&TrunkRailPaint<'_>>,
    fold_width: u16,
) -> (Vec<Span<'static>>, usize) {
    let step = usize::from(rank_step(fold_width));
    let line_style = Style::default().fg(p.overlay0);
    // Above the connector the rail is the parent's, and a parent's line reaches
    // its last child exactly as it reaches the others — `is_last_child` only
    // decides whether anything continues *past* the connector.
    let own_level_continues = match segment {
        CardRailSegment::AboveConnector => true,
        CardRailSegment::BelowConnector => !is_last_child,
    };
    let branch = |spans: &mut Vec<Span<'static>>| {
        if own_level_continues {
            spans.push(Span::styled("│", line_style));
            spans.push(Span::raw(" ".repeat(2 + step)));
        } else {
            spans.push(Span::raw(" ".repeat(3 + step)));
        }
    };

    let depth = crate::app::agent_tree::display_depth(depth);
    if depth == 0 {
        return (vec![Span::raw(" ")], 1);
    }

    let mut spans = vec![Span::raw(" ")];
    for level in 1..depth {
        if ancestors_continue
            .get(level as usize)
            .copied()
            .unwrap_or(false)
        {
            let (glyph, style) = trunk_rail_cell(trunk, level, line_style);
            spans.push(Span::styled(glyph.to_string(), style));
            spans.push(Span::raw(" ".repeat(2 + step)));
        } else {
            spans.push(Span::raw(" ".repeat(3 + step)));
        }
    }
    branch(&mut spans);
    (spans, tree_column(depth, fold_width) as usize)
}

/// The card frame's own column, on the row the connector lands on.
///
/// A card's prefix is followed by two columns before its first token: the
/// column its left border stands in, and the pad inside it. On the connector
/// row the border's column is not blank — it is the last stretch of the branch,
/// running into the border it points at.
///
/// Under the character shell this is invisible: the frame is drawn over it, and
/// the two glyphs already met, because a `─` and a `│` are both centred in
/// their cells. It exists for the pixel card, whose border is a stroke standing
/// at the column's centre rather than at its left edge — see
/// [`super::sidebar::image_card::RAIL_INK_COLUMN_FRACTION`] for why it stands
/// there. Without this the branch would stop half a column short of the card,
/// which is the residue of the gap the joint in [`push_connector_spans`]
/// closes. The pixel card is drawn over the text, so its own body hides the
/// half of this glyph that falls inside it and the line reads as ending exactly
/// at the border.
fn connector_joint_span(p: &Palette) -> Span<'static> {
    Span::styled("─", Style::default().fg(p.overlay0))
}

/// The rail leaving a card downwards, in the card's own border column.
///
/// The vertical twin of [`connector_joint_span`], and it exists for the same
/// reason: a child's connector stands in its parent's border column, so the
/// stretch of that line which crosses the parent's own rows has nowhere else to
/// be drawn. Under the character shell the card's border is a `│` in that column
/// already and this changes nothing; under a drawn card the border is a shape
/// that stops short of the row's last pixel row, and without this the line
/// leaving the first mate starts a gutter below the card it leaves — the
/// captain's *"the trunk does not touch firstmate"*.
fn branch_rail_span(p: &Palette) -> Span<'static> {
    Span::styled("│", Style::default().fg(p.overlay0))
}

/// The `├──` / `└──` connector of a child row's first line, charge and all.
///
/// One function for both kinds of row so a charge cannot run one shape for a
/// Space and another for a pane; the sidebar already insists those be the same
/// three glyphs, and this is what keeps them the same three glyphs while
/// something is travelling them.
///
/// # Why the third cell depends on what follows
///
/// It used to be a space unconditionally, which left the connector stopping one
/// whole column short of the card it points at. Under the character shell that
/// read as padding, because the card's own border was a `│` in the next column
/// and the eye joined them up. Under a pixel card it does not: the card is a
/// drawn shape with a real edge, and a connector that ends a column early is a
/// branch hanging in space beside it — one half of *"branches not aligned with
/// secondmates."* So against a card the joint carries the line all the way to
/// the border, and against a bare line it stays the space that keeps `├─` off
/// the name.
fn push_connector_spans(
    spans: &mut Vec<Span<'static>>,
    is_last_child: bool,
    charge: Option<&ConnectorCharge<'_>>,
    base: Style,
    meets_a_card: bool,
    step: usize,
) {
    // The last cell is the joint. Against a card it is the connector's own line
    // running into the border it points at; against a bare line it is the space
    // that keeps the glyphs off the name. `step` is what right alignment moved
    // the card by, and the branch simply runs the extra distance to reach it, so
    // the connector is always exactly as long as the gap it has to cross.
    let corner = if is_last_child { '└' } else { '├' };
    let mut connector: Vec<char> = std::iter::once(corner)
        .chain(std::iter::repeat_n('─', 2 + step))
        .collect();
    if !meets_a_card {
        connector.pop();
        connector.push(' ');
    }
    for (cell, settled) in connector.into_iter().enumerate() {
        let (glyph, style) = connector_cell(charge, cell as u16, settled, base);
        spans.push(Span::styled(glyph.to_string(), style));
    }
}

/// The stage each kind of relation signal is about.
///
/// The vocabulary is not written down here any more: a signal says something
/// happened to a row's *work*, so it names the stage that work moved to and the
/// hue follows from [`crate::anim::cell::LifecycleStage`] like every other
/// stage hue in the tree. That is what stops the connector and the card it runs
/// into from having two different opinions about what "finished" looks like.
fn relation_signal_stage(kind: RelationSignalKind) -> crate::anim::cell::LifecycleStage {
    use crate::anim::cell::LifecycleStage;
    match kind {
        RelationSignalKind::Transfer => LifecycleStage::Running,
        RelationSignalKind::Completed => LifecycleStage::Done,
        RelationSignalKind::Failed => LifecycleStage::Failed,
        // The quiet one, and the only one that must not compete for attention:
        // "this branch stopped" is the least urgent thing a fleet can say, and a
        // row with nothing left to do is back where it started.
        RelationSignalKind::Idle => LifecycleStage::Queued,
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
    /// `severity` is the carrier row's own, not the signal's. A charge is a
    /// report about a row, so it arrives as loud as the row it is about: a
    /// completion off a row in serious trouble is not the same event as a
    /// completion off a healthy one, and reading the severity from the row is
    /// what lets the connector and the card it runs into agree about that
    /// without either being told by the other.
    fn new(
        app: &'a AppState,
        base: Style,
        phase: Option<RelationSignalPhase>,
        severity: crate::anim::cell::Severity,
    ) -> Option<Self> {
        let phase = phase?;
        let behaviour = app
            .anim
            .catalogue()
            .get(relation_signal_behaviour(phase.kind))?;
        let hue =
            relation_signal_stage(phase.kind).hue(&app.sidebar_palette, &app.host_terminal_theme);
        Some(Self {
            behaviour,
            progress: phase.progress,
            ink: crate::anim::cell::InkPalette::resolve(
                base,
                backdrop_rgb(app),
                &app.palette,
                &app.host_terminal_theme,
            )
            .with_signal(hue, severity),
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

/// One row's trunk-rail paint, resolved once and asked about per ancestor
/// level.
///
/// The vertical-line counterpart of [`ConnectorCharge`]: where a charge
/// travels the three cells of one row's own branch, this reaches the `│`
/// cells beside it that belong to an *ancestor's* line — each ancestor column
/// is its own [`crate::anim::ElementId::TrunkSegment`], addressed by
/// [`entry_card_row`], so a level that has nothing configured for it is left
/// exactly as it always drew.
///
/// A segment is asked about at one fixed cell in a `1×1` extent rather than
/// across the several terminal rows it may visually span, which is what makes
/// it *one* object rather than a per-cell gradient: [`CellExtent::normalize`]
/// resolves a one-cell axis to `0.0`, so every behaviour reads it as settled
/// at a single point in its run and every cell of the segment agrees. A
/// segment that travels smoothly along its own length — a charge, eventually
/// the spider this unblocks — is later work built on this same identity, not
/// a widening of what this paints.
struct TrunkRailPaint<'a> {
    anim: &'a crate::anim::Animator,
    below: crate::anim::CardRow,
    ink: crate::anim::cell::InkPalette,
}

impl<'a> TrunkRailPaint<'a> {
    fn new(app: &'a AppState, below: Option<crate::anim::CardRow>, base: Style) -> Option<Self> {
        Some(Self {
            anim: &app.anim,
            below: below?,
            ink: crate::anim::cell::InkPalette::resolve(
                base,
                backdrop_rgb(app),
                &app.palette,
                &app.host_terminal_theme,
            ),
        })
    }

    /// The paint for the `│` at this ancestor level, or `None` when that
    /// segment has nothing to play — no mount/dismount configured, or the
    /// segment is not (yet) tracked by the engine — which a caller reads as
    /// "draw the settled glyph."
    fn cell(&self, level: u8) -> Option<crate::anim::cell::CellPaint> {
        let frame = self.anim.frame(
            &crate::anim::ElementId::trunk_segment(self.below.clone(), level),
            None,
        )?;
        frame.behaviour?;
        Some(frame.cell(
            crate::anim::cell::CellPos::col(0),
            crate::anim::cell::CellExtent::new(1, 1),
            self.ink,
        ))
    }
}

/// One `│` cell of an ancestor's trunk rail, charge and all.
///
/// Mirrors [`connector_cell`] for the vertical rail rather than the branch:
/// with no [`TrunkRailPaint`] — the ordinary case, nothing configured to
/// animate row arrival — this returns the settled glyph unchanged, so the
/// mechanism costs nothing when it is not asked to do anything.
fn trunk_rail_cell(trunk: Option<&TrunkRailPaint<'_>>, level: u8, base: Style) -> (char, Style) {
    const SETTLED: char = '│';
    let Some(trunk) = trunk else {
        return (SETTLED, base);
    };
    let Some(paint) = trunk.cell(level) else {
        return (SETTLED, base);
    };
    (paint.glyph_over(SETTLED), paint.text_style(base, trunk.ink))
}

/// The failure spider: a persistent, pulsing marker that climbs a card with an
/// open defect on it, up that card's own trunk/branch to rest at its
/// top-centre border, and stays there until the defect clears.
///
/// Two channels meet on it and neither may move the other's number. Its **hue**
/// is the row's [`crate::anim::cell::LifecycleStage`], so a marker rides a card
/// that is still running without repainting that card's stage; its
/// **intensity** is the severity the fleet published for the open defect
/// (`sev`, at 25/50/75/100%), held constant as the stage's hue changes
/// underneath it. Nothing at all is drawn when the fleet says `sev=-`: that is
/// the fleet stating the defect is closed, which is a thing only it can know.
/// See [`crate::quality_streak`].
///
/// Deliberately not a `glyph_over` substitution — nothing settled already
/// stands at the cell it rests on that has to keep meaning what it means —
/// but its own drawn cell, the same way [`render_agent_row`]'s worker-summary
/// badge is. And deliberately at the *border*, not the card face: the face is
/// the crowded surface the sidebar's own width rules protect, the border is
/// not.
///
/// # The character shell's half of the marker
///
/// This is one of *two* renderings of the same creature, and it draws only when
/// the other one is not going to. A pixel card covers the cells this marker
/// would stand on — the sheet is opaque over them, a shape stands the character
/// card down entirely — so on that path the spider is rasterised into the card
/// itself by the `image_card` spider module, at a size a character cell has no way to
/// offer, and this returns early. [`image_card::card_covers_row`] is the one
/// question both sides ask, so exactly one of them draws.
///
/// The two are held to the same three facts and may not re-derive any of them:
/// existence from the fleet's own defect token, hue from the row's
/// [`crate::anim::cell::LifecycleStage`], intensity from
/// [`crate::quality_streak::DefectMark::intensity`]. Everything below this line
/// is how a *cell* says that; the pixel module's own doc comment is how a card
/// says it.
fn render_failure_spiders(
    app: &AppState,
    frame: &mut Frame,
    cards: &[crate::app::state::WorkspaceCardArea],
    entries: &[WorkspaceListEntry],
    agents: &[AgentPanelEntry],
    fold_width: u16,
) {
    if image_card::card_covers_row(app, fold_width) {
        return;
    }
    for card in cards {
        // A worker drawn inside its mate's box has no border of its own to
        // climb, and its marker follows it there rather than being dropped —
        // the same call [`image_card::crew`] makes, and for the same reason: an
        // open defect that stops being drawn the moment the design changed is a
        // regression nobody sees until they need it.
        let crew = crew_row_layout(app, agents, entries, card.entry_idx, fold_width);
        if card.card_frame.is_none() && crew.is_none() {
            continue;
        }
        let Some(entry) = entries.get(card.entry_idx) else {
            continue;
        };
        let Some(row) = entry_card_row(app, agents, entry) else {
            continue;
        };
        // The same `row_signal` the app loop mounted this element from, so the
        // hue drawn and the marker's very existence come from one reading
        // rather than two that can disagree — see `App::failing_card_rows`.
        let Some(signal) = entry_row_signal(app, agents, entry) else {
            continue;
        };
        let Some(defect) = signal.defect else {
            continue;
        };
        let id = crate::anim::ElementId::failure_spider(row);
        let Some(elem_frame) = app.anim.frame(
            &id,
            Some(crate::anim::behaviour::names::FAILURE_SPIDER_PULSE),
        ) else {
            continue;
        };
        let t = match elem_frame.phase {
            crate::anim::Phase::Idle => 1.0,
            crate::anim::Phase::Mount | crate::anim::Phase::Dismount => elem_frame.progress,
            crate::anim::Phase::Retired => continue,
        };
        let Some((x, y)) = (match &crew {
            Some(crew) => crew_marker_position(card, crew, t),
            None => failure_spider_position(card, t),
        }) else {
            continue;
        };
        let buf = frame.buffer_mut();
        if x < buf.area.left()
            || x >= buf.area.right()
            || y < buf.area.top()
            || y >= buf.area.bottom()
        {
            continue;
        }

        // The two channels, and neither may reach into the other. The hue is
        // the row's own lifecycle stage, so a marker on a card that is still
        // running is drawn in the running hue rather than recolouring the
        // card's stage to red behind its back. The intensity is the fleet's
        // published defect severity, at 25/50/75/100% of the marker's full
        // loudness — held across every stage, so moving from `Running` to
        // `Failed` never reads as the defect having got worse.
        //
        // "Full" is `Severity::Serious`'s reach and not `Critical`'s, and that
        // ceiling is measured rather than chosen: the ramp pushes an ink
        // toward the light bound as it escalates, and at `Critical` a dark
        // panel washed the spider to a pale rose instead of a real red —
        // caught on a live render, not by a unit test. See
        // `anim::cell::MARKER_FULL_REACH`.
        let ink = crate::anim::cell::InkPalette::resolve(
            Style::default(),
            backdrop_rgb(app),
            &app.palette,
            &app.host_terminal_theme,
        )
        .with_marker(
            signal.stage.hue(&app.palette, &app.host_terminal_theme),
            defect.intensity(),
        );
        let paint = elem_frame.cell(
            crate::anim::cell::CellPos::new(0, 0),
            crate::anim::cell::CellExtent::new(1, 1),
            ink,
        );
        let style = paint
            .text_style(Style::default(), ink)
            .add_modifier(Modifier::BOLD);
        // `Buffer::set_string`, never direct cell indexing: the glyph is
        // double-width in every terminal that has been checked against this,
        // and only `set_string`'s own `unicode-width` accounting resets the
        // cell it would otherwise leave stale — see the `AGENTS.md` bullet on
        // a decoration's glyph never being free to move a column, which a
        // raw `set_symbol` on one cell silently violates for anything wider
        // than the settled glyph it stands on.
        buf.set_string(x, y, FAILURE_SPIDER_GLYPH, style);
    }
}

/// The spider's own glyph. A pictograph rather than a box-drawing run: the
/// captain's spec is explicit that this reads as a spider, not as a retro
/// blocky mark, and a character cell's only way to say that is the glyph it
/// draws — there is no multi-cell shape to compose one from without leaving
/// the character shell entirely (see [`render_failure_spiders`]'s doc
/// comment).
///
/// The trailing `U+FE0F` (emoji presentation selector) is load-bearing, not
/// decoration on the literal: without it a live render left a trail of stale
/// spider glyphs behind every position the climb passed through, one per
/// animation frame, because `ratatui::buffer::Buffer::diff` only emits an
/// explicit clear for a wide grapheme's trailing cell when the grapheme
/// contains `U+FE0F` — its own documented workaround for terminals that do
/// not reliably clear a wide emoji's trailing cell otherwise. A bare spider
/// codepoint is ambiguous width without it, so the clear was silently
/// skipped and the border kept every frame's leftover column live at once.
/// Caught live at `sidebar_width = 42`, not by a unit test — see this crate's
/// PR description for the capture.
const FAILURE_SPIDER_GLYPH: &str = "🕷\u{fe0f}";

/// The waypoints the spider's climb walks, in order: up this row's own trunk
/// column to its branch, along the branch to the card's own left border, up
/// that border to the top, then across the top border to centre.
///
/// Every leg is a single cell-grid axis move, never a diagonal, because that
/// is how the tree's own lines are drawn — see the `AGENTS.md` entry on the
/// character tree being the layout authority. `rect.x` is the trunk column
/// deliberately rather than a resolved ancestor level's own column: bullet 62
/// of that file records that a card's left border already stands in its own
/// connector's column, which is the one column guaranteed to exist for every
/// card regardless of how deep it is nested, so the climb needs no ancestor
/// topology to be well-defined.
fn failure_spider_waypoints(
    card: &crate::app::state::WorkspaceCardArea,
) -> Option<[(u16, u16); 5]> {
    let frame = card.card_frame?;
    if frame.width == 0 {
        return None;
    }
    let trunk_x = card.rect.x;
    let connector_y = card.content_y();
    let border_x = frame.x;
    let border_y = frame.y;
    let centre_x = frame.x + frame.width.saturating_sub(1) / 2;
    let start_y = card
        .rect
        .y
        .saturating_add(card.rect.height)
        .saturating_sub(1)
        .max(connector_y);
    Some([
        (trunk_x, start_y),
        (trunk_x, connector_y),
        (border_x, connector_y),
        (border_x, border_y),
        (centre_x, border_y),
    ])
}

/// Where a worker's marker sits on the row it is drawn on, at `t`.
///
/// A worker inside its mate's box has no border of its own to climb, so the
/// journey is the row's: up its own last line to its first, then in to the
/// middle of its text column. The same two moves at the scale the row actually
/// has — which is what [`image_card::crew`] gives it on the pixel path, a climb
/// up the row's own band rather than up a card's border.
///
/// It stays inside that text column at every point of the climb. A marker that
/// set out from the trunk, as a card's does, would cross its mate's border on
/// the way in — a mark from inside the box drawn outside it.
fn crew_marker_position(
    card: &crate::app::state::WorkspaceCardArea,
    crew: &CrewRowLayout,
    t: f32,
) -> Option<(u16, u16)> {
    if crew.text_width == 0 || card.rect.height == 0 {
        return None;
    }
    let left = card.rect.x.saturating_add(crew.text_offset);
    let top = card.rect.y;
    let bottom = card
        .rect
        .y
        .saturating_add(card.rect.height)
        .saturating_sub(1);
    let centre = left.saturating_add(crew.text_width.saturating_sub(1) / 2);
    let t = t.clamp(0.0, 1.0);
    let climb = f32::from(bottom.saturating_sub(top));
    let across = f32::from(centre.saturating_sub(left));
    let total = climb + across;
    if total <= 0.0 {
        return Some((centre, top));
    }
    let travelled = t * total;
    Some(if travelled <= climb {
        let leg = if climb > 0.0 { travelled / climb } else { 1.0 };
        (left, lerp_u16(bottom, top, leg))
    } else {
        let leg = if across > 0.0 {
            (travelled - climb) / across
        } else {
            1.0
        };
        (lerp_u16(left, centre, leg), top)
    })
}

/// Where the spider sits on its climb, at `t` in `0.0..=1.0`: `0.0` is just
/// setting out from below the row, `1.0` is arrived and resting at the top
/// centre border. Each leg gets a share of `t` proportional to its own length
/// in cells, so a tall card's climb up its own border is not rushed relative
/// to the jog to centre a short one gets.
fn failure_spider_position(
    card: &crate::app::state::WorkspaceCardArea,
    t: f32,
) -> Option<(u16, u16)> {
    let waypoints = failure_spider_waypoints(card)?;
    let t = t.clamp(0.0, 1.0);
    let legs: Vec<f32> = waypoints
        .windows(2)
        .map(|pair| {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            f32::from(x0.abs_diff(x1)) + f32::from(y0.abs_diff(y1))
        })
        .collect();
    let total: f32 = legs.iter().sum();
    if total <= 0.0 {
        let (x, y) = waypoints[waypoints.len() - 1];
        return Some((x, y));
    }
    let mut travelled = t * total;
    for (i, &len) in legs.iter().enumerate() {
        if travelled <= len || i == legs.len() - 1 {
            let leg_t = if len > 0.0 {
                (travelled / len).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let (x0, y0) = waypoints[i];
            let (x1, y1) = waypoints[i + 1];
            return Some((lerp_u16(x0, x1, leg_t), lerp_u16(y0, y1, leg_t)));
        }
        travelled -= len;
    }
    let (x, y) = waypoints[waypoints.len() - 1];
    Some((x, y))
}

fn lerp_u16(a: u16, b: u16, t: f32) -> u16 {
    let a = f32::from(a);
    let b = f32::from(b);
    (a + (b - a) * t).round().max(0.0) as u16
}

#[cfg(test)]
mod failure_spider_geometry {
    use super::*;

    fn card(rect: Rect, frame: Rect) -> crate::app::state::WorkspaceCardArea {
        crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect,
            worktree_child: false,
            entry_idx: 0,
            agent: None,
            card_frame: Some(frame),
            motion_cells: (0, 0),
            arriving: false,
            drawn_card: true,
        }
    }

    #[test]
    fn a_settled_spider_rests_at_the_top_centre_border() {
        let area = card(Rect::new(0, 0, 30, 6), Rect::new(2, 0, 26, 6));
        let (x, y) = failure_spider_position(&area, 1.0).expect("a card frame gives a position");
        assert_eq!(y, 0, "the top border row");
        assert_eq!(x, 2 + (26 - 1) / 2, "horizontally centred on the card");
    }

    #[test]
    fn a_spider_just_setting_out_starts_in_the_trunk_column_at_or_below_the_branch() {
        let area = card(Rect::new(0, 0, 30, 6), Rect::new(2, 0, 26, 6));
        let (x, y) = failure_spider_position(&area, 0.0).expect("a card frame gives a position");
        assert_eq!(
            x, area.rect.x,
            "the trunk column, not the card's own border"
        );
        assert!(y >= area.content_y(), "at or below this row's own branch");
    }

    #[test]
    fn a_card_with_no_frame_has_no_climb_at_all() {
        let mut area = card(Rect::new(0, 0, 10, 1), Rect::new(0, 0, 10, 1));
        area.card_frame = None;
        assert_eq!(
            failure_spider_position(&area, 0.5),
            None,
            "a bare line has no border to rest on"
        );
    }

    /// **A worker inside its mate's box keeps its marker, and keeps it inside
    /// the box.**
    ///
    /// It has no border of its own to climb, and dropping the marker for that
    /// reason would retire an open defect the moment the design changed. So it
    /// rides its own row instead — and never sets out from the trunk, which
    /// would put a mark from inside the box in the gutter outside it.
    #[test]
    fn a_workers_marker_rides_its_own_row_and_never_leaves_the_box() {
        let mut area = card(Rect::new(0, 4, 40, 2), Rect::new(6, 4, 34, 2));
        area.card_frame = None;
        let crew = CrewRowLayout {
            text_offset: 8,
            text_width: 30,
            via_mate: false,
            closes_the_box: false,
            lines: Vec::new(),
        };
        let left = area.rect.x + crew.text_offset;
        let right = left + crew.text_width;

        let (x, y) = crew_marker_position(&area, &crew, 1.0).expect("a crew row gives a position");
        assert_eq!(y, area.rect.y, "settled on the row's own first line");
        assert_eq!(
            x,
            left + (crew.text_width - 1) / 2,
            "centred on its own row"
        );

        let mut previous = crew_marker_position(&area, &crew, 0.0).unwrap();
        for step in 0u16..=20 {
            let current = crew_marker_position(&area, &crew, f32::from(step) / 20.0)
                .expect("a crew row gives a position");
            assert!(
                current.0 >= left && current.0 < right,
                "the marker left its mate's text column at step {step}: {current:?}"
            );
            assert!(
                current.1 >= area.rect.y && current.1 < area.rect.y + area.rect.height,
                "the marker left its own row at step {step}: {current:?}"
            );
            // One axis at a time, exactly as a card's own climb is.
            assert!(
                current.0 == previous.0 || current.1 == previous.1,
                "a step moved diagonally: {previous:?} -> {current:?}"
            );
            previous = current;
        }
    }

    /// The tree's own lines never run diagonally, and the climb has to match:
    /// each step is a move along one axis, the same way a `│` then a `─`
    /// meets a card rather than a line cutting the gutter on the bias.
    #[test]
    fn the_climb_only_ever_moves_one_axis_at_a_time() {
        let area = card(Rect::new(0, 0, 30, 6), Rect::new(2, 0, 26, 6));
        let mut previous = failure_spider_position(&area, 0.0).unwrap();
        for step in 1u16..=20 {
            let t = f32::from(step) / 20.0;
            let current = failure_spider_position(&area, t).unwrap();
            let dx = current.0.abs_diff(previous.0);
            let dy = current.1.abs_diff(previous.1);
            assert!(
                dx == 0 || dy == 0,
                "a step moved diagonally at t={t}: {previous:?} -> {current:?}"
            );
            previous = current;
        }
    }

    #[test]
    fn progress_is_monotonic_along_the_path() {
        let area = card(Rect::new(0, 0, 30, 8), Rect::new(2, 0, 26, 8));
        let waypoints = failure_spider_waypoints(&area).unwrap();
        let total: f32 = waypoints
            .windows(2)
            .map(|pair| {
                f32::from(pair[0].0.abs_diff(pair[1].0)) + f32::from(pair[0].1.abs_diff(pair[1].1))
            })
            .sum();
        assert!(
            total > 0.0,
            "a card taller than one row has real distance to climb"
        );
    }
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
    let backdrop = backdrop_rgb(app);
    let buf = frame.buffer_mut();
    for row in 0..height {
        let y = body_top + row;
        for col in 0..area.width {
            let x = area.x + col;
            let base = buf[(x, y)].style();
            let ink = InkPalette::resolve(base, backdrop, &app.palette, &app.host_terminal_theme);
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
                    backdrop,
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
    /// The colour under the panel, for a token whose own style names no
    /// background of its own.
    surface: Option<crate::ui::color::Rgb>,
    host: &'a crate::terminal_theme::TerminalTheme,
}

impl<'a> RowAnimation<'a> {
    fn for_workspace(app: &'a AppState, workspace_id: Option<&str>) -> Self {
        Self {
            anim: &app.anim,
            id: workspace_id.map(crate::anim::ElementId::workspace_row),
            palette: &app.palette,
            surface: backdrop_rgb(app),
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
            surface: backdrop_rgb(app),
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
        let ink = InkPalette::resolve(span.style, anim.surface, anim.palette, anim.host);
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
        anim.surface,
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
    surface: Option<crate::ui::color::Rgb>,
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
    let ink = InkPalette::resolve(style, surface, palette, host);
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

/// Whether a workspace row is the one the panel is calling out: the row the
/// cursor is on, the active Space, or the row being dragged.
///
/// Shared with the drawn card so the two card paths cannot disagree about which
/// row is lifted. The character card's glow ramp and the pixel card's lift are
/// the same selection cue at two resolutions, and the wash below is only ever
/// drawn where neither of them is.
pub(super) fn workspace_row_highlighted(app: &AppState, ws_idx: usize) -> bool {
    let selected = ws_idx == app.selected && matches!(app.mode, Mode::Navigate);
    let dragged = matches!(
        app.drag.as_ref().map(|drag| &drag.target),
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. })
            if *source_ws_idx == ws_idx
    );
    selected || Some(ws_idx) == app.active || dragged
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.sidebar_palette;
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
    let fold_width = row_fold_width(app, area);

    render_header_row(app, frame, area);

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;
    let entries = workspace_list_entries(app);
    let agents = sidebar_agent_entries(app);
    // Ranked once for the whole panel, not once per row. See [`body_register`].
    let bodies = body_register::BodyRegister::resolve(app);

    for card in cards {
        if card.agent.is_some() {
            render_agent_row(
                app,
                frame,
                card,
                &entries,
                &agents,
                area.y,
                list_bottom,
                fold_width,
            );
            continue;
        }
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let (own_depth, own_ancestors, own_is_last, own_rank) = match entries.get(card.entry_idx) {
            Some(entry) => (
                entry.depth(),
                entry.ancestors_continue().to_vec(),
                entry.is_last_child(),
                entry.rank(),
            ),
            // A Space with no entry to read a rank off is drawn as the mate its
            // depth makes it, which is what its entry would have said.
            None => (
                0,
                Vec::new(),
                true,
                crate::app::agent_tree::AgentRelation::FirstMate,
            ),
        };
        let own_ancestors = own_ancestors.as_slice();
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = workspace_row_highlighted(app, i);
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        // A card's first content row is its title, and it gets full weight
        // whether or not the cursor is on it: the frame, the chip and the glow
        // already say which row this is and what state it is in, so the name is
        // not competing with them for the same channel.
        let name_style = if selected || is_active || is_dragged || card.card_frame.is_some() {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let display_label = if card.worktree_child {
            grouped_child_display_label(&label, ws.branch().as_deref(), ws.custom_name.is_some())
        } else {
            label
        };
        let parent_group = (!card.worktree_child)
            .then(|| workspace_parent_group_state(app, i))
            .flatten();
        let (display_state, display_seen, display_state_age) = parent_group
            .as_ref()
            .filter(|(_, collapsed)| *collapsed)
            .map(|(key, _)| space_aggregate_state_and_age(app, key))
            .unwrap_or_else(|| (agg_state, agg_seen, space_state_age(app, ws)));
        let mark = state_icon(display_state, display_seen, app.status_indicators, p);
        // The severity channel, read once for this row: the connector charge and
        // the row's own card both draw from it, so the branch line and the card
        // it points at cannot disagree about how bad this row's trouble is.
        let row_severity = crate::app::lifecycle::severity(
            ws.metadata_tokens
                .get(crate::app::lifecycle::SEVERITY_TOKEN),
        );
        let card_shell = card.card_frame.and_then(|rect| {
            Card::new(
                rect,
                state_label_color(display_state, display_seen, p),
                highlighted,
                p,
                &app.host_terminal_theme,
            )
            // The box may reach well past this row — it grows over the workers
            // drawn inside it — but the card's *own* block is still this row's
            // rect, and that is what its pill closes.
            .map(|shell| shell.over_body(card.rect.height.saturating_sub(card::CHROME_ROWS)))
        });
        // Whether this Space's box was stretched down over workers of its own.
        // Read off the frame rather than re-walked, so the rule can only be
        // drawn on a box that actually holds a crew.
        let heads_a_crew = card
            .card_frame
            .is_some_and(|frame| frame.height > card.rect.height);
        // A transparent pixel card carries this row's ink itself, and anything
        // drawn under it would show through it rather than be covered by it. The
        // shell is still constructed — the row's content width, its rails and its
        // prefix are measured off it — it is just not drawn.
        let covered = card_shell.is_some() && image_card::shape_covers_row(app, fold_width);
        // Where this row is drawn, which is where the layout put it unless its
        // card is mid-slide. Resolved here rather than earlier because it is
        // only honoured while a pixel card is actually on this row.
        let motion = row_motion_cells(card, covered);

        // The selection wash follows the row it belongs to. Drawn after the
        // shell is known so it can, and before anything else this row paints so
        // it is still under all of it — it patches the background and leaves
        // every symbol alone.
        //
        // # Why it is clipped to the card, and dropped under a drawn one
        //
        // A row is wider than the card standing on it: the rails, the prefix and
        // the gutter either side are the row's, not the card's. A wash over the
        // whole row therefore paints a flat rectangle *around* the card, and on
        // a card row that rectangle is the only part of it anyone ever sees —
        // inside the frame the card's own glow ramp paints over it completely.
        // Clipped to the frame, the highlight stops exactly where the card's
        // border is drawn.
        //
        // Under a drawn card there is no cell-sized box to clip to at all: the
        // shape's border sits *inside* its first and last cell and leaves a
        // gutter above and below it, so even a frame-clipped wash would show as
        // a halo a fraction of a cell outside the drawn edge. The card carries
        // the selection itself there — see `image_card::lift` — so nothing is
        // painted under it.
        if highlighted && motion.0 == 0 && !covered {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let wash = card.card_frame.unwrap_or(card.rect);
            let buf = frame.buffer_mut();
            for y in wash.y..wash.y + wash.height {
                let Some(drawn_y) = moved_row(y, motion.1, area.y, list_bottom) else {
                    continue;
                };
                for x in wash.x..wash.x + wash.width {
                    buf[(x, drawn_y)].set_style(Style::default().bg(bg));
                }
            }
        }
        // The same mark, padded and plated. The alphabet is untouched: a chip
        // is where a mark is set, not a mark of its own.
        let chip = card_shell.as_ref().map(|shell| shell.chip(mark.0));
        let state_icon = match &chip {
            Some((text, style)) => (text.as_str(), *style),
            None => mark,
        };
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
                body: bodies.get(&crate::anim::CardRow::Space(ws.id.clone())),
                suppress_git_details: card.worktree_child,
                wall_now: app.wall_now,
            },
        );
        // The same call the layout made when it decided this row's height, so
        // the reserved height and the drawn lines cannot disagree about the
        // chevron or the badge.
        let content_width = entries
            .get(card.entry_idx)
            .map(|entry| list_entry_content_width(app, &agents, entry, fold_width))
            .unwrap_or_else(|| {
                // A Space with no entry to read a rank off is drawn as the mate
                // its depth makes it, which is what its entry would have said.
                row_content_width(
                    fold_width,
                    own_depth,
                    crate::app::agent_tree::AgentRelation::for_space(own_depth),
                    0,
                )
            });
        let shell = RowShell::for_fold_width(fold_width);
        let content_rows = row_height
            .saturating_sub(if card_shell.is_some() {
                shell.chrome_rows()
            } else {
                0
            })
            .max(1);
        let rows = shell_row_lines(rows, content_width, Some(content_rows), shell);
        let pill = card_shell.as_ref().and_then(|shell| {
            card_pill(
                shell,
                state_label(display_state, display_seen),
                rows.len(),
                content_rows,
            )
        });
        let pill_reservation = pill
            .as_ref()
            .zip(card_shell.as_ref())
            .map(|(pill, shell)| usize::from(shell.pill_reservation(&pill.label)))
            .unwrap_or(0);
        let content_y = card.content_y();
        let trunk = TrunkRailPaint::new(
            app,
            Some(crate::anim::CardRow::Space(ws.id.clone())),
            Style::default().fg(p.overlay0),
        );

        if let Some(shell) = &card_shell {
            let mut connector = Vec::new();
            let connector_style = Style::default().fg(p.overlay0);
            let top_charge = ConnectorCharge::new(app, connector_style, signal_phase, row_severity);
            if own_depth > 0 {
                let (mut owned, _) = agent_row_prefix(
                    own_depth,
                    own_is_last,
                    own_ancestors,
                    0,
                    p,
                    top_charge.as_ref(),
                    true,
                    trunk.as_ref(),
                    fold_width,
                );
                connector.append(&mut owned);
                // The branch runs on to the card's border, or stops short of
                // the column its level claimed when the card starts first.
                fit_rails_to_card(&mut connector, own_depth, own_rank, fold_width, true, p);
                connector.push(connector_joint_span(p));
            } else {
                connector.push(Span::raw(" "));
                fit_rails_to_card(&mut connector, own_depth, own_rank, fold_width, true, p);
            }
            let (mut above, _) = card_rail_prefix(
                own_depth,
                own_is_last,
                own_ancestors,
                CardRailSegment::AboveConnector,
                p,
                trunk.as_ref(),
                fold_width,
            );
            let (mut below, _) = card_rail_prefix(
                own_depth,
                own_is_last,
                own_ancestors,
                CardRailSegment::BelowConnector,
                p,
                trunk.as_ref(),
                fold_width,
            );
            fit_rails_to_card(&mut above, own_depth, own_rank, fold_width, false, p);
            fit_rails_to_card(&mut below, own_depth, own_rank, fold_width, false, p);
            render_card_border_rails(
                frame,
                card,
                connector,
                above,
                below,
                row_opens_a_branch(&entries, card.entry_idx).then(|| branch_rail_span(p)),
                list_entry_gap(app, &entries, card.entry_idx, fold_width),
                area.y,
                list_bottom,
                motion,
                image_card::row_arrival(app, card),
            );
            if !covered {
                shell.render_glow(frame, list_bottom);
            }
        }

        let last_content_row = rows.len().saturating_sub(1);
        // See the same line in `render_agent_row`: the row the branch lands on,
        // which the rails drawn above this loop also use.
        let connector_row = card.connector_y().saturating_sub(content_y);
        for (row_index, resolved) in rows.iter().enumerate() {
            if row_index as u16 >= content_rows || content_y + row_index as u16 >= list_bottom {
                break;
            }
            // A row still crossing the panel draws nothing in characters: its
            // own rail would point at a card that has not arrived, and under a
            // shape this line carries only that rail anyway.
            if motion.0 != 0 {
                continue;
            }
            let Some(drawn_y) =
                moved_row(content_y + row_index as u16, motion.1, area.y, list_bottom)
            else {
                continue;
            };
            // The branch line exists on exactly one row of a child card, so
            // that is the only row a signal can travel and the only row it
            // damages.
            let row_signal_phase = (row_index as u16 == connector_row)
                .then_some(signal_phase)
                .flatten();
            let mut spans = Vec::new();
            // Resolved once per row rather than per cell: the charge's colour
            // and its behaviour are the same for every cell of the route, and
            // only its position along that route differs.
            let connector_style = Style::default().fg(p.overlay0);
            let row_charge =
                ConnectorCharge::new(app, connector_style, row_signal_phase, row_severity);
            let token_budget = match &card_shell {
                // The connector is on `connector_row` — the card's *name* row
                // under the character shell, because that is the thing the tree
                // is pointing at, and the row a drawn card's own middle falls in
                // when the two differ. Every other row of the card carries the
                // plain rail, and the frame holds the content in from the column
                // beside it.
                Some(shell) => {
                    let (mut prefix, _) = if row_index as u16 == connector_row {
                        agent_row_prefix(
                            own_depth,
                            own_is_last,
                            own_ancestors,
                            0,
                            p,
                            row_charge.as_ref(),
                            true,
                            trunk.as_ref(),
                            fold_width,
                        )
                    } else {
                        card_rail_prefix(
                            own_depth,
                            own_is_last,
                            own_ancestors,
                            if (row_index as u16) < connector_row {
                                CardRailSegment::AboveConnector
                            } else {
                                CardRailSegment::BelowConnector
                            },
                            p,
                            trunk.as_ref(),
                            fold_width,
                        )
                    };
                    spans.append(&mut prefix);
                    // The columns between where the rails stop and where the
                    // card begins, before the border's own column.
                    let on_connector_row = row_index as u16 == connector_row;
                    fit_rails_to_card(
                        &mut spans,
                        own_depth,
                        own_rank,
                        fold_width,
                        on_connector_row,
                        p,
                    );
                    // The frame's own column and the pad inside it. Blank, so
                    // the border can be laid over the first of them once the
                    // row has had its say — except on the connector row of a
                    // nested card, where that column is where the branch meets
                    // the border.
                    if on_connector_row && own_depth > 0 {
                        spans.push(connector_joint_span(p));
                    } else {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::raw(" "));
                    usize::from(shell.content_width())
                }
                None => {
                    // Every Space row gets its whole prefix, rails included,
                    // from `agent_row_prefix` — a worktree child no differently
                    // from a Space somebody's `owner` token points at, because
                    // both are nodes of one tree and a connector that changed
                    // shape between them would put the two in different
                    // columns. At depth 0 - a fleet that declares no `owner`
                    // anywhere and runs no worktrees - this is one space and
                    // the row draws exactly as it always has.
                    let prefix_width = if own_depth > 0 {
                        // An owned Space takes the same connector a worker
                        // does: the tree runs through the Space/pane boundary,
                        // so it must not change shape at it — and, for the same
                        // reason, a charge has to run it too. A fleet that
                        // declares ownership with `owner` tokens is the shape
                        // the sidebar sketch actually describes, so this is the
                        // path most signals take.
                        let (mut owned, width) = agent_row_prefix(
                            own_depth,
                            own_is_last,
                            own_ancestors,
                            row_index,
                            p,
                            row_charge.as_ref(),
                            false,
                            trunk.as_ref(),
                            fold_width,
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
                    // Same right-alignment offset a card would have taken, so a
                    // panel folding mid-fleet keeps one set of left edges.
                    fit_rails_to_card(&mut spans, own_depth, own_rank, fold_width, false, p);
                    (card.rect.width as usize)
                        .saturating_sub(prefix_width)
                        .saturating_sub(usize::from(rank_width_inset(own_rank, fold_width)))
                }
            };
            // The first content row keeps the chevron cell clear, and the
            // badge's own width on top of it, so a mate's name is truncated
            // instead of being drawn under either control. The last one keeps
            // the status pill's columns for the same reason.
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
            } + if row_index == last_content_row {
                pill_reservation
            } else {
                0
            };
            // The whole first content row of a card is its title, however the
            // row's tokens are configured.
            let secondary_style = if card_shell.is_some() && row_index == 0 {
                name_style
            } else {
                branch_style
            };
            // Under a shape the prefix still draws — it is the tree's connector,
            // outside the card — but the row's own tokens do not: the pixel card
            // has already set this title, this chip and this tidbit in its own
            // type, and a transparent card would show both.
            if !covered {
                spans.extend(resolved_token_spans(
                    resolved,
                    (
                        state_icon.0,
                        arrived_state_icon_style(state_icon.1, row_charge.as_ref(), p),
                    ),
                    state_text_style,
                    name_style,
                    secondary_style,
                    secondary_style,
                    p,
                    &RowAnimation::for_workspace(app, Some(ws.id.as_str())),
                    token_budget.saturating_sub(trailing_width),
                ));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(card.rect.x, drawn_y, card.rect.width, 1),
            );
        }

        if let Some(shell) = &card_shell {
            if !covered {
                shell.render_frame(frame, list_bottom, pill.as_ref());
                if heads_a_crew {
                    // On the row this box used to close on, which is the one row
                    // of a stretched box that carries neither the card's own
                    // content nor a worker's.
                    shell.render_crew_rule(frame, card.rect.height.saturating_sub(1), list_bottom);
                }
            }
        }

        if let Some((owner, count)) = &summary_badge {
            if !covered {
                render_worker_summary_badge(app, frame, card, &agents, owner, *count, list_bottom);
            }
        }

        if let Some((_, collapsed)) = parent_group {
            // Skipped with the badge beside it, and for the same reason: the
            // card carries both now. They anchor inside the card's frame, so a
            // shape drawn over this row would have covered them — so the card
            // draws them itself, on its own right rail, and this is the bare
            // row's copy rather than the only one. See `image_card::ControlRail`.
            if !covered {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        if collapsed { "▸" } else { "▾" },
                        Style::default().fg(p.accent),
                    )),
                    workspace_group_chevron_rect(card),
                );
            }
        }
    }

    render_failure_spiders(app, frame, cards, &entries, &agents, fold_width);

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
        workspace_list_body_rect(app, area, scrollbar_rect.is_some()),
        list_bottom,
    );

    // The tray owns rows the tree was never given, so it draws outside the
    // transition rather than under it: a re-root is a change of what the tree
    // is showing, and the fleet's signals do not change with it.
    tray::render(app, frame, area);

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

    /// The captain's fleet in miniature: a first mate Space owning a second
    /// mate Space, with a worker pane under each of them. The first mate's
    /// worker is *directly* dispatched — it runs in the first mate's own Space
    /// and names the first mate as its owner, which is the row the report is
    /// about.
    fn direct_worker_fleet() -> AppState {
        let mut app = AppState::test_new();
        let mut first_mate = Workspace::test_new("firstmate");
        let direct_pane = first_mate.test_split(ratatui::layout::Direction::Vertical);
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let nested_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![first_mate, second_mate];
        app.ensure_test_terminals();
        app.active = Some(0);

        let now = std::time::Instant::now();
        let own = |app: &mut AppState, owner: &str, ws_idx: usize| {
            app.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([("owner".to_string(), Some(owner.to_string()))]),
                None,
                now,
            );
        };
        own(&mut app, "firstmate", 1);

        let worker = |app: &mut AppState, ws_idx: usize, pane, name: &str, owner: &str| {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app
                .terminals
                .get_mut(&terminal_id)
                .expect("test terminal present");
            terminal.set_agent_name(name.to_string());
            terminal.state = AgentState::Idle;
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([("owner".to_string(), Some(owner.to_string()))]),
                None,
                now,
            );
        };
        worker(&mut app, 0, direct_pane, "direct-worker", "firstmate");
        worker(&mut app, 1, nested_pane, "nested-worker", "2ndmate-explore");
        app
    }

    /// The captain's report: a worker the first mate dispatched itself draws
    /// immediately under the first mate's own row, with nothing above it but
    /// the first mate.
    ///
    /// It used to draw last. Node index decided sibling order, and every Space
    /// is appended to the node list before every pane, so the direct worker
    /// sorted below the second mate — and the depth-first walk then drew that
    /// mate's own worker in between, putting the first mate's most immediate
    /// row at the bottom of the panel.
    #[test]
    fn a_first_mates_own_worker_draws_directly_under_it() {
        let app = direct_worker_fleet();

        assert_eq!(
            tree_shape(&app),
            vec![
                (0, "firstmate".to_string()),
                (1, "direct-worker".to_string()),
                (1, "2ndmate-explore".to_string()),
                (2, "nested-worker".to_string()),
            ]
        );
    }

    /// Reordering siblings has to move their connectors with them: the row that
    /// is now last in the first mate's column is the one that closes it, and
    /// the direct worker above it must keep an open `├`.
    #[test]
    fn reordered_siblings_carry_their_connectors() {
        let app = direct_worker_fleet();
        let agents = sidebar_agent_entries(&app);
        let closes: Vec<(String, bool)> = workspace_list_entries_whole_fleet(&app)
            .into_iter()
            .map(|row| {
                let label = match row {
                    WorkspaceListEntry::Workspace { ws_idx, .. } => app.workspaces[ws_idx]
                        .display_name_from(&app.terminals, &TerminalRuntimeRegistry::new()),
                    WorkspaceListEntry::Agent { entry_idx, .. } => {
                        agents[entry_idx].agent_name.clone().unwrap_or_default()
                    }
                };
                (label, row.is_last_child())
            })
            .collect();

        assert_eq!(
            closes,
            vec![
                ("firstmate".to_string(), true),
                // No longer the last row in the first mate's column...
                ("direct-worker".to_string(), false),
                // ...the second mate is.
                ("2ndmate-explore".to_string(), true),
                ("nested-worker".to_string(), true),
            ]
        );
    }

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

    /// A worker's own row owns the ancestor gap that is still open beneath
    /// it, and nothing else in the tree does.
    ///
    /// Two second mates share one first mate, with a worker under the first
    /// of them. The worker sits at depth 2, and the ancestor column at level
    /// 1 — the first mate's own column, running past the first second mate —
    /// is still open, because the second mate follows: exactly the gap
    /// `agent_row_prefix` already draws a `│` for. Neither mate carries a
    /// segment of its own: both sit at depth 1, and the loop `agent_row_prefix`
    /// draws rails from starts at level 1, which for a depth-1 row is already
    /// past its own depth.
    #[test]
    fn a_workers_own_row_owns_the_open_ancestor_gap_beneath_it() {
        let mut app = crate::app::state::AppState::test_new();
        let mut mate_a = Workspace::test_new("2ndmate-a");
        let worker_pane = mate_a.test_split(ratatui::layout::Direction::Vertical);
        let mate_b = Workspace::test_new("2ndmate-b");
        app.workspaces = vec![Workspace::test_new("firstmate"), mate_a, mate_b];
        app.ensure_test_terminals();
        app.active = Some(0);

        let now = std::time::Instant::now();
        for idx in [1, 2] {
            app.workspaces[idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                now,
            );
        }
        let worker_terminal = app.workspaces[1].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&worker_terminal).unwrap();
        terminal.set_agent_name("worker".to_string());
        terminal.state = AgentState::Idle;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("2ndmate-a".to_string()))]),
            None,
            now,
        );

        let members = sidebar_trunk_segment_members(&app);
        assert_eq!(
            members,
            vec![(
                crate::anim::ElementId::trunk_segment(crate::anim::CardRow::Agent(worker_pane), 1,),
                crate::anim::behaviour::DriveInputs::default(),
            )],
        );
    }

    /// The same miniature fleet with **nothing hand-stamped on the worker**.
    ///
    /// Instead of a published `owner` token the worker's terminal carries the
    /// `created_by` record Herdr writes at creation, naming the mate's own pane
    /// in the mate's own Space — which is exactly what a `herdr tab create`
    /// issued from inside that pane leaves behind.
    ///
    /// `owner_workspace` selects which Space the creating pane stood in, so one
    /// helper covers both halves of the rule.
    fn natively_owned_fleet(
        width: u16,
        owner_workspace: usize,
    ) -> (crate::app::state::AppState, Vec<String>) {
        let mut app = crate::app::state::AppState::test_new();
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let worker_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![Workspace::test_new("firstmate"), second_mate];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        // The Space edge is unchanged by this feature: mates are still grouped
        // by their own published token.
        app.workspaces[1].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            std::time::Instant::now(),
        );

        let creator_workspace_id = app.workspaces[owner_workspace].id.clone();
        let worker_terminal = app.workspaces[1].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&worker_terminal).unwrap();
        terminal.set_agent_name("worker".to_string());
        terminal.state = AgentState::Idle;
        terminal.created_by = Some(crate::api::schema::PaneOrigin {
            pane_id: "creator".to_string(),
            workspace_id: creator_workspace_id,
        });

        let area = Rect::new(0, 0, width, 20);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rows = {
            let buffer = terminal.backend().buffer();
            (0..20).map(|row| row_text(buffer, row, width)).collect()
        };
        (app, rows)
    }

    /// The headline: a worker created from inside its mate's Space draws under
    /// that mate with no `owner` token published by anybody.
    ///
    /// Line shell, at a width below [`card::MIN_FOLD_WIDTH`], where the
    /// connector shares the row with the name. The card shell moves it onto the
    /// border row, which is why
    /// [`native_ownership_still_nests_when_the_panel_draws_cards`] checks the
    /// same ownership by indentation instead.
    #[test]
    fn a_natively_owned_worker_nests_under_its_space_with_no_token_published() {
        let (app, rows) = natively_owned_fleet(WIDEST_LINE_WIDTH, 1);
        let screen = rows.join("\n");

        assert!(
            all_agent_panel_entries(&app)
                .iter()
                .all(|entry| !entry.tokens.contains_key("owner")),
            "the worker was supposed to carry no owner token"
        );

        let row_of = |name: &str| {
            rows.iter()
                .position(|row| row.contains(name))
                .unwrap_or_else(|| panic!("{name} row missing:\n{screen}"))
        };
        let mate_row = row_of("2ndmate-explore");
        let worker_row = row_of("worker");
        assert!(
            mate_row < worker_row,
            "the worker did not land under its mate:\n{screen}"
        );
        assert!(
            rows[worker_row].contains('└') || rows[worker_row].contains('├'),
            "the worker drew with no connector, so it is not nested:\n{screen}"
        );
    }

    /// The tree's line is a line, not a dash per card.
    ///
    /// A card is four or more rows tall and the panel puts a gap under it, and
    /// the rail used to be drawn on exactly two of those rows — the top border
    /// and the bottom. Every row between them, and every gap row, came out
    /// blank, so the "line" joining a mate to its workers was a column of
    /// disconnected ticks. This walks the rows between two siblings' connectors
    /// and insists every one of them carries the rail.
    ///
    /// **Two second mates, each running a worker of its own.** The siblings are
    /// mates rather than workers now: a worker is drawn inside the box of the
    /// mate that dispatched it and hangs off nothing
    /// ([`crew_folds_into_its_space`]). That makes the question harder rather
    /// than easier — the box the rail has to run past is as tall as its mate's
    /// whole crew — which is exactly the run that used to come out as ticks.
    #[test]
    fn a_cards_rail_is_unbroken_from_one_sibling_to_the_next() {
        let width = NARROWEST_CARD_WIDTH + 6;
        let mut app = crate::app::state::AppState::test_new();
        let mut explore = Workspace::test_new("2ndmate-explore");
        let first = explore.test_split(ratatui::layout::Direction::Vertical);
        let mut build = Workspace::test_new("2ndmate-build");
        let second = build.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![Workspace::test_new("firstmate"), explore, build];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        for ws_idx in [1, 2] {
            app.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );
        }
        for (ws_idx, pane, name) in [(1usize, first, "worker-one"), (2, second, "worker-two")] {
            let owner_id = app.workspaces[ws_idx].id.clone();
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let Some(terminal) = app.terminals.get_mut(&terminal_id) else {
                continue;
            };
            terminal.set_agent_name(name.to_string());
            terminal.state = AgentState::Idle;
            terminal.created_by = Some(crate::api::schema::PaneOrigin {
                pane_id: name.to_string(),
                workspace_id: owner_id,
            });
        }

        let area = Rect::new(0, 0, width, 30);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).expect("test backend");
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .expect("draw");
        let rows: Vec<String> = terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let screen = rows.join("\n");

        let row_of = |name: &str| {
            rows.iter()
                .position(|row| row.contains(name))
                .unwrap_or_else(|| panic!("{name} row missing:\n{screen}"))
        };
        // Which of the two mates is on top is not what this test is about; that
        // there is an unbroken rail between them is.
        let upper = row_of("2ndmate-explore").min(row_of("2ndmate-build"));
        let lower = row_of("2ndmate-explore").max(row_of("2ndmate-build"));
        // Each of them is holding a worker of its own, so the run under test is
        // a run past a whole crew rather than past a bare card.
        let held = row_of("worker-one").min(row_of("worker-two"));
        assert!(
            held > upper && held < lower,
            "the upper mate is not holding a worker of its own:\n{screen}"
        );
        // Only the columns left of the mates' own frame count. The card's left
        // border is itself a `│`, so a test that looked at the whole row would
        // pass on a rail that was never drawn at all.
        let rail_columns = app
            .view
            .workspace_card_areas
            .iter()
            .filter(|card| {
                let row = u16::try_from(upper).unwrap_or(u16::MAX);
                card.rect.y <= row && row < card.rect.y.saturating_add(card.rect.height)
            })
            .find_map(|card| card.card_frame.map(|frame| frame.x))
            .unwrap_or_else(|| panic!("no card frame to measure the rail against:\n{screen}"));
        for (index, row) in rows.iter().enumerate().take(lower).skip(upper) {
            let rail: String = row.chars().take(usize::from(rail_columns)).collect();
            assert!(
                rail.contains('│') || rail.contains('├') || rail.contains('└'),
                "row {index} rail {rail:?} broke between the two mates:\n{screen}"
            );
        }
    }

    /// The same ownership, drawn in the **card** shell.
    ///
    /// Two properties, because a card can satisfy either one alone and still be
    /// wrong. A *mate* steps in by rank, which is what "nested" looks like at a
    /// glance, and its row carries its own `├`/`└`, which is what says who it is
    /// nested under. The card used to put that connector on its top border row
    /// instead — pointing at a corner rather than at a name — and this asserted
    /// only the indent, so the misplacement had nothing to fail.
    ///
    /// A *worker* answers neither, and that is the captain's rule rather than a
    /// gap: it is drawn inside its own mate's box, so it steps in past that
    /// box's border rather than onto a rung of the ladder, and it grows no
    /// connector at all because the border it is inside already says whose it
    /// is. See [`crew_folds_into_its_space`].
    #[test]
    fn native_ownership_still_nests_when_the_panel_draws_cards() {
        let (_app, rows) = natively_owned_fleet(NARROWEST_CARD_WIDTH + 6, 1);
        let screen = rows.join("\n");
        assert!(
            rows.iter().any(|row| row.contains('╭')),
            "expected the card shell at this width:\n{screen}"
        );

        let row_index_of = |name: &str| {
            rows.iter()
                .position(|row| row.contains(name))
                .unwrap_or_else(|| panic!("{name} row missing:\n{screen}"))
        };
        // The *column* the name starts in, not its byte offset: the prefixes are
        // box-drawing glyphs of three bytes each, so two names in the same
        // column are at wildly different offsets depending on how much rail is
        // to their left.
        let indent_of = |name: &str| {
            let row = &rows[row_index_of(name)];
            row.find(name)
                .map(|byte| row[..byte].chars().count())
                .unwrap_or_else(|| panic!("{name} row is blank:\n{screen}"))
        };

        let first = indent_of("firstmate");
        let mate = indent_of("2ndmate-explore");
        assert!(
            first < mate,
            "the mates' cards did not step in by rank (first={first}, mate={mate}):\n{screen}"
        );
        let mate_row = &rows[row_index_of("2ndmate-explore")];
        assert!(
            mate_row.contains('└') || mate_row.contains('├'),
            "the mate's connector is not on the row carrying its name:\n{screen}"
        );

        // The worker is inside that mate's box: flush with the mate's own text
        // column — a worker the mate dispatched itself takes no step, exactly as
        // the pixel list's tier `0` does — with no border and no connector of
        // its own between them.
        let worker_row = row_index_of("worker");
        assert_eq!(
            indent_of("worker"),
            mate,
            "the worker is not flush with its mate's own text column:\n{screen}"
        );
        assert!(
            mate > first,
            "the mate's box is not stepped in from the first mate's:\n{screen}"
        );
        for row in &rows[row_index_of("2ndmate-explore")..=worker_row] {
            assert!(
                !row.contains('╭'),
                "a box was opened between a mate and its own worker:\n{screen}"
            );
        }
        assert!(
            !rows[worker_row].contains('└') && !rows[worker_row].contains('├'),
            "the worker grew a connector inside the box that already holds it:\n{screen}"
        );
    }

    /// The load-bearing clause. A pane created from a *different* Space is a
    /// new Space being spun up, not a worker, so it takes no pane-level owner
    /// and must not draw as a child row inside the Space it happens to sit in.
    #[test]
    fn a_worker_created_from_another_space_takes_no_pane_level_owner() {
        let (app, _) = natively_owned_fleet(26, 0);
        let worker = all_agent_panel_entries(&app)
            .into_iter()
            .find(|entry| entry.agent_name.as_deref() == Some("worker"))
            .expect("the worker pane is an agent pane");

        assert_eq!(worker.owner, None);
        assert!(!worker.delegated_in_space);
    }

    /// The record holds a workspace *id* and the tree matches on a *label*, so
    /// the edge has to be re-resolved every pass. This is the test that fails
    /// the moment anyone snapshots the label at creation instead.
    #[test]
    fn renaming_a_space_moves_its_group_instead_of_orphaning_it() {
        let (mut app, _) = natively_owned_fleet(26, 1);
        let owner_of_worker = |app: &crate::app::state::AppState| {
            all_agent_panel_entries(app)
                .into_iter()
                .find(|entry| entry.agent_name.as_deref() == Some("worker"))
                .and_then(|entry| entry.owner)
        };

        assert_eq!(owner_of_worker(&app), Some("2ndmate-explore".to_string()));

        app.workspaces[1].set_custom_name("2ndmate-renamed".to_string());

        assert_eq!(owner_of_worker(&app), Some("2ndmate-renamed".to_string()));
    }

    /// A pane delegated *here* keeps its row even when the owner does not
    /// resolve. Losing the row is the failure this whole area exists to end;
    /// drawing it flat at the root is merely a worse tree.
    #[test]
    fn a_delegated_pane_with_no_resolved_owner_is_drawn_rather_than_deleted() {
        let mut entry = AgentPanelEntry::test_new("worker");
        entry.owner = None;
        entry.delegated_in_space = true;
        assert!(keeps_a_tree_row(&entry));
    }

    /// A pane that spun up a Space of its own is already on screen as that
    /// Space's row. Admitting it here as well draws the whole fleet twice —
    /// this was seen live in a lab before it was written down.
    #[test]
    fn a_pane_that_spun_up_its_own_space_is_not_drawn_a_second_time() {
        let mut entry = AgentPanelEntry::test_new("spun-up");
        entry.owner = None;
        entry.delegated_in_space = false;
        assert!(!keeps_a_tree_row(&entry));
    }

    /// An explicitly published token still admits a pane by itself, so a fleet
    /// that stamps ownership by hand keeps exactly the rows it always had.
    #[test]
    fn a_published_owner_still_earns_a_row_on_its_own() {
        let mut entry = AgentPanelEntry::test_new("background-helper");
        entry.owner = Some("background".into());
        entry.delegated_in_space = false;
        assert!(keeps_a_tree_row(&entry));
    }

    /// The cross-Space pane must be absent from the drawn rows entirely, not
    /// merely unowned - the whole-topology form of the case above.
    #[test]
    fn a_cross_space_creation_draws_no_pane_row_at_all() {
        let (app, _) = natively_owned_fleet(26, 0);
        assert!(
            sidebar_agent_live_entries(&app)
                .iter()
                .all(|entry| entry.agent_name.as_deref() != Some("worker")),
            "a pane that spun up its own Space must not also draw as a pane row"
        );
    }

    /// The other half of that rule: a pane nobody asked for still stays out.
    /// A mate's own pane is opened by a person, so drawing it here would put
    /// each mate on screen twice — once as its Space row, once as a child.
    #[test]
    fn a_pane_nobody_asked_for_is_still_left_out_of_the_tree() {
        let (mut app, _) = natively_owned_fleet(26, 1);
        let worker_terminal = all_agent_panel_entries(&app)
            .into_iter()
            .find(|entry| entry.agent_name.as_deref() == Some("worker"))
            .and_then(|entry| {
                app.workspaces[1].tabs[0]
                    .panes
                    .get(&entry.pane_id)
                    .map(|pane| pane.attached_terminal_id.clone())
            })
            .expect("the worker pane has a terminal");
        // Take away the record, leaving a pane that looks hand-made.
        app.terminals.get_mut(&worker_terminal).unwrap().created_by = None;

        assert!(
            sidebar_agent_live_entries(&app)
                .iter()
                .all(|entry| entry.agent_name.as_deref() != Some("worker")),
            "a pane with neither an owner nor an origin should stay out"
        );
    }

    /// Two second mates under one first mate, each with its own workers, and
    /// the panes created in an **interleaved** order: `a-one`, `b-one`,
    /// `a-two`, `b-two`, `a-three`.
    ///
    /// The interleaving is the point. A rule that only reversed the whole list
    /// would satisfy a single-group fixture by luck; here the two groups have to
    /// come out head-first *independently* of each other, which is what "the
    /// highest branch its parent allows" actually means.
    pub(super) fn interleaved_worker_fleet() -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        let mut mate_a = Workspace::test_new("2ndmate-a");
        let mut mate_b = Workspace::test_new("2ndmate-b");
        // Pane ids are allocated from one process-wide counter, so this
        // sequence *is* the creation order the entry rule reads.
        let a_one = mate_a.test_split(ratatui::layout::Direction::Vertical);
        let b_one = mate_b.test_split(ratatui::layout::Direction::Vertical);
        let a_two = mate_a.test_split(ratatui::layout::Direction::Vertical);
        let b_two = mate_b.test_split(ratatui::layout::Direction::Vertical);
        let a_three = mate_a.test_split(ratatui::layout::Direction::Vertical);

        app.workspaces = vec![Workspace::test_new("firstmate"), mate_a, mate_b];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        for ws_idx in [1usize, 2] {
            app.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                now,
            );
        }

        let groups = [
            (
                1usize,
                vec![(a_one, "a-one"), (a_two, "a-two"), (a_three, "a-three")],
            ),
            (2usize, vec![(b_one, "b-one"), (b_two, "b-two")]),
        ];
        for (ws_idx, panes) in groups {
            let owner_id = app.workspaces[ws_idx].id.clone();
            for (pane, name) in panes {
                let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                    .attached_terminal_id
                    .clone();
                let Some(terminal) = app.terminals.get_mut(&terminal_id) else {
                    continue;
                };
                terminal.set_agent_name(name.to_string());
                terminal.state = AgentState::Idle;
                terminal.created_by = Some(crate::api::schema::PaneOrigin {
                    pane_id: name.to_string(),
                    workspace_id: owner_id.clone(),
                });
            }
        }
        app
    }

    /// Every row the panel would draw, in drawn order — Spaces by their tree
    /// name, panes by their agent name.
    ///
    /// Read through the flattened tree rather than off the entry list, so what
    /// is asserted is what a viewer would see rather than an intermediate the
    /// arranger is still free to reorder.
    pub(super) fn drawn_tree_rows(app: &crate::app::state::AppState) -> Vec<String> {
        let agents = sidebar_agent_entries(app);
        workspace_list_entries_expanded(app)
            .into_iter()
            .filter_map(|entry| match entry {
                WorkspaceListEntry::Workspace { ws_idx, .. } => space_tree_name(app, ws_idx),
                WorkspaceListEntry::Agent { entry_idx, .. } => agents
                    .get(entry_idx)
                    .and_then(|agent| agent.agent_name.clone()),
            })
            .collect()
    }

    /// The rows drawn directly beneath `parent`, as a contiguous run.
    ///
    /// Contiguity is doing real work here: the tree walk emits a parent's
    /// subtree in one unbroken block, so a card that had escaped into another
    /// mate's group would show up as the wrong name inside this slice rather
    /// than merely as a different order.
    pub(super) fn rows_under(
        app: &crate::app::state::AppState,
        parent: &str,
        count: usize,
    ) -> Vec<String> {
        let rows = drawn_tree_rows(app);
        let at = rows
            .iter()
            .position(|row| row == parent)
            .unwrap_or_else(|| panic!("{parent} is not on screen: {rows:?}"));
        rows.into_iter().skip(at + 1).take(count).collect()
    }

    /// The entry rule under `Spaces`, which is the mode a card is actually
    /// watched arriving in: nothing sorts, so where a card entered is where it
    /// stays, and the newest is at the top of its own group.
    #[test]
    fn a_new_card_enters_at_the_head_of_its_parent_under_the_spaces_sort() {
        let mut app = interleaved_worker_fleet();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Spaces;

        assert_eq!(
            rows_under(&app, "2ndmate-a", 3),
            vec!["a-three", "a-two", "a-one"],
            "the newest worker did not enter at the top of its own mate's group"
        );
        assert_eq!(
            rows_under(&app, "2ndmate-b", 2),
            vec!["b-two", "b-one"],
            "the second mate's group did not get the same rule independently"
        );
    }

    /// The same rule under `Priority`. The workers are all idle-and-seen with
    /// no state-change sequence, so the sort ranks them equal and is
    /// *indifferent* — which is exactly where the entry position has to still
    /// be visible, because a stable sort is what carries it through.
    #[test]
    fn a_new_card_enters_at_the_head_of_its_parent_under_the_priority_sort() {
        let mut app = interleaved_worker_fleet();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        assert_eq!(
            rows_under(&app, "2ndmate-a", 3),
            vec!["a-three", "a-two", "a-one"]
        );
        assert_eq!(rows_under(&app, "2ndmate-b", 2), vec!["b-two", "b-one"]);
    }

    /// Entry position and sort order are two separate things and both hold:
    /// having entered at the head, a card is the sort's from then on. The
    /// oldest worker blocks, and `Priority` must pull it to the top of its
    /// group *over* the entry order — a fleet where a burst of new panes buried
    /// a blocked one would be the bug this half prevents.
    #[test]
    fn the_sort_still_owns_a_card_after_it_has_entered() {
        let mut app = interleaved_worker_fleet();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let blocked = sidebar_agent_entries(&app)
            .into_iter()
            .find(|entry| entry.agent_name.as_deref() == Some("a-one"))
            .expect("the oldest worker is on screen");
        let terminal_id = app.workspaces[blocked.ws_idx].tabs[0].panes[&blocked.pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("the worker has a terminal")
            .state = AgentState::Blocked;

        assert_eq!(
            rows_under(&app, "2ndmate-a", 3),
            vec!["a-one", "a-three", "a-two"],
            "the sort did not move a card the entry rule had put at the bottom"
        );
        // ...and it moved it *within its own parent*. The other mate's group is
        // untouched, and the blocked worker did not surface above it.
        assert_eq!(rows_under(&app, "2ndmate-b", 2), vec!["b-two", "b-one"]);
    }

    /// A card enters at the top of the branch that owns it and no higher.
    /// Whatever the sort, every worker stays inside its own mate's contiguous
    /// block — the only thing that ever takes a card out of its parent is
    /// removal.
    #[test]
    fn entry_never_lifts_a_card_out_of_its_own_parent() {
        for sort in [
            crate::app::state::AgentPanelSort::Spaces,
            crate::app::state::AgentPanelSort::Priority,
        ] {
            let mut app = interleaved_worker_fleet();
            app.agent_panel_sort = sort;

            let mut under_a = rows_under(&app, "2ndmate-a", 3);
            let mut under_b = rows_under(&app, "2ndmate-b", 2);
            under_a.sort();
            under_b.sort();
            assert_eq!(under_a, vec!["a-one", "a-three", "a-two"], "{sort:?}");
            assert_eq!(under_b, vec!["b-one", "b-two"], "{sort:?}");
        }
    }

    /// Entry is about *where a card comes in*, so the test that names it best
    /// is the one that adds a pane to a fleet that was already drawn: the new
    /// worker takes the top of its group, and pushes the ones that were there
    /// down rather than landing beneath them.
    #[test]
    fn a_pane_created_later_takes_the_top_of_a_group_that_already_had_rows() {
        let mut app = interleaved_worker_fleet();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Spaces;
        let before = rows_under(&app, "2ndmate-a", 3);

        let arrival = app.workspaces[1].test_split(ratatui::layout::Direction::Vertical);
        app.ensure_test_terminals();
        let owner_id = app.workspaces[1].id.clone();
        let terminal_id = app.workspaces[1].tabs[0].panes[&arrival]
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("the new pane has a terminal");
        terminal.set_agent_name("a-four".to_string());
        terminal.state = AgentState::Idle;
        terminal.created_by = Some(crate::api::schema::PaneOrigin {
            pane_id: "a-four".to_string(),
            workspace_id: owner_id,
        });

        let after = rows_under(&app, "2ndmate-a", 4);
        assert_eq!(after.first().map(String::as_str), Some("a-four"));
        assert_eq!(
            after[1..],
            before[..],
            "the rows that were already there should have been pushed down intact"
        );
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
            worktree_child: false,
            entry_idx: 0,
            agent: None,
            card_frame: None,
            motion_cells: (0, 0),
            arriving: false,
            drawn_card: true,
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

    /// A branch short enough that a row's two configured lines still merge
    /// inside [`card::MIN_FOLD_WIDTH`].
    ///
    /// Folding is the bare line's layout, and the bare line is now what the
    /// panel draws *below* the card threshold, so a fixture that only folds at
    /// 47 columns cannot exercise the fold at all. This one can.
    const FOLDABLE_BRANCH: &str = "main";

    /// The two panel widths either side of the shell threshold.
    ///
    /// The threshold is stated in fold widths, and a panel folds against
    /// `width - 2` (one column for the divider bar, one for the scrollbar the
    /// fold always assumes), so the narrowest panel that draws cards is
    /// `MIN_FOLD_WIDTH + 2` and the widest that draws lines is one below it.
    const NARROWEST_CARD_WIDTH: u16 = card::MIN_FOLD_WIDTH + 2;
    const WIDEST_LINE_WIDTH: u16 = NARROWEST_CARD_WIDTH - 1;

    /// A realistic captain fleet rendered with the *default* sidebar layout
    /// (two token rows per Space and per agent), so the dump shows what a user
    /// who never edited `[ui.sidebar]` actually sees.
    fn default_layout_fleet_rows(width: u16, height: u16) -> Vec<String> {
        default_layout_fleet(width, height, None).1
    }

    /// The same fleet on a branch short enough to fold inside a narrow panel.
    fn foldable_fleet_rows(width: u16, height: u16) -> Vec<String> {
        default_layout_fleet_on(width, height, None, FOLDABLE_BRANCH).1
    }

    /// The same fleet, optionally with one worker having published a summary so
    /// its owning mate earns a badge over row 0.
    fn default_layout_fleet(
        width: u16,
        height: u16,
        summary: Option<&str>,
    ) -> (crate::app::state::AppState, Vec<String>) {
        default_layout_fleet_on(width, height, summary, FIXTURE_BRANCH)
    }

    fn default_layout_fleet_on(
        width: u16,
        height: u16,
        summary: Option<&str>,
        branch: &str,
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

        // The Space rows are pinned to the two-line branch layout these fold
        // tests were written against, for exactly the reason the branch below is
        // pinned: they measure the *fold machinery*, and a fixture whose line
        // count came from the shipped default would re-measure the default every
        // time it changed. The shipped default is now three lines — the name and
        // the two body registers, see `body_register` — and it is checked where
        // it belongs, in `config::sidebar`'s own defaults test.
        app.sidebar_spaces.rows = vec![
            vec![
                crate::config::SpaceSidebarToken::StateIcon,
                crate::config::SpaceSidebarToken::Workspace,
            ],
            vec![
                crate::config::SpaceSidebarToken::Branch,
                crate::config::SpaceSidebarToken::GitStatus,
            ],
        ];

        // `Workspace::test_new` resolves `cached_git_branch` from the real
        // checkout, so an unpinned fixture inherits whatever branch the tree
        // happens to be on - and renders one line per Space instead of two on a
        // detached HEAD. That is not hypothetical: `actions/checkout` builds a
        // pull request from a detached `refs/pull/N/merge`, and a rebase detaches
        // too. Pin it so the fold is measured against a fixed layout.
        for workspace in app.workspaces.iter_mut() {
            workspace.cached_git_branch = Some(branch.to_string());
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
        // Swept across folds because right alignment made the prefix a function
        // of the panel's width: at the floor the ladder is worth nothing and at
        // the captain's width it is worth its full step.
        for fold in [card::MIN_FOLD_WIDTH, card::MIN_FOLD_WIDTH + 4, 40, 60] {
            for depth in 0u8..5 {
                for row_index in [0usize, 1] {
                    for is_last_child in [true, false] {
                        // The joint changes a glyph, never a column: both shells
                        // are measured, so a card row and a line row cannot be
                        // handed different budgets for the same prefix.
                        for meets_a_card in [true, false] {
                            let ancestors = vec![true; depth as usize + 1];
                            let (_, drawn) = agent_row_prefix(
                                depth,
                                is_last_child,
                                &ancestors,
                                row_index,
                                &p,
                                None,
                                meets_a_card,
                                None,
                                fold,
                            );
                            assert_eq!(
                                drawn,
                                tree_prefix_columns(depth, row_index, fold),
                                "fold {fold} depth {depth} row {row_index} \
                             last={is_last_child} card={meets_a_card}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The card's rails are measured by the same function, at the same row
    /// index, as the connector row above them — every row of a card starts in
    /// the same column, or the frame would step in and out as it descended.
    #[test]
    fn a_cards_rails_stop_where_its_border_starts() {
        let p = Palette::catppuccin();
        for fold in [card::MIN_FOLD_WIDTH, card::MIN_FOLD_WIDTH + 4, 40, 60] {
            for depth in 0u8..5 {
                for is_last_child in [true, false] {
                    for segment in [
                        CardRailSegment::AboveConnector,
                        CardRailSegment::BelowConnector,
                    ] {
                        let ancestors = vec![true; depth as usize + 1];
                        let (_, drawn) = card_rail_prefix(
                            depth,
                            is_last_child,
                            &ancestors,
                            segment,
                            &p,
                            None,
                            fold,
                        );
                        assert_eq!(
                            drawn,
                            tree_prefix_columns(depth, 0, fold),
                            "fold {fold} depth {depth} last={is_last_child} {segment:?}"
                        );
                    }
                }
            }
        }
    }

    /// A last child's line has to reach it and then stop. Before the reality
    /// pass the same rail was drawn above and below the connector, so a last
    /// child got no line at all coming down to it and a middle child's line ran
    /// on past the bottom of the tree.
    #[test]
    fn a_last_childs_rail_arrives_and_then_stops() {
        let p = Palette::catppuccin();
        const FOLD: u16 = 40;
        let ancestors = vec![true, true];
        let ink = |segment| {
            let (spans, _) = card_rail_prefix(1, true, &ancestors, segment, &p, None, FOLD);
            spans.iter().any(|span| span.content.contains('│'))
        };
        assert!(
            ink(CardRailSegment::AboveConnector),
            "the parent's line has to reach its last child"
        );
        assert!(
            !ink(CardRailSegment::BelowConnector),
            "and has to stop there, or it points at a sibling that does not exist"
        );

        let ink_middle = |segment| {
            let (spans, _) = card_rail_prefix(1, false, &ancestors, segment, &p, None, FOLD);
            spans.iter().any(|span| span.content.contains('│'))
        };
        assert!(ink_middle(CardRailSegment::AboveConnector));
        assert!(
            ink_middle(CardRailSegment::BelowConnector),
            "a child with siblings under it keeps the line running"
        );
    }

    /// The layout reserves `content + chrome` rows and the renderer draws the
    /// chrome at exactly those offsets, so a frame always closes inside the
    /// rows its own row reserved.
    #[test]
    fn a_cards_reserved_height_is_its_content_plus_its_chrome() {
        for lines in 1usize..6 {
            assert_eq!(
                shell_row_height(lines, RowShell::Card),
                shell_row_height(lines, RowShell::Line) + card::CHROME_ROWS
            );
        }
    }

    /// Every row of the tree earns a frame at the threshold width, at every
    /// depth the tree can reach, and a worktree child earns one on the same
    /// terms as any other Space — its prefix is now measured from depth alone,
    /// so being a worktree child can no longer cost it columns its siblings
    /// keep. A depth that fell back to a bare line while its siblings drew
    /// cards would be two layouts stacked on each other, and its reserved
    /// height would already have been spent on chrome it never drew.
    #[test]
    fn every_depth_still_has_room_for_a_frame_at_the_threshold() {
        for depth in 0u8..5 {
            for worktree_child in [false, true] {
                let entry = WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    worktree_child,
                    depth,
                    is_last_child: true,
                    ancestors_continue: vec![true; depth as usize + 1],
                };
                let rect = Rect::new(0, 0, card::MIN_FOLD_WIDTH + 1, 4);
                let frame =
                    card_frame_for(rect, &entry, card::MIN_FOLD_WIDTH).unwrap_or_else(|| {
                        panic!("depth {depth} worktree_child={worktree_child} drew no frame")
                    });
                assert!(
                    frame.width > card::CHROME_COLS,
                    "depth {depth} worktree_child={worktree_child} left no room inside its frame"
                );
            }
        }
    }

    /// The badge and the chevron used to anchor on `card.rect.y`, which on a
    /// card is the top border. They move down onto the first content row and in
    /// past the right border, so neither is ever drawn on the frame itself.
    #[test]
    fn a_cards_controls_sit_on_its_first_content_row_inside_the_frame() {
        let rect = Rect::new(0, 5, 40, 4);
        let frame = Rect::new(1, 5, 38, 4);
        let card = crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect,
            worktree_child: false,
            entry_idx: 0,
            agent: None,
            card_frame: Some(frame),
            motion_cells: (0, 0),
            arriving: false,
            drawn_card: true,
        };
        let chevron = workspace_group_chevron_rect(&card);
        assert_eq!(
            chevron.y,
            rect.y + 1,
            "the chevron stayed on the top border"
        );
        assert_eq!(
            chevron.x,
            frame.x + frame.width - 2,
            "the chevron landed on the frame's right border"
        );

        let badge = worker_summary_badge_rect(&card, 3);
        assert_eq!(badge.y, chevron.y, "the badge and the chevron parted rows");
        assert_eq!(
            badge.x + badge.width,
            chevron.x,
            "the badge did not stop one cell left of the chevron"
        );

        // The bare line is untouched: its content starts on its own first row
        // and reaches its own right edge.
        let line = crate::app::state::WorkspaceCardArea {
            card_frame: None,
            motion_cells: (0, 0),
            arriving: false,
            ..card
        };
        assert_eq!(workspace_group_chevron_rect(&line).y, rect.y);
        assert_eq!(
            workspace_group_chevron_rect(&line).x,
            rect.x + rect.width - 1
        );
    }

    /// The narrow end is the regression bar: at the widths that still draw the
    /// bare line, every row spends the lines its layout asked for and the tree
    /// keeps its connectors.
    ///
    /// The sweep stops at [`WIDEST_LINE_WIDTH`] because past it the panel draws
    /// cards, whose rows are counted by
    /// `a_card_never_folds_the_lines_it_is_made_of` instead.
    #[test]
    fn a_narrow_sidebar_still_stacks_every_configured_line() {
        for width in [18u16, 22, 26, 30, WIDEST_LINE_WIDTH] {
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

    /// The whole point of the fold: give a row room and it gives a line back
    /// rather than sitting in a stack sized for a panel half as wide.
    ///
    /// Measured at [`WIDEST_LINE_WIDTH`] now rather than at 70 columns. The
    /// fold is the *bare line's* layout, and a panel wide enough to hold a
    /// 30-character branch on one line is a panel wide enough to draw cards, in
    /// which the title and the subtitle are the two rows the card is made of.
    /// So the room the fold needs is bought with a short branch instead of a
    /// wide panel; what is being tested — merge two configured lines when they
    /// fit — is unchanged.
    #[test]
    fn a_wide_sidebar_folds_a_rows_configured_lines_onto_one() {
        let cramped = tree_rows(&foldable_fleet_rows(18, 24)).len();
        let rows = foldable_fleet_rows(WIDEST_LINE_WIDTH, 24);
        let screen = rows.join("\n");
        let tree = tree_rows(&rows);

        assert!(
            tree.len() < cramped,
            "the tree spent {} lines at {WIDEST_LINE_WIDTH} columns, no fewer than the \
             {cramped} it spent at 18:\n{screen}",
            tree.len()
        );
        assert!(
            tree[0].contains("firstmate") && tree[0].contains(FOLDABLE_BRANCH),
            "the first mate's branch did not join its name:\n{screen}"
        );
        // A folded row is still a row of the tree: the merge buys a line back
        // and touches nothing else.
        assert!(
            tree[1].contains("├─ ")
                && tree[1].contains("2ndmate-explore")
                && tree[1].contains(FOLDABLE_BRANCH),
            "a folded mate lost its connector or its branch:\n{screen}"
        );
        assert!(
            tree.iter().any(|row| row.contains("│  └─ ")),
            "the last worker lost its rail or its closing connector:\n{screen}"
        );
    }

    /// Folding is only ever allowed to buy a row back, never to spend a
    /// character: a line the layout judged foldable is a line that draws whole.
    #[test]
    fn folding_never_elides_what_it_folded() {
        for width in 24u16..=WIDEST_LINE_WIDTH {
            let rows = foldable_fleet_rows(width, 24);
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
        // widths steps straight over it. The sweep stops at the widest bare
        // line and rides a short branch, because folding is the bare line's
        // layout: a card keeps its title and its subtitle on separate rows, and
        // its badge is reserved out of the first of them.
        let mut ever_folded = false;
        for width in 28u16..=WIDEST_LINE_WIDTH {
            let (_, rows) =
                default_layout_fleet_on(width, 24, Some("rebased and green"), FOLDABLE_BRANCH);
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
            if !badged.contains(FOLDABLE_BRANCH) {
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

    /// Resizing has to be monotonic to feel like resizing: inside one shell, no
    /// width may cost the tree rows that a narrower one could show.
    ///
    /// Swept per shell rather than across the whole range, because the panel
    /// changes shell exactly once — at [`card::MIN_FOLD_WIDTH`], where the bare
    /// line becomes a card — and that step *does* cost rows. It is the trade
    /// the card is: two rows of chrome an entity, bought deliberately. What
    /// must not happen is a second such step hiding anywhere else in the range,
    /// which is what these two sweeps rule out.
    #[test]
    fn widening_the_sidebar_never_costs_the_tree_a_row() {
        for shell in [18u16..=WIDEST_LINE_WIDTH, NARROWEST_CARD_WIDTH..=90] {
            let mut previous = usize::MAX;
            for width in shell {
                let lines = tree_rows(&default_layout_fleet_rows(width, 24)).len();
                assert!(
                    lines <= previous,
                    "widening to {width} columns grew the tree from {previous} lines to {lines}"
                );
                previous = lines;
            }
        }
    }

    /// The one step that does cost rows, pinned so it stays deliberate: the
    /// column at which the panel stops drawing lines and starts drawing cards.
    ///
    /// **A box per mate, not per entity.** A worker is drawn inside the box of
    /// the mate that dispatched it ([`crew_folds_into_its_space`]), so the
    /// fixture's six rows open three boxes: the first mate's, and one for each
    /// second mate. The bill is two rows a box — its top border and its closing
    /// rule — plus one more for every box whose rule had to move down past a
    /// crew, which is the row that rule landed on. See
    /// [`CrewRowLayout::closes_the_box`].
    #[test]
    fn the_card_shell_starts_at_one_named_width_and_costs_two_rows_a_box() {
        const BOXES: usize = 3;
        const BOXES_WITH_A_CREW: usize = 1;
        let line = default_layout_fleet_rows(WIDEST_LINE_WIDTH, 40);
        let card = default_layout_fleet_rows(NARROWEST_CARD_WIDTH, 40);
        assert!(
            !line.iter().any(|row| row.contains('╭')),
            "a card was drawn at {WIDEST_LINE_WIDTH} columns, below the threshold:\n{}",
            line.join("\n")
        );
        assert_eq!(
            tree_rows(&card).iter().filter(|r| r.contains('╭')).count(),
            BOXES,
            "the card shell did not start at {NARROWEST_CARD_WIDTH} columns:\n{}",
            card.join("\n")
        );
        assert_eq!(
            tree_rows(&card).len(),
            tree_rows(&line).len() + 2 * BOXES + BOXES_WITH_A_CREW,
            "the shell cost something other than two rows a box:\n{}",
            card.join("\n")
        );
    }

    /// A row that cannot have every line it asked for used to lose the tail
    /// outright. It is folded onto what it does have instead, so the token
    /// budget elides it rather than the layout dropping it.
    ///
    /// One body row, at a width narrow enough that the row's two configured
    /// lines do not merge on their own: the squeeze is the only thing that can
    /// bring the branch up onto the name's line.
    #[test]
    fn a_row_squeezed_below_its_line_count_keeps_its_tail() {
        let rows = default_layout_fleet_rows(30, 3);
        let screen = rows.join("\n");
        let tree = tree_rows(&rows);

        assert!(!tree.is_empty(), "nothing rendered:\n{screen}");
        assert!(
            tree[0].contains("firstmate") && tree[0].contains("fm/herd"),
            "the first mate's second line was dropped instead of folded:\n{screen}"
        );
        assert!(
            tree[0].contains('…'),
            "the squeezed tail fitted whole, so nothing was actually elided:\n{screen}"
        );
    }

    /// A card is drawn whole or not at all.
    ///
    /// The layout already refused to place a row that does not fit the body
    /// (`compute_workspace_list_areas`), and a card makes that refusal visible:
    /// half a card is a top border and a title with no closing rule, which
    /// reads as a rendering fault rather than as a list that ran out of room.
    #[test]
    fn a_card_is_drawn_whole_or_not_at_all() {
        for height in 3u16..=12 {
            let rows = default_layout_fleet_rows(44, height);
            let screen = rows.join("\n");
            let tree = tree_rows(&rows);
            let opened = tree.iter().filter(|row| row.contains('╭')).count();
            let closed = tree.iter().filter(|row| row.contains('╰')).count();
            assert_eq!(
                opened, closed,
                "a card was opened and not closed at height {height}:\n{screen}"
            );
        }
    }

    /// **A crew list the panel cannot finish stops one worker short, so the
    /// closing rule is a rule.**
    ///
    /// A worker that ends its mate's list reserves the row the box closes on;
    /// the ones before it do not, because the rule is not coming there. Where
    /// the panel runs out mid-list it *is* coming there, and with nothing kept
    /// free it landed on the last worker's own line — a closing rule with a
    /// branch name set into the middle of it, live at every height that cut a
    /// crew. See [`crew_closing_reserve`].
    #[test]
    fn a_cut_crew_list_closes_on_a_rule_and_not_on_a_workers_name() {
        for height in 3u16..=20 {
            let rows = default_layout_fleet_rows(40, height);
            let screen = rows.join("\n");
            for row in tree_rows(&rows)
                .iter()
                .filter(|row| row.contains('\u{2570}'))
            {
                let rule: String = row
                    .chars()
                    .skip_while(|glyph| *glyph != '\u{2570}')
                    .take_while(|glyph| *glyph != '\u{256f}')
                    .collect();
                assert!(
                    rule.chars()
                        .all(|glyph| glyph == '\u{2570}' || glyph == '\u{2500}'),
                    "a closing rule at height {height} has a worker\'s row set \
                     into it — {rule:?}:\n{screen}"
                );
            }
        }
    }

    /// The card's content rows *are* the card, so they never merge however much
    /// room the panel has. A folded card would be a bordered line, which is the
    /// one thing the shell exists not to be.
    ///
    /// A *worker* inside one is the other way round, and deliberately: it is a
    /// line, and a line folds. That is what makes a mate's crew the compact
    /// one-row-per-worker list the mockups ask for rather than a second card's
    /// worth of block set inside the first — see [`crew_row_layout`]. So the
    /// height under test is each mate's own, asked of the layout, rather than
    /// the whole tree's row count, which now moves with how far a crew folds.
    #[test]
    fn a_card_never_folds_the_lines_it_is_made_of() {
        for width in [NARROWEST_CARD_WIDTH, 44, 70, 90] {
            let (app, rows) = default_layout_fleet_on(width, 40, None, FOLDABLE_BRANCH);
            let screen = rows.join("\n");
            let tree = tree_rows(&rows);
            // Three boxes, one per mate: a top border, two content rows and a
            // closing rule each, whatever the width.
            assert_eq!(
                tree.iter().filter(|row| row.contains('╭')).count(),
                3,
                "not every mate drew a card at {width} columns:\n{screen}"
            );

            let area = Rect::new(0, 0, width, 40);
            let fold = row_fold_width(&app, workspace_list_rect(area));
            let body = workspace_list_body_rect(&app, workspace_list_rect(area), false);
            let entries = workspace_list_entries(&app);
            let agents = sidebar_agent_entries(&app);
            let bodies = body_register::BodyRegister::resolve(&app);
            for (entry_idx, entry) in entries.iter().enumerate() {
                if !matches!(entry, WorkspaceListEntry::Workspace { .. }) {
                    continue;
                }
                assert_eq!(
                    list_entry_height(
                        &app,
                        &agents,
                        &entries,
                        entry_idx,
                        body.height,
                        fold,
                        &bodies
                    ),
                    4,
                    "a card folded its two content rows at {width} columns:\n{screen}"
                );
            }

            let title = tree
                .iter()
                .find(|row| row.contains("firstmate"))
                .expect("the first mate has a card");
            assert!(
                !title.contains(FOLDABLE_BRANCH),
                "the subtitle merged onto the title row at {width} columns:\n{screen}"
            );
        }
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

    /// Puts `working` panes into the working state and `blocked` panes into the
    /// blocked state, one per pane, so a test can say how big each count is
    /// without assembling terminals by hand.
    fn drive_pane_states(app: &mut crate::app::state::AppState, working: usize, blocked: usize) {
        let ids: Vec<_> = app.terminals.keys().cloned().collect();
        assert!(
            ids.len() >= working + blocked,
            "the fleet has {} panes but {} were asked for",
            ids.len(),
            working + blocked
        );
        for (index, id) in ids.into_iter().take(working + blocked).enumerate() {
            app.terminals.get_mut(&id).expect("terminal exists").state = if index < working {
                crate::detect::AgentState::Working
            } else {
                crate::detect::AgentState::Blocked
            };
        }
    }

    /// Renders a sidebar with the fleet pulse switched on and returns the drawn
    /// rows.
    ///
    /// Every reading is driven through the state the tree below it already
    /// reads — detected agent state for the counts, a workspace metadata token
    /// for the quota — so the test drives a real fleet rather than reaching
    /// into the row.
    fn fleet_pulse_rows(
        width: u16,
        working: usize,
        blocked: usize,
        quota: Option<&str>,
    ) -> Vec<String> {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (0..(working + blocked).max(1))
            .map(|index| Workspace::test_new(&format!("ws{index}")))
            .collect();
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_notifications.enabled = true;

        if let Some(quota) = quota {
            app.workspaces[0].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    crate::quota::SESSION_TOKEN.to_string(),
                    Some(quota.to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );
        }

        drive_pane_states(&mut app, working, blocked);

        // The app loop is what publishes the live set; a render-only test has
        // to stand in for it or nothing would ever be mounted.
        let now = std::time::Instant::now();
        let lifecycle = app.sidebar_notifications.lifecycle();
        let live: Vec<_> = crate::app::fleet_signals::FleetSignals::resolve(&app)
            .animation_membership()
            .collect();
        app.anim
            .observe(now, crate::anim::Family::Named, &lifecycle, live);
        // Past the arrival, so an alerting reading is drawn in its steady state
        // rather than mid-fade.
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

    /// The whole point of the row. It used to draw the same eight signals the
    /// tray draws, in the same order, and a second copy of a readout is not a
    /// readout. Nothing the tray already says may appear here.
    #[test]
    fn the_pulse_row_says_nothing_the_tray_below_it_already_says() {
        let rows = fleet_pulse_rows(60, 3, 1, Some("62"));
        let header = &rows[0];

        assert!(
            header.contains("3 running"),
            "the running count is missing:\n{header}"
        );
        assert!(
            header.contains("1 needs you"),
            "the waiting count is missing:\n{header}"
        );
        assert!(
            header.contains("quota 62%"),
            "the quota reading is missing:\n{header}"
        );

        for signal in crate::app::fleet_signals::FleetSignal::ALL {
            assert!(
                !header.contains(signal.mark()),
                "{signal:?} still draws the tray's mark on the header row:\n{header}"
            );
            assert!(
                !header.contains(signal.name()),
                "{signal:?} still draws the tray's name on the header row:\n{header}"
            );
        }
    }

    /// A count, not a lamp. Two blocked panes light exactly one tray badge, so
    /// this is information the tray structurally cannot carry.
    #[test]
    fn the_row_counts_panes_where_the_tray_can_only_light_a_kind() {
        let one = fleet_pulse_rows(60, 0, 1, None);
        let two = fleet_pulse_rows(60, 0, 2, None);

        assert!(one[0].contains("1 needs you"), "{}", one[0]);
        assert!(two[0].contains("2 needs you"), "{}", two[0]);

        // The tray's own reading of the same two fleets is identical: `ask` is
        // lit either way.
        for panes in [1, 2] {
            let mut app = crate::app::state::AppState::test_new();
            app.workspaces = (0..panes)
                .map(|index| Workspace::test_new(&format!("ws{index}")))
                .collect();
            app.ensure_test_terminals();
            drive_pane_states(&mut app, 0, panes);
            assert!(
                crate::app::fleet_signals::FleetSignals::resolve(&app)
                    .is_live(crate::app::fleet_signals::FleetSignal::Ask),
                "the tray reads {panes} blocked panes as nothing at all"
            );
        }
    }

    /// A reading nobody published is absent rather than zero, and the counts
    /// are still stated on a fleet where nothing is happening — "all clear" is
    /// a fact worth a row.
    #[test]
    fn a_quiet_fleet_still_states_its_counts_and_omits_an_unpublished_quota() {
        let rows = fleet_pulse_rows(60, 0, 0, None);
        assert!(rows[0].contains("0 running"), "{}", rows[0]);
        assert!(rows[0].contains("0 needs you"), "{}", rows[0]);
        assert!(
            !rows[0].contains("quota"),
            "an unpublished quota drew anyway:\n{}",
            rows[0]
        );
    }

    /// A real 26-column sidebar - the default width - still says all three
    /// numbers, in the compact wording.
    #[test]
    fn the_default_sidebar_width_holds_the_whole_pulse() {
        let rows = fleet_pulse_rows(26, 3, 1, Some("62"));
        let header = &rows[0];
        assert!(header.contains("3 run"), "{header}");
        assert!(header.contains("1 you"), "{header}");
        assert!(header.contains("62%"), "{header}");
    }

    /// A sidebar too narrow for any wording still states all three numbers.
    /// The row this replaced survived to eight columns, and giving that up
    /// would make the change a narrowing as well as a rewrite.
    #[test]
    fn a_narrow_sidebar_keeps_every_number_and_drops_the_words() {
        let rows = fleet_pulse_rows(18, 3, 1, Some("62"));
        let header = &rows[0];
        assert!(
            header.starts_with("3·1·62%"),
            "the numbers did not survive an 18-column sidebar:\n{header}"
        );
        assert!(
            !header.contains("run") && !header.contains("you"),
            "a word drew at 18 columns:\n{header}"
        );
    }

    /// Off by default: an unconfigured Herdr draws the header row exactly as it
    /// did before the row existed.
    #[test]
    fn the_pulse_is_not_drawn_until_it_is_configured_on() {
        let without = mate_fleet_sidebar_rows(26, None);
        assert!(
            without[0].chars().all(|ch| ch.is_whitespace() || ch == '│'),
            "something drew on the header row of an unconfigured Herdr:\n{}",
            without.join("\n")
        );
    }

    /// The pulse takes the left of the row and the status keeps the right, so
    /// turning the pulse on never writes over a published status.
    #[test]
    fn the_pulse_and_the_session_status_share_the_header_row_without_overlapping() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_notifications.enabled = true;
        app.session_status = Some("weekly".to_string());

        let area = Rect::new(0, 0, 40, 12);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let row = row_text(terminal.backend().buffer(), 0, area.width);

        assert!(
            row.starts_with('0'),
            "the pulse is not hung on the left: {row}"
        );
        assert!(
            row.trim_end_matches('│').ends_with("weekly"),
            "the status is not right-aligned beside the pulse: {row}"
        );

        // Both measured in columns: the pulse's last reading has to end before
        // the status begins, or one has been drawn over the other.
        let columns: Vec<char> = row.chars().collect();
        let pulse_end = row
            .find("needs you")
            .map(|byte| row[..byte].chars().count() + "needs you".chars().count())
            .expect("the pulse's last reading is missing");
        let status_start = columns
            .windows(6)
            .position(|window| window.iter().collect::<String>() == "weekly")
            .expect("the status is missing");
        assert!(
            status_start >= pulse_end,
            "the status overlapped the pulse: {row}"
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
    ///
    /// `worker` is the *newest* of the two rank-2 panes on purpose. A card
    /// enters at the head of its parent's group ([`enter_at_head`]), so making
    /// the clamped child's parent the newer one is what puts a true sibling
    /// *below* the clamped row — which is the whole shape under test. Give
    /// `sub` to the older pane instead and the clamped row is simply last, and
    /// closing the column would be correct.
    fn capped_fleet_sidebar_rows(width: u16) -> Vec<String> {
        let mut app = crate::app::state::AppState::test_new();
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let sub_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        let worker_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        let worker2_pane = *second_mate.tabs[0]
            .panes
            .keys()
            .find(|pane| **pane != sub_pane && **pane != worker_pane)
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
    fn expanded_and_collapsed_sidebars_use_custom_background() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        app.active = None;
        app.palette.sidebar_bg = ratatui::style::Color::Rgb(12, 34, 56);
        app.refresh_sidebar_palette();
        let area = Rect::new(0, 0, 26, 20);

        let mut expanded = Terminal::new(TestBackend::new(26, 20)).unwrap();
        expanded
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert!(expanded
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == app.palette.sidebar_bg));

        let mut collapsed = Terminal::new(TestBackend::new(26, 20)).unwrap();
        collapsed
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .unwrap();
        assert!(collapsed
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == app.palette.sidebar_bg));
    }

    /// What an animation composites against is the colour the panel is filled
    /// with, which is `sidebar_bg` and not `panel_bg`.
    ///
    /// A dissolving cell and an animated notification slot both fade toward the
    /// ground, so a ground nowhere on screen inside the panel is a visible
    /// wrong blend. With no theme override the fill is `Color::Reset` — inherit
    /// the host — and the order behind it is exactly the one the pixel cards
    /// already float on.
    #[test]
    fn animated_sidebar_ink_composites_against_the_panels_own_fill() {
        use crate::anim::cell::InkPalette;

        let mut app = crate::app::state::AppState::test_new();
        app.palette.panel_bg = ratatui::style::Color::Rgb(30, 30, 46);

        // No fill and no measured host background: the panel background is all
        // there is, which is what this path resolved to before.
        assert_eq!(backdrop_rgb(&app), Some((30, 30, 46)));

        // A host that answered OSC 11 is the ground, because `Color::Reset`
        // means the panel is showing the host's own background.
        app.host_terminal_theme = app.host_terminal_theme.with_color(
            crate::terminal_theme::DefaultColorKind::Background,
            crate::terminal_theme::RgbColor {
                r: 239,
                g: 241,
                b: 245,
            },
        );
        assert_eq!(backdrop_rgb(&app), Some((239, 241, 245)));

        // A theme that paints the panel wins over both: that fill is the pixel
        // every cell in the panel is drawn on.
        app.palette.sidebar_bg = ratatui::style::Color::Rgb(12, 34, 56);
        app.refresh_sidebar_palette();
        assert_eq!(backdrop_rgb(&app), Some((12, 34, 56)));

        // And that is the surface the engine resolves, for a span with no
        // background of its own.
        let ink = InkPalette::resolve(
            Style::default().fg(app.sidebar_palette.text),
            backdrop_rgb(&app),
            &app.palette,
            &app.host_terminal_theme,
        );
        assert_eq!(ink.surface, (12, 34, 56));

        // A span that names its own background still keeps it — a selected
        // row's highlight is what its own text sits on.
        let highlighted = InkPalette::resolve(
            Style::default().bg(ratatui::style::Color::Rgb(7, 7, 7)),
            backdrop_rgb(&app),
            &app.palette,
            &app.host_terminal_theme,
        );
        assert_eq!(highlighted.surface, (7, 7, 7));
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
        // The pulse loops over `DEFAULT_PERIOD`, so its trough is half of that
        // in. Stated as a literal because that is what the assertions below are
        // about — a pulse whose period moved is a pulse whose trough moved with
        // it, and the two have to be read off the same number.
        const HALF_CYCLE_MS: u64 = 1_120;
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
        let body = workspace_list_body_rect(&app, workspace_area, false);

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
            None,
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
            worktree_child: false,
            card_frame: None,
            motion_cells: (0, 0),
            arriving: false,
            drawn_card: true,
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
        assert_eq!(buffer[(cards[1].rect.x + 1, cards[1].rect.y)].symbol(), "├");
        assert_eq!(buffer[(cards[2].rect.x + 1, cards[2].rect.y)].symbol(), "└");
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
                .accept(
                    "firstmate",
                    None,
                    kind,
                    crate::app::relation_signal::CarrierId::Workspace(carrier),
                    None,
                    now,
                )
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
        // Three connector cells sit after the depth-1 rail's single column,
        // and the state icon is the first token drawn after them.
        let first = child.x + 1;
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

    /// A mate's Space owning one worker pane, with the worker's own row wired
    /// up as a relation-signal carrier — the mate->worker connector the whole
    /// feature exists for. Mirrors [`render_signalled_tree`], but the carrier
    /// is the worker's pane id rather than a workspace id.
    fn render_signalled_worker_tree(
        kind: Option<crate::app::relation_signal::RelationSignalKind>,
        position: u16,
    ) -> (ratatui::buffer::Buffer, AppState) {
        let mut app = AppState::test_new();
        let mut mate = Workspace::test_new("firstmate");
        let worker_pane = mate.test_split(ratatui::layout::Direction::Vertical);
        let worker_public_id = crate::workspace::public_pane_id_for_number(
            &mate.id,
            mate.public_pane_number(worker_pane)
                .expect("split pane has a public number"),
        );
        app.workspaces = vec![mate];
        app.ensure_test_terminals();
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        // `test_split` is a raw layout split, so it stamps no creation origin
        // — unlike the real `pane.split` API path, it does not read as
        // delegated-in-space. An explicit `owner` token, published the same
        // way a real fleet publishes one, is what puts the worker on its
        // mate's row.
        let worker_terminal = app.workspaces[0].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&worker_terminal).unwrap();
        terminal.set_agent_name("worker".to_string());
        terminal.state = AgentState::Idle;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            std::time::Instant::now(),
        );

        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);

        if let Some(kind) = kind {
            let now = std::time::Instant::now();
            app.relation_signals
                .accept(
                    "firstmate",
                    None,
                    kind,
                    crate::app::relation_signal::CarrierId::Pane(worker_public_id),
                    None,
                    now,
                )
                .expect("a fresh row always accepts its first signal");
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

    #[test]
    fn a_pane_carrier_renders_the_charge_on_the_workers_own_connector() {
        // The case the whole carrier extension exists for: workers are panes,
        // so the mate->worker connector has to carry a signal, not only the
        // Space-level connector `render_signalled_tree` already covers.
        use crate::app::relation_signal::{RelationSignalKind, SIGNAL_POSITIONS};

        let mut frames = vec![render_signalled_worker_tree(None, 0).0];
        for position in 0..SIGNAL_POSITIONS {
            frames
                .push(render_signalled_worker_tree(Some(RelationSignalKind::Transfer), position).0);
        }
        frames.push(render_signalled_worker_tree(None, 0).0);

        let (_, app) = render_signalled_worker_tree(None, 0);
        let worker_card = app
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.agent.is_some())
            .expect("the worker's own row is laid out");
        let child = worker_card.rect;

        let mut moved = 0;
        for pair in frames.windows(2) {
            let changed = changed_cells(&pair[0], &pair[1]);
            for (x, y) in &changed {
                assert_eq!(
                    *y, child.y,
                    "a signal on the worker's pane must not touch any row but the worker's own"
                );
                let _ = x;
            }
            moved += changed.len();
        }
        assert!(
            moved > 0,
            "the worker's own connector has to actually draw the crackle"
        );
    }

    #[test]
    fn a_pane_carrier_does_not_light_a_workspace_row_with_the_same_number() {
        // Regression guard: a pane carrier and a workspace row are different
        // rows even when nested one under the other. Confirms the render
        // path reads `pane_relation_signal_phase`, not `workspace_relation_signal_phase`,
        // for the mate's own Space row.
        let (unsignalled, _) = render_signalled_worker_tree(None, 0);
        let (signalled, app) = render_signalled_worker_tree(
            Some(crate::app::relation_signal::RelationSignalKind::Transfer),
            0,
        );
        let mate_card = app
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.agent.is_none())
            .expect("the mate's own Space row is laid out");
        for x in mate_card.rect.x..mate_card.rect.x + mate_card.rect.width {
            assert_eq!(
                unsignalled[(x, mate_card.rect.y)].symbol(),
                signalled[(x, mate_card.rect.y)].symbol(),
                "a worker's signal must not draw on its mate's own Space row"
            );
            assert_eq!(
                unsignalled[(x, mate_card.rect.y)].style(),
                signalled[(x, mate_card.rect.y)].style(),
                "a worker's signal must not draw on its mate's own Space row"
            );
        }
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
        let connector = child.x + 1;
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
                    peak_cell(&buffer, (child.x + 1, child.y), settled)
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

        // The cell the charge has taken furthest from the line's own ink, rather
        // than the brightest one. A stage's hue is placed at the distance from
        // the panel its *severity* asks for, so a quiet signal can legitimately
        // sit no brighter than the connector it runs along — brightness stopped
        // being a proxy for "most charged" when the two channels split apart.
        let settled = {
            let (buffer, _) = render_signalled_tree(None, 0);
            buffer[(child.x + 1, child.y)]
                .style()
                .fg
                .and_then(crate::ui::color::color_to_rgb)
        };

        let mut inks = Vec::new();
        for kind in EVERY_SIGNAL_KIND {
            // Half-way along, where every kind has its charge on the connector.
            let (buffer, _) = render_signalled_tree(Some(kind), 14);
            let charged = (0..u16::from(CONNECTOR_CELLS))
                .map(|cell| buffer[(child.x + 1 + cell, child.y)].style().fg)
                .max_by_key(|fg| {
                    let Some((r, g, b)) = fg.and_then(crate::ui::color::color_to_rgb) else {
                        return 0;
                    };
                    let Some((br, bg, bb)) = settled else {
                        return 0;
                    };
                    u32::from(r.abs_diff(br))
                        + u32::from(g.abs_diff(bg))
                        + u32::from(b.abs_diff(bb))
                })
                .flatten()
                .unwrap_or_else(|| panic!("{kind:?} lit no connector cell"));
            inks.push((kind, charged));
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
                let icon = &buffer[(child.x + 1 + u16::from(CONNECTOR_CELLS), child.y)];

                let workspace = &app.workspaces[1];
                let (state, seen) = workspace.aggregate_state(&app.terminals);
                let (expected_symbol, expected_style) =
                    state_icon(state, seen, app.status_indicators, &app.palette);
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
        // Both linked worktrees render as children of that checkout, on the
        // ownership tree's own depth-1 column.
        assert_eq!(buffer[(cards[1].rect.x + 1, cards[1].rect.y)].symbol(), "├");
        assert_eq!(buffer[(cards[2].rect.x + 1, cards[2].rect.y)].symbol(), "└");
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
            terminal.backend().buffer()[(child.rect.x + 1, child.rect.y)].symbol(),
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
        assert!(!cards[0].worktree_child);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].worktree_child);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height);
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
            spacious[0].rect.y + spacious[0].rect.height
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
        assert_eq!(spacious_metrics.viewport_rows, 3);
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
                    ws_idx,
                    worktree_child,
                    ..
                } => Some((*ws_idx, *worktree_child)),
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
        assert!(!cards[2].worktree_child);
        assert_eq!(buffer[(cards[1].rect.x + 1, cards[1].rect.y)].symbol(), "└");
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
        // The configured half rather than a literal: this test is about the
        // deadline not *moving*, and pinning the number here would make every
        // change to how long a switch takes read as a broken transition.
        let half = app
            .sidebar_animation
            .view_switch_stage()
            .expect("the switch is on out of the box")
            .duration;
        assert_eq!(
            app.next_tree_view_commit_deadline(),
            Some(settled + half),
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

    /// A first mate owning one worker, for the command-acknowledgement render
    /// tests: `summary_fleet`'s shape without the summary, and a plain
    /// `PaneId` handle back so a test can drive `sidebar_cmd_acks` directly
    /// against the exact row it renders.
    fn single_worker_fleet() -> (crate::app::state::AppState, crate::layout::PaneId) {
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
        terminal.state = AgentState::Working;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([(
                "owner".to_string(),
                Some("2ndmate-explore".to_string()),
            )]),
            None,
            now,
        );
        (app, worker_pane)
    }

    /// Renders `app` at the sidebar width the captain runs — see
    /// `ownership_is_drawn_as_written::CAPTAIN_SIDEBAR_WIDTH`, the same 42
    /// columns, kept as a separate literal here so this module does not reach
    /// into a sibling test module for a constant that is really just "how
    /// wide the captain's panel is" — and returns the drawn buffer.
    fn render_cmd_ack_fleet(app: &mut crate::app::state::AppState) -> ratatui::buffer::Buffer {
        const WIDTH: u16 = 42;
        let area = Rect::new(0, 0, WIDTH, 20);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(app, area);
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn find_all_symbol_x(
        buffer: &ratatui::buffer::Buffer,
        row: u16,
        width: u16,
        symbol: &str,
    ) -> Vec<u16> {
        (0..width)
            .filter(|&x| buffer[(x, row)].symbol() == symbol)
            .collect()
    }

    fn worker_row(buffer: &ratatui::buffer::Buffer, width: u16) -> u16 {
        (0..20)
            .find(|&y| row_text(buffer, y, width).contains("worker"))
            .unwrap_or_else(|| {
                panic!(
                    "no worker row on screen:\n{}",
                    (0..20)
                        .map(|y| row_text(buffer, y, width))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
    }

    /// Drives the same fold `App::advance_animations` uses for command acks —
    /// `CmdAcks::observe` into `Animator::observe` — without pulling in the
    /// full app-loop tick, so the test controls the wall clock exactly.
    fn advance_cmd_acks(
        app: &mut crate::app::state::AppState,
        pane_id: crate::layout::PaneId,
        now: std::time::Instant,
    ) {
        let mount = crate::anim::behaviour::CMD_ACK_MOUNT_PERIOD;
        let hold = crate::anim::behaviour::CMD_ACK_HOLD_PERIOD;
        let dismount = crate::anim::behaviour::CMD_ACK_DISMOUNT_PERIOD;
        let active_window = mount + hold;
        let retain_window = active_window + dismount;
        let lifecycle = crate::app::cmd_ack::CmdAcks::lifecycle(mount, dismount);
        let row = crate::anim::CardRow::Agent(pane_id);
        let members = app
            .sidebar_cmd_acks
            .observe(now, active_window, retain_window, [row]);
        app.anim
            .observe(now, crate::anim::Family::CmdAck, &lifecycle, members);
    }

    /// The end-to-end proof this task asked for: a real render, at the
    /// sidebar width the captain runs, of two commands acknowledged apart in
    /// time — showing they are two genuinely independent settle clocks rather
    /// than one marker restarting, and rather than a coalesced "×2" counter.
    #[test]
    fn two_commands_recorded_apart_animate_as_two_independent_instances() {
        let (mut app, pane_id) = single_worker_fleet();
        let row = crate::anim::CardRow::Agent(pane_id);
        let glyph = CMD_ACK_GLYPH.to_string();
        let t0 = std::time::Instant::now();
        let mount = crate::anim::behaviour::CMD_ACK_MOUNT_PERIOD;
        let window = mount + crate::anim::behaviour::CMD_ACK_HOLD_PERIOD;

        app.sidebar_cmd_acks.record(row.clone(), t0);
        advance_cmd_acks(&mut app, pane_id, t0);

        // Snapshot 1, mid-mount: only the first command has run.
        let mid_mount = t0 + mount / 2;
        advance_cmd_acks(&mut app, pane_id, mid_mount);
        let buffer1 = render_cmd_ack_fleet(&mut app);
        let y = worker_row(&buffer1, 42);
        let xs1 = find_all_symbol_x(&buffer1, y, 42, &glyph);
        assert_eq!(
            xs1.len(),
            1,
            "only one command has run so far:\n{}",
            row_text(&buffer1, y, 42)
        );

        // The second command runs 600ms later — long after the first has
        // settled into its hold — so the two are caught at visibly different
        // points in their own settle at the same wall-clock instant.
        let t1 = t0 + std::time::Duration::from_millis(600);
        app.sidebar_cmd_acks.record(row.clone(), t1);
        advance_cmd_acks(&mut app, pane_id, t1);
        let buffer2 = render_cmd_ack_fleet(&mut app);
        let xs2 = find_all_symbol_x(&buffer2, y, 42, &glyph);
        assert_eq!(
            xs2.len(),
            2,
            "a burst of two commands is two markers, not a coalesced counter:\n{}",
            row_text(&buffer2, y, 42)
        );
        let fg = |buffer: &ratatui::buffer::Buffer, x: u16| buffer[(x, y)].fg;
        let a_at_t1 = fg(&buffer2, xs2[0]);
        let b_at_t1 = fg(&buffer2, xs2[1]);
        assert_ne!(
            a_at_t1,
            b_at_t1,
            "the settled first marker and the still-snapping-in second marker \
             must not read as the same frame:\n{}",
            row_text(&buffer2, y, 42)
        );

        // The first marker's window has now closed and it starts fading out
        // on its own schedule; the second, meanwhile, has had time to finish
        // its own mount and settle into its hold. Reaching the boundary is
        // its own pass — it is what starts the dismount clock — and only a
        // further pass after that lets the fade actually progress before
        // this snapshot.
        advance_cmd_acks(&mut app, pane_id, t0 + window);
        let t2 = t0 + window + std::time::Duration::from_millis(70);
        advance_cmd_acks(&mut app, pane_id, t2);
        let buffer3 = render_cmd_ack_fleet(&mut app);
        let xs3 = find_all_symbol_x(&buffer3, y, 42, &glyph);
        assert_eq!(
            xs3.len(),
            2,
            "the second marker must still be live while the first fades:\n{}",
            row_text(&buffer3, y, 42)
        );
        let a_at_t2 = fg(&buffer3, xs3[0]);
        let b_at_t2 = fg(&buffer3, xs3[1]);
        assert_ne!(
            a_at_t1, a_at_t2,
            "the first marker's own colour must have moved as it faded — a \
             static marker is not an animation"
        );
        assert_ne!(
            b_at_t1, b_at_t2,
            "the second marker's own colour must have moved too, as it snapped \
             in from where it started at t1 to settled at t2 — each marker \
             ticks its own clock, not a shared one"
        );
        assert_eq!(
            a_at_t1, b_at_t2,
            "a marker fully settled into its hold reads the same regardless \
             of which instance it is — the first at t1, the second at t2"
        );
    }
}

/// The tree draws ownership: where a line meets a card, where a card hangs, and
/// how wide it is drawn.
///
/// One module because the three are one structure. The captain reported the
/// alignment as two findings — *"tree trunk not aligned with firstmate/workers.
/// branches not aligned with secondmates"* — and settled who hangs where in the
/// same breath: a worker the first mate opens is tied to a second mate if one
/// fits, and keeps the first mate if none does.
///
/// # What a worker is drawn as, and what it is no longer drawn as
///
/// The captain's first answer was a card of its own — *"sub agent size card,
/// not secondmate or first mate ssize"*, *"have it create the card as a sub
/// agent as a branch under it"* — and he replaced it after seeing both drawn:
/// a worker belongs **inside the card of the mate that dispatched it**, and
/// *"2nd mates' workers will function just like firstmate's workers, only they
/// show up in their respective 2nd [mate's card]"*. So the rank ladder is still
/// three rungs and still decides how wide a box is, but a worker only ever
/// opens one where its own mate's box is not on screen. See
/// [`crew_folds_into_its_space`].
///
/// A mate is untouched by that. A second mate is still its own card, still
/// branch-nested under whoever owns it, at its own rung of the ladder.
///
/// Everything here is asserted through the flattened tree or the geometry the
/// renderer is handed, never against the source of a glyph.
#[cfg(test)]
mod ownership_is_drawn_as_written {
    use super::tests::{drawn_tree_rows, interleaved_worker_fleet, rows_under};
    use super::*;
    use crate::app::agent_tree::AgentRelation;
    use crate::workspace::Workspace;

    /// The width the captain runs, and the fold width it produces.
    const CAPTAIN_SIDEBAR_WIDTH: u16 = 42;

    fn fold_width_at(sidebar_width: u16) -> u16 {
        workspace_list_body_width(
            workspace_list_rect(Rect::new(0, 0, sidebar_width, 40)),
            true,
        )
    }

    /// The tree the captain's own fleet has: a first mate, two second mates
    /// under it, and workers under each mate.
    pub(super) fn mate_fleet() -> crate::app::state::AppState {
        interleaved_worker_fleet()
    }

    /// Where each row's **own** box would start and end, by name, on a panel
    /// this wide: the rank ladder, asked of the one function that decides it.
    ///
    /// A worker is drawn inside its own mate's box on a resting panel and is
    /// handed no frame at all there — see [`drawn_frames`], which is what the
    /// layout actually produces. What this measures is the box a row of that
    /// depth and rank *opens*, which is still the ladder every card is placed
    /// on and is still what a worker gets when its mate's box is off screen.
    fn card_frames(
        app: &crate::app::state::AppState,
        sidebar_width: u16,
    ) -> Vec<(String, Rect, u8, AgentRelation)> {
        let agents = sidebar_agent_entries(app);
        let fold = fold_width_at(sidebar_width);
        let row = Rect::new(0, 0, fold, 4);
        workspace_list_entries_expanded(app)
            .into_iter()
            .filter_map(|entry| {
                let name = match &entry {
                    WorkspaceListEntry::Workspace { ws_idx, .. } => space_tree_name(app, *ws_idx),
                    WorkspaceListEntry::Agent { entry_idx, .. } => agents
                        .get(*entry_idx)
                        .and_then(|agent| agent.agent_name.clone()),
                }?;
                let frame = card_frame_for(row, &entry, fold)?;
                Some((name, frame, entry.depth(), entry.rank()))
            })
            .collect()
    }

    fn frame_of(
        app: &crate::app::state::AppState,
        sidebar_width: u16,
        name: &str,
    ) -> (Rect, u8, AgentRelation) {
        card_frames(app, sidebar_width)
            .into_iter()
            .find(|(row, ..)| row == name)
            .map(|(_, frame, depth, rank)| (frame, depth, rank))
            .unwrap_or_else(|| panic!("{name} draws no card"))
    }

    /// What the **layout** hands each row, by name: the box it draws, or `None`
    /// where it opens none because it is drawn inside somebody else's.
    ///
    /// [`card_frames`] answers the ladder; this answers the panel. They differ
    /// on exactly one kind of row, and that difference is the captain's rule.
    fn drawn_frames(
        app: &crate::app::state::AppState,
        sidebar_width: u16,
    ) -> Vec<(String, Option<Rect>)> {
        let area = Rect::new(0, 0, sidebar_width, 60);
        let agents = sidebar_agent_entries(app);
        let entries = workspace_list_entries(app);
        compute_workspace_card_areas(app, area)
            .into_iter()
            .filter_map(|card| {
                let name = match entries.get(card.entry_idx)? {
                    WorkspaceListEntry::Workspace { ws_idx, .. } => space_tree_name(app, *ws_idx),
                    WorkspaceListEntry::Agent { entry_idx, .. } => agents
                        .get(*entry_idx)
                        .and_then(|agent| agent.agent_name.clone()),
                }?;
                Some((name, card.card_frame))
            })
            .collect()
    }

    /// The box the panel drew for `name`, and `None` when it drew none.
    fn drawn_frame_of(
        app: &crate::app::state::AppState,
        sidebar_width: u16,
        name: &str,
    ) -> Option<Rect> {
        drawn_frames(app, sidebar_width)
            .into_iter()
            .find(|(row, _)| row == name)
            .map(|(_, frame)| frame)
            .unwrap_or_else(|| panic!("{name} drew no row at all"))
    }

    /// The row index of `name` in the drawn tree.
    fn drawn_row_of(app: &crate::app::state::AppState, sidebar_width: u16, name: &str) -> usize {
        drawn_frames(app, sidebar_width)
            .into_iter()
            .position(|(row, _)| row == name)
            .unwrap_or_else(|| panic!("{name} drew no row at all"))
    }

    // ---------------------------------------------------------------- alignment

    /// The column a row of this depth puts its `├`/`└` in, read off the prefix
    /// the renderer actually draws rather than recomputed from the maths the
    /// prefix is measured with.
    fn connector_column(depth: u8, ancestors: &[bool], fold_width: u16) -> u16 {
        let p = Palette::catppuccin();
        let (spans, _) =
            agent_row_prefix(depth, false, ancestors, 0, &p, None, true, None, fold_width);
        let mut column = 0u16;
        for span in &spans {
            for glyph in span.content.chars() {
                if glyph == '\u{251c}' || glyph == '\u{2514}' {
                    return column;
                }
                column += 1;
            }
        }
        panic!("depth {depth} drew no connector at all");
    }

    /// **The alignment contract, stated once.** A child's connector lands in the
    /// column its parent's card border stands in — that is what "the trunk is
    /// aligned with the first mate" and "the branches are aligned with the
    /// second mates" both mean, and they are the same fact asked at two levels.
    ///
    /// Asserted across the two functions that have to agree — the prefix the
    /// renderer draws and the frame [`card_frame_for`] hands the card — because
    /// a tree whose lines miss its cards is exactly those two disagreeing.
    ///
    /// Every row here still answers it, worker rows included, because the
    /// question is about the ladder rather than about what the panel drew: a
    /// worker only *uses* its connector where it stands outside a box, and it
    /// has to land in the right column when it does. Where it does not is
    /// [`a_worker_hangs_off_nothing_inside_the_box_that_holds_it`].
    #[test]
    fn every_branch_starts_in_the_column_its_parents_border_stands_in() {
        let app = mate_fleet();
        let frames = card_frames(&app, CAPTAIN_SIDEBAR_WIDTH);
        assert!(frames.len() >= 6, "the fixture lost rows: {frames:?}");
        let fold = fold_width_at(CAPTAIN_SIDEBAR_WIDTH);

        let entries = workspace_list_entries_expanded(&app);
        // The card a row of this depth hangs off is the nearest row above it one
        // level shallower, which is exactly how the walk emits a subtree.
        for (index, (name, _, depth, _)) in frames.iter().enumerate() {
            if *depth == 0 {
                continue;
            }
            let parent = frames[..index]
                .iter()
                .rev()
                .find(|(_, _, parent_depth, _)| *parent_depth < *depth)
                .unwrap_or_else(|| panic!("{name} has no parent above it"));
            let ancestors = entries[index].ancestors_continue();
            assert_eq!(
                connector_column(*depth, ancestors, fold),
                parent.1.x,
                "{name}'s connector column is not the column {}'s border stands in",
                parent.0
            );
        }
    }

    /// **A worker draws no branch at all, because the box it is in is the
    /// relation.**
    ///
    /// The other half of the alignment contract, and the half the captain's new
    /// rule created: a `├─` beside a worker would be a second, weaker statement
    /// of what its mate's border already says, drawn in the gutter of a card it
    /// is not outside of. Read off the panel the captain runs.
    #[test]
    fn a_worker_hangs_off_nothing_inside_the_box_that_holds_it() {
        let app = mate_fleet();
        let rows = drawn_panel(mate_fleet(), 40);
        let screen = rows.join("\n");
        for (name, frame) in drawn_frames(&app, CAPTAIN_SIDEBAR_WIDTH) {
            let mate = matches!(name.as_str(), "firstmate" | "2ndmate-a" | "2ndmate-b");
            assert_eq!(
                frame.is_some(),
                mate,
                "{name} {} a box of its own",
                if mate { "lost" } else { "opened" }
            );
            if mate {
                continue;
            }
            let row = rows
                .iter()
                .find(|row| row.contains(&name))
                .unwrap_or_else(|| panic!("{name} drew no row:\n{screen}"));
            assert!(
                !row.contains('\u{251c}') && !row.contains('\u{2514}'),
                "{name} grew a connector inside the box that already holds it:\n{screen}"
            );
        }
    }

    /// The trunk itself: the first mate's card border stands in the column every
    /// row hanging off it points at.
    #[test]
    fn the_trunk_stands_in_the_first_mates_own_border_column() {
        let app = mate_fleet();
        let (first_mate, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "firstmate");
        assert_eq!(
            first_mate.x,
            connector_column(1, &[false], fold_width_at(CAPTAIN_SIDEBAR_WIDTH)),
            "the trunk and the first mate's border are in different columns"
        );
    }

    /// The connector reaches the card instead of stopping a column short of it.
    ///
    /// Read off the drawn buffer, at the narrow end where the shell is a line
    /// and at the captain's width where it is a card, because the joint is the
    /// one part of the prefix that differs between the two: against a card the
    /// third cell carries the line, against a bare name it stays a space.
    #[test]
    fn a_branch_meets_the_card_it_points_at_and_keeps_its_gap_from_a_name() {
        let p = Palette::catppuccin();
        let fold = fold_width_at(CAPTAIN_SIDEBAR_WIDTH);
        for is_last_child in [true, false] {
            let (card, card_cols) =
                agent_row_prefix(1, is_last_child, &[false], 0, &p, None, true, None, fold);
            let (line, line_cols) =
                agent_row_prefix(1, is_last_child, &[false], 0, &p, None, false, None, fold);
            let text = |spans: &[Span<'static>]| -> String {
                spans.iter().map(|span| span.content.to_string()).collect()
            };

            assert!(
                text(&card).ends_with("──"),
                "the branch stopped short of the card: {:?}",
                text(&card)
            );
            assert!(
                text(&line).ends_with("─ "),
                "the branch ran into the name: {:?}",
                text(&line)
            );
            // The joint changes a glyph and never a column, so the layout does
            // not have to know which shell the renderer picked.
            assert_eq!(card_cols, line_cols);
            assert_eq!(card_cols, tree_column(1, fold) as usize);
        }
    }

    // ---------------------------------------------------------------- parentage

    /// A worker the first mate opened, running in a second mate's Space, hangs
    /// off **that mate** — the captain's *"it should always be tied to a
    /// secondmate."*
    ///
    /// It publishes `owner: firstmate`, which is the strongest claim a fleet can
    /// make about provenance, and it is still the Space it runs in that decides
    /// where it hangs, because that is its scope.
    #[test]
    fn a_worker_the_first_mate_opened_hangs_off_the_second_mate_whose_scope_fits() {
        let mut app = mate_fleet();
        let pane = app.workspaces[1].test_split(ratatui::layout::Direction::Vertical);
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[1].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("the new pane has a terminal");
        terminal.set_agent_name("fm-opened".to_string());
        terminal.state = AgentState::Idle;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            std::time::Instant::now(),
        );

        let under_a = rows_under(&app, "2ndmate-a", 4);
        assert!(
            under_a.contains(&"fm-opened".to_string()),
            "the worker did not join the mate whose Space it runs in: {under_a:?}"
        );
        let (_, depth, rank) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "fm-opened");
        assert_eq!(depth, 2, "it did not hang at worker depth");
        assert_eq!(rank, AgentRelation::Worker);

        // And it is drawn in *that* mate's card, not in the card of the mate
        // whose name its token carries.
        let entries = workspace_list_entries(&app);
        assert_eq!(
            crew_head(
                &entries,
                drawn_row_of(&app, CAPTAIN_SIDEBAR_WIDTH, "fm-opened")
            ),
            Some(drawn_row_of(&app, CAPTAIN_SIDEBAR_WIDTH, "2ndmate-a")),
            "the worker's ink went to a card its scope does not put it in"
        );
    }

    /// **A second mate's own worker is in the second mate's card — the
    /// generalisation, stated on the row it was generalised for.**
    ///
    /// The captain settled the first mate's case first and then widened it in
    /// one sentence: *"2nd mates' workers will function just like firstmate's
    /// workers, only they show up in their respective 2nd [mate's card]"*. Two
    /// mates, each running workers of its own, so a rule that only ever found
    /// the first mate would show up here as a worker in the wrong box.
    ///
    /// The mate itself is untouched by the fold, which is the other half of what
    /// was asked: a second mate is still its own card, still branch-nested under
    /// the first mate, at its own rung of the ladder.
    #[test]
    fn a_second_mates_own_worker_lands_in_the_second_mates_card() {
        let app = mate_fleet();
        let entries = workspace_list_entries(&app);
        let row = |name: &str| drawn_row_of(&app, CAPTAIN_SIDEBAR_WIDTH, name);

        for (mate, workers) in [("2ndmate-a", ["a-one", "a-two", "a-three"].as_slice())] {
            for worker in workers {
                assert_eq!(
                    crew_head(&entries, row(worker)),
                    Some(row(mate)),
                    "{worker} is not drawn in {mate}'s own card"
                );
                assert_ne!(
                    crew_head(&entries, row(worker)),
                    Some(row("firstmate")),
                    "{worker} was folded into the first mate's card instead"
                );
                assert_eq!(
                    crew_tier(&entries, row(worker)),
                    Some(0),
                    "{worker} was stepped in as though it came through somebody"
                );
                assert_eq!(
                    drawn_frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, worker),
                    None,
                    "{worker} opened a box of its own inside {mate}'s"
                );
            }
        }
        for worker in ["b-one", "b-two"] {
            assert_eq!(
                crew_head(&entries, row(worker)),
                Some(row("2ndmate-b")),
                "{worker} left its own mate's card"
            );
        }

        // The mates themselves are unchanged: their own cards, nested under the
        // first mate, at a second mate's rung.
        for mate in ["2ndmate-a", "2ndmate-b"] {
            let (ladder, depth, rank) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, mate);
            assert_eq!(depth, 1, "{mate} stopped hanging off the first mate");
            assert_eq!(rank, AgentRelation::SecondMate);
            let drawn = drawn_frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, mate)
                .unwrap_or_else(|| panic!("{mate} lost its own box"));
            assert_eq!(
                (drawn.x, drawn.width),
                (ladder.x, ladder.width),
                "{mate}'s box left its own rung of the ladder"
            );
            // And it closes under its last worker rather than above the first.
            let last = drawn_frames(&app, CAPTAIN_SIDEBAR_WIDTH)
                .into_iter()
                .position(|(name, _)| name == mate)
                .expect("the mate drew a row");
            assert!(
                drawn.height
                    > compute_workspace_card_areas(
                        &app,
                        Rect::new(0, 0, CAPTAIN_SIDEBAR_WIDTH, 60)
                    )[last]
                        .rect
                        .height,
                "{mate}'s box did not grow over the workers inside it"
            );
        }
    }

    /// With no mate whose scope fits, the same worker stays under the first
    /// mate — and is drawn **inside the first mate's own card**.
    ///
    /// This is the captain's revision. His first answer was a card of its own,
    /// *"have it create the card as a sub agent as a branch under it"*, and this
    /// test used to pin exactly that: a narrower box, offset from the first
    /// mate's, ending where the other sub agents end. Having seen both drawn he
    /// replaced it — a worker belongs in the card of the mate that dispatched
    /// it — so what is pinned here now is the box it is *in* rather than the box
    /// it opens.
    ///
    /// It is still deliberately **not** pushed a level deeper. A row in the
    /// second mate column with no second mate above it belongs to nothing; what
    /// stops the promotion is its rank, which is also what its card is measured
    /// from on the one panel that still gives it one — a panel scrolled past its
    /// mate.
    #[test]
    fn a_worker_with_no_fitting_mate_is_drawn_inside_the_first_mates_card() {
        let mut app = mate_fleet();
        // In the first mate's *own* Space, so no second mate's scope holds it.
        let pane = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.ensure_test_terminals();
        let owner_id = app.workspaces[0].id.clone();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("the new pane has a terminal");
        terminal.set_agent_name("fm-direct".to_string());
        terminal.state = AgentState::Idle;
        terminal.created_by = Some(crate::api::schema::PaneOrigin {
            pane_id: "fm-direct".to_string(),
            workspace_id: owner_id,
        });

        let rows = drawn_tree_rows(&app);
        assert!(
            rows.contains(&"fm-direct".to_string()),
            "the worker lost its row entirely: {rows:?}"
        );
        assert!(
            rows_under(&app, "firstmate", rows.len()).contains(&"fm-direct".to_string()),
            "the worker left the first mate's subtree: {rows:?}"
        );

        let (frame, depth, rank) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "fm-direct");
        assert_eq!(depth, 1, "it hangs off the first mate, so it is one deep");
        assert_eq!(
            rank,
            AgentRelation::Worker,
            "being opened by the first mate promoted its rank"
        );

        // The rank it kept is still a rung of the ladder — never a peer of the
        // first mate, never a second mate's width — which is what it is drawn at
        // on a panel that has scrolled its mate off the top.
        let (mate, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "2ndmate-a");
        let (first_mate, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "firstmate");
        let (worker, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "a-one");
        assert!(
            frame.x > first_mate.x,
            "it drew as a peer of the first mate"
        );
        assert!(
            frame.width < mate.width,
            "a worker drew at second mate width: {} vs {}",
            frame.width,
            mate.width
        );
        assert_eq!(
            frame.x + frame.width,
            worker.x + worker.width,
            "it does not end where the other workers end"
        );

        // And on the panel itself it opens no box at all: it is a row inside the
        // first mate's, directly under it and above the mates nested below.
        assert_eq!(
            drawn_frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "fm-direct"),
            None,
            "it drew a box of its own inside the card it is in"
        );
        let entries = workspace_list_entries(&app);
        let fm_direct = entries
            .iter()
            .position(|entry| {
                matches!(entry, WorkspaceListEntry::Agent { entry_idx, .. }
                    if sidebar_agent_entries(&app)[*entry_idx].agent_name.as_deref()
                        == Some("fm-direct"))
            })
            .expect("the worker has a row");
        let first_mate_row = entries
            .iter()
            .position(|entry| {
                matches!(entry, WorkspaceListEntry::Workspace { ws_idx, .. }
                    if space_tree_name(&app, *ws_idx).as_deref() == Some("firstmate"))
            })
            .expect("the first mate has a row");
        assert_eq!(
            crew_head(&entries, fm_direct),
            Some(first_mate_row),
            "the worker is not in the first mate's own list"
        );
        assert_eq!(
            crew_tier(&entries, fm_direct),
            Some(0),
            "a worker the first mate dispatched itself must sit flush in its card"
        );
    }

    /// The rule is narrow. A worker already under a second mate is untouched by
    /// it, whatever the panel is sorted by.
    #[test]
    fn a_workers_own_mate_is_never_taken_away_from_it() {
        for sort in [
            crate::app::state::AgentPanelSort::Spaces,
            crate::app::state::AgentPanelSort::Priority,
        ] {
            let mut app = mate_fleet();
            app.agent_panel_sort = sort;
            let mut under_a = rows_under(&app, "2ndmate-a", 3);
            let mut under_b = rows_under(&app, "2ndmate-b", 2);
            under_a.sort();
            under_b.sort();
            assert_eq!(under_a, vec!["a-one", "a-three", "a-two"], "{sort:?}");
            assert_eq!(under_b, vec!["b-one", "b-two"], "{sort:?}");
        }
    }

    /// Entry direction survives the re-parenting, in both modes.
    ///
    /// A card enters at the head of *its parent's* children, and a worker tied
    /// to a second mate by scope has to enter at the head of **that mate's**
    /// group rather than of whoever its token names. This is the interaction the
    /// two rules have with each other, so it is asserted rather than assumed.
    #[test]
    fn a_worker_tied_to_a_mate_still_enters_at_the_head_of_that_mates_branch() {
        for sort in [
            crate::app::state::AgentPanelSort::Spaces,
            crate::app::state::AgentPanelSort::Priority,
        ] {
            let mut app = mate_fleet();
            app.agent_panel_sort = sort;
            let pane = app.workspaces[1].test_split(ratatui::layout::Direction::Vertical);
            app.ensure_test_terminals();
            let terminal_id = app.workspaces[1].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app
                .terminals
                .get_mut(&terminal_id)
                .expect("the new pane has a terminal");
            terminal.set_agent_name("fm-opened".to_string());
            terminal.state = AgentState::Idle;
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );

            assert_eq!(
                rows_under(&app, "2ndmate-a", 4),
                vec!["fm-opened", "a-three", "a-two", "a-one"],
                "the newest card did not enter at the head of the mate it was tied to ({sort:?})"
            );
            // ...and it moved nothing in the group it was never part of.
            assert_eq!(rows_under(&app, "2ndmate-b", 2), vec!["b-two", "b-one"]);
        }
    }

    // -------------------------------------------------------------------- width

    /// **Width reads as rank.** At the captain's own width a worker's card is
    /// visibly narrower than a second mate's, which is visibly narrower than the
    /// first mate's — and the right edges form a staircase, which is the part a
    /// reader actually compares.
    ///
    /// The worker's rung is asked of the ladder rather than of the panel,
    /// because a worker is drawn inside its own mate's box and opens a card of
    /// its own only where that box is off screen. The rung still has to be
    /// there, and still has to be legible, for the panel that gives it one.
    #[test]
    fn a_rank_is_legible_from_the_cards_width_alone() {
        let app = mate_fleet();
        let (first_mate, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "firstmate");
        let (mate, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "2ndmate-a");
        let (worker, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "a-one");

        assert!(
            first_mate.width > mate.width && mate.width > worker.width,
            "the ladder is not monotone: {} / {} / {}",
            first_mate.width,
            mate.width,
            worker.width
        );

        // Right edges are flush — that is the right-aligned layout — so the
        // whole difference is carried by the left ones, which now step by the
        // full six columns instead of the connector indent's three.
        let right = |frame: Rect| frame.x + frame.width;
        assert_eq!(
            (right(first_mate), right(mate)),
            (right(worker), right(worker)),
            "the cards are not aligned to the right edge"
        );
        assert!(
            first_mate.x < mate.x && mate.x < worker.x,
            "the left edges are not a staircase: {} / {} / {}",
            first_mate.x,
            mate.x,
            worker.x
        );

        // The step the captain asked to be able to see. Under the old geometry
        // it was three columns, about 7%, and it fell out of the connector
        // indent rather than being a ladder at all.
        let step = mate.width - worker.width;
        assert!(
            step >= 6,
            "a worker is only {step} columns narrower than a second mate"
        );
        assert!(
            f32::from(step) / f32::from(mate.width) > 0.12,
            "the step is under an eighth of the card and will not read as rank"
        );
    }

    /// **The right-alignment contract, stated once.** Every card in the tree
    /// ends in the same column, at every width the panel can be dragged to.
    ///
    /// The captain ruled out the symmetric layout — a card inset on both sides,
    /// which is what a rank ladder spent on the right edge produces — and asked
    /// for the cards to be aligned to the right side instead. Asserted over the
    /// whole fleet rather than the three sample ranks, so a row type that grows
    /// its own left offset later cannot quietly opt out of the alignment.
    #[test]
    fn every_card_ends_in_the_panels_own_last_column() {
        let app = mate_fleet();
        // Only widths that actually draw cards: below the shell's own floor a
        // row is a line of text, which has no frame to align.
        for sidebar_width in [card_shell_min_sidebar_width(), CAPTAIN_SIDEBAR_WIDTH, 60] {
            let fold = fold_width_at(sidebar_width);
            let frames = card_frames(&app, sidebar_width);
            assert!(
                frames.len() >= 6,
                "the fixture lost rows at {sidebar_width}: {frames:?}"
            );
            for (name, frame, ..) in &frames {
                assert_eq!(
                    frame.x + frame.width,
                    fold,
                    "{name} does not end at the panel's right edge on a \
                     {sidebar_width}-wide sidebar"
                );
            }
        }
    }

    /// Right alignment moved the cards, and the wires had to move with them or
    /// the tree would point at where its cards used to be.
    ///
    /// This is [`every_branch_starts_in_the_column_its_parents_border_stands_in`]
    /// asked at every width instead of the captain's own, because the offset the
    /// alignment spends is a function of the fold: it is zero at the card
    /// floor - where this must still hold - and a full step above it.
    #[test]
    fn the_wires_follow_the_cards_to_the_right_at_every_width() {
        let app = mate_fleet();
        for sidebar_width in [card_shell_min_sidebar_width(), CAPTAIN_SIDEBAR_WIDTH, 60] {
            let fold = fold_width_at(sidebar_width);
            let frames = card_frames(&app, sidebar_width);
            let entries = workspace_list_entries_expanded(&app);
            for (index, (name, _, depth, _)) in frames.iter().enumerate() {
                if *depth == 0 {
                    continue;
                }
                let parent = frames[..index]
                    .iter()
                    .rev()
                    .find(|(_, _, parent_depth, _)| *parent_depth < *depth)
                    .unwrap_or_else(|| panic!("{name} has no parent above it"));
                assert_eq!(
                    connector_column(*depth, entries[index].ancestors_continue(), fold),
                    parent.1.x,
                    "at {sidebar_width} wide, {name}'s connector misses {}'s border",
                    parent.0
                );
            }
        }
    }

    /// Give `ws_idx` an `owner` token, the way `workspace report-metadata`
    /// does.
    fn publish_space_owner(app: &mut crate::app::state::AppState, ws_idx: usize, owner: &str) {
        let mut patch = std::collections::HashMap::new();
        patch.insert(
            crate::app::agent_tree::OWNER_TOKEN.to_string(),
            Some(owner.to_string()),
        );
        app.workspaces[ws_idx]
            .metadata_tokens
            .patch(patch, None, std::time::Instant::now());
    }

    /// A mate whose home is its **own** checkout is drawn as a root — a second
    /// first mate — and the only thing that nests it is its own `owner` token.
    ///
    /// This is the captain's *"the Okta corpus second mate renders as if it were
    /// the root"*, reproduced as its shape rather than by its name. Herdr nests
    /// a Space by exactly two facts, and a standalone home has neither:
    ///
    /// - **Linked-worktree membership.** Every other mate in his fleet lives
    ///   under a `.treehouse/firstmate-*/` worktree of the first mate's own
    ///   checkout, so [`crate::app::state::AppState::worktree_space_group`]
    ///   hands it a `structural_parent` for free. A mate in a repository of its
    ///   own shares no key with anything and gets no group at all.
    /// - **A published `owner` token.** Nothing stamps one on a *mate* — a mate
    ///   is not a worker — so the Space publishes nothing and
    ///   [`crate::app::agent_tree::arrange_owner_tree`] correctly makes it a
    ///   root.
    ///
    /// So this is not a naming or ordering bug and no rule here is wrong: the
    /// tree has no third channel, and the fix is for such a home to publish the
    /// token this asserts already works. Pinned as a test because that is the
    /// contract a fleet has to hold up its end of, and because a future
    /// structural edge for Spaces would show up here as this first case
    /// changing.
    #[test]
    fn a_mate_in_its_own_checkout_is_a_root_until_it_publishes_an_owner() {
        let depth_of = |app: &crate::app::state::AppState, name: &str| {
            workspace_list_entries_expanded(app)
                .into_iter()
                .find_map(|entry| match entry {
                    WorkspaceListEntry::Workspace { ws_idx, depth, .. } => {
                        (space_tree_name(app, ws_idx).as_deref() == Some(name)).then_some(depth)
                    }
                    WorkspaceListEntry::Agent { .. } => None,
                })
                .unwrap_or_else(|| panic!("{name} drew no row"))
        };

        let home = |name: &str, key: &str, path: &str, linked: bool| {
            let mut ws = Workspace::test_new(name);
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "fleet".into(),
                repo_root: std::path::PathBuf::from("/fleet"),
                checkout_path: std::path::PathBuf::from(path),
                is_linked_worktree: linked,
            });
            ws
        };

        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            // The first mate's own checkout.
            home("firstmate", "fleet", "/fleet", false),
            // A mate that is a linked worktree of it, which is every mate in the
            // captain's fleet but one.
            home("2ndmate-herdr", "fleet", "/fleet/wt/herdr", true),
            // And the one whose home is a repository of its own.
            home(
                "2ndmate-oktacorpus",
                "oktacorpus",
                "/homes/oktacorpus",
                false,
            ),
        ];

        assert_eq!(
            depth_of(&app, "2ndmate-herdr"),
            1,
            "a linked-worktree mate should hang off the first mate"
        );
        assert_eq!(
            depth_of(&app, "2ndmate-oktacorpus"),
            0,
            "the reported bug: a mate in its own checkout is drawn as a second root"
        );

        // The channel that is already there, and all such a home needs to use.
        publish_space_owner(&mut app, 2, "firstmate");
        assert_eq!(
            depth_of(&app, "2ndmate-oktacorpus"),
            1,
            "an owner token naming the first mate has to nest a standalone mate \
             exactly like a worktree one"
        );
        assert_eq!(
            depth_of(&app, "2ndmate-herdr"),
            1,
            "and must not disturb the mate that was already nested"
        );
    }

    /// The ladder is paid for out of slack, so it can never push the deepest
    /// card below the width the card shell needs — and so the sidebar's drag
    /// detent, which is anchored on that same floor, does not move.
    #[test]
    fn the_ladder_is_spent_only_out_of_what_a_panel_has_above_the_card_floor() {
        for fold in card::MIN_FOLD_WIDTH..=card::MIN_FOLD_WIDTH + 24 {
            let deepest = fold
                .saturating_sub(
                    tree_prefix_width(crate::app::agent_tree::MAX_DISPLAY_DEPTH, 0) as u16,
                )
                .saturating_sub(rank_width_inset(AgentRelation::Worker, fold));
            let floor = card::MIN_FOLD_WIDTH
                - tree_prefix_width(crate::app::agent_tree::MAX_DISPLAY_DEPTH, 0) as u16;
            assert!(
                deepest >= floor,
                "at fold {fold} the deepest card is {deepest}, under the {floor} the shell needs"
            );

            // Monotone at every width, and flat at the floor itself, where there
            // is nothing to spend.
            let mate = rank_width_inset(AgentRelation::SecondMate, fold);
            assert!(rank_width_inset(AgentRelation::FirstMate, fold) == 0);
            assert!(rank_width_inset(AgentRelation::Worker, fold) >= mate);
        }
        assert_eq!(
            rank_width_inset(AgentRelation::Worker, card::MIN_FOLD_WIDTH),
            0,
            "the floor has no slack, so every rank must draw the width it always did"
        );
    }

    /// The detent the sidebar drag sticks at is anchored on the card floor, and
    /// the ladder must not have moved it.
    #[test]
    fn the_card_shell_detent_did_not_move() {
        assert_eq!(card_shell_min_sidebar_width(), card::MIN_FOLD_WIDTH + 2);
    }

    /// Rank is what a row **is**; depth is where it hangs. The two agree for
    /// every Space and come apart for the one row the captain's rule is about.
    #[test]
    fn rank_follows_what_a_row_is_and_never_who_opened_it() {
        let app = mate_fleet();
        for (name, _, depth, rank) in card_frames(&app, CAPTAIN_SIDEBAR_WIDTH) {
            let expected = match name.as_str() {
                "firstmate" => AgentRelation::FirstMate,
                "2ndmate-a" | "2ndmate-b" => AgentRelation::SecondMate,
                _ => AgentRelation::Worker,
            };
            assert_eq!(
                rank, expected,
                "{name} at depth {depth} drew the wrong rank"
            );
        }
    }

    /// The whole ladder, at the width the captain runs, written down so a change
    /// to it is a change somebody had to make on purpose.
    #[test]
    fn the_ladder_at_the_captains_own_sidebar_width() {
        let fold = fold_width_at(CAPTAIN_SIDEBAR_WIDTH);
        let widths: Vec<u16> = [
            AgentRelation::FirstMate,
            AgentRelation::SecondMate,
            AgentRelation::Worker,
        ]
        .into_iter()
        .enumerate()
        .map(|(depth, rank)| {
            fold - tree_prefix_width(depth as u8, 0) as u16 - rank_width_inset(rank, fold)
        })
        .collect();
        assert_eq!(widths, vec![39, 33, 27]);
    }

    /// A Space with no worktree group of its own is unaffected by any of this:
    /// a flat fleet that declares no ownership draws one full-width card per
    /// Space, exactly as it always did.
    #[test]
    fn a_flat_fleet_still_draws_full_width_cards() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];

        let fold = fold_width_at(CAPTAIN_SIDEBAR_WIDTH);
        for (name, frame, depth, rank) in card_frames(&app, CAPTAIN_SIDEBAR_WIDTH) {
            assert_eq!(depth, 0, "{name} nested with nothing to nest under");
            assert_eq!(rank, AgentRelation::FirstMate);
            assert_eq!(frame.x, 1);
            assert_eq!(frame.width, fold - 1);
        }
    }

    // ------------------------------------------------------- nested mates

    /// A second mate that has itself dispatched a second mate, with a worker
    /// under the deeper of the two.
    ///
    /// No home has run this shape yet, so it is built here rather than
    /// captured: nesting a Space is nothing more than pointing its `owner`
    /// token at another Space's tree name, which is the same edge
    /// [`mate_fleet`] uses one level up.
    fn nested_mate_fleet() -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        let mut nested = Workspace::test_new("3rdmate");
        let worker = nested.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![
            Workspace::test_new("firstmate"),
            Workspace::test_new("2ndmate-a"),
            nested,
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        for (ws_idx, owner) in [(1usize, "firstmate"), (2, "2ndmate-a")] {
            app.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([("owner".to_string(), Some(owner.to_string()))]),
                None,
                now,
            );
        }

        let owner_id = app.workspaces[2].id.clone();
        let terminal_id = app.workspaces[2].tabs[0].panes[&worker]
            .attached_terminal_id
            .clone();
        if let Some(terminal) = app.terminals.get_mut(&terminal_id) {
            terminal.set_agent_name("deep-worker".to_string());
            terminal.state = AgentState::Idle;
            terminal.created_by = Some(crate::api::schema::PaneOrigin {
                pane_id: "deep-worker".to_string(),
                workspace_id: owner_id,
            });
        }
        app
    }

    /// Every row the panel actually draws, as text, at the captain's width.
    fn drawn_panel(mut app: crate::app::state::AppState, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, CAPTAIN_SIDEBAR_WIDTH, height);
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
                .expect("a test backend");
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .expect("the panel draws");
        let buffer = terminal.backend().buffer();
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|col| buffer[(col, area.y + row)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    /// A mate is a mate wherever it hangs. Nested under another mate it sits at
    /// depth 2, which is a *worker's* rung of the ladder — and reading its rank
    /// off that depth drew a persistent mate at exactly the size of the one-off
    /// tasks running underneath it.
    ///
    /// The stakes went up with the captain's fold rule rather than down: a mate
    /// mistaken for a worker would no longer merely be drawn small, it would be
    /// drawn *inside its parent's card*, with its own crew under it. So this
    /// also asserts what the panel hands each of them — the nested mate a box,
    /// its worker none.
    #[test]
    fn a_mate_nested_under_a_mate_is_still_a_mate() {
        let app = nested_mate_fleet();
        assert!(
            drawn_frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "3rdmate").is_some(),
            "a nested mate was folded into its parent's card"
        );
        assert_eq!(
            drawn_frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "deep-worker"),
            None,
            "the nested mate's own worker opened a box of its own"
        );
        assert_eq!(
            crew_head(
                &workspace_list_entries(&app),
                drawn_row_of(&app, CAPTAIN_SIDEBAR_WIDTH, "deep-worker")
            ),
            Some(drawn_row_of(&app, CAPTAIN_SIDEBAR_WIDTH, "3rdmate")),
            "the worker is not in the card of the mate that dispatched it"
        );
        let (nested, depth, rank) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "3rdmate");
        assert_eq!(depth, 2, "the fixture did not nest the second mate at all");
        assert_eq!(
            rank,
            AgentRelation::SecondMate,
            "a mate two levels down was demoted to a worker by its depth"
        );

        // The worker in its Space hangs deeper still, and is the row the mate
        // must not be mistaken for.
        let (worker, worker_depth, worker_rank) =
            frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "deep-worker");
        assert!(worker_depth > depth, "the fixture lost the worker's edge");
        assert_eq!(worker_rank, AgentRelation::Worker);
        assert!(
            nested.width > worker.width,
            "a nested mate drew a worker's card: {} vs {} cols",
            nested.width,
            worker.width
        );

        // And it is still nested: a mate under a mate is not drawn as its
        // parent's peer.
        let (parent, ..) = frame_of(&app, CAPTAIN_SIDEBAR_WIDTH, "2ndmate-a");
        assert!(
            nested.width < parent.width,
            "a nested mate drew its parent's card: {} vs {} cols",
            nested.width,
            parent.width
        );
        assert_eq!(
            nested.x + nested.width,
            parent.x + parent.width,
            "the cards stopped being right-aligned"
        );
    }

    /// The rails of a nested mate stop where its card starts.
    ///
    /// Its card is wider than its level's column, which is the one direction
    /// [`rank_residual_spans`] cannot express: without [`fit_rails_to_card`]
    /// the columns the level claimed are still drawn, and the branch runs on
    /// *inside* the card it is pointing at.
    #[test]
    fn a_nested_mates_branch_stops_at_its_border() {
        let app = nested_mate_fleet();
        let rows = drawn_panel(app, 24);
        let screen = rows.join("\n");
        let row = rows
            .iter()
            .find(|row| row.contains("3rdmate"))
            .unwrap_or_else(|| panic!("the nested mate drew no row:\n{screen}"));

        let cells: Vec<char> = row.chars().collect();
        let name = row
            .find("3rdmate")
            .and_then(|byte| row[..byte].chars().count().into())
            .expect("the row holds the name");
        let border = cells[..name]
            .iter()
            .rposition(|glyph| *glyph == '\u{2502}')
            .unwrap_or_else(|| panic!("the nested mate drew no left border:\n{screen}"));

        assert!(
            cells[border + 1..name].iter().all(|glyph| *glyph == ' '),
            "the branch was drawn inside the card it points at:\n{screen}"
        );
        assert!(
            cells[..border]
                .iter()
                .any(|glyph| *glyph == '\u{251c}' || *glyph == '\u{2514}'),
            "the nested mate lost the connector it hangs off:\n{screen}"
        );
        assert_eq!(
            cells[border - 1],
            '\u{2500}',
            "the branch stops short of the border it points at:\n{screen}"
        );
    }

    /// No rail is ever drawn into a border, and the ladder is the same three
    /// widths on a fleet with no nested mate.
    ///
    /// [`fit_rails_to_card`] only takes columns back from a row whose card is
    /// wider than its level, which no row in the captain's own fleet is, so
    /// nothing it does may reach that fleet's rails.
    #[test]
    fn a_fleet_with_no_nested_mate_keeps_its_rails_out_of_every_border() {
        let rows = drawn_panel(mate_fleet(), 32);
        for (index, row) in rows.iter().enumerate() {
            assert!(
                !row.contains("\u{2502}\u{2500}"),
                "row {index} ran a rail into a card border:\n{}",
                rows.join("\n")
            );
        }
        // The ladder itself is unchanged: three ranks, three widths.
        let widths: Vec<u16> = ["firstmate", "2ndmate-a", "a-one"]
            .into_iter()
            .map(|name| frame_of(&mate_fleet(), CAPTAIN_SIDEBAR_WIDTH, name).0.width)
            .collect();
        assert_eq!(widths, vec![39, 33, 27]);
    }
}

/// Where the tree's branch line meets a card it points at.
///
/// The captain, live on a real Rio at his own 42 columns: *"branch lines are not
/// centered on the card pane's vertical portion."* The line lands on a whole
/// character row and the drawn card is a shape of a fixed pixel height, and the
/// two agreed only while a row happened to be an odd number of cells — three at
/// a 21 px cell, four at 15–18 px, where the line ran into every card in the
/// tree half a cell above its middle at once.
#[cfg(test)]
mod a_branch_line_meets_its_card_in_the_middle {
    use super::*;
    use crate::app::state::{AppState, WorkspaceCardArea};
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    fn area(frame_height: u16, drawn_card: bool) -> WorkspaceCardArea {
        WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 4, 40, frame_height),
            worktree_child: false,
            entry_idx: 0,
            agent: None,
            card_frame: Some(Rect::new(4, 4, 30, frame_height)),
            motion_cells: (0, 0),
            arriving: false,
            drawn_card,
        }
    }

    /// A drawn card's line lands on the row its own middle falls in, whatever
    /// its cell height made the row.
    #[test]
    fn a_drawn_cards_line_lands_on_the_row_its_middle_falls_in() {
        for (rows, expected) in [(1u16, 4u16), (2, 4), (3, 5), (4, 5), (5, 6)] {
            assert_eq!(
                area(rows, true).connector_y(),
                expected,
                "a {rows}-cell drawn card"
            );
        }
    }

    /// A character card's line still points at its *name*. Its rows are its own
    /// content — the captain's three token rows make a five-row box — and a line
    /// to the middle of that box would be pointing at whatever token happened to
    /// be third.
    #[test]
    fn a_character_cards_line_still_points_at_its_name() {
        for rows in [3u16, 4, 5, 6] {
            let card = area(rows, false);
            assert_eq!(card.connector_y(), card.content_y(), "a {rows}-cell box");
            assert_eq!(card.connector_y(), 5);
        }
    }

    /// A row with no card shell at all answers with its own first row, which is
    /// what every caller of `content_y` already relied on.
    #[test]
    fn a_bare_row_answers_with_its_own_first_row() {
        let mut card = area(3, true);
        card.card_frame = None;
        assert_eq!(card.connector_y(), card.rect.y);
    }

    /// The renderer draws it there.
    ///
    /// The link the two geometry tests cannot make on their own: `connector_y`
    /// being right is worth nothing if `render_card_border_rails` goes on
    /// drawing the `└──` on `content_y`. Driven through the real
    /// [`render_sidebar`] at an 11 px cell, where a card needs five cells and
    /// the middle row is two down rather than one — a four-cell row cannot say
    /// anything here, because there the two rows agree and it is the card that
    /// moves onto the line instead.
    ///
    /// **The row under test is the second mate's Space, not the worker inside
    /// it.** A worker is drawn inside its Space's own box now (see
    /// [`crew_head`]) and hangs off no branch at all, so it can no longer answer
    /// this question. The branch this asserts is the one that still exists —
    /// the first mate's line reaching down to the second mate — and it is the
    /// same geometry on the same code path.
    #[test]
    fn the_renderer_draws_the_branch_on_that_row_and_not_on_the_name_row() {
        let mut app = AppState::test_new();
        let mut mate = Workspace::test_new("2ndmate-a");
        let worker = mate.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![Workspace::test_new("firstmate"), mate];
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
        // The pane is left *unowned* on purpose. A pane that named the second
        // mate as its owner is a worker, and a worker is drawn inside its
        // Space's own box now (see [`crew_head`]) — which stretches that box
        // over its worker rows and makes its cell count even, and an even count
        // is exactly the case this fixture cannot use: there the connector row
        // and the name row agree and there is nothing to tell apart. The branch
        // under test is the first mate's line down to the second mate, and that
        // exists whether or not the second mate is running anything.
        let terminal_id = app.workspaces[1].tabs[0].panes[&worker]
            .attached_terminal_id
            .clone();
        let carrier = app
            .terminals
            .get_mut(&terminal_id)
            .expect("a test terminal");
        carrier.state = AgentState::Idle;
        app.kitty_graphics_enabled = true;
        app.kitty_graphics_capability_confirmed = true;
        app.sidebar_card_shapes = true;
        app.view.sidebar_card_layers_published = true;
        // A cell short enough that a card needs an *odd* number of them, which
        // is the case this fixture exists to exercise: an even count puts the
        // frame's middle on a cell boundary and there is no middle row for the
        // branch line to land on. 13 px is that cell for the card's current
        // height; it was 11 px when the card carried one caption line rather
        // than `image_card::CAPTION_LINES`.
        app.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 13,
        };

        let area = Rect::new(0, 0, 42, 30);
        app.sidebar_width = area.width;
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let Some(card) = app
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.agent.is_none() && card.ws_idx == 1)
            .copied()
        else {
            panic!("the second mate's row is missing from the tree");
        };
        if !card.drawn_card {
            return; // No proportional face on this machine; nothing draws a card.
        }
        let frame = card.card_frame.expect("a card at 42 columns");
        assert!(
            frame.height >= 5 && frame.height % 2 == 1,
            "a 13 px cell must give an odd row of five cells or more, not {}",
            frame.height
        );
        assert_ne!(
            card.connector_y(),
            card.content_y(),
            "this fixture has to be one where the two rows disagree"
        );

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|f| render_sidebar(&app, &TerminalRuntimeRegistry::new(), f, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let branch_rows: Vec<u16> = (card.rect.y..card.rect.y + card.rect.height)
            .filter(|y| (0..frame.x).any(|x| matches!(buffer[(x, *y)].symbol(), "├" | "└")))
            .collect();
        assert_eq!(
            branch_rows,
            vec![card.connector_y()],
            "the second mate's branch is not on the row its card's middle falls in"
        );
    }
}

/// **The line leaving a card.** A child's connector stands in the column its
/// parent's own border stands in, so the stretch of that line which crosses the
/// parent's own rows has nowhere to be drawn except *in* that border column.
///
/// Under the character shell the card's border already fills it and nothing was
/// missing. Under a drawn card it is not filled by anything: the shape shell
/// draws no character border at all, and the sheet paints its backdrop over the
/// cells either way. That is the captain's *"the trunk line from firstmate does
/// not visually touch the firstmate root node"* — the trunk started a gutter
/// below the card it leaves. The character half of the fix is asserted here; the
/// pixel half is [`super::sidebar::image_card`]'s `the_tree_lines_reach_the_cards_they_join`.
#[cfg(test)]
mod a_branch_leaves_its_parents_own_border_column {
    use super::tests::interleaved_worker_fleet;
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// Where each row's card stands, and the whole panel as drawn, on a panel
    /// running the *shape* shell — the one that draws no character card, so the
    /// only thing that can be in a card's border column is the tree.
    fn shaped_fleet_screen() -> (Vec<Rect>, Vec<String>) {
        let width = 42u16;
        let area = Rect::new(0, 0, width, 40);
        let mut app = interleaved_worker_fleet();
        app.kitty_graphics_enabled = true;
        app.kitty_graphics_capability_confirmed = true;
        app.sidebar_card_shapes = true;
        app.view.sidebar_card_layers_published = true;
        app.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        app.view.sidebar_rect = area;
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);

        let mut terminal = Terminal::new(TestBackend::new(width, 40)).expect("test backend");
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .expect("draw");
        let rows: Vec<String> = terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let frames = app
            .view
            .workspace_card_areas
            .iter()
            .filter_map(|card| card.card_frame)
            .collect();
        (frames, rows)
    }

    fn glyph(rows: &[String], x: u16, y: u16) -> String {
        rows.get(usize::from(y))
            .and_then(|row| row.chars().nth(usize::from(x)))
            .map(|glyph| glyph.to_string())
            .unwrap_or_default()
    }

    /// **No rail cell lands inside any pane box.**
    ///
    /// A18/A32, and the artifact's own gate: *a thin vertical rail drops from
    /// the parent pane's **bottom edge** — never inside it, at any alpha* — turns
    /// one hard right angle, and runs into the child's left edge. The overlap
    /// between any rail and any pane box has to be exactly zero.
    ///
    /// This test used to assert the opposite. It was called
    /// `the_trunk_is_drawn_inside_the_first_mates_own_frame`, and it required
    /// the root card's own rows to carry the trunk through the card's border
    /// column — because the pixel sheet painted an opaque backdrop over those
    /// cells and the line had nowhere else to be. The sheet paints no backdrop
    /// now (a card is glass; see `image_card`), so the tree's line is free to
    /// run in the gutter where it belongs and the rail starts at the card's
    /// bottom edge.
    ///
    /// The one cell deliberately not counted is the connector's own terminating
    /// tick — the `─` in the child card's first column, drawn by
    /// [`connector_joint_span`]. The reference's elbow *"runs right into the
    /// worker's left edge, terminating in a small tick"*, and under a drawn card
    /// that column's ink and the card's own stroke are the same line: the stroke
    /// stands at the column's centre ([`super::image_card::RAIL_INK_COLUMN_FRACTION`]),
    /// so the tick abuts the edge rather than crossing it.
    #[test]
    fn no_rail_cell_lands_inside_a_pane_box() {
        let (frames, rows) = shaped_fleet_screen();
        assert!(!frames.is_empty(), "the fixture drew no cards");
        let screen = rows.join("\n");
        let mut checked = 0;
        for frame in &frames {
            for y in frame.y..frame.y + frame.height {
                // The border column itself is where the terminating tick lands,
                // so the sweep starts inside it and runs to the card's right
                // edge — every cell the card's own box owns.
                for x in frame.x..frame.x + frame.width {
                    let glyph = glyph(&rows, x, y);
                    if x == frame.x && glyph == "─" {
                        // The tick. See this test's own doc.
                        continue;
                    }
                    assert!(
                        !matches!(glyph.as_str(), "│" | "├" | "└" | "┃"),
                        "a tree rail ({glyph}) is drawn at ({x}, {y}), inside the pane \
                         box at {frame:?}:\n{screen}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no pane box was swept");
    }

    /// **The rail grows down from its own fixed anchor before the branch
    /// grows right, before the card blooms.**
    ///
    /// [`image_card::row_arrival`]'s gesture, on the characters that carry the
    /// first two of its four beats: the rail grows *down* to the elbow from a
    /// fixed top anchor, and only once it has fully reached the elbow does the
    /// branch grow *right* out of it. A rail fully drawn while `rail < 1.0`,
    /// or a branch drawn before the rail has reached it, is the gesture played
    /// out of order.
    #[test]
    fn the_rail_grows_down_before_the_branch_grows_right() {
        use crate::ui::sidebar::motion::ArrivalCircuit;

        // Only the rows the renderer actually draws, per circuit, over one
        // card four cells tall whose connector lands on its second row.
        let frame = Rect::new(7, 10, 33, 4);
        let connector_y = frame.y + (frame.height - 1) / 2;
        let drawn = |circuit: ArrivalCircuit| {
            let mut rows = Vec::new();
            for y in frame.y..frame.y + frame.height {
                let carried = if y == connector_y {
                    circuit.tick > 0.0
                } else if y < connector_y {
                    let run = f32::from(connector_y - frame.y);
                    let grown = run * circuit.rail;
                    let offset = f32::from(y - frame.y);
                    offset < grown
                } else {
                    circuit.card > 0.0
                };
                if carried {
                    rows.push(y);
                }
            }
            rows
        };

        let settled = || ArrivalCircuit {
            push: 1.0,
            rail: 1.0,
            tick: 1.0,
            card: 1.0,
        };

        // At the very start of the rail's own growth, nothing is lit at all —
        // the fixed anchor is the segment's own top, and the segment has zero
        // extent until growth reaches it. Nothing after it has begun either.
        let opening = drawn(ArrivalCircuit {
            rail: 0.0,
            tick: 0.0,
            card: 0.0,
            ..settled()
        });
        assert_eq!(
            opening,
            Vec::<u16>::new(),
            "the rail lit before it had grown at all"
        );
        assert!(
            !drawn(ArrivalCircuit {
                rail: 0.9,
                tick: 0.0,
                card: 0.0,
                ..settled()
            })
            .contains(&connector_y),
            "the branch lit while the rail was still growing toward it"
        );
        // And it does reach the elbow's own row by the end of its growth.
        assert!(drawn(ArrivalCircuit {
            rail: 1.0,
            tick: 0.0,
            card: 0.0,
            ..settled()
        })
        .contains(&(connector_y - 1)));

        // The branch lights once it has started growing, and a fully settled
        // row has the whole tree line.
        assert!(drawn(ArrivalCircuit {
            tick: 0.5,
            ..settled()
        })
        .contains(&connector_y));
        assert_eq!(
            drawn(settled()).len(),
            usize::from(frame.height),
            "a settled row is missing part of its own rail"
        );
    }

    /// And the rail is still there, in the gutter, so the tree is still a tree.
    ///
    /// The other half of [`no_rail_cell_lands_inside_a_pane_box`]: moving the
    /// line out of the card must not delete it. A mate with children carries the
    /// trunk on every row between its own bottom edge and its first child's
    /// first row.
    #[test]
    fn the_rail_runs_in_the_gutter_under_the_card_it_leaves() {
        let (frames, rows) = shaped_fleet_screen();
        let first_mate = frames.first().copied().expect("the fixture drew no cards");
        let screen = rows.join("\n");
        let below = first_mate.y + first_mate.height;
        assert_eq!(
            glyph(&rows, first_mate.x, below),
            "│",
            "the trunk does not leave the first mate's bottom edge at row {below}:\n{screen}"
        );
    }

    /// And a row nothing hangs off does not grow one. A rail leaving a card with
    /// no children is a line pointing at nothing, and it would also be the one
    /// glyph standing between the last card of a group and the panel below it.
    #[test]
    fn a_card_with_nothing_under_it_leaves_no_rail_behind() {
        let (frames, rows) = shaped_fleet_screen();
        // `b-one` is the last row of the whole tree: nothing hangs off it and
        // nothing follows it.
        let last = frames.last().copied().expect("the fixture drew no cards");
        let connector = last.y + (last.height.saturating_sub(1)) / 2;
        let screen = rows.join("\n");
        for y in connector + 1..last.y + last.height {
            assert_ne!(
                glyph(&rows, last.x, y),
                "│",
                "the last card in the tree grew a rail out of its bottom:\n{screen}"
            );
        }
    }

    /// The predicate itself, at the display cap: two rows the tree keeps at
    /// different depths but *draws* in the same column have no line between
    /// them, so the deeper one is not a branch off the shallower one.
    #[test]
    fn a_row_past_the_display_cap_opens_no_branch() {
        let entry = |depth: u8| WorkspaceListEntry::Workspace {
            ws_idx: 0,
            worktree_child: false,
            depth,
            ancestors_continue: Vec::new(),
            is_last_child: false,
        };
        let cap = crate::app::agent_tree::MAX_DISPLAY_DEPTH;
        assert!(row_opens_a_branch(&[entry(0), entry(1)], 0));
        assert!(!row_opens_a_branch(&[entry(1), entry(1)], 0));
        assert!(
            !row_opens_a_branch(&[entry(cap), entry(cap + 1)], 0),
            "a row drawn in its parent's own column must not open a branch off it"
        );
        assert!(!row_opens_a_branch(&[entry(0)], 0), "nothing follows it");
    }
}

/// The tree has to say when a view is the reason it looks empty.
///
/// The captain reported he could not see the first mate or any second mate in
/// the left-hand panel "at all". The tree itself was innocent — it draws the
/// mates from worktree membership alone, with no `owner` token needed — but his
/// durable Agents view filters on `exists owner`, and nothing in the fleet was
/// publishing that token, so every worker row was filtered out and the panel
/// gave no sign that a filter was responsible.
///
/// [`crate::app::agent_view::AgentViewHidden`] had counted exactly that all
/// along and nothing drew it, which is the same hazard
/// [`crate::agent_view::AgentViewSlots::durable`] names when it refuses to
/// persist the UI tier: a filtered panel that comes back "with nothing on
/// screen explaining why".
#[cfg(test)]
mod a_filtered_tree_says_what_it_is_holding_back {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::{Workspace, WorktreeSpaceMembership};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    const REPO: &str = "/repo/firstmate";

    fn membership(checkout: &str, linked: bool) -> WorktreeSpaceMembership {
        WorktreeSpaceMembership {
            key: "firstmate".to_string(),
            label: "firstmate".to_string(),
            repo_root: REPO.into(),
            checkout_path: checkout.into(),
            is_linked_worktree: linked,
        }
    }

    /// The captain's fleet shape: a main checkout Space with the second mates
    /// as linked worktrees of it, and a worker pane inside each.
    fn fleet(publish_owner: bool) -> AppState {
        let mut app = AppState::test_new();

        let mut root = Workspace::test_new("firstmate");
        root.identity_cwd = REPO.into();
        root.cached_identity_cwd = REPO.into();
        root.worktree_space = Some(membership(REPO, false));
        app.workspaces = vec![root];

        for (name, home) in [
            ("2ndmate-herdr", "/pool/12/firstmate"),
            ("2ndmate-game", "/pool/4/firstmate"),
        ] {
            let mut ws = Workspace::test_new(name);
            ws.identity_cwd = home.into();
            ws.cached_identity_cwd = home.into();
            ws.worktree_space = Some(membership(home, true));
            app.workspaces.push(ws);
        }
        app.ensure_test_terminals();

        let now = std::time::Instant::now();
        for (ws_idx, owner) in [(1usize, "2ndmate-herdr"), (2, "2ndmate-game")] {
            let pane = app.workspaces[ws_idx].test_split(ratatui::layout::Direction::Vertical);
            app.ensure_test_terminals();
            let terminal_id = app.workspaces[ws_idx]
                .pane_state(pane)
                .expect("split pane")
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal");
            terminal.set_agent_name(format!("{owner}-worker"));
            if publish_owner {
                terminal.metadata_tokens.patch(
                    std::collections::HashMap::from([(
                        "owner".to_string(),
                        Some(owner.to_string()),
                    )]),
                    None,
                    now,
                );
            }
        }

        app.active = Some(0);
        app.selected = 0;
        app.sidebar_width = 42;
        app.default_sidebar_width = 42;
        app
    }

    /// The captain's own `agent_view`, verbatim off his `session.json`.
    fn workers_only(app: &mut AppState) {
        app.agent_views.set(
            crate::agent_view::AgentViewTier::Api,
            Some(crate::api::schema::AgentViewSetParams {
                source: "workers-only".to_string(),
                label: None,
                filter: Some(crate::api::schema::AgentViewFilter::Exists {
                    field: crate::api::schema::AgentViewField::Token {
                        token: "owner".to_string(),
                    },
                }),
                sort: Vec::new(),
            }),
        );
    }

    fn panel(app: &AppState) -> String {
        let rect = app.view.sidebar_rect;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(app, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (rect.y..rect.y + rect.height)
            .map(|y| {
                (rect.x..rect.x + rect.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn drawn(app: &mut AppState) -> String {
        crate::ui::compute_view(app, Rect::new(0, 0, 120, 40));
        panel(app)
    }

    /// The report, end to end: an unpublished `owner` token empties the panel
    /// through the view, and the header now names the view as the reason.
    #[test]
    fn a_view_that_hides_every_worker_says_so_in_the_header() {
        let mut app = fleet(false);
        workers_only(&mut app);
        let drawn = drawn(&mut app);

        assert_eq!(
            app.view.sidebar_view_hidden.hidden, 2,
            "both workers were filtered out for carrying no `owner`"
        );
        assert!(
            drawn.contains("2 hidden"),
            "the header did not say the view was holding rows back:\n{drawn}"
        );
    }

    /// The half the tree was always getting right, pinned so a future change to
    /// the notice cannot be mistaken for a fix to the rows themselves: the
    /// mates draw from worktree membership whether or not anything publishes an
    /// `owner` token, and whether or not a view is filtering.
    #[test]
    fn the_mates_still_draw_under_a_filter_that_hides_every_worker() {
        let mut app = fleet(false);
        workers_only(&mut app);
        let drawn = drawn(&mut app);

        for mate in ["firstmate", "2ndmate-herdr", "2ndmate-game"] {
            assert!(
                drawn.contains(mate),
                "the tree dropped {mate:?} under an active view:\n{drawn}"
            );
        }
    }

    /// The notice is about the *view*, not about an empty fleet: publishing the
    /// token the view asks for retires it, leaving the header exactly as it was
    /// before the slot existed.
    #[test]
    fn a_view_that_hides_nothing_draws_no_notice() {
        let mut app = fleet(true);
        workers_only(&mut app);
        let drawn = drawn(&mut app);

        assert!(!app.view.sidebar_view_hidden.any());
        assert!(
            !drawn.contains("hidden"),
            "a view hiding nothing still drew a notice:\n{drawn}"
        );
    }

    /// With no view at all there is nothing to explain, so the row is untouched.
    #[test]
    fn no_view_draws_no_notice() {
        let mut app = fleet(false);
        let drawn = drawn(&mut app);

        assert!(!app.view.sidebar_view_hidden.any());
        assert!(!drawn.contains("hidden"), "{drawn}");
    }

    /// A blocked agent is waiting on the user, so it is called out separately
    /// rather than folded into the count — the case the counter was built for.
    #[test]
    fn a_hidden_blocked_agent_is_named_separately() {
        let mut app = fleet(false);
        workers_only(&mut app);
        let terminal_id = app.workspaces[1]
            .pane_details(&app.terminals)
            .into_iter()
            .find_map(|detail| app.workspaces[1].terminal_id(detail.pane_id).cloned())
            .expect("a worker terminal");
        app.terminals
            .values_mut()
            .find(|terminal| terminal.id == terminal_id)
            .expect("terminal")
            .state = crate::detect::AgentState::Blocked;

        let drawn = drawn(&mut app);
        assert_eq!(app.view.sidebar_view_hidden.hidden_blocked, 1);
        assert!(
            drawn.contains("blocked"),
            "a blocked agent was hidden without being named:\n{drawn}"
        );
    }

    /// The header is a fixed single row shared with the pulse, the breadcrumb
    /// and the session status. A notice that does not fit is dropped whole,
    /// because a clipped count is a wrong count.
    #[test]
    fn a_notice_that_does_not_fit_is_not_drawn_at_all() {
        let mut app = fleet(false);
        workers_only(&mut app);
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
        let hidden = app.view.sidebar_view_hidden;
        let label = tree_view_notice_label(hidden).expect("something is hidden");

        let area = Rect::new(0, 0, display_width(&label) as u16, 8);
        assert_eq!(
            view_notice_rect_after(&app, area, 1).width,
            0,
            "the notice drew into columns it did not have"
        );
    }
}

/// **A Space's card carries the workers it is running, inside its own box.**
///
/// The captain's confirmed mockups: header, bars and orbit line, then a dashed
/// rule, then one compact row per worker — one border around the Space and
/// everything running in it, rather than one border each. What this module
/// pins is the part that is a *fact about the tree* rather than about pixels:
/// which rows are in which card's list, at which step, in which order, and that
/// every one of them is still a row you can click.
#[cfg(test)]
pub(crate) mod a_space_card_carries_its_own_workers {
    use super::*;
    use crate::workspace::Workspace;

    /// A Space running three things: a worker it dispatched itself, a second
    /// mate *pane*, and that mate's own worker.
    ///
    /// The second mate is a pane rather than a Space on purpose. A mate with a
    /// checkout of its own is a Space and heads its own card — that case is
    /// [`super::crew_head`]'s scan stopping at a `Workspace`. The case this
    /// fixture is for is the captain's third requirement: a second mate working
    /// in the *same* Space a direct worker is in, where both have to land in the
    /// one list told apart by nothing but the step.
    pub(super) fn crewed_fleet() -> AppState {
        let mut app = AppState::test_new();
        let mut space = Workspace::test_new("herdr");
        let direct = space.test_split(ratatui::layout::Direction::Vertical);
        let mate = space.test_split(ratatui::layout::Direction::Vertical);
        let via = space.test_split(ratatui::layout::Direction::Vertical);
        app.workspaces = vec![space];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        for (pane, name, owner) in [
            (direct, "fm/direct", "herdr"),
            (mate, "mate", "herdr"),
            (via, "fm/via", "mate"),
        ] {
            let id = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&id).expect("a test terminal");
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

    /// The same fleet with the pixel path live, which is the only panel that
    /// draws a worker list at all.
    fn drawing(mut app: AppState) -> AppState {
        app.kitty_graphics_enabled = true;
        app.kitty_graphics_capability_confirmed = true;
        app.sidebar_card_shapes = true;
        app.view.sidebar_card_layers_published = true;
        app.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        app
    }

    fn area() -> Rect {
        Rect::new(0, 0, 42, 46)
    }

    /// The tree, with each row's name so a failure says which row moved.
    fn named(app: &AppState) -> Vec<(String, Option<usize>, Option<u8>)> {
        let entries = workspace_list_entries(app);
        let agents = sidebar_agent_entries(app);
        entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let name = match entry {
                    WorkspaceListEntry::Workspace { ws_idx, .. } => {
                        app.workspaces[*ws_idx].display_name().to_string()
                    }
                    WorkspaceListEntry::Agent { entry_idx, .. } => {
                        agents[*entry_idx].agent_name.clone().unwrap_or_default()
                    }
                };
                (name, crew_head(&entries, idx), crew_tier(&entries, idx))
            })
            .collect()
    }

    /// **Both mates' workers are in the one list, told apart only by the step.**
    ///
    /// A direct worker and a second mate's worker running in the same Space land
    /// in the same card, at tier `0` and tier `1` — never in two sections and
    /// never in two cards. The second mate's own pane is a row of that list too,
    /// at the Space's own margin: it is something the Space dispatched, which is
    /// all tier `0` claims.
    #[test]
    fn both_tiers_land_in_one_list() {
        let app = crewed_fleet();
        let rows = named(&app);
        assert_eq!(rows[0].1, None, "the Space itself is in nobody's list");
        for (name, head, tier) in rows.iter().skip(1) {
            assert_eq!(*head, Some(0), "{name} left the Space's own card");
            match name.as_str() {
                "fm/direct" | "mate" => assert_eq!(*tier, Some(0), "{name} is not flush"),
                "fm/via" => assert_eq!(*tier, Some(1), "{name} is not stepped in"),
                other => panic!("unexpected row {other}"),
            }
        }
        assert_eq!(crew_len(&workspace_list_entries(&app), 0), 3);
    }

    /// **One step, whichever mate it came through and however deep the chain.**
    ///
    /// A worker dispatched by a second mate's own second mate draws at the same
    /// step as one dispatched by the second mate. Past the first step the indent
    /// has stopped saying anything a reader needs and started eating the panel.
    #[test]
    fn a_deeper_chain_still_draws_at_one_step() {
        let mut app = crewed_fleet();
        let deeper = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.ensure_test_terminals();
        let id = app.workspaces[0].tabs[0].panes[&deeper]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&id).expect("a test terminal");
        terminal.set_agent_name("fm/deeper".to_string());
        terminal.state = AgentState::Idle;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("fm/via".to_string()))]),
            None,
            std::time::Instant::now(),
        );
        let rows = named(&app);
        let deeper = rows
            .iter()
            .find(|(name, _, _)| name == "fm/deeper")
            .expect("the deeper worker is missing from the tree");
        assert_eq!(deeper.1, Some(0), "it left the Space's own card");
        assert_eq!(deeper.2, Some(1), "a third tier was invented");
    }

    /// **The newest worker lands above the one added before it.**
    ///
    /// Not something the spawning frame has to catch: [`enter_at_head`] orders
    /// the whole roster newest-first and [`crate::app::agent_tree::arrange_owner_tree`]
    /// keeps that order inside every group, so "the newest is on top" is an
    /// invariant of the list. This is the assertion that says so out loud, on the
    /// list the card actually draws.
    #[test]
    fn the_newest_worker_lands_above_the_one_before_it() {
        let app = crewed_fleet();
        let rows = named(&app);
        let position = |wanted: &str| {
            rows.iter()
                .position(|(name, _, _)| name == wanted)
                .unwrap_or_else(|| panic!("{wanted} is missing from the tree"))
        };
        // `via` was split last, so it is the newest pane in the fleet; `mate`
        // came before it and `direct` before that.
        assert!(
            position("mate") < position("fm/direct"),
            "the older worker is above the newer one: {rows:?}"
        );
        assert!(
            position("fm/via") < position("fm/direct"),
            "a second mate's newer worker sank below an older direct one: {rows:?}"
        );
    }

    /// **A worker is drawn inside its Space's box and gets no box of its own.**
    ///
    /// The Space's frame reaches the bottom of its last worker's row, and every
    /// worker row answers with no frame at all — which is what stops the panel
    /// drawing a card per worker inside the card that already contains them.
    #[test]
    fn a_worker_gets_no_box_of_its_own_and_the_space_reaches_over_it() {
        let app = drawing(crewed_fleet());
        if image_card::row_height_cells(&app, row_fold_width(&app, workspace_list_rect(area())))
            .is_none()
        {
            return; // No proportional face on this machine; nothing draws a card.
        }
        let cards = compute_workspace_card_areas(&app, area());
        let entries = workspace_list_entries(&app);
        let (head, crew): (Vec<&crate::app::state::WorkspaceCardArea>, Vec<_>) = cards
            .iter()
            .partition(|card| crew_head(&entries, card.entry_idx).is_none());
        assert_eq!(head.len(), 1, "the Space did not stay one card");
        assert_eq!(crew.len(), 3, "a worker row went missing");
        assert!(
            crew.iter().all(|card| card.card_frame.is_none()),
            "a worker drew a box of its own inside the card it is in"
        );
        let frame = head[0].card_frame.expect("the Space has a box");
        let last = crew.last().expect("a worker row");
        assert_eq!(
            frame.y + frame.height,
            last.rect.y + last.rect.height,
            "the Space's box does not close under its last worker"
        );
    }

    /// **Every worker is still a row, so a click on one still selects its pane.**
    ///
    /// The rows tile with no gap and no overlap, and each worker's own rect is
    /// inside the box drawn around it. Hit testing resolves a row from a `y`
    /// through these rects and nothing else, so this *is* the argument that
    /// moving the ink did not move the click.
    #[test]
    fn the_rows_still_tile_so_a_click_still_lands_on_the_worker() {
        let app = drawing(crewed_fleet());
        if image_card::row_height_cells(&app, row_fold_width(&app, workspace_list_rect(area())))
            .is_none()
        {
            return;
        }
        let cards = compute_workspace_card_areas(&app, area());
        let entries = workspace_list_entries(&app);
        let frame = cards[0].card_frame.expect("the Space has a box");
        let mut next = cards[0].rect.y;
        for card in &cards {
            assert!(card.rect.height > 0, "a row with no cells is not a row");
            assert_eq!(card.rect.y, next, "the rows stopped tiling at {card:?}");
            next = card.rect.y + card.rect.height;
            if crew_head(&entries, card.entry_idx).is_none() {
                continue;
            }
            assert!(
                card.rect.y >= frame.y && card.rect.y + card.rect.height <= frame.y + frame.height,
                "a worker's own row is outside the box it is drawn in: {card:?}"
            );
            assert!(
                card.agent.is_some(),
                "a worker row lost the pane a click on it resolves to"
            );
        }
    }

    /// **A panel that draws no boxes keeps every row it always had.**
    ///
    /// The crew list is a *box*: below [`card::MIN_FOLD_WIDTH`] a row is a
    /// styled line, there is nothing to be inside of, and a worker is its own
    /// row with its own height and its own air around it, exactly as before.
    ///
    /// The pixel path has nothing to do with it. That is the change: the fold
    /// used to wait on a pixel card being published, so the very panel the
    /// captain runs — a character card at 42 columns — drew a card per worker
    /// inside the card that already contained them.
    #[test]
    fn a_panel_that_draws_no_boxes_keeps_every_worker_row_it_had() {
        let app = crewed_fleet();
        let narrow = Rect::new(0, 0, card::MIN_FOLD_WIDTH - 1, 46);
        assert!(
            !RowShell::for_fold_width(row_fold_width(&app, workspace_list_rect(narrow))).is_card(),
            "this panel has to be one that draws no boxes at all"
        );
        let cards = compute_workspace_card_areas(&app, narrow);
        let entries = workspace_list_entries(&app);
        assert_eq!(cards.len(), entries.len(), "a row went missing");
        assert!(
            cards.iter().all(|card| card.card_frame.is_none()),
            "a shell was drawn below the width the shell starts at"
        );
    }

    /// **The character card merges too, with no pixel card anywhere near it.**
    ///
    /// The captain's own panel: 42 columns, `session_kitty_graphics=false`. The
    /// box is the character shell's, so the workers fold into their Space's card
    /// and none of them opens one — which is the whole of the reported bug.
    #[test]
    fn a_character_card_holds_its_workers_just_as_a_pixel_one_does() {
        let app = crewed_fleet();
        let fold = row_fold_width(&app, workspace_list_rect(area()));
        assert!(
            !crew_is_drawn(&app, fold),
            "this fixture has to be one with no pixel card"
        );
        assert!(RowShell::for_fold_width(fold).is_card());

        let cards = compute_workspace_card_areas(&app, area());
        let entries = workspace_list_entries(&app);
        let (head, crew): (Vec<&crate::app::state::WorkspaceCardArea>, Vec<_>) = cards
            .iter()
            .partition(|card| crew_head(&entries, card.entry_idx).is_none());
        assert_eq!(head.len(), 1, "the Space did not stay one card");
        assert_eq!(crew.len(), 3, "a worker row went missing");
        assert!(
            crew.iter().all(|card| card.card_frame.is_none()),
            "a worker drew a box of its own inside the card it is in"
        );
        let frame = head[0].card_frame.expect("the Space has a box");
        let last = crew.last().expect("a worker row");
        assert_eq!(
            frame.y + frame.height,
            last.rect.y + last.rect.height,
            "the Space's box does not close under its last worker"
        );
    }
}

/// **A worker list survives being scrolled through.**
///
/// The card's box is drawn by its *head*, so the two rows that can come apart
/// are the ones the panel cuts: a worker whose head has scrolled off the top,
/// and a list that runs off the bottom. Both were live defects of the first
/// implementation — the first drew nothing at all where the workers should be,
/// the second drew rows past the border the layout had already closed.
#[cfg(test)]
mod a_scrolled_panel_still_draws_its_workers {
    use super::a_space_card_carries_its_own_workers::crewed_fleet;
    use super::*;

    fn drawing(mut app: AppState) -> AppState {
        app.kitty_graphics_enabled = true;
        app.kitty_graphics_capability_confirmed = true;
        app.sidebar_card_shapes = true;
        app.view.sidebar_card_layers_published = true;
        app.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        app
    }

    fn area() -> Rect {
        Rect::new(0, 0, 42, 46)
    }

    /// **A worker whose Space has scrolled off is a card of its own again.**
    ///
    /// There is no box above it to be inside of, so nesting it would leave a row
    /// with no frame, no ink and no border — an invisible row where a worker is.
    #[test]
    fn a_worker_whose_head_scrolled_off_gets_its_own_box_back() {
        let mut app = drawing(crewed_fleet());
        if image_card::row_height_cells(&app, row_fold_width(&app, workspace_list_rect(area())))
            .is_none()
        {
            return; // No proportional face on this machine.
        }
        let entries = workspace_list_entries(&app);
        assert!(crew_head(&entries, 1).is_some(), "row 1 heads no crew list");

        // Scrolled past the Space itself.
        app.workspace_scroll = 1;
        assert_eq!(
            drawn_crew_head(&app, &entries, 1),
            None,
            "a worker was still nested under a head the panel is not drawing"
        );
        let cards = compute_workspace_card_areas(&app, area());
        assert!(!cards.is_empty(), "the scrolled panel drew nothing at all");
        for card in &cards {
            assert!(
                card.card_frame.is_some(),
                "a row with no head above it drew neither a box nor ink: {card:?}"
            );
        }
    }

    /// **A list that runs off the bottom closes its box under the last row it
    /// drew, and draws no more than that.**
    ///
    /// The card's own height comes from the crew it is *drawn* with, so a card
    /// handed more workers than the panel laid out would paint them past its own
    /// closing edge.
    #[test]
    fn a_list_cut_by_the_panels_floor_stops_where_the_panel_does() {
        let app = drawing(crewed_fleet());
        let full = area();
        if image_card::row_height_cells(&app, row_fold_width(&app, workspace_list_rect(full)))
            .is_none()
        {
            return;
        }
        let entries = workspace_list_entries(&app);
        let all = compute_workspace_card_areas(&app, full);
        assert_eq!(
            all.len(),
            entries.len(),
            "the fixture did not fit to begin with"
        );

        // Exactly the Space and one worker, plus the rows the panel reserves for
        // its own header and footer — so the last two workers have nowhere to go.
        let cut = Rect::new(
            0,
            0,
            42,
            all[0].rect.height
                + all[1].rect.height
                + WORKSPACE_SECTION_HEADER_ROWS
                + WORKSPACE_SECTION_FOOTER_ROWS,
        );
        let cards = compute_workspace_card_areas(&app, cut);
        assert!(
            cards.len() < entries.len(),
            "the fixture was not actually cut: {} rows in {:?}",
            cards.len(),
            cut
        );
        let head = cards
            .iter()
            .find(|card| crew_head(&entries, card.entry_idx).is_none())
            .expect("the Space's own row");
        let frame = head.card_frame.expect("the Space has a box");
        let last = cards.last().expect("a laid-out row");
        assert_eq!(
            frame.y + frame.height,
            last.rect.y + last.rect.height,
            "the box does not close under the last worker the panel drew"
        );
    }
}

/// The tray's rows belong to the tree the moment the tray stops drawing.
///
/// [`tray::reserved_rows`] returning `0` is only half of the promise. The other
/// half — the half a reader of that function cannot check — is that the rows it
/// stops reserving are *taken*: that the tree lays more workers out into them
/// rather than leaving a band of panel below the last card. A tray switched off
/// that left ten dead rows at the foot of the panel would satisfy every
/// existing test in this file, because every one of them asks about the tray's
/// own geometry and none of them asks what the tree did with what it gave back.
///
/// So this module asks the tree instead, at three joints:
///
/// - the body grows by exactly the rows the tray had reserved,
/// - more workers are drawn, and the lowest one drawn reaches down past where
///   the tray's top edge used to be,
/// - and a tree already scrolled to its tray-on bottom refills through the real
///   view pass, rather than holding a stale offset that would leave the
///   reclaimed rows blank until the reader scrolled.
#[cfg(test)]
mod the_trays_rows_go_back_to_the_tree {
    use super::*;
    use crate::ui::sidebar::AgentState;
    use crate::workspace::Workspace;

    /// `mates` second mates under the first, each running one named idle agent
    /// delegated inside its own Space — the shape the tree draws crew rows for.
    ///
    /// **A mate per worker, not one mate carrying all of them.** A worker is
    /// drawn inside its own mate's box and goes back to a card of its own the
    /// moment that box scrolls off the top ([`drawn_crew_head`]), so a single
    /// mate holding twenty workers changes the tree's whole height as soon as
    /// the panel is scrolled past it. That is a fact about the crew list, and
    /// this module is asking about the tray.
    fn fleet(mates: usize) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("firstmate")];
        let mut panes = Vec::with_capacity(mates);
        for index in 0..mates {
            let mut mate = Workspace::test_new(&format!("2ndmate-{index}"));
            panes.push(mate.test_split(ratatui::layout::Direction::Vertical));
            app.workspaces.push(mate);
        }
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        for (index, pane) in panes.iter().enumerate() {
            let ws_idx = index + 1;
            app.workspaces[ws_idx].metadata_tokens.patch(
                std::collections::HashMap::from([(
                    "owner".to_string(),
                    Some("firstmate".to_string()),
                )]),
                None,
                std::time::Instant::now(),
            );
            let space_id = app.workspaces[ws_idx].id.clone();
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[pane]
                .attached_terminal_id
                .clone();
            let Some(terminal) = app.terminals.get_mut(&terminal_id) else {
                continue;
            };
            terminal.set_agent_name(format!("worker-{index}"));
            terminal.state = AgentState::Idle;
            terminal.created_by = Some(crate::api::schema::PaneOrigin {
                pane_id: format!("creator-{index}"),
                workspace_id: space_id,
            });
        }
        app
    }

    /// A panel wide enough to draw cards and short enough that the fleet
    /// overflows it, which is the only condition under which the question means
    /// anything: a tree that already fits has no use for the rows either way.
    const PANEL: Rect = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 30,
    };

    /// The whole terminal the view is computed against. The sidebar is pinned
    /// to [`PANEL`]'s width so the panel the view lays out is the panel the
    /// direct-layout assertions measure.
    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };

    #[test]
    fn switching_the_tray_off_hands_its_rows_to_the_tree_and_the_tree_fills_them() {
        let mut app = fleet(20);
        let list = workspace_list_rect(PANEL);

        app.sidebar_signal_tray.enabled = true;
        let reserved = tray::reserved_rows(&app, list);
        assert!(
            reserved > 0,
            "the panel is too small to hold the tray, so there is nothing to reclaim"
        );
        let with_tray_body = workspace_list_body_rect(&app, list, false);
        let with_tray = workspace_list_scroll_metrics(&app, list);
        let (with_tray_cards, _) = compute_workspace_list_areas(&app, PANEL);
        let tray_top = tray::tray_rect(&app, list).y;

        app.sidebar_signal_tray.enabled = false;
        let no_tray_body = workspace_list_body_rect(&app, list, false);
        let without_tray = workspace_list_scroll_metrics(&app, list);
        let (without_tray_cards, _) = compute_workspace_list_areas(&app, PANEL);

        assert_eq!(
            no_tray_body.height,
            with_tray_body.height + reserved,
            "the body did not grow by the rows the tray stopped reserving"
        );
        assert!(
            without_tray.viewport_rows > with_tray.viewport_rows,
            "the tree draws no more rows without the tray ({} then {})",
            with_tray.viewport_rows,
            without_tray.viewport_rows
        );
        assert!(
            without_tray.max_offset_from_bottom < with_tray.max_offset_from_bottom,
            "the tree still has as far to scroll with the tray gone"
        );

        let lowest = |cards: &[crate::app::state::WorkspaceCardArea]| {
            cards.last().map(|card| card.rect.y + card.rect.height)
        };
        let with_tray_lowest = lowest(&with_tray_cards).expect("the panel drew rows with the tray");
        let without_tray_lowest =
            lowest(&without_tray_cards).expect("the panel drew rows without the tray");
        assert!(
            with_tray_cards.len() < without_tray_cards.len(),
            "the same number of rows was drawn into a taller body"
        );
        assert!(
            with_tray_lowest <= tray_top,
            "a row was drawn over the tray's own rect"
        );
        assert!(
            without_tray_lowest > tray_top,
            "the lowest worker stops where the tray used to start ({without_tray_lowest} against \
             a tray top of {tray_top}), so the reclaimed rows are drawing nothing"
        );
    }

    /// The transition, not the two end states: a tree parked at the bottom of
    /// the tray-on layout must refill when the tray goes, rather than holding
    /// an offset that is now past the end and leaving the reclaimed rows blank.
    ///
    /// The renormalisation is [`crate::ui::compute_view`]'s, which is why this
    /// goes through the view rather than calling the layout directly — the
    /// direct call is exactly the path that would still hold the stale offset.
    #[test]
    fn a_tree_parked_at_the_tray_on_bottom_refills_when_the_tray_goes() {
        let mut app = fleet(20);
        app.sidebar_width = PANEL.width;
        let list = workspace_list_rect(PANEL);

        app.sidebar_signal_tray.enabled = true;
        let reserved = tray::reserved_rows(&app, list);
        app.workspace_scroll = workspace_list_scroll_metrics(&app, list).max_offset_from_bottom;
        assert!(
            app.workspace_scroll > 0,
            "the fleet did not overflow the panel, so there is no stale offset to hold"
        );
        let parked = app.workspace_scroll;

        app.sidebar_signal_tray.enabled = false;
        crate::ui::compute_view(&mut app, SCREEN);

        assert!(
            app.workspace_scroll < parked,
            "the view kept the tray-on offset ({parked}), so the reclaimed rows draw nothing"
        );

        let body = workspace_list_body_rect(&app, list, false);
        let lowest = app
            .view
            .workspace_card_areas
            .last()
            .map(|card| card.rect.y + card.rect.height)
            .expect("the panel drew rows after the tray went");
        let unused = (body.y + body.height).saturating_sub(lowest);
        assert!(
            unused < reserved,
            "{unused} of the tray's {reserved} rows are still blank under the lowest worker"
        );
    }
}
