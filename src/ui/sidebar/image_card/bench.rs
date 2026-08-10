//! A synthetic fleet and a single-frame draw of it, for `herdr bench cards`.
//!
//! # Why this is here and not in `src/cli/`
//!
//! A `CardScene` is the wire form of a *placement*, and everything it is made
//! of — [`CardContent`], [`StageHues`], [`ControlRail`] — is private to
//! [`super`]. The real one is built by `compute_card_placement` off an
//! `AppState`, and `AppState::test_new` is `#[cfg(test)]`, so a shipped binary
//! has no way to stand a fleet up. This module is the child that can see those
//! types, and it is the whole of the benchmark that has to be.
//!
//! # What the load is, and what it is not
//!
//! It is synthetic and says so. The card *contents* are real in the ways the
//! rasteriser is sensitive to — every stage, every severity, a spread of depths,
//! tidbits, rails, residue stacks and spiders — because those are what decide
//! how much ink a card carries and how far its bloom reaches. The tree they hang
//! on is a repeating pattern rather than a real fleet's shape, which the panel
//! geometry does not care about: a card is drawn from its own frame and its own
//! content.
//!
//! The panel grows to hold whatever card count is asked for. A 48-card run is a
//! sidebar four screens tall, which nobody has; it is a way of putting four
//! screens of card work through the pass in one frame, and the per-card numbers
//! are what carry over.
//!
//! # Every frame is a full redraw
//!
//! [`draw`] passes no previous layers, so `match_held` finds nothing to carry
//! forward and every card is rasterised and re-encoded. That is the frame this
//! path exists for — the one where the tree's content changed — and it is the
//! only frame where the CPU and GPU backends do different amounts of work. A
//! settled panel returns `Ok(None)` from `shapes` without drawing anything, and
//! benchmarking that would measure a hash comparison.

use super::*;

/// One drawn frame's worth of accounting, for the report's per-frame columns.
pub(crate) struct Frame {
    /// Card layers that came back.
    pub(crate) cards: usize,
    /// Encoded bytes across every layer — what would go to the terminal.
    pub(crate) bytes: usize,
    /// Pixels across every card image, which is the size of the frame the two
    /// backends are racing over.
    pub(crate) pixels: u64,
}

/// How the synthetic fleet is shaped.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fleet {
    /// Cards in the tree.
    pub(crate) cards: usize,
    /// Panel width in cells, before [`sidebar_content_rect`] takes the
    /// scrollbar column off it.
    ///
    /// [`sidebar_content_rect`]: crate::ui::sidebar::sidebar_content_rect
    pub(crate) panel_cols: u16,
    /// The host cell size the client would have measured off its terminal.
    pub(crate) cell: HostCellSize,
}

impl Fleet {
    /// The captain's own panel, at the cell size a 13 px face gives on his
    /// terminal, holding one screen of cards.
    pub(crate) const fn default_fleet() -> Self {
        Self {
            cards: 12,
            panel_cols: 42,
            cell: HostCellSize {
                width_px: 10,
                height_px: 21,
            },
        }
    }
}

/// Why a synthetic fleet could not be stood up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoFleet {
    /// No proportional face on this machine, so no card can be set at all.
    NoFace,
    /// The panel is too narrow, too short, or the cell size is nonsense.
    Ungeometric,
}

impl std::fmt::Display for NoFleet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoFleet::NoFace => f.write_str(
                "no proportional font on this machine, so no card can be set — \
                 Herdr ships no font and takes the face off the system",
            ),
            NoFleet::Ungeometric => {
                f.write_str("the requested panel and cell size do not make a drawable card")
            }
        }
    }
}

