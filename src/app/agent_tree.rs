//! Derive the Agents panel's parent/child tree from pane metadata.
//!
//! Herdr already knows who owns whom: a pane names itself with
//! `pane report-agent --name` (`TerminalState::agent_name`) and names its owner
//! with an `owner` metadata token (`pane report-metadata`). This module reads
//! those two facts and nothing else, so there is exactly one source of truth
//! for parentage and no script has to declare it a second time.
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

/// Stamp each entry's [`AgentRelation`] from the *whole* fleet.
///
/// Runs before any filtering, which is the whole point: what a pane **is** must
/// not depend on what is currently on screen. A worker whose second mate is
/// filtered out is still a worker, so the category selector cannot make panes
/// change category by looking at them.
pub(crate) fn classify_agent_relations(entries: &mut [AgentPanelEntry]) {
    let parents = resolve_parents(entries);

    let depths: Vec<u8> = (0..entries.len())
        .map(|idx| {
            let mut depth: u8 = 0;
            let mut cursor = parents[idx];
            while let Some(parent) = cursor {
                depth = depth.saturating_add(1);
                cursor = parents[parent];
            }
            depth
        })
        .collect();

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

    let parents = resolve_parents(entries);

    // Children in the caller's order, so sort survives inside each level.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (idx, parent) in parents.iter().enumerate() {
        match parent {
            Some(parent_idx) => children[*parent_idx].push(idx),
            None => roots.push(idx),
        }
    }

    let mut order: Vec<(usize, u8, Vec<bool>)> = Vec::with_capacity(entries.len());
    for (position, root) in roots.iter().enumerate() {
        visit(
            *root,
            0,
            &Vec::new(),
            position + 1 < roots.len(),
            &children,
            &mut order,
        );
    }

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
fn resolve_parents(entries: &[AgentPanelEntry]) -> Vec<Option<usize>> {
    let mut by_name: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        if let Some(name) = entry.agent_name.as_deref() {
            // First writer wins, so a duplicated name cannot silently steal
            // another pane's children on a later frame.
            by_name.entry(name).or_insert(idx);
        }
    }

    let direct: Vec<Option<usize>> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let owner = entry.owner.as_deref()?;
            let parent = *by_name.get(owner)?;
            (parent != idx).then_some(parent)
        })
        .collect();

    // Break cycles: anything that cannot reach a root in `len` hops is on one.
    direct
        .iter()
        .enumerate()
        .map(|(idx, parent)| {
            let mut cursor = *parent;
            let mut hops = 0usize;
            while let Some(next) = cursor {
                if hops > entries.len() {
                    return None;
                }
                cursor = direct[next];
                hops += 1;
            }
            let _ = idx;
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
fn apply_order(entries: &mut Vec<AgentPanelEntry>, order: Vec<(usize, u8, Vec<bool>)>) {
    let mut taken: Vec<Option<AgentPanelEntry>> = entries.drain(..).map(Some).collect();

    for (idx, depth, ancestors) in order {
        let Some(mut entry) = taken[idx].take() else {
            continue;
        };
        entry.depth = depth;
        entry.ancestors_continue = ancestors;
        entries.push(entry);
    }

    // Defensive: anything the walk missed still has to reach the panel.
    for leftover in taken.iter_mut() {
        if let Some(mut entry) = leftover.take() {
            entry.depth = 0;
            entry.ancestors_continue = Vec::new();
            entries.push(entry);
        }
    }

    // `is_last_child` is "no later sibling at my own depth before my parent
    // ends", which the flattened order makes a single backwards scan.
    for idx in 0..entries.len() {
        let depth = entries[idx].depth;
        let last = entries[idx + 1..]
            .iter()
            .take_while(|next| next.depth >= depth)
            .all(|next| next.depth > depth);
        entries[idx].is_last_child = last;
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
    fn empty_input_is_left_alone() {
        let mut entries: Vec<AgentPanelEntry> = Vec::new();
        arrange_agent_tree(&mut entries);
        assert!(entries.is_empty());
    }
}
