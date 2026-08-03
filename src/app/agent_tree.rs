//! Derive the sidebar tree's parent/child shape from published metadata.
//!
//! Herdr already knows who owns whom: an entity names itself — a pane with
//! `pane report-agent --name` (`TerminalState::agent_name`), a Space with its
//! own label — and names its owner with an `owner` metadata token
//! (`pane report-metadata` / `workspace report-metadata`). This module reads
//! those two facts and nothing else, so there is exactly one source of truth
//! for parentage and no script has to declare it a second time.
//!
//! Nothing here knows what an entity *is*. [`arrange_owner_tree`] takes bare
//! name/owner pairs, so the Spaces tree can run Spaces and panes through one
//! walk and get one set of connectors over both.
//!
//! The relation is a *runtime* fact, so it lives here rather than in the
//! drawing code; [`crate::ui::sidebar`] only turns the depth this module
//! assigns into connector glyphs.

use crate::ui::AgentPanelEntry;

/// The metadata token a pane uses to name its owner.
pub(crate) const OWNER_TOKEN: &str = "owner";

/// The `relation` value for a sub agent.
///
/// It has no [`AgentRelation`] variant on purpose: a sub agent runs *inside*
/// another agent's pane rather than owning one, so no pane can currently hold
/// this relation. It is still legal to filter on — the Sub Agents tab does
/// exactly that — and it starts matching by itself if sub agents ever get panes.
pub(crate) const SUB_AGENT_RELATION: &str = "sub_agent";

/// Every legal value of the view grammar's `relation` field.
///
/// One list so the validator, the evaluator, and the category selector cannot
/// drift apart: a value that is not here is rejected at both doors.
pub(crate) const RELATION_VALUES: [&str; 4] =
    ["first_mate", "second_mate", "worker", SUB_AGENT_RELATION];

/// How deep the panel will *draw* before it stops indenting.
///
/// The captain's sketch is three levels (First Mate, Second Mate, Worker), and
/// each level costs three columns off a sidebar that is routinely 26 wide. Past
/// this depth the tree keeps its logical shape — parents still own children,
/// categories still classify — but every deeper row draws at the cap so a
/// runaway `owner` chain can never indent a name off the panel.
pub(crate) const MAX_DISPLAY_DEPTH: u8 = 2;

/// The column a node at `depth` is actually drawn in.
///
/// The one place the cap is applied, so the connector maths here and the glyph
/// drawing in [`crate::ui::sidebar`] cannot disagree about which rows share a
/// column.
pub(crate) fn display_depth(depth: u8) -> u8 {
    depth.min(MAX_DISPLAY_DEPTH)
}

/// Where a pane sits in the ownership tree.
///
/// This is derived, never stored: it is recomputed from `agent_name` and the
/// `owner` token every time the panel is built, so it cannot drift from the
/// panes it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentRelation {
    /// Owns nobody above it — the root of a tree.
    FirstMate,
    /// Owned by a first mate.
    SecondMate,
    /// Owned by a second mate or deeper.
    Worker,
}

impl AgentRelation {
    /// The relation a pane at `depth` has.
    pub(crate) fn from_depth(depth: u8) -> Self {
        match depth {
            0 => Self::FirstMate,
            1 => Self::SecondMate,
            _ => Self::Worker,
        }
    }

    /// Stable wire/config name. Matched by the Agents view grammar, so these
    /// strings are part of the API surface.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FirstMate => "first_mate",
            Self::SecondMate => "second_mate",
            Self::Worker => "worker",
        }
    }
}

/// What one entity says about its place in the tree, and nothing else.
///
/// Both fields are what the entity *published*, not what it turned out to be:
/// resolving them against the rest of the fleet is this module's whole job.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OwnedNode<'a> {
    /// The handle another entity's `owner` token names.
    pub name: Option<&'a str>,
    /// The `owner` token, naming whoever owns this entity.
    pub owner: Option<&'a str>,
}

/// Where one node landed once the tree was walked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    /// Index back into the slice the caller passed in.
    pub index: usize,
    pub depth: u8,
    /// For each ancestor level, whether that level still has a sibling below,
    /// which is what decides between a `│` continuation and blank space.
    pub ancestors_continue: Vec<bool>,
    /// Whether this is the last row drawn in its own column before that column
    /// closes, picking `└` over `├`.
    ///
    /// Decided on [`display_depth`], not [`Self::depth`]: past
    /// [`MAX_DISPLAY_DEPTH`] a whole subtree flattens into the cap column, and a
    /// glyph that ignored that would put two `└` in one column — the deepest row
    /// of a clamped subtree closing the column, then a later true sibling of its
    /// parent closing it again.
    pub is_last_child: bool,
}

