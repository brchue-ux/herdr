use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{AppState, SettingsSection, THEME_NAMES},
        App, Mode,
    },
    config::{StatusIndicatorStyle, ToastDelivery},
};

#[derive(Debug, Clone, PartialEq, Eq)]
// The shared `Save` verb is semantic: these actions persist settings.
#[allow(clippy::enum_variant_names)]
pub(super) enum SettingsAction {
    SaveTheme(String),
    SaveCardPulse(bool),
    SaveCardWash(bool),
    SaveCardStageHue(bool),
    SaveStatusIndicators(StatusIndicatorStyle),
    SaveSound(bool),
    SaveToastDelivery(ToastDelivery),
    SaveAgentBorderLabels(bool),
    SaveSignalTrayEnabled(bool),
    SaveSignalTrayActions(bool),
    SaveSignalTrayAnimate(bool),
    InstallRecommendedIntegrations,
}

impl App {
    pub(crate) fn handle_settings_key(&mut self, key: KeyEvent) {
        let previous_section = self.state.settings.section;
        if let Some(action) = update_settings_state(&mut self.state, key) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveCardPulse(enabled) => self.save_card_pulse(enabled),
                SettingsAction::SaveCardWash(enabled) => self.save_card_wash(enabled),
                SettingsAction::SaveCardStageHue(enabled) => self.save_card_stage_hue(enabled),
                SettingsAction::SaveStatusIndicators(style) => self.save_status_indicators(style),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToastDelivery(delivery) => self.save_toast_delivery(delivery),
                SettingsAction::SaveAgentBorderLabels(enabled) => {
                    self.save_agent_border_labels(enabled)
                }
                SettingsAction::SaveSignalTrayEnabled(enabled) => {
                    self.save_signal_tray_enabled(enabled)
                }
                SettingsAction::SaveSignalTrayActions(enabled) => {
                    self.save_signal_tray_actions(enabled)
                }
                SettingsAction::SaveSignalTrayAnimate(enabled) => {
                    self.save_signal_tray_animate(enabled)
                }
                SettingsAction::InstallRecommendedIntegrations => {
                    self.install_recommended_integrations()
                }
            }
        }
        if previous_section != SettingsSection::Integrations
            && self.state.settings.section == SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
    }
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn current_theme_index(theme_name: &str) -> usize {
    let normalized = normalize_theme_name(theme_name);
    THEME_NAMES
        .iter()
        .position(|name| normalize_theme_name(name) == normalized)
        .unwrap_or(0)
}

/// Keep in step with the choice list in `ui::settings`.
const STATUS_INDICATOR_COUNT: usize = 3;

fn status_indicator_index(style: StatusIndicatorStyle) -> usize {
    match style {
        StatusIndicatorStyle::Ascii => 0,
        StatusIndicatorStyle::Dots => 1,
        StatusIndicatorStyle::Symbols => 2,
    }
}

fn status_indicator_for_index(idx: usize) -> StatusIndicatorStyle {
    match idx {
        0 => StatusIndicatorStyle::Ascii,
        1 => StatusIndicatorStyle::Dots,
        _ => StatusIndicatorStyle::Symbols,
    }
}

fn toast_delivery_index(delivery: ToastDelivery) -> usize {
    match delivery {
        ToastDelivery::Off => 0,
        ToastDelivery::Herdr => 1,
        ToastDelivery::Terminal => 2,
        ToastDelivery::System => 3,
    }
}

fn toast_delivery_for_index(idx: usize) -> ToastDelivery {
    match idx {
        0 => ToastDelivery::Off,
        1 => ToastDelivery::Herdr,
        2 => ToastDelivery::Terminal,
        _ => ToastDelivery::System,
    }
}

fn preview_selected_theme(state: &mut AppState) {
    use crate::app::state::Palette;

    let name = THEME_NAMES[state.settings.list.selected];
    if let Some(mut palette) = Palette::from_name(name) {
        if let Some(custom) = &state.theme_runtime.custom {
            palette = palette.with_overrides(custom);
        }
        if let Some(accent) = &state.theme_runtime.legacy_accent {
            palette.accent = crate::config::parse_color(accent);
        }
        state.palette = palette;
        state.theme_name = name.to_string();
    }
}

fn cancel_settings(state: &mut AppState) {
    if let Some(palette) = state.settings.original_palette.take() {
        state.palette = palette;
    }
    if let Some(theme_name) = state.settings.original_theme.take() {
        state.theme_name = theme_name;
    }
    super::modal::leave_modal(state);
}

