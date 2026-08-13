use serde::{Deserialize, Serialize};

use super::agents::{AgentInfo, AgentViewInfo};
use super::panes::{PaneInfo, PaneLayoutSnapshot};
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    /// The Agents view in force, if any. Absent when nothing is filtering or
    /// reordering the agent list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_view: Option<AgentViewInfo>,
    /// The session status string in force, if any. Absent when nothing has set
    /// one, which is the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Whether the persistent whole-terminal background scene is drawing, and
    /// when it is not, which of its conditions is the one that is unmet.
    #[serde(default)]
    pub background_scene: BackgroundSceneInfo,
    /// The host machine's own state — CPU, memory, swap and load — with its
    /// recent past. See [`MachineRegisterInfo`].
    #[serde(default)]
    pub machine_register: MachineRegisterInfo,
}

/// The persistent whole-terminal background scene's live state, condition by
/// condition.
///
/// Every condition is reported separately rather than rolled into `active`
/// alone, because the scene fails *silently*: an unmet condition draws nothing
/// and says nothing, and three of the five are facts about the viewer's
/// terminal that no amount of reading the config can reveal. A caller that
/// wants the one-line answer reads [`Self::active`]; a caller asking why it is
/// false reads the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BackgroundSceneInfo {
    /// The scene is being drawn. True exactly when every other condition here
    /// that gates it is met.
    pub active: bool,
    /// `[experimental] persistent_background` in config.toml.
    pub enabled: bool,
    /// `[experimental] kitty_graphics` in config.toml. The scene is a Kitty
    /// Graphics surface, so this gates it too.
    pub kitty_graphics_enabled: bool,
    /// The host terminal answered the Kitty Graphics capability probe (`a=q`).
    /// Reported for diagnosis rather than as a gate: it is what every *other*
    /// pixel surface requires, so a terminal that fails it will draw no cards
    /// or tray art either.
    pub kitty_graphics_capability_confirmed: bool,
    /// What the host terminal identified itself as, in band, over the pty
    /// (`kitty`, `rio`, or `other` for one Herdr could not positively name).
    /// Over an SSH hop this is the only source that survives.
    pub host_terminal: String,
    /// The host terminal is one Herdr has positively identified as drawing an
    /// opaque ambient wash *under* the text rather than over it.
    pub host_draws_ambient_wash: bool,
    /// Every other attached viewer draws one too. The scene is a single shared
    /// image placed for all of them, so one viewer that would composite it over
    /// its own text withholds it from everybody.
    pub every_viewer_draws_ambient_wash: bool,
    /// How many second mates the scene's orbit ring can seat at once.
    ///
    /// A composition number rather than a limit found by measurement: the ring's
    /// spacing is what the whole field is built on, so its capacity is decided
    /// rather than discovered.
    /// `#[serde(default)]` on all three, matching this schema's own convention
    /// for fields added after the fact: a newer CLI asking an older server for a
    /// purely informational readout should report zero rather than fail to parse
    /// the whole response.
    #[serde(default)]
    pub ladder_capacity: u32,
    /// How many second mates are currently seated on it.
    #[serde(default)]
    pub mates_seated: u32,
    /// How many second mates the fleet has that the ring had no slot for.
    ///
    /// Reported rather than dropped in silence: a scene that quietly shows fewer
    /// bodies than the fleet has is lying by omission. The losers are the
    /// *smallest* by tracked files at HEAD — the same register the scene already
    /// draws each body's size from — so this number is arguable rather than
    /// mysterious. Zero whenever the fleet fits.
    #[serde(default)]
    pub mates_beyond_ladder: u32,
    /// How much of the main area — the frame outside the sidebar — carries no
    /// interface element over it, `0.0..=1.0`.
    ///
    /// The composition bound the whole scene exists to satisfy: if the interface
    /// crowds the sky out, the scene is not worth drawing. Pane text does not
    /// count against it, and that is a real distinction: the scene is an opaque
    /// wash placed *under* the text with no pane background of its own, so a
    /// terminal region is ink on the scene rather than a panel over it. What
    /// counts is anything that puts a surface between the reader and the sky.
    #[serde(default)]
    pub sky_clear_fraction: f32,
    /// The floor [`Self::sky_clear_fraction`] is held to.
    #[serde(default)]
    pub sky_clear_floor: f32,
    /// Ambient events consumed, and ambient motes emitted, since this session
    /// started.
    ///
    /// **These two are equal, always.** Every mote the scene's ambient tier
    /// draws traces to one unit of work a body's own agent actually did; none
    /// is emitted by a timer, a loop, or a decorative oscillator. Published as
    /// a pair rather than as one number precisely so the equality is
    /// inspectable on a running session rather than only in a test.
    #[serde(default)]
    pub ambient_events_consumed: u64,
    #[serde(default)]
    pub ambient_motes_emitted: u64,
}

/// The host machine's own state: CPU aggregate and per core, memory, swap and
/// load average, each with its recent past.
///
/// A register about the **substrate** rather than about the work — the machine
/// the fleet is running on, not the fleet. Published here rather than only
/// drawn, for two reasons: it is a shared runtime fact rather than one client's
/// presentation, and the drawn form is deliberately wordless (see the corner's
/// own doc), so this is where the numbers can be read as numbers.
///
/// **Nothing here is ever fabricated.** On a platform this build does not read,
/// before a second sample has landed, or once the newest sample has gone stale,
/// every value is absent and [`Self::absent_because`] says which of those it is.
/// A plausible number invented from nothing is worse than an empty readout that
/// says why it is empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MachineRegisterInfo {
    /// There is a current reading. False whenever [`Self::absent_because`] is
    /// set, and the two are never both meaningful at once.
    pub reading: bool,
    /// Why there is no reading, in the readout's own words. Absent when there
    /// is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent_because: Option<String>,
    /// The files these numbers were read from. A reader who wants to check a
    /// number has to be told where to check it.
    #[serde(default)]
    pub sources: Vec<String>,
    /// How often the register samples, in milliseconds.
    #[serde(default)]
    pub sample_interval_ms: u64,
    /// How old the newest sample is, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_sample_age_ms: Option<u64>,
    /// Each quantity's current value and how much history is held for it.
    #[serde(default)]
    pub quantities: Vec<MachineQuantityInfo>,
    /// Each logical CPU's current busy fraction, in the operating system's own
    /// core order. A core that reported nothing is `null` — drawn absent rather
    /// than at zero, and never dropped, since dropping it would silently
    /// re-number every core after it.
    #[serde(default)]
    pub cores: Vec<Option<f32>>,
}

/// One quantity of [`MachineRegisterInfo`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MachineQuantityInfo {
    /// `cpu`, `mem`, `swap` or `load`.
    pub name: String,
    /// The newest reading, `0.0..=1.0`. Absent when nothing has been measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f32>,
    /// How many samples of recent past are held for it.
    #[serde(default)]
    pub history_samples: u32,
}

/// Params for `session.status.set`.
///
/// The status is one caller-supplied line of text about this session. Herdr
/// never interprets it and never composes it: whatever the caller publishes is
/// what a client shows, so the same slot carries a quota readout, a build
/// number, or a deploy banner without Herdr learning about any of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionStatusSetParams {
    pub status: String,
}
