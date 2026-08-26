use std::collections::BTreeMap;

use crate::workspace::{GitDiffLine, GitDiffText};

/// Per-pane record of agent-driven file edits, keyed by absolute file path.
/// Each file's entry is always the diff against that file's *session-start*
/// content — callers replace (never append to) a file's entry on every
/// report, which is what makes repeated edits to one file collapse into a
/// single cumulative diff instead of stacking fragments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentEditLog {
    entries: BTreeMap<String, GitDiffText>,
}

// Only this module's own tests call these today. The `pane.report_edit_diff`
// RPC handler and the Changes-zone renderer are the production callers, and
// they land in later tasks of this feature.
#[cfg_attr(not(test), allow(dead_code))]
impl AgentEditLog {
    /// Replaces `path`'s entry with `diff`, or removes it entirely if
    /// `diff` has no lines (an edit that reverted the file back to its
    /// baseline reports an empty diff; that should clear the file's card,
    /// not leave a stale "0 changes" one behind).
    pub(crate) fn set_or_clear(&mut self, path: String, diff: GitDiffText) {
        if diff.lines.is_empty() {
            self.entries.remove(&path);
        } else {
            self.entries.insert(path, diff);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Flattens every file's lines into one stream, files in path-sorted
    /// order, for the renderer to consume exactly like a single
    /// `GitDiffText` today.
    pub(crate) fn flatten(&self) -> Vec<GitDiffLine> {
        self.entries
            .values()
            .flat_map(|text| text.lines.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::GitDiffLineKind;

    fn line(kind: GitDiffLineKind, text: &str) -> GitDiffLine {
        GitDiffLine {
            kind,
            text: text.to_string(),
        }
    }

    fn sample_diff(marker: &str) -> GitDiffText {
        GitDiffText {
            lines: vec![
                line(GitDiffLineKind::FileHeader, &format!("--- a/{marker}")),
                line(GitDiffLineKind::Added, &format!("+{marker} content")),
            ],
            truncated: false,
        }
    }

    #[test]
    fn new_log_is_empty() {
        let log = AgentEditLog::default();
        assert!(log.is_empty());
        assert_eq!(log.flatten(), Vec::new());
    }

    #[test]
    fn set_or_clear_inserts_a_new_file_entry() {
        let mut log = AgentEditLog::default();
        log.set_or_clear("foo.rs".into(), sample_diff("foo.rs"));
        assert!(!log.is_empty());
        assert_eq!(log.flatten(), sample_diff("foo.rs").lines);
    }

    #[test]
    fn set_or_clear_replaces_not_appends_on_repeat_calls() {
        let mut log = AgentEditLog::default();
        log.set_or_clear("foo.rs".into(), sample_diff("foo.rs"));
        log.set_or_clear(
            "foo.rs".into(),
            GitDiffText {
                lines: vec![line(GitDiffLineKind::Added, "+second edit")],
                truncated: false,
            },
        );
        assert_eq!(
            log.flatten(),
            vec![line(GitDiffLineKind::Added, "+second edit")]
        );
    }

    #[test]
    fn set_or_clear_with_empty_diff_removes_the_entry() {
        let mut log = AgentEditLog::default();
        log.set_or_clear("foo.rs".into(), sample_diff("foo.rs"));
        log.set_or_clear(
            "foo.rs".into(),
            GitDiffText {
                lines: vec![],
                truncated: false,
            },
        );
        assert!(log.is_empty());
    }

    #[test]
    fn flatten_orders_files_by_path() {
        let mut log = AgentEditLog::default();
        log.set_or_clear("zebra.rs".into(), sample_diff("zebra.rs"));
        log.set_or_clear("alpha.rs".into(), sample_diff("alpha.rs"));
        let flattened = log.flatten();
        assert_eq!(flattened[0].text, "--- a/alpha.rs");
        assert_eq!(flattened[2].text, "--- a/zebra.rs");
    }

    #[test]
    fn clear_empties_all_entries() {
        let mut log = AgentEditLog::default();
        log.set_or_clear("foo.rs".into(), sample_diff("foo.rs"));
        log.set_or_clear("bar.rs".into(), sample_diff("bar.rs"));
        log.clear();
        assert!(log.is_empty());
    }
}
