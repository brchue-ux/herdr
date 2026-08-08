//! What one cell of an animated element looks like on one frame.
//!
//! The medium is a character-cell grid, so this type is the whole of what an
//! animation is allowed to say about a cell: its foreground, its background,
//! its attributes, how much of its own glyph is present, and — for a cell that
//! is pure decoration — which glyph that is. A glyph never moves, stretches, or
//! leaves its cell. That is a property of the terminal, not a gap in this type,
//! and it is why nothing here has a position delta.
//!
//! Four properties this module is responsible for holding:
//!
//! - **A cell paint is a patch, never a replacement.** Every field is optional
//!   or a tri-state, so an animation that says nothing about a cell leaves it
//!   exactly as the settled rendering drew it. Dropping every frame of an
//!   animation must leave the element identical to its unanimated self.
//! - **Colour is resolved once, in RGB.** Behaviours name inks symbolically
//!   ([`Ink`]) and a caller resolves them against the palette it is already
//!   drawing with, so the same behaviour reads correctly on a light theme, a
//!   dark theme, and whatever the host terminal actually reported.
//! - **Sub-cell resolution comes from the glyph set or not at all.** Text cells
//!   express coverage as a mix toward the background, because a letter cannot
//!   be half-drawn; filled cells express it through the eighth-block ramp,
//!   which genuinely resolves eight steps inside one cell. A decoration cell
//!   gets a third option: [`CellPaint::glyph`] swaps in a glyph of the *same
//!   display width*, which is how an effect resolves a position finer than a
//!   cell — or takes a shape no styling of the settled glyph could express.
//! - **A glyph substitution is an offer, not a command.** [`CellPaint::glyph`]
//!   is honoured only by a call site drawing pure decoration, through
//!   [`CellPaint::glyph_over`], which refuses any substitute whose display
//!   width differs from the glyph it would replace. [`CellPaint::text_style`]
//!   never applies one at all: an animation must not be able to garble a label
//!   it was only asked to emphasise.

use ratatui::style::{Modifier, Style};

use crate::ui::color::{mix_rgb, resolve_color_rgb, Rgb};

/// Where a cell sits inside the element being animated.
///
/// Element-relative, never screen-relative: an element that moves because the
/// sidebar scrolled or a pane was resized keeps painting the same way, and no
/// animation state has to be rewritten when geometry changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellPos {
    pub(crate) col: u16,
    pub(crate) row: u16,
}

impl CellPos {
    pub(crate) fn new(col: u16, row: u16) -> Self {
        Self { col, row }
    }

    /// The single-row case: a token span, a status glyph, a connector.
    pub(crate) fn col(col: u16) -> Self {
        Self { col, row: 0 }
    }
}

/// The element's own cell grid.
///
/// A token span is `cols × 1`; a pane's content area is its full rect. Both go
/// through the same field maths, which is what keeps a behaviour written for
/// one usable on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellExtent {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl CellExtent {
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// A single row of `cols` cells.
    pub(crate) fn row(cols: u16) -> Self {
        Self { cols, rows: 1 }
    }

    /// Normalised position of `pos` along each axis, in `0.0..=1.0`.
    ///
    /// A one-cell axis normalises to `0.0` rather than dividing by zero, so a
    /// sweep across a one-column element is simply uniform.
    pub(crate) fn normalize(self, pos: CellPos) -> (f32, f32) {
        fn axis(index: u16, extent: u16) -> f32 {
            let last = extent.saturating_sub(1);
            if last == 0 {
                return 0.0;
            }
            f32::from(index.min(last)) / f32::from(last)
        }
        (axis(pos.col, self.cols), axis(pos.row, self.rows))
    }

    pub(crate) fn is_empty(self) -> bool {
        self.cols == 0 || self.rows == 0
    }
}

/// Attribute changes an animation makes to a cell.
///
/// Tri-state per attribute for the same reason the sidebar's own token styling
/// is: `None` means "whatever the settled rendering decided", which is not the
/// same as "off".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AttrPatch {
    pub(crate) bold: Option<bool>,
    pub(crate) dim: Option<bool>,
    pub(crate) italic: Option<bool>,
    pub(crate) underline: Option<bool>,
    pub(crate) reverse: Option<bool>,
}

impl AttrPatch {
    pub(crate) const NONE: Self = Self {
        bold: None,
        dim: None,
        italic: None,
        underline: None,
        reverse: None,
    };

    pub(crate) const fn bold() -> Self {
        Self {
            bold: Some(true),
            ..Self::NONE
        }
    }

    pub(crate) const fn dim() -> Self {
        Self {
            dim: Some(true),
            ..Self::NONE
        }
    }

    pub(crate) const fn reverse() -> Self {
        Self {
            reverse: Some(true),
            ..Self::NONE
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self == Self::NONE
    }

    fn apply(self, mut style: Style) -> Style {
        fn set(style: Style, value: Option<bool>, modifier: Modifier) -> Style {
            match value {
                Some(true) => style.add_modifier(modifier),
                Some(false) => style.remove_modifier(modifier),
                None => style,
            }
        }
        style = set(style, self.bold, Modifier::BOLD);
        style = set(style, self.dim, Modifier::DIM);
        style = set(style, self.italic, Modifier::ITALIC);
        style = set(style, self.underline, Modifier::UNDERLINED);
        set(style, self.reverse, Modifier::REVERSED)
    }
}

/// A colour named by role rather than by value.
///
/// Behaviours are written against roles so one definition works on every theme.
/// The caller resolves them with [`InkPalette`] from whatever it is already
/// drawing with, which is also what keeps the host terminal's *measured*
/// palette authoritative rather than a second static table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    /// The surface this element composites against.
    Surface,
    /// The element's own settled foreground.
    Own,
    /// The palette accent.
    Accent,
    /// The colour of *what is being signalled* on this element right now.
    ///
    /// A role rather than a hue so one behaviour can carry a whole vocabulary:
    /// the caller resolves it from the category of the thing it is drawing —
    /// work arriving, work finishing, a failure, a branch going quiet — and the
    /// same catalogue entry then reads as four different signals.
    ///
    /// The role is resolved from *two* facts, not one: which [`LifecycleStage`]
    /// the work is at, which decides the hue, and how bad the problem on it is
    /// ([`Severity`]), which decides how far off the surface it stands. See
    /// [`InkPalette::with_signal`].
    Signal,
    /// A literal colour, for a behaviour that genuinely means one hue.
    Fixed(Rgb),
}

