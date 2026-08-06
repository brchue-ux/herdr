use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::detect::Agent;

const MAX_SIDEBAR_ROWS: usize = 16;
const MAX_SIDEBAR_TOKENS_PER_ROW: usize = 16;
const DEFAULT_SIDEBAR_ROW_GAP: u16 = 0;

fn deserialize_sidebar_rows<'de, D, T>(deserializer: D) -> Result<Vec<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let rows = Vec::<Vec<T>>::deserialize(deserializer)?;
    validate_sidebar_rows(&rows).map_err(serde::de::Error::custom)?;
    Ok(rows)
}

fn validate_sidebar_rows<T>(rows: &[Vec<T>]) -> Result<(), String> {
    if rows.len() > MAX_SIDEBAR_ROWS {
        return Err(format!(
            "sidebar layouts may contain at most {MAX_SIDEBAR_ROWS} rows"
        ));
    }
    if rows
        .iter()
        .any(|row| row.len() > MAX_SIDEBAR_TOKENS_PER_ROW)
    {
        return Err(format!(
            "sidebar rows may contain at most {MAX_SIDEBAR_TOKENS_PER_ROW} tokens"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarTokenColor {
    r: u8,
    g: u8,
    b: u8,
}

impl SidebarTokenColor {
    pub(crate) fn ratatui(self) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(self.r, self.g, self.b)
    }
}

impl Serialize for SidebarTokenColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b))
    }
}

impl<'de> Deserialize<'de> for SidebarTokenColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let hex = value.strip_prefix('#').filter(|hex| {
            hex.is_ascii()
                && matches!(hex.len(), 3 | 6)
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        let Some(hex) = hex else {
            return Err(serde::de::Error::custom(format!(
                "sidebar token colors must be #RGB or #RRGGBB, got `{value}`"
            )));
        };
        let (r, g, b) = if hex.len() == 3 {
            let mut digits = hex
                .bytes()
                .map(|byte| char::from(byte).to_digit(16).expect("validated hex digit") as u8 * 17);
            (
                digits.next().expect("three hex digits"),
                digits.next().expect("three hex digits"),
                digits.next().expect("three hex digits"),
            )
        } else {
            (
                u8::from_str_radix(&hex[0..2], 16).expect("validated hex digits"),
                u8::from_str_radix(&hex[2..4], 16).expect("validated hex digits"),
                u8::from_str_radix(&hex[4..6], 16).expect("validated hex digits"),
            )
        };
        Ok(Self { r, g, b })
    }
}

/// Which named animation behaviour a sidebar element plays.
///
/// Every variant but `None` names a behaviour in the animation engine's
/// catalogue (`crate::anim::behaviour::names`), which is what actually decides
/// how it looks. This enum exists only so a config typo is a parse error
/// listing the valid names rather than a silently dead animation; adding a
/// behaviour is a variant here and a row in that catalogue, and touches no
/// render code.
///
/// Animation is opt-in: an omitted or `none` value renders exactly like an
/// unstyled token and keeps the sidebar animation clock disarmed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarTokenEmphasis {
    /// No animation. The calm default.
    #[default]
    None,
    /// Ramp the foreground toward the panel background and back.
    Pulse,
    /// A bright band travels across the element.
    Shimmer,
    /// A phase-shifted ripple along the element.
    Wave,
    /// Brightness and tempo follow how hard this element's pane is working.
    Activity,
    /// A band whose speed follows how hard this element's pane is working.
    ActivityShimmer,
    /// Arrive everywhere at once.
    Fade,
    /// Arrive one cell at a time, left to right.
    Typewriter,
    /// A soft edge sweeps left to right.
    Wipe,
    /// Arrive top row first.
    DropIn,
    /// Cells arrive in a stable scatter.
    Dissolve,
    /// Close inward from the edges toward the centre.
    Collapse,
}

impl SidebarTokenEmphasis {
    fn animates(self) -> bool {
        self.behaviour().is_some()
    }

    /// The catalogue name this resolves to, or `None` for the calm default.
    pub(crate) fn behaviour(self) -> Option<&'static str> {
        use crate::anim::behaviour::names;
        Some(match self {
            Self::None => return None,
            Self::Pulse => names::PULSE,
            Self::Shimmer => names::SHIMMER,
            Self::Wave => names::WAVE,
            Self::Activity => names::ACTIVITY,
            Self::ActivityShimmer => names::ACTIVITY_SHIMMER,
            Self::Fade => names::FADE,
            Self::Typewriter => names::TYPEWRITER,
            Self::Wipe => names::WIPE,
            Self::DropIn => names::DROP_IN,
            Self::Dissolve => names::DISSOLVE,
            Self::Collapse => names::COLLAPSE,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarTokenStyle {
    pub fg: Option<SidebarTokenColor>,
    pub bg: Option<SidebarTokenColor>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub reverse: Option<bool>,
    pub emphasis: Option<SidebarTokenEmphasis>,
}

impl SidebarTokenStyle {
    /// True when this occurrence needs the sidebar animation clock running.
    pub(crate) fn animates(self) -> bool {
        self.emphasis.is_some_and(SidebarTokenEmphasis::animates)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSidebarToken {
    StateIcon,
    StateText,
    StateAge,
    Workspace,
    Tab,
    Pane,
    Agent,
    TerminalTitle,
    TerminalTitleStripped,
    Custom(String),
    Styled {
        token: Box<AgentSidebarToken>,
        style: SidebarTokenStyle,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSidebarToken {
    StateIcon,
    StateText,
    StateAge,
    Workspace,
    Branch,
    GitStatus,
    /// Uncommitted work in this Space's checkout.
    GitDirty,
    /// Open pull requests on the forge repository this Space pushes to.
    PullRequests,
    TerminalTitle,
    TerminalTitleStripped,
    Custom(String),
    Styled {
        token: Box<SpaceSidebarToken>,
        style: SidebarTokenStyle,
    },
}

impl AgentSidebarToken {
    pub(crate) fn parts(&self) -> (&Self, SidebarTokenStyle) {
        match self {
            Self::Styled { token, style } => (token, *style),
            token => (token, SidebarTokenStyle::default()),
        }
    }
}

impl SpaceSidebarToken {
    pub(crate) fn parts(&self) -> (&Self, SidebarTokenStyle) {
        match self {
            Self::Styled { token, style } => (token, *style),
            token => (token, SidebarTokenStyle::default()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStyledSidebarToken {
    token: String,
    #[serde(default)]
    fg: Option<SidebarTokenColor>,
    #[serde(default)]
    bg: Option<SidebarTokenColor>,
    #[serde(default)]
    bold: Option<bool>,
    #[serde(default)]
    dim: Option<bool>,
    #[serde(default)]
    italic: Option<bool>,
    #[serde(default)]
    underline: Option<bool>,
    #[serde(default)]
    reverse: Option<bool>,
    #[serde(default)]
    emphasis: Option<SidebarTokenEmphasis>,
}

enum RawSidebarToken {
    Plain(String),
    Styled(RawStyledSidebarToken),
}

/// Hand-written instead of `#[serde(untagged)]` so an invalid inline style
/// table reports the offending field instead of "did not match any variant".
impl<'de> Deserialize<'de> for RawSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RawSidebarTokenVisitor;

        impl<'de> serde::de::Visitor<'de> for RawSidebarTokenVisitor {
            type Value = RawSidebarToken;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a sidebar token name or an inline style table")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RawSidebarToken::Plain(value.to_string()))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                RawStyledSidebarToken::deserialize(serde::de::value::MapAccessDeserializer::new(
                    map,
                ))
                .map(RawSidebarToken::Styled)
            }
        }

        deserializer.deserialize_any(RawSidebarTokenVisitor)
    }
}

impl RawSidebarToken {
    fn parts(self) -> (String, Option<SidebarTokenStyle>) {
        match self {
            Self::Plain(token) => (token, None),
            Self::Styled(token) => (
                token.token,
                Some(SidebarTokenStyle {
                    fg: token.fg,
                    bg: token.bg,
                    bold: token.bold,
                    dim: token.dim,
                    italic: token.italic,
                    underline: token.underline,
                    reverse: token.reverse,
                    emphasis: token.emphasis,
                }),
            ),
        }
    }
}

fn parse_sidebar_token<T>(value: String, builtins: &[(&str, T)]) -> Result<T, String>
where
    T: Clone + From<String>,
{
    if let Some((_, token)) = builtins.iter().find(|(name, _)| *name == value) {
        return Ok(token.clone());
    }
    let Some(name) = value.strip_prefix('$') else {
        return Err(format!(
            "unknown sidebar token `{value}`; custom tokens must start with `$`"
        ));
    };
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!("invalid custom sidebar token `{value}`"));
    }
    Ok(T::from(name.to_string()))
}

fn serialize_styled_token<S>(
    name: String,
    style: SidebarTokenStyle,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(None)?;
    map.serialize_entry("token", &name)?;
    if let Some(fg) = style.fg {
        map.serialize_entry("fg", &fg)?;
    }
    if let Some(bg) = style.bg {
        map.serialize_entry("bg", &bg)?;
    }
    if let Some(bold) = style.bold {
        map.serialize_entry("bold", &bold)?;
    }
    if let Some(dim) = style.dim {
        map.serialize_entry("dim", &dim)?;
    }
    if let Some(italic) = style.italic {
        map.serialize_entry("italic", &italic)?;
    }
    if let Some(underline) = style.underline {
        map.serialize_entry("underline", &underline)?;
    }
    if let Some(reverse) = style.reverse {
        map.serialize_entry("reverse", &reverse)?;
    }
    if let Some(emphasis) = style.emphasis {
        map.serialize_entry("emphasis", &emphasis)?;
    }
    map.end()
}

fn agent_token_name(token: &AgentSidebarToken) -> String {
    match token {
        AgentSidebarToken::StateIcon => "state_icon".into(),
        AgentSidebarToken::StateText => "state_text".into(),
        AgentSidebarToken::StateAge => "state_age".into(),
        AgentSidebarToken::Workspace => "workspace".into(),
        AgentSidebarToken::Tab => "tab".into(),
        AgentSidebarToken::Pane => "pane".into(),
        AgentSidebarToken::Agent => "agent".into(),
        AgentSidebarToken::TerminalTitle => "terminal_title".into(),
        AgentSidebarToken::TerminalTitleStripped => "terminal_title_stripped".into(),
        AgentSidebarToken::Custom(name) => format!("${name}"),
        AgentSidebarToken::Styled { token, .. } => agent_token_name(token),
    }
}

fn space_token_name(token: &SpaceSidebarToken) -> String {
    match token {
        SpaceSidebarToken::StateIcon => "state_icon".into(),
        SpaceSidebarToken::StateText => "state_text".into(),
        SpaceSidebarToken::StateAge => "state_age".into(),
        SpaceSidebarToken::Workspace => "workspace".into(),
        SpaceSidebarToken::Branch => "branch".into(),
        SpaceSidebarToken::GitStatus => "git_status".into(),
        SpaceSidebarToken::GitDirty => "git_dirty".into(),
        SpaceSidebarToken::PullRequests => "pull_requests".into(),
        SpaceSidebarToken::TerminalTitle => "terminal_title".into(),
        SpaceSidebarToken::TerminalTitleStripped => "terminal_title_stripped".into(),
        SpaceSidebarToken::Custom(name) => format!("${name}"),
        SpaceSidebarToken::Styled { token, .. } => space_token_name(token),
    }
}

impl Serialize for AgentSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Styled { token, style } => {
                serialize_styled_token(agent_token_name(token), *style, serializer)
            }
            token => serializer.serialize_str(&agent_token_name(token)),
        }
    }
}

impl From<String> for AgentSidebarToken {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl<'de> Deserialize<'de> for AgentSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (value, style) = RawSidebarToken::deserialize(deserializer)?.parts();
        let token = parse_sidebar_token(
            value,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("state_age", Self::StateAge),
                ("workspace", Self::Workspace),
                ("tab", Self::Tab),
                ("pane", Self::Pane),
                ("agent", Self::Agent),
                ("terminal_title", Self::TerminalTitle),
                ("terminal_title_stripped", Self::TerminalTitleStripped),
            ],
        )
        .map_err(serde::de::Error::custom)?;
        Ok(style.map_or(token.clone(), |style| Self::Styled {
            token: Box::new(token),
            style,
        }))
    }
}

