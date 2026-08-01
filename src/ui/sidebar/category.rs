//! The Agents panel's four-way category selector.
//!
//! The captain's sketch puts a tab selector top-left with four categories:
//! First Mate, Second Mate, Workers, Sub Agents. Selecting one is a UI gesture,
//! so it is expressed as a `ui`-tier Agents view
//! ([`crate::agent_view::AgentViewTier::Ui`]) rather than a private filter: a
//! plugin's `api` view still outranks it, a `config` default still shows
//! through when it is cleared, and `herdr agent view get` can read back what
//! the panel is doing.
//!
//! The category itself is server-side vocabulary — the `relation` field of the
//! view grammar — so this module only decides which one is selected and how the
//! tabs are drawn.

use ratatui::layout::Rect;

use crate::agent_view::UI_VIEW_SOURCE;
use crate::api::schema::{
    AgentViewBuiltinField, AgentViewField, AgentViewFilter, AgentViewSetParams, AgentViewValue,
};
use crate::app::agent_tree::AgentRelation;

/// One tab of the selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentCategory {
    FirstMate,
    SecondMate,
    Workers,
    SubAgents,
}

/// Left-to-right tab order, matching the sketch.
pub(crate) const AGENT_CATEGORIES: [AgentCategory; 4] = [
    AgentCategory::FirstMate,
    AgentCategory::SecondMate,
    AgentCategory::Workers,
    AgentCategory::SubAgents,
];

impl AgentCategory {
    /// Full name, used for whichever tab is selected when there is room.
    pub(crate) fn full_label(self) -> &'static str {
        match self {
            Self::FirstMate => "first mate",
            Self::SecondMate => "second mate",
            Self::Workers => "workers",
            Self::SubAgents => "sub agents",
        }
    }

    /// Three-column form, which is all a 26-wide sidebar can spare per tab.
    pub(crate) fn short_label(self) -> &'static str {
        match self {
            Self::FirstMate => "1st",
            Self::SecondMate => "2nd",
            Self::Workers => "wrk",
            Self::SubAgents => "sub",
        }
    }

    /// The `relation` value this category selects.
    ///
    /// `SubAgents` deliberately names a relation no pane can currently hold.
    /// A Claude sub agent runs *inside* another agent's pane — the integration
    /// hook drops events that carry an `agent_id`
    /// (`src/integration/assets/claude/herdr-agent-state.sh`) — so it has no
    /// pane of its own and cannot appear in a panel that enumerates panes.
    /// Naming the empty set honestly is better than pointing this tab at the
    /// worker set and calling two different things by one name.
    pub(crate) fn relation_value(self) -> &'static str {
        match self {
            Self::FirstMate => AgentRelation::FirstMate.as_str(),
            Self::SecondMate => AgentRelation::SecondMate.as_str(),
            Self::Workers => AgentRelation::Worker.as_str(),
            Self::SubAgents => crate::app::agent_tree::SUB_AGENT_RELATION,
        }
    }

    /// The `ui`-tier view that selecting this category installs.
    pub(crate) fn view(self) -> AgentViewSetParams {
        AgentViewSetParams {
            source: UI_VIEW_SOURCE.to_string(),
            label: Some(self.short_label().to_string()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Relation),
                value: AgentViewValue::String(self.relation_value().to_string()),
            }),
            sort: Vec::new(),
        }
    }
}

/// One drawn tab: what it says, where it is, and whether it is selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentCategoryTab {
    pub category: AgentCategory,
    pub label: String,
    pub rect: Rect,
    pub selected: bool,
}

