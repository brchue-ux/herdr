//! The tray's popover: one mechanism, three contents.
//!
//! Anchored above the badge it came from, tail pointing down at it. The tray is
//! at the foot of the panel, so a popover always opens upward and never has to
//! decide which way to go. Anchored to the *badge* rather than centred on the
//! screen, because the whole point of the tray is to action a thing right
//! there, and a centred modal is not there.
//!
//! One widget carries all three contents — the yes/no, the jump, and the legend
//! behind the `···` button. Two mechanisms would be two things to build, two
//! things to dismiss, and two places for focus to get stuck.
//!
//! Two properties this module is responsible for holding:
//!
//! - **The buttons are laid out once.** [`buttons`] is what the renderer draws
//!   and what the hit test measures, so a button can never be drawn in one place
//!   and clicked in another — the same rule the tray's own slots follow.
//! - **A button that is refused is never drawn.** When
//!   [`crate::app::signal_tray::TrayBadge::refusal`] answers, the popup prints
//!   the reason and offers the jump instead. There is no disabled button to
//!   click twice and no path that reaches a command the badge refused.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::fleet_signals::FleetSignal;
use crate::app::signal_tray::{self, TrayAction, TrayBadge};
use crate::app::state::AppState;

/// The widest the popover will grow, in columns.
///
/// Wide enough for a prompt line from a blocked agent, narrow enough that it
/// still reads as a popover belonging to the sidebar rather than as a modal
/// that happens to be off-centre.
const MAX_WIDTH: u16 = 52;
/// The narrowest it is worth drawing. Below this the question would be one word
/// per line and the buttons would not fit beside each other.
const MIN_WIDTH: u16 = 24;

/// One thing the popover can be clicked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    /// Answer the blocked pane yes.
    Yes,
    /// Answer the blocked pane no.
    No,
    /// Run the badge's named command, which the popup has already printed.
    Run,
    /// Clear every unseen finish.
    Sweep,
    /// Go to the item this popup is pointed at.
    Open,
    /// Point at the next item this badge covers.
    Next,
}

impl Button {
    /// What this button says. The verb, never the noun: the popup's body has
    /// already said what it is about.
    fn label(self, badge: &TrayBadge) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Run => match badge.signal {
                FleetSignal::Push => "push",
                _ => "sync",
            },
            Self::Sweep => "mark all seen",
            Self::Open => match badge.signal {
                FleetSignal::Report => "open reports",
                _ => "open",
            },
            Self::Next => "next",
        }
    }
}

/// Which buttons this popup offers, left to right.
///
/// The one place the authority boundary turns into pixels. A badge whose action
/// is [`TrayAction::JumpOnly`] gets no acting button here, and neither does one
/// whose refusal has fired or whose in-place acts have been switched off in
/// config — in every one of those cases what is left is the jump.
pub(crate) fn buttons(app: &AppState, badge: &TrayBadge, item: usize) -> Vec<Button> {
    let mut buttons = Vec::new();
    let may_act = app.sidebar_signal_tray.actions && badge.refusal(app, item).is_none();

    if may_act {
        match badge.action {
            TrayAction::YesNo => {
                if badge.command(app, item, true).is_some() {
                    buttons.push(Button::Yes);
                    buttons.push(Button::No);
                }
            }
            TrayAction::Confirm => {
                if badge.command(app, item, true).is_some() {
                    buttons.push(Button::Run);
                }
            }
            TrayAction::JumpAndSweep => buttons.push(Button::Sweep),
            TrayAction::JumpOnly | TrayAction::OpenSummaries => {}
        }
    }

    buttons.push(Button::Open);
    if badge.items.len() > 1 {
        buttons.push(Button::Next);
    }
    buttons
}