/// Depth-first placement for `nodes`, ordered so every child follows its parent.
///
/// This is the whole tree contract in one call: cycles, self-ownership,
/// duplicate names and owners nobody answers to all degrade to roots rather
/// than dropping a node, because an unreachable entity is one the user cannot
/// rescue. Sibling order is whatever order the caller established.
pub(crate) fn arrange_owner_tree(nodes: &[OwnedNode<'_>]) -> Vec<Placement> {
    let parents = resolve_parents(nodes);
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (idx, parent) in parents.iter().enumerate() {
        match parent {
            Some(parent_idx) => children[*parent_idx].push(idx),
            None => roots.push(idx),
        }
    }
    walk_tree(&children, &roots)
}

/// Depth-first walk of an explicit child map, stamping depth and connectors.
///
/// Exposed separately from [`arrange_owner_tree`] because the Spaces tree
/// composes two different kinds of parentage — worktree membership and the
/// `owner` token — into one child list before walking it, and both kinds have
/// to draw from the same connector maths.
pub(crate) fn walk_tree(children: &[Vec<usize>], roots: &[usize]) -> Vec<Placement> {
    let mut order: Vec<(usize, u8, Vec<bool>)> = Vec::with_capacity(children.len());
    for (position, root) in roots.iter().enumerate() {
        visit(
            *root,
            0,
            &Vec::new(),
            position + 1 < roots.len(),
            children,
            &mut order,
        );
    }

    // The scan runs on drawn columns rather than logical depths, because that
    // is what the glyph describes. Below the cap the two are the same walk; at
    // the cap a clamped child sits in its parent's own column, so it has to
    // count as a later row in that column instead of closing it early.
    let columns: Vec<u8> = order
        .iter()
        .map(|(_, depth, _)| display_depth(*depth))
        .collect();
    order
        .into_iter()
        .enumerate()
        .map(|(position, (index, depth, ancestors))| Placement {
            index,
            depth,
            ancestors_continue: ancestors,
            // "Nothing later lands in my column before it closes", which the
            // flattened order makes a single forward scan.
            is_last_child: {
                let column = columns[position];
                columns[position + 1..]
                    .iter()
                    .take_while(|next| **next >= column)
                    .all(|next| *next > column)
            },
        })
        .collect()
}

/// How deep each node sits, in input order. Ordering is irrelevant here: this
/// answers *what a node is*, which must not depend on what is on screen.
pub(crate) fn owner_depths(nodes: &[OwnedNode<'_>]) -> Vec<u8> {
    let parents = resolve_parents(nodes);
    (0..nodes.len())
        .map(|idx| {
            let mut depth: u8 = 0;
            let mut cursor = parents[idx];
            while let Some(parent) = cursor {
                depth = depth.saturating_add(1);
                cursor = parents[parent];
            }
            depth
        })
        .collect()
}

fn agent_nodes(entries: &[AgentPanelEntry]) -> Vec<OwnedNode<'_>> {
    entries
        .iter()
        .map(|entry| OwnedNode {
            name: entry.agent_name.as_deref(),
            owner: entry.owner.as_deref(),
        })
        .collect()
}

/// Stamp each entry's [`AgentRelation`] from the *whole* fleet.
///
/// Runs before any filtering, which is the whole point: what a pane **is** must
/// not depend on what is currently on screen. A worker whose second mate is
/// filtered out is still a worker, so the category selector cannot make panes
/// change category by looking at them.
pub(crate) fn classify_agent_relations(entries: &mut [AgentPanelEntry]) {
    let depths = owner_depths(&agent_nodes(entries));
    for (entry, depth) in entries.iter_mut().zip(depths) {
        entry.relation = AgentRelation::from_depth(depth);
    }
}

/// Resolve each entry's parent, then reorder the list depth-first.
///
/// Runs *after* filtering and sorting, so an entry's parent is its nearest
/// surviving ancestor: hiding a second mate re-parents its workers onto the
/// first mate instead of leaving a connector pointing at a row that is not
/// there. Sibling order is whatever order the caller already established, so
/// the panel's sort still decides what comes first within a level.
///
/// Only the *drawing* fields are touched. [`AgentRelation`] is left exactly as
/// [`classify_agent_relations`] set it, so indentation can shrink to fit the
/// visible set without a pane silently changing category.
pub(crate) fn arrange_agent_tree(entries: &mut Vec<AgentPanelEntry>) {
    if entries.is_empty() {
        return;
    }

    let order = arrange_owner_tree(&agent_nodes(entries));

    debug_assert_eq!(
        order.len(),
        entries.len(),
        "every entry must appear exactly once in the arranged tree"
    );

    apply_order(entries, order);
}

/// Map every entry to its parent's index, or `None` when it is a root.
///
/// A pane is a root when it declares no owner, names an owner that no visible
/// pane answers to, or sits on a cycle. Falling back to "root" rather than
/// dropping the entry is deliberate: an unreachable pane is a pane the user
/// cannot rescue.
fn resolve_parents(nodes: &[OwnedNode<'_>]) -> Vec<Option<usize>> {
    let mut by_name: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        if let Some(name) = node.name {
            // First writer wins, so a duplicated name cannot silently steal
            // another node's children on a later frame.
            by_name.entry(name).or_insert(idx);
        }
    }

    let direct: Vec<Option<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let parent = *by_name.get(node.owner?)?;
            (parent != idx).then_some(parent)
        })
        .collect();

    // Break cycles: anything that cannot reach a root in `len` hops is on one.
    direct
        .iter()
        .map(|parent| {
            let mut cursor = *parent;
            let mut hops = 0usize;
            while let Some(next) = cursor {
                if hops > nodes.len() {
                    return None;
                }
                cursor = direct[next];
                hops += 1;
            }
            *parent
        })
        .collect()
}

