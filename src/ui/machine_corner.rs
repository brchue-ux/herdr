//! The machine register's words: the corner's header, its labels, its current values and how far
//! back its grooves reach — drawn as terminal cells over the graphics surface that draws the rest.
//!
//! # Why this is here rather than in the surface
//!
//! `crate::solar_system::machine_corner_frame` draws the corner's picture — four worn grooves and
//! one shaded body per logical CPU — and refuses, in its own words, to draw a font: *herdr's text
//! surface is the terminal itself, and painting a private bitmap font into a wash that sits under
//! real glyphs is exactly the thing this scene does not do.* That refusal is kept. What it left
//! behind was a readout nobody could read: four unlabelled bars over a row of dots say which
//! quantity is busier than it was, and nothing else — not which quantity, not what it is worth,
//! not where the number came from, not how far back the trace reaches.
//!
//! So the words are set here, in the terminal's own font, on the cells beside the picture. This is
//! the same move [`crate::ui::status::render_status_feed`] already makes for herdr's own stream,
//! over this same scene, on the same frame — and that renderer already reserves the columns this
//! corner occupies (`crate::ui::status::corner_reservation`), so the two readouts have never
//! collided and still do not.
//!
//! Three things follow from being cells rather than pixels, and all three are the point:
//!
//! - **Real glyphs.** The host's own font at the host's own size, not a 5x7 bitmap scaled to
//!   whatever the cell happened to be.
//! - **Real theming.** Every colour here is the active [`Palette`]'s, so the corner follows a
//!   theme change with everything else.
//! - **Real legibility.** `crate::app::background_legibility` already composites the corner's own
//!   RGBA when it decides a foreground for the cells it covers, so these words are sprung against
//!   the grooves under them for free. Nothing here has to invent a scrim.
//!
//! # Collision
//!
//! [`crate::solar_system::MachineCornerLayout`] is the single source of both halves' geometry:
//! the words own the columns left of `field_col`, the grooves and core bodies start at it, and
//! every row centre is a cell-row centre so a label sits on the same baseline as the groove it
//! names. Neither half computes a position the other does not already agree with.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::state::AppState;
use crate::app::state::Palette;
use crate::machine_register::{
    Absence, MachineRegister, Quantity, HISTORY_SAMPLES, SAMPLE_INTERVAL,
};
use crate::solar_system::MachineCornerLayout;

use super::text::{display_width_u16, truncate_end};

/// Draw the machine corner's words over its surface.
///
/// Nothing at all when the scene is not drawing, when the corner has no box, or when the box is
/// too narrow to carry both words and picture — the layout decides the last of those, and it
/// decides it the same way for both halves.
pub(super) fn render_machine_corner(app: &AppState, frame: &mut Frame, rect: Rect, p: &Palette) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let layout = MachineCornerLayout::new(rect.width, rect.height);
    if layout.text_cols() == 0 {
        return;
    }
    // Neither drawing nor explaining: on a host this build reads no machine state on, the corner
    // is not a surface at all, and a lone header standing over the sky would be a new panel on
    // every machine that is not Linux. The words follow the picture; they do not outlive it.
    if app.machine_corner_layer.is_none() && app.machine_corner_absence.is_none() {
        return;
    }

    // The header first: it is the one line that is true whether or not a number is.
    if let Some(row) = layout.header_row() {
        render_header(app, frame, rect, &layout, row, p);
    }

    if let Some(absence) = app.machine_corner_absence {
        render_absence(frame, rect, &layout, absence, p);
        return;
    }

    let register = &app.machine_register;
    if let Some(row) = layout.cores_row() {
        let cores = register.cores();
        let reporting = cores.iter().filter(|core| core.current().is_some()).count();
        render_pair(
            frame,
            rect,
            &layout,
            row,
            "cores",
            &format!("{reporting}/{}", cores.len()),
            p,
        );
    }

    for (index, quantity) in Quantity::ALL.iter().enumerate() {
        let Some(row) = layout.quantity_row(index) else {
            continue;
        };
        let series = register.series(*quantity);
        // F21 reaches the words too: a quantity with no reading gets its label and no number, not
        // a zero. The groove beside it is absent for the same reason.
        let value = series
            .current()
            .map(|value| value_text(*quantity, value))
            .unwrap_or_default();
        render_pair(frame, rect, &layout, row, quantity.label(), &value, p);
    }

    if let Some(row) = layout.footer_row() {
        render_footer(register, frame, rect, &layout, row, p);
    }
}

