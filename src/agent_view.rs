//! Shared owner of the Agents view grammar.
//!
//! The Agents view is a filter tree plus a multi-key sort applied to the
//! Agents panel. It has two front doors: the `agent.view.set` socket API and
//! the declarative `[ui.sidebar.agents.view]` config key. The grammar's
//! *types* live in [`crate::api::schema`] and are shared by both doors through
//! serde; the rules that decide whether a spec is legal live here so neither
//! door can drift from the other. `crate::app::agent_view` evaluates an
//! already-validated spec against app state.

use crate::api::schema::{
    AgentViewBuiltinField, AgentViewBuiltinSortField, AgentViewContext, AgentViewField,
    AgentViewFilter, AgentViewSetParams, AgentViewSortField, AgentViewValue,
};

const MAX_FILTER_DEPTH: usize = 8;
const MAX_FILTER_NODES: usize = 64;
const MAX_FILTER_VALUES: usize = 32;
const MAX_SORT_FIELDS: usize = 8;
const MAX_SOURCE_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 32;
const MAX_TOKEN_CHARS: usize = 32;

/// Source string owned by the `[ui.sidebar.agents.view]` config key.
pub(crate) const CONFIG_VIEW_SOURCE: &str = "config";
/// Source string owned by views the TUI sets for itself.
pub(crate) const UI_VIEW_SOURCE: &str = "ui";

/// Who owns an Agents view, ordered by precedence: later tiers win.
///
/// `config` < `ui` < `api`. A config-declared view is a standing default the
/// user wrote down once; a UI gesture is a deliberate in-the-moment override of
/// that default; an API view comes from a program the user installed and
/// enabled on purpose, so a line of config must never silently displace it.
/// Clearing a tier reveals the next one down rather than jumping straight to
/// the unfiltered panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentViewTier {
    /// Declared in `config.toml`. Reloaded with the config, so it survives a
    /// server restart.
    Config,
    /// Set by the TUI for this session.
    Ui,
    /// Set over the socket API, normally by a plugin.
    Api,
}

impl AgentViewTier {
    /// Short name shown in the Agents panel so the active source is never a
    /// mystery.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Ui => "ui",
            Self::Api => "plugin",
        }
    }
}

/// The tier that reserves `source`, if any. Reserved sources are set by Herdr
/// itself and may not be claimed over the API.
pub(crate) fn reserved_source_owner(source: &str) -> Option<AgentViewTier> {
    match source {
        CONFIG_VIEW_SOURCE => Some(AgentViewTier::Config),
        UI_VIEW_SOURCE => Some(AgentViewTier::Ui),
        _ => None,
    }
}

/// Every Agents view that currently wants to own the panel, one slot per tier.
///
/// Only [`Self::active`] is projected onto the panel; the lower slots stay
/// intact so clearing the winner falls back instead of dropping the user into a
/// state they never asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentViewSlots {
    config: Option<AgentViewSetParams>,
    ui: Option<AgentViewSetParams>,
    api: Option<AgentViewSetParams>,
}

impl AgentViewSlots {
    fn slot(&self, tier: AgentViewTier) -> &Option<AgentViewSetParams> {
        match tier {
            AgentViewTier::Config => &self.config,
            AgentViewTier::Ui => &self.ui,
            AgentViewTier::Api => &self.api,
        }
    }

    fn slot_mut(&mut self, tier: AgentViewTier) -> &mut Option<AgentViewSetParams> {
        match tier {
            AgentViewTier::Config => &mut self.config,
            AgentViewTier::Ui => &mut self.ui,
            AgentViewTier::Api => &mut self.api,
        }
    }

    /// The winning view, highest tier first.
    pub(crate) fn active(&self) -> Option<&AgentViewSetParams> {
        self.api
            .as_ref()
            .or(self.ui.as_ref())
            .or(self.config.as_ref())
    }

    /// The tier that owns the winning view.
    pub(crate) fn active_tier(&self) -> Option<AgentViewTier> {
        [AgentViewTier::Api, AgentViewTier::Ui, AgentViewTier::Config]
            .into_iter()
            .find(|tier| self.slot(*tier).is_some())
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active().is_some()
    }