/// The lines the popover's body draws, top to bottom.
///
/// Built here rather than in the renderer because the popover's height is
/// measured from them: a popup that reserved a different number of rows from
/// the ones it drew would clip its own buttons.
fn body_lines(app: &AppState, badge: &TrayBadge, item_index: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(item) = badge.item(item_index) else {
        return lines;
    };

    lines.push(item.label.clone());
    lines.extend(item.detail.iter().cloned());

    // The refusal comes before the command, because when there is a refusal
    // there is no command and the reader needs to know why.
    if let Some(refusal) = badge.refusal(app, item_index) {
        lines.push(String::new());
        lines.push(refusal);
    } else if let Some(command) = badge.command(app, item_index, true) {
        lines.push(String::new());
        lines.push(command.description(app));
    }

    if let Some(popup) = app.signal_tray.popup.as_ref() {
        if let Some(outcome) = popup.outcome.as_ref() {
            lines.push(String::new());
            lines.push(outcome.message.clone());
        }
    }
    lines
}

/// The legend: all eight names with one line of meaning each.
///
/// This is what makes the legend permanent and free — it is the thing that
/// stops the tray being eight pictures nobody can name.
fn legend_lines() -> Vec<String> {
    FleetSignal::ALL
        .into_iter()
        .map(|signal| format!("{:<8} {}", signal.name(), signal.meaning()))
        .collect()
}

/// Everything the popover needs, resolved once so the renderer and the hit test
/// cannot disagree about any of it.
pub(crate) struct PopupView {
    pub outer: Rect,
    pub inner: Rect,
    /// The cell the tail points down at, if it is on screen.
    pub tail: Option<(u16, u16)>,
    pub title: String,
    pub lines: Vec<String>,
    pub footer: &'static str,
    pub buttons: Vec<Button>,
    pub badge: TrayBadge,
    pub item: usize,
}

/// Resolve the open popover, or `None` when nothing is open or it cannot fit.
pub(crate) fn view(app: &AppState) -> Option<PopupView> {
    let popup = app.signal_tray.popup.as_ref()?;
    let reading = signal_tray::resolve(app);
    let badge = reading.badge(popup.signal).clone();

    let (title, lines, footer, buttons, item) = if popup.legend {
        (
            "signals".to_string(),
            legend_lines(),
            "esc close",
            Vec::new(),
            0,
        )
    } else {
        let item = popup.item % badge.items.len().max(1);
        let title = if badge.items.len() > 1 {
            format!(
                "{} · {} of {}",
                badge.signal.name(),
                item + 1,
                badge.items.len()
            )
        } else {
            badge.signal.name().to_string()
        };
        let footer = match badge.action {
            TrayAction::YesNo => "↵ open pane",
            _ => "esc close",
        };
        (
            title,
            body_lines(app, &badge, item),
            footer,
            buttons(app, &badge, item),
            item,
        )
    };

    let screen = app.screen_rect();
    let slot = anchor_slot(app, popup.signal);
    let width = MAX_WIDTH.min(screen.width.saturating_sub(2)).max(MIN_WIDTH);
    if screen.width < width + 2 || screen.height < 6 {
        return None;
    }

    // The body is wrapped *here*, not by the widget, so the rows it will occupy
    // and the rows the popover reserves are the same number by construction.
    // Measuring a `Wrap` after the fact cannot do that: it breaks on words, so a
    // long path takes more rows than dividing its width by the column count
    // predicts, and the popover overflows its own border by exactly the
    // difference.
    let content_width = width.saturating_sub(2).max(1);
    let lines = wrap_lines(&lines, content_width);

    // Height: two borders, the title, a blank, the body, a blank, the action row.
    let action_rows = u16::from(!buttons.is_empty() || !footer.is_empty());
    let wanted = 2 + 1 + 1 + lines.len() as u16 + 1 + action_rows;

    // Anchored above the *tray*, never over it: the badges have to stay visible
    // while their own popover is open, or clicking the badge again to dismiss it
    // means clicking something you cannot see.
    let tray = crate::ui::sidebar::tray::tray_rect(
        app,
        crate::ui::sidebar::sidebar_content_rect(app.view.sidebar_rect),
    );
    let bottom = if tray.height > 0 { tray.y } else { slot.y };
    let height = wanted.min(bottom.saturating_sub(screen.y));
    if height < 5 {
        return None;
    }

    let ideal_x = slot
        .x
        .saturating_add(slot.width / 2)
        .saturating_sub(width / 2);
    let x = ideal_x.min(screen.x + screen.width.saturating_sub(width));
    let outer = Rect::new(x, bottom.saturating_sub(height), width, height);

    // The tail is a nub cut into the popover's own bottom border rather than a
    // glyph on the row below it. That row belongs to the tray's header, and a
    // tail drawn there eats a letter out of the tray's name — which is what the
    // first capture of this showed. Cutting it into the border costs no row and
    // still points at the badge's column.
    let tail = (slot.width > 0).then(|| {
        (
            (slot.x + slot.width / 2).clamp(outer.x + 1, outer.x + outer.width.saturating_sub(2)),
            outer.y + outer.height.saturating_sub(1),
        )
    });

    let inner = Rect::new(
        outer.x + 1,
        outer.y + 1,
        outer.width.saturating_sub(2),
        outer.height.saturating_sub(2),
    );

    Some(PopupView {
        outer,
        inner,
        tail,
        title,
        lines,
        footer,
        buttons,
        badge,
        item,
    })
}

