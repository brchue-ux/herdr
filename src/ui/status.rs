use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::text::display_width_u16;
use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{CopyFeedback, Palette, ToastKind, ToastNotification},
    config::{StatusIndicatorStyle, ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
};

pub(crate) fn copy_feedback_rect(
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = match position {
        ToastClipboardPosition::TopLeft | ToastClipboardPosition::BottomLeft => area.x,
        ToastClipboardPosition::TopCenter | ToastClipboardPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastClipboardPosition::TopRight | ToastClipboardPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match position {
        ToastClipboardPosition::TopLeft
        | ToastClipboardPosition::TopCenter
        | ToastClipboardPosition::TopRight => area.y + offset_rows.min(area.height),
        ToastClipboardPosition::BottomLeft
        | ToastClipboardPosition::BottomCenter
        | ToastClipboardPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + offset_rows)
        }
    };
    Rect::new(x, y, width, height)
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
) -> Rect {
    let content_width = display_width_u16(&toast.title)
        .max(display_width_u16(&toast.context))
        .saturating_add(4);
    let width = content_width.saturating_add(2).min(area.width);
    let content_height = if toast.context.is_empty() { 1 } else { 2 };
    let height = (content_height + 2).min(area.height);
    let x = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::BottomLeft => area.x,
        ToastHerdrPosition::TopRight | ToastHerdrPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let warning_offset = u16::from(offset_for_warning);
    let y = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::TopRight => {
            area.y + warning_offset.min(area.height)
        }
        ToastHerdrPosition::BottomLeft | ToastHerdrPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + warning_offset)
        }
    };
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
        ToastKind::ProcessFailed => p.red,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning, position);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 1 {
        return;
    }

    let [title_row, context_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let title = Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            &toast.title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    let context = Line::from(vec![
        Span::styled("  ", Style::default().fg(p.overlay0)),
        Span::styled(&toast.context, Style::default().fg(p.overlay0)),
    ]);

    frame.render_widget(Paragraph::new(title), title_row);
    if !toast.context.is_empty() && inner.height >= 2 {
        frame.render_widget(Paragraph::new(context), context_row);
    }
}

pub(super) fn render_copy_feedback(
    frame: &mut Frame,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
    p: &Palette,
) {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows, position);
    if feedback_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, feedback_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.green))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(feedback_area);
    frame.render_widget(block, feedback_area);

    if inner.height == 0 {
        return;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(p.green).bg(p.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), inner);
}

/// The shared look of the top-right warning banner: bold dark-on-yellow, right
/// aligned, one row per line, clearing whatever it covers.
fn render_banner_line(frame: &mut Frame, area: Rect, row: u16, text: &str, p: &Palette) {
    if row >= area.height {
        return;
    }
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);
    let width = display_width_u16(text).min(area.width);
    let notif_area = Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + row,
        width,
        1,
    );

    frame.render_widget(Clear, notif_area);
    frame.render_widget(
        Paragraph::new(Span::styled(text.to_string(), style)),
        notif_area,
    );
}

/// How many banner rows [`render_config_diagnostic`] will occupy, so anything
/// stacked under it starts below rather than over it.
pub(super) fn config_diagnostic_rows(message: &str, area: Rect) -> u16 {
    config_diagnostic_lines(message)
        .take(area.height as usize)
        .count() as u16
}

fn config_diagnostic_lines(message: &str) -> impl Iterator<Item = &str> {
    message.lines().filter(|line| !line.trim().is_empty())
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    for (row, line) in config_diagnostic_lines(message)
        .take(area.height as usize)
        .enumerate()
    {
        render_banner_line(frame, area, row as u16, &format!(" {line} "), p);
    }
}

/// Say why this client's panes are drawn around grids that are not its size.
///
/// The head of the line carries the fact and the culprit's size, because a
/// narrow client truncates the tail: the shared size is what identifies which
/// other client to detach.
pub(super) fn render_pane_size_pin(
    frame: &mut Frame,
    area: Rect,
    pin: &crate::app::state::PaneSizePin,
    row: u16,
    p: &Palette,
) {
    let (shared_cols, shared_rows) = pin.shared;
    let (client_cols, client_rows) = pin.client;
    render_banner_line(
        frame,
        area,
        row,
        &format!(
            " panes pinned to {shared_cols}x{shared_rows} by another client; this one is {client_cols}x{client_rows} "
        ),
        p,
    );
}

