use crate::config::{
    Keybinds, NewTerminalCwdConfig, SoundConfig, TabBarPositionConfig, ToastConfig, ToastDelivery,
};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Direction, Rect};
use ratatui::style::Color;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::detect::AgentState;
use crate::layout::{PaneId, PaneInfo, SplitBorder};
use crate::selection::Selection;

pub(crate) type InstalledPluginRegistry =
    std::collections::HashMap<String, crate::api::schema::InstalledPluginInfo>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginPaneRecord {
    pub plugin_id: String,
    pub entrypoint: String,
}

/// A looping frame sequence attached to a [`GraphicsLayer`], armed once via Kitty's native
/// animation-frame transport (`a=f`/`a=a` in `src/kitty_graphics.rs`) instead of being
/// re-uploaded every tick. `GraphicsLayer::data` above carries the root/first frame; `frames`
/// here are the rest, in loop order. Only looping/ambient content benefits — anything
/// event-driven still needs a fresh `GraphicsLayer` per change, same as before this existed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GraphicsAnimation {
    /// Milliseconds each frame is shown before the terminal advances to the next one.
    pub frame_gap_ms: u32,
    pub frames: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GraphicsLayer {
    pub format: crate::api::schema::PaneGraphicsFormat,
    pub image_width: u32,
    pub image_height: u32,
    pub data: Vec<u8>,
    pub data_fingerprint: u64,
    pub render: crate::api::schema::PaneGraphicsPlacementParams,
    pub animation: Option<GraphicsAnimation>,
}

impl GraphicsLayer {
    pub(crate) fn new(
        format: crate::api::schema::PaneGraphicsFormat,
        image_width: u32,
        image_height: u32,
        data: Vec<u8>,
        render: crate::api::schema::PaneGraphicsPlacementParams,
    ) -> Self {
        let data_fingerprint = graphics_data_fingerprint(&data);
        Self {
            format,
            image_width,
            image_height,
            data,
            data_fingerprint,
            render,
            animation: None,
        }
    }

    /// Attaches a looping frame sequence, transmitted once and then played back by the terminal
    /// on its own clock. See [`GraphicsAnimation`].
    pub(crate) fn with_animation(mut self, animation: GraphicsAnimation) -> Self {
        self.animation = Some(animation);
        self
    }
}

fn graphics_data_fingerprint(data: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PopupPaneState {
    pub pane_id: PaneId,
    pub terminal_id: crate::terminal::TerminalId,
    pub width: Option<crate::popup_size::PopupSize>,
    pub height: Option<crate::popup_size::PopupSize>,
}

// ---------------------------------------------------------------------------
// Selection autoscroll types
// ---------------------------------------------------------------------------

/// Direction of automatic scrolling during text selection drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionAutoscrollDirection {
    Up,
    Down,
}

/// State for automatic scrolling during text selection drag.
///
/// When the cursor hovers in the 1-row hot zone at the top or bottom edge
/// of a pane (or outside the pane), this struct captures the direction and
/// last known mouse position so a recurring 30ms tick can continue scrolling
/// and extending the selection even when the mouse is not moving.
#[derive(Clone, Debug)]
pub(crate) struct SelectionAutoscroll {
    pub direction: SelectionAutoscrollDirection,
    pub last_mouse_screen_col: u16,
    pub last_mouse_screen_row: u16,
    pub inner_rect: Rect,
}

#[derive(Clone)]
pub(crate) struct RightClickPassthroughGesture {
    pub pane_info: PaneInfo,
    pub modifiers: KeyModifiers,
}
use crate::terminal_theme::{HostAppearance, TerminalTheme};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Theme palette — all UI colors in one place, ready for theming
// ---------------------------------------------------------------------------

/// All colors used by the UI. Derived from a base accent color for now,
/// but structured so a full theme system can replace it later.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // all fields defined for theming — some used later
pub struct Palette {
    /// Primary accent (highlight, active borders).
    pub accent: Color,
    /// Background for the tab bar, floating panels, overlays, and modals.
    pub panel_bg: Color,
    /// Optional desktop sidebar background. Reset preserves the terminal background.
    pub sidebar_bg: Color,
    /// Subtle surface background for selected/focused items.
    pub surface0: Color,
    /// Slightly lighter surface for hover/active states.
    pub surface1: Color,
    /// Very dim surface for separators.
    pub surface_dim: Color,
    /// Muted text (secondary info, numbers).
    pub overlay0: Color,
    /// Slightly brighter overlay text.
    pub overlay1: Color,
    /// Main text color — soft white.
    pub text: Color,
    /// Subdued text (workspace numbers, dim labels).
    pub subtext0: Color,
    /// Branch name / special label color.
    pub mauve: Color,
    /// Done / idle states.
    pub green: Color,
    /// Working / running states.
    pub yellow: Color,
    /// Needs attention / blocked states.
    pub red: Color,
    /// Unseen / done notification accent.
    pub blue: Color,
    /// Notification accent / unseen markers.
    pub teal: Color,
    /// Interrupted / warning states.
    pub peach: Color,
}