    pub(crate) fn get(&self, tier: AgentViewTier) -> Option<&AgentViewSetParams> {
        self.slot(tier).as_ref()
    }

    /// Replace one tier's view. Returns whether the winning view changed, so
    /// callers only reset panel scroll when the panel actually changed.
    pub(crate) fn set(&mut self, tier: AgentViewTier, view: Option<AgentViewSetParams>) -> bool {
        let before = self.active().cloned();
        *self.slot_mut(tier) = view;
        before.as_ref() != self.active()
    }

    /// The view the session file stores, so it comes back after a restart.
    ///
    /// Only the API tier is durable here. The config tier already survives a
    /// restart by being read out of `config.toml` at startup, and the UI tier
    /// is a gesture the user made in the moment — restoring that one would
    /// bring a filtered panel back with nothing on screen explaining why.
    pub(crate) fn durable(&self) -> Option<&AgentViewSetParams> {
        self.get(AgentViewTier::Api)
    }

    /// Clear the winning tier so the next tier down takes over. Returns the
    /// tier that was cleared.
    pub(crate) fn clear_active_tier(&mut self) -> Option<AgentViewTier> {
        let tier = self.active_tier()?;
        *self.slot_mut(tier) = None;
        Some(tier)
    }
}

pub(crate) fn validate_agent_view(spec: &mut AgentViewSetParams) -> Result<(), String> {
    spec.source = normalize_source(&spec.source)?;
    spec.label = spec
        .label
        .take()
        .map(|label| normalize_label(&label))
        .transpose()?;

    let mut nodes = 0;
    if let Some(filter) = &spec.filter {
        validate_filter(filter, 1, &mut nodes)?;
    }
    if spec.sort.len() > MAX_SORT_FIELDS {
        return Err(format!(
            "agent view sort may contain at most {MAX_SORT_FIELDS} fields"
        ));
    }
    for sort in &spec.sort {
        validate_sort_field(&sort.field)?;
    }
    Ok(())
}

pub(crate) fn validate_agent_view_source(source: &str) -> Result<String, String> {
    normalize_source(source)
}

fn normalize_source(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty()
        || source.chars().count() > MAX_SOURCE_CHARS
        || !source
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
    {
        return Err(format!(
            "agent view source must be non-empty, at most {MAX_SOURCE_CHARS} characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen"
        ));
    }
    Ok(source.to_string())
}

fn normalize_label(label: &str) -> Result<String, String> {
    let label = label
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "agent view label must be non-empty and at most {MAX_LABEL_CHARS} characters"
        ));
    }
    Ok(label)
}

fn validate_filter(
    filter: &AgentViewFilter,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_FILTER_DEPTH {
        return Err(format!(
            "agent view filter may be nested at most {MAX_FILTER_DEPTH} levels"
        ));
    }
    *nodes += 1;
    if *nodes > MAX_FILTER_NODES {
        return Err(format!(
            "agent view filter may contain at most {MAX_FILTER_NODES} nodes"
        ));
    }

    match filter {
        AgentViewFilter::All { filters } | AgentViewFilter::Any { filters } => {
            if filters.is_empty() {
                return Err("agent view all/any filters must not be empty".to_string());
            }
            for filter in filters {
                validate_filter(filter, depth + 1, nodes)?;
            }
        }
        AgentViewFilter::Not { filter } => validate_filter(filter, depth + 1, nodes)?,
        AgentViewFilter::Eq { field, value } => validate_field_value(field, value)?,
        AgentViewFilter::In { field, values } => {
            if values.is_empty() || values.len() > MAX_FILTER_VALUES {
                return Err(format!(
                    "agent view in filters require 1 to {MAX_FILTER_VALUES} values"
                ));
            }
            for value in values {
                validate_field_value(field, value)?;
            }
        }
        AgentViewFilter::Exists { field } => validate_field(field)?,
    }
    Ok(())
}

