//! One second mate's finished workers, and what each of them said it did.
//!
//! Opened from the badge on that mate's row in the Spaces tree, so the list is
//! scoped by the same `owner` edge that decides where those workers are drawn.
//! It is a plain modal in the existing dialog language — dimmed background,
//! panel shell, header, scrollable body, one action button — rather than a new
//! kind of screen.
//!
//! The text is whatever the worker published through `pane report-metadata`;
//! see [`crate::app::worker_summary`] for how the token family becomes lines.
//! Nothing is generated here, so a mate whose workers published nothing simply
//! never grows a badge to open this from.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::status::{state_dot, state_label, state_label_color};
use super::text::truncate_end;
use super::widgets::{
    action_button_row_rects, centered_popup_rect, modal_stack_areas, render_action_button,
    render_modal_header, render_panel_shell, ActionButtonSpec,
};
use crate::app::worker_summary::{summaries_for_owner, WorkerSummary};
use crate::app::AppState;

const POPUP_WIDTH: u16 = 64;
const POPUP_HEIGHT: u16 = 18;

pub(crate) fn worker_summaries_popup_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, POPUP_WIDTH, POPUP_HEIGHT)
}

/// The panel interior, one cell inside the shell's border.
pub(crate) fn worker_summaries_inner_rect(popup: Rect) -> Rect {
    Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    )
}

/// The action strip at the bottom of the panel, where the close button sits.
///
/// Exposed so mouse hit-testing derives the button rect from the same stack
/// layout the renderer used, instead of re-deriving the geometry by hand.
pub(crate) fn worker_summaries_action_row(inner: Rect) -> Rect {
    modal_stack_areas(inner, 1, 0, 1, 1)
        .actions
        .unwrap_or_default()
}

pub(crate) fn worker_summaries_close_button_rect(inner: Rect) -> Rect {
    action_button_row_rects(
        inner,
        &[ActionButtonSpec {
            hint: Some("esc"),
            label: "close",
        }],
        2,
        1,
    )[0]
}

/// The body rows, already wrapped to `width`, in list order.
///
/// Built as one flat run of lines so scrolling is a single offset and a worker
/// whose summary is longer than the panel cannot hide the workers under it.
fn body_lines(summaries: &[WorkerSummary], app: &AppState, width: u16) -> Vec<Line<'static>> {
    let p = &app.palette;
    let mut lines = Vec::new();
    for (idx, summary) in summaries.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::from(""));
        }
        let (glyph, glyph_style) = state_dot(summary.state, summary.seen, p);
        let mut header = vec![
            Span::raw(" "),
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
            Span::styled(
                truncate_end(&summary.name, usize::from(width).saturating_sub(16)),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
        ];
        header.push(Span::raw("  "));
        header.push(Span::styled(
            state_label(summary.state, summary.seen).to_string(),
            Style::default()
                .fg(state_label_color(summary.state, summary.seen, p))
                .add_modifier(Modifier::DIM),
        ));
        lines.push(Line::from(header));

        // Each published token is one authored line; wrapping is only ever
        // applied to what will not fit, so the publisher's own line breaks
        // survive.
        for raw in &summary.lines {
            for wrapped in wrap(raw, usize::from(width).saturating_sub(4)) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(wrapped, Style::default().fg(p.subtext0)),
                ]));
            }
        }
    }
    lines
}

/// Break `text` on whitespace into chunks of at most `width` display columns.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if super::text::display_width(&candidate) > width && !line.is_empty() {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// How many body rows the panel can show at once, for clamping the scroll.
pub(crate) fn worker_summaries_visible_rows(area: Rect) -> usize {
    let Some(popup) = worker_summaries_popup_rect(area) else {
        return 0;
    };
    let inner = worker_summaries_inner_rect(popup);
    usize::from(modal_stack_areas(inner, 1, 0, 1, 1).content.height)
}

/// The body rows the open view currently has, for clamping the scroll.
pub(crate) fn worker_summaries_total_rows(app: &AppState, area: Rect) -> usize {
    let Some(open) = app.worker_summaries.as_ref() else {
        return 0;
    };
    let Some(popup) = worker_summaries_popup_rect(area) else {
        return 0;
    };
    let inner = worker_summaries_inner_rect(popup);
    let summaries = summaries_for_owner(&super::sidebar::sidebar_agent_entries(app), &open.owner);
    body_lines(&summaries, app, inner.width).len()
}

pub(super) fn render_worker_summaries_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(open) = app.worker_summaries.as_ref() else {
        return;
    };
    super::dim_background(frame, area);
    let Some(popup) = worker_summaries_popup_rect(area) else {
        return;
    };
    let p = &app.palette;
    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };

    let stack = modal_stack_areas(inner, 1, 0, 1, 1);
    let summaries = summaries_for_owner(&super::sidebar::sidebar_agent_entries(app), &open.owner);

    render_modal_header(
        frame,
        stack.header,
        &format!(
            " summaries · {}",
            truncate_end(&open.owner, usize::from(inner.width).saturating_sub(16))
        ),
        p,
    );

    let lines = body_lines(&summaries, app, inner.width);
    let visible = usize::from(stack.content.height);
    if summaries.is_empty() {
        frame.render_widget(
            Paragraph::new(" no worker has published a summary yet")
                .style(Style::default().fg(p.overlay0)),
            stack.content,
        );
    } else {
        let scroll = open.scroll.min(lines.len().saturating_sub(visible));
        for (row, line) in lines.iter().skip(scroll).take(visible).enumerate() {
            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect::new(
                    stack.content.x,
                    stack.content.y + row as u16,
                    stack.content.width,
                    1,
                ),
            );
        }
        if lines.len() > visible {
            let shown = (scroll + visible).min(lines.len());
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("{shown}/{} ", lines.len()),
                    Style::default().fg(p.overlay0),
                ))
                .alignment(Alignment::Right),
                Rect::new(stack.content.x, stack.header.y, stack.content.width, 1),
            );
        }
    }

    if let Some(actions) = stack.actions {
        render_action_button(
            frame,
            worker_summaries_close_button_rect(actions),
            Some("esc"),
            "close",
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_breaks_on_words_and_never_drops_text() {
        let wrapped = wrap("one two three four five", 9);
        assert!(wrapped.iter().all(|line| line.chars().count() <= 9));
        assert_eq!(wrapped.join(" "), "one two three four five");
    }

    #[test]
    fn a_word_longer_than_the_width_still_survives_on_its_own_line() {
        let wrapped = wrap("short supercalifragilistic", 6);
        assert_eq!(wrapped, vec!["short", "supercalifragilistic"]);
    }

    #[test]
    fn zero_width_returns_the_text_rather_than_looping() {
        assert_eq!(wrap("anything", 0), vec!["anything"]);
    }

    #[test]
    fn empty_text_still_yields_one_line() {
        assert_eq!(wrap("", 10), vec![String::new()]);
    }
}