/// Which stage of its own life the work behind an element is at.
///
/// **This carries hue and nothing else.** The five are the stages a unit of work
/// passes through, not five degrees of anything: a card at any one of them can
/// be perfectly healthy or in serious trouble, and saying which is
/// [`Severity`]'s job. Keeping them on separate channels is what makes "running,
/// but badly" expressible at all — with one channel it collapses into either a
/// stage that is really a warning or a warning that is really a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum LifecycleStage {
    /// Accepted, nothing running yet.
    Queued,
    /// Work is happening.
    Running,
    /// Stopped on something outside itself — usually a person.
    Waiting,
    /// Finished the work it was given.
    Done,
    /// Did not finish the work it was given.
    Failed,
}

impl LifecycleStage {
    /// The palette role this stage takes its hue from.
    ///
    /// Roles rather than literal hues, because the visual target's *theme
    /// blending* condition is explicit that state colours "must sit inside the
    /// active theme". Four of the five are the role the palette already
    /// documents itself as meaning — `yellow` is commented *"Working / running
    /// states"*, `green` *"Done / idle states"*, `red` *"Needs attention"*,
    /// `peach` *"Interrupted / warning states"*. `Queued` takes `teal`, the
    /// coolest chromatic role, because a queue is the absence of demand.
    pub(crate) fn role(self, p: &crate::app::state::Palette) -> ratatui::style::Color {
        match self {
            Self::Queued => p.teal,
            Self::Running => p.yellow,
            Self::Waiting => p.mauve,
            Self::Done => p.green,
            Self::Failed => p.red,
        }
    }

    /// The hue this stage falls back to when the theme's own role has none.
    ///
    /// A monochrome or near-monochrome theme supplies a role whose hue angle is
    /// decided by a channel of rounding (see
    /// [`crate::ui::color::hue_is_meaningful`]). Reading it anyway would collapse
    /// the whole vocabulary onto one or two accidental angles, which is the one
    /// failure this channel cannot survive — so the stated angles stand in. They
    /// are the Catppuccin Mocha roles' own angles, which is where the default
    /// theme puts them.
    fn fallback_hue(self) -> f32 {
        match self {
            Self::Queued => 170.0,
            Self::Running => 41.0,
            Self::Waiting => 267.0,
            Self::Done => 115.0,
            Self::Failed => 343.0,
        }
    }

    /// This stage's hue angle under the palette and the host's own colours.
    pub(crate) fn hue(
        self,
        p: &crate::app::state::Palette,
        host: &crate::terminal_theme::TerminalTheme,
    ) -> f32 {
        crate::ui::color::resolve_color_rgb(self.role(p), host)
            .filter(|rgb| crate::ui::color::hue_is_meaningful(*rgb))
            .map_or_else(
                || self.fallback_hue(),
                |rgb| crate::ui::color::to_hsl(rgb).0,
            )
    }

    /// Every stage, in lifecycle order. For a legend, a matrix, or a test.
    pub(crate) const ALL: [Self; 5] = [
        Self::Queued,
        Self::Running,
        Self::Waiting,
        Self::Done,
        Self::Failed,
    ];
}

/// How bad the problem on an element is, whatever stage it is at.
///
/// **This carries intensity and nothing else.** It never touches hue, because
/// hue is spoken for: a card that got louder because it is in trouble must not
/// also look like it changed what it is doing.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub(crate) enum Severity {
    /// Nothing wrong. The stage speaking on its own.
    #[default]
    Clear,
    /// Something worth knowing about, nothing that stops the work.
    Mild,
    /// A real problem. The work is compromised.
    Serious,
    /// The loudest thing this vocabulary can say.
    Critical,
}

/// How far each severity stands off the surface, as a fraction of the room
/// there is between the surface and the end of the scale it is heading for.
///
/// # Why lightness distance and not the WCAG contrast ratio
///
/// The obvious metric is contrast, and it is wrong here for a reason the
/// vocabulary's own floor already recorded: contrast is defined on *relative
/// luminance*, and equal luminance across hues is unreachable without washing
/// the dark ones toward white. Pure blue tops out at 2.2:1 on a near-black panel
/// where pure green reaches 13.9:1, so normalising the two to one ratio means
/// whitening the blue until it is no longer blue — "destroying exactly the hue
/// separation the vocabulary exists to carry", which is the thing the hue
/// channel cannot survive.
///
/// Lightness distance has no such cost. `L` is hue-agnostic by construction, so
/// every hue reaches every step of this ramp at full saturation and none of them
/// is bleached to get there.
///
/// # Why a fraction of the headroom rather than an absolute distance
///
/// Because a mid-grey panel has half the room a dark one does, and an absolute
/// ramp run against it clamps — collapsing the two loudest severities onto the
/// same ink, which is the one failure this channel cannot have. Scaling to the
/// headroom keeps four distinct steps on every possible surface, and it stays a
/// pure function of the severity and the theme, never of the stage.
///
/// # It is still legible without colour
///
/// Within one hue, luminance is monotone in `L`, so the four steps are four
/// steps in greyscale as well — see
/// `severity_is_still_four_steps_in_greyscale`. What is given up is
/// *cross-hue* luminance comparison, which nothing here asks for: severity is
/// read against the same card a moment earlier, not against a different card of
/// a different stage.
const SEVERITY_LIGHT_REACH: [f32; 4] = [0.38, 0.56, 0.74, 0.92];