fn validate_field(field: &AgentViewField) -> Result<(), String> {
    if let AgentViewField::Token { token } = field {
        validate_token(token)?;
    }
    Ok(())
}

fn validate_field_value(field: &AgentViewField, value: &AgentViewValue) -> Result<(), String> {
    validate_field(field)?;
    match (field, value) {
        (
            AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
            AgentViewValue::Context {
                context: AgentViewContext::CurrentWorkspaceId,
            },
        )
        | (
            AgentViewField::Builtin(AgentViewBuiltinField::TabId),
            AgentViewValue::Context {
                context: AgentViewContext::CurrentTabId,
            },
        ) => Ok(()),
        (_, AgentViewValue::Context { .. }) => {
            Err("agent view context type does not match the selected field".to_string())
        }
        (AgentViewField::Builtin(AgentViewBuiltinField::Seen), AgentViewValue::Bool(_))
        | (
            AgentViewField::Builtin(AgentViewBuiltinField::StateChangeSeq),
            AgentViewValue::Number(_),
        ) => Ok(()),
        (
            AgentViewField::Builtin(
                AgentViewBuiltinField::Status
                | AgentViewBuiltinField::WorkspaceId
                | AgentViewBuiltinField::TabId
                | AgentViewBuiltinField::PaneId
                | AgentViewBuiltinField::Agent
                | AgentViewBuiltinField::Relation,
            )
            | AgentViewField::Token { .. },
            AgentViewValue::String(value),
        ) => {
            if matches!(
                field,
                AgentViewField::Builtin(AgentViewBuiltinField::Status)
            ) && !matches!(
                value.as_str(),
                "idle" | "working" | "blocked" | "done" | "unknown"
            ) {
                return Err(format!("unknown agent status `{value}`"));
            }
            if matches!(
                field,
                AgentViewField::Builtin(AgentViewBuiltinField::Relation)
            ) && !crate::app::agent_tree::RELATION_VALUES.contains(&value.as_str())
            {
                return Err(format!("unknown agent relation `{value}`"));
            }
            Ok(())
        }
        _ => Err("agent view value type does not match the selected field".to_string()),
    }
}

fn validate_sort_field(field: &AgentViewSortField) -> Result<(), String> {
    if let AgentViewSortField::Token { token } = field {
        validate_token(token)?;
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty()
        || token.len() > MAX_TOKEN_CHARS
        || !token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!("invalid agent view token `{token}`"));
    }
    Ok(())
}

/// Turn a serde failure inside a declarative view into something a human can
/// act on.
///
/// `field`, `value`, and `context` are `#[serde(untagged)]` positions, so serde
/// collapses a typo there into "data did not match any variant". This walks the
/// raw TOML and re-runs the *same* `Deserialize` impls on the suspect strings so
/// the precise "unknown variant `x`, expected one of ..." message survives. It
/// reads the grammar rather than restating it, so it cannot drift.
pub(crate) fn explain_view_value(value: &toml::Value, sort_position: bool) -> Option<String> {
    if let toml::Value::Table(table) = value {
        if let Some(toml::Value::String(name)) = table.get("field") {
            if let Some(message) = explain_enum(name, sort_position) {
                return Some(format!("`field = \"{name}\"` is not valid: {message}"));
            }
        }
        if let Some(toml::Value::String(name)) = table.get("context") {
            if let Err(err) = deserialize_name::<AgentViewContext>(name) {
                return Some(format!("`context = \"{name}\"` is not valid: {err}"));
            }
        }
    }
    match value {
        toml::Value::Table(table) => table
            .values()
            .find_map(|value| explain_view_value(value, sort_position)),
        toml::Value::Array(items) => items
            .iter()
            .find_map(|value| explain_view_value(value, sort_position)),
        _ => None,
    }
}

fn explain_enum(name: &str, sort_position: bool) -> Option<String> {
    if sort_position {
        deserialize_name::<AgentViewBuiltinSortField>(name).err()
    } else {
        deserialize_name::<AgentViewBuiltinField>(name).err()
    }
}