/// The slot the popover is anchored to.
///
/// The legend anchors on the `···` button it was opened from; a badge popup
/// anchors on its own slot. Both come out of the tray's own layout functions,
/// so the tail lands on the thing that was clicked.
fn anchor_slot(app: &AppState, signal: FleetSignal) -> Rect {
    let area = crate::ui::sidebar::sidebar_content_rect(app.view.sidebar_rect);
    let tray = crate::ui::sidebar::tray::tray_rect(app, area);
    if app.signal_tray.popup.as_ref().is_some_and(|p| p.legend) {
        return crate::ui::sidebar::tray::menu_rect(tray);
    }
    let grid = crate::ui::sidebar::tray::grid_rect(tray);
    let index = FleetSignal::ALL
        .iter()
        .position(|candidate| *candidate == signal)
        .unwrap_or(0);
    crate::ui::sidebar::tray::slot_rect(grid, index)
}

/// Break every line to `width` columns, on word boundaries where it can.
///
/// The popover draws these lines verbatim, so this is the only wrap in the
/// path and the reserved height is exact. A single word longer than the width
/// — a filesystem path, usually — is broken mid-word rather than allowed to
/// overflow, because a command the reader cannot see all of is not a
/// confirmation.
fn wrap_lines(lines: &[String], width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in line.split(' ') {
            let mut word = word;
            // A word that cannot fit on a line of its own is cut, in as many
            // pieces as it takes.
            while crate::ui::text::display_width(word) > width {
                let head: String = word.chars().take(width).collect();
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                word = &word[head.len()..];
                out.push(head);
            }
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if crate::ui::text::display_width(&candidate) > width {
                out.push(std::mem::take(&mut current));
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        out.push(current);
    }
    out
}

/// The action row's rect inside the popover.
fn action_row(inner: Rect) -> Rect {
    if inner.height == 0 {
        return Rect::default();
    }
    Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1)
}

/// Where each button is drawn, in the same order [`buttons`] returned them.
///
/// Left-aligned rather than centred: the footer hint sits at the right end of
/// the same row, and two centred groups on one row read as one long muddle.
pub(crate) fn button_rects(view: &PopupView) -> Vec<Rect> {
    let row = action_row(view.inner);
    let mut x = row.x;
    view.buttons
        .iter()
        .map(|button| {
            let text = crate::ui::widgets::action_button_text(None, button.label(&view.badge));
            let width = crate::ui::text::display_width_u16(&text);
            let rect = Rect::new(x, row.y, width.min(row.width.saturating_sub(x - row.x)), 1);
            x = x.saturating_add(width).saturating_add(1);
            rect
        })
        .collect()
}

