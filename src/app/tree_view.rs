//! Which node the sidebar tree is rooted on, and the switch between two roots.
//!
//! The tree normally shows the whole fleet. Selecting a second mate *re-roots*
//! it: the selected node takes rank 0, its own children take rank 1, and
//! everything else is simply not in the view. Switching back is the same motion
//! the other way round.
//!
//! Three properties this module is responsible for holding:
//!
//! - **Nothing travels between two views.** A re-root is not a re-layout with
//!   rows moving to new ranks; the view being left is taken apart and the one
//!   being arrived at is put together. That is why the layout only ever swaps
//!   at the single instant the panel is fully dissolved, and why no row is ever
//!   asked to animate from one coordinate to another.
//! - **The transition is one element of the animation engine, not a second
//!   clock.** [`crate::anim::ElementId::Named`] holds the view's own life, so
//!   the panel's dissolve composes with each row's own arrival and departure
//!   instead of competing with it. A worker spawning mid-switch materializes on
//!   its own life while the view it is inside is coming apart on the view's.
//! - **The root is named, not indexed.** Ownership in this tree is already
//!   expressed by name (`crate::app::agent_tree`), and a workspace index moves
//!   whenever a Space is closed or reordered. A root whose node has gone away
//!   degrades back to the fleet rather than pointing at whatever inherited its
//!   index.

use std::time::Instant;

use crate::anim::{ElementId, Lifecycle};

/// The engine element that carries a whole view's arrival and departure.
///
/// One singleton rather than a per-row flag: the whole panel materializes and
/// dematerializes together, and the rows underneath keep their own separate
/// lives so the two can run at once. It has its own family, so a subsystem that
/// reconciles a *shared* family by membership cannot retire it — see
/// [`ElementId::TreeView`].
pub(crate) fn view_element() -> ElementId {
    ElementId::TreeView
}

/// What the sidebar tree is rooted on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum TreeRoot {
    /// Every top-level Space, the way the tree has always drawn.
    #[default]
    Fleet,
    /// One node's subtree, that node drawn at rank 0.
    ///
    /// Held by tree name — the handle an `owner` token names, which for a Space
    /// is its own label — because that is the identity the tree is built from.
    Node(String),
}

impl TreeRoot {
    pub(crate) fn node_name(&self) -> Option<&str> {
        match self {
            Self::Fleet => None,
            Self::Node(name) => Some(name.as_str()),
        }
    }

    pub(crate) fn is_fleet(&self) -> bool {
        matches!(self, Self::Fleet)
    }
}

/// A switch that has begun but whose new root has not been shown yet.
///
/// The old view is coming apart until `commit_at`; the new root is adopted at
/// that instant and starts materializing. Holding the deadline here rather than
/// reading it back off the engine is what lets the app loop *arm* it: a panel
/// mid-dissolve with no other animation running would otherwise park and never
/// finish the switch.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingTreeRoot {
    pub(crate) root: TreeRoot,
    pub(crate) commit_at: Instant,
}

/// A row of a flattened tree, as far as re-rooting is concerned.
///
/// Deliberately not the sidebar's own row type: re-rooting is depth arithmetic
/// over a depth-first sequence and nothing else, so it is stated over the two
/// facts it actually needs and stays testable without a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RootedRow {
    /// Index back into the sequence the caller passed in.
    pub(crate) index: usize,
    /// Depth after re-rooting; the new root is `0`.
    pub(crate) depth: u8,
    /// How many ancestor levels were dropped off the front of this row's
    /// `ancestors_continue`, so a caller can realign its own connector data.
    pub(crate) trimmed_levels: u8,
}

