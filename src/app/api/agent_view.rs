use crate::api::schema::{AgentViewClearParams, AgentViewSetParams, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_agent_view_set(
        &mut self,
        id: String,
        mut params: AgentViewSetParams,
    ) -> String {
        if let Err(message) = crate::agent_view::validate_agent_view(&mut params) {
            return encode_error(id, "invalid_agent_view", message);
        }
        if let Some(tier) = crate::agent_view::reserved_source_owner(&params.source) {
            return encode_error(
                id,
                "invalid_agent_view",
                format!(
                    "agent view source `{}` is reserved for the {} tier and cannot be set over the API",
                    params.source,
                    tier.label()
                ),
            );
        }
        if let Some(plugin_id) = params.source.strip_prefix("plugin:") {
            let Some(plugin_id) = super::plugins::normalize_plugin_id(plugin_id) else {
                return encode_error(
                    id,
                    "invalid_agent_view",
                    "plugin-owned agent view source has an invalid plugin id",
                );
            };
            let Some(plugin) = self.state.installed_plugins.get(&plugin_id) else {
                return encode_error(id, "plugin_not_found", "plugin not found");
            };
            if !plugin.enabled {
                return encode_error(id, "plugin_disabled", "plugin is disabled");
            }
        }
        let source = params.source.clone();
        let label = params.label.clone();
        self.replace_agent_view(crate::agent_view::AgentViewTier::Api, Some(params));
        encode_success(
            id,
            ResponseResult::AgentView {
                active: true,
                source: Some(source),
                label,
            },
        )
    }

    pub(super) fn handle_agent_view_clear(
        &mut self,
        id: String,
        params: AgentViewClearParams,
    ) -> String {
        let source = match params.source {
            Some(source) => match crate::agent_view::validate_agent_view_source(&source) {
                Ok(source) => Some(source),
                Err(message) => return encode_error(id, "invalid_agent_view", message),
            },
            None => None,
        };
        // `agent.view.clear` only owns the API tier. A config- or UI-declared
        // view is not the caller's to drop, and the response reports whichever
        // view is left in charge so a plugin can see it fell back rather than
        // off.
        if source.as_deref().is_none_or(|source| {
            self.state
                .agent_views
                .get(crate::agent_view::AgentViewTier::Api)
                .is_some_and(|active| active.source == source)
        }) {
            self.replace_agent_view(crate::agent_view::AgentViewTier::Api, None);
        }
        let active = self.state.agent_views.active();
        encode_success(
            id,
            ResponseResult::AgentView {
                active: active.is_some(),
                source: active.map(|view| view.source.clone()),
                label: active.and_then(|view| view.label.clone()),
            },
        )
    }

    pub(crate) fn clear_agent_view_for_source(&mut self, source: &str) -> bool {
        if self
            .state
            .agent_views
            .get(crate::agent_view::AgentViewTier::Api)
            .is_some_and(|active| active.source == source)
        {
            self.replace_agent_view(crate::agent_view::AgentViewTier::Api, None);
            true
        } else {
            false
        }
    }

    /// Replace one tier's view, resetting panel scroll only when the projected
    /// view actually changed.
    pub(crate) fn replace_agent_view(
        &mut self,
        tier: crate::agent_view::AgentViewTier,
        view: Option<AgentViewSetParams>,
    ) {
        // Only the API tier is written to the session file, so only a change
        // there is worth a save. Compare before setting: a plugin that
        // re-publishes the same view every few seconds must not rewrite
        // `session.json` every few seconds.
        let durable_changed = tier == crate::agent_view::AgentViewTier::Api
            && self.state.agent_views.durable() != view.as_ref();
        if self.state.agent_views.set(tier, view) {
            self.state.mobile_switcher_scroll = 0;
        }
        if durable_changed {
            self.state.mark_session_dirty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_view::{AgentViewTier, CONFIG_VIEW_SOURCE, UI_VIEW_SOURCE};
    use crate::api::schema::{
        AgentViewBuiltinField, AgentViewField, AgentViewFilter, AgentViewValue,
    };

    fn test_app() -> App {
        app_with_config(&crate::config::Config::default())
    }

    fn app_with_config(config: &crate::config::Config) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(config, true, None, api_rx, crate::api::EventHub::default())
    }

    fn status_view(source: &str, status: &str) -> AgentViewSetParams {
        AgentViewSetParams {
            source: source.to_string(),
            label: Some(status.to_string()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                value: AgentViewValue::String(status.to_string()),
            }),
            sort: Vec::new(),
        }
    }

    fn working_view(source: &str) -> AgentViewSetParams {
        status_view(source, "working")
    }

    fn active_source(app: &App) -> Option<&str> {
        app.state
            .agent_views
            .active()
            .map(|view| view.source.as_str())
    }

    #[test]
    fn setting_and_clearing_the_api_agent_view_marks_the_session_dirty() {
        let mut app = test_app();
        app.state.session_dirty = false;

        app.handle_agent_view_set("set".to_string(), working_view("workers-only"));
        assert!(app.state.session_dirty);

        // Re-publishing the identical view is not a change worth a disk write.
        app.state.session_dirty = false;
        app.handle_agent_view_set("set".to_string(), working_view("workers-only"));
        assert!(!app.state.session_dirty);

        app.handle_agent_view_clear("clear".to_string(), AgentViewClearParams::default());
        assert!(app.state.session_dirty);
    }

    #[test]
    fn set_and_source_guarded_clear_replace_transient_view() {
        let mut app = test_app();

        let set = app.handle_agent_view_set("set".to_string(), working_view("example.views"));
        let set: crate::api::schema::SuccessResponse = serde_json::from_str(&set).unwrap();
        assert_eq!(
            set.result,
            ResponseResult::AgentView {
                active: true,
                source: Some("example.views".to_string()),
                label: Some("working".to_string()),
            }
        );
        assert_eq!(active_source(&app), Some("example.views"));

        app.handle_agent_view_clear(
            "wrong-source".to_string(),
            AgentViewClearParams {
                source: Some("other.views".to_string()),
            },
        );
        assert!(app.state.agent_views.is_active());

        app.handle_agent_view_clear(
            "right-source".to_string(),
            AgentViewClearParams {
                source: Some("example.views".to_string()),
            },
        );
        assert!(!app.state.agent_views.is_active());
    }

    #[test]
    fn invalid_view_does_not_replace_active_view() {
        let mut app = test_app();
        app.handle_agent_view_set("set".to_string(), working_view("example.views"));

        let mut invalid = working_view("example.other");
        invalid.filter = Some(AgentViewFilter::Any {
            filters: Vec::new(),
        });
        let response = app.handle_agent_view_set("invalid".to_string(), invalid);
        let response: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(response.error.code, "invalid_agent_view");
        assert_eq!(active_source(&app), Some("example.views"));
    }

    #[test]
    fn declared_config_view_is_applied_at_startup() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.agents.view]