/// The glyph half of [`state_icon`], for surfaces that build their own style
/// (the mobile header roll-up tones its own text). Keeping one match here is
/// what stops a surface from re-typing a mark and drifting — and it is what
/// makes replacing the alphabet a one-place edit.
///
/// This alphabet is deliberately all-ASCII and deliberately interim. The
/// previous set (`◉ ◐ ● ○ ·`) failed four separate ways that were measured, not
/// argued: blocked (`◉`) and done (`●`) shared 90% of their ink even though
/// blocked is the mark you must never miss; `◉` is present in only one of the
/// five monospace families on a stock Linux box; four of the five marks were
/// East-Asian *Ambiguous* width and one was not, so the icon column silently
/// widened by state on a terminal configured to draw ambiguous glyphs
/// double-width; and `·` was the same character, in the same colour, as the
/// sidebar's own token separator (`src/ui/sidebar/tokens.rs`). ASCII is one
/// cell in every terminal, present in every font, and shares no ink between
/// `!`, `>` and `-`.
///
/// `Unknown` draws a blank rather than a mark. It is not a state — the detector
/// documents it as "plain shell or unrecognized program" — so the state column
/// says nothing about it, which also dissolves the `·` collision.
///
/// Idle draws `-` whether or not it has been seen; the two are still separated
/// by colour and by [`state_label`] (`done` vs `idle`). Giving unacknowledged
/// its own channel is a runtime change and is not part of this set.
pub(super) fn state_mark(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "!",
        (AgentState::Working, _) => ">",
        (AgentState::Idle, _) => "-",
        (AgentState::Unknown, _) => " ",
    }
}

/// The configured alphabet, of which [`state_mark`] is one.
///
/// `Ascii` is this fork's default and delegates to `state_mark`; upstream's
/// `Dots` and `Symbols` stay selectable through `ui.status_indicators`. Only one
/// is ever drawn at a time, so the two sets never appear side by side.
pub(super) fn state_icon_symbol(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
) -> &'static str {
    match (indicator_style, state, seen) {
        (StatusIndicatorStyle::Ascii, _, _) => state_mark(state, seen),
        (StatusIndicatorStyle::Dots, AgentState::Blocked, _) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Working, _) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Idle, false) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Dots, AgentState::Unknown, _) => "·",
        (StatusIndicatorStyle::Symbols, AgentState::Blocked, _) => "×",
        (StatusIndicatorStyle::Symbols, AgentState::Working, _) => "◐",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, false) => "✓",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Symbols, AgentState::Unknown, _) => "·",
    }
}