impl Serialize for SpaceSidebarToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Styled { token, style } => {
                serialize_styled_token(space_token_name(token), *style, serializer)
            }
            token => serializer.serialize_str(&space_token_name(token)),
        }
    }
}

impl From<String> for SpaceSidebarToken {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl<'de> Deserialize<'de> for SpaceSidebarToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (value, style) = RawSidebarToken::deserialize(deserializer)?.parts();
        let token = parse_sidebar_token(
            value,
            &[
                ("state_icon", Self::StateIcon),
                ("state_text", Self::StateText),
                ("state_age", Self::StateAge),
                ("workspace", Self::Workspace),
                ("branch", Self::Branch),
                ("git_status", Self::GitStatus),
                ("git_dirty", Self::GitDirty),
                ("pull_requests", Self::PullRequests),
                ("terminal_title", Self::TerminalTitle),
                ("terminal_title_stripped", Self::TerminalTitleStripped),
            ],
        )
        .map_err(serde::de::Error::custom)?;
        Ok(style.map_or(token.clone(), |style| Self::Styled {
            token: Box::new(token),
            style,
        }))
    }
}

type AgentSidebarRows = Vec<Vec<AgentSidebarToken>>;
type SpaceSidebarRows = Vec<Vec<SpaceSidebarToken>>;

fn deserialize_rows_by_agent<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, AgentSidebarRows>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let rows_by_agent = BTreeMap::<String, AgentSidebarRows>::deserialize(deserializer)?;
    for (id, rows) in &rows_by_agent {
        if crate::detect::parse_canonical_agent_label(id).is_none() {
            return Err(serde::de::Error::custom(format!(
                "unknown canonical agent id `{id}` in sidebar rows_by_agent"
            )));
        }
        validate_sidebar_rows(rows).map_err(serde::de::Error::custom)?;
    }
    Ok(rows_by_agent)
}

/// A declaratively owned Agents view: the `agent.view.set` filter/sort grammar
/// written down in `config.toml` so it survives a server restart instead of
/// needing an external process to reapply it.
///
/// The grammar itself is owned by [`crate::agent_view`]; this type only parses
/// and records a diagnostic. An illegal view is dropped rather than partially
/// applied, so a broken config shows every agent instead of an empty panel that
/// looks like "no agents".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct AgentsViewConfig {
    /// Short name shown in the Agents panel header while this view is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Filter tree, identical in shape to `agent.view.set`'s `filter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<crate::api::schema::AgentViewFilter>,
    /// Sort keys, identical in shape to `agent.view.set`'s `sort`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<crate::api::schema::AgentViewSort>,
    /// Why the declared view was rejected. Not a config key.
    #[serde(skip)]
    pub diagnostic: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentsViewConfig {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    filter: Option<toml::Value>,
    #[serde(default)]
    sort: Option<toml::Value>,
}

impl<'de> Deserialize<'de> for AgentsViewConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Buffer the table instead of failing the deserializer: one malformed
        // view must not take the rest of the user's config down with it.
        Ok(Self::from_toml_value(toml::Value::deserialize(
            deserializer,
        )?))
    }
}

impl AgentsViewConfig {
    fn from_toml_value(value: toml::Value) -> Self {
        let raw = match value.try_into::<RawAgentsViewConfig>() {
            Ok(raw) => raw,
            Err(err) => return Self::rejected(err.to_string()),
        };

        let filter = match raw.filter {
            Some(value) => match value.clone().try_into() {
                Ok(filter) => Some(filter),
                Err(err) => {
                    return Self::rejected(format!(
                        "filter is not a valid agent view filter: {}",
                        crate::agent_view::explain_view_value(&value, false)
                            .unwrap_or_else(|| err.to_string())
                    ))
                }
            },
            None => None,
        };
        let sort = match raw.sort {
            Some(value) => match value.clone().try_into::<Vec<_>>() {
                Ok(sort) => sort,
                Err(err) => {
                    return Self::rejected(format!(
                        "sort is not a valid agent view sort: {}",
                        crate::agent_view::explain_view_value(&value, true)
                            .unwrap_or_else(|| err.to_string())
                    ))
                }
            },
            None => Vec::new(),
        };

        let declared_anything = raw.label.is_some() || filter.is_some() || !sort.is_empty();
        let mut spec = crate::api::schema::AgentViewSetParams {
            source: crate::agent_view::CONFIG_VIEW_SOURCE.to_string(),
            label: raw.label,
            filter,
            sort,
        };
        if let Err(message) = crate::agent_view::validate_agent_view(&mut spec) {
            return Self::rejected(message);
        }
        if declared_anything && spec.filter.is_none() && spec.sort.is_empty() {
            return Self::rejected(
                "declares no filter and no sort, so it selects nothing".to_string(),
            );
        }

        Self {
            label: spec.label,
            filter: spec.filter,
            sort: spec.sort,
            diagnostic: None,
        }
    }