/// Lay the four tabs out across `area`, widest labels that still fit.
///
/// The sidebar is routinely 26 columns and every column here is one the names
/// below do not get, so the labels degrade in three steps rather than
/// truncating: full names for everything, then the full name for the selected
/// tab with the other three abbreviated, then all four abbreviated. If even the
/// abbreviated row does not fit, nothing is drawn and the panel keeps its rows
/// for agents — the selector is never worth an unreadable header.
pub(crate) fn agent_category_tabs(
    area: Rect,
    selected: Option<AgentCategory>,
) -> Vec<AgentCategoryTab> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let labels = choose_labels(area.width, selected);
    let Some(labels) = labels else {
        return Vec::new();
    };

    let mut tabs = Vec::with_capacity(AGENT_CATEGORIES.len());
    // One leading column so the row lines up with the panel's other content.
    let mut x = area.x.saturating_add(1);
    for (category, label) in AGENT_CATEGORIES.iter().zip(labels) {
        let width = label.chars().count() as u16;
        tabs.push(AgentCategoryTab {
            category: *category,
            rect: Rect::new(x, area.y, width, 1),
            label,
            selected: selected == Some(*category),
        });
        x = x.saturating_add(width).saturating_add(1);
    }
    tabs
}

/// Pick the widest label set that fits `width`, or `None` if none does.
fn choose_labels(width: u16, selected: Option<AgentCategory>) -> Option<Vec<String>> {
    let full: Vec<String> = AGENT_CATEGORIES
        .iter()
        .map(|category| category.full_label().to_string())
        .collect();
    let short: Vec<String> = AGENT_CATEGORIES
        .iter()
        .map(|category| category.short_label().to_string())
        .collect();
    let mixed: Vec<String> = AGENT_CATEGORIES
        .iter()
        .map(|category| {
            if selected == Some(*category) {
                category.full_label().to_string()
            } else {
                category.short_label().to_string()
            }
        })
        .collect();

    [full, mixed, short]
        .into_iter()
        .find(|labels| row_width(labels) <= width)
}