/// How near black or white a signal ink may be placed.
///
/// A hue at either extreme is not a hue, so this is where the ramp's headroom is
/// measured to rather than a clamp applied afterwards.
const SIGNAL_LIGHT_BOUNDS: (f32, f32) = (0.10, 0.92);

/// Saturation every signal ink is drawn at, whatever its severity.
///
/// **One value and not a ramp, and that is a finding rather than a
/// simplification.** Saturation was a second intensity cue here until it was
/// measured: raising it moves a colour's *maximum* channel up, which brightens
/// the ink on a dark panel — the direction wanted — and brightens it on a light
/// one too, which is the direction not wanted. On a light theme a luminance-heavy
/// hue then gets louder and quieter at once and the two cancel; green stopped
/// being four distinguishable steps entirely. Lightness distance alone is
/// monotone on every theme, so it carries the channel by itself and saturation
/// is held still.
///
/// At the reference's own S 60–65% for an active card, so a signal sits in the
/// family the cards were sampled from rather than louder than all of them.
const SIGNAL_SATURATION: f32 = 0.66;

impl Severity {
    fn index(self) -> usize {
        match self {
            Self::Clear => 0,
            Self::Mild => 1,
            Self::Serious => 2,
            Self::Critical => 3,
        }
    }

    /// How far off the surface an element at this severity stands, as a
    /// fraction of the room between the surface and the end of the scale.
    pub(crate) fn light_reach(self) -> f32 {
        SEVERITY_LIGHT_REACH[self.index()]
    }

    /// Position on the ramp, in `0.0..=1.0`, for a consumer that wants the
    /// severity as an amount rather than as two colour numbers.
    pub(crate) fn amount(self) -> f32 {
        self.index() as f32 / (SEVERITY_LIGHT_REACH.len() - 1) as f32
    }

    /// True when this severity is loud enough to change how an element *moves*
    /// rather than only how it looks.
    ///
    /// The escalation threshold, and the reason it exists: colour is a poor
    /// carrier of type on its own, so past this point the element climbs the
    /// behavioural ladder the tray badges and the card breath already use, and
    /// says it in rhythm as well as in light.
    pub(crate) fn escalates(self) -> bool {
        matches!(self, Self::Serious | Self::Critical)
    }

    /// Every severity, quietest first. For a legend, a matrix, or a test.
    pub(crate) const ALL: [Self; 4] = [Self::Clear, Self::Mild, Self::Serious, Self::Critical];
}

/// The ink one stage at one severity resolves to, over `surface`.
///
/// The two channels meet here and nowhere else. The stage supplies a hue angle
/// and stops; the severity supplies a saturation and a distance from the
/// surface and stops. Neither can reach into the other's number, which is not a
/// convention this function follows but the only thing it does.
///
/// The hue is handed straight to [`crate::ui::color::from_hsl`] and nothing
/// downstream may rotate it, so this is genuinely orthogonal rather than
/// approximately so.
pub(crate) fn signal_ink(hue: f32, severity: Severity, surface: Rgb) -> Rgb {
    crate::ui::color::from_hsl(hue, SIGNAL_SATURATION, signal_light(severity, surface))
}

/// The lightness one severity is placed at over `surface`.
///
/// Split out from [`signal_ink`] because it is the severity channel's whole
/// observable, and a test that asserts the channel is independent of the stage
/// should be able to ask for it without going through a hue.
pub(crate) fn signal_light(severity: Severity, surface: Rgb) -> f32 {
    signal_light_at_reach(severity.light_reach(), surface)
}

/// How far off the surface the defect marker at full intensity stands.
///
/// [`Severity::Serious`]'s own reach, and it is a *measured* ceiling rather than
/// a rounded-down maximum. The ramp pushes an ink toward the light bound as it
/// escalates, and at [`Severity::Critical`]'s reach a live render washed the
/// marker to a pale rose on a dark panel instead of reading as a real signal —
/// see [`crate::ui::sidebar::render_failure_spiders`], which held this same
/// value as a hard-coded severity before the fleet had a severity to give it.
/// The intensity steps are fractions *of this*, so the loudest one lands exactly
/// on the ink that was validated on screen.
pub(crate) const MARKER_FULL_REACH: f32 = SEVERITY_LIGHT_REACH[2];

/// The ink a defect marker draws at, at `intensity` of its full loudness.
///
/// The continuous sibling of [`signal_ink`], and it exists for a channel the
/// four-step [`Severity`] ladder cannot spell: the fleet's own S1–S4 defect
/// ladder places its steps at 25/50/75/100% of full intensity
/// (see [`crate::quality_streak::DefectSeverity::intensity`]), which is a
/// *proportion* of the reach rather than four hand-placed points on it.
///
/// The orthogonality rule is the same one [`signal_ink`] holds and is the reason
/// this takes two arguments rather than one: `hue` comes from the row's
/// [`LifecycleStage`] and `intensity` from its severity, and neither may reach
/// into the other's number. A row that moves from `Running` to `Failed` changes
/// hue and nothing else; a defect that is restated from S3 to S1 changes
/// intensity and nothing else.
pub(crate) fn marker_ink(hue: f32, intensity: f32, surface: Rgb) -> Rgb {
    let reach = MARKER_FULL_REACH * intensity.clamp(0.0, 1.0);
    crate::ui::color::from_hsl(
        hue,
        SIGNAL_SATURATION,
        signal_light_at_reach(reach, surface),
    )
}