    fn rejected(reason: String) -> Self {
        Self {
            diagnostic: Some(format!(
                "invalid [ui.sidebar.agents.view]: {reason}; ignoring the declared view and showing every agent"
            )),
            ..Self::default()
        }
    }

    /// The view this config declares, ready for the config tier of the view
    /// slots. `None` when nothing was declared or the declaration was rejected.
    pub(crate) fn declared_view(&self) -> Option<crate::api::schema::AgentViewSetParams> {
        (self.filter.is_some() || !self.sort.is_empty()).then(|| {
            crate::api::schema::AgentViewSetParams {
                source: crate::agent_view::CONFIG_VIEW_SOURCE.to_string(),
                label: self.label.clone(),
                filter: self.filter.clone(),
                sort: self.sort.clone(),
            }
        })
    }

    pub(crate) fn diagnostics(&self) -> Option<String> {
        self.diagnostic.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentsSidebarConfig {
    #[serde(deserialize_with = "deserialize_sidebar_rows")]
    pub rows: AgentSidebarRows,
    #[serde(default, deserialize_with = "deserialize_rows_by_agent")]
    pub rows_by_agent: BTreeMap<String, AgentSidebarRows>,
    pub row_gap: u16,
    pub view: AgentsViewConfig,
}

impl AgentsSidebarConfig {
    /// True when any configured Agent row asks for animated emphasis.
    pub(crate) fn has_animated_tokens(&self) -> bool {
        let animates = |rows: &AgentSidebarRows| {
            rows.iter()
                .flatten()
                .any(|token| token.parts().1.animates())
        };
        animates(&self.rows) || self.rows_by_agent.values().any(animates)
    }

    pub(crate) fn rows_for_agent(&self, agent: Option<Agent>) -> &AgentSidebarRows {
        agent
            .and_then(|agent| self.rows_by_agent.get(crate::detect::agent_label(agent)))
            .unwrap_or(&self.rows)
    }

    /// Whether any configured Agent row, including per-agent overrides, renders
    /// a terminal title.
    pub(crate) fn uses_terminal_title(&self) -> bool {
        std::iter::once(&self.rows)
            .chain(self.rows_by_agent.values())
            .flatten()
            .flatten()
            .any(|token| {
                matches!(
                    token.parts().0,
                    AgentSidebarToken::TerminalTitle | AgentSidebarToken::TerminalTitleStripped
                )
            })
    }

    /// Every distinct behaviour a configured Agent row asks for, including
    /// per-agent overrides.
    ///
    /// The animation engine has to be told up front which behaviours a row can
    /// be asked to draw, because it accumulates a separate phase for each one.
    /// See `crate::anim::Lifecycle::idle`.
    pub(crate) fn animated_behaviours(&self) -> Vec<&'static str> {
        collect_behaviours(
            std::iter::once(&self.rows)
                .chain(self.rows_by_agent.values())
                .flatten()
                .flatten()
                .filter_map(|token| token.parts().1.emphasis),
        )
    }

    /// Whether any configured Agent row, including per-agent overrides, draws
    /// an elapsed time. Nothing else may arm the repaint clock that keeps one
    /// current, so an unconfigured Herdr never wakes up for it.
    pub(crate) fn uses_state_age(&self) -> bool {
        std::iter::once(&self.rows)
            .chain(self.rows_by_agent.values())
            .flatten()
            .flatten()
            .any(|token| matches!(token.parts().0, AgentSidebarToken::StateAge))
    }
}

impl Default for AgentsSidebarConfig {
    fn default() -> Self {
        Self {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Workspace,
                    AgentSidebarToken::Tab,
                ],
                vec![AgentSidebarToken::Agent],
            ],
            rows_by_agent: BTreeMap::new(),
            row_gap: DEFAULT_SIDEBAR_ROW_GAP,
            view: AgentsViewConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SpacesSidebarConfig {
    #[serde(deserialize_with = "deserialize_sidebar_rows")]
    pub rows: SpaceSidebarRows,
    pub row_gap: u16,
}

impl SpacesSidebarConfig {
    /// Whether any configured Space row renders a terminal title.
    pub(crate) fn uses_terminal_title(&self) -> bool {
        self.rows.iter().flatten().any(|token| {
            matches!(
                token.parts().0,
                SpaceSidebarToken::TerminalTitle | SpaceSidebarToken::TerminalTitleStripped
            )
        })
    }

    /// True when any configured Space row asks for animated emphasis.
    pub(crate) fn has_animated_tokens(&self) -> bool {
        self.rows
            .iter()
            .flatten()
            .any(|token| token.parts().1.animates())
    }

    /// Every distinct behaviour a configured Space row asks for. Same contract
    /// as the Agent panel's version above.
    pub(crate) fn animated_behaviours(&self) -> Vec<&'static str> {
        collect_behaviours(
            self.rows
                .iter()
                .flatten()
                .filter_map(|token| token.parts().1.emphasis),
        )
    }

    /// Whether any configured Space row draws an elapsed time.
    pub(crate) fn uses_state_age(&self) -> bool {
        self.rows
            .iter()
            .flatten()
            .any(|token| matches!(token.parts().0, SpaceSidebarToken::StateAge))
    }
}

impl Default for SpacesSidebarConfig {
    fn default() -> Self {
        Self {
            rows: vec![
                vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::Branch, SpaceSidebarToken::GitStatus],
            ],
            row_gap: DEFAULT_SIDEBAR_ROW_GAP,
        }
    }
}

/// Distinct behaviour names from a run of configured emphases, in the order
/// they were first configured.
///
/// Deduplicated because the engine keeps one phase per declared behaviour, and
/// two tokens asking for the same behaviour genuinely are one behaviour — they
/// should stay in step, not drift apart.
fn collect_behaviours(emphases: impl Iterator<Item = SidebarTokenEmphasis>) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for name in emphases.filter_map(SidebarTokenEmphasis::behaviour) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SidebarConfig {
    pub agents: AgentsSidebarConfig,
    pub spaces: SpacesSidebarConfig,
    pub animation: SidebarAnimationConfig,
    pub notifications: SidebarNotificationsConfig,
    pub signal_tray: SidebarSignalTrayConfig,
}

/// How long a signal's arrival runs. Short: an alert lighting up is news, and
/// news that takes half a second to become readable is late news.
const DEFAULT_SIGNAL_ENTER_MS: u64 = 220;

/// The always-present bar of fleet signals above the tree.
///
/// Off by default, and deliberately so. Three of the eight slots read counts
/// that cost a `git status` scan or a network round trip to the forge, and both
/// of those are demand-gated on something rendering them — turning the bar on
/// by default would start paying for both in every session, for a readout
/// nobody asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarNotificationsConfig {
    /// Whether the bar is drawn at all. When it is, it is drawn always: every
    /// slot is present in every frame, named and grey until its own signal goes
    /// live. A bar that came and went would make its positions unlearnable.
    pub enabled: bool,
    /// What a slot does while its signal is live. `none` leaves a live slot
    /// coloured but still.
    pub emphasis: SidebarTokenEmphasis,
    /// Behaviour a slot plays as it goes live.
    pub enter: SidebarTokenEmphasis,
    /// How long that arrival takes, in milliseconds.
    pub enter_ms: u64,
}

impl Default for SidebarNotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            // Colour alone is a weak signal on a one-cell mark, so the live
            // state breathes by default. It is still overridable to `none` for
            // anyone who wants colour and nothing else.
            emphasis: SidebarTokenEmphasis::Pulse,
            enter: SidebarTokenEmphasis::Fade,
            enter_ms: DEFAULT_SIGNAL_ENTER_MS,
        }
    }
}