fn deserialize_name<'de, T: serde::Deserialize<'de>>(name: &'de str) -> Result<T, String> {
    T::deserialize(serde::de::value::StrDeserializer::<serde::de::value::Error>::new(name))
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentViewSort;

    fn view(source: &str) -> AgentViewSetParams {
        AgentViewSetParams {
            source: source.to_string(),
            label: None,
            filter: None,
            sort: Vec::new(),
        }
    }

    fn status_view(source: &str, status: &str) -> AgentViewSetParams {
        AgentViewSetParams {
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                value: AgentViewValue::String(status.to_string()),
            }),
            ..view(source)
        }
    }

    fn relation_view(source: &str, relation: &str) -> AgentViewSetParams {
        AgentViewSetParams {
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Relation),
                value: AgentViewValue::String(relation.to_string()),
            }),
            ..view(source)
        }
    }

    #[test]
    fn every_relation_value_passes_validation() {
        // Both doors run this validator, so a value it rejects is a value the
        // config key and the API cannot use — which is how `relation` shipped
        // unusable the first time.
        for relation in crate::app::agent_tree::RELATION_VALUES {
            let mut spec = relation_view("example.views", relation);
            validate_agent_view(&mut spec)
                .unwrap_or_else(|err| panic!("relation `{relation}` rejected: {err}"));
        }
    }

    #[test]
    fn every_category_tab_installs_a_view_that_validates() {
        // The click path builds these; if one failed validation the tab would
        // silently do nothing.
        for category in crate::ui::AGENT_CATEGORIES {
            let mut spec = category.view();
            validate_agent_view(&mut spec).unwrap_or_else(|err| {
                panic!("category {:?} produced an invalid view: {err}", category)
            });
        }
    }

    #[test]
    fn an_unknown_relation_is_rejected() {
        let mut spec = relation_view("example.views", "captain");
        let err = validate_agent_view(&mut spec).unwrap_err();
        assert!(err.contains("unknown agent relation"), "{err}");
    }

    #[test]
    fn a_relation_filter_still_rejects_a_non_string_value() {
        let mut spec = AgentViewSetParams {
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Relation),
                value: AgentViewValue::Bool(true),
            }),
            ..view("example.views")
        };
        assert!(validate_agent_view(&mut spec).is_err());
    }

    #[test]
    fn validation_trims_source_and_label_and_rejects_empty_labels() {
        let mut spec = AgentViewSetParams {
            label: Some("  working  ".to_string()),
            ..view("  example.views  ")
        };
        validate_agent_view(&mut spec).unwrap();
        assert_eq!(spec.source, "example.views");
        assert_eq!(spec.label.as_deref(), Some("working"));

        let mut blank = AgentViewSetParams {
            label: Some("   ".to_string()),
            ..view("example.views")
        };
        assert!(validate_agent_view(&mut blank)
            .unwrap_err()
            .contains("label"));
    }

    #[test]
    fn validation_rejects_unknown_status_and_oversized_sorts() {
        let mut spec = status_view("example.views", "workin");
        assert!(validate_agent_view(&mut spec)
            .unwrap_err()
            .contains("unknown agent status `workin`"));

        let mut spec = AgentViewSetParams {
            sort: std::iter::repeat_n(
                AgentViewSort {
                    field: AgentViewSortField::Builtin(AgentViewBuiltinSortField::Attention),
                    order: crate::api::schema::AgentViewSortOrder::Desc,
                },
                MAX_SORT_FIELDS + 1,
            )
            .collect(),
            ..view("example.views")
        };
        assert!(validate_agent_view(&mut spec)
            .unwrap_err()
            .contains("at most 8 fields"));
    }

    #[test]
    fn config_and_ui_sources_are_reserved_for_herdr_itself() {
        assert_eq!(
            reserved_source_owner(CONFIG_VIEW_SOURCE),
            Some(AgentViewTier::Config)
        );
        assert_eq!(
            reserved_source_owner(UI_VIEW_SOURCE),
            Some(AgentViewTier::Ui)
        );
        assert_eq!(reserved_source_owner("plugin:agents"), None);
        assert_eq!(reserved_source_owner("example.views"), None);
    }

    #[test]
    fn api_outranks_ui_which_outranks_config() {
        let mut slots = AgentViewSlots::default();
        assert!(slots.active().is_none());
        assert!(slots.active_tier().is_none());

        assert!(slots.set(
            AgentViewTier::Config,
            Some(status_view(CONFIG_VIEW_SOURCE, "working"))
        ));
        assert_eq!(slots.active_tier(), Some(AgentViewTier::Config));

        assert!(slots.set(AgentViewTier::Ui, Some(status_view(UI_VIEW_SOURCE, "idle"))));
        assert_eq!(slots.active_tier(), Some(AgentViewTier::Ui));

        assert!(slots.set(
            AgentViewTier::Api,
            Some(status_view("plugin:agents", "blocked"))
        ));
        assert_eq!(slots.active_tier(), Some(AgentViewTier::Api));
        assert_eq!(
            slots.active().map(|view| view.source.as_str()),
            Some("plugin:agents")
        );
    }

    #[test]
    fn clearing_a_tier_falls_back_to_the_next_one_down() {
        let mut slots = AgentViewSlots::default();
        slots.set(
            AgentViewTier::Config,
            Some(status_view(CONFIG_VIEW_SOURCE, "working")),
        );
        slots.set(AgentViewTier::Ui, Some(status_view(UI_VIEW_SOURCE, "idle")));
        slots.set(
            AgentViewTier::Api,
            Some(status_view("plugin:agents", "blocked")),
        );

        assert_eq!(slots.clear_active_tier(), Some(AgentViewTier::Api));
        assert_eq!(slots.active_tier(), Some(AgentViewTier::Ui));
        assert_eq!(slots.clear_active_tier(), Some(AgentViewTier::Ui));
        assert_eq!(slots.active_tier(), Some(AgentViewTier::Config));
        assert_eq!(slots.clear_active_tier(), Some(AgentViewTier::Config));
        assert!(!slots.is_active());
        assert_eq!(slots.clear_active_tier(), None);
    }

    #[test]
    fn replacing_a_shadowed_tier_leaves_the_active_view_alone() {
        let mut slots = AgentViewSlots::default();
        slots.set(
            AgentViewTier::Api,
            Some(status_view("plugin:agents", "blocked")),
        );

        assert!(!slots.set(
            AgentViewTier::Config,
            Some(status_view(CONFIG_VIEW_SOURCE, "working"))
        ));
        assert_eq!(
            slots.active().map(|view| view.source.as_str()),
            Some("plugin:agents")
        );
        assert!(slots.get(AgentViewTier::Config).is_some());
    }

    #[test]
    fn explain_view_value_recovers_untagged_field_and_context_errors() {
        let filter: toml::Value =
            toml::from_str("op = 'eq'\nfield = 'statuss'\nvalue = 'working'\n").unwrap();
        let message = explain_view_value(&filter, false).expect("field diagnostic");
        assert!(message.contains("`field = \"statuss\"`"), "{message}");
        assert!(message.contains("status"), "{message}");

        let sort: toml::Value = toml::from_str("field = 'attention'\n").unwrap();
        assert!(explain_view_value(&sort, true).is_none());
        assert!(explain_view_value(&sort, false).is_some());

        let context: toml::Value = toml::from_str(
            "op = 'eq'\nfield = 'workspace_id'\n[value]\ncontext = 'current_space_id'\n",
        )
        .unwrap();
        let message = explain_view_value(&context, false).expect("context diagnostic");
        assert!(
            message.contains("`context = \"current_space_id\"`"),
            "{message}"
        );

        let nested: toml::Value = toml::from_str(
            "op = 'all'\n[[filters]]\nop = 'eq'\nfield = 'agent'\nvalue = 'claude'\n[[filters]]\nop = 'exists'\nfield = 'nope'\n",
        )
        .unwrap();
        assert!(explain_view_value(&nested, false)
            .expect("nested diagnostic")
            .contains("`field = \"nope\"`"));
    }
}