label = "blocked"
filter = { op = "eq", field = "status", value = "blocked" }
"#,
        )
        .unwrap();
        let app = app_with_config(&config);

        assert_eq!(active_source(&app), Some(CONFIG_VIEW_SOURCE));
        assert_eq!(
            app.state.agent_views.active_tier(),
            Some(AgentViewTier::Config)
        );
    }

    #[test]
    fn api_view_outranks_config_view_and_clearing_it_falls_back() {
        let config: crate::config::Config = toml::from_str(
            "[ui.sidebar.agents.view]\nfilter = { op = \"eq\", field = \"status\", value = \"blocked\" }\n",
        )
        .unwrap();
        let mut app = app_with_config(&config);
        assert_eq!(active_source(&app), Some(CONFIG_VIEW_SOURCE));

        app.handle_agent_view_set("set".to_string(), working_view("plugin-ish.views"));
        assert_eq!(active_source(&app), Some("plugin-ish.views"));

        // Clearing the API view reveals the config view again instead of
        // dropping the user into an unfiltered panel they never asked for.
        let cleared =
            app.handle_agent_view_clear("clear".to_string(), AgentViewClearParams::default());
        let cleared: crate::api::schema::SuccessResponse = serde_json::from_str(&cleared).unwrap();
        assert_eq!(
            cleared.result,
            ResponseResult::AgentView {
                active: true,
                source: Some(CONFIG_VIEW_SOURCE.to_string()),
                label: None,
            }
        );
        assert_eq!(active_source(&app), Some(CONFIG_VIEW_SOURCE));
    }

    #[test]
    fn api_cannot_clear_or_claim_a_reserved_tier_source() {
        let config: crate::config::Config = toml::from_str(
            "[ui.sidebar.agents.view]\nfilter = { op = \"eq\", field = \"status\", value = \"blocked\" }\n",
        )
        .unwrap();
        let mut app = app_with_config(&config);

        for source in [CONFIG_VIEW_SOURCE, UI_VIEW_SOURCE] {
            let response = app.handle_agent_view_set("set".to_string(), working_view(source));
            let response: crate::api::schema::ErrorResponse =
                serde_json::from_str(&response).unwrap();
            assert_eq!(response.error.code, "invalid_agent_view");
            assert!(
                response.error.message.contains("reserved"),
                "{}",
                response.error.message
            );
        }

        app.handle_agent_view_clear(
            "clear-config".to_string(),
            AgentViewClearParams {
                source: Some(CONFIG_VIEW_SOURCE.to_string()),
            },
        );
        assert_eq!(active_source(&app), Some(CONFIG_VIEW_SOURCE));
    }

    #[test]
    fn ui_view_outranks_config_but_loses_to_the_api() {
        let config: crate::config::Config = toml::from_str(
            "[ui.sidebar.agents.view]\nfilter = { op = \"eq\", field = \"status\", value = \"blocked\" }\n",
        )
        .unwrap();
        let mut app = app_with_config(&config);

        app.replace_agent_view(AgentViewTier::Ui, Some(status_view(UI_VIEW_SOURCE, "idle")));
        assert_eq!(active_source(&app), Some(UI_VIEW_SOURCE));

        app.handle_agent_view_set("set".to_string(), working_view("plugin-ish.views"));
        assert_eq!(active_source(&app), Some("plugin-ish.views"));

        app.handle_agent_view_clear("clear".to_string(), AgentViewClearParams::default());
        assert_eq!(active_source(&app), Some(UI_VIEW_SOURCE));
    }

    #[test]
    fn invalid_config_view_leaves_the_panel_unfiltered_with_a_diagnostic() {
        let config: crate::config::Config = toml::from_str(
            "[ui.sidebar.agents.view]\nfilter = { op = \"eq\", field = \"status\", value = \"workin\" }\n",
        )
        .unwrap();
        let app = app_with_config(&config);

        assert!(!app.state.agent_views.is_active());
        let diagnostics = config.collect_diagnostics();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("unknown agent status `workin`")),
            "{diagnostics:?}"
        );
    }
}