/// The lightness an ink at `reach` is placed at over `surface`.
///
/// The one place the ramp's direction and headroom are decided, so
/// [`signal_light`]'s four fixed steps and [`marker_ink`]'s continuous ones
/// cannot drift apart.
fn signal_light_at_reach(reach: f32, surface: Rgb) -> f32 {
    let (low, high) = SIGNAL_LIGHT_BOUNDS;
    let ground = crate::ui::color::to_hsl(surface).2;
    // Away from the panel, whichever way that is. The same rule
    // [`crate::ui::color::ensure_contrast`] picks its direction by, so a light
    // theme darkens where a dark theme lightens rather than both brightening.
    if ground >= 0.5 {
        ground - (ground - low).max(0.0) * reach
    } else {
        ground + (high - ground).max(0.0) * reach
    }
}

/// The concrete colours [`Ink`] resolves against for one element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InkPalette {
    pub(crate) surface: Rgb,
    pub(crate) own: Rgb,
    pub(crate) accent: Rgb,
    /// What [`Ink::Signal`] resolves to. Defaults to the accent, so a call site
    /// with no category of its own still draws a signal behaviour correctly.
    pub(crate) signal: Rgb,
}

impl InkPalette {
    /// Resolve the three roles from the app palette and the style a call site
    /// was already going to draw with.
    ///
    /// `Color::Reset` and unresolvable indexed colours fall back to the palette
    /// surface, so an element drawn against the host's own background still
    /// animates instead of silently doing nothing.
    ///
    /// `surface` is what the call site's own panel is filled with, for the
    /// surfaces that have a fill of their own — the sidebar's ground is
    /// `palette.sidebar_bg` rather than `panel_bg`, and an element with no
    /// explicit background must composite against the colour on screen under it
    /// rather than against the app-wide panel colour. `None` says the call site
    /// draws on the app's panel background, which is the answer everywhere
    /// outside a panel with its own fill.
    pub(crate) fn resolve(
        base: Style,
        surface: Option<Rgb>,
        palette: &crate::app::state::Palette,
        host: &crate::terminal_theme::TerminalTheme,
    ) -> Self {
        let rgb = |color| resolve_color_rgb(color, host);
        let surface = base
            .bg
            .and_then(rgb)
            .or(surface)
            .or_else(|| rgb(palette.panel_bg))
            .unwrap_or(crate::ui::color::BLACK);
        let accent = rgb(palette.accent).unwrap_or(surface);
        Self {
            surface,
            own: base
                .fg
                .and_then(rgb)
                .or_else(|| rgb(palette.text))
                .unwrap_or(crate::ui::color::WHITE),
            accent,
            signal: accent,
        }
    }

    /// Bind [`Ink::Signal`] to a stage's hue at a severity's intensity.
    ///
    /// The two arguments are the two channels, and taking them separately is the
    /// point: there is no way to spell a signal here that says its stage with
    /// its intensity or its severity with its hue. A caller that knows only what
    /// kind of thing it is drawing passes [`Severity::Clear`] and gets exactly
    /// the vocabulary that shipped before this channel existed.
    ///
    /// Resolved against `self.surface`, so a vocabulary entry that happens to
    /// sit close to the host terminal's background — a muted grey on a grey
    /// theme, a green on a green one — still arrives at the distance its
    /// severity asked for rather than invisible.
    pub(crate) fn with_signal(mut self, hue: f32, severity: Severity) -> Self {
        self.signal = signal_ink(hue, severity, self.surface);
        self
    }

    /// Bind [`Ink::Signal`] to a stage's hue at a defect marker's intensity.
    ///
    /// [`with_signal`](Self::with_signal) for the one caller whose intensity is
    /// a proportion rather than one of the four [`Severity`] steps — see
    /// [`marker_ink`], which is the whole of the difference.
    pub(crate) fn with_marker(mut self, hue: f32, intensity: f32) -> Self {
        self.signal = marker_ink(hue, intensity, self.surface);
        self
    }

    pub(crate) fn ink(self, ink: Ink) -> Rgb {
        match ink {
            Ink::Surface => self.surface,
            Ink::Own => self.own,
            Ink::Accent => self.accent,
            Ink::Signal => self.signal,
            Ink::Fixed(rgb) => rgb,
        }
    }
}

/// The eighth-block ramp, lightest first.
///
/// Nine entries so index `0` is genuinely empty: a ramp that starts at `▁`
/// cannot express "nothing here yet", which is exactly what the first frame of
/// a reveal needs.
const COVERAGE_BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// What an animation says about one cell on one frame.
///
/// Every field is a patch over the settled rendering. A default `CellPaint`
/// changes nothing, which is the state every animation returns to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellPaint {
    pub(crate) fg: Option<Rgb>,
    pub(crate) bg: Option<Rgb>,
    pub(crate) attrs: AttrPatch,
    /// How much of the cell's own glyph is present, in `0.0..=1.0`.
    ///
    /// `1.0` for anything that is not a reveal, so a caller that ignores this
    /// field still draws every non-revealing behaviour correctly.
    pub(crate) coverage: f32,
    /// A glyph offered in place of the cell's settled one.
    ///
    /// `None` for every behaviour that does not deal in shape, which is nearly
    /// all of them. Read it through [`CellPaint::glyph_over`] rather than
    /// directly: that is where the same-width rule is enforced, and it is the
    /// only reason a substitution cannot move a column.
    pub(crate) glyph: Option<char>,
}

impl Default for CellPaint {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            attrs: AttrPatch::NONE,
            coverage: 1.0,
            glyph: None,
        }
    }
}