/// Emit `node` then its children, recording the depth and the ancestor lines
/// that are still open at that point.
fn visit(
    node: usize,
    depth: u8,
    ancestors: &[bool],
    has_next_sibling: bool,
    children: &[Vec<usize>],
    out: &mut Vec<(usize, u8, Vec<bool>)>,
) {
    out.push((node, depth, ancestors.to_vec()));

    let mut child_ancestors = ancestors.to_vec();
    child_ancestors.push(has_next_sibling);

    let kids = &children[node];
    for (position, child) in kids.iter().enumerate() {
        visit(
            *child,
            depth.saturating_add(1),
            &child_ancestors,
            position + 1 < kids.len(),
            children,
            out,
        );
    }
}

/// Rewrite `entries` into `order`, stamping the tree fields as it goes.
fn apply_order(entries: &mut Vec<AgentPanelEntry>, order: Vec<Placement>) {
    let mut taken: Vec<Option<AgentPanelEntry>> = entries.drain(..).map(Some).collect();

    for placement in order {
        let Some(mut entry) = taken[placement.index].take() else {
            continue;
        };
        entry.depth = placement.depth;
        entry.ancestors_continue = placement.ancestors_continue;
        entry.is_last_child = placement.is_last_child;
        entries.push(entry);
    }

    // Defensive: anything the walk missed still has to reach the tree.
    for leftover in taken.iter_mut() {
        if let Some(mut entry) = leftover.take() {
            entry.depth = 0;
            entry.ancestors_continue = Vec::new();
            entry.is_last_child = true;
            entries.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, owner: Option<&str>) -> AgentPanelEntry {
        let mut entry = AgentPanelEntry::test_new(name);
        entry.owner = owner.map(str::to_string);
        entry
    }

    fn shape(entries: &[AgentPanelEntry]) -> Vec<(String, u8, bool)> {
        entries
            .iter()
            .map(|entry| {
                (
                    entry.agent_name.clone().unwrap_or_default(),
                    entry.depth,
                    entry.is_last_child,
                )
            })
            .collect()
    }

    #[test]
    fn worker_nests_under_its_owning_second_mate() {
        let mut entries = vec![
            entry("first", None),
            entry("second", Some("first")),
            entry("worker", Some("second")),
        ];

        classify_agent_relations(&mut entries);
        arrange_agent_tree(&mut entries);

        assert_eq!(
            shape(&entries),
            vec![
                ("first".to_string(), 0, true),
                ("second".to_string(), 1, true),
                ("worker".to_string(), 2, true),
            ]
        );
        assert_eq!(entries[0].relation, AgentRelation::FirstMate);
        assert_eq!(entries[1].relation, AgentRelation::SecondMate);
        assert_eq!(entries[2].relation, AgentRelation::Worker);
    }

    #[test]
    fn children_follow_their_parent_even_when_listed_out_of_order() {
        let mut entries = vec![
            entry("worker", Some("second")),
            entry("other", None),
            entry("second", Some("first")),
            entry("first", None),
        ];

        arrange_agent_tree(&mut entries);

        // `other` is a root and keeps its position relative to the other root;
        // `first` pulls its subtree along behind it.
        assert_eq!(
            shape(&entries),
            vec![
                ("other".to_string(), 0, false),
                ("first".to_string(), 0, true),
                ("second".to_string(), 1, true),
                ("worker".to_string(), 2, true),
            ]
        );
    }

    #[test]
    fn sibling_order_is_preserved_and_only_the_last_is_last() {
        let mut entries = vec![
            entry("first", None),
            entry("a", Some("first")),
            entry("b", Some("first")),
        ];

        arrange_agent_tree(&mut entries);

        assert_eq!(
            shape(&entries),
            vec![
                ("first".to_string(), 0, true),
                ("a".to_string(), 1, false),
                ("b".to_string(), 1, true),
            ]
        );
    }

    #[test]
    fn an_owner_nobody_answers_to_leaves_the_pane_reachable_as_a_root() {
        let mut entries = vec![entry("orphan", Some("ghost"))];

        arrange_agent_tree(&mut entries);

        assert_eq!(shape(&entries), vec![("orphan".to_string(), 0, true)]);
        assert_eq!(entries[0].relation, AgentRelation::FirstMate);
    }

    #[test]
    fn a_cycle_does_not_hang_or_drop_a_pane() {
        let mut entries = vec![entry("a", Some("b")), entry("b", Some("a"))];

        arrange_agent_tree(&mut entries);

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.depth == 0));
    }

    #[test]
    fn a_pane_owning_itself_is_a_root() {
        let mut entries = vec![entry("solo", Some("solo"))];

        arrange_agent_tree(&mut entries);

        assert_eq!(shape(&entries), vec![("solo".to_string(), 0, true)]);
    }

    #[test]
    fn hiding_a_middle_pane_reparents_its_workers_onto_the_survivor() {
        // The second mate was filtered out before arranging, so the worker's
        // owner is no longer present.
        let mut entries = vec![entry("first", None), entry("worker", Some("second"))];

        arrange_agent_tree(&mut entries);

        // The worker stays visible rather than dangling off a missing row.
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .any(|entry| { entry.agent_name.as_deref() == Some("worker") && entry.depth == 0 }));
    }

    #[test]
    fn a_duplicated_name_does_not_steal_children() {
        let mut entries = vec![
            entry("dup", None),
            entry("dup", None),
            entry("child", Some("dup")),
        ];

        arrange_agent_tree(&mut entries);

        assert_eq!(entries.len(), 3);
        let child = entries
            .iter()
            .find(|entry| entry.agent_name.as_deref() == Some("child"));
        assert_eq!(child.map(|entry| entry.depth), Some(1));
    }

    #[test]
    fn ancestors_continue_tracks_open_lines() {
        let mut entries = vec![
            entry("first", None),
            entry("a", Some("first")),
            entry("a_child", Some("a")),
            entry("b", Some("first")),
        ];

        arrange_agent_tree(&mut entries);

        let a_child = entries
            .iter()
            .find(|entry| entry.agent_name.as_deref() == Some("a_child"))
            .expect("a_child present");
        // `a` still has sibling `b` below it, so the level-1 line stays open.
        assert_eq!(a_child.ancestors_continue, vec![false, true]);
    }

    #[test]
    fn a_workers_category_survives_its_second_mate_being_filtered_out() {
        // Classify against the whole fleet, the way the panel does...
        let mut fleet = vec![
            entry("first", None),
            entry("second", Some("first")),
            entry("worker", Some("second")),
        ];
        classify_agent_relations(&mut fleet);
        assert_eq!(fleet[2].relation, AgentRelation::Worker);

        // ...then drop the middle pane the way a view filter would, and
        // re-arrange only for drawing.
        fleet.remove(1);
        arrange_agent_tree(&mut fleet);

        let worker = fleet
            .iter()
            .find(|entry| entry.agent_name.as_deref() == Some("worker"))
            .expect("worker present");
        // It draws unindented because its parent is gone, but it is still a
        // worker — the Workers tab must not lose it.
        assert_eq!(worker.depth, 0);
        assert_eq!(worker.relation, AgentRelation::Worker);
    }

    #[test]
    fn every_pane_lands_in_exactly_one_category() {
        let mut entries = vec![
            entry("first", None),
            entry("second", Some("first")),
            entry("worker", Some("second")),
            entry("deep", Some("worker")),
            entry("orphan", Some("ghost")),
        ];
        classify_agent_relations(&mut entries);

        let relations: Vec<AgentRelation> = entries.iter().map(|entry| entry.relation).collect();
        assert_eq!(
            relations,
            vec![
                AgentRelation::FirstMate,
                AgentRelation::SecondMate,
                AgentRelation::Worker,
                // Past the sketch's three levels everything is still a worker,
                // so nothing falls out of the four tabs.
                AgentRelation::Worker,
                // An owner nobody answers to is a root, so it stays reachable.
                AgentRelation::FirstMate,
            ]
        );
    }

    #[test]
    fn every_relation_variant_is_a_legal_filter_value() {
        // The validator checks against RELATION_VALUES; if a variant's wire
        // name were missing from it, that relation would be unfilterable.
        for relation in [
            AgentRelation::FirstMate,
            AgentRelation::SecondMate,
            AgentRelation::Worker,
        ] {
            assert!(
                RELATION_VALUES.contains(&relation.as_str()),
                "{} is not a legal filter value",
                relation.as_str()
            );
        }
        assert!(RELATION_VALUES.contains(&SUB_AGENT_RELATION));
    }

    #[test]
    fn a_clamped_row_does_not_close_the_column_its_parent_still_shares() {
        // `sub` is a rank past the cap, so it draws in `worker`'s own column.
        let mut entries = vec![
            entry("first", None),
            entry("second", Some("first")),
            entry("worker", Some("second")),
            entry("sub", Some("worker")),
            entry("worker2", Some("second")),
        ];

        arrange_agent_tree(&mut entries);

        assert_eq!(
            shape(&entries),
            vec![
                ("first".to_string(), 0, true),
                ("second".to_string(), 1, true),
                // `worker` keeps `├` because rows still follow it in its column.
                ("worker".to_string(), 2, false),
                // `sub` is the last child of `worker`, but `worker2` still lands
                // in the same column below it, so it must not draw `└`.
                ("sub".to_string(), 3, false),
                ("worker2".to_string(), 2, true),
            ]
        );
    }

    #[test]
    fn the_capped_column_closes_exactly_once() {
        let mut entries = vec![
            entry("first", None),
            entry("second", Some("first")),
            entry("worker", Some("second")),
            entry("sub_a", Some("worker")),
            entry("sub_b", Some("worker")),
            entry("deeper", Some("sub_b")),
            entry("worker2", Some("second")),
            entry("sub_c", Some("worker2")),
        ];

        arrange_agent_tree(&mut entries);

        let closers: Vec<&str> = entries
            .iter()
            .filter(|entry| display_depth(entry.depth) == MAX_DISPLAY_DEPTH && entry.is_last_child)
            .filter_map(|entry| entry.agent_name.as_deref())
            .collect();
        // One `└` for the whole flattened run, on its genuinely final row.
        assert_eq!(closers, vec!["sub_c"]);
    }

    #[test]
    fn clamping_leaves_every_shallow_row_untouched() {
        // The same fleet with and without a past-the-cap descendant: adding one
        // must not move a `└` anywhere above the cap.
        let fleet = || {
            vec![
                entry("first", None),
                entry("a", Some("first")),
                entry("a_child", Some("a")),
                entry("b", Some("first")),
            ]
        };
        let mut shallow = fleet();
        let mut deep = fleet();
        deep.push(entry("past_cap", Some("a_child")));

        arrange_agent_tree(&mut shallow);
        arrange_agent_tree(&mut deep);

        let above_cap = |entries: &[AgentPanelEntry]| -> Vec<(String, u8, bool)> {
            shape(entries)
                .into_iter()
                .filter(|(_, depth, _)| *depth < MAX_DISPLAY_DEPTH)
                .collect()
        };
        assert_eq!(above_cap(&shallow), above_cap(&deep));
    }

    #[test]
    fn empty_input_is_left_alone() {
        let mut entries: Vec<AgentPanelEntry> = Vec::new();
        arrange_agent_tree(&mut entries);
        assert!(entries.is_empty());
    }
}