/// Columns a label set costs: one leading space, one between each pair, and one
/// trailing column so the last tab cannot end flush against whatever the panel
/// right-aligns next to it.
fn row_width(labels: &[String]) -> u16 {
    let text: usize = labels.iter().map(|label| label.chars().count()).sum();
    let gaps = labels.len().saturating_sub(1);
    (text + gaps + 2).min(u16::MAX as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_agents_selects_a_relation_no_pane_holds() {
        // The runtime cannot currently distinguish a sub agent as its own pane,
        // so this tab is empty by construction rather than a duplicate of
        // Workers. If sub agents ever get panes, this value starts matching.
        assert_eq!(AgentCategory::SubAgents.relation_value(), "sub_agent");
        assert_ne!(
            AgentCategory::SubAgents.relation_value(),
            AgentCategory::Workers.relation_value()
        );
    }

    #[test]
    fn the_three_pane_categories_cover_every_relation() {
        // Union of the pane-bearing tabs must be all of AgentRelation, or a
        // pane would be unreachable through every tab.
        let covered: Vec<&str> = [
            AgentCategory::FirstMate,
            AgentCategory::SecondMate,
            AgentCategory::Workers,
        ]
        .iter()
        .map(|category| category.relation_value())
        .collect();

        for relation in [
            AgentRelation::FirstMate,
            AgentRelation::SecondMate,
            AgentRelation::Worker,
        ] {
            assert!(
                covered.contains(&relation.as_str()),
                "{} is not reachable through any tab",
                relation.as_str()
            );
        }
    }

    #[test]
    fn a_twenty_six_wide_sidebar_still_gets_four_tabs() {
        // The captain runs at 26; the panel is laid out inside width - 1.
        let tabs = agent_category_tabs(Rect::new(0, 0, 25, 1), Some(AgentCategory::Workers));
        assert_eq!(tabs.len(), 4);
        // The selected tab spells itself out; the rest abbreviate.
        assert_eq!(tabs[2].label, "workers");
        assert_eq!(tabs[0].label, "1st");
        let last = tabs.last().expect("four tabs");
        assert!(
            last.rect.x + last.rect.width <= 25,
            "tabs must not overflow the panel"
        );
    }

    #[test]
    fn the_longest_selection_still_fits_twenty_six() {
        // "second mate" is the widest full label, so it is the worst case.
        let tabs = agent_category_tabs(Rect::new(0, 0, 25, 1), Some(AgentCategory::SecondMate));
        assert_eq!(tabs.len(), 4);
        assert_eq!(tabs[1].label, "second mate");
        let last = tabs.last().expect("four tabs");
        assert!(last.rect.x + last.rect.width <= 25);
    }

    #[test]
    fn a_very_narrow_panel_abbreviates_rather_than_truncating() {
        // 17 is the exact minimum: four 3-column labels, three gaps, plus one
        // leading and one trailing column.
        let tabs = agent_category_tabs(Rect::new(0, 0, 17, 1), Some(AgentCategory::SecondMate));
        assert_eq!(tabs.len(), 4);
        assert_eq!(
            tabs.iter()
                .map(|tab| tab.label.as_str())
                .collect::<Vec<_>>(),
            vec!["1st", "2nd", "wrk", "sub"]
        );

        // One column narrower and the selector steps aside entirely rather
        // than giving up the trailing gap and fusing with the control label.
        assert!(
            agent_category_tabs(Rect::new(0, 0, 16, 1), Some(AgentCategory::SecondMate)).is_empty()
        );
    }

    #[test]
    fn the_last_tab_leaves_a_trailing_column() {
        // Otherwise the last tab ends flush against whatever the panel
        // right-aligns beside it and the two read as one word.
        for width in [16u16, 25, 30, 60] {
            let tabs = agent_category_tabs(Rect::new(0, 0, width, 1), None);
            if let Some(last) = tabs.last() {
                assert!(
                    last.rect.x + last.rect.width < width,
                    "tabs touch the right edge at width {width}"
                );
            }
        }
    }

    #[test]
    fn the_captains_thirty_wide_sidebar_spells_out_the_selection() {
        // His real width is 30, not 26. The panel is laid out inside width - 1,
        // and the control label takes the right end, so this is the budget the
        // selector actually gets.
        let tabs = agent_category_tabs(Rect::new(0, 0, 29, 1), Some(AgentCategory::SecondMate));
        assert_eq!(tabs.len(), 4);
        assert_eq!(
            tabs.iter()
                .map(|tab| tab.label.as_str())
                .collect::<Vec<_>>(),
            vec!["1st", "second mate", "wrk", "sub"]
        );
    }

    #[test]
    fn a_panel_too_narrow_for_even_abbreviations_draws_nothing() {
        let tabs = agent_category_tabs(Rect::new(0, 0, 10, 1), None);
        assert!(tabs.is_empty());
    }

    #[test]
    fn a_wide_sidebar_spells_every_tab_out() {
        let tabs = agent_category_tabs(Rect::new(0, 0, 60, 1), None);
        assert_eq!(
            tabs.iter()
                .map(|tab| tab.label.as_str())
                .collect::<Vec<_>>(),
            vec!["first mate", "second mate", "workers", "sub agents"]
        );
    }

    #[test]
    fn tabs_do_not_overlap() {
        let tabs = agent_category_tabs(Rect::new(0, 0, 60, 1), None);
        for pair in tabs.windows(2) {
            assert!(
                pair[0].rect.x + pair[0].rect.width < pair[1].rect.x,
                "tabs must keep a gap so a click lands on one tab only"
            );
        }
    }

    #[test]
    fn the_selected_category_is_the_one_marked_selected() {
        let tabs = agent_category_tabs(Rect::new(0, 0, 60, 1), Some(AgentCategory::Workers));
        let selected: Vec<AgentCategory> = tabs
            .iter()
            .filter(|tab| tab.selected)
            .map(|tab| tab.category)
            .collect();
        assert_eq!(selected, vec![AgentCategory::Workers]);
    }

    #[test]
    fn the_view_filters_on_the_relation_field() {
        let view = AgentCategory::Workers.view();
        assert_eq!(view.source, UI_VIEW_SOURCE);
        match view.filter {
            Some(AgentViewFilter::Eq { field, value }) => {
                assert_eq!(
                    field,
                    AgentViewField::Builtin(AgentViewBuiltinField::Relation)
                );
                assert_eq!(value, AgentViewValue::String("worker".to_string()));
            }
            other => panic!("expected an Eq filter on relation, got {other:?}"),
        }
    }
}