impl CellPaint {
    /// True when this paint would draw the cell exactly as it already is.
    ///
    /// The per-frame diff is built on this: an element every one of whose cells
    /// is settled costs no repaint at all.
    pub(crate) fn is_settled(&self) -> bool {
        self.fg.is_none()
            && self.bg.is_none()
            && self.attrs.is_empty()
            && self.coverage >= 1.0
            && self.glyph.is_none()
    }

    /// The glyph this cell should actually draw, given the one it settles to.
    ///
    /// A substitute is taken only when it occupies exactly the columns the
    /// settled glyph did. That is the whole of what the old style-only rule was
    /// protecting: no cell count, no column, and no reserved width can move,
    /// whatever a behaviour asks for. A behaviour that asks for something wider
    /// or narrower is simply not honoured — a decoration is never allowed to
    /// break the thing it decorates.
    pub(crate) fn glyph_over(&self, settled: char) -> char {
        match self.glyph {
            Some(glyph) if display_width(glyph) == display_width(settled) => glyph,
            _ => settled,
        }
    }

    /// Fold this paint into the style a text cell was going to be drawn with.
    ///
    /// Coverage becomes a mix toward the surface rather than a partial glyph: a
    /// letter cannot be half-drawn, and dimming it toward the background is the
    /// honest cell-grid rendering of "not fully here yet". A cell with no
    /// coverage at all still draws its glyph in the surface colour rather than
    /// being blanked, so a reveal never changes the element's width mid-flight.
    pub(crate) fn text_style(&self, base: Style, palette: InkPalette) -> Style {
        let mut style = base;
        if let Some(fg) = self.fg {
            style = style.fg(rgb_color(fg));
        }
        if let Some(bg) = self.bg {
            style = style.bg(rgb_color(bg));
        }
        if self.coverage < 1.0 {
            let from = self
                .fg
                .or_else(|| color_rgb(style.fg))
                .unwrap_or(palette.own);
            let to = self
                .bg
                .or_else(|| color_rgb(style.bg))
                .unwrap_or(palette.surface);
            style = style.fg(rgb_color(mix_rgb(
                from,
                to,
                1.0 - self.coverage.clamp(0.0, 1.0),
            )));
        }
        self.attrs.apply(style)
    }

    /// The block glyph that represents this cell's coverage.
    ///
    /// For filled surfaces — a meter, a wash over a pane, a bar — where the
    /// glyph set really does resolve eight steps inside one cell. Text cells
    /// use [`text_style`](Self::text_style) instead.
    pub(crate) fn coverage_block(&self) -> char {
        let step = (self.coverage.clamp(0.0, 1.0) * 8.0).round() as usize;
        COVERAGE_BLOCKS[step.min(COVERAGE_BLOCKS.len() - 1)]
    }

    /// Quantized form used for per-frame diffing.
    ///
    /// Colour is compared at full 8-bit depth because that is what actually
    /// reaches the terminal, and coverage at the eight steps the block ramp can
    /// resolve. Anything finer than that is a difference no cell can show, so
    /// letting it request a repaint would be spending frames on nothing.
    pub(crate) fn digest(&self) -> u64 {
        fn channel(rgb: Option<Rgb>) -> u64 {
            match rgb {
                None => 0,
                Some((r, g, b)) => 1 << 24 | u64::from(r) << 16 | u64::from(g) << 8 | u64::from(b),
            }
        }
        fn tri(value: Option<bool>) -> u64 {
            match value {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            }
        }
        let attrs = tri(self.attrs.bold)
            | tri(self.attrs.dim) << 2
            | tri(self.attrs.italic) << 4
            | tri(self.attrs.underline) << 6
            | tri(self.attrs.reverse) << 8;
        let coverage = (self.coverage.clamp(0.0, 1.0) * 8.0).round() as u64;
        // A glyph swap is the coarsest difference a cell can show and the most
        // visible, so it is compared exactly rather than quantized.
        let glyph = self.glyph.map_or(0, |glyph| u64::from(glyph) | 1 << 32);
        channel(self.fg)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(17)
            ^ channel(self.bg).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ attrs.rotate_left(41)
            ^ coverage.rotate_left(53)
            ^ glyph.wrapping_mul(0xD6E8_FEB8_6659_FD93)
    }
}

/// Columns a glyph occupies, with anything unmeasurable treated as one.
///
/// Control characters and unassigned code points report `None`; a decoration
/// call site has already reserved one column for the glyph it settled to, so
/// treating an unmeasurable glyph as one column compares like with like rather
/// than silently letting a zero-width substitute through.
fn display_width(glyph: char) -> usize {
    unicode_width::UnicodeWidthChar::width(glyph).unwrap_or(1)
}

fn rgb_color(rgb: Rgb) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2)
}