impl Palette {
    /// Catppuccin Mocha — the default.
    pub fn catppuccin() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250), // blue
            panel_bg: Color::Rgb(24, 24, 37),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(49, 50, 68),
            surface1: Color::Rgb(69, 71, 90),
            surface_dim: Color::Rgb(30, 30, 46),
            overlay0: Color::Rgb(108, 112, 134),
            overlay1: Color::Rgb(127, 132, 156),
            text: Color::Rgb(205, 214, 244),
            subtext0: Color::Rgb(166, 173, 200),
            mauve: Color::Rgb(203, 166, 247),
            green: Color::Rgb(166, 227, 161),
            yellow: Color::Rgb(249, 226, 175),
            red: Color::Rgb(243, 139, 168),
            blue: Color::Rgb(137, 180, 250),
            teal: Color::Rgb(148, 226, 213),
            peach: Color::Rgb(250, 179, 135),
        }
    }

    /// Catppuccin Latte — the light Catppuccin flavor.
    pub fn catppuccin_latte() -> Self {
        Self {
            accent: Color::Rgb(30, 102, 245),
            panel_bg: Color::Rgb(239, 241, 245),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(204, 208, 218),
            surface1: Color::Rgb(188, 192, 204),
            surface_dim: Color::Rgb(230, 233, 239),
            overlay0: Color::Rgb(156, 160, 176),
            overlay1: Color::Rgb(140, 143, 161),
            text: Color::Rgb(76, 79, 105),
            subtext0: Color::Rgb(108, 111, 133),
            mauve: Color::Rgb(136, 57, 239),
            green: Color::Rgb(64, 160, 43),
            yellow: Color::Rgb(223, 142, 29),
            red: Color::Rgb(210, 15, 57),
            blue: Color::Rgb(30, 102, 245),
            teal: Color::Rgb(23, 146, 153),
            peach: Color::Rgb(254, 100, 11),
        }
    }

    /// Terminal 16-color theme.
    pub fn terminal() -> Self {
        Self {
            accent: Color::Blue,
            panel_bg: Color::Reset,
            sidebar_bg: Color::Reset,
            surface0: Color::Reset,
            surface1: Color::DarkGray,
            surface_dim: Color::DarkGray,
            overlay0: Color::Gray,
            overlay1: Color::White,
            text: Color::Reset,
            subtext0: Color::Gray,
            mauve: Color::Gray,
            green: Color::Green,
            yellow: Color::Yellow,
            red: Color::LightRed,
            blue: Color::Blue,
            teal: Color::Cyan,
            peach: Color::Yellow,
        }
    }

    /// Tokyo Night — blue-purple aesthetic.
    pub fn tokyo_night() -> Self {
        Self {
            accent: Color::Rgb(122, 162, 247), // blue
            panel_bg: Color::Rgb(26, 27, 38),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(36, 40, 59),
            surface1: Color::Rgb(65, 72, 104),
            surface_dim: Color::Rgb(26, 27, 38),
            overlay0: Color::Rgb(86, 95, 137),
            overlay1: Color::Rgb(105, 113, 150),
            text: Color::Rgb(192, 202, 245),
            subtext0: Color::Rgb(169, 177, 214),
            mauve: Color::Rgb(187, 154, 247),
            green: Color::Rgb(158, 206, 106),
            yellow: Color::Rgb(224, 175, 104),
            red: Color::Rgb(247, 118, 142),
            blue: Color::Rgb(122, 162, 247),
            teal: Color::Rgb(125, 207, 255),
            peach: Color::Rgb(255, 158, 100),
        }
    }

    /// Tokyo Night Day — the light Tokyo Night style.
    pub fn tokyo_night_day() -> Self {
        Self {
            accent: Color::Rgb(46, 125, 233),
            panel_bg: Color::Rgb(225, 226, 231),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(196, 200, 218),
            surface1: Color::Rgb(168, 174, 203),
            surface_dim: Color::Rgb(210, 211, 218),
            overlay0: Color::Rgb(137, 144, 179),
            overlay1: Color::Rgb(104, 112, 154),
            text: Color::Rgb(55, 96, 191),
            subtext0: Color::Rgb(97, 114, 176),
            mauve: Color::Rgb(120, 71, 189),
            green: Color::Rgb(88, 117, 57),
            yellow: Color::Rgb(140, 108, 62),
            red: Color::Rgb(245, 42, 101),
            blue: Color::Rgb(46, 125, 233),
            teal: Color::Rgb(17, 140, 116),
            peach: Color::Rgb(177, 92, 0),
        }
    }

    /// Dracula — purple/pink/green.
    pub fn dracula() -> Self {
        Self {
            accent: Color::Rgb(189, 147, 249), // purple
            panel_bg: Color::Rgb(40, 42, 54),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(68, 71, 90),
            surface1: Color::Rgb(98, 114, 164),
            surface_dim: Color::Rgb(40, 42, 54),
            overlay0: Color::Rgb(98, 114, 164),
            overlay1: Color::Rgb(130, 140, 180),
            text: Color::Rgb(248, 248, 242),
            subtext0: Color::Rgb(210, 210, 220),
            mauve: Color::Rgb(255, 121, 198), // pink
            green: Color::Rgb(80, 250, 123),
            yellow: Color::Rgb(241, 250, 140),
            red: Color::Rgb(255, 85, 85),
            blue: Color::Rgb(139, 233, 253), // cyan-ish
            teal: Color::Rgb(139, 233, 253),
            peach: Color::Rgb(255, 184, 108),
        }
    }

    /// Nord — frosty blue palette.
    pub fn nord() -> Self {
        Self {
            accent: Color::Rgb(136, 192, 208), // frost
            panel_bg: Color::Rgb(46, 52, 64),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(59, 66, 82),
            surface1: Color::Rgb(67, 76, 94),
            surface_dim: Color::Rgb(46, 52, 64),
            overlay0: Color::Rgb(76, 86, 106),
            overlay1: Color::Rgb(100, 110, 130),
            text: Color::Rgb(236, 239, 244),
            subtext0: Color::Rgb(216, 222, 233),
            mauve: Color::Rgb(180, 142, 173),
            green: Color::Rgb(163, 190, 140),
            yellow: Color::Rgb(235, 203, 139),
            red: Color::Rgb(191, 97, 106),
            blue: Color::Rgb(129, 161, 193),
            teal: Color::Rgb(143, 188, 187),
            peach: Color::Rgb(208, 135, 112),
        }
    }

    /// Gruvbox Dark — warm retro palette.
    pub fn gruvbox() -> Self {
        Self {
            accent: Color::Rgb(215, 153, 33), // yellow
            panel_bg: Color::Rgb(40, 40, 40),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(60, 56, 54),
            surface1: Color::Rgb(80, 73, 69),
            surface_dim: Color::Rgb(40, 40, 40),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(168, 153, 132),
            text: Color::Rgb(235, 219, 178),
            subtext0: Color::Rgb(213, 196, 161),
            mauve: Color::Rgb(211, 134, 155),
            green: Color::Rgb(184, 187, 38),
            yellow: Color::Rgb(250, 189, 47),
            red: Color::Rgb(251, 73, 52),
            blue: Color::Rgb(131, 165, 152),
            teal: Color::Rgb(142, 192, 124),
            peach: Color::Rgb(254, 128, 25),
        }
    }

    /// Gruvbox Light — the light retro palette.
    pub fn gruvbox_light() -> Self {
        Self {
            accent: Color::Rgb(7, 102, 120),
            panel_bg: Color::Rgb(251, 241, 199),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(235, 219, 178),
            surface1: Color::Rgb(213, 196, 161),
            surface_dim: Color::Rgb(242, 229, 188),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(124, 111, 100),
            text: Color::Rgb(60, 56, 54),
            subtext0: Color::Rgb(80, 73, 69),
            mauve: Color::Rgb(143, 63, 113),
            green: Color::Rgb(121, 116, 14),
            yellow: Color::Rgb(181, 118, 20),
            red: Color::Rgb(157, 0, 6),
            blue: Color::Rgb(7, 102, 120),
            teal: Color::Rgb(66, 123, 88),
            peach: Color::Rgb(175, 58, 3),
        }
    }

    /// One Dark — Atom's classic dark theme.
    pub fn one_dark() -> Self {
        Self {
            accent: Color::Rgb(97, 175, 239), // blue
            panel_bg: Color::Rgb(40, 44, 52),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(44, 49, 58),
            surface1: Color::Rgb(62, 68, 81),
            surface_dim: Color::Rgb(40, 44, 52),
            overlay0: Color::Rgb(92, 99, 112),
            overlay1: Color::Rgb(115, 122, 135),
            text: Color::Rgb(171, 178, 191),
            subtext0: Color::Rgb(150, 156, 168),
            mauve: Color::Rgb(198, 120, 221),
            green: Color::Rgb(152, 195, 121),
            yellow: Color::Rgb(229, 192, 123),
            red: Color::Rgb(224, 108, 117),
            blue: Color::Rgb(97, 175, 239),
            teal: Color::Rgb(86, 182, 194),
            peach: Color::Rgb(209, 154, 102),
        }
    }

    /// One Light — Atom's classic light theme.
    pub fn one_light() -> Self {
        Self {
            accent: Color::Rgb(64, 120, 242),
            panel_bg: Color::Rgb(250, 250, 250),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(240, 240, 241),
            surface1: Color::Rgb(229, 229, 230),
            surface_dim: Color::Rgb(245, 245, 246),
            overlay0: Color::Rgb(160, 161, 167),
            overlay1: Color::Rgb(104, 107, 119),
            text: Color::Rgb(56, 58, 66),
            subtext0: Color::Rgb(104, 107, 119),
            mauve: Color::Rgb(166, 38, 164),
            green: Color::Rgb(80, 161, 79),
            yellow: Color::Rgb(193, 132, 1),
            red: Color::Rgb(228, 86, 73),
            blue: Color::Rgb(64, 120, 242),
            teal: Color::Rgb(1, 132, 188),
            peach: Color::Rgb(152, 104, 1),
        }
    }

    /// Solarized Dark — Ethan Schoonover's classic.
    pub fn solarized() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210), // blue
            panel_bg: Color::Rgb(0, 43, 54),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(7, 54, 66),
            surface1: Color::Rgb(88, 110, 117),
            surface_dim: Color::Rgb(0, 43, 54),
            overlay0: Color::Rgb(88, 110, 117),
            overlay1: Color::Rgb(101, 123, 131),
            text: Color::Rgb(147, 161, 161),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
        }
    }

    /// Solarized Light — Ethan Schoonover's light variant.
    pub fn solarized_light() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210),
            panel_bg: Color::Rgb(253, 246, 227),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(238, 232, 213),
            surface1: Color::Rgb(147, 161, 161),
            surface_dim: Color::Rgb(238, 232, 213),
            overlay0: Color::Rgb(147, 161, 161),
            overlay1: Color::Rgb(88, 110, 117),
            text: Color::Rgb(101, 123, 131),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
        }
    }

    /// Kanagawa — inspired by Katsushika Hokusai.
    pub fn kanagawa() -> Self {
        Self {
            accent: Color::Rgb(126, 156, 216), // blue
            panel_bg: Color::Rgb(31, 31, 40),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(42, 42, 55),
            surface1: Color::Rgb(54, 54, 70),
            surface_dim: Color::Rgb(31, 31, 40),
            overlay0: Color::Rgb(114, 113, 105),
            overlay1: Color::Rgb(135, 134, 125),
            text: Color::Rgb(220, 215, 186),
            subtext0: Color::Rgb(200, 195, 170),
            mauve: Color::Rgb(149, 127, 184),
            green: Color::Rgb(118, 148, 106),
            yellow: Color::Rgb(192, 163, 110),
            red: Color::Rgb(195, 64, 67),
            blue: Color::Rgb(126, 156, 216),
            teal: Color::Rgb(127, 180, 202),
            peach: Color::Rgb(255, 160, 102),
        }
    }

    /// Kanagawa Lotus — the light Kanagawa variant.
    pub fn kanagawa_lotus() -> Self {
        Self {
            accent: Color::Rgb(77, 105, 155),
            panel_bg: Color::Rgb(242, 236, 188),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(220, 213, 172),
            surface1: Color::Rgb(201, 203, 209),
            surface_dim: Color::Rgb(213, 206, 163),
            overlay0: Color::Rgb(160, 156, 172),
            overlay1: Color::Rgb(138, 137, 128),
            text: Color::Rgb(84, 84, 100),
            subtext0: Color::Rgb(67, 67, 108),
            mauve: Color::Rgb(98, 76, 131),
            green: Color::Rgb(111, 137, 78),
            yellow: Color::Rgb(119, 113, 63),
            red: Color::Rgb(200, 64, 83),
            blue: Color::Rgb(77, 105, 155),
            teal: Color::Rgb(78, 140, 162),
            peach: Color::Rgb(204, 109, 0),
        }
    }

    /// Rosé Pine — muted, elegant.
    pub fn rose_pine() -> Self {
        Self {
            accent: Color::Rgb(196, 167, 231), // iris
            panel_bg: Color::Rgb(25, 23, 36),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(31, 29, 46),
            surface1: Color::Rgb(38, 35, 58),
            surface_dim: Color::Rgb(38, 35, 58),
            overlay0: Color::Rgb(110, 106, 134),
            overlay1: Color::Rgb(144, 140, 170),
            text: Color::Rgb(224, 222, 244),
            subtext0: Color::Rgb(200, 197, 220),
            mauve: Color::Rgb(196, 167, 231),  // iris
            green: Color::Rgb(49, 116, 143),   // pine
            yellow: Color::Rgb(246, 193, 119), // gold
            red: Color::Rgb(235, 111, 146),    // love
            blue: Color::Rgb(49, 116, 143),    // pine
            teal: Color::Rgb(156, 207, 216),   // foam
            peach: Color::Rgb(234, 154, 151),  // rose
        }
    }

    /// Rosé Pine Dawn — the light Rosé Pine variant.
    pub fn rose_pine_dawn() -> Self {
        Self {
            accent: Color::Rgb(144, 122, 169),
            panel_bg: Color::Rgb(250, 244, 237),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(242, 233, 225),
            surface1: Color::Rgb(255, 250, 243),
            surface_dim: Color::Rgb(242, 233, 225),
            overlay0: Color::Rgb(152, 147, 165),
            overlay1: Color::Rgb(121, 117, 147),
            text: Color::Rgb(70, 66, 97),
            subtext0: Color::Rgb(121, 117, 147),
            mauve: Color::Rgb(144, 122, 169),
            green: Color::Rgb(40, 105, 131),
            yellow: Color::Rgb(234, 157, 52),
            red: Color::Rgb(180, 99, 122),
            blue: Color::Rgb(40, 105, 131),
            teal: Color::Rgb(86, 148, 159),
            peach: Color::Rgb(215, 130, 126),
        }
    }

    /// Vesper — minimal high-contrast monochrome with peach and mint accents.
    pub fn vesper() -> Self {
        Self {
            accent: Color::Rgb(255, 199, 153),
            panel_bg: Color::Rgb(26, 26, 26),
            sidebar_bg: Color::Reset,
            surface0: Color::Rgb(35, 35, 35),
            surface1: Color::Rgb(40, 40, 40),
            surface_dim: Color::Rgb(16, 16, 16),
            overlay0: Color::Rgb(92, 92, 92),
            overlay1: Color::Rgb(126, 126, 126),
            text: Color::Rgb(255, 255, 255),
            subtext0: Color::Rgb(160, 160, 160),
            mauve: Color::Rgb(255, 209, 168),
            green: Color::Rgb(153, 255, 228),
            yellow: Color::Rgb(255, 199, 153),
            red: Color::Rgb(255, 128, 128),
            blue: Color::Rgb(176, 176, 176),
            teal: Color::Rgb(102, 221, 204),
            peach: Color::Rgb(255, 199, 153),
        }
    }

    /// Resolve a theme by name. Returns None for unknown names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().replace([' ', '_'], "-").as_str() {
            "catppuccin" | "catppuccin-mocha" => Some(Self::catppuccin()),
            "catppuccin-latte" | "latte" | "light" => Some(Self::catppuccin_latte()),
            "terminal" => Some(Self::terminal()),
            "tokyo-night" | "tokyonight" => Some(Self::tokyo_night()),
            "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => Some(Self::tokyo_night_day()),
            "dracula" => Some(Self::dracula()),
            "nord" => Some(Self::nord()),
            "gruvbox" | "gruvbox-dark" => Some(Self::gruvbox()),
            "gruvbox-light" => Some(Self::gruvbox_light()),
            "one-dark" | "onedark" => Some(Self::one_dark()),
            "one-light" | "onelight" => Some(Self::one_light()),
            "solarized" | "solarized-dark" => Some(Self::solarized()),
            "solarized-light" => Some(Self::solarized_light()),
            "kanagawa" => Some(Self::kanagawa()),
            "kanagawa-lotus" | "lotus" => Some(Self::kanagawa_lotus()),
            "rose-pine" | "rosepine" => Some(Self::rose_pine()),
            "rose-pine-dawn" | "rosepine-dawn" | "dawn" => Some(Self::rose_pine_dawn()),
            "vesper" => Some(Self::vesper()),
            _ => None,
        }
    }

    /// Apply custom color overrides on top of this palette.
    pub fn with_overrides(mut self, custom: &crate::config::CustomThemeColors) -> Self {
        use crate::config::parse_color;
        if let Some(c) = &custom.accent {
            self.accent = parse_color(c);
        }
        if let Some(c) = &custom.panel_bg {
            self.panel_bg = parse_color(c);
        }
        if let Some(c) = &custom.sidebar_bg {
            self.sidebar_bg = parse_color(c);
        }
        if let Some(c) = &custom.surface0 {
            self.surface0 = parse_color(c);
        }
        if let Some(c) = &custom.surface1 {
            self.surface1 = parse_color(c);
        }
        if let Some(c) = &custom.surface_dim {
            self.surface_dim = parse_color(c);
        }
        if let Some(c) = &custom.overlay0 {
            self.overlay0 = parse_color(c);
        }
        if let Some(c) = &custom.overlay1 {
            self.overlay1 = parse_color(c);
        }
        if let Some(c) = &custom.text {
            self.text = parse_color(c);
        }
        if let Some(c) = &custom.subtext0 {
            self.subtext0 = parse_color(c);
        }
        if let Some(c) = &custom.mauve {
            self.mauve = parse_color(c);
        }
        if let Some(c) = &custom.green {
            self.green = parse_color(c);
        }
        if let Some(c) = &custom.yellow {
            self.yellow = parse_color(c);
        }
        if let Some(c) = &custom.red {
            self.red = parse_color(c);
        }
        if let Some(c) = &custom.blue {
            self.blue = parse_color(c);
        }
        if let Some(c) = &custom.teal {
            self.teal = parse_color(c);
        }
        if let Some(c) = &custom.peach {
            self.peach = parse_color(c);
        }
        self
    }

    /// WCAG AA for body text — `overlay1` is read as text.
    const OVERLAY1_FLOOR: f32 = 4.5;
    /// WCAG AA for large text and non-text UI — `overlay0` is secondary text
    /// and scrollbar thumbs.
    const OVERLAY0_FLOOR: f32 = 3.0;
    /// Deliberately *not* a WCAG number. Separators, scrollbar tracks and
    /// selection fills are meant to be nearly invisible, so this only has to
    /// catch "literally indistinguishable from the background" and lift just
    /// far enough to be seen. Calibrated so a hand-tuned theme on its own
    /// background is barely touched — Catppuccin Mocha's `surface_dim` (which
    /// equals its base colour, so it vanishes on a matching host) moves
    /// #1e1e2e → #262635 and nothing else in the theme changes.
    const SURFACE_FLOOR: f32 = 1.1;

    /// Raise the four muted tokens until they clear a minimum contrast against
    /// one background.
    ///
    /// Only `surface0`, `surface_dim`, `overlay0` and `overlay1`. Accents carry
    /// meaning and are hand-tuned per theme, so a computed floor on them would
    /// look worse than the palette it "fixed"; these four are the tokens whose
    /// whole job is to be quiet, and therefore the ones that disappear when the
    /// palette and the background disagree.
    ///
    /// One background, never a sequence of them: `ensure_contrast` promises
    /// only that it will not lower contrast against *the background it was
    /// given*, so lifting a token away from a second background can push it
    /// back under its floor on the first. A token drawn on two surfaces
    /// therefore needs two floored copies, not one colour floored twice.
    fn floor_quiet_tokens(
        &mut self,
        host: &crate::terminal_theme::TerminalTheme,
        background: crate::ui::color::Rgb,
    ) {
        use crate::ui::color::{ensure_contrast, resolve_color_rgb};

        for (token, floor) in [
            (&mut self.surface0, Self::SURFACE_FLOOR),
            (&mut self.surface_dim, Self::SURFACE_FLOOR),
            (&mut self.overlay0, Self::OVERLAY0_FLOOR),
            (&mut self.overlay1, Self::OVERLAY1_FLOOR),
        ] {
            let Some(rgb) = resolve_color_rgb(*token, host) else {
                continue;
            };
            let floored = ensure_contrast(rgb, background, floor);
            if floored != rgb {
                *token = Color::Rgb(floored.0, floored.1, floored.2);
            }
        }
    }

    /// The palette as everything outside the sidebar draws it: floored against
    /// the host terminal background Herdr measured over OSC 10/11.
    ///
    /// That is the right surface for these tokens almost everywhere, because
    /// Herdr paints no global fill — a pane, the tab bar and an unstyled panel
    /// all composite straight onto the host's background.
    ///
    /// Two deliberate no-ops:
    /// - No measured background (the host never answered the OSC query, which
    ///   multiplexers commonly cause) leaves the palette exactly as authored.
    /// - `Color::Reset` means "inherit the host" and is never rewritten, so a
    ///   theme that opts out of painting a surface keeps opting out.
    pub fn with_contrast_floor(mut self, host: &crate::terminal_theme::TerminalTheme) -> Self {
        use crate::ui::color::terminal_theme_to_rgb;

        let Some(background) = host.background.map(terminal_theme_to_rgb) else {
            return self;
        };
        self.floor_quiet_tokens(host, background);
        self
    }

    /// The same palette as the desktop sidebar draws it.
    ///
    /// A theme that sets `sidebar_bg` gives the panel a fill of its own, so the
    /// quiet tokens in the tree land on that colour rather than on the host's
    /// background and need their own floor against it. It has to be a second
    /// copy: `overlay1` is also the settings and modal description ink, and a
    /// single token floored for a dark panel on a light host is unreadable in
    /// the modal — the two surfaces can straddle mid-grey, and then no one
    /// colour clears both.
    ///
    /// Returns the palette unchanged when the panel has no fill of its own,
    /// which is the default: `Color::Reset` resolves to no colour, so the
    /// sidebar keeps drawing with the host-floored tokens.
    pub fn for_sidebar(&self, host: &crate::terminal_theme::TerminalTheme) -> Self {
        let mut sidebar = self.clone();
        let Some(background) = crate::ui::color::resolve_color_rgb(self.sidebar_bg, host) else {
            return sidebar;
        };
        sidebar.floor_quiet_tokens(host, background);
        sidebar
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCardArea {
    pub ws_idx: usize,
    pub rect: Rect,
    pub worktree_child: bool,
    /// Position of this row in the sidebar tree, so the renderer and the hit
    /// tests read the same connector depth the layout used.
    pub entry_idx: usize,
    /// The pane this row draws, when the row is an agent rather than a Space.
    ///
    /// Agent rows share the card list because they share the layout, but they
    /// are not Spaces: clicking one focuses its pane, and a workspace reorder
    /// drag must not treat it as a drop anchor.
    pub agent: Option<AgentCardTarget>,
    /// The card shell drawn around this row, when the panel is wide enough for
    /// one.
    ///
    /// `None` is the bare styled line, whose content starts on `rect`'s own
    /// first row and reaches `rect`'s own right edge. `Some(frame)` is the
    /// bordered box: it starts after the row's tree rails and its columns are
    /// measured against the panel's *fold* width rather than its drawn width,
    /// so the frame's right edge cannot shift by a column when the scrollbar
    /// comes and goes.
    ///
    /// Controls drawn over a row rather than laid out in it — the worktree
    /// chevron, the worker-summary badge — anchor on
    /// [`WorkspaceCardArea::content_y`] and
    /// [`WorkspaceCardArea::control_right`], never on `rect` directly, because
    /// a card's first row is a border.
    pub card_frame: Option<Rect>,
    /// Where this row is *drawn* relative to where the layout put it, in whole
    /// cells, while it is arriving or leaving. `(0, 0)` at rest and whenever
    /// rows are not moving on this host.
    ///
    /// Deliberately beside [`WorkspaceCardArea::rect`] rather than folded into
    /// it. The layout stays the sole authority on where a row *is* — hit
    /// testing, the wheel, the scrollbar and the workspace drop slots all read
    /// `rect`, and moving it would make a click during a transition land on a
    /// row the user is not pointing at. This is the drawing side of the same
    /// row, and only the two renderers that draw it read it: the pixel card's
    /// placement and the tree's connector rails beside it. Both take the number
    /// from here rather than deriving it, because being attached to each other
    /// is the whole point.
    ///
    /// Stamped by [`crate::ui::sidebar::image_card::build_cards`], which is the
    /// one place the offset is resolved — see
    /// [`crate::ui::sidebar::motion::cell_offsets`].
    pub motion_cells: (i32, i32),
}

impl WorkspaceCardArea {
    /// The first row of this card that carries content rather than chrome.
    pub fn content_y(&self) -> u16 {
        self.card_frame
            .map(|frame| frame.y.saturating_add(1))
            .unwrap_or(self.rect.y)
    }

    /// One past the rightmost column a control drawn over this row may occupy.
    ///
    /// The bare row ends at its own right edge; a card ends one column inside
    /// its right border, so a chevron never lands *on* the frame.
    pub fn control_right(&self) -> u16 {
        self.card_frame
            .map(|frame| frame.x.saturating_add(frame.width).saturating_sub(1))
            .unwrap_or_else(|| self.rect.x.saturating_add(self.rect.width))
    }
}

/// The pane an agent row in the sidebar tree points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCardTarget {
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCreateState {
    pub source_workspace_id: String,
    pub source_checkout_path: std::path::PathBuf,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub branch: String,
    pub checkout_path: std::path::PathBuf,
    pub error: Option<String>,
    pub creating: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemoveState {
    pub workspace_id: String,
    pub repo_root: std::path::PathBuf,
    pub path: std::path::PathBuf,
    pub error: Option<String>,
    pub removing: bool,
    pub force_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOpenEntry {
    pub path: std::path::PathBuf,
    pub branch: Option<String>,
    pub is_linked_worktree: bool,
    pub already_open_ws_idx: Option<usize>,
}

impl WorktreeOpenEntry {
    pub(crate) fn display_name(&self) -> String {
        self.branch.clone().unwrap_or_else(|| {
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| self.path.display().to_string())
        })
    }

    pub(crate) fn status_label(&self) -> &'static str {
        if self.already_open_ws_idx.is_some() {
            "open"
        } else if self.branch.is_some() {
            ""
        } else if self.is_linked_worktree {
            "detached"
        } else {
            "root"
        }
    }

    fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.display_name(),
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
            self.path.display(),
            self.status_label()
        )
        .to_lowercase()
    }

    fn matches_query(&self, query: &str) -> bool {
        text_matches_query(query, &self.search_text())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOpenState {
    pub source_workspace_id: String,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub entries: Vec<WorktreeOpenEntry>,
    pub selected: usize,
    pub query: String,
    pub search_focused: bool,
    pub error: Option<String>,
}

impl WorktreeOpenState {
    pub(crate) fn filtered_indices(&self) -> Vec<usize> {
        let query = self.query.trim();
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                (query.is_empty() || entry.matches_query(query)).then_some(idx)
            })
            .collect()
    }

    pub(crate) fn selected_entry_index(&self) -> Option<usize> {
        let indices = self.filtered_indices();
        if indices.contains(&self.selected) {
            Some(self.selected)
        } else {
            indices.first().copied()
        }
    }

    pub(crate) fn normalize_selection(&mut self) {
        if let Some(selected) = self.selected_entry_index() {
            self.selected = selected;
        }
    }

    pub(crate) fn select_previous_filtered(&mut self) {
        let indices = self.filtered_indices();
        let Some(current) = self.selected_entry_index() else {
            return;
        };
        let pos = indices.iter().position(|idx| *idx == current).unwrap_or(0);
        self.selected = indices[pos.saturating_sub(1)];
    }

    pub(crate) fn select_next_filtered(&mut self) {
        let indices = self.filtered_indices();
        let Some(current) = self.selected_entry_index() else {
            return;
        };
        let pos = indices.iter().position(|idx| *idx == current).unwrap_or(0);
        self.selected = indices[(pos + 1).min(indices.len().saturating_sub(1))];
    }
}