/// Which button covers this cell, if any.
pub(crate) fn button_at(app: &AppState, col: u16, row: u16) -> Option<Button> {
    let view = view(app)?;
    button_rects(&view)
        .into_iter()
        .zip(view.buttons.iter().copied())
        .find(|(rect, _)| {
            rect.width > 0 && col >= rect.x && col < rect.x + rect.width && row == rect.y
        })
        .map(|(_, button)| button)
}

/// Whether this cell is inside the open popover at all.
///
/// Clicking away closes, so this is what tells "aimed at the popup" from "aimed
/// at anything else".
pub(crate) fn contains(app: &AppState, col: u16, row: u16) -> bool {
    view(app).is_some_and(|view| {
        col >= view.outer.x
            && col < view.outer.x + view.outer.width
            && row >= view.outer.y
            && row < view.outer.y + view.outer.height
    })
}

/// Draw the open popover over everything else.
pub(crate) fn render(app: &AppState, frame: &mut Frame) {
    let Some(view) = view(app) else { return };
    let p = &app.palette;
    // Peach when the badge is demanding attention, so the popover carries the
    // same hue the badge that opened it is wearing.
    let border = match view.badge.state {
        crate::app::signal_tray::BadgeState::Attention => p.peach,
        _ => p.accent,
    };
    let Some(inner) = crate::ui::widgets::render_panel_shell(frame, view.outer, border, p.panel_bg)
    else {
        return;
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            view.title.clone(),
            Style::default().fg(border).add_modifier(Modifier::BOLD),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let body = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(3),
    );
    if body.height > 0 {
        frame.render_widget(
            // Already wrapped by `view`, and deliberately not wrapped again:
            // the reserved height was measured from exactly these lines.
            Paragraph::new(
                view.lines
                    .iter()
                    .map(|line| Line::from(Span::styled(line.clone(), Style::default().fg(p.text))))
                    .collect::<Vec<_>>(),
            ),
            body,
        );
    }

    let row = action_row(inner);
    for (rect, button) in button_rects(&view).into_iter().zip(view.buttons.iter()) {
        if rect.width == 0 {
            continue;
        }
        // The two that act on the pane or the shell take the alert hue; the
        // ones that only navigate stay neutral. A reader should be able to tell
        // "this runs something" from "this takes me there" without reading.
        let acts = matches!(button, Button::Yes | Button::Run | Button::Sweep);
        let style = if acts {
            Style::default().fg(p.peach).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        crate::ui::widgets::render_action_button(
            frame,
            rect,
            None,
            button.label(&view.badge),
            style,
        );
    }
    if !view.footer.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(view.footer, Style::default().fg(p.overlay0)))
                .alignment(Alignment::Right),
            row,
        );
    }

    // The tail, last, so it replaces the border cell it is cut into rather than
    // being drawn under it.
    if let Some((x, y)) = view.tail {
        if x < frame.area().width && y < frame.area().height {
            let buf = frame.buffer_mut();
            buf[(x, y)].set_symbol("▽");
            buf[(x, y)].set_style(Style::default().fg(border));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::signal_tray::SignalTrayPopup;

    fn app_with_open_popup(signal: FleetSignal) -> AppState {
        let mut app = AppState::test_new();
        app.sidebar_signal_tray.enabled = true;
        app.view.sidebar_rect = Rect::new(0, 0, 42, 60);
        app.view.terminal_area = Rect::new(43, 0, 117, 60);
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.signal_tray.popup = Some(SignalTrayPopup {
            signal,
            item: 0,
            legend: false,
            outcome: None,
        });
        app
    }

    /// The popover opens *upward*, above the badge, always. The tray is at the
    /// foot of the panel, so there is never a decision to make.
    #[test]
    fn the_popover_opens_above_the_badge_it_came_from() {
        let mut app = app_with_open_popup(FleetSignal::Push);
        app.workspaces[0].cached_git_ahead_behind = Some((2, 0));

        let view = view(&app).expect("an open popup has a view");
        let area = crate::ui::sidebar::sidebar_content_rect(app.view.sidebar_rect);
        let tray = crate::ui::sidebar::tray::tray_rect(&app, area);
        // Wholly above the tray, not merely above the badge. The badges have to
        // stay visible while their own popover is open, or clicking the badge
        // again to dismiss it means clicking something you cannot see — and the
        // tray's own name is on the row a tail would otherwise land on.
        assert!(
            view.outer.y + view.outer.height <= tray.y,
            "the popover covered the tray it came from"
        );
        // The tail is cut into the popover's own bottom border, so it costs no
        // row outside the box and points at the badge's column.
        let (tail_x, tail_y) = view.tail.expect("a badge popup has a tail");
        assert_eq!(tail_y, view.outer.y + view.outer.height - 1);
        assert!(tail_x > view.outer.x && tail_x < view.outer.x + view.outer.width - 1);
    }

    /// The popover draws exactly the rows it reserved.
    ///
    /// The bug this pins: the height used to be measured by dividing each
    /// line's width by the column count, while the widget wrapped on word
    /// boundaries. A `git -C <long path> push origin <branch>` line takes one
    /// more row that way than the division predicts, and the body ran out
    /// through the bottom border and over the tray.
    #[test]
    fn the_body_never_draws_outside_the_border_it_reserved() {
        let mut app = app_with_open_popup(FleetSignal::Push);
        app.workspaces[0].cached_git_ahead_behind = Some((3, 0));
        app.workspaces[0].cached_git_branch = Some("fm/a-branch-with-a-long-name".into());
        app.workspaces[0].cached_identity_cwd =
            std::path::PathBuf::from("/home/someone/.treehouse/herdr-94dd9b/5/herdr");

        let view = view(&app).expect("an open popup has a view");
        let body_rows = view.inner.height.saturating_sub(3);
        assert!(
            view.lines.len() as u16 <= body_rows,
            "{} wrapped lines do not fit the {body_rows} rows reserved for them: {:#?}",
            view.lines.len(),
            view.lines
        );
        let width = usize::from(view.inner.width);
        for line in &view.lines {
            assert!(
                crate::ui::text::display_width(line) <= width,
                "{line:?} is wider than the {width} columns it is drawn in"
            );
        }
        // And the command is still there in full, across however many rows it
        // took: a confirmation the reader cannot see all of is not one.
        let joined = view.lines.join(" ");
        assert!(
            joined.contains("fm/a-branch-with-a-long-name") && joined.contains("push"),
            "the command was truncated: {joined}"
        );
    }

    /// A word longer than the popover is broken rather than allowed to overflow.
    #[test]
    fn a_word_too_long_for_the_box_is_cut_rather_than_spilled() {
        let long = "a".repeat(70);
        let wrapped = wrap_lines(std::slice::from_ref(&long), 20);
        assert!(wrapped.len() >= 4);
        for line in &wrapped {
            assert!(crate::ui::text::display_width(line) <= 20);
        }
        assert_eq!(wrapped.concat(), long);
    }

    /// A refused `sync` must print the reason and offer no button that would
    /// run it anyway. This is the safety boundary as the user meets it.
    #[test]
    fn a_refused_sync_shows_why_and_offers_only_the_jump() {
        let mut app = app_with_open_popup(FleetSignal::Sync);
        app.workspaces[0].cached_git_ahead_behind = Some((0, 4));
        app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts {
            staged: 0,
            unstaged: 2,
            untracked: 0,
        });

        let view = view(&app).expect("an open popup has a view");
        assert_eq!(view.buttons, vec![Button::Open]);
        assert!(
            view.lines.iter().any(|line| line.contains("uncommitted")),
            "the popup did not say why it refused: {:?}",
            view.lines
        );
    }

    /// A clean tree gets the button, and the popup prints the command it will
    /// run before there is anything to press.
    #[test]
    fn a_clean_sync_states_its_command_above_the_button() {
        let mut app = app_with_open_popup(FleetSignal::Sync);
        app.workspaces[0].cached_git_ahead_behind = Some((0, 4));
        app.workspaces[0].cached_git_dirty = Some(crate::workspace::GitDirtyCounts::default());
        app.workspaces[0].cached_git_branch = Some("feature".into());

        let view = view(&app).expect("an open popup has a view");
        assert!(view.buttons.contains(&Button::Run));
        // Joined, because the body is wrapped to the popover's width before it
        // is measured; the command may legitimately span rows.
        let joined = view.lines.join(" ");
        assert!(
            joined.contains("git -C")
                && joined.contains("pull --rebase")
                && joined.contains("feature"),
            "the command was not printed: {joined}"
        );
    }

    /// Turning the in-place acts off in config leaves every badge a jump, and
    /// must not leave a button that would run anything.
    #[test]
    fn config_can_take_every_acting_button_away() {
        let mut app = app_with_open_popup(FleetSignal::Push);
        app.workspaces[0].cached_git_ahead_behind = Some((2, 0));
        app.workspaces[0].cached_git_branch = Some("main".into());
        assert!(view(&app).expect("view").buttons.contains(&Button::Run));

        app.sidebar_signal_tray.actions = false;
        let view = view(&app).expect("view");
        assert_eq!(view.buttons, vec![Button::Open]);
    }

    /// The jump-only badges must never grow an acting button in the UI either,
    /// whatever the model would have said.
    #[test]
    fn a_jump_only_badge_offers_no_acting_button() {
        for signal in [FleetSignal::Stopped, FleetSignal::Pr, FleetSignal::Checks] {
            let mut app = app_with_open_popup(signal);
            app.workspaces[0].cached_pull_requests = Some(crate::forge::PullRequestCounts {
                review_requested: 1,
                checks_failing: 1,
                ..Default::default()
            });
            let view = view(&app).expect("view");
            assert!(
                view.buttons
                    .iter()
                    .all(|b| matches!(b, Button::Open | Button::Next)),
                "{signal:?} offered {:?}",
                view.buttons
            );
        }
    }

    /// Every button drawn has to be clickable at the rect it was drawn in.
    #[test]
    fn every_button_is_clickable_where_it_was_drawn() {
        let mut app = app_with_open_popup(FleetSignal::Push);
        app.workspaces[0].cached_git_ahead_behind = Some((2, 0));
        app.workspaces[0].cached_git_branch = Some("main".into());

        let view = view(&app).expect("view");
        for (rect, button) in button_rects(&view).into_iter().zip(view.buttons.iter()) {
            for x in rect.x..rect.x + rect.width {
                assert_eq!(
                    button_at(&app, x, rect.y),
                    Some(*button),
                    "at {x},{}",
                    rect.y
                );
            }
        }
    }

    #[test]
    fn the_legend_names_all_eight() {
        let mut app = app_with_open_popup(FleetSignal::Ask);
        app.signal_tray.popup.as_mut().expect("popup").legend = true;

        let view = view(&app).expect("view");
        assert_eq!(view.lines.len(), FleetSignal::COUNT);
        for signal in FleetSignal::ALL {
            assert!(
                view.lines
                    .iter()
                    .any(|line| line.starts_with(signal.name())),
                "the legend does not name {signal:?}"
            );
        }
        assert!(view.buttons.is_empty(), "the legend is not a control panel");
    }

    /// The popup must not index past a badge whose items shrank under it — a
    /// worker finishing while its popup is open is the common case.
    #[test]
    fn an_item_index_past_the_end_wraps_rather_than_panicking() {
        let mut app = app_with_open_popup(FleetSignal::Push);
        app.workspaces[0].cached_git_ahead_behind = Some((2, 0));
        app.signal_tray.popup.as_mut().expect("popup").item = 97;

        let view = view(&app).expect("view");
        assert_eq!(view.item, 0);
    }
}