/// How many independently toggled rows the animation section lists: card
/// breathing, card wash, card stage colour, and signal badge motion.
const ANIMATION_ROW_COUNT: usize = 4;
/// How many independently toggled rows the fleet-signal tray section lists:
/// whether the tray is shown, and whether it may act in place.
const SIGNALS_ROW_COUNT: usize = 2;

/// The row highlighted when a section is switched to, before any input.
///
/// Centralised because switching sections has to seed a value-matching index
/// for every section that reads one, and a caller re-deriving that mapping
/// per call site is exactly how the pane-labels tab and the theme tab drift
/// apart from each other.
fn initial_list_index(state: &AppState, section: SettingsSection) -> usize {
    match section {
        SettingsSection::Theme => current_theme_index(&state.theme_name),
        SettingsSection::Animation => 0,
        SettingsSection::Indicators => status_indicator_index(state.status_indicators),
        SettingsSection::Sound => usize::from(!state.sound_enabled()),
        SettingsSection::Toast => toast_delivery_index(state.toast_delivery()),
        SettingsSection::PaneLabels => usize::from(!state.agent_border_labels_enabled()),
        SettingsSection::Signals => 0,
        SettingsSection::Legend => 0,
        SettingsSection::Integrations => 0,
    }
}

/// What toggling the highlighted row in the animation section saves.
fn toggle_animation_row(state: &AppState, row: usize) -> SettingsAction {
    match row {
        0 => SettingsAction::SaveCardPulse(!state.sidebar_cards.pulse),
        1 => SettingsAction::SaveCardWash(!state.sidebar_cards.wash),
        2 => SettingsAction::SaveCardStageHue(!state.sidebar_cards.stage_hue),
        _ => SettingsAction::SaveSignalTrayAnimate(!state.sidebar_signal_tray.animate),
    }
}

/// What toggling the highlighted row in the fleet-signal tray section saves.
fn toggle_signals_row(state: &AppState, row: usize) -> SettingsAction {
    match row {
        0 => SettingsAction::SaveSignalTrayEnabled(!state.sidebar_signal_tray.enabled),
        _ => SettingsAction::SaveSignalTrayActions(!state.sidebar_signal_tray.actions),
    }
}

fn integrations_need_install(state: &AppState) -> bool {
    state
        .integration_recommendations
        .iter()
        .any(crate::integration::IntegrationRecommendation::needs_install)
}

fn apply_settings(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => {
            let theme_name = state.theme_name.clone();
            state.settings.original_palette = None;
            state.settings.original_theme = None;
            super::modal::leave_modal(state);
            Some(SettingsAction::SaveTheme(theme_name))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        SettingsSection::Integrations => None,
        _ => {
            super::modal::leave_modal(state);
            None
        }
    }
}