/// Stand up a synthetic `CardScene` of `fleet.cards` cards.
///
/// The panel is sized to hold them: a run asking for more cards than a screen
/// holds gets a taller panel, not a clipped tree.
pub(crate) fn scene(fleet: Fleet) -> Result<CardScene, NoFleet> {
    let font = font::card_font(None).ok_or(NoFleet::NoFace)?;
    if fleet.cards == 0 {
        return Err(NoFleet::Ungeometric);
    }
    let cell_w = f32::from(u16::try_from(fleet.cell.width_px).map_err(|_| NoFleet::Ungeometric)?);
    let cell_h = f32::from(u16::try_from(fleet.cell.height_px).map_err(|_| NoFleet::Ungeometric)?);
    if cell_w <= 0.0 || cell_h <= 0.0 || fleet.panel_cols < MIN_FOLD_WIDTH.saturating_add(1) {
        return Err(NoFleet::Ungeometric);
    }

    let title_metrics = font.metrics(TITLE_PX);
    let tidbit_metrics = font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);
    // The same arithmetic `row_height_cells` does, without an `AppState` to read
    // the cell size off: a row is as tall as a card wants, in whole cells.
    let row_cells = {
        let wanted = card_height_px(title_metrics, tidbit_metrics);
        ((wanted / cell_h).ceil() as u16).max(crate::ui::sidebar::card::CHROME_ROWS + 1)
    };
    // The air the layout leaves under a row. One cell is what the character
    // layout puts between rows, and it is what the bloom of the card above
    // spills into.
    const ROW_GAP_CELLS: u16 = 1;
    let span = row_cells + ROW_GAP_CELLS;

    let rows = u16::try_from(fleet.cards).map_err(|_| NoFleet::Ungeometric)?;
    let panel_rows = rows
        .checked_mul(span)
        .ok_or(NoFleet::Ungeometric)?
        .checked_add(2)
        .ok_or(NoFleet::Ungeometric)?;
    let panel = Rect::new(0, 0, fleet.panel_cols, panel_rows);
    let bounds = crate::ui::sidebar::sidebar_content_rect(panel);
    if bounds.width == 0 || bounds.height == 0 {
        return Err(NoFleet::Ungeometric);
    }
    // No tray in a synthetic panel, so the bloom may reach the panel's floor —
    // the same answer `compute_card_placement` gives when `tray_rect` is empty.
    let bloom_floor = bounds.y.saturating_add(bounds.height);

    let hues = synthetic_hues();
    let mut placed: Vec<(Rect, CardContentWire)> = Vec::with_capacity(fleet.cards);
    for index in 0..fleet.cards {
        let depth = depth_of(index);
        // `card_frame_for`'s geometry, with the rank inset folded into the
        // prefix: what matters to the rasteriser is that cards at different
        // depths are different widths, which is what rank is *for*.
        let prefix =
            u16::try_from(crate::ui::sidebar::tree_prefix_width(depth, index)).unwrap_or(0);
        let width = bounds.width.saturating_sub(prefix);
        if width <= crate::ui::sidebar::card::CHROME_COLS {
            return Err(NoFleet::Ungeometric);
        }
        let y = bounds.y + u16::try_from(index).map_err(|_| NoFleet::Ungeometric)? * span;
        let frame = Rect::new(bounds.x + prefix, y, width, row_cells);
        placed.push((frame, CardContentWire::from(&content(index, depth, hues))));
    }

    let extents: Vec<Rect> = placed.iter().map(|(frame, _)| *frame).collect();
    let field = dissolve_field_rect(&extents, (cell_w, cell_h), bounds, bloom_floor)
        .ok_or(NoFleet::Ungeometric)?;

    Ok(CardScene {
        offsets: vec![(0, 0); placed.len()],
        placed,
        field,
        bounds,
        bloom_floor,
        backdrop: measured::CANVAS,
    })
}

/// Draw one frame of `scene`, with nothing carried forward.
///
/// This is [`rasterise_card_scene`] — the real client entry point, the same one
/// a Windows client takes on a `ServerMessage::CardScene` — so what is timed is
/// the shipped path and not a copy of it. Which bloom backend runs inside is
/// decided by [`crate::gpu::enabled`], which the benchmark pins.
pub(crate) fn draw(scene: &CardScene, cell: HostCellSize) -> Result<Frame, ()> {
    let layers = rasterise_card_scene(
        scene,
        None,
        cell,
        // A local Kitty host, which is the format decision the captain's own
        // terminal makes. Named rather than defaulted because it picks the
        // encoder, and the encode is a real share of a frame.
        crate::kitty_graphics::HostTerminalKind::Kitty,
        true,
        &[],
    )?
    // `None` is "nothing changed", which cannot happen with no previous layers.
    .ok_or(())?;
    Ok(Frame {
        cards: layers.len(),
        bytes: layers.iter().map(|layer| layer.layer.data.len()).sum(),
        pixels: layers
            .iter()
            .map(|layer| u64::from(layer.layer.image_width) * u64::from(layer.layer.image_height))
            .sum(),
    })
}

/// Every stage's hue, resolved off the default palette against a default host
/// theme — the same two inputs `StageHues::resolve` reads, without an
/// `AppState` between them.
fn synthetic_hues() -> StageHues {
    let palette = crate::app::state::Palette::catppuccin();
    let theme = crate::terminal_theme::TerminalTheme::default();
    let mut hues = [0.0; 5];
    for (slot, stage) in hues.iter_mut().zip(LifecycleStage::ALL) {
        *slot = stage.hue(&palette, &theme);
    }
    StageHues(hues)
}