pub(super) fn state_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    (
        state_icon_symbol(state, seen, indicator_style),
        Style::default().fg(state_label_color(state, seen, p)),
    )
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        // `unknown`, not `idle`: this is the only one of six copies of this
        // mapping that said `idle`, so a configured `state_text` token called a
        // plain shell idle while `herdr api` and the navigator both called the
        // same pane unknown.
        (AgentState::Unknown, _) => "unknown",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToastClipboardPosition, ToastHerdrPosition};

    fn toast() -> ToastNotification {
        ToastNotification {
            kind: ToastKind::Finished,
            title: "done".to_string(),
            context: "workspace".to_string(),
            position: None,
            target: None,
        }
    }

    fn feedback() -> CopyFeedback {
        CopyFeedback {
            message: "copied to clipboard".to_string(),
        }
    }

    #[test]
    fn state_icons_support_dot_and_distinct_symbol_styles() {
        let palette = Palette::catppuccin();
        for (indicator_style, expected_symbols) in [
            // This fork's default alphabet, and the two upstream ships.
            (StatusIndicatorStyle::Ascii, ["!", ">", "-", "-", " "]),
            (StatusIndicatorStyle::Dots, ["●", "●", "●", "○", "·"]),
            (StatusIndicatorStyle::Symbols, ["×", "◐", "✓", "○", "·"]),
        ] {
            for ((state, seen, color), expected_symbol) in [
                (AgentState::Blocked, true, palette.red),
                (AgentState::Working, true, palette.yellow),
                (AgentState::Idle, false, palette.teal),
                (AgentState::Idle, true, palette.green),
                (AgentState::Unknown, true, palette.overlay0),
            ]
            .into_iter()
            .zip(expected_symbols)
            {
                let (actual_symbol, style) = state_icon(state, seen, indicator_style, &palette);
                assert_eq!(actual_symbol, expected_symbol);
                assert_eq!(display_width_u16(actual_symbol), 1);
                assert_eq!(style.fg, Some(color));
            }
        }
    }

    /// One mark per real agent state, and no two of them the same picture.
    ///
    /// `Unknown` is exempt: it is not a state, so it deliberately draws nothing.
    /// `seen` is not in the key — acknowledgement is not carried by the mark.
    #[test]
    fn state_dots_are_distinct_single_cell_glyphs() {
        let states = [AgentState::Blocked, AgentState::Working, AgentState::Idle];

        let mut used: Vec<&'static str> = Vec::new();
        for state in states {
            let symbol = state_mark(state, true);
            assert_eq!(
                crate::ui::text::display_width(symbol),
                1,
                "state mark {symbol:?} must occupy exactly one cell so token layout stays aligned"
            );
            assert!(
                !used.contains(&symbol),
                "state mark {symbol:?} is reused; rolled-up state must not be encoded in colour alone"
            );
            used.push(symbol);
        }

        assert_eq!(
            state_mark(AgentState::Unknown, true),
            " ",
            "a pane that is not an agent has no state to report"
        );
    }

    /// The sidebar's `state_text` token is the sixth copy of the state-to-word
    /// mapping, and it was the only one that called a plain shell `idle` — the
    /// same word a genuinely idle agent gets, while `herdr api`, the navigator
    /// and the agent panel all called the same pane `unknown`.
    #[test]
    fn state_label_agrees_with_the_other_copies_of_the_mapping() {
        for seen in [true, false] {
            assert_eq!(state_label(AgentState::Unknown, seen), "unknown");
            assert_eq!(
                state_label(AgentState::Unknown, seen),
                crate::detect::manifest::agent_state_label(AgentState::Unknown),
                "the sidebar and the JSON API must name the same state the same way"
            );
        }
    }

    /// The width contract cannot be settled by measuring: `unicode-width`
    /// reports 1 for East-Asian *Ambiguous* characters, which iTerm2, Konsole
    /// and Windows Terminal will all draw two cells wide when configured for
    /// CJK. The previous set passed that measurement and still jittered the
    /// icon column by state on those terminals. ASCII has no ambiguous class,
    /// so this is the assertion that actually holds in a real terminal.
    #[test]
    fn state_marks_are_ascii_so_the_column_cannot_widen_by_state() {
        for state in [
            AgentState::Blocked,
            AgentState::Working,
            AgentState::Idle,
            AgentState::Unknown,
        ] {
            for seen in [true, false] {
                let mark = state_mark(state, seen);
                assert!(
                    mark.is_ascii(),
                    "state mark {mark:?} for {state:?} seen={seen} is not ASCII, so its cell \
                     width depends on the terminal's ambiguous-width setting"
                );
                assert_eq!(
                    mark.chars().count(),
                    1,
                    "state mark {mark:?} must be exactly one character"
                );
            }
        }
    }

    #[test]
    fn toast_rect_uses_configured_corner() {
        let area = Rect::new(10, 20, 100, 40);
        let toast = toast();

        let top_left = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopLeft);
        assert_eq!(top_left.x, area.x);
        assert_eq!(top_left.y, area.y);

        let top_right = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);
        assert_eq!(top_right.x + top_right.width, area.x + area.width);
        assert_eq!(top_right.y, area.y);

        let bottom_left =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomLeft);
        assert_eq!(bottom_left.x, area.x);
        assert_eq!(bottom_left.y + bottom_left.height, area.y + area.height);

        let bottom_right =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomRight);
        assert_eq!(bottom_right.x + bottom_right.width, area.x + area.width);
        assert_eq!(bottom_right.y + bottom_right.height, area.y + area.height);
    }

    #[test]
    fn toast_rect_uses_display_width_for_cjk_labels() {
        let area = Rect::new(0, 0, 100, 20);
        let toast = ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: "重构用户认证模块".to_string(),
            context: "提交 herdr 的反馈".to_string(),
            position: None,
            target: None,
        };

        let rect = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);

        let expected_content_width =
            display_width_u16(&toast.title).max(display_width_u16(&toast.context)) + 6;
        assert_eq!(rect.width, expected_content_width);
        assert_eq!(rect.x + rect.width, area.x + area.width);
    }

    #[test]
    fn copy_feedback_rect_uses_configured_position() {
        let area = Rect::new(10, 20, 100, 40);
        let feedback = feedback();

        let top_center = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::TopCenter);
        assert_eq!(top_center.y, area.y);
        assert_eq!(
            top_center.x,
            area.x + area.width.saturating_sub(top_center.width) / 2
        );

        let bottom_center =
            copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::BottomCenter);
        assert_eq!(bottom_center.y + bottom_center.height, area.y + area.height);
        assert_eq!(
            bottom_center.x,
            area.x + area.width.saturating_sub(bottom_center.width) / 2
        );
    }
}
