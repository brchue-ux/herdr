use crate::terminal::TerminalId;

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub attached_terminal_id: TerminalId,
    /// Whether new output has arrived on this pane since it was last viewed.
    /// False = unread: content arrived while the pane's tab was not active.
    pub seen: bool,
}

impl PaneState {
    pub fn new(attached_terminal_id: TerminalId) -> Self {
        Self {
            attached_terminal_id,
            seen: true,
        }
    }

    /// Restore a pane with a persisted `seen` value instead of the `true`
    /// default `new` uses for a genuinely new pane.
    pub fn restored(attached_terminal_id: TerminalId, seen: bool) -> Self {
        Self {
            attached_terminal_id,
            seen,
        }
    }
}