/// `machine` on the left, and on the right where these numbers are read from and how often.
///
/// The source is the register's own answer to *"where would I check this"*, which
/// `crate::machine_register` states is a reader's due. Shortened to the directory the sources
/// share — `/proc/stat`, `/proc/meminfo` and `/proc/loadavg` are one place, and naming all three
/// in twenty-six columns is not possible or useful.
fn render_header(
    app: &AppState,
    frame: &mut Frame,
    rect: Rect,
    layout: &MachineCornerLayout,
    row: u16,
    p: &Palette,
) {
    draw_run(frame, rect, 0, row, layout.cols(), "machine", p.subtext0);

    let cadence = format!("{}s", SAMPLE_INTERVAL.as_secs());
    let right = match source_root(app.machine_register.sources()) {
        Some(root) => format!("{root} · {cadence}"),
        None => cadence,
    };
    // Right-aligned to the box's own edge rather than to the text gutter: the header is about the
    // whole readout, not about the column of labels under it.
    let width = display_width_u16(&right);
    let room = layout
        .cols()
        .saturating_sub(display_width_u16("machine") + 1);
    if width == 0 || width > room {
        return;
    }
    draw_run(
        frame,
        rect,
        layout.cols().saturating_sub(width),
        row,
        width,
        &right,
        p.overlay0,
    );
}

/// How far back the grooves reach, under them.
///
/// Live rather than fixed, and that matters most in the first two minutes of a session: a corner
/// that has been watching for twenty-eight seconds says so, which is the same statement
/// `draw_groove` makes in pixels by drawing a partial history short rather than stretched.
fn render_footer(
    register: &MachineRegister,
    frame: &mut Frame,
    rect: Rect,
    layout: &MachineCornerLayout,
    row: u16,
    p: &Palette,
) {
    // The longest series, because that is the groove that reaches furthest back. Capped at what a
    // full groove is drawn as, so the footer can never claim more past than the picture shows.
    let samples = Quantity::ALL
        .iter()
        .map(|quantity| register.series(*quantity).len())
        .max()
        .unwrap_or(0)
        .min(HISTORY_SAMPLES);
    if samples == 0 {
        return;
    }
    let text = format!(
        "{} of history",
        span_text(samples as u64 * SAMPLE_INTERVAL.as_secs())
    );
    draw_run(frame, rect, 0, row, layout.cols(), &text, p.overlay0);
}

/// Why the corner is blank, in the register's own words, under the header.
///
/// The picture cannot say this — an empty box and a box that is not there are the same rectangle
/// of sky — and a reader who is looking at nothing deserves to know whether it is broken or simply
/// young. Every reason drawn here resolves on its own; the one that does not
/// ([`Absence::Unsupported`]) is filtered out before it reaches this state, by
/// `crate::app::App::note_corner_absence`.
fn render_absence(
    frame: &mut Frame,
    rect: Rect,
    layout: &MachineCornerLayout,
    absence: Absence,
    p: &Palette,
) {
    let first = layout.cores_row().unwrap_or(0);
    let last = layout.footer_row().unwrap_or(layout.rows());
    for (offset, line) in wrap(absence.reason(), layout.cols())
        .into_iter()
        .enumerate()
    {
        let row = first.saturating_add(offset as u16);
        if row >= last {
            break;
        }
        draw_run(frame, rect, 0, row, layout.cols(), &line, p.overlay0);
    }
}

/// One row of the readout: what it is on the left, what it is worth on the right.
///
/// Both inside [`MachineCornerLayout::text_cols`], so neither can reach the groove that shares the
/// row. The value is right-aligned against the gutter's own edge so four numbers of different
/// widths read down as a column.
fn render_pair(
    frame: &mut Frame,
    rect: Rect,
    layout: &MachineCornerLayout,
    row: u16,
    label: &str,
    value: &str,
    p: &Palette,
) {
    let gutter = layout.text_cols();
    draw_run(frame, rect, 0, row, gutter, label, p.overlay1);
    let width = display_width_u16(value);
    if width == 0 || width >= gutter {
        return;
    }
    draw_run(
        frame,
        rect,
        gutter.saturating_sub(width),
        row,
        width,
        value,
        p.subtext0,
    );
}