impl SidebarNotificationsConfig {
    /// The arrival stage a slot plays as it goes live, or `None` when a live
    /// slot is configured to simply appear.
    pub(crate) fn enter_stage(&self) -> Option<crate::anim::Stage> {
        let behaviour = self.enter.behaviour()?;
        Some(crate::anim::Stage::new(
            behaviour,
            std::time::Duration::from_millis(
                self.enter_ms.clamp(MIN_ROW_ENTER_MS, MAX_ROW_ENTER_MS),
            ),
        ))
    }

    /// The life a slot is given when its signal goes live.
    ///
    /// One idle behaviour, because a slot is one mark saying one thing — unlike
    /// a tree row, which carries several tokens that may each animate their own
    /// way.
    pub(crate) fn lifecycle(&self) -> crate::anim::Lifecycle {
        let mut lifecycle = crate::anim::Lifecycle::still();
        lifecycle.mount = self.enter_stage();
        if let Some(behaviour) = self.emphasis.behaviour() {
            lifecycle = lifecycle.with_idle(behaviour);
        }
        lifecycle
    }

    /// True when a drawn bar has something moving in it.
    pub(crate) fn animates(&self) -> bool {
        self.enabled && (self.emphasis.animates() || self.enter_stage().is_some())
    }
}

/// The notification tray at the foot of the Spaces panel.
///
/// Off by default for the same reason the signal bar is, and then some: four of
/// its eight slots read counts that cost a network round trip to the forge or a
/// `git status` scan, and both are demand-gated on something rendering them.
/// The tray also arms the scan the bar deliberately does not, because its `sync`
/// slot refuses on a dirty tree — see
/// [`crate::app::fleet_signals::FleetSignalDemand::for_tray`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarSignalTrayConfig {
    /// Whether the tray is drawn at all. When it is, all eight slots are drawn,
    /// every frame, for the same reason the bar draws all eight: position is
    /// half of what makes a badge readable at a glance.
    pub enabled: bool,
    /// Whether a badge click may run the in-place acts at all.
    ///
    /// `false` turns every badge into a jump. Nothing else changes: the same
    /// eight slots light on the same eight conditions, and the popup still says
    /// what each one covers. This exists because "may Herdr run `git push` on my
    /// behalf from a click" is a policy question with a legitimate no.
    pub actions: bool,
}

impl Default for SidebarSignalTrayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            // On, but only ever reached from a popup that has already printed
            // the exact command. Turning the whole tray on is the opt-in; being
            // asked to confirm a push after that is not a second one.
            actions: true,
        }
    }
}

/// How long a row's arrival runs when one is configured.
///
/// Short enough that a row is readable almost immediately, long enough that the
/// arrival is a movement rather than a flicker.
const DEFAULT_ROW_ENTER_MS: u64 = 320;
/// Bounds on a configured arrival. A publisher of config cannot make a row
/// unreadable for a second and a half, and cannot ask for a duration too short
/// to resolve a single frame.
const MIN_ROW_ENTER_MS: u64 = 60;
const MAX_ROW_ENTER_MS: u64 = 1_500;

/// How long a row's departure runs when one is configured.
///
/// Shorter than the arrival on purpose: an arrival is introducing something the
/// eye has to find, a departure is releasing something it has already read.
const DEFAULT_ROW_EXIT_MS: u64 = 220;

/// How long each half of a tree view switch takes by default.
///
/// Unlike a row arrival this is on out of the box, because the switch itself is
/// the behaviour: re-rooting the tree with no transition is a hard cut from one
/// set of rows to another, which is precisely the jumbling the transition
/// exists to avoid.
///
/// # Why it is not the 220 ms it was
///
/// The transition redraws on `SMOOTH_FRAME_INTERVAL`, which is 50 ms, so the
/// duration is not a continuous dial — it is a frame count. 220 ms is *four
/// frames per half*, and four frames is not an animation anyone can read: it
/// is one intermediate state on the way in, one on the way out, and a lot of
/// hard edge. At 640 ms a half is twelve or thirteen frames, which is where a
/// dissolve stops reading as a flicker and starts reading as a thing coming
/// apart — and it is still under two thirds of a second each way, so drilling
/// into a mate is still a navigation rather than a wait.
const DEFAULT_VIEW_SWITCH_MS: u64 = 640;
const MIN_VIEW_SWITCH_MS: u64 = 60;
const MAX_VIEW_SWITCH_MS: u64 = 1_500;

/// Particles per terminal cell in the card sheet's own dissolve, by default.
///
/// Off. The characters keep dissolving exactly as they did; the pixel cards
/// keep hard-cutting at the commit instant. See
/// [`SidebarAnimationConfig::view_switch_particles_per_cell`] for what turning
/// it on costs and why the number is per *cell* rather than in pixels.
const DEFAULT_VIEW_SWITCH_PARTICLES_PER_CELL: u16 = 0;

/// Most particles one cell may be broken into.
///
/// A 10x21 px cell holds 210 pixels, so anything past that is asking for a
/// particle finer than a pixel — the same dissolve, costing more to compute.
/// The ceiling is stated rather than derived because the cell size is the
/// host's to report and a config value must mean something before one is known.
const MAX_VIEW_SWITCH_PARTICLES_PER_CELL: u16 = 256;

/// Whether a sidebar row's arrival and departure *move* it.
///
/// Deliberately a separate setting from [`SidebarAnimationConfig::row_enter`]
/// rather than another variant of it. `row_enter` says what a row's *cells* do
/// while it arrives — dissolve, wipe, fade — and every one of those names is an
/// effect a character cell can express. Position is not: a cell cannot leave its
/// cell, so motion is not a behaviour the catalogue could hold, and folding it
/// into the same enum would make two orthogonal choices mutually exclusive.
///
/// Keeping them apart is what lets them compose. A row can dissolve in *and*
/// slide in, dissolve in without moving, or move without dissolving; and turning
/// motion on retires nothing that was already configured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarRowMotion {
    /// Rows are drawn where the layout puts them, on every frame. The default,
    /// and exactly what the panel has always done.
    #[default]
    None,
    /// An arriving row slides in from the panel's right edge while every row
    /// below it pans down to open its slot; a departing row slides back out the
    /// way it came while they pan up to close it.
    ///
    /// Only the *pixel* cards move. See [`crate::ui::sidebar::motion`] for why
    /// the character fallback cuts instead.
    Slide,
}