pub(crate) fn text_matches_query(query: &str, text: &str) -> bool {
    let haystack = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|needle| haystack.contains(needle))
}

/// Computed view geometry — derived from AppState + terminal size.
/// Updated before each render, consumed by render and mouse handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewLayout {
    Desktop,
    Mobile,
}

pub struct ViewState {
    pub layout: ViewLayout,
    pub sidebar_rect: Rect,
    pub workspace_card_areas: Vec<WorkspaceCardArea>,
    /// Whether *this* pass built the sidebar's card artwork.
    ///
    /// A property of the pass about to be encoded, never of shared state.
    /// `AppState::host_cell_size` and `AppState::sidebar_card_layers` belong to
    /// the foreground client, and a pass that cannot see the host's cell size
    /// deliberately leaves both alone. It is the single truth both halves of the
    /// pixel path read: `ui::sidebar::image_card::shape_covers_row` will not
    /// suppress a row's character card unless this pass drew a shape over it,
    /// and `kitty_graphics::surface_layer_placement_targets` will not send a
    /// pass card images it did not publish. Splitting those two apart is what
    /// draws a tree of bare connectors on one client and doubled borders on the
    /// other.
    pub sidebar_card_layers_published: bool,
    pub tab_bar_rect: Rect,
    pub tab_hit_areas: Vec<Rect>,
    pub tab_scroll_left_hit_area: Rect,
    pub tab_scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
    pub terminal_area: Rect,
    pub mobile_header_rect: Rect,
    pub mobile_menu_hit_area: Rect,
    pub toast_hit_area: Rect,
    pub pane_infos: Vec<PaneInfo>,
    pub split_borders: Vec<SplitBorder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Onboarding,
    ReleaseNotes,
    ProductAnnouncement,
    Navigate,
    Prefix,
    Copy,
    Terminal,
    RenameWorkspace,
    RenameTab,
    RenamePane,
    NewLinkedWorktree,
    OpenExistingWorktree,
    ConfirmRemoveWorktree,
    Resize,
    ConfirmClose,
    ContextMenu,
    Settings,
    GlobalMenu,
    KeybindHelp,
    Navigator,
    /// One second mate's finished workers and what they said they did.
    WorkerSummaries,
    /// The notification tray's popover: one badge's contents, or the legend.
    SignalTray,
}

impl Mode {
    pub(crate) fn mouse_motion_changes_view(self) -> bool {
        matches!(self, Self::GlobalMenu | Self::ContextMenu | Self::Navigator)
    }

    /// Whether keys in this mode are commands/navigation (an ASCII input source is wanted) rather
    /// than free text. This is an explicit **allowlist** of the prefix command/navigation realm:
    /// any mode NOT listed defaults to leaving the user's IME alone (the safe default), so adding a
    /// new text-entry or overlay mode can never silently force ASCII. Used by
    /// `sync_prefix_input_source` (gated by `switch_ascii_input_source_in_prefix`) so multi-level
    /// prefix commands keep ASCII until they return to the terminal.
    ///
    /// Known limitation: the search boxes in `Navigator` and `KeybindHelp` are also held on ASCII,
    /// since this `Mode`-level predicate can't see `search_focused` (non-ASCII filtering there
    /// would need a runtime check).
    pub(crate) fn wants_ascii_input(self) -> bool {
        matches!(
            self,
            Mode::Prefix
                | Mode::Navigate
                | Mode::Navigator
                | Mode::Copy
                | Mode::Resize
                | Mode::ConfirmClose
                | Mode::ConfirmRemoveWorktree
                | Mode::ContextMenu
                | Mode::GlobalMenu
                | Mode::KeybindHelp
                | Mode::WorkerSummaries
                | Mode::SignalTray
        )
    }
}

