use super::App;

impl App {
    pub(super) fn query_host_terminal_theme(&self) {
        use std::io::Write;

        let query = crate::terminal_theme::host_terminal_theme_query_sequence();
        let _ = std::io::stdout().write_all(query.as_bytes());
        let _ = std::io::stdout().flush();
    }

    /// Asks the host how big one of its cells is. See
    /// [`crate::kitty_graphics::HOST_CELL_SIZE_QUERY_SEQUENCE`].
    pub(super) fn query_host_cell_size(&self) {
        use std::io::Write;

        let query = crate::kitty_graphics::HOST_CELL_SIZE_QUERY_SEQUENCE;
        let _ = std::io::stdout().write_all(query.as_bytes());
        let _ = std::io::stdout().flush();
    }

    pub(super) fn update_host_terminal_theme(
        &mut self,
        kind: crate::terminal_theme::DefaultColorKind,
        color: crate::terminal_theme::RgbColor,
    ) -> bool {
        let mut changed = false;
        if matches!(kind, crate::terminal_theme::DefaultColorKind::Background)
            && !self.state.host_terminal_appearance_explicit
        {
            changed |= self.set_host_terminal_appearance(color.inferred_appearance(), false);
        }
        let next_theme = self.state.host_terminal_theme.with_color(kind, color);
        changed | self.set_host_terminal_theme(next_theme)
    }

    pub(super) fn update_host_terminal_palette_colors(
        &mut self,
        colors: &[(u8, crate::terminal_theme::RgbColor)],
    ) -> bool {
        let mut next_theme = self.state.host_terminal_theme;
        for &(index, color) in colors {
            next_theme = next_theme.with_palette_color(index, color);
        }
        self.set_host_terminal_theme(next_theme)
    }

    pub(super) fn set_host_terminal_appearance(
        &mut self,
        appearance: crate::terminal_theme::HostAppearance,
        explicit: bool,
    ) -> bool {
        if self.state.host_terminal_appearance == Some(appearance)
            && self.state.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        if self.state.host_terminal_appearance_explicit && !explicit {
            return false;
        }
        self.state.host_terminal_appearance = Some(appearance);
        self.state.host_terminal_appearance_explicit = explicit;
        self.refresh_effective_app_theme()
    }

    pub(crate) fn set_host_terminal_appearance_state(
        &mut self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
        explicit: bool,
    ) -> bool {
        if self.state.host_terminal_appearance == appearance
            && self.state.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        self.state.host_terminal_appearance = appearance;
        self.state.host_terminal_appearance_explicit = explicit;
        self.refresh_effective_app_theme()
    }

    pub(crate) fn set_host_terminal_theme(
        &mut self,
        theme: crate::terminal_theme::TerminalTheme,
    ) -> bool {
        if theme == self.state.host_terminal_theme {
            return false;
        }
        self.state.host_terminal_theme = theme;
        // The measured host colours feed Herdr's own chrome too, not just the
        // panes: the palette contrast floor is derived from them.
        self.refresh_effective_app_theme();
        self.apply_host_terminal_theme_to_panes();
        true
    }

    pub(super) fn refresh_effective_app_theme(&mut self) -> bool {
        let (palette, theme_name) = super::resolve_effective_theme(
            &self.state.theme_runtime,
            self.state.host_terminal_appearance,
            &self.state.host_terminal_theme,
        );
        if self.state.theme_name == theme_name && self.state.palette == palette {
            return false;
        }
        self.state.theme_name = theme_name;
        self.state.palette = palette;
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
        true
    }

    fn apply_host_terminal_theme_to_panes(&self) {
        for runtime in self.terminal_runtimes.values() {
            runtime.apply_host_terminal_theme(self.state.host_terminal_theme);
        }

        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::terminal_theme::{DefaultColorKind, RgbColor, TerminalTheme};
    use crate::ui::color::{contrast_ratio, resolve_color_rgb};

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn white() -> RgbColor {
        RgbColor {
            r: 255,
            g: 255,
            b: 255,
        }
    }

    #[test]
    fn a_measured_background_re_derives_the_palette() {
        let mut app = test_app();
        let before = app.state.palette.clone();

        // Not through `update_host_terminal_theme`: this is the case where the
        // host is light but the theme stays dark (auto-switch off, or an
        // explicit appearance), which is where the floor has to do the work.
        let theme = TerminalTheme::default().with_color(DefaultColorKind::Background, white());
        assert!(app.set_host_terminal_theme(theme));

        assert_ne!(app.state.palette, before);
        let overlay1 =
            resolve_color_rgb(app.state.palette.overlay1, &theme).expect("overlay1 should resolve");
        assert!(contrast_ratio(overlay1, (255, 255, 255)) >= 4.5);
    }

    #[test]
    fn losing_the_measurement_restores_the_authored_palette() {
        let mut app = test_app();
        let authored = app.state.palette.clone();

        app.set_host_terminal_theme(
            TerminalTheme::default().with_color(DefaultColorKind::Background, white()),
        );
        assert_ne!(app.state.palette, authored);

        app.set_host_terminal_theme(TerminalTheme::default());
        assert_eq!(app.state.palette, authored);
    }
}