/// The rows of `root`'s subtree, re-depthed so `root` sits at rank 0.
///
/// `names` is the tree name of each row in depth-first order, alongside its
/// depth — exactly what a flattened owner tree already produces. Returns `None`
/// when the named root is not in the tree, which is the caller's signal to draw
/// the whole fleet instead: a root that has gone away must never blank the
/// panel.
pub(crate) fn rooted_rows<'a>(
    rows: impl IntoIterator<Item = (u8, Option<&'a str>)>,
    root: &TreeRoot,
) -> Option<Vec<RootedRow>> {
    let name = root.node_name()?;
    let rows: Vec<(u8, Option<&str>)> = rows.into_iter().collect();
    let start = rows
        .iter()
        .position(|(_, row_name)| *row_name == Some(name))?;
    let root_depth = rows[start].0;

    let mut kept = vec![RootedRow {
        index: start,
        depth: 0,
        trimmed_levels: root_depth,
    }];
    for (index, (depth, _)) in rows.iter().enumerate().skip(start + 1) {
        // The subtree is contiguous in a depth-first flattening, so the first
        // row back at or above the root's own level ends it.
        if *depth <= root_depth {
            break;
        }
        kept.push(RootedRow {
            index,
            depth: depth.saturating_sub(root_depth),
            trimmed_levels: root_depth,
        });
    }
    Some(kept)
}

/// The life a view is given: it forms on arrival and comes apart on departure.
///
/// No idle behaviour at all, which is what keeps a settled panel free: the
/// element exists, holds still, and arms no deadline until the next switch.
pub(crate) fn view_lifecycle(
    enter: Option<crate::anim::Stage>,
    leave: Option<crate::anim::Stage>,
) -> Lifecycle {
    let mut lifecycle = Lifecycle::still();
    lifecycle.mount = enter;
    lifecycle.dismount = leave;
    lifecycle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<(u8, Option<&'static str>)> {
        vec![
            (0, Some("firstmate")),
            (1, Some("2nd-a")),
            (2, Some("worker-a1")),
            (2, Some("worker-a2")),
            (1, Some("2nd-b")),
            (2, Some("worker-b1")),
        ]
    }

    #[test]
    fn a_second_mate_takes_rank_zero_and_its_workers_take_rank_one() {
        let kept = rooted_rows(rows(), &TreeRoot::Node("2nd-a".into())).expect("root is in tree");
        assert_eq!(
            kept.iter()
                .map(|row| (row.index, row.depth))
                .collect::<Vec<_>>(),
            vec![(1, 0), (2, 1), (3, 1)]
        );
        assert!(kept.iter().all(|row| row.trimmed_levels == 1));
    }

    #[test]
    fn everything_outside_the_selected_subtree_is_left_out() {
        let kept = rooted_rows(rows(), &TreeRoot::Node("2nd-b".into())).expect("root is in tree");
        assert_eq!(
            kept.iter().map(|row| row.index).collect::<Vec<_>>(),
            vec![4, 5]
        );
    }

    #[test]
    fn rooting_on_a_leaf_keeps_just_that_leaf() {
        let kept =
            rooted_rows(rows(), &TreeRoot::Node("worker-a1".into())).expect("root is in tree");
        assert_eq!(
            kept.iter()
                .map(|row| (row.index, row.depth))
                .collect::<Vec<_>>(),
            vec![(2, 0)]
        );
    }

    /// A mate whose Space is closed while its view is open must not blank the
    /// panel; the caller falls back to the fleet.
    #[test]
    fn a_root_that_is_no_longer_in_the_tree_does_not_resolve() {
        assert_eq!(rooted_rows(rows(), &TreeRoot::Node("gone".into())), None);
        assert_eq!(rooted_rows(rows(), &TreeRoot::Fleet), None);
    }

    #[test]
    fn rooting_on_the_first_mate_keeps_the_whole_tree() {
        let kept =
            rooted_rows(rows(), &TreeRoot::Node("firstmate".into())).expect("root is in tree");
        assert_eq!(kept.len(), rows().len());
        assert!(kept.iter().all(|row| row.trimmed_levels == 0));
    }
}