/// Put one run of text at one place in the corner, and write nothing anywhere else.
///
/// Deliberately not one padded line per row. The corner sits over whatever pane holds the
/// top-right of the screen, and a full-width line would blank every cell of it including the ones
/// between the label and the value; a run writes only the cells it actually has glyphs for, so a
/// pane's own text survives in the gaps. It also leaves every cell's background at `Reset`, which
/// is what lets `crate::ui::apply_background_legibility` spring these glyphs against the grooves
/// underneath them.
fn draw_run(
    frame: &mut Frame,
    rect: Rect,
    col: u16,
    row: u16,
    max_width: u16,
    text: &str,
    color: ratatui::style::Color,
) {
    if row >= rect.height || col >= rect.width || max_width == 0 {
        return;
    }
    let room = max_width.min(rect.width.saturating_sub(col));
    let text = truncate_end(text, usize::from(room));
    let width = display_width_u16(&text);
    if width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color)))),
        Rect::new(
            rect.x.saturating_add(col),
            rect.y.saturating_add(row),
            width,
            1,
        ),
    );
}

/// A quantity's current reading, in the unit a reader expects it in.
///
/// Three of the four are shares of their own whole and read as percentages. Load is the odd one:
/// `crate::machine_register` normalises it by core count, so `1.00` is one runnable task per core,
/// and a load average has always been read as a decimal rather than a percentage. Writing it as
/// `60%` would be the same number wearing a unit nobody uses for it.
fn value_text(quantity: Quantity, value: f32) -> String {
    match quantity {
        Quantity::Load => format!("{value:.2}"),
        _ => format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as u32),
    }
}

/// A span of seconds, short enough for a corner: `28s`, `1m30s`, `2m`.
fn span_text(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let (minutes, rest) = (seconds / 60, seconds % 60);
    if rest == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m{rest:02}s")
    }
}