/// Lifecycle animation for sidebar rows themselves, as opposed to the tokens
/// drawn inside them.
///
/// A row's arrival is a property of the row, so it lives here rather than on any
/// one token: every token on an arriving row arrives with it. Its departure is
/// the same property read the other way, and is deliberately the same set of
/// behaviour names — the engine plays an exit as its entry reversed, so a fleet
/// that wants a group to close the way it opened writes one name twice rather
/// than hunting for a mirror-image behaviour that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarAnimationConfig {
    /// Behaviour a row plays as it arrives. `none` — the default — means rows
    /// appear the way they always have, fully drawn on their first frame.
    pub row_enter: SidebarTokenEmphasis,
    /// How long that arrival takes, in milliseconds.
    pub row_enter_ms: u64,
    /// Behaviour a row plays as it leaves. `none` — the default — means a row
    /// whose pane is gone stops being drawn on the very next frame, exactly as
    /// it always has.
    pub row_exit: SidebarTokenEmphasis,
    /// How long that departure takes, in milliseconds.
    pub row_exit_ms: u64,
    /// Whether a row's arrival and departure move it, on top of whatever its
    /// cells are doing. `none` — the default — leaves every row exactly where
    /// the layout puts it, which is what the panel has always done.
    ///
    /// Motion runs on the arrival and departure phases `row_enter_ms` and
    /// `row_exit_ms` already time, so it needs no duration of its own and can
    /// never disagree with the effect it composes with. It also *creates* those
    /// phases: a row asked to slide with `row_enter = "none"` still gets a
    /// bounded arrival to slide through, playing
    /// [`crate::anim::behaviour::names::STILL`] so its characters look settled
    /// throughout.
    ///
    /// Pixel cards only, so a host without `[experimental] kitty_graphics` and
    /// `sidebar_card_shapes` is left exactly as it was — including the phase
    /// motion would have invented, which is why the gate is
    /// [`crate::app::state::AppState::sidebar_rows_move`] rather than this
    /// field.
    pub row_motion: SidebarRowMotion,
    /// Behaviour the whole tree plays when it is re-rooted onto one second
    /// mate, and again on the way back out.
    ///
    /// One name for both halves for the same reason a row's exit reuses its
    /// entry: the engine plays a dismount as its mount reversed, so the view
    /// comes apart exactly the way it formed. Unlike `row_enter` this is on out
    /// of the box, because re-rooting with no transition is a hard cut from one
    /// set of rows to another — precisely the jumbling the transition exists to
    /// avoid. `none` restores that hard cut.
    pub view_switch: SidebarTokenEmphasis,
    /// How long each half of that switch takes, in milliseconds.
    ///
    /// The view being left dematerializes for this long, the new root is
    /// adopted at the instant nothing is on screen, and the view being arrived
    /// at materializes for the same again.
    pub view_switch_ms: u64,
    /// How many particles one terminal cell of the *pixel* card sheet is broken
    /// into as the view comes apart. `0` — the default — leaves the sheet
    /// hard-cutting at the commit instant, which is what it has always done.
    ///
    /// # What this is for
    ///
    /// On a terminal without Kitty graphics the tree is characters and
    /// `view_switch` is the whole effect. On one with it, the sidebar's cards
    /// are a picture drawn *over* those characters and opaque across every cell
    /// a card occupies — so the character dissolve is running underneath a
    /// picture that is standing still, and what is actually visible is a thin
    /// border of connectors coming apart around a block of cards that jump. The
    /// sheet is an image, so it can dissolve at whatever resolution it is asked
    /// to, and this is that resolution.
    ///
    /// # Why per cell and not in pixels
    ///
    /// So a setting means the same thing on every host. The particle edge is
    /// derived from the cell the terminal actually reports, so `1` is one
    /// particle per cell — the finest a character dissolve can be, matched
    /// exactly — and the count goes up from there. `20` is twenty times the
    /// particles a character dissolve has available to it, on a cell of any
    /// size. Clamped to 256, which is finer than a pixel on most cells.
    pub view_switch_particles_per_cell: u16,
}

impl Default for SidebarAnimationConfig {
    fn default() -> Self {
        Self {
            row_enter: SidebarTokenEmphasis::None,
            row_enter_ms: DEFAULT_ROW_ENTER_MS,
            row_exit: SidebarTokenEmphasis::None,
            row_exit_ms: DEFAULT_ROW_EXIT_MS,
            row_motion: SidebarRowMotion::None,
            view_switch: SidebarTokenEmphasis::Dissolve,
            view_switch_ms: DEFAULT_VIEW_SWITCH_MS,
            view_switch_particles_per_cell: DEFAULT_VIEW_SWITCH_PARTICLES_PER_CELL,
        }
    }
}

impl SidebarAnimationConfig {
    /// True when rows are *configured* to move as they arrive and leave.
    ///
    /// Configuration only. Whether rows can actually move is a property of the
    /// host as much as of the config — motion is an offset applied to a pixel
    /// card, and a terminal drawing characters has nothing to offset — so every
    /// caller outside this module asks
    /// [`crate::app::state::AppState::sidebar_rows_move`], which folds the two
    /// together. Handing this flag straight to [`Self::stage`] is what once let
    /// a character-only host synthesize a departure phase it could not play,
    /// and keep a closed pane's row on screen for the whole of it.
    pub(crate) fn rows_move(&self) -> bool {
        matches!(self.row_motion, SidebarRowMotion::Slide)
    }

    /// The arrival stage, or `None` when rows are configured to just appear.
    ///
    /// `moves` is whether rows move *on this host*, from
    /// [`crate::app::state::AppState::sidebar_rows_move`].
    pub(crate) fn row_enter_stage(&self, moves: bool) -> Option<crate::anim::Stage> {
        Self::stage(self.row_enter, self.row_enter_ms, moves)
    }

    /// The departure stage, or `None` when rows are configured to just vanish.
    ///
    /// This is the one thing that decides whether a row that has left is still
    /// drawn: with no exit stage a departing row is retired on the spot, so
    /// nothing downstream has to know an exit was configured.
    pub(crate) fn row_exit_stage(&self, moves: bool) -> Option<crate::anim::Stage> {
        Self::stage(self.row_exit, self.row_exit_ms, moves)
    }

    /// `moves` is what lets motion stand on its own. A row with no cell
    /// emphasis normally has no bounded phase at all, and a phase is exactly
    /// what motion is carried on — so when rows are asked to move, the phase is
    /// created anyway and plays a behaviour that draws nothing. That is the
    /// whole of "slide composes with dissolve rather than replacing it": both
    /// read the same clock, and neither needs the other to be configured.
    ///
    /// It is deliberately a parameter rather than [`Self::rows_move`] read
    /// here: a phase created for a movement that cannot happen is a row drawn
    /// from memory with nothing playing on it.
    fn stage(emphasis: SidebarTokenEmphasis, ms: u64, moves: bool) -> Option<crate::anim::Stage> {
        let behaviour = match emphasis.behaviour() {
            Some(behaviour) => behaviour,
            None if moves => crate::anim::behaviour::names::STILL,
            None => return None,
        };
        Some(crate::anim::Stage::new(
            behaviour,
            std::time::Duration::from_millis(ms.clamp(MIN_ROW_ENTER_MS, MAX_ROW_ENTER_MS)),
        ))
    }

    /// The stage a whole view plays as it forms, and — reversed by the engine —
    /// as it comes apart.
    ///
    /// One stage serves both halves, so the duration the loop waits before
    /// adopting the incoming root is by construction the same duration the
    /// outgoing view spends leaving. They cannot disagree about when the panel
    /// is empty.
    /// Particles per cell the sheet's own dissolve runs at, clamped.
    ///
    /// Zero is off and stays zero: the clamp is a ceiling on a live effect, not
    /// a floor that turns one on.
    pub(crate) fn view_switch_particles(&self) -> u16 {
        self.view_switch_particles_per_cell
            .min(MAX_VIEW_SWITCH_PARTICLES_PER_CELL)
    }

    pub(crate) fn view_switch_stage(&self) -> Option<crate::anim::Stage> {
        let behaviour = self.view_switch.behaviour()?;
        Some(crate::anim::Stage::new(
            behaviour,
            std::time::Duration::from_millis(
                self.view_switch_ms
                    .clamp(MIN_VIEW_SWITCH_MS, MAX_VIEW_SWITCH_MS),
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_exit_parses_and_is_clamped_like_an_arrival() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.animation]
row_enter = "wipe"
row_exit = "wipe"
row_exit_ms = 5
"#,
        )
        .expect("row exit config");

        let animation = config.ui.sidebar.animation;
        assert_eq!(animation.row_exit, SidebarTokenEmphasis::Wipe);
        let exit = animation
            .row_exit_stage(animation.rows_move())
            .expect("a named exit is a stage");
        assert_eq!(
            exit.duration,
            std::time::Duration::from_millis(MIN_ROW_ENTER_MS),
            "an exit too short to resolve a frame is clamped, same as an arrival"
        );
        // The same name in both directions is the point: the engine plays the
        // exit as the arrival reversed rather than needing a mirror behaviour.
        assert_eq!(
            exit.behaviour,
            animation
                .row_enter_stage(animation.rows_move())
                .expect("a named arrival is a stage")
                .behaviour
        );
    }

    #[test]
    fn rows_neither_arrive_nor_leave_unless_asked_to() {
        let animation = SidebarAnimationConfig::default();
        assert!(animation.row_enter_stage(animation.rows_move()).is_none());
        assert!(
            animation.row_exit_stage(animation.rows_move()).is_none(),
            "an unconfigured Herdr must drop a closed pane's row on the next frame"
        );
        assert!(!animation.rows_move(), "motion must not arrive switched on");
    }

    /// Motion and a cell effect are two settings, and either one alone is a
    /// whole answer. This is the part the captain asked to be spelled out:
    /// turning `row_motion` on retires nothing about the dissolve he chose.
    #[test]
    fn motion_composes_with_a_cell_effect_rather_than_replacing_it() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.animation]
row_enter = "dissolve"
row_enter_ms = 260
row_exit = "dissolve"
row_exit_ms = 220
row_motion = "slide"
"#,
        )
        .expect("row motion config");

        let animation = config.ui.sidebar.animation;
        assert!(animation.rows_move());
        // The dissolve still owns what the cells do, and it still runs for
        // exactly as long as it was told to.
        let enter = animation
            .row_enter_stage(animation.rows_move())
            .expect("an arrival");
        assert_eq!(enter.behaviour, crate::anim::behaviour::names::DISSOLVE);
        assert_eq!(enter.duration, std::time::Duration::from_millis(260));
        assert_eq!(
            animation
                .row_exit_stage(animation.rows_move())
                .expect("a departure")
                .duration,
            std::time::Duration::from_millis(220)
        );
    }

    /// And motion alone still gets a phase to move through — otherwise asking
    /// for it without also asking for a cell effect would silently do nothing.
    #[test]
    fn motion_alone_still_gives_a_row_an_arrival_and_a_departure() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.animation]