/// A fleet's shape: a first mate, then mates and workers under it, repeating.
/// Three depths is what the rank ladder actually draws.
fn depth_of(index: usize) -> u8 {
    match index % 5 {
        0 => 0,
        1 | 3 => 1,
        _ => 2,
    }
}

/// One card's content.
///
/// Every field that changes how much work the card is to draw is varied across
/// the fleet, and deliberately not in step with each other: stage cycles on 5,
/// severity on 4, and the optional furniture on other periods again, so the
/// combinations a real fleet reaches are reached here too rather than five cards
/// repeating.
fn content(index: usize, depth: u8, hues: StageHues) -> CardContent {
    let stage = LifecycleStage::ALL[index % LifecycleStage::ALL.len()];
    let severity = Severity::ALL[index % Severity::ALL.len()];
    let state = match index % 4 {
        0 => AgentState::Working,
        1 => AgentState::Idle,
        2 => AgentState::Blocked,
        _ => AgentState::Unknown,
    };
    CardContent {
        // A name of the length agent panes actually carry, varied so no two
        // adjacent cards hash the same and the held-image matcher cannot carry
        // one forward for another.
        title: format!("herdr-worker-{index:03} rasteriser"),
        // Two cards in three carry one. A tidbit is a second line of shaped
        // text, which is real work the card either does or does not do.
        tidbit: (!index.is_multiple_of(3))
            .then(|| format!("src/ui/sidebar/image_card.rs:{}", 1000 + index)),
        state_label: match state {
            AgentState::Working => "working".to_string(),
            AgentState::Idle => "idle".to_string(),
            AgentState::Blocked => "blocked".to_string(),
            AgentState::Unknown => "shell".to_string(),
        },
        state,
        stage,
        severity,
        hues,
        ground: measured::CANVAS,
        split_channels: true,
        seen: index.is_multiple_of(2),
        depth,
        // One selected card, as a panel has.
        lifted: index == 1,
        mark: None,
        // A mate that has taken workers back draws a residue stack; a worker
        // does not.
        residue: if depth == 1 { (index % 4) as u8 } else { 0 },
        controls: ControlRail {
            summary: (depth == 1).then_some(SummaryBadge {
                count: index % 12,
                fresh: index.is_multiple_of(2),
            }),
            group: match index % 7 {
                0 => Some(GroupChevron::Expanded),
                3 => Some(GroupChevron::Collapsed),
                _ => None,
            },
        },
        // Mid-breath rather than settled: the breath is a light ramp the card
        // resolves per pixel, and a fleet drawn entirely at rest would skip it.
        breath: quantize((index % 12) as f32 / 12.0, CARD_BREATH_STEPS),
        // A quarter of the fleet is failing and carries a spider, mid-climb.
        spider: (index % 4 == 3).then(|| {
            spider::synthetic_for_bench(
                0.6,
                1.0,
                0.25 + 0.25 * ((index % 3) as f32),
                f32::from(u8::from(stage == LifecycleStage::Done)),
            )
        }),
        // The wash is not carried on the wire — a client resolves its own — and
        // `CardContentWire` drops it, so setting it here would be a lie about
        // what a client draws.
        wash: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The benchmark's fleet actually rasterises. This is the only thing about
    /// it that can rot silently: a change to card geometry that stops the
    /// synthetic panel producing cards would turn `herdr bench cards` into a
    /// one-line error nobody runs until the captain does.
    #[test]
    fn the_synthetic_fleet_draws() {
        let fleet = Fleet::default_fleet();
        let scene = match scene(fleet) {
            Ok(scene) => scene,
            Err(NoFleet::NoFace) => {
                println!("SKIP: no proportional face on this machine");
                return;
            }
            Err(other) => panic!("the benchmark's own fleet is not drawable: {other}"),
        };
        assert_eq!(
            scene.placed.len(),
            fleet.cards,
            "the fleet lost cards between the request and the placement"
        );
        let frame = draw(&scene, fleet.cell).expect("the synthetic fleet rasterised");
        assert_eq!(frame.cards, fleet.cards);
        assert!(
            frame.bytes > 0,
            "a drawn frame encoded no bytes, so nothing would reach a terminal"
        );
    }

    /// Cards at different depths are different widths. If they were not, the
    /// benchmark would be drawing one card `n` times and the held-image matcher
    /// would carry the first one forward for all of them.
    #[test]
    fn the_fleet_is_not_one_card_repeated() {
        let Ok(scene) = scene(Fleet::default_fleet()) else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        let widths: std::collections::HashSet<u16> =
            scene.placed.iter().map(|(frame, _)| frame.width).collect();
        assert!(
            widths.len() > 1,
            "every card in the synthetic fleet is the same width"
        );
    }
}