/// The open summary view: one second mate's workers, never the fleet.
///
/// The owner handle is kept rather than a row index because rows move — a
/// worker finishing re-sorts the tree under `AgentPanelSort::Priority` — and
/// the view must keep describing the mate the user actually clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerSummariesState {
    /// The second mate's tree handle, as its workers' `owner` tokens spell it.
    pub owner: String,
    /// Rows scrolled off the top of the body.
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavigatorTarget {
    Workspace {
        ws_idx: usize,
    },
    Tab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Pane {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavigatorRow {
    pub target: NavigatorTarget,
    pub depth: u8,
    pub label: String,
    pub meta: String,
    pub status: AgentState,
    pub seen: bool,
    pub is_current: bool,
    pub is_workspace: bool,
    pub is_tab: bool,
    pub expanded: bool,
    pub search_text: String,
    /// Whether this row itself matched the active query/state filter, as
    /// opposed to being included as ancestor context or cascaded subtree of a
    /// matching workspace or tab. Always true when no filter is active.
    pub matched: bool,
}

/// One rendered line in the navigator body. Spacer lines separate workspace
/// groups visually and are not selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigatorDisplayLine {
    Spacer,
    Row(usize),
}

pub(crate) fn navigator_display_lines(rows: &[NavigatorRow]) -> Vec<NavigatorDisplayLine> {
    let mut lines = Vec::with_capacity(rows.len().saturating_mul(2));
    for (idx, row) in rows.iter().enumerate() {
        if row.is_workspace && !lines.is_empty() {
            lines.push(NavigatorDisplayLine::Spacer);
        }
        lines.push(NavigatorDisplayLine::Row(idx));
    }
    lines
}

pub(crate) fn navigator_display_index_of_row(
    lines: &[NavigatorDisplayLine],
    row_idx: usize,
) -> Option<usize> {
    lines
        .iter()
        .position(|line| *line == NavigatorDisplayLine::Row(row_idx))
}

pub(crate) fn navigator_first_row_at_or_after(
    lines: &[NavigatorDisplayLine],
    line_idx: usize,
) -> Option<usize> {
    lines.get(line_idx..)?.iter().find_map(|line| match line {
        NavigatorDisplayLine::Row(idx) => Some(*idx),
        NavigatorDisplayLine::Spacer => None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigatorStateFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NavigatorState {
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    pub search_focused: bool,
    pub state_filter: Option<NavigatorStateFilter>,
    pub expanded_workspaces: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeState {
    pub pane_id: PaneId,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub entry_offset_from_bottom: usize,
    pub selection: Option<CopyModeSelection>,
    pub search: CopyModeSearchState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSelection {
    Character,
    Linewise { anchor_row: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeSearchPrompt {
    pub direction: CopyModeSearchDirection,
    pub query: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CopyModeSearchState {
    pub prompt: Option<CopyModeSearchPrompt>,
    pub query: String,
    pub direction: Option<CopyModeSearchDirection>,
    pub matches: Vec<crate::pane::TerminalTextMatch>,
    pub current: Option<usize>,
    pub geometry: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentPanelSort {
    #[default]
    Spaces,
    Priority,
}

// ---------------------------------------------------------------------------
// Settings UI state
// ---------------------------------------------------------------------------

/// Which section of the settings panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Theme,
    Animation,
    Indicators,
    Sound,
    Toast,
    PaneLabels,
    Signals,
    Legend,
    Integrations,
}

impl SettingsSection {
    pub const ALL: &[Self] = &[
        Self::Theme,
        Self::Animation,
        Self::Indicators,
        Self::Sound,
        Self::Toast,
        Self::PaneLabels,
        Self::Signals,
        Self::Legend,
        Self::Integrations,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Animation => "animation",
            Self::Indicators => "indicators",
            Self::Sound => "sound",
            Self::Toast => "toasts",
            Self::PaneLabels => "pane labels",
            Self::Signals => "signals",
            Self::Legend => "legend",
            Self::Integrations => "integrations",
        }
    }
}

/// All built-in theme names in display order.
pub const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuListState {
    pub highlighted: usize,
}

impl MenuListState {
    pub fn new(highlighted: usize) -> Self {
        Self { highlighted }
    }

    pub fn move_prev(&mut self) {
        self.highlighted = self.highlighted.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.highlighted = (self.highlighted + 1).min(item_count - 1);
        }
    }

    pub fn hover(&mut self, idx: Option<usize>) {
        if let Some(idx) = idx {
            self.highlighted = idx;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionListState {
    pub selected: usize,
}

impl SelectionListState {
    pub fn new(selected: usize) -> Self {
        Self { selected }
    }

    pub fn move_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.selected = (self.selected + 1).min(item_count - 1);
        }
    }

    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
    }
}

#[derive(Debug, Clone)]
pub struct ThemeRuntimeConfig {
    pub manual_name: String,
    pub dark_name: String,
    pub light_name: String,
    pub auto_switch: bool,
    pub custom: Option<crate::config::CustomThemeColors>,
    pub legacy_accent: Option<String>,
}

pub struct SettingsState {
    /// Which section tab is active.
    pub section: SettingsSection,
    /// Selected item index within the current section.
    pub list: SelectionListState,
    /// The palette before opening settings (for cancel/restore).
    pub original_palette: Option<Palette>,
    /// The theme name before opening settings.
    pub original_theme: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceDropTarget {
    Before(usize),
    End,
}

pub(crate) enum DragTarget {
    WorkspaceReorder {
        source_ws_idx: usize,
        drop_target: Option<WorkspaceDropTarget>,
    },
    TabReorder {
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: Option<usize>,
    },
    WorkspaceListScrollbar {
        grab_row_offset: u16,
    },
    PaneSplit {
        path: Vec<bool>,
        direction: Direction,
        area: Rect,
        grab_offset: u16,
    },
    PaneScrollbar {
        pane_id: crate::layout::PaneId,
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    ProductAnnouncementScrollbar {
        grab_row_offset: u16,
    },
    KeybindHelpScrollbar {
        grab_row_offset: u16,
    },
    SidebarDivider {
        grab_offset: i16,
    },
}

/// Active mouse drag on a split border or sidebar divider.
pub(crate) struct DragState {
    pub target: DragTarget,
}

pub(crate) struct WorkspacePressState {
    pub ws_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct TabPressState {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuKind {
    Workspace {
        ws_idx: usize,
    },
    GitWorkspace {
        ws_idx: usize,
        is_linked_worktree: bool,
        has_worktree_children: bool,
        collapsed: bool,
    },
    Tab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Pane {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
        source_pane_id: Option<PaneId>,
        has_manual_label: bool,
    },
}

/// Right-click context menu state.
pub struct ContextMenuState {
    pub kind: ContextMenuKind,
    pub x: u16,
    pub y: u16,
    pub list: MenuListState,
}

impl ContextMenuState {
    pub fn items(&self) -> &'static [&'static str] {
        match self.kind {
            ContextMenuKind::Workspace { .. } => &["Rename", "Close"],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: false,
                ..
            } => &["Rename", "Close", "New worktree", "Open worktree..."],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: true,
                ..
            } => &["Rename", "Close", "Delete worktree checkout..."],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: true,
                ..
            } => &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Expand",
            ],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
                ..
            } => &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Collapse",
            ],
            ContextMenuKind::Tab { .. } => &["New tab", "Rename", "Close"],
            ContextMenuKind::Pane {
                has_manual_label: true,
                source_pane_id: Some(_),
                ..
            } => &[
                "Rename pane",
                "Clear pane name",
                "Swap with focused pane",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                source_pane_id: Some(_),
                ..
            } => &[
                "Rename pane",
                "Swap with focused pane",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: true,
                source_pane_id: None,
                ..
            } => &[
                "Rename pane",
                "Clear pane name",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                source_pane_id: None,
                ..
            } => &[
                "Rename pane",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
    /// A pane's process ended with a failing status.
    ProcessFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastNotification {
    pub kind: ToastKind,
    pub title: String,
    pub context: String,
    pub position: Option<crate::config::ToastHerdrPosition>,
    pub target: Option<ToastTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAgentNotification {
    pub pane_id: PaneId,
    pub workspace_id: String,
    pub agent_label: String,
    pub known_agent: Option<crate::detect::Agent>,
    pub kind: ToastKind,
    pub state: AgentState,
    pub deadline: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNotificationDelivery {
    pub pane_id: PaneId,
    pub workspace_id: String,
    pub agent_label: String,
    pub known_agent: Option<crate::detect::Agent>,
    pub kind: ToastKind,
    pub toast: Option<ToastNotification>,
    pub client_notification: Option<ToastNotification>,
    pub sound: Option<crate::sound::Sound>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFeedback {
    pub message: String,
}

pub struct ReleaseNotesState {
    pub version: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

pub struct ProductAnnouncementState {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

#[derive(Default)]
pub struct KeybindHelpState {
    pub scroll: u16,
    pub query: String,
    pub search_focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarWidthSource {
    ConfigDefault,
    Persisted,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneFocusTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

/// All application state — pure data, no channels or async runtime.
/// Testable without PTYs or a tokio runtime.
pub struct AppState {
    pub terminals:
        std::collections::HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    /// Terminal ids whose size is currently owned by a direct attach client.
    pub direct_attach_resize_locks: std::collections::HashSet<crate::terminal::TerminalId>,
    pub(crate) pane_id_aliases: std::collections::HashMap<u32, PaneId>,
    pub(crate) public_pane_id_aliases: std::collections::HashMap<String, PaneId>,
    pub workspaces: Vec<Workspace>,
    pub active: Option<usize>,
    pub(crate) previous_pane_focus: Option<PaneFocusTarget>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    /// In monolithic --no-session mode, detach exits the app because there is no server to detach from.
    pub detach_exits: bool,
    /// Set when the current client should detach from the persistent session.
    /// The server's event loop checks this and handles client detach.
    pub detach_requested: bool,
    pub request_new_workspace: bool,
    pub request_new_tab: bool,
    pub request_new_linked_worktree: Option<usize>,
    pub request_open_existing_worktree: Option<usize>,
    pub request_new_workspace_cwd: Option<std::path::PathBuf>,
    pub request_remove_linked_worktree: Option<usize>,
    pub request_submit_worktree_create: bool,
    pub request_submit_worktree_open: bool,
    pub request_submit_worktree_remove: bool,
    pub request_reload_config: bool,
    /// Set when the headless server should ask attached clients to reload
    /// their client-local sound config from disk.
    pub request_client_config_reload: bool,
    /// Set when UI interaction requested a clipboard write that must be
    /// handled by the outer App/event loop instead of directly from AppState.
    pub request_clipboard_write: Option<Vec<u8>>,
    pub creating_new_tab: bool,
    pub requested_new_tab_name: Option<String>,
    pub pending_workspace_create_cwd: Option<std::path::PathBuf>,
    pub rename_pane_target: Option<PaneId>,
    pub worktree_create: Option<WorktreeCreateState>,
    pub worktree_open: Option<WorktreeOpenState>,
    pub worktree_remove: Option<WorktreeRemoveState>,
    /// The open worker-summary view, scoped to one second mate.
    pub(crate) worker_summaries: Option<WorkerSummariesState>,
    pub worktree_directory: std::path::PathBuf,
    pub collapsed_space_keys: std::collections::HashSet<String>,
    pub request_complete_onboarding: bool,
    pub name_input: String,
    pub name_input_replace_on_type: bool,
    pub release_notes: Option<ReleaseNotesState>,
    pub product_announcement: Option<ProductAnnouncementState>,
    pub keybind_help: KeybindHelpState,
    pub navigator: NavigatorState,
    pub copy_mode: Option<CopyModeState>,
    pub workspace_scroll: usize,
    pub tab_scroll: usize,
    pub tab_scroll_follow_active: bool,
    pub mobile_switcher_scroll: usize,
    // View geometry (computed before render, consumed by render + mouse)
    pub view: ViewState,
    pub(crate) drag: Option<DragState>,
    /// Whether the pointer currently sits inside the sidebar divider's grab
    /// band. Pure client presentation state: it only decides how the divider is
    /// drawn, is never persisted, and never leaves the TUI.
    pub(crate) sidebar_divider_hover: bool,
    /// Whether a live divider drag is currently being held at the card/line
    /// shell boundary rather than tracking the pointer. Pure client
    /// presentation state, like the hover beside it: it only decides how the
    /// divider is drawn, is never persisted, and never leaves the TUI.
    pub(crate) sidebar_divider_detent: bool,
    pub(crate) workspace_press: Option<WorkspacePressState>,
    pub(crate) tab_press: Option<TabPressState>,
    pub selection: Option<Selection>,
    pub selection_autoscroll: Option<SelectionAutoscroll>,
    pub context_menu: Option<ContextMenuState>,
    // Notifications
    pub update_available: Option<String>,
    pub update_install_command: String,
    pub latest_release_notes_available: bool,
    pub update_dismissed: bool,
    pub config_diagnostic: Option<String>,
    pub toast: Option<ToastNotification>,
    pub pending_agent_notifications: std::collections::HashMap<PaneId, PendingAgentNotification>,
    pub copy_feedback: Option<CopyFeedback>,
    /// Last reported focus state for the outer terminal hosting herdr.
    /// None means unsupported or not yet reported, which preserves active-pane suppression.
    pub outer_terminal_focus: Option<bool>,
    // Config
    pub prefix_code: KeyCode,
    pub prefix_mods: KeyModifiers,
    pub default_sidebar_width: u16,
    pub sidebar_width: u16,
    pub sidebar_min_width: u16,
    pub sidebar_max_width: u16,
    pub mobile_width_threshold: u16,
    pub sidebar_width_source: SidebarWidthSource,
    pub sidebar_width_auto: bool,
    pub sidebar_collapsed: bool,
    pub sidebar_collapsed_mode: crate::config::SidebarCollapsedModeConfig,
    /// Order agent rows take within their owner in the sidebar tree.
    pub agent_panel_sort: AgentPanelSort,
    /// Every source that currently wants to own the built-in Agents view. It
    /// projects onto the mobile switcher and `agent view get`; the sidebar tree
    /// is deliberately never filtered by it, because it is the only place a
    /// mate or worker is drawn and a filter there could hide the whole fleet.
    pub agent_views: crate::agent_view::AgentViewSlots,
    /// One caller-supplied line of text about this session, published through
    /// `session.status.set` and shown on the sidebar's reserved header row.
    ///
    /// Herdr neither composes nor interprets it. The slot exists so an outside
    /// publisher - a timer, a plugin, a shell script - owns both the content
    /// and the refresh cadence; nothing here knows or cares whether it is a
    /// quota readout, a branch name, or a deploy banner. It is deliberately
    /// not persisted: a status restored from a session file would be a stale
    /// claim about the world, and the publisher republishes anyway.
    pub session_status: Option<String>,
    pub status_indicators: crate::config::StatusIndicatorStyle,
    pub sidebar_agents: crate::config::AgentsSidebarConfig,
    pub sidebar_spaces: crate::config::SpacesSidebarConfig,
    pub sidebar_animation: crate::config::SidebarAnimationConfig,
    /// Floor under every behaviour's frame interval when this app is being
    /// driven by a server with no local terminal, from
    /// `[advanced] headless_animation_interval_ms`.
    ///
    /// Held resolved rather than read from config at each pass so a reload is
    /// the only thing that can change it, and so the value the engine is
    /// actually running on is inspectable from state in a test.
    pub headless_animation_interval: std::time::Duration,
    pub sidebar_notifications: crate::config::SidebarNotificationsConfig,
    /// The notification tray at the foot of the panel.
    pub sidebar_signal_tray: crate::config::SidebarSignalTrayConfig,
    /// How the tree's pixel cards move.
    pub sidebar_cards: crate::config::SidebarCardsConfig,
    pub next_agent_state_change_seq: u64,
    /// Capture mouse input for Herdr's own mouse UI. When false, Herdr only
    /// captures mouse while the focused pane app requests mouse reporting.
    pub mouse_capture: bool,
    pub copy_on_select: bool,
    pub right_click_passthrough_modifiers: Option<KeyModifiers>,
    pub right_click_passthrough: Option<RightClickPassthroughGesture>,
    pub redraw_on_focus_gained: bool,
    pub mouse_scroll_lines: usize,
    pub confirm_close: bool,
    pub prompt_new_tab_name: bool,
    pub prompt_new_workspace_name: bool,
    pub pane_borders: bool,
    pub pane_scrollbars: bool,
    pub pane_gaps: bool,
    pub show_agent_labels_on_pane_borders: bool,
    pub hide_tab_bar_when_single_tab: bool,
    /// When to draw a rolled-up agent state dot on each tab label.
    pub show_tab_state_dots: crate::config::TabDecorationConfig,
    /// When to draw the tab's jump number next to custom tab titles.
    pub show_tab_numbers: crate::config::TabDecorationConfig,
    pub tab_bar_position: TabBarPositionConfig,
    pub pane_history_persistence: bool,
    /// Expose the focused pane's cursor anchor to the outer terminal even when
    /// the pane requested `?25l`. See `[experimental] reveal_hidden_cursor_for_cjk_ime`.
    pub reveal_hidden_cursor_for_cjk_ime: bool,
    /// Restrict cursor reveal to focused panes whose detected agent matches
    /// one of these. When false, apply to any focused pane.
    pub cjk_ime_agent_filter_configured: bool,
    pub cjk_ime_agents: Vec<crate::detect::Agent>,
    /// DECSCUSR shape parameter (1–6) for the IME anchor cursor.
    pub cjk_ime_cursor_shape: u8,
    /// While prefix mode is active, switch the macOS host input source to an
    /// ASCII-capable layout so prefix commands register as ASCII even when a
    /// CJK IME is active. macOS only; a no-op elsewhere. See
    /// `[experimental] switch_ascii_input_source_in_prefix`.
    pub switch_ascii_input_source_in_prefix: bool,
    pub kitty_graphics_enabled: bool,
    /// `[experimental] sidebar_particle_field`: draw the sidebar's ambient particle-field wash.
    /// Only read while `kitty_graphics_enabled` is also true.
    pub sidebar_particle_field_enabled: bool,
    /// `[experimental] sidebar_card_font`: an explicit face for the sidebar's
    /// pixel cards, for a machine whose fonts are not where the search looks.
    /// `None` means search.
    pub sidebar_card_font: Option<String>,
    /// `[experimental] sidebar_card_shapes`: draw each card as its own
    /// transparent shape at its own placement rather than as one opaque sheet
    /// spanning the tree. See the field's doc on `ExperimentalConfig`.
    pub sidebar_card_shapes: bool,
    pub default_shell: String,
    pub shell_mode: crate::config::ShellModeConfig,
    pub new_terminal_cwd: NewTerminalCwdConfig,
    pub pane_scrollback_limit_bytes: usize,
    #[allow(dead_code)] // kept for backward compat; palette.accent is the source of truth
    pub accent: Color,
    pub sound: SoundConfig,
    pub local_sound_playback: bool,
    pub toast_config: ToastConfig,
    pub keybinds: Keybinds,
    /// The clock the sidebar's elapsed-time tokens are rendered against.
    ///
    /// Render stays pure by taking its `now` from state rather than reading
    /// the system clock mid-draw, the same way animated elements take their
    /// position from `anim`. The runtime refreshes this every loop iteration;
    /// a test can set it and get a deterministic render.
    pub state_age_now: Instant,
    /// Fleet relation signals currently travelling a sidebar row.
    ///
    /// Deliberately absent from both `persist::SessionSnapshot` and the live
    /// handoff manifest: a signal is decoration over state that is already
    /// correct, so a client that attaches late, or a server that restarts, is
    /// right to show the settled row and nothing else.
    pub(crate) relation_signals: crate::app::relation_signal::RelationSignals,
    /// Live per-terminal work volume, sampled by Herdr's own loop.
    ///
    /// Deliberately absent from both `persist::SessionSnapshot` and the live
    /// handoff manifest, for the same reason `relation_signals` is: it is a
    /// pure function of output happening *now*, so a restored or handed-off
    /// server is right to start every pane at rest and re-derive the level from
    /// the next few samples rather than resurrect a rate that has stopped
    /// being true.
    pub(crate) pane_activity: crate::app::pane_activity::PaneActivityMap,
    /// Every visual element's place in its own lifecycle, and the named
    /// behaviours they can play.
    ///
    /// Presentation state: it decides how something is drawn, never what is
    /// true about the session, so it is deliberately absent from both
    /// `persist::SessionSnapshot` and the live handoff manifest. A client that
    /// attaches late is right to draw every element settled rather than replay
    /// arrivals it was not there for.
    pub(crate) anim: crate::anim::Animator,
    /// What state each drawn card was last seen in, so a change can be told
    /// from a state. Presentation state for the same reason [`Self::anim`] is.
    pub(crate) sidebar_card_washes: crate::app::card_wash::CardWashes,
    /// Which command lines each drawn card has already acknowledged, and which
    /// of those acknowledgements are still live. Presentation state for the
    /// same reason [`Self::anim`] is.
    pub(crate) sidebar_cmd_acks: crate::app::cmd_ack::CmdAcks,
    /// The sidebar tree's owned agent rows as of the last loop pass.
    ///
    /// The tree is derived from panes that exist, so a pane closing takes the
    /// only copy of its row with it and there is nothing left to animate a
    /// departure *from*. This is that missing frame, and nothing more: the
    /// runtime republishes it every pass, and
    /// [`crate::ui::sidebar::sidebar_agent_entries`] re-inserts a remembered
    /// row only while the engine still has an exit to play for it.
    ///
    /// Presentation state, for the same reason `anim` is, and empty unless a
    /// row exit is configured — so an unconfigured Herdr pays nothing and a
    /// restored or handed-off server starts with every group settled.
    pub(crate) sidebar_tree_row_memory: Vec<crate::ui::AgentPanelEntry>,
    /// Which node the sidebar tree is currently rooted on.
    ///
    /// Client presentation state, like [`Self::anim`]: it decides what this
    /// viewer is looking at, never what is true about the session, so it is
    /// neither persisted nor published. Two clients on one session are entitled
    /// to be looking at different mates.
    pub(crate) tree_root: crate::app::tree_view::TreeRoot,
    /// A root that has been chosen but is not being shown yet, because the view
    /// it replaces is still coming apart. See [`crate::app::tree_view`].
    pub(crate) pending_tree_root: Option<crate::app::tree_view::PendingTreeRoot>,
    /// UI color palette — all sidebar/UI colors centralized for theming.
    pub palette: Palette,
    /// [`Self::palette`] as the desktop sidebar panel draws it.
    ///
    /// Derived from `palette` and the measured host theme by
    /// [`Self::refresh_sidebar_palette`], which `compute_view` runs once a
    /// frame, so a theme change, a config reload or a host appearance report
    /// all reach the panel through the same repaint that already follows them.
    /// Stored rather than computed per read because the sidebar asks for it
    /// once per row and once per fleet-signal slot, and a panel with a fill of
    /// its own re-floors four tokens on every one of those.
    pub sidebar_palette: Palette,
    /// Currently applied theme name (for settings UI).
    pub theme_name: String,
    /// Runtime theme configuration used to resolve manual and auto-switch palettes.
    pub theme_runtime: ThemeRuntimeConfig,
    /// Last known foreground host terminal appearance.
    pub host_terminal_appearance: Option<HostAppearance>,
    /// True when the foreground host explicitly reported appearance via Mode 2031.
    pub host_terminal_appearance_explicit: bool,
    /// Settings panel state.
    pub settings: SettingsState,
    /// Cached integration recommendations for onboarding/settings UI.
    pub integration_recommendations: Vec<crate::integration::IntegrationRecommendation>,
    /// Cached detection manifest source/version summaries for runtime/API status.
    pub agent_manifest_summaries: Vec<crate::detect::manifest::AgentManifestSummary>,
    /// Cached remote detection manifest update diagnostics for runtime/API status.
    pub agent_manifest_update_status: crate::detect::manifest_update::ManifestUpdateStatus,
    /// Result messages from the latest integration install action.
    pub integration_install_messages: Vec<String>,
    /// Installed or linked plugins known to this running Herdr instance.
    pub(crate) installed_plugins: InstalledPluginRegistry,
    /// Pane ids opened through the plugin pane API.
    pub(crate) plugin_panes: std::collections::HashMap<PaneId, PluginPaneRecord>,
    /// The notification tray's memory: escalation history and the question a
    /// blocked pane is showing. See [`crate::app::signal_tray`].
    pub(crate) signal_tray: crate::app::signal_tray::SignalTrayState,
    /// The tray's badge artwork, rasterised by the app loop and composited over
    /// the sidebar. `None` whenever the host cannot show images, the tray is
    /// off, or the panel is too small to hold it.
    pub(crate) signal_tray_graphics: Option<GraphicsLayer>,
    /// What the artwork above was drawn for: the eight states, the grid rect and
    /// the cell size, folded into one number. Rasterising eight badges is not
    /// free, so it is redone when this moves and not once per frame.
    pub(crate) signal_tray_graphics_key: u64,
    /// The sidebar's ambient particle-field wash, its loop frames included via
    /// [`GraphicsLayer::animation`]. `None` when disabled, not yet generated, or the sidebar
    /// column has no area.
    pub(crate) sidebar_particle_field: Option<GraphicsLayer>,
    /// What the wash above was generated for: its pixel dimensions, folded into one number.
    /// Generating a whole loop is not free, so it is redone only when this moves — a resize —
    /// and never once per tick.
    pub(crate) sidebar_particle_field_key: u64,
    /// Runtime image layers owned by API clients and composited over panes.
    pub(crate) pane_graphics_layers: std::collections::HashMap<PaneId, GraphicsLayer>,
    /// The same layers, anchored to a named non-pane region of the client
    /// viewport instead of to a pane rect.
    pub(crate) surface_graphics_layers:
        std::collections::HashMap<crate::api::schema::GraphicsSurface, GraphicsLayer>,
    /// The sidebar tree's cards, rasterised.
    ///
    /// Client presentation state, not a runtime fact: it is a picture of rows
    /// this client is drawing, at this client's cell size, and it never leaves
    /// the TUI. It rides the same placement pipeline as an API-owned layer but
    /// deliberately does not live in `surface_graphics_layers`, so a client
    /// putting a backdrop on the sidebar and the sidebar drawing its own cards
    /// are two placements rather than one overwriting the other.
    ///
    /// A list rather than one layer because a card is its own object: under
    /// `[experimental] sidebar_card_shapes` each card is a separate transparent
    /// image at its own placement, so moving, fading or reflowing one card
    /// touches one entry and leaves the rest alone. The sheet path puts exactly
    /// one entry here, so both paths are the same shape downstream.
    pub(crate) sidebar_card_layers: Vec<crate::ui::sidebar::SidebarCardLayer>,
    /// Active streaming graphics owner token by pane id.
    pub(crate) pane_graphics_streams: std::collections::HashMap<PaneId, String>,
    /// Monotonic marker for accepted pane graphics mutations.
    pub(crate) pane_graphics_revision: u64,
    /// Session-modal terminal popup. This is intentionally outside workspace layouts.
    pub(crate) popup_pane: Option<PopupPaneState>,
    /// Recent plugin action/event command executions.
    pub(crate) plugin_command_logs: Vec<crate::api::schema::PluginCommandLogInfo>,
    pub(crate) next_plugin_command_log_id: u64,
    pub(crate) plugin_commands_in_flight: usize,
    /// Highlight state for the bottom-right global launcher menu.
    pub global_menu: MenuListState,
    /// Resolved host terminal default colors for theming embedded panes.
    pub host_terminal_theme: TerminalTheme,
    /// Last known foreground host terminal cell size in pixels.
    pub(crate) host_cell_size: crate::kitty_graphics::HostCellSize,
    /// Set when a persisted session snapshot would change.
    pub session_dirty: bool,
    /// Terminal runtimes that should be shut down by the app/runtime layer
    /// after state has detached their terminal metadata.
    pub(crate) terminal_runtime_shutdowns: Vec<crate::terminal::TerminalId>,
}

/// The workspaces one worktree space key contributes as a parent/child group.
///
/// See [`AppState::worktree_space_group`] for how it is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSpaceGroup {
    /// The repo's main (non-linked) checkout workspace: the group's parent.
    pub parent_idx: usize,
    /// Linked-worktree members, in workspace order.
    pub children: Vec<usize>,
}

impl WorktreeSpaceGroup {
    pub fn contains(&self, ws_idx: usize) -> bool {
        self.parent_idx == ws_idx || self.children.contains(&ws_idx)
    }

    /// Parent first, then the linked worktrees in workspace order.
    pub fn indices(&self) -> Vec<usize> {
        let mut indices = Vec::with_capacity(self.children.len() + 1);
        indices.push(self.parent_idx);
        indices.extend(self.children.iter().copied());
        indices
    }
}

impl AppState {
    /// Resolve the parent/child group for one worktree space key.
    ///
    /// A worktree group means exactly one thing: the parent is the repo's main
    /// (non-linked) checkout and every child is a linked worktree of it. Two
    /// rules keep that promise honest when a repo has more than one workspace
    /// open in the same main checkout:
    ///
    /// * Only linked worktrees are ever children. A second workspace opened in
    ///   the main checkout is a peer, not a child, so it never joins the group
    ///   and never renders as a worktree of its own sibling.
    /// * A group only forms when it has at least one linked worktree. A key
    ///   whose members are all non-linked has no parentage to express.
    ///
    /// The parent is the first non-linked member in workspace order. Workspace
    /// order is the user-controlled sidebar order, so the choice is
    /// deterministic rather than dependent on map iteration order; and because
    /// every non-linked member of a key describes the same main checkout, they
    /// all name the same parent row.
    pub(crate) fn worktree_space_group(&self, key: &str) -> Option<WorktreeSpaceGroup> {
        let mut parent_idx = None;
        let mut children = Vec::new();
        for (ws_idx, space) in self
            .workspaces
            .iter()
            .enumerate()
            .filter_map(|(ws_idx, ws)| ws.worktree_space().map(|space| (ws_idx, space)))
            .filter(|(_, space)| space.key == key)
        {
            if space.is_linked_worktree {
                children.push(ws_idx);
            } else if parent_idx.is_none() {
                parent_idx = Some(ws_idx);
            }
        }
        let parent_idx = parent_idx?;
        (!children.is_empty()).then_some(WorktreeSpaceGroup {
            parent_idx,
            children,
        })
    }

    /// The group `ws_idx` belongs to, or `None` when it is a standalone row.
    pub(crate) fn worktree_space_group_of(&self, ws_idx: usize) -> Option<WorktreeSpaceGroup> {
        let key = &self.workspaces.get(ws_idx)?.worktree_space()?.key;
        self.worktree_space_group(key)
            .filter(|group| group.contains(ws_idx))
    }

    pub(crate) fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    /// True when a visible sidebar element opts into animation.
    ///
    /// Drives whether the engine tracks sidebar rows at all. The collapsed
    /// sidebar renders its own compact layout without configured token rows, so
    /// a collapsed sidebar never animates.
    pub(crate) fn sidebar_animation_active(&self) -> bool {
        let moves = self.sidebar_rows_move();
        !self.sidebar_collapsed
            && (self.sidebar_agents.has_animated_tokens()
                || self.sidebar_spaces.has_animated_tokens()
                || self.sidebar_animation.row_enter_stage(moves).is_some()
                || self.sidebar_animation.row_exit_stage(moves).is_some()
                || self.sidebar_card_animation_active())
    }

    /// True when the tree's cards are moving.
    ///
    /// Gated on the pixel path and not only on the config, the same way
    /// [`Self::signal_tray_animation_active`] is and for the same reason: a
    /// card's breath and its wash *are* its artwork — the card is rasterised at
    /// a different light — so on a host drawing character rows there is nothing
    /// for either to happen to. Publishing elements nothing could show would arm
    /// a deadline for a frame no one sees.
    ///
    /// The panel's width is deliberately not part of it, for the reason
    /// [`Self::sidebar_rows_move`] gives about lifecycles: the sidebar can be
    /// dragged past the card threshold while a wash is mid-sweep, and a
    /// width-dependent gate would change that sweep's life underneath it. Width
    /// is handled on the drawing side, where it belongs.
    pub(crate) fn sidebar_card_animation_active(&self) -> bool {
        !self.sidebar_collapsed
            && self.sidebar_cards.animates()
            && self.sidebar_card_shapes
            && self.kitty_graphics_enabled
            && self.host_cell_size.is_known()
            && crate::ui::sidebar::image_card::card_face_available(
                self.sidebar_card_font.as_deref(),
            )
    }

    /// True when a sidebar row's arrival and departure move it *on this host*.
    ///
    /// The config setting is only half the answer. A slide is an offset applied
    /// to where an already-rasterised pixel card is placed, so it exists only
    /// where those cards do: with `[experimental] kitty_graphics` off, or
    /// `sidebar_card_shapes` off — where the tree is one opaque sheet with no
    /// single row in it to move — there is nothing a row could travel through.
    /// Asking the config alone is what once synthesized an exit phase on a
    /// character-only host, which left a closed pane's row drawn for the whole
    /// of `row_exit_ms` with nothing playing on it.
    ///
    /// The panel's *width* is deliberately not part of this even though
    /// `image_card::is_available` gates on it too. A lifecycle is what an
    /// element's whole life is built from, and the sidebar can be dragged past
    /// the card threshold while a row is mid-flight — a width-dependent
    /// lifecycle would change that life underneath it. Width is handled where
    /// it belongs, on the drawing side.
    ///
    /// The host's *cell size* is deliberately not part of it either, and for a
    /// different reason: it is a per-client report, while this lifecycle is
    /// shared [`AppState`] read by every attached client. Folding it in would
    /// let one client's cell size decide another client's row lives, which is
    /// the class of defect this area has already produced three times — code
    /// deciding what to draw from a different client's view.
    ///
    /// That leaves one reachable residual, recorded here rather than left to be
    /// rediscovered: a server with both experimental flags on, attached by a
    /// client whose own config has graphics off, reports no cell size. No card
    /// is drawn for that client, but this still answers `true`, so a closed
    /// pane's row lingers for `row_exit_ms` with nothing playing on it. Closing
    /// it means giving a row a per-client lifecycle, which is a larger change
    /// than the defect is worth — do not "fix" it by reading per-client state
    /// from here.
    pub(crate) fn sidebar_rows_move(&self) -> bool {
        self.rows_move_given_face(crate::ui::sidebar::image_card::card_face_available(
            self.sidebar_card_font.as_deref(),
        ))
    }

    /// [`Self::sidebar_rows_move`] with the face condition passed in.
    ///
    /// Split out only so the gate itself is testable: the face is resolved
    /// through a process-lifetime `OnceLock`, so a test cannot make the machine
    /// it is running on stop having one. What is worth pinning is that *all
    /// four* terms are required, and that is what this exposes.
    fn rows_move_given_face(&self, face_available: bool) -> bool {
        self.sidebar_animation.rows_move()
            && self.sidebar_card_shapes
            && self.kitty_graphics_enabled
            && face_available
    }

    /// The life a sidebar row is given when it arrives.
    ///
    /// The arrival is the row's own; every steady behaviour comes from the
    /// tokens configured on it, and the render pass names the one it wants
    /// through [`crate::anim::Animator::frame`]'s override. Declaring them all
    /// here is what lets one row carry several tokens animating differently —
    /// each keeps its own period and its own live rate — while still sharing
    /// one arrival.
    pub(crate) fn sidebar_row_lifecycle(&self) -> crate::anim::Lifecycle {
        let moves = self.sidebar_rows_move();
        let mut lifecycle = crate::anim::Lifecycle::still();
        lifecycle.mount = self.sidebar_animation.row_enter_stage(moves);
        lifecycle.dismount = self.sidebar_animation.row_exit_stage(moves);
        for behaviour in self
            .sidebar_agents
            .animated_behaviours()
            .into_iter()
            .chain(self.sidebar_spaces.animated_behaviours())
            // The card's own breath rides the row's element rather than one of
            // its own: a card *is* the row, so there is no second membership to
            // reconcile and no way for the two to disagree about when a card
            // exists. Both breaths are declared whichever one the card's state
            // wants today, for the reason `Lifecycle::idle` gives.
            .chain(
                self.sidebar_card_animation_active()
                    .then(|| self.sidebar_cards.pulse_behaviours().iter().copied())
                    .into_iter()
                    .flatten(),
            )
        {
            lifecycle = lifecycle.with_idle(behaviour);
        }
        lifecycle
    }

    /// The life a trunk segment is given when its gap opens.
    ///
    /// Reads the same `[ui.sidebar.animation]` `row_enter`/`row_exit` config a
    /// row's own arrival does — a segment extending or retracting is the same
    /// event as the row it is attached to arriving or leaving, so one config
    /// pair serves both rather than inventing a second. `moves` is
    /// deliberately not threaded through the way [`Self::sidebar_row_lifecycle`]
    /// threads it: `row_motion` slides a card's *position*, and a trunk
    /// segment has none to slide, so it is not given a synthesized phase for a
    /// feature it takes no part in. No idle behaviours are declared — a
    /// segment's steady state is unanimated until something is asked to
    /// travel it, which is a later piece of work, not this one.
    pub(crate) fn sidebar_trunk_lifecycle(&self) -> crate::anim::Lifecycle {
        let mut lifecycle = crate::anim::Lifecycle::still();
        lifecycle.mount = self.sidebar_animation.row_enter_stage(false);
        lifecycle.dismount = self.sidebar_animation.row_exit_stage(false);
        lifecycle
    }

    /// True when the fleet signal bar is drawn.
    ///
    /// The bar lives on the reserved header row of the expanded panel, so a
    /// collapsed sidebar has nowhere to draw it and asks nothing of it.
    pub(crate) fn fleet_signal_bar_active(&self) -> bool {
        !self.sidebar_collapsed && self.sidebar_notifications.enabled
    }

    /// True when a drawn signal bar has something moving in it.
    pub(crate) fn fleet_signal_animation_active(&self) -> bool {
        self.fleet_signal_bar_active() && self.sidebar_notifications.animates()
    }

    /// True when the tray's badges are moving.
    ///
    /// Gated on the graphics path as well as on the two config switches,
    /// because the motion *is* the artwork: a badge moves by being rasterised
    /// at a different offset and glow, and a host with no graphics is drawing
    /// the fallback marks, which cannot move. Publishing eight elements that
    /// nothing could show would arm a deadline for a frame no one sees — the
    /// same trap `sidebar_rows_move` documents for row slides.
    pub(crate) fn signal_tray_animation_active(&self) -> bool {
        crate::ui::signal_tray_active(self)
            && self.sidebar_signal_tray.animate
            && self.kitty_graphics_enabled
            && self.host_cell_size.is_known()
    }

    /// True while a view switch still has something to play or to commit.
    ///
    /// The loop consults this before forgetting every element: a switch that
    /// is mid-dissolve, or one whose new root is still waiting to be adopted,
    /// must survive a pass in which nothing else in the panel is animating —
    /// otherwise it strands with the outgoing view half gone.
    pub(crate) fn tree_view_switch_active(&self) -> bool {
        self.pending_tree_root.is_some()
            || self
                .anim
                .frame(&crate::app::tree_view::view_element(), None)
                .is_some_and(|frame| frame.behaviour.is_some())
    }
    /// Root the sidebar tree on `root`, taking the current view apart first.
    ///
    /// The new root is not adopted here. It is held until the outgoing view has
    /// finished dematerializing, because that is the one instant at which the
    /// layout can change without a single row appearing to move — the whole
    /// point of carrying the switch by materialize/dematerialize rather than by
    /// reflow. With no transition configured the swap is immediate, which is
    /// the honest rendering of "this Herdr does not animate".
    ///
    /// Returns whether anything changed, so a caller can ask for a repaint.
    pub(crate) fn select_tree_root(
        &mut self,
        root: crate::app::tree_view::TreeRoot,
        now: std::time::Instant,
    ) -> bool {
        if self.tree_root == root && self.pending_tree_root.is_none() {
            return false;
        }
        let lifecycle = self.tree_view_lifecycle();
        let Some(leave) = lifecycle.dismount.clone() else {
            self.pending_tree_root = None;
            self.tree_root = root;
            return true;
        };
        match self.pending_tree_root.as_mut() {
            // Already mid-dissolve. Re-aiming it is not the same as restarting
            // it: the view being left is the same view either way, and
            // restarting would be the transition cancelling itself.
            Some(pending) => pending.root = root,
            None => {
                self.pending_tree_root = Some(crate::app::tree_view::PendingTreeRoot {
                    root,
                    commit_at: now + leave.duration,
                });
                // Brought into existence and asked to leave in the same pass, so
                // it enters its departure without a frame of the arrival it was
                // created holding. Nothing has advanced between the two calls,
                // so no renderer can see the phase it passed through.
                let id = crate::app::tree_view::view_element();
                self.anim.enter(
                    id.clone(),
                    &lifecycle,
                    crate::anim::behaviour::DriveInputs::default(),
                    now,
                );
                self.anim.leave(&id, now);
            }
        }
        true
    }

    /// The life a whole view is given: it forms on arrival, comes apart on
    /// departure, and does nothing at all in between.
    ///
    /// The same stage on both ends, because the engine plays a dismount as its
    /// mount reversed — the view comes apart exactly the way it formed, and the
    /// duration the loop waits before adopting the incoming root is by
    /// construction the duration the outgoing one spends leaving.
    fn tree_view_lifecycle(&self) -> crate::anim::Lifecycle {
        let stage = self.sidebar_animation.view_switch_stage();
        crate::app::tree_view::view_lifecycle(stage.clone(), stage)
    }

    /// When the app loop must wake to finish a view switch.
    ///
    /// A panel mid-dissolve with nothing else animating would otherwise park
    /// with the outgoing view half gone and the incoming one never drawn.
    pub(crate) fn next_tree_view_commit_deadline(&self) -> Option<std::time::Instant> {
        self.pending_tree_root
            .as_ref()
            .map(|pending| pending.commit_at)
    }

    /// Adopt a due root and start the incoming view materializing.
    ///
    /// Runs on the app loop, before the row membership for this pass is
    /// published, so the rows that arrive with the new view are published
    /// against the root they belong to rather than one pass late.
    ///
    /// Nothing happens outside a switch. A Herdr nobody is re-rooting holds no
    /// view element and arms no deadline for one, which is the same bargain
    /// every other animated element here makes.
    pub(crate) fn advance_tree_view(&mut self, now: std::time::Instant) -> bool {
        let Some(pending) = self.pending_tree_root.as_ref() else {
            return false;
        };
        // Mid-dissolve. Touching the element at all would resurrect it.
        if now < pending.commit_at {
            return false;
        }
        let root = pending.root.clone();
        self.pending_tree_root = None;
        self.tree_root = root;

        let lifecycle = self.tree_view_lifecycle();
        if lifecycle.mount.is_none() {
            return true;
        }
        // `enter` on an element that is still leaving restarts it from the
        // beginning rather than resuming, which is exactly right here: the view
        // really did go away, and what arrives now is a different one.
        self.anim.enter(
            crate::app::tree_view::view_element(),
            &lifecycle,
            crate::anim::behaviour::DriveInputs::default(),
            now,
        );
        true
    }

    /// How hard the busiest terminal in this workspace is working, in
    /// `0.0..=1.0`.
    ///
    /// The busiest rather than the mean: a row stands for whatever is happening
    /// under it, and averaging a working pane against three idle ones would
    /// report a quiet row for a workspace that is plainly busy.
    pub(crate) fn workspace_activity_level(&self, workspace: &Workspace) -> f32 {
        workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| self.terminal_activity_level(&pane.attached_terminal_id))
            .fold(0.0, f32::max)
    }

    /// True when a visible sidebar token draws an elapsed time.
    ///
    /// Same gate as [`Self::sidebar_animation_active`], and for the same
    /// reason: the collapsed sidebar draws its own compact layout with no
    /// configured token rows, so it can never show an age.
    pub(crate) fn sidebar_state_age_active(&self) -> bool {
        !self.sidebar_collapsed
            && (self.sidebar_agents.uses_state_age() || self.sidebar_spaces.uses_state_age())
    }

    /// When the sidebar's elapsed-time tokens would next draw different text.
    ///
    /// Not a fixed interval. The token's resolution coarsens as the state ages
    /// (seconds, then minutes, then hours), so this schedules the one wake-up
    /// that actually changes a character rather than a repaint every second
    /// forever. A fleet whose agents have all been in state for an hour costs
    /// one wake-up an hour; with no timestamps at all it costs nothing, because
    /// there is no deadline to arm.
    pub(crate) fn next_sidebar_state_age_tick(&self, now: Instant) -> Option<Instant> {
        if !self.sidebar_state_age_active() {
            return None;
        }
        self.terminals
            .values()
            .filter_map(|terminal| terminal.state_age(now))
            .map(|age| now + crate::state_age::next_change_after(age))
            .min()
    }

    /// How hard the terminal behind `pane_id` is working, in `0.0..=1.0`.
    ///
    /// The accessor a paint pass binds to. Reading it is pure: the level is
    /// sampled and smoothed by the loop, never here, so drawing can consult it
    /// as often as it likes without changing what anyone else sees. A pane with
    /// no runtime, no terminal, or no samples yet is `0.0` — quiet, not absent,
    /// so a caller never has to branch on "unknown".
    // Addressed by pane for callers that have a pane id rather than a terminal.
    // A Space row rolls up through `workspace_activity_level` above; a worker or
    // sub agent row is one pane and reads it here, so two rows under one second
    // mate do not breathe in lockstep because a third pane beside them is busy.
    pub(crate) fn pane_activity_level(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> f32 {
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .map_or(0.0, |pane| {
                self.pane_activity.level(&pane.attached_terminal_id)
            })
    }

    /// The same level, addressed by the terminal that produces the output.
    ///
    /// Prefer this where a terminal id is already in hand: the terminal is what
    /// the sampler actually keys on, so this skips a workspace lookup that can
    /// only fail.
    pub(crate) fn terminal_activity_level(&self, terminal_id: &crate::terminal::TerminalId) -> f32 {
        self.pane_activity.level(terminal_id)
    }

    /// True when at least one live relation signal is travelling a row the
    /// sidebar actually laid out.
    ///
    /// This is the damage test. `view.workspace_card_areas` is the authority on
    /// what was laid out for the current frame: it is empty for a collapsed
    /// sidebar and for the mobile layout, and it omits rows scrolled past the
    /// end of the list or hidden inside a collapsed group. A signal aimed
    /// anywhere else damages nothing, so the loop is never woken to repaint for
    /// it — while the signal itself still expires on schedule, because expiry
    /// does not go through here. Checked against both carriers a card can be:
    /// a Space's own row, or — the row this feature exists for — a worker's
    /// pane row nested under one.
    pub(crate) fn relation_signal_damage(&self) -> bool {
        use crate::app::relation_signal::CarrierId;

        self.relation_signals.iter().any(|signal| {
            self.view.workspace_card_areas.iter().any(|card| {
                match (&card.agent, signal.carrier_id()) {
                    (Some(agent), CarrierId::Pane(pane_id)) => self
                        .workspaces
                        .get(card.ws_idx)
                        .and_then(|workspace| workspace.public_pane_number(agent.pane_id))
                        .is_some_and(|number| {
                            *pane_id
                                == crate::workspace::public_pane_id_for_number(
                                    &self.workspaces[card.ws_idx].id,
                                    number,
                                )
                        }),
                    (None, CarrierId::Workspace(workspace_id)) => self
                        .workspaces
                        .get(card.ws_idx)
                        .is_some_and(|workspace| workspace.id == *workspace_id),
                    _ => false,
                }
            })
        })
    }

    /// Where a relation signal has reached on this workspace's row, if one is
    /// travelling it.
    pub(crate) fn workspace_relation_signal_phase(
        &self,
        ws_idx: usize,
    ) -> Option<crate::app::relation_signal::RelationSignalPhase> {
        let workspace = self.workspaces.get(ws_idx)?;
        self.relation_signals.phase_for_workspace(&workspace.id)
    }

    /// Where a relation signal has reached on this pane's row, if one is
    /// travelling it.
    ///
    /// A worker's row is a pane, not a workspace, so this is the lookup that
    /// lets a mate→worker connector carry a signal at all — see
    /// [`crate::app::relation_signal::RelationSignals::phase_for_pane`].
    pub(crate) fn pane_relation_signal_phase(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::app::relation_signal::RelationSignalPhase> {
        let workspace = self.workspaces.get(ws_idx)?;
        let number = workspace.public_pane_number(pane_id)?;
        let public_pane_id = crate::workspace::public_pane_id_for_number(&workspace.id, number);
        self.relation_signals.phase_for_pane(&public_pane_id)
    }

    pub(crate) fn remove_alias_shadowed_by_new_pane(&mut self, pane_id: PaneId) {
        self.pane_id_aliases.remove(&pane_id.raw());
    }

    pub fn sound_enabled(&self) -> bool {
        self.sound.enabled
    }

    pub fn toast_delivery(&self) -> ToastDelivery {
        self.toast_config.delivery
    }

    pub fn agent_border_labels_enabled(&self) -> bool {
        self.show_agent_labels_on_pane_borders
    }

    pub(crate) fn pane_exposes_host_cursor(
        &self,
        _ws_idx: usize,
        _pane_id: crate::layout::PaneId,
    ) -> bool {
        true
    }

    pub(crate) fn integration_updates_available(&self) -> bool {
        self.integration_recommendations
            .iter()
            .any(|item| item.state == crate::integration::IntegrationStatusKind::Outdated)
    }

    pub(crate) fn refresh_agent_manifest_summaries(&mut self) {
        self.agent_manifest_summaries = crate::detect::manifest::manifest_summaries();
    }

    pub(crate) fn global_menu_attention_badge_visible(&self) -> bool {
        self.update_available.is_some() || self.integration_updates_available()
    }

    pub(crate) fn global_menu_item_has_badge(&self, item: &str) -> bool {
        (item == "update ready" && self.update_available.is_some())
            || (item == "settings" && self.integration_updates_available())
    }

    pub(crate) fn settings_section_has_badge(&self, section: SettingsSection) -> bool {
        section == SettingsSection::Integrations && self.integration_updates_available()
    }

    pub(crate) fn focused_pane_requests_mouse_capture_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        self.mode == Mode::Terminal
            && self
                .active
                .and_then(|idx| self.focused_runtime_in_workspace(terminal_runtimes, idx))
                .and_then(crate::terminal::TerminalRuntime::input_state)
                .is_some_and(crate::pane::InputState::mouse_reporting_enabled)
    }

    pub(crate) fn should_capture_host_mouse_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        self.mouse_capture
            || self.popup_pane.is_some()
            || self.focused_pane_requests_mouse_capture_from(terminal_runtimes)
    }

    pub fn is_prefix_key(&self, key: &crate::input::TerminalKey) -> bool {
        crate::config::terminal_key_matches_combo(key, (self.prefix_code, self.prefix_mods))
    }

    pub fn estimate_pane_size(&self) -> (u16, u16) {
        if let Some(info) = self.view.pane_infos.first() {
            (info.rect.height, info.rect.width)
        } else {
            (24, 80)
        }
    }

    /// Returns true when the given (workspace, tab, pane) refers to the
    /// currently focused pane in the active workspace's active tab.
    pub(crate) fn runtime_for_pane_in_workspace<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        #[cfg(test)]
        if let Some(runtime) = self.workspaces.get(ws_idx)?.test_runtimes.get(&pane_id) {
            return Some(runtime);
        }
        #[cfg(test)]
        if let Some(runtime) = self
            .workspaces
            .get(ws_idx)?
            .tabs
            .iter()
            .find_map(|tab| tab.runtimes.get(&pane_id))
        {
            return Some(runtime);
        }
        let terminal_id = self.workspaces.get(ws_idx)?.terminal_id(pane_id)?;
        terminal_runtimes.get(terminal_id)
    }

    #[cfg(test)]
    pub(crate) fn runtime_for_pane<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        self.workspaces.iter().find_map(|ws| {
            #[cfg(test)]
            if let Some(runtime) = ws.test_runtimes.get(&pane_id) {
                return Some(runtime);
            }
            #[cfg(test)]
            if let Some(runtime) = ws.tabs.iter().find_map(|tab| tab.runtimes.get(&pane_id)) {
                return Some(runtime);
            }
            let terminal_id = ws.terminal_id(pane_id)?;
            terminal_runtimes.get(terminal_id)
        })
    }

    pub(crate) fn focused_runtime_in_workspace<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        ws_idx: usize,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        let ws = self.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
    }

    pub fn is_active_pane(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if ws_idx != active_ws_idx {
            return false;
        }
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return false;
        };
        if tab_idx != ws.active_tab_index() {
            return false;
        }
        ws.active_tab().map(|tab| tab.layout.focused()) == Some(pane_id)
    }

    /// Re-derive [`Self::sidebar_palette`] from the palette and host theme it
    /// is a function of.
    ///
    /// Called from `compute_view`, which every render path runs first, so the
    /// two can only be out of step for a state that was never laid out. It is
    /// the palette itself whenever the panel has no fill of its own, which is
    /// the default.
    pub fn refresh_sidebar_palette(&mut self) {
        self.sidebar_palette = self.palette.for_sidebar(&self.host_terminal_theme);
    }
}

#[cfg(test)]
pub fn key_matches(
    key: &crossterm::event::KeyEvent,
    expected_code: KeyCode,
    expected_mods: KeyModifiers,
) -> bool {
    crate::config::terminal_key_matches_combo(
        &crate::input::TerminalKey::from(*key),
        (expected_code, expected_mods),
    )
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
impl AppState {
    /// Create an AppState for testing — no channels, no PTYs.
    pub fn test_new() -> Self {
        Self {
            terminals: std::collections::HashMap::new(),
            direct_attach_resize_locks: std::collections::HashSet::new(),
            pane_id_aliases: std::collections::HashMap::new(),
            public_pane_id_aliases: std::collections::HashMap::new(),
            workspaces: Vec::new(),
            active: None,
            previous_pane_focus: None,
            selected: 0,
            mode: Mode::Navigate,
            should_quit: false,
            detach_exits: false,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_new_linked_worktree: None,
            request_open_existing_worktree: None,
            request_new_workspace_cwd: None,
            request_remove_linked_worktree: None,
            request_submit_worktree_create: false,
            request_submit_worktree_open: false,
            request_submit_worktree_remove: false,
            request_reload_config: false,
            request_client_config_reload: false,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            pending_workspace_create_cwd: None,
            rename_pane_target: None,
            worktree_create: None,
            worktree_open: None,
            worktree_remove: None,
            worker_summaries: None,
            worktree_directory: std::path::PathBuf::from("/tmp/herdr-worktrees"),
            collapsed_space_keys: std::collections::HashSet::new(),
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            release_notes: None,
            product_announcement: None,
            keybind_help: KeybindHelpState::default(),
            navigator: NavigatorState::default(),
            copy_mode: None,
            workspace_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            mobile_switcher_scroll: 0,
            view: ViewState {
                layout: ViewLayout::Desktop,
                sidebar_rect: Rect::default(),
                workspace_card_areas: Vec::new(),
                sidebar_card_layers_published: false,
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                mobile_header_rect: Rect::default(),
                mobile_menu_hit_area: Rect::default(),
                toast_hit_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            sidebar_divider_hover: false,
            sidebar_divider_detent: false,
            workspace_press: None,
            tab_press: None,
            selection: None,
            selection_autoscroll: None,
            context_menu: None,
            update_available: None,
            update_install_command: "herdr update".into(),
            latest_release_notes_available: false,
            update_dismissed: false,
            config_diagnostic: None,
            toast: None,
            pending_agent_notifications: std::collections::HashMap::new(),
            copy_feedback: None,
            outer_terminal_focus: None,
            prefix_code: KeyCode::Char('b'),
            prefix_mods: KeyModifiers::CONTROL,
            default_sidebar_width: 26,
            sidebar_width: 26,
            sidebar_min_width: crate::config::DEFAULT_SIDEBAR_BOUNDS.0,
            sidebar_max_width: crate::config::DEFAULT_SIDEBAR_BOUNDS.1,
            mobile_width_threshold: crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD,
            sidebar_width_source: SidebarWidthSource::ConfigDefault,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            sidebar_collapsed_mode: crate::config::SidebarCollapsedModeConfig::Compact,
            agent_panel_sort: AgentPanelSort::Spaces,
            agent_views: crate::agent_view::AgentViewSlots::default(),
            session_status: None,
            status_indicators: crate::config::StatusIndicatorStyle::Ascii,
            sidebar_agents: crate::config::AgentsSidebarConfig::default(),
            sidebar_spaces: crate::config::SpacesSidebarConfig::default(),
            sidebar_animation: crate::config::SidebarAnimationConfig::default(),
            headless_animation_interval: crate::config::Config::default()
                .advanced
                .headless_animation_interval(),
            sidebar_notifications: crate::config::SidebarNotificationsConfig::default(),
            sidebar_signal_tray: crate::config::SidebarSignalTrayConfig::default(),
            sidebar_cards: crate::config::SidebarCardsConfig::default(),
            next_agent_state_change_seq: 0,
            mouse_capture: true,
            copy_on_select: true,
            right_click_passthrough_modifiers: None,
            right_click_passthrough: None,
            redraw_on_focus_gained: true,
            mouse_scroll_lines: crate::config::DEFAULT_MOUSE_SCROLL_LINES,
            confirm_close: true,
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
            pane_borders: true,
            pane_scrollbars: true,
            pane_gaps: false,
            show_agent_labels_on_pane_borders: false,
            hide_tab_bar_when_single_tab: false,
            show_tab_state_dots: crate::config::TabDecorationConfig::default(),
            show_tab_numbers: crate::config::TabDecorationConfig::default(),
            tab_bar_position: TabBarPositionConfig::Top,
            pane_history_persistence: false,
            reveal_hidden_cursor_for_cjk_ime: false,
            cjk_ime_agent_filter_configured: false,
            cjk_ime_agents: Vec::new(),
            cjk_ime_cursor_shape: 2, // steady_block
            switch_ascii_input_source_in_prefix: false,
            kitty_graphics_enabled: false,
            sidebar_particle_field_enabled: false,
            sidebar_card_font: None,
            sidebar_card_shapes: false,
            default_shell: String::new(),
            shell_mode: crate::config::ShellModeConfig::Auto,
            new_terminal_cwd: NewTerminalCwdConfig::Follow,
            pane_scrollback_limit_bytes: crate::config::DEFAULT_SCROLLBACK_LIMIT_BYTES,
            accent: Color::Cyan,
            sound: SoundConfig {
                enabled: false,
                ..SoundConfig::default()
            },
            local_sound_playback: false,
            toast_config: ToastConfig::default(),
            keybinds: Keybinds::default(),
            state_age_now: Instant::now(),
            relation_signals: crate::app::relation_signal::RelationSignals::default(),
            pane_activity: crate::app::pane_activity::PaneActivityMap::default(),
            anim: crate::anim::Animator::default(),
            sidebar_card_washes: crate::app::card_wash::CardWashes::default(),
            sidebar_cmd_acks: crate::app::cmd_ack::CmdAcks::default(),
            sidebar_tree_row_memory: Vec::new(),
            tree_root: crate::app::tree_view::TreeRoot::default(),
            pending_tree_root: None,
            palette: Palette::catppuccin(),
            sidebar_palette: Palette::catppuccin(),
            theme_name: "catppuccin".to_string(),
            theme_runtime: ThemeRuntimeConfig {
                manual_name: "catppuccin".to_string(),
                dark_name: "catppuccin".to_string(),
                light_name: "catppuccin-latte".to_string(),
                auto_switch: false,
                custom: None,
                legacy_accent: None,
            },
            host_terminal_appearance: None,
            host_terminal_appearance_explicit: false,
            settings: SettingsState {
                section: SettingsSection::Theme,
                list: SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
            },
            integration_recommendations: Vec::new(),
            agent_manifest_summaries: Vec::new(),
            agent_manifest_update_status:
                crate::detect::manifest_update::ManifestUpdateStatus::default(),
            integration_install_messages: Vec::new(),
            installed_plugins: std::collections::HashMap::new(),
            plugin_panes: std::collections::HashMap::new(),
            signal_tray: crate::app::signal_tray::SignalTrayState::default(),
            signal_tray_graphics: None,
            signal_tray_graphics_key: 0,
            sidebar_particle_field: None,
            sidebar_particle_field_key: 0,
            pane_graphics_layers: std::collections::HashMap::new(),
            surface_graphics_layers: std::collections::HashMap::new(),
            sidebar_card_layers: Vec::new(),
            pane_graphics_streams: std::collections::HashMap::new(),
            pane_graphics_revision: 0,
            popup_pane: None,
            plugin_command_logs: Vec::new(),
            next_plugin_command_log_id: 1,
            plugin_commands_in_flight: 0,
            global_menu: MenuListState::new(0),
            host_terminal_theme: TerminalTheme::default(),
            host_cell_size: crate::kitty_graphics::HostCellSize::default(),
            session_dirty: false,
            terminal_runtime_shutdowns: Vec::new(),
        }
    }

    /// Populate missing `TerminalState` entries for every pane so tests that
    /// read or write terminal metadata don't need to manually create them.
    pub fn ensure_test_terminals(&mut self) {
        use crate::terminal::TerminalState;
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    if !self.terminals.contains_key(&pane.attached_terminal_id) {
                        let cwd = ws.identity_cwd.clone();
                        self.terminals.insert(
                            pane.attached_terminal_id.clone(),
                            TerminalState::new(pane.attached_terminal_id.clone(), cwd),
                        );
                    }
                }
            }
        }
    }

    pub fn test_with_adversarial_identity_state() -> Self {
        let mut state = Self::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_adversarial_identity_state()];
        state.active = Some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        state
    }

    pub fn assert_invariants_for_test(&self) {
        if self.workspaces.is_empty() {
            assert!(
                self.active.is_none(),
                "empty app state must not have active workspace {:?}",
                self.active
            );
            assert_eq!(
                self.selected, 0,
                "empty app state should keep selected workspace at 0"
            );
            assert!(
                self.pane_id_aliases.is_empty(),
                "empty app state must not keep raw pane aliases"
            );
            assert!(
                self.public_pane_id_aliases.is_empty(),
                "empty app state must not keep public pane aliases"
            );
            assert!(
                self.previous_pane_focus.is_none(),
                "empty app state must not keep previous pane focus"
            );
            assert!(
                self.plugin_panes.is_empty(),
                "empty app state must not keep plugin pane records"
            );
            assert!(
                self.pending_agent_notifications.is_empty(),
                "empty app state must not keep pending agent notifications"
            );
            assert!(
                self.copy_mode.is_none(),
                "empty app state must not keep copy mode"
            );
            assert!(
                self.rename_pane_target.is_none(),
                "empty app state must not keep rename pane target"
            );
            assert!(
                self.selection.is_none(),
                "empty app state must not keep text selection"
            );
            assert!(
                self.selection_autoscroll.is_none(),
                "empty app state must not keep selection autoscroll"
            );
            if let Some(toast) = &self.toast {
                assert!(
                    toast.target.is_none(),
                    "empty app state must not keep pane-targeted toast"
                );
            }
            assert!(
                self.right_click_passthrough.is_none(),
                "empty app state must not keep right-click passthrough gesture"
            );
            assert!(
                self.drag.is_none(),
                "empty app state must not keep drag state"
            );
            assert!(
                self.workspace_press.is_none(),
                "empty app state must not keep workspace press state"
            );
            assert!(
                self.tab_press.is_none(),
                "empty app state must not keep tab press state"
            );
            assert!(
                self.context_menu.is_none(),
                "empty app state must not keep context menu"
            );
            return;
        }

        assert!(
            self.selected < self.workspaces.len(),
            "selected workspace {} out of bounds for {} workspaces",
            self.selected,
            self.workspaces.len()
        );
        let active = self
            .active
            .expect("non-empty app state must have active workspace");
        assert!(
            active < self.workspaces.len(),
            "active workspace {} out of bounds for {} workspaces",
            active,
            self.workspaces.len()
        );

        let mut workspace_ids = std::collections::HashSet::new();
        let mut workspace_id_to_idx = std::collections::HashMap::new();
        let mut pane_ids = std::collections::HashSet::new();
        let mut attached_terminal_ids = std::collections::HashSet::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            assert!(
                workspace_ids.insert(ws.id.clone()),
                "duplicate workspace id {} at workspace index {}",
                ws.id,
                ws_idx
            );
            workspace_id_to_idx.insert(ws.id.clone(), ws_idx);
            ws.assert_invariants_for_test();

            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    assert!(
                        pane_ids.insert(*pane_id),
                        "pane {:?} appears in more than one workspace",
                        pane_id
                    );
                    assert!(
                        attached_terminal_ids.insert(pane.attached_terminal_id.clone()),
                        "terminal {} is attached to more than one app pane",
                        pane.attached_terminal_id
                    );
                    assert!(
                        self.terminals.contains_key(&pane.attached_terminal_id),
                        "pane {:?} is attached to missing terminal {}",
                        pane_id,
                        pane.attached_terminal_id
                    );
                }
            }
        }

        let assert_live_pane = |pane_id: PaneId, context: &str| {
            assert!(
                pane_ids.contains(&pane_id),
                "{context} references missing pane {:?}",
                pane_id
            );
        };
        let assert_workspace_pane = |workspace_id: &str, pane_id: PaneId, context: &str| {
            let ws_idx = workspace_id_to_idx
                .get(workspace_id)
                .copied()
                .unwrap_or_else(|| panic!("{context} references missing workspace {workspace_id}"));
            assert!(
                self.workspaces[ws_idx].pane_state(pane_id).is_some(),
                "{context} references pane {:?} outside workspace {}",
                pane_id,
                workspace_id
            );
        };
        let assert_workspace_index = |ws_idx: usize, context: &str| {
            assert!(
                ws_idx < self.workspaces.len(),
                "{context} references workspace index {} out of bounds for {} workspaces",
                ws_idx,
                self.workspaces.len()
            );
        };
        let assert_tab_index = |ws_idx: usize, tab_idx: usize, context: &str| {
            assert_workspace_index(ws_idx, context);
            assert!(
                tab_idx < self.workspaces[ws_idx].tabs.len(),
                "{context} references tab index {} out of bounds for workspace {} with {} tabs",
                tab_idx,
                ws_idx,
                self.workspaces[ws_idx].tabs.len()
            );
        };

        for (&raw, &pane_id) in &self.pane_id_aliases {
            assert_live_pane(pane_id, &format!("raw pane alias {raw}"));
        }
        for (public_id, &pane_id) in &self.public_pane_id_aliases {
            assert_live_pane(pane_id, &format!("public pane alias {public_id}"));
        }
        if let Some(focus) = &self.previous_pane_focus {
            assert_workspace_pane(&focus.workspace_id, focus.pane_id, "previous pane focus");
        }
        if let Some(toast) = &self.toast {
            if let Some(target) = &toast.target {
                assert_workspace_pane(&target.workspace_id, target.pane_id, "toast target");
            }
        }
        for (&pane_id, notification) in &self.pending_agent_notifications {
            assert_eq!(
                pane_id, notification.pane_id,
                "pending agent notification map key must match payload pane id"
            );
            assert_workspace_pane(
                &notification.workspace_id,
                notification.pane_id,
                "pending agent notification",
            );
        }
        if let Some(popup) = &self.popup_pane {
            assert!(
                self.terminals.contains_key(&popup.terminal_id),
                "popup {:?} references missing terminal {}",
                popup.pane_id,
                popup.terminal_id
            );
            assert!(
                !attached_terminal_ids.contains(&popup.terminal_id),
                "popup terminal {} must not be attached to a tiled pane",
                popup.terminal_id
            );
        }
        for &pane_id in self.plugin_panes.keys() {
            assert_live_pane(pane_id, "plugin pane record");
        }
        if let Some(copy_mode) = &self.copy_mode {
            assert_live_pane(copy_mode.pane_id, "copy mode");
        }
        if let Some(pane_id) = self.rename_pane_target {
            assert_live_pane(pane_id, "rename pane target");
        }
        if let Some(selection) = &self.selection {
            assert_live_pane(selection.pane_id, "text selection");
        } else {
            assert!(
                self.selection_autoscroll.is_none(),
                "selection autoscroll must not remain without an active text selection"
            );
        }
        if let Some(gesture) = &self.right_click_passthrough {
            assert_live_pane(gesture.pane_info.id, "right-click passthrough gesture");
        }
        if let Some(drag) = &self.drag {
            match &drag.target {
                DragTarget::WorkspaceReorder {
                    source_ws_idx,
                    drop_target,
                } => {
                    assert_workspace_index(*source_ws_idx, "workspace drag source");
                    if let Some(WorkspaceDropTarget::Before(ws_idx)) = drop_target {
                        assert_workspace_index(*ws_idx, "workspace drag target");
                    }
                }
                DragTarget::TabReorder {
                    ws_idx,
                    source_tab_idx,
                    insert_idx,
                } => {
                    assert_tab_index(*ws_idx, *source_tab_idx, "tab drag source");
                    if let Some(insert_idx) = insert_idx {
                        assert!(
                            *insert_idx <= self.workspaces[*ws_idx].tabs.len(),
                            "tab drag insert index {} out of bounds for workspace {} with {} tabs",
                            insert_idx,
                            ws_idx,
                            self.workspaces[*ws_idx].tabs.len()
                        );
                    }
                }
                DragTarget::PaneScrollbar { pane_id, .. } => {
                    assert_live_pane(*pane_id, "pane scrollbar drag")
                }
                _ => {}
            }
        }
        if let Some(press) = &self.workspace_press {
            assert_workspace_index(press.ws_idx, "workspace press");
        }
        if let Some(press) = &self.tab_press {
            assert_tab_index(press.ws_idx, press.tab_idx, "tab press");
        }
        if let Some(menu) = &self.context_menu {
            match menu.kind {
                ContextMenuKind::Workspace { ws_idx }
                | ContextMenuKind::GitWorkspace { ws_idx, .. } => {
                    assert_workspace_index(ws_idx, "context menu workspace")
                }
                ContextMenuKind::Tab { ws_idx, tab_idx } => {
                    assert_tab_index(ws_idx, tab_idx, "context menu tab")
                }
                ContextMenuKind::Pane {
                    ws_idx,
                    tab_idx,
                    pane_id,
                    source_pane_id,
                    ..
                } => {
                    assert_tab_index(ws_idx, tab_idx, "context menu pane tab");
                    assert!(
                        self.workspaces[ws_idx].tabs[tab_idx]
                            .panes
                            .contains_key(&pane_id),
                        "context menu pane references pane {:?} outside workspace {} tab {}",
                        pane_id,
                        ws_idx,
                        tab_idx
                    );
                    if let Some(source_pane_id) = source_pane_id {
                        assert_live_pane(source_pane_id, "context menu source pane");
                    }
                }
            }
        }
    }

    pub fn insert_test_runtime(
        &mut self,
        pane_id: crate::layout::PaneId,
        runtime: crate::terminal::TerminalRuntime,
    ) {
        if let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.terminal_id(pane_id).is_some())
        {
            ws.insert_test_runtime(pane_id, runtime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    /// Row motion needs all four of its conditions, not the config flag alone.
    ///
    /// The three host conditions are what stop a synthesized departure phase
    /// existing on a host that draws no pixel cards — which is what once left a
    /// closed pane's row on screen for the whole of `row_exit_ms` with nothing
    /// playing on it. The face is included here and asserted through
    /// [`AppState::rows_move_given_face`] rather than by removing the machine's
    /// fonts, because the face is resolved once per process behind a
    /// `OnceLock`: a test cannot make the machine it is running on stop having
    /// one, but it can pin that the answer is required.
    #[test]
    fn row_motion_needs_the_config_the_two_flags_and_a_face() {
        let mut app = AppState::test_new();
        app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::Slide;
        app.sidebar_card_shapes = true;
        app.kitty_graphics_enabled = true;
        assert!(
            app.rows_move_given_face(true),
            "all four present and it still refused"
        );

        assert!(
            !app.rows_move_given_face(false),
            "a host with no proportional face draws no card, so no row may be \
             given a phase to move through"
        );

        for drop in ["config", "shapes", "graphics"] {
            let mut app = AppState::test_new();
            app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::Slide;
            app.sidebar_card_shapes = true;
            app.kitty_graphics_enabled = true;
            match drop {
                "config" => {
                    app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::None
                }
                "shapes" => app.sidebar_card_shapes = false,
                _ => app.kitty_graphics_enabled = false,
            }
            assert!(
                !app.rows_move_given_face(true),
                "motion survived without {drop}"
            );
        }
    }

    /// And the gate is what the lifecycle reads, so a host that cannot move a
    /// row is given no phase to move it through.
    #[test]
    fn a_host_without_a_face_is_given_no_synthesized_departure() {
        let mut app = AppState::test_new();
        app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::Slide;
        app.sidebar_card_shapes = true;
        app.kitty_graphics_enabled = true;
        // Motion and nothing else, so the only life a row could have is the
        // one motion invents for it.
        app.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::None;
        app.sidebar_animation.row_exit = crate::config::SidebarTokenEmphasis::None;

        let moves = app.sidebar_rows_move();
        let lifecycle = app.sidebar_row_lifecycle();
        assert_eq!(
            lifecycle.dismount.is_some(),
            moves,
            "the synthesized departure and the motion gate have drifted apart, \
             which is what leaves a closed pane's row drawn for row_exit_ms"
        );
        assert_eq!(lifecycle.mount.is_some(), moves);
    }

    mod contrast_floor {
        use super::super::Palette;
        use crate::terminal_theme::{RgbColor, TerminalTheme};
        use crate::ui::color::{contrast_ratio, resolve_color_rgb};
        use ratatui::style::Color;

        fn host_background(r: u8, g: u8, b: u8) -> TerminalTheme {
            TerminalTheme::default().with_color(
                crate::terminal_theme::DefaultColorKind::Background,
                RgbColor { r, g, b },
            )
        }

        fn ratio_against(color: Color, host: &TerminalTheme, background: (u8, u8, u8)) -> f32 {
            let rgb = resolve_color_rgb(color, host).expect("token should resolve");
            contrast_ratio(rgb, background)
        }

        #[test]
        fn white_terminal_background_makes_the_terminal_theme_readable() {
            let background = (255, 255, 255);
            let host = host_background(background.0, background.1, background.2);
            let before = Palette::terminal();
            let after = before.clone().with_contrast_floor(&host);

            // Precondition: the authored palette really is unreadable here.
            assert!(ratio_against(before.overlay1, &host, background) < 4.5);
            assert!(ratio_against(before.overlay0, &host, background) < 3.0);

            assert!(ratio_against(after.overlay1, &host, background) >= 4.5);
            assert!(ratio_against(after.overlay0, &host, background) >= 3.0);
            assert!(ratio_against(after.surface_dim, &host, background) >= 1.5);
        }

        #[test]
        fn reset_tokens_keep_inheriting_the_host() {
            let host = host_background(255, 255, 255);
            let floored = Palette::terminal().with_contrast_floor(&host);
            // `terminal` deliberately paints no surface0 fill; the floor must
            // not turn that into an opaque background.
            assert_eq!(Palette::terminal().surface0, Color::Reset);
            assert_eq!(floored.surface0, Color::Reset);
        }

        #[test]
        fn an_unmeasured_host_leaves_the_palette_untouched() {
            let palette = Palette::catppuccin();
            assert_eq!(
                palette
                    .clone()
                    .with_contrast_floor(&TerminalTheme::default()),
                palette
            );
        }

        #[test]
        fn a_hand_tuned_theme_on_its_own_background_is_barely_touched() {
            // Catppuccin Mocha on a Mocha terminal. `surface0` and `overlay0`
            // already clear their floors untouched; `surface_dim` is literally
            // the base colour and so renders invisible, and `overlay1` sits a
            // hair under AA. Both get the smallest nudge that clears the floor
            // — a nudge, not a restyle.
            let before = Palette::catppuccin();
            let after = before
                .clone()
                .with_contrast_floor(&host_background(30, 30, 46));

            assert_eq!(after.surface0, before.surface0);
            assert_eq!(after.overlay0, before.overlay0);

            for (label, b, a) in [
                ("surface_dim", before.surface_dim, after.surface_dim),
                ("overlay1", before.overlay1, after.overlay1),
            ] {
                let unmeasured = TerminalTheme::default();
                let b = resolve_color_rgb(b, &unmeasured).expect("rgb");
                let a = resolve_color_rgb(a, &unmeasured).expect("rgb");
                let moved =
                    a.0.abs_diff(b.0)
                        .max(a.1.abs_diff(b.1))
                        .max(a.2.abs_diff(b.2));
                assert!(moved <= 12, "{label} moved {b:?} -> {a:?}");
            }
        }

        #[test]
        fn accents_are_never_floored() {
            let host = host_background(255, 255, 255);
            let before = Palette::terminal();
            let after = before.clone().with_contrast_floor(&host);
            assert_eq!(after.accent, before.accent);
            assert_eq!(after.green, before.green);
            assert_eq!(after.yellow, before.yellow);
            assert_eq!(after.red, before.red);
            assert_eq!(after.text, before.text);
            assert_eq!(after.subtext0, before.subtext0);
            assert_eq!(after.surface1, before.surface1);
        }

        /// A theme with a sidebar fill on a host whose background is nowhere
        /// near it — the case where the two surfaces straddle mid-grey and no
        /// one colour clears both floors.
        fn straddling_surfaces() -> (TerminalTheme, Palette) {
            let host = host_background(239, 241, 245);
            let custom = crate::config::CustomThemeColors {
                sidebar_bg: Some("#181825".to_string()),
                ..Default::default()
            };
            let palette = Palette::catppuccin_latte()
                .with_overrides(&custom)
                .with_contrast_floor(&host);
            (host, palette)
        }

        #[test]
        fn a_sidebar_fill_never_detunes_the_tokens_drawn_outside_the_sidebar() {
            let (host, palette) = straddling_surfaces();
            let without_fill = Palette::catppuccin_latte().with_contrast_floor(&host);

            // `overlay1` is the settings and modal description ink and
            // `overlay0` the navigator's secondary text, so a sidebar fill
            // reaching them is unreadable modal text, not a quieter sidebar.
            assert_eq!(palette.overlay1, without_fill.overlay1);
            assert_eq!(palette.overlay0, without_fill.overlay0);
            assert_eq!(palette.surface0, without_fill.surface0);
            assert_eq!(palette.surface_dim, without_fill.surface_dim);
        }

        #[test]
        fn every_floored_token_clears_its_floor_on_the_surface_it_is_drawn_on() {
            let (host, palette) = straddling_surfaces();
            let sidebar = palette.for_sidebar(&host);

            // Outside the sidebar the tokens land on the host background, and
            // a modal on this theme fills with `panel_bg`, which is the same
            // light colour — so the ink a modal is read in has to clear its
            // floor there and not merely against the dark panel.
            let host_bg = (239, 241, 245);
            let modal_bg = resolve_color_rgb(palette.panel_bg, &host).expect("modal fill");
            for background in [host_bg, modal_bg] {
                assert!(
                    ratio_against(palette.overlay1, &host, background) >= 4.5,
                    "overlay1 is unreadable on {background:?}"
                );
                assert!(ratio_against(palette.overlay0, &host, background) >= 3.0);
                assert!(ratio_against(palette.surface_dim, &host, background) >= 1.1);
                assert!(ratio_against(palette.surface0, &host, background) >= 1.1);
            }

            // Inside the panel they land on its own fill instead.
            let sidebar_bg = resolve_color_rgb(palette.sidebar_bg, &host).expect("panel fill");
            assert!(ratio_against(sidebar.overlay1, &host, sidebar_bg) >= 4.5);
            assert!(ratio_against(sidebar.overlay0, &host, sidebar_bg) >= 3.0);
            assert!(ratio_against(sidebar.surface_dim, &host, sidebar_bg) >= 1.1);
            assert!(ratio_against(sidebar.surface0, &host, sidebar_bg) >= 1.1);

            // The panel's floor is a second copy, never a second pass over the
            // shared one: this is the assertion that fails if the two are ever
            // folded back into one token.
            assert_ne!(sidebar.overlay1, palette.overlay1);
            assert_eq!(sidebar.accent, palette.accent);
        }

        #[test]
        fn a_panel_with_no_fill_of_its_own_draws_with_the_host_floored_palette() {
            let host = host_background(239, 241, 245);
            let palette = Palette::catppuccin_latte().with_contrast_floor(&host);
            assert_eq!(palette.sidebar_bg, Color::Reset);
            assert_eq!(palette.for_sidebar(&host), palette);
        }

        #[test]
        fn the_measured_palette_decides_whether_a_named_token_needs_lifting() {
            // A host whose "white" slot is a light grey on a white background:
            // the static table would call it compliant, the measurement does not.
            let background = (255, 255, 255);
            let host = host_background(background.0, background.1, background.2)
                .with_palette_color(
                    15,
                    RgbColor {
                        r: 224,
                        g: 224,
                        b: 224,
                    },
                );
            let floored = Palette::terminal().with_contrast_floor(&host);
            assert!(ratio_against(floored.overlay1, &host, background) >= 4.5);
        }
    }

    #[test]
    fn agent_terminal_keeps_final_child_cursor_exposed() {
        let mut state = AppState::test_new();
        let ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        state.terminals.insert(
            ws.tabs[0].panes[&pane_id].attached_terminal_id.clone(),
            crate::terminal::TerminalState::new(
                ws.tabs[0].panes[&pane_id].attached_terminal_id.clone(),
                std::path::PathBuf::from("/tmp"),
            ),
        );
        state
            .terminals
            .get_mut(&ws.tabs[0].panes[&pane_id].attached_terminal_id)
            .expect("terminal state")
            .launch_argv = Some(vec!["codex".to_string()]);
        state.workspaces = vec![ws];

        assert!(state.pane_exposes_host_cursor(0, pane_id));
    }

    #[test]
    fn adversarial_identity_state_satisfies_app_invariants_after_mutation() {
        let mut state = AppState::test_with_adversarial_identity_state();
        state.assert_invariants_for_test();

        let ws = &mut state.workspaces[0];
        let active_public = ws.tabs[ws.active_tab].number;
        assert_ne!(ws.active_tab + 1, active_public);
        let new_pane = ws.test_split(ratatui::layout::Direction::Horizontal);
        assert!(ws.public_pane_number(new_pane).is_some());
        state.ensure_test_terminals();

        state.assert_invariants_for_test();
    }

    #[test]
    fn relation_signal_damage_covers_a_pane_carrier_and_leaves_the_workspace_row_alone() {
        let mut state = AppState::test_new();
        let mut ws = Workspace::test_new("mate");
        let worker_pane = ws.test_split(Direction::Horizontal);
        state.workspaces = vec![ws];
        let worker_number = state.workspaces[0]
            .public_pane_number(worker_pane)
            .expect("split pane has a public number");
        let worker_public_id =
            crate::workspace::public_pane_id_for_number(&state.workspaces[0].id, worker_number);

        // A card for the worker's own row, as the sidebar would lay it out —
        // the mate->worker connector this feature exists for.
        state.view.workspace_card_areas = vec![WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 0, 20, 1),
            worktree_child: false,
            entry_idx: 0,
            agent: Some(AgentCardTarget {
                tab_idx: 0,
                pane_id: worker_pane,
            }),
            card_frame: None,
            motion_cells: (0, 0),
        }];

        assert!(
            !state.relation_signal_damage(),
            "no live signal yet, nothing should be damaged"
        );

        state
            .relation_signals
            .accept(
                "firstmate",
                None,
                crate::app::relation_signal::RelationSignalKind::Transfer,
                crate::app::relation_signal::CarrierId::Pane(worker_public_id),
                None,
                Instant::now(),
            )
            .expect("a fresh row always accepts its first signal");
        assert!(
            state.relation_signal_damage(),
            "a signal on the worker's own pane must damage the laid-out worker row"
        );

        // The same live signal must not read as damage against the mate's own
        // Space row: a pane carrier and a workspace carrier are different
        // rows even when one nests under the other.
        state.view.workspace_card_areas = vec![WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 0, 20, 1),
            worktree_child: false,
            entry_idx: 0,
            agent: None,
            card_frame: None,
            motion_cells: (0, 0),
        }];
        assert!(
            !state.relation_signal_damage(),
            "a pane carrier must not be mistaken for the workspace it lives in"
        );
    }

    #[test]
    fn relation_signal_damage_is_unchanged_for_a_workspace_carrier() {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("mate")];
        let workspace_id = state.workspaces[0].id.clone();

        state.view.workspace_card_areas = vec![WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 0, 20, 1),
            worktree_child: false,
            entry_idx: 0,
            agent: None,
            card_frame: None,
            motion_cells: (0, 0),
        }];
        assert!(!state.relation_signal_damage());

        state
            .relation_signals
            .accept(
                "firstmate",
                None,
                crate::app::relation_signal::RelationSignalKind::Transfer,
                crate::app::relation_signal::CarrierId::Workspace(workspace_id),
                None,
                Instant::now(),
            )
            .expect("a fresh row always accepts its first signal");
        assert!(
            state.relation_signal_damage(),
            "a signal on the workspace's own row must still damage a laid-out Space card"
        );
    }

    fn navigator_row_for_display(is_workspace: bool) -> NavigatorRow {
        NavigatorRow {
            target: NavigatorTarget::Workspace { ws_idx: 0 },
            depth: if is_workspace { 0 } else { 1 },
            label: String::new(),
            meta: String::new(),
            status: crate::detect::AgentState::Idle,
            seen: true,
            is_current: false,
            is_workspace,
            is_tab: false,
            expanded: true,
            search_text: String::new(),
            matched: true,
        }
    }

    #[test]
    fn navigator_display_lines_separate_workspace_groups() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
            navigator_row_for_display(false),
        ];
        assert_eq!(
            navigator_display_lines(&rows),
            vec![
                NavigatorDisplayLine::Row(0),
                NavigatorDisplayLine::Row(1),
                NavigatorDisplayLine::Spacer,
                NavigatorDisplayLine::Row(2),
                NavigatorDisplayLine::Row(3),
            ]
        );
    }

    #[test]
    fn navigator_display_lines_have_no_leading_spacer() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
        ];
        assert_eq!(
            navigator_display_lines(&rows),
            vec![NavigatorDisplayLine::Row(0), NavigatorDisplayLine::Row(1)]
        );
        assert!(navigator_display_lines(&[]).is_empty());
    }

    #[test]
    fn navigator_display_index_maps_row_to_line() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
        ];
        let lines = navigator_display_lines(&rows);
        assert_eq!(navigator_display_index_of_row(&lines, 2), Some(3));
        assert_eq!(navigator_display_index_of_row(&lines, 9), None);
    }

    #[test]
    fn navigator_first_row_skips_spacer_lines() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
        ];
        let lines = navigator_display_lines(&rows);
        // Line 2 is the spacer before the second workspace.
        assert_eq!(navigator_first_row_at_or_after(&lines, 2), Some(2));
        assert_eq!(navigator_first_row_at_or_after(&lines, 4), None);
    }

    #[test]
    fn built_in_theme_names_resolve() {
        for name in THEME_NAMES {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn built_in_themes_leave_sidebar_background_unset() {
        for name in THEME_NAMES {
            let palette = Palette::from_name(name).unwrap();
            assert_eq!(
                palette.sidebar_bg,
                Color::Reset,
                "built-in theme changed the sidebar background: {name}"
            );
        }
    }

    #[test]
    fn custom_sidebar_background_overrides_the_default() {
        let custom = crate::config::CustomThemeColors {
            sidebar_bg: Some("#181825".to_string()),
            ..Default::default()
        };

        assert_eq!(
            Palette::catppuccin().with_overrides(&custom).sidebar_bg,
            Color::Rgb(24, 24, 37)
        );
    }

    #[test]
    fn light_theme_aliases_resolve() {
        for name in ["light", "latte", "tokyo-day", "onelight", "lotus", "dawn"] {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn key_matches_requires_exact_modifiers() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));

        assert!(!key_matches(
            &KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));
    }

    #[test]
    fn key_matches_letters_case_insensitively() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
            KeyCode::Char('b'),
            KeyModifiers::SHIFT,
        ));
    }

    #[test]
    fn linked_worktree_context_menu_keeps_safe_close_and_explicit_remove() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                ws_idx: 0,
                is_linked_worktree: true,
                has_worktree_children: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert_eq!(
            menu.items(),
            &["Rename", "Close", "Delete worktree checkout..."]
        );
    }

    #[test]
    fn git_workspace_context_menu_keeps_remove_for_managed_worktrees_only() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert_eq!(
            menu.items(),
            &["Rename", "Close", "New worktree", "Open worktree..."]
        );
    }

    #[test]
    fn parent_worktree_context_menu_uses_repo_actions() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert_eq!(
            menu.items(),
            &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Collapse"
            ]
        );
    }
}