row_motion = "slide"
"#,
        )
        .expect("row motion config");

        let animation = config.ui.sidebar.animation;
        assert_eq!(animation.row_enter, SidebarTokenEmphasis::None);
        let enter = animation
            .row_enter_stage(animation.rows_move())
            .expect("motion needs a phase");
        assert_eq!(
            enter.behaviour,
            crate::anim::behaviour::names::STILL,
            "a row asked only to move must not also be given a cell effect"
        );
        assert_eq!(
            animation
                .row_exit_stage(animation.rows_move())
                .expect("a departure")
                .behaviour,
            crate::anim::behaviour::names::STILL
        );
    }

    /// A row playing `still` looks exactly like a row that is not animating,
    /// which is what makes it safe to give one to a row that only wants to move.
    #[test]
    fn the_still_behaviour_draws_nothing_at_all() {
        let catalogue = crate::anim::behaviour::Catalogue::built_in();
        let behaviour = catalogue
            .get(crate::anim::behaviour::names::STILL)
            .expect("still is a built-in");
        let extent = crate::anim::cell::CellExtent::new(20, 4);
        let palette = crate::anim::cell::InkPalette {
            surface: (0, 0, 0),
            own: (200, 200, 200),
            accent: (0, 0, 255),
            signal: (0, 0, 255),
        };
        for progress in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for row in 0..extent.rows {
                for col in 0..extent.cols {
                    assert!(
                        behaviour
                            .cell(
                                crate::anim::cell::CellPos::new(col, row),
                                extent,
                                progress,
                                crate::anim::behaviour::DriveInputs::default(),
                                palette,
                            )
                            .is_settled(),
                        "still painted a cell at progress {progress}"
                    );
                }
            }
        }
    }

    /// Off by default, and off means the engine is asked for nothing. Turning
    /// it on is what pays for the Git scan and the forge request behind three
    /// of its slots, so it must never arrive switched on.
    #[test]
    fn the_signal_bar_is_off_until_it_is_asked_for() {
        let config = SidebarConfig::default();
        assert!(!config.notifications.enabled);
        assert!(!config.notifications.animates());
    }

    #[test]
    fn a_configured_signal_bar_declares_its_own_life() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.notifications]
enabled = true
emphasis = "shimmer"
enter = "wipe"
enter_ms = 900
"#,
        )
        .expect("signal bar config");
        let notifications = config.ui.sidebar.notifications;

        assert!(notifications.enabled);
        assert!(notifications.animates());
        let lifecycle = notifications.lifecycle();
        assert_eq!(
            lifecycle.mount,
            Some(crate::anim::Stage::new(
                crate::anim::behaviour::names::WIPE,
                std::time::Duration::from_millis(900),
            ))
        );
        assert_eq!(
            lifecycle.idle,
            vec![crate::anim::behaviour::names::SHIMMER.to_string()]
        );
    }

    /// A live slot can be colour-only. It is still a change the eye catches,
    /// and it costs the loop nothing.
    #[test]
    fn a_still_signal_bar_asks_the_animation_clock_for_nothing() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.notifications]
enabled = true
emphasis = "none"
enter = "none"
"#,
        )
        .expect("still signal bar config");
        let notifications = config.ui.sidebar.notifications;

        assert!(notifications.enabled);
        assert!(!notifications.animates());
        assert_eq!(notifications.lifecycle(), crate::anim::Lifecycle::still());
    }

    #[test]
    fn a_signal_arrival_is_clamped_the_same_way_a_row_arrival_is() {
        let long = SidebarNotificationsConfig {
            enabled: true,
            enter_ms: 60_000,
            ..Default::default()
        };
        assert_eq!(
            long.enter_stage().map(|stage| stage.duration),
            Some(std::time::Duration::from_millis(MAX_ROW_ENTER_MS))
        );

        let short = SidebarNotificationsConfig {
            enabled: true,
            enter_ms: 1,
            ..Default::default()
        };
        assert_eq!(
            short.enter_stage().map(|stage| stage.duration),
            Some(std::time::Duration::from_millis(MIN_ROW_ENTER_MS))
        );
    }

    #[test]
    fn defaults_match_the_compact_agent_and_existing_space_layouts() {
        let config = SidebarConfig::default();
        assert_eq!(
            config.agents.rows,
            vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Workspace,
                    AgentSidebarToken::Tab,
                ],
                vec![AgentSidebarToken::Agent],
            ]
        );
        assert!(config.agents.rows_by_agent.is_empty());
        assert_eq!(config.agents.row_gap, 0);
        assert_eq!(
            config.spaces.rows,
            vec![
                vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
                vec![SpaceSidebarToken::Branch, SpaceSidebarToken::GitStatus],
            ]
        );
        assert_eq!(config.spaces.row_gap, 0);
    }

    #[test]
    fn state_age_parses_round_trips_and_arms_only_its_own_rows() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.agents]
rows = [["state_text", "state_age"]]

[ui.sidebar.spaces]
rows = [["workspace"]]
"#,
        )
        .expect("state_age config");

        assert_eq!(
            config.ui.sidebar.agents.rows,
            vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::StateAge
            ]]
        );
        assert!(config.ui.sidebar.agents.uses_state_age());
        // The two panels are gated independently, so a Space row without the
        // token does not pay for an Agent row that has it, or the reverse.
        assert!(!config.ui.sidebar.spaces.uses_state_age());
        assert!(!SidebarConfig::default().agents.uses_state_age());
        assert!(!SidebarConfig::default().spaces.uses_state_age());

        // A styled occurrence still counts, or a coloured age would silently
        // stop repainting.
        let styled: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.spaces]
rows = [[{ token = "state_age", fg = "#89b4fa" }]]
"##,
        )
        .expect("styled state_age config");
        assert!(styled.ui.sidebar.spaces.uses_state_age());

        // Serializing back out keeps the name the config file used.
        let round_trip = toml::to_string(&config.ui.sidebar).expect("serialize");
        assert!(round_trip.contains("state_age"), "{round_trip}");
    }

    #[test]
    fn parses_builtin_and_arbitrary_custom_tokens() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.agents]
rows = [["state_icon", "workspace"], ["state_text", "agent", "$summary"], ["terminal_title", "terminal_title_stripped", "$terminal_title"]]
row_gap = 1

[ui.sidebar.agents.rows_by_agent]
claude = [["terminal_title_stripped"], ["agent", "$model"]]

[ui.sidebar.spaces]
rows = [["workspace"], ["$jj_status"]]
row_gap = 3
"#,
        )
        .expect("sidebar token config");

        assert_eq!(
            config.ui.sidebar.agents.rows[1],
            vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Agent,
                AgentSidebarToken::Custom("summary".into()),
            ]
        );
        assert_eq!(
            config.ui.sidebar.agents.rows[2],
            vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]
        );
        assert_eq!(
            config.ui.sidebar.agents.rows_by_agent["claude"],
            vec![
                vec![AgentSidebarToken::TerminalTitleStripped],
                vec![
                    AgentSidebarToken::Agent,
                    AgentSidebarToken::Custom("model".into()),
                ],
            ]
        );
        assert_eq!(config.ui.sidebar.agents.row_gap, 1);
        assert_eq!(
            config.ui.sidebar.spaces.rows[1],
            vec![SpaceSidebarToken::Custom("jj_status".into())]
        );
        assert_eq!(config.ui.sidebar.spaces.row_gap, 3);
    }

    #[test]
    fn space_rows_parse_terminal_title_builtins_distinctly_from_custom_tokens() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.spaces]