fn color_rgb(color: Option<ratatui::style::Color>) -> Option<Rgb> {
    color.and_then(crate::ui::color::color_to_rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_paint_changes_nothing() {
        let paint = CellPaint::default();
        assert!(paint.is_settled());
        let base = Style::default().fg(ratatui::style::Color::Rgb(1, 2, 3));
        let palette = InkPalette {
            surface: (0, 0, 0),
            own: (1, 2, 3),
            accent: (9, 9, 9),
            signal: (9, 9, 9),
        };
        assert_eq!(paint.text_style(base, palette), base);
    }

    #[test]
    fn a_one_cell_axis_normalises_instead_of_dividing_by_zero() {
        let extent = CellExtent::new(1, 1);
        assert_eq!(extent.normalize(CellPos::new(0, 0)), (0.0, 0.0));
        // And an out-of-range cell clamps rather than running past 1.0.
        assert_eq!(
            CellExtent::new(4, 1).normalize(CellPos::col(99)),
            (1.0, 0.0)
        );
    }

    #[test]
    fn coverage_resolves_eight_steps_inside_one_cell() {
        let blocks: Vec<char> = (0..=8)
            .map(|step| {
                CellPaint {
                    coverage: step as f32 / 8.0,
                    ..CellPaint::default()
                }
                .coverage_block()
            })
            .collect();
        assert_eq!(blocks, COVERAGE_BLOCKS.to_vec());
    }

    #[test]
    fn partial_coverage_dims_text_toward_the_surface_without_blanking_it() {
        let palette = InkPalette {
            surface: (0, 0, 0),
            own: (200, 200, 200),
            accent: (0, 0, 255),
            signal: (0, 0, 255),
        };
        let base = Style::default().fg(ratatui::style::Color::Rgb(200, 200, 200));
        let half = CellPaint {
            coverage: 0.5,
            ..CellPaint::default()
        };
        assert_eq!(
            half.text_style(base, palette).fg,
            Some(ratatui::style::Color::Rgb(100, 100, 100))
        );
        // Fully uncovered still resolves to a colour, never to "no glyph": the
        // element must not change width part-way through a reveal.
        let none = CellPaint {
            coverage: 0.0,
            ..CellPaint::default()
        };
        assert_eq!(
            none.text_style(base, palette).fg,
            Some(ratatui::style::Color::Rgb(0, 0, 0))
        );
    }

    #[test]
    fn an_attribute_patch_is_tri_state() {
        let base = Style::default().add_modifier(Modifier::DIM | Modifier::BOLD);
        let patch = AttrPatch {
            bold: Some(false),
            italic: Some(true),
            ..AttrPatch::NONE
        };
        let style = patch.apply(base);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
        assert!(style.sub_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        // Untouched attributes survive: `None` is not `Some(false)`.
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_digest_ignores_changes_no_cell_could_show() {
        let a = CellPaint {
            coverage: 0.500,
            ..CellPaint::default()
        };
        let b = CellPaint {
            coverage: 0.505,
            ..CellPaint::default()
        };
        assert_eq!(a.digest(), b.digest(), "sub-step coverage is not a frame");

        // But a difference the terminal can actually draw is one.
        let c = CellPaint {
            coverage: 0.625,
            ..CellPaint::default()
        };
        assert_ne!(a.digest(), c.digest());
    }

    #[test]
    fn a_substitution_of_the_wrong_width_is_refused_rather_than_drawn() {
        let paint = |glyph| CellPaint {
            glyph: Some(glyph),
            ..CellPaint::default()
        };
        // A wide substitute would push every column right of it one cell over,
        // which is exactly the failure the old style-only rule existed to stop.
        assert_eq!(paint('한').glyph_over('─'), '─');
        // A same-width one is taken, because that is the whole point.
        assert_eq!(paint('╫').glyph_over('─'), '╫');
        // Including over a blank, which a foreground colour alone could never
        // ink — the connector's third cell is a space.
        assert_eq!(paint('▌').glyph_over(' '), '▌');
        // And a paint offering nothing leaves the settled glyph alone.
        assert_eq!(CellPaint::default().glyph_over('├'), '├');
    }

    #[test]
    fn a_paint_that_offers_a_glyph_is_never_settled() {
        // Or the per-frame diff would skip the frame that takes it away again,
        // and a discharge would be left burned into the line.
        let arc = CellPaint {
            glyph: Some('╫'),
            ..CellPaint::default()
        };
        assert!(!arc.is_settled());
        assert_ne!(arc.digest(), CellPaint::default().digest());
        // Two different marks are two different frames.
        let other = CellPaint {
            glyph: Some('╪'),
            ..CellPaint::default()
        };
        assert_ne!(arc.digest(), other.digest());
    }

    #[test]
    fn text_never_takes_a_glyph_substitution() {
        // The line the amended rule draws: decoration may change shape, a label
        // may not. `text_style` is the path every label takes.
        let palette = InkPalette {
            surface: (0, 0, 0),
            own: (200, 200, 200),
            accent: (0, 0, 255),
            signal: (0, 255, 0),
        };
        let base = Style::default().fg(ratatui::style::Color::Rgb(200, 200, 200));
        let arc = CellPaint {
            glyph: Some('╫'),
            ..CellPaint::default()
        };
        assert_eq!(
            arc.text_style(base, palette),
            base,
            "a glyph offer must not leak into a label's styling either"
        );
    }

    /// **The two channels do not touch each other.**
    ///
    /// The contract the whole split rests on, asserted on the observable ink and
    /// not on how it is spelled: every severity at one stage resolves to the
    /// same hue, and every stage at one severity resolves to the same saturation
    /// and the same distance from the surface.
    #[test]
    fn a_signals_hue_answers_only_to_its_stage_and_its_intensity_only_to_its_severity() {
        use crate::ui::color::to_hsl;
        // A dark ground and a light one, because the intensity channel is a
        // distance *from the surface* and the two themes place it in opposite
        // directions.
        for surface in [(9, 17, 28), (239, 241, 245)] {
            for hue in [23.0, 41.0, 115.0, 170.0, 217.0, 267.0, 343.0] {
                for severity in Severity::ALL {
                    let (h, _, _) = to_hsl(signal_ink(hue, severity, surface));
                    let gap = {
                        let raw = (h - hue).abs() % 360.0;
                        raw.min(360.0 - raw)
                    };
                    assert!(
                        gap < 1.5,
                        "{severity:?} moved hue {hue} to {h} on {surface:?}: the \
                         intensity channel reached into the hue channel"
                    );
                }
            }

            for severity in Severity::ALL {
                let placed = signal_light(severity, surface);
                for hue in [23.0, 41.0, 115.0, 170.0, 217.0, 267.0, 343.0] {
                    let (_, s, l) = to_hsl(signal_ink(hue, severity, surface));
                    assert!(
                        (s - SIGNAL_SATURATION).abs() < 0.02,
                        "hue {hue} at {severity:?} is drawn at saturation {s:.3} \
                         rather than the shared {SIGNAL_SATURATION:.3}"
                    );
                    assert!(
                        (l - placed).abs() < 0.02,
                        "hue {hue} at {severity:?} is placed at lightness {l:.3} \
                         rather than the {placed:.3} its severity asks for"
                    );
                }
            }
        }
    }

    /// **The defect marker's two channels do not touch each other either.**
    ///
    /// The [`crate::quality_streak`] intensity steps are a proportion of the
    /// reach rather than one of `Severity`'s four points, so they get the same
    /// contract asserted on their own path: the stage moves the hue and nothing
    /// else, the severity moves the distance from the surface and nothing else.
    #[test]
    fn a_markers_hue_answers_only_to_its_stage_and_its_intensity_only_to_its_severity() {
        use crate::quality_streak::DefectSeverity;
        use crate::ui::color::to_hsl;

        let palette = crate::app::state::Palette::catppuccin();
        let host = crate::terminal_theme::TerminalTheme::default();
        for surface in [(9, 17, 28), (239, 241, 245)] {
            for severity in DefectSeverity::ALL {
                // Every lifecycle hue at this one severity: the ink must land
                // at one lightness, so a card moving red -> orange -> yellow ->
                // green never reads as its defect having changed size.
                let placed: Vec<f32> = LifecycleStage::ALL
                    .into_iter()
                    .map(|stage| {
                        let hue = stage.hue(&palette, &host);
                        let ink = marker_ink(hue, severity.intensity(), surface);
                        let (h, s, l) = to_hsl(ink);
                        let gap = {
                            let raw = (h - hue).abs() % 360.0;
                            raw.min(360.0 - raw)
                        };
                        assert!(
                            gap < 1.5,
                            "{severity:?} moved {stage:?}'s hue {hue} to {h}: the \
                             intensity channel reached into the hue channel"
                        );
                        assert!(
                            (s - SIGNAL_SATURATION).abs() < 0.02,
                            "{stage:?} at {severity:?} is drawn at saturation {s:.3} \
                             rather than the shared {SIGNAL_SATURATION:.3}"
                        );
                        l
                    })
                    .collect();
                let first = placed[0];
                for l in &placed {
                    assert!(
                        (l - first).abs() < 0.02,
                        "{severity:?} is placed at {placed:?} across the stages: a \
                         stage change is reading as a severity change"
                    );
                }
            }

            // And the four steps are four steps: distinct, quietest first.
            let lights: Vec<f32> = DefectSeverity::ALL
                .into_iter()
                .map(|severity| {
                    signal_light_at_reach(MARKER_FULL_REACH * severity.intensity(), surface)
                })
                .collect();
            let ground = to_hsl(surface).2;
            for pair in lights.windows(2) {
                assert!(
                    (pair[0] - pair[1]).abs() > 0.03,
                    "two severities land on the same light: {lights:?}"
                );
                assert!(
                    (pair[1] - ground).abs() > (pair[0] - ground).abs(),
                    "a worse defect must stand further off the panel: {lights:?}"
                );
            }
        }
    }

    /// The loudest defect marker is exactly the ink the marker shipped with.
    ///
    /// S1 is 100% of [`MARKER_FULL_REACH`], and that constant is
    /// [`Severity::Serious`]'s reach — the value a live render settled on before
    /// severity had a channel at all. Pinning it here is what makes "a fleet
    /// that publishes nothing sees no change" checkable rather than asserted.
    #[test]
    fn the_loudest_step_lands_on_the_ink_the_marker_shipped_with() {
        use crate::quality_streak::DefectSeverity;
        let surface = (9, 17, 28);
        for hue in [23.0, 115.0, 217.0, 343.0] {
            assert_eq!(
                marker_ink(hue, DefectSeverity::S1.intensity(), surface),
                signal_ink(hue, Severity::Serious, surface),
            );
        }
    }

    /// Every stage at every severity is a signal somebody could tell from the
    /// other nineteen.
    #[test]
    fn every_stage_by_severity_combination_resolves_to_its_own_ink() {
        let palette = crate::app::state::Palette::catppuccin();
        let host = crate::terminal_theme::TerminalTheme::default();
        let surface = (9, 17, 28);
        let matrix: Vec<_> = LifecycleStage::ALL
            .into_iter()
            .flat_map(|stage| {
                let hue = stage.hue(&palette, &host);
                Severity::ALL
                    .into_iter()
                    .map(move |severity| (stage, severity, signal_ink(hue, severity, surface)))
            })
            .collect();
        assert_eq!(matrix.len(), 20);
        for (i, a) in matrix.iter().enumerate() {
            for b in &matrix[i + 1..] {
                assert_ne!(
                    a.2, b.2,
                    "{:?}/{:?} and {:?}/{:?} resolve to the same ink",
                    a.0, a.1, b.0, b.1
                );
            }
        }
    }

    /// The five stage hues are far enough apart to survive the card's own
    /// left-to-right gradient running through them.
    #[test]
    fn the_stage_hues_are_separated_under_every_shipped_theme() {
        let host = crate::terminal_theme::TerminalTheme::default();
        for palette in [
            crate::app::state::Palette::catppuccin(),
            crate::app::state::Palette::catppuccin_latte(),
        ] {
            let hues: Vec<f32> = LifecycleStage::ALL
                .into_iter()
                .map(|stage| stage.hue(&palette, &host))
                .collect();
            for (i, a) in hues.iter().enumerate() {
                for b in &hues[i + 1..] {
                    let raw = (a - b).abs() % 360.0;
                    let gap = raw.min(360.0 - raw);
                    assert!(gap > 40.0, "two stages sit {gap:.1}° apart: {hues:?}");
                }
            }
        }
    }

    /// A theme with no colour in it still gets five distinct stage hues.
    ///
    /// Reading a hue angle out of a grey is reading rounding noise, so the
    /// stated angles stand in. Without this the whole vocabulary collapses onto
    /// one or two accidental angles on a monochrome theme.
    #[test]
    fn a_monochrome_theme_falls_back_to_the_stated_angles() {
        let mut palette = crate::app::state::Palette::catppuccin();
        let grey = ratatui::style::Color::Rgb(128, 128, 128);
        palette.teal = grey;
        palette.yellow = grey;
        palette.mauve = grey;
        palette.green = grey;
        palette.red = grey;
        let host = crate::terminal_theme::TerminalTheme::default();
        for stage in LifecycleStage::ALL {
            assert_eq!(stage.hue(&palette, &host), stage.fallback_hue());
        }
    }

    /// Severity is monotone: worse is louder, at every hue and on either theme,
    /// *and in greyscale* — which is the claim that makes this channel readable
    /// without colour discrimination at all.
    ///
    /// Read as the WCAG contrast against the panel, which is a pure function of
    /// relative luminance and therefore exactly what survives desaturation.
    ///
    /// Checked on a dark panel and a light one, which is what Herdr draws on —
    /// `backdrop_rgb` resolves the host's own background and falls back to the
    /// reference's `#09111C`. A panel at exactly mid grey is the one surface
    /// where this cannot hold for every hue, because a luminance-heavy hue is
    /// *brighter* than mid grey at a lightness well below it, so the quiet end
    /// of the ramp crosses the panel's own luminance on the way out. Lightness
    /// separation still holds there — see
    /// `the_four_severities_are_four_inks_on_any_surface` — and the escalated
    /// rhythm is what carries the reading.
    #[test]
    fn worse_is_always_louder_including_in_greyscale() {
        for surface in [(9, 17, 28), (239, 241, 245)] {
            for hue in [23.0, 115.0, 217.0, 343.0] {
                let mut previous = 0.0;
                for severity in Severity::ALL {
                    let ink = signal_ink(hue, severity, surface);
                    let contrast = crate::ui::color::contrast_ratio(ink, surface);
                    assert!(
                        contrast > previous * 1.10,
                        "{severity:?} at hue {hue} on {surface:?} is not a step \
                         louder than the one below it: {contrast:.2} after {previous:.2}"
                    );
                    previous = contrast;
                }
                let quietest = crate::ui::color::contrast_ratio(
                    signal_ink(hue, Severity::Clear, surface),
                    surface,
                );
                // A mid-grey panel has about half the lightness headroom a real
                // theme does and is the binding case here, at about 2.8×. A
                // Herdr theme reaches 6× and better.
                assert!(
                    previous > quietest * 2.5,
                    "hue {hue} on {surface:?} spans only {:.2}× from clear to \
                     critical, which is not a channel",
                    previous / quietest
                );
            }
        }
    }

    /// Whatever the panel, the four severities are four different inks, placed
    /// in order and far enough apart to see.
    #[test]
    fn the_four_severities_are_four_inks_on_any_surface() {
        for surface in [(9, 17, 28), (239, 241, 245), (128, 128, 128), (0, 0, 0)] {
            for hue in [23.0, 115.0, 217.0, 343.0] {
                let mut previous: Option<f32> = None;
                for severity in Severity::ALL {
                    let light = crate::ui::color::to_hsl(signal_ink(hue, severity, surface)).2;
                    if let Some(previous) = previous {
                        assert!(
                            (light - previous).abs() > 0.06,
                            "{severity:?} at hue {hue} on {surface:?} landed at \
                             lightness {light:.3}, {:.3} from the step below it",
                            (light - previous).abs()
                        );
                    }
                    previous = Some(light);
                }
            }
        }
    }

    /// And every hue stays a hue at every severity: normalising the intensity
    /// must not bleach the channel that carries the stage.
    #[test]
    fn no_severity_washes_a_hue_out() {
        for surface in [(9, 17, 28), (239, 241, 245)] {
            for hue in [23.0, 115.0, 217.0, 343.0] {
                for severity in Severity::ALL {
                    let ink = signal_ink(hue, severity, surface);
                    assert!(
                        crate::ui::color::hue_is_meaningful(ink),
                        "{severity:?} at hue {hue} on {surface:?} resolved to {ink:?}, \
                         which no longer has a hue to read"
                    );
                }
            }
        }
    }

    /// A caller with no severity of its own gets the quietest rung, and only
    /// a genuinely serious problem changes how an element moves.
    #[test]
    fn clear_is_the_default_and_only_serious_escalates() {
        assert_eq!(Severity::default(), Severity::Clear);
        assert_eq!(Severity::Clear.light_reach(), SEVERITY_LIGHT_REACH[0]);
        assert!(!Severity::Clear.escalates());
        assert!(!Severity::Mild.escalates());
        assert!(Severity::Serious.escalates());
        assert!(Severity::Critical.escalates());
    }

    #[test]
    fn the_digest_separates_a_missing_colour_from_a_black_one() {
        let absent = CellPaint::default();
        let black = CellPaint {
            fg: Some((0, 0, 0)),
            ..CellPaint::default()
        };
        assert_ne!(absent.digest(), black.digest());
        // And foreground is not confusable with background.
        let black_bg = CellPaint {
            bg: Some((0, 0, 0)),
            ..CellPaint::default()
        };
        assert_ne!(black.digest(), black_bg.digest());
    }
}