pub(super) fn update_settings_state(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let previous = state.settings.list.selected;
                state.settings.list.move_prev();
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let previous = state.settings.list.selected;
                state.settings.list.move_next(THEME_NAMES.len());
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Animation;
                state.settings.list.selected =
                    initial_list_index(state, SettingsSection::Animation);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected = 0;
            }
            _ => match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
                Some(super::modal::ModalAction::Apply) => return apply_settings(state),
                Some(super::modal::ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
        SettingsSection::Animation => match key.code {
            KeyCode::Up | KeyCode::Char('k') => state.settings.list.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.move_next(ANIMATION_ROW_COUNT);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                return Some(toggle_animation_row(state, state.settings.list.selected));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = initial_list_index(state, SettingsSection::Theme);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Indicators;
                state.settings.list.selected =
                    initial_list_index(state, SettingsSection::Indicators);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Indicators => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let current = state.settings.list.selected.min(STATUS_INDICATOR_COUNT - 1);
                state.settings.list.selected =
                    (current + STATUS_INDICATOR_COUNT - 1) % STATUS_INDICATOR_COUNT;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let current = state.settings.list.selected.min(STATUS_INDICATOR_COUNT - 1);
                state.settings.list.selected = (current + 1) % STATUS_INDICATOR_COUNT;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let style = status_indicator_for_index(state.settings.list.selected);
                return Some(SettingsAction::SaveStatusIndicators(style));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Animation;
                state.settings.list.selected =
                    initial_list_index(state, SettingsSection::Animation);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Sound => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                return Some(SettingsAction::SaveSound(enabled));
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = toast_delivery_index(state.toast_delivery());
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Indicators;
                state.settings.list.selected = status_indicator_index(state.status_indicators);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Toast => match key.code {
            KeyCode::Up | KeyCode::Char('k') => state.settings.list.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => state.settings.list.move_next(4),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let delivery = toast_delivery_for_index(state.settings.list.selected);
                return Some(SettingsAction::SaveToastDelivery(delivery));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::PaneLabels;
                state.settings.list.selected = usize::from(!state.agent_border_labels_enabled());
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::PaneLabels => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                return Some(SettingsAction::SaveAgentBorderLabels(enabled));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = toast_delivery_index(state.toast_delivery());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Signals;
                state.settings.list.selected = initial_list_index(state, SettingsSection::Signals);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Signals => match key.code {
            KeyCode::Up | KeyCode::Char('k') => state.settings.list.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.move_next(SIGNALS_ROW_COUNT);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                return Some(toggle_signals_row(state, state.settings.list.selected));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::PaneLabels;
                state.settings.list.selected =
                    initial_list_index(state, SettingsSection::PaneLabels);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Legend;
                state.settings.list.selected = initial_list_index(state, SettingsSection::Legend);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Legend => match key.code {
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Signals;
                state.settings.list.selected = initial_list_index(state, SettingsSection::Signals);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected =
                    initial_list_index(state, SettingsSection::Integrations);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Integrations => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') if integrations_need_install(state) => {
                return Some(SettingsAction::InstallRecommendedIntegrations);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Legend;
                state.settings.list.selected = initial_list_index(state, SettingsSection::Legend);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            _ => match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
                Some(super::modal::ModalAction::Apply) => return apply_settings(state),
                Some(super::modal::ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
    }

    None
}

pub(crate) fn open_settings(state: &mut AppState) {
    open_settings_at(state, SettingsSection::Theme);
}

pub(crate) fn open_settings_at(state: &mut AppState, section: SettingsSection) {
    state.integration_install_messages.clear();
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    state.settings.section = section;
    state.settings.list.selected = initial_list_index(state, section);
    state.mode = Mode::Settings;
}

impl AppState {
    fn settings_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(
            self.screen_rect(),
            crate::ui::SETTINGS_POPUP_WIDTH,
            crate::ui::settings_popup_height(self),
        )
        .unwrap_or_default()
    }

    fn settings_inner_rect(&self) -> Rect {
        let popup = self.settings_popup_rect();
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    }

    fn settings_tab_at(&self, col: u16, row: u16) -> Option<SettingsSection> {
        let inner = self.settings_inner_rect();
        let tab_y = inner.y + 1;
        if row != tab_y {
            return None;
        }
        let mut x = inner.x;
        for section in SettingsSection::ALL {
            let badge_width = if self.settings_section_has_badge(*section) {
                2
            } else {
                0
            };
            let width = section.label().len() as u16 + 2 + badge_width;
            if col >= x && col < x + width {
                return Some(*section);
            }
            x += width + 1;
        }
        None
    }

    pub(crate) fn settings_content_rect(&self) -> Rect {
        let inner = self.settings_inner_rect();
        crate::ui::modal_stack_areas(inner, 3, 2, 0, 1).content
    }

    fn settings_list_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height || col < area.x || col >= area.x + area.width
        {
            return None;
        }

        match self.settings.section {
            SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.list.selected >= max_visible {
                    self.settings.list.selected - max_visible + 1
                } else {
                    0
                };
                let idx = scroll + (row - area.y) as usize;
                (idx < THEME_NAMES.len()).then_some(idx)
            }
            SettingsSection::Indicators => {
                let list_y = area.y + 3;
                // Bounded by the alphabet itself, not by a literal: this fork
                // ships a third style beside upstream's two, and a hardcoded
                // row count is what makes the last one keyboard-only.
                let rows = u16::try_from(STATUS_INDICATOR_COUNT).unwrap_or(u16::MAX);
                if row >= list_y && row < list_y.saturating_add(rows) {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Sound => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Toast => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 8 {
                    Some(((row - list_y) / 2) as usize)
                } else {
                    None
                }
            }
            SettingsSection::PaneLabels => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Animation => {
                let list_y = area.y + 3;
                let rows = ANIMATION_ROW_COUNT as u16;
                if row >= list_y && row < list_y.saturating_add(rows) {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Signals => {
                let list_y = area.y + 3;
                let rows = SIGNALS_ROW_COUNT as u16;
                if row >= list_y && row < list_y.saturating_add(rows) {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            // Read-only reference content: nothing to click.
            SettingsSection::Legend | SettingsSection::Integrations => None,
        }
    }

    pub(super) fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    let idx = initial_list_index(self, section);
                    self.settings.section = section;
                    self.settings.list.select(idx);
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.column, mouse.row) {
                    self.settings.list.select(idx);
                    return match self.settings.section {
                        SettingsSection::Theme => {
                            preview_selected_theme(self);
                            None
                        }
                        SettingsSection::Indicators => Some(SettingsAction::SaveStatusIndicators(
                            status_indicator_for_index(idx),
                        )),
                        SettingsSection::Sound => {
                            let enabled = idx == 0;
                            Some(SettingsAction::SaveSound(enabled))
                        }
                        SettingsSection::Toast => {
                            let delivery = toast_delivery_for_index(idx);
                            Some(SettingsAction::SaveToastDelivery(delivery))
                        }
                        SettingsSection::PaneLabels => {
                            let enabled = idx == 0;
                            Some(SettingsAction::SaveAgentBorderLabels(enabled))
                        }
                        SettingsSection::Animation => Some(toggle_animation_row(self, idx)),
                        SettingsSection::Signals => Some(toggle_signals_row(self, idx)),
                        SettingsSection::Legend | SettingsSection::Integrations => None,
                    };
                }

                let inner = self.settings_inner_rect();
                let show_primary = crate::ui::settings_show_primary_action(self);
                let (apply, close) =
                    crate::ui::settings_button_rects(inner, self.settings.section, show_primary);
                let mut buttons = vec![(close, super::modal::ModalAction::Close)];
                if let Some(apply) = apply {
                    buttons.insert(0, (apply, super::modal::ModalAction::Apply));
                }
                match super::modal::modal_action_from_buttons(mouse.column, mouse.row, &buttons) {
                    Some(super::modal::ModalAction::Apply) => apply_settings(self),
                    Some(super::modal::ModalAction::Close) => {
                        cancel_settings(self);
                        None
                    }
                    _ => {
                        cancel_settings(self);
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};

    use super::super::{app_for_mouse_test, mouse, state_with_workspaces};
    use super::*;

    #[test]
    fn settings_cancel_restores_previewed_theme_from_other_sections() {
        let mut state = state_with_workspaces(&["test"]);
        let original_palette = state.palette.clone();
        let original_theme = state.theme_name.clone();

        open_settings(&mut state);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_ne!(state.theme_name, original_theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.section,
            crate::app::state::SettingsSection::Animation
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert_eq!(state.theme_name, original_theme);
        assert_eq!(state.palette.accent, original_palette.accent);
        assert_eq!(state.palette.panel_bg, original_palette.panel_bg);
    }

    #[test]
    fn settings_indicator_choice_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Indicators);
        // 0 is this fork's ascii default; 1 and 2 are upstream's two styles.
        for (selected, expected) in [
            (0, StatusIndicatorStyle::Ascii),
            (1, StatusIndicatorStyle::Dots),
            (2, StatusIndicatorStyle::Symbols),
        ] {
            state.settings.list.selected = selected;

            let action = update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            );

            assert_eq!(action, Some(SettingsAction::SaveStatusIndicators(expected)));
            // The action is returned for the caller to apply; nothing is
            // committed to the state here.
            assert_eq!(state.status_indicators, StatusIndicatorStyle::Ascii);
            assert_eq!(state.mode, Mode::Settings);
        }
    }

    #[test]
    fn settings_sound_toggle_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.section = crate::app::state::SettingsSection::Sound;
        state.settings.list.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(!state.sound.enabled);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_tab_cycle_wraps_after_integrations() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::PaneLabels);

        for expected in [
            SettingsSection::Signals,
            SettingsSection::Legend,
            SettingsSection::Integrations,
            SettingsSection::Theme,
        ] {
            update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            );
            assert_eq!(state.settings.section, expected);
        }

        for expected in [
            SettingsSection::Integrations,
            SettingsSection::Legend,
            SettingsSection::Signals,
            SettingsSection::PaneLabels,
        ] {
            update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
            );
            assert_eq!(state.settings.section, expected);
        }
    }

    /// Every section reachable by tabbing forward all the way around is
    /// exactly the fixed set the tab strip draws — one full lap, no section
    /// skipped and none visited twice.
    #[test]
    fn a_full_tab_lap_visits_every_section_exactly_once() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);

        let mut visited = vec![state.settings.section];
        for _ in 0..SettingsSection::ALL.len() - 1 {
            update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            );
            visited.push(state.settings.section);
        }
        // One more Tab returns to the start, closing the loop.
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Theme);

        visited.sort_by_key(|section| {
            SettingsSection::ALL
                .iter()
                .position(|candidate| candidate == section)
                .unwrap()
        });
        visited.dedup();
        assert_eq!(visited, SettingsSection::ALL);
    }

    /// The animation section's rows toggle independently: switching one off
    /// says nothing about the others.
    #[test]
    fn animation_rows_toggle_independently() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Animation);
        assert!(state.sidebar_cards.pulse);
        assert!(state.sidebar_signal_tray.animate);

        state.settings.list.selected = 0;
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(action, Some(SettingsAction::SaveCardPulse(false)));

        state.settings.list.selected = 3;
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(action, Some(SettingsAction::SaveSignalTrayAnimate(false)));
        // Neither toggle commits anything itself — the caller applies the
        // returned action, exactly like every other section.
        assert!(state.sidebar_cards.pulse);
        assert!(state.sidebar_signal_tray.animate);
    }

    /// The fleet-signal tray section's two rows are independent switches too:
    /// whether the tray is shown, and whether it may act in place.
    #[test]
    fn signals_rows_toggle_independently() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Signals);

        state.settings.list.selected = 0;
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(
            action,
            Some(SettingsAction::SaveSignalTrayEnabled(
                !state.sidebar_signal_tray.enabled
            ))
        );

        state.settings.list.selected = 1;
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(
            action,
            Some(SettingsAction::SaveSignalTrayActions(
                !state.sidebar_signal_tray.actions
            ))
        );
    }

    /// The legend is read-only reference content: it has nothing to apply, so
    /// only the close button is shown.
    #[test]
    fn legend_shows_no_apply_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Legend);
        assert!(!crate::ui::settings_show_primary_action(&state));
    }

    /// The legend names all eight signals, exactly like the tray's own
    /// popover — a single source read from two surfaces.
    #[test]
    fn legend_names_all_eight_signals() {
        use crate::app::fleet_signals::FleetSignal;

        let lines = crate::ui::signal_tray_popup::legend_lines();
        assert_eq!(lines.len(), FleetSignal::COUNT);
        for signal in FleetSignal::ALL {
            assert!(
                lines.iter().any(|line| line.starts_with(signal.name())),
                "the legend does not name {signal:?}"
            );
        }
    }

    #[test]
    fn integrations_enter_does_nothing_when_nothing_needs_install() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Integrations);

        let enter_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(enter_action, None);

        let space_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );
        assert_eq!(space_action, None);
    }

    #[test]
    fn settings_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.list.select(0);

        let area = app.state.settings_content_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 2, area.y + 2));

        assert_eq!(app.state.settings.list.selected, 0);
    }

    #[test]
    fn integration_update_badge_only_tracks_outdated_recommendations() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            false,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Current,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        assert!(state.integration_updates_available());
    }

    #[test]
    fn settings_tab_hit_area_includes_integration_update_badge() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        open_settings(&mut state);

        let inner = state.settings_inner_rect();
        let tab_y = inner.y + 1;
        let integrations_idx = SettingsSection::ALL
            .iter()
            .position(|section| *section == SettingsSection::Integrations)
            .expect("integrations section should be present");
        let integrations_x = inner.x
            + SettingsSection::ALL[..integrations_idx]
                .iter()
                .map(|section| {
                    let badge_width = if state.settings_section_has_badge(*section) {
                        2
                    } else {
                        0
                    };
                    section.label().len() as u16 + 3 + badge_width
                })
                .sum::<u16>();
        let dotted_width = SettingsSection::Integrations.label().len() as u16 + 4;

        assert_eq!(
            state.settings_tab_at(integrations_x + dotted_width - 1, tab_y),
            Some(SettingsSection::Integrations)
        );
    }

    fn integration_recommendation(
        state: crate::integration::IntegrationStatusKind,
        available: bool,
    ) -> crate::integration::IntegrationRecommendation {
        crate::integration::IntegrationRecommendation {
            target: crate::api::schema::IntegrationTarget::Claude,
            label: "claude",
            command: "claude",
            available,
            path: std::path::PathBuf::from("/tmp/herdr-test-integration"),
            state,
        }
    }
}