rows = [["workspace"], ["terminal_title", "terminal_title_stripped", "$terminal_title"]]
"#,
        )
        .expect("space terminal title config");

        assert_eq!(
            config.ui.sidebar.spaces.rows[1],
            vec![
                SpaceSidebarToken::TerminalTitle,
                SpaceSidebarToken::TerminalTitleStripped,
                SpaceSidebarToken::Custom("terminal_title".into()),
            ]
        );
        assert!(config.ui.sidebar.spaces.uses_terminal_title());
        assert!(!config.ui.sidebar.agents.uses_terminal_title());
    }

    #[test]
    fn terminal_title_space_tokens_round_trip_through_serialization() {
        let rows = vec![vec![
            SpaceSidebarToken::TerminalTitle,
            SpaceSidebarToken::Styled {
                token: Box::new(SpaceSidebarToken::TerminalTitleStripped),
                style: SidebarTokenStyle {
                    bold: Some(true),
                    ..Default::default()
                },
            },
        ]];
        let config = SpacesSidebarConfig {
            rows: rows.clone(),
            ..Default::default()
        };

        let encoded = toml::to_string(&config).expect("serialize spaces config");
        let decoded: SpacesSidebarConfig = toml::from_str(&encoded).expect("round trip");

        assert_eq!(decoded.rows, rows);
    }

    #[test]
    fn default_space_and_agent_layouts_do_not_use_terminal_titles() {
        let config = SidebarConfig::default();

        assert!(!config.spaces.uses_terminal_title());
        assert!(!config.agents.uses_terminal_title());
    }

    #[test]
    fn parses_occurrence_styles_without_changing_plain_tokens() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [[{ token = "workspace", fg = "#abc", bold = false }, "workspace"], [{ token = "$summary", dim = false }]]

[ui.sidebar.agents.rows_by_agent]
claude = [[{ token = "agent", fg = "#112233", bold = true, dim = false }]]

[ui.sidebar.spaces]
rows = [[{ token = "git_status", fg = "#ff00aa" }], [{ token = "$jj", bold = true }]]
"##,
        )
        .unwrap();

        let (token, style) = config.ui.sidebar.agents.rows[0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Workspace);
        assert_eq!(style.bold, Some(false));
        assert_eq!(
            style.fg.unwrap().ratatui(),
            ratatui::style::Color::Rgb(0xaa, 0xbb, 0xcc)
        );
        assert_eq!(
            config.ui.sidebar.agents.rows[0][1],
            AgentSidebarToken::Workspace
        );

        let (token, style) = config.ui.sidebar.agents.rows_by_agent["claude"][0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Agent);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.dim, Some(false));

        let (token, style) = config.ui.sidebar.spaces.rows[0][0].parts();
        assert_eq!(token, &SpaceSidebarToken::GitStatus);
        assert_eq!(
            style.fg.unwrap().ratatui(),
            ratatui::style::Color::Rgb(0xff, 0x00, 0xaa)
        );
        let (token, style) = config.ui.sidebar.spaces.rows[1][0].parts();
        assert_eq!(token, &SpaceSidebarToken::Custom("jj".into()));
        assert_eq!(style.bold, Some(true));
    }

    #[test]
    fn rejects_invalid_occurrence_styles() {
        for entry in [
            r##"{ token = "workspace", fg = "red" }"##,
            r##"{ token = "workspace", fg = "#abcd" }"##,
            r##"{ token = "workspace", bg = "rebeccapurple" }"##,
            r##"{ token = "workspace", bg = "#12345" }"##,
            r##"{ token = "workspace", italic = "yes" }"##,
            r##"{ token = "workspace", underline = 1 }"##,
            r##"{ token = "workspace", reverse = "on" }"##,
            r##"{ token = "workspace", emphasis = "flash" }"##,
            r##"{ token = "workspace", emphasis = true }"##,
            r##"{ token = "workspace", blink = true }"##,
        ] {
            let input = format!("[ui.sidebar.agents]\nrows = [[{entry}]]\n");
            assert!(
                toml::from_str::<crate::config::Config>(&input).is_err(),
                "accepted {entry}"
            );
        }
    }

    #[test]
    fn parses_every_static_attribute_and_pulse_emphasis() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [[{ token = "$dot", fg = "#a6e3a1", bg = "#181825", bold = true, dim = false, italic = true, underline = true, reverse = true, emphasis = "pulse" }]]

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "none" }]]
"##,
        )
        .expect("full style table");

        let (token, style) = config.ui.sidebar.agents.rows[0][0].parts();
        assert_eq!(token, &AgentSidebarToken::Custom("dot".into()));
        assert_eq!(
            style.fg.expect("fg").ratatui(),
            ratatui::style::Color::Rgb(0xa6, 0xe3, 0xa1)
        );
        assert_eq!(
            style.bg.expect("bg").ratatui(),
            ratatui::style::Color::Rgb(0x18, 0x18, 0x25)
        );
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.dim, Some(false));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(true));
        assert_eq!(style.reverse, Some(true));
        assert_eq!(style.emphasis, Some(SidebarTokenEmphasis::Pulse));
        assert!(style.animates());

        let (_, calm) = config.ui.sidebar.spaces.rows[0][0].parts();
        assert_eq!(calm.emphasis, Some(SidebarTokenEmphasis::None));
        assert!(!calm.animates());
    }

    #[test]
    fn invalid_style_fields_report_the_offending_value() {
        let color_error = toml::from_str::<crate::config::Config>(
            "[ui.sidebar.agents]\nrows = [[{ token = \"workspace\", bg = \"red\" }]]\n",
        )
        .expect_err("invalid bg")
        .to_string();
        assert!(
            color_error.contains("#RGB or #RRGGBB") && color_error.contains("red"),
            "unhelpful color error: {color_error}"
        );

        let emphasis_error = toml::from_str::<crate::config::Config>(
            "[ui.sidebar.agents]\nrows = [[{ token = \"workspace\", emphasis = \"flash\" }]]\n",
        )
        .expect_err("invalid emphasis")
        .to_string();
        assert!(
            emphasis_error.contains("flash")
                && emphasis_error.contains("pulse")
                && emphasis_error.contains("none"),
            "unhelpful emphasis error: {emphasis_error}"
        );

        let unknown_error = toml::from_str::<crate::config::Config>(
            "[ui.sidebar.agents]\nrows = [[{ token = \"workspace\", blink = true }]]\n",
        )
        .expect_err("unknown field")
        .to_string();
        assert!(
            unknown_error.contains("blink"),
            "unhelpful unknown-field error: {unknown_error}"
        );
    }

    #[test]
    fn only_pulse_emphasis_arms_the_animation_clock() {
        let calm: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [[{ token = "workspace", fg = "#abc", bold = true, italic = true, underline = true, reverse = true }], ["agent"]]

[ui.sidebar.spaces]
rows = [[{ token = "branch", emphasis = "none" }]]
"##,
        )
        .expect("calm config");
        assert!(!calm.ui.sidebar.agents.has_animated_tokens());
        assert!(!calm.ui.sidebar.spaces.has_animated_tokens());

        let default = SidebarConfig::default();
        assert!(!default.agents.has_animated_tokens());
        assert!(!default.spaces.has_animated_tokens());

        let spaces_pulse: crate::config::Config = toml::from_str(
            "[ui.sidebar.spaces]\nrows = [[{ token = \"branch\", emphasis = \"pulse\" }]]\n",
        )
        .expect("space pulse");
        assert!(spaces_pulse.ui.sidebar.spaces.has_animated_tokens());
        assert!(!spaces_pulse.ui.sidebar.agents.has_animated_tokens());

        let override_pulse: crate::config::Config = toml::from_str(
            "[ui.sidebar.agents.rows_by_agent]\nclaude = [[{ token = \"agent\", emphasis = \"pulse\" }]]\n",
        )
        .expect("override pulse");
        assert!(override_pulse.ui.sidebar.agents.has_animated_tokens());
    }

    #[test]
    fn styles_without_new_attributes_serialize_exactly_as_before() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [["state_icon", { token = "workspace", fg = "#89b4fa", bold = true, dim = false }]]