/// The one place a set of sources came from, when they share one.
///
/// `None` when they do not, rather than a guess — naming `/proc` for a set that also read
/// somewhere else would send a reader to check a number in a file it was never in.
fn source_root(sources: &[&'static str]) -> Option<String> {
    fn parent(source: &str) -> &str {
        match source.rsplit_once('/') {
            Some((head, _)) if !head.is_empty() => head,
            _ => source,
        }
    }
    let first = parent(sources.first()?);
    sources
        .iter()
        .all(|source| parent(source) == first)
        .then(|| first.to_string())
}

/// Break `text` on spaces into lines no wider than `width`.
///
/// A word longer than the whole width is cut rather than allowed to overflow the box — the
/// alternative is a reason that runs into the sky beside it.
fn wrap(text: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if display_width_u16(&candidate) <= width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        line = truncate_end(word, usize::from(width));
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect as TestRect;
    use ratatui::Terminal;

    /// A register carrying `samples` readings of a machine at a known state.
    ///
    /// Driven through `MachineRegister::sample` with real counters rather than by writing series
    /// directly, so the numbers these tests read back went through the same arithmetic a live one
    /// does — including the CPU fraction's two-sample rule.
    fn register_with(samples: usize, busy_per_sample: u64) -> MachineRegister {
        use crate::platform::MachineCounters;
        let mut register = MachineRegister::default();
        let origin = std::time::Instant::now();
        let mut busy = 0u64;
        let mut total = 0u64;
        for index in 0..=samples {
            busy += busy_per_sample;
            total += 1_000;
            register.sample(
                Some(MachineCounters {
                    cpu_total: Some((busy, total)),
                    cpu_per_core: (0..12).map(|_| Some((busy / 12, total / 12))).collect(),
                    memory_kib: Some((4_718_000, 10_000_000)),
                    swap_kib: Some((0, 8_000_000)),
                    load_average_1m: Some(7.25),
                    sources: vec!["/proc/stat", "/proc/meminfo", "/proc/loadavg"],
                }),
                origin + SAMPLE_INTERVAL * index as u32,
            );
        }
        register
    }

    /// The corner drawn into a terminal buffer, one string per row of its own box.
    fn drawn(app: &AppState, rect: TestRect) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(
            rect.x + rect.width + 2,
            rect.y + rect.height + 2,
        ))
        .expect("a test terminal");
        let palette = app.palette.clone();
        terminal
            .draw(|frame| render_machine_corner(app, frame, rect, &palette))
            .expect("a drawn frame");
        let buffer = terminal.backend().buffer().clone();
        (rect.y..rect.y + rect.height)
            .map(|row| {
                (rect.x..rect.x + rect.width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// An app whose corner is live, holding `register`.
    fn app_with(register: MachineRegister) -> AppState {
        let mut app = AppState::test_new();
        app.machine_register = register;
        app.machine_corner_absence = None;
        // The values are drawn only over a corner that is actually being drawn, so the fixture has
        // to stand one up. Its contents do not matter here — only that there is one.
        app.machine_corner_layer = Some(crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Png,
            1,
            1,
            vec![0],
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 0,
                viewport_row: 0,
                grid_cols: 0,
                grid_rows: 0,
                z: -1,
            },
        ));
        app
    }

    #[test]
    fn a_load_reads_as_a_load_and_a_share_reads_as_a_percentage() {
        // The register normalises load by core count, so 1.00 is one runnable task per core.
        // Writing that as "100%" is the same number in a unit nobody reads a load average in.
        assert_eq!(value_text(Quantity::Load, 0.6042), "0.60");
        assert_eq!(value_text(Quantity::Cpu, 0.2387), "24%");
        assert_eq!(value_text(Quantity::Memory, 0.4718), "47%");
        assert_eq!(value_text(Quantity::Swap, 0.0), "0%");
        assert_eq!(value_text(Quantity::Cpu, 1.0), "100%");
    }

    #[test]
    fn every_value_fits_the_gutter_it_is_drawn_in() {
        // The words own `TEXT_COLS` and the picture starts after them, so a value that outgrew the
        // gutter would be drawn over a groove — the exact collision this layout exists to prevent.
        // Held against the widest each quantity can be rather than against a sampled one.
        let layout = MachineCornerLayout::new(26, 8);
        let gutter = layout.text_cols();
        for quantity in Quantity::ALL {
            for value in [0.0, 0.005, 0.5, 0.999, 1.0] {
                let text = value_text(quantity, value);
                let label = quantity.label();
                assert!(
                    display_width_u16(&text) + display_width_u16(label) < gutter,
                    "{label} {text} does not fit {gutter} columns"
                );
            }
        }
        // ...and the cores row, whose value is a pair rather than a number.
        for (reporting, total) in [(1usize, 1usize), (12, 12), (7, 12), (64, 64)] {
            let text = format!("{reporting}/{total}");
            assert!(
                display_width_u16(&text) + display_width_u16("cores") < gutter,
                "cores {text} does not fit {gutter} columns"
            );
        }
    }

    #[test]
    fn a_span_is_written_the_way_a_glance_reads_it() {
        assert_eq!(span_text(0), "0s");
        assert_eq!(span_text(28), "28s");
        assert_eq!(span_text(60), "1m");
        assert_eq!(span_text(90), "1m30s");
        assert_eq!(span_text(120), "2m");
    }

    #[test]
    fn a_footer_never_claims_more_past_than_the_groove_draws() {
        // `HISTORY_SAMPLES` at `SAMPLE_INTERVAL` is what a full groove is, and the footer is the
        // sentence under it. If the two ever disagreed the words would be describing a picture
        // that is not there.
        let full = span_text(HISTORY_SAMPLES as u64 * SAMPLE_INTERVAL.as_secs());
        assert_eq!(full, "2m");
        assert!(
            display_width_u16(&format!("{full} of history")) <= 26,
            "the footer does not fit the corner"
        );
    }

    #[test]
    fn sources_are_named_by_the_one_place_they_share() {
        assert_eq!(
            source_root(&["/proc/stat", "/proc/meminfo", "/proc/loadavg"]).as_deref(),
            Some("/proc")
        );
        // Not a guess when they do not share one: sending a reader to check a number in a file it
        // was never in is worse than not naming a file at all.
        assert_eq!(
            source_root(&["/proc/stat", "/sys/fs/cgroup/cpu.stat"]),
            None
        );
        assert_eq!(source_root(&[]), None);
    }

    #[test]
    fn every_absence_reason_fits_the_box_it_explains() {
        // The reason is drawn between the header and the footer, so it has to wrap into the rows
        // actually left over. A reason that needed more would be cut mid-sentence.
        let layout = MachineCornerLayout::new(26, 8);
        let rows = layout
            .footer_row()
            .unwrap_or(layout.rows())
            .saturating_sub(layout.cores_row().unwrap_or(0));
        for absence in [
            Absence::NeverSampled,
            Absence::AwaitingSecondSample,
            Absence::Stalled,
            Absence::Unsupported,
        ] {
            let lines = wrap(absence.reason(), layout.cols());
            assert!(
                lines.len() <= usize::from(rows),
                "{absence:?} wrapped to {} lines, {rows} available",
                lines.len()
            );
            for line in &lines {
                assert!(display_width_u16(line) <= layout.cols());
            }
        }
    }

    #[test]
    fn a_word_longer_than_the_box_is_cut_rather_than_left_to_overflow() {
        let lines = wrap("supercalifragilistic tail", 8);
        assert!(lines.iter().all(|line| display_width_u16(line) <= 8));
        assert!(!lines.is_empty());
    }

    #[test]
    fn the_corner_names_every_quantity_and_says_what_it_is_worth() {
        // The whole reason this renderer exists. Four unlabelled grooves over a row of dots is
        // what the corner drew before, and it answered none of *which quantity*, *what value*,
        // *from where* or *how far back*.
        let rect = TestRect::new(4, 1, 26, 8);
        let layout = MachineCornerLayout::new(rect.width, rect.height);
        let app = app_with(register_with(30, 240));
        let rows = drawn(&app, rect);

        let row = |index: u16| rows[usize::from(index)].as_str();
        assert!(
            row(layout.header_row().expect("a header")).starts_with("machine"),
            "{rows:?}"
        );
        assert!(
            row(layout.header_row().expect("a header")).ends_with("/proc · 2s"),
            "the header does not say where the numbers came from or how often: {rows:?}"
        );
        assert!(
            row(layout.cores_row().expect("a cores row")).starts_with("cores"),
            "{rows:?}"
        );
        assert!(
            row(layout.cores_row().expect("a cores row")).contains("12/12"),
            "{rows:?}"
        );

        for (index, quantity) in Quantity::ALL.iter().enumerate() {
            let line = row(layout.quantity_row(index).expect("a quantity row"));
            assert!(
                line.starts_with(quantity.label()),
                "{} is not named on its own row: {rows:?}",
                quantity.label()
            );
            let value = app
                .machine_register
                .series(*quantity)
                .current()
                .map(|value| value_text(*quantity, value))
                .expect("a reading");
            assert!(
                line.contains(&value),
                "{} is named but not valued: {rows:?}",
                quantity.label()
            );
        }
        // 24% busy, 47% of memory, no swap, and a load of 7.25 across twelve cores.
        let value_row = |index: usize| row(layout.quantity_row(index).expect("a quantity row"));
        assert!(value_row(0).contains("24%"), "{rows:?}");
        assert!(value_row(1).contains("47%"), "{rows:?}");
        assert!(value_row(2).contains("0%"), "{rows:?}");
        assert!(value_row(3).contains("0.60"), "{rows:?}");

        let footer = row(layout.footer_row().expect("a footer"));
        assert!(footer.ends_with("of history"), "{rows:?}");
    }

    #[test]
    fn no_word_reaches_the_columns_the_picture_draws_in() {
        // The cell half of the seam `solar_system`'s
        // `the_picture_never_reaches_into_the_columns_the_words_own` holds from the pixel side.
        // Checked against the widest text every row can carry rather than against one sample.
        let rect = TestRect::new(0, 0, 26, 8);
        let layout = MachineCornerLayout::new(rect.width, rect.height);
        let app = app_with(register_with(HISTORY_SAMPLES + 4, 1_000));
        for row in drawn(&app, rect)
            .into_iter()
            .enumerate()
            // The header and the footer own their whole row: the picture leaves both empty, which
            // `every_row_of_the_picture_stays_inside_the_terminal_row_it_shares` holds.
            .filter(|(index, _)| {
                let index = *index as u16;
                Some(index) != layout.header_row() && Some(index) != layout.footer_row()
            })
            .map(|(_, row)| row)
        {
            assert!(
                display_width_u16(&row) <= layout.text_cols(),
                "{row:?} runs past the {} columns the words own",
                layout.text_cols()
            );
        }
    }

    #[test]
    fn the_words_move_when_the_machine_does() {
        // "Clean crisp and *reliable updates*". The values are read off the register on every
        // frame rather than baked with the picture, so a corner drawn one sample later says
        // something different. A renderer that cached its strings would pass every test above and
        // fail this one.
        let rect = TestRect::new(0, 0, 26, 8);
        let layout = MachineCornerLayout::new(rect.width, rect.height);
        let cpu = usize::from(layout.quantity_row(0).expect("a cpu row"));
        let footer = usize::from(layout.footer_row().expect("a footer row"));

        let quiet = drawn(&app_with(register_with(20, 40)), rect);
        let busy = drawn(&app_with(register_with(20, 960)), rect);
        assert_ne!(
            quiet, busy,
            "the corner said the same thing about two different machines"
        );
        assert!(quiet[cpu].contains("4%"), "{quiet:?}");
        assert!(busy[cpu].contains("96%"), "{busy:?}");

        // ...and the footer grows with the history behind it, which is what makes a young corner
        // legible as young rather than as broken.
        let young = drawn(&app_with(register_with(9, 240)), rect);
        let old = drawn(&app_with(register_with(HISTORY_SAMPLES + 4, 240)), rect);
        assert!(young[footer].starts_with("20s of history"), "{young:?}");
        assert!(old[footer].starts_with("2m of history"), "{old:?}");
    }

    #[test]
    fn a_blank_corner_says_why_it_is_blank() {
        // An empty box and a box that is not there are the same rectangle of sky. The picture
        // cannot tell them apart and does not try; this is the half that can.
        let rect = TestRect::new(0, 0, 26, 8);
        let mut app = app_with(register_with(20, 240));
        app.machine_corner_layer = None;
        app.machine_corner_absence = Some(Absence::Stalled);
        let rows = drawn(&app, rect);
        assert!(rows[0].starts_with("machine"), "{rows:?}");
        assert!(
            rows[1..].iter().any(|row| row.contains("stalled")),
            "a stalled corner did not say so: {rows:?}"
        );
        // ...and no number, because there is no current one. A stale value drawn as though it were
        // now is the specific dishonesty H12 names.
        assert!(
            !rows.iter().any(|row| row.contains('%')),
            "a stalled corner still published a value: {rows:?}"
        );
    }

    #[test]
    fn a_corner_with_no_picture_and_no_reason_is_left_alone() {
        // Neither drawing nor explaining: on a host this build reads no machine state on, the
        // corner is not a surface at all, and putting a permanent note over the sky there would be
        // a new panel on every machine that is not Linux.
        let rect = TestRect::new(0, 0, 26, 8);
        let mut app = app_with(MachineRegister::default());
        app.machine_corner_layer = None;
        app.machine_corner_absence = None;
        let rows = drawn(&app, rect);
        assert!(
            rows.iter().all(|row| row.is_empty()),
            "an unread corner put words over the sky: {rows:?}"
        );
    }

    #[test]
    fn a_box_too_narrow_for_both_halves_keeps_all_of_it_for_the_picture() {
        // A readout squeezed until neither half is legible is worse than either half alone, and
        // the layout is what decides which one gives way — the same decision the picture reads.
        let rect = TestRect::new(0, 0, MachineCornerLayout::TEXT_COLS + 2, 8);
        assert_eq!(
            MachineCornerLayout::new(rect.width, rect.height).text_cols(),
            0
        );
        let app = app_with(register_with(20, 240));
        assert!(
            drawn(&app, rect).iter().all(|row| row.is_empty()),
            "the words drew into a box that had no room for them"
        );
    }
}