"##,
        )
        .expect("legacy style config");

        let serialized = toml::to_string(&config.ui.sidebar.agents).expect("serialize agents");
        assert!(
            serialized.contains("fg = \"#89b4fa\"")
                && serialized.contains("bold = true")
                && serialized.contains("dim = false"),
            "legacy style lost an attribute: {serialized}"
        );
        for absent in ["bg", "italic", "underline", "reverse", "emphasis"] {
            assert!(
                !serialized.contains(absent),
                "{absent} leaked into a legacy style: {serialized}"
            );
        }

        let reparsed: AgentsSidebarConfig = toml::from_str(&serialized).expect("roundtrip agents");
        assert_eq!(reparsed, config.ui.sidebar.agents);
    }

    #[test]
    fn rejects_unknown_bare_and_malformed_custom_tokens() {
        for token in ["summary", "$", "$bad.name"] {
            let input = format!("[ui.sidebar.agents]\\nrows = [[\"{token}\"]]\\n");
            assert!(toml::from_str::<crate::config::Config>(&input).is_err());
        }
    }

    #[test]
    fn rejects_oversized_sidebar_layouts() {
        let too_many_rows = std::iter::repeat_n("[\"agent\"]", MAX_SIDEBAR_ROWS + 1)
            .collect::<Vec<_>>()
            .join(",");
        let input = format!("[ui.sidebar.agents]\nrows = [{too_many_rows}]\n");
        assert!(toml::from_str::<crate::config::Config>(&input).is_err());

        let too_many_tokens = std::iter::repeat_n("\"workspace\"", MAX_SIDEBAR_TOKENS_PER_ROW + 1)
            .collect::<Vec<_>>()
            .join(",");
        let input = format!("[ui.sidebar.spaces]\nrows = [[{too_many_tokens}]]\n");
        assert!(toml::from_str::<crate::config::Config>(&input).is_err());

        let input = format!("[ui.sidebar.agents.rows_by_agent]\nclaude = [{too_many_rows}]\n");
        assert!(toml::from_str::<crate::config::Config>(&input).is_err());
    }

    #[test]
    fn accepts_every_canonical_agent_override_key() {
        let agents = [
            Agent::Pi,
            Agent::Claude,
            Agent::Codex,
            Agent::Gemini,
            Agent::Cursor,
            Agent::Devin,
            Agent::Antigravity,
            Agent::Cline,
            Agent::Omp,
            Agent::Mastracode,
            Agent::OpenCode,
            Agent::GithubCopilot,
            Agent::Kimi,
            Agent::Kiro,
            Agent::Droid,
            Agent::Amp,
            Agent::Grok,
            Agent::Hermes,
            Agent::Kilo,
            Agent::Qodercli,
            Agent::Maki,
        ];
        let entries = agents
            .iter()
            .map(|agent| format!("{} = [[\"agent\"]]", crate::detect::agent_label(*agent)))
            .collect::<Vec<_>>()
            .join("\n");
        let input = format!("[ui.sidebar.agents.rows_by_agent]\n{entries}\n");
        let config: crate::config::Config = toml::from_str(&input).expect("canonical keys");

        assert_eq!(config.ui.sidebar.agents.rows_by_agent.len(), agents.len());
    }

    fn view_config(body: &str) -> AgentsViewConfig {
        let config: crate::config::Config =
            toml::from_str(&format!("[ui.sidebar.agents.view]\n{body}"))
                .expect("a malformed view must not fail the whole config load");
        config.ui.sidebar.agents.view
    }

    #[test]
    fn no_declared_view_leaves_the_panel_alone() {
        let config = AgentsSidebarConfig::default();
        assert_eq!(config.view, AgentsViewConfig::default());
        assert!(config.view.declared_view().is_none());
        assert!(config.view.diagnostics().is_none());
    }

    #[test]
    fn parses_the_same_filter_and_sort_grammar_as_the_socket_api() {
        let view = view_config(
            r#"
label = "attention"
filter = { op = "all", filters = [
  { op = "in", field = "status", values = ["working", "blocked"] },
  { op = "not", filter = { op = "eq", field = "seen", value = true } },
  { op = "exists", field = { token = "model" } },
] }
sort = [
  { field = "attention", order = "desc" },
  { field = { token = "model" } },
]
"#,
        );

        assert!(view.diagnostics().is_none(), "{:?}", view.diagnostics());
        let declared = view.declared_view().expect("declared view");
        assert_eq!(declared.source, crate::agent_view::CONFIG_VIEW_SOURCE);
        assert_eq!(declared.label.as_deref(), Some("attention"));
        assert_eq!(declared.sort.len(), 2);
        assert!(matches!(
            declared.filter,
            Some(crate::api::schema::AgentViewFilter::All { ref filters }) if filters.len() == 3
        ));
    }

    #[test]
    fn a_context_filter_survives_the_config_front_door() {
        let view = view_config(
            "filter = { op = \"eq\", field = \"workspace_id\", value = { context = \"current_workspace_id\" } }\n",
        );

        assert!(view.diagnostics().is_none(), "{:?}", view.diagnostics());
        assert!(view.declared_view().is_some());
    }

    #[test]
    fn an_invalid_view_is_dropped_and_named_in_a_diagnostic() {
        // Each case must leave the panel unfiltered: an empty Agents panel that
        // really means "your config is broken" is the failure to design against.
        for (body, expected) in [
            (
                "filter = { op = \"eq\", field = \"status\", value = \"workin\" }\n",
                "unknown agent status `workin`",
            ),
            (
                "filter = { op = \"eq\", field = \"statuss\", value = \"working\" }\n",
                "`field = \"statuss\"`",
            ),
            (
                "filter = { op = \"equals\", field = \"status\", value = \"working\" }\n",
                "unknown variant `equals`",
            ),
            (
                "filter = { op = \"any\", filters = [] }\n",
                "all/any filters must not be empty",
            ),
            (
                "sort = [{ field = \"attension\" }]\n",
                "`field = \"attension\"`",
            ),
            (
                "filterr = { op = \"exists\", field = \"agent\" }\n",
                "filterr",
            ),
            ("label = \"orphan\"\n", "declares no filter and no sort"),
        ] {
            let view = view_config(body);
            let diagnostic = view
                .diagnostics()
                .unwrap_or_else(|| panic!("expected a diagnostic for {body}"));
            assert!(
                diagnostic.contains(expected),
                "{body} produced {diagnostic}"
            );
            assert!(
                diagnostic.contains("[ui.sidebar.agents.view]"),
                "{diagnostic}"
            );
            assert!(diagnostic.contains("showing every agent"), "{diagnostic}");
            assert!(view.declared_view().is_none(), "{body} was still applied");
        }
    }

    /// The snippet printed by `herdr --default-config` and shown in the docs.
    /// If it stops parsing, the documentation is lying.
    #[test]
    fn the_documented_example_view_parses() {
        let view = view_config(
            r#"
label = "needs me"
filter = { op = "any", filters = [
  { op = "eq", field = "status", value = "blocked" },
  { op = "eq", field = "seen", value = false },
] }
sort = [{ field = "attention", order = "desc" }]
"#,
        );

        assert!(view.diagnostics().is_none(), "{:?}", view.diagnostics());
        assert!(view.declared_view().is_some());
    }

    #[test]
    fn a_broken_view_does_not_take_the_rest_of_the_config_with_it() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui]
agent_panel_sort = "priority"

[ui.sidebar.agents]
row_gap = 1

[ui.sidebar.agents.view]
filter = { op = "eq", field = "nope", value = "working" }
"#,
        )
        .expect("config load");

        assert_eq!(config.ui.sidebar.agents.row_gap, 1);
        assert_eq!(
            config.ui.agent_panel_sort,
            crate::config::AgentPanelSortConfig::Priority
        );
        assert!(config
            .collect_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.contains("[ui.sidebar.agents.view]")));
    }

    #[test]
    fn rejects_alias_case_whitespace_and_unknown_override_keys() {
        for key in ["claude-code", "Claude", "' claude '", "unknown"] {
            let input = format!("[ui.sidebar.agents.rows_by_agent]\n{key} = [[\"agent\"]]\n");
            assert!(
                toml::from_str::<crate::config::Config>(&input).is_err(),
                "accepted key {key:?}"
            );
        }
    }
}
