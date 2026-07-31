//! Best-effort source locations for config diagnostics.
//!
//! Diagnostics are produced as plain strings all over `config/`, and most of
//! them already name the offending key path (`ui.sidebar_min_width`,
//! `keys.command[0].key`, `theme.custom`). This module re-parses the config
//! source with span information and resolves those key paths back to a
//! `line:column`, so `herdr config check` can point at the offending line
//! instead of leaving the user to search the file by hand.
//!
//! Resolution is deliberately best-effort: a diagnostic whose key path cannot
//! be found in the source (or a source that no longer parses) simply keeps its
//! message with no location attached.

use toml_edit::{ImDocument, Item};

/// A resolved position inside a config file. Both are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigLocation {
    pub line: usize,
    pub column: usize,
}

/// A diagnostic together with the place in the config file it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedDiagnostic {
    /// The diagnostic text as produced by the config loaders, without any
    /// location suffix.
    pub message: String,
    pub location: Option<ConfigLocation>,
}

impl LocatedDiagnostic {
    /// The user-facing rendering: message plus `(config.toml:12:3)` when the
    /// key path could be resolved.
    pub fn render(&self, file_label: &str) -> String {
        match self.location {
            Some(location) => format!(
                "{} ({file_label}:{}:{})",
                self.message, location.line, location.column
            ),
            None => self.message.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Key(String),
    Index(usize),
}

/// Span-aware view of a config file.
pub(super) struct ConfigSource {
    doc: ImDocument<String>,
    line_starts: Vec<usize>,
}

impl ConfigSource {
    pub(super) fn parse(content: &str) -> Option<Self> {
        let doc = ImDocument::parse(content.to_string()).ok()?;
        let mut line_starts = vec![0];
        line_starts.extend(
            content
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Some(Self { doc, line_starts })
    }

    fn location_at(&self, offset: usize) -> ConfigLocation {
        let line_index = match self.line_starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let line_start = self.line_starts.get(line_index).copied().unwrap_or(0);
        let column = self
            .doc
            .raw()
            .get(line_start..offset)
            .map(|prefix| prefix.chars().count())
            .unwrap_or(0);
        ConfigLocation {
            line: line_index + 1,
            column: column + 1,
        }
    }

    /// Resolve a key path such as `keys.command[0].key` to a position.
    ///
    /// Falls back to the longest resolvable prefix, so a diagnostic about a
    /// value inside a table still points at the table it lives in.
    fn locate(&self, path: &[Segment]) -> Option<ConfigLocation> {
        for length in (1..=path.len()).rev() {
            if let Some(offset) = self.offset_of(&path[..length]) {
                return Some(self.location_at(offset));
            }
        }
        None
    }

    fn offset_of(&self, path: &[Segment]) -> Option<usize> {
        let mut cursor = Cursor::Item(self.doc.as_item());
        let mut offset = None;

        for segment in path {
            match segment {
                Segment::Key(key) => {
                    match cursor
                        .as_table_like()
                        .and_then(|table| table.get_key_value(key))
                    {
                        Some((found_key, found_item)) => {
                            offset = found_key
                                .span()
                                .or_else(|| found_item.span())
                                .map(|span| span.start)
                                .or(offset);
                            cursor = Cursor::Item(found_item);
                        }
                        // `format_config_key_path` renders array indices as
                        // plain dotted segments (`keys.command.0.key`), so a
                        // numeric key that is not a real key is an index.
                        None => {
                            let index = key.parse().ok()?;
                            cursor = cursor.index(index, &mut offset)?;
                        }
                    }
                }
                Segment::Index(index) => {
                    cursor = cursor.index(*index, &mut offset)?;
                }
            }
        }

        offset
    }
}

/// A position in the document tree. Array-of-tables entries are `Table`s rather
/// than `Item`s, so the walk cannot stay on `&Item` alone.
enum Cursor<'a> {
    Item(&'a Item),
    Table(&'a toml_edit::Table),
    Value(&'a toml_edit::Value),
}

impl<'a> Cursor<'a> {
    fn as_table_like(&self) -> Option<&'a dyn toml_edit::TableLike> {
        match self {
            Self::Item(item) => item.as_table_like(),
            Self::Table(table) => Some(*table),
            Self::Value(value) => value
                .as_inline_table()
                .map(|table| table as &dyn toml_edit::TableLike),
        }
    }

    fn index(&self, index: usize, offset: &mut Option<usize>) -> Option<Cursor<'a>> {
        if let Some(array) = self.array() {
            let value = array.get(index)?;
            *offset = value.span().map(|span| span.start).or(*offset);
            return Some(Cursor::Value(value));
        }

        let Self::Item(item) = self else {
            return None;
        };
        let table = item.as_array_of_tables()?.get(index)?;
        *offset = table.span().map(|span| span.start).or(*offset);
        Some(Cursor::Table(table))
    }

    fn array(&self) -> Option<&'a toml_edit::Array> {
        match self {
            Self::Item(item) => item.as_array(),
            Self::Value(value) => value.as_array(),
            Self::Table(_) => None,
        }
    }
}

/// Attach locations to a batch of diagnostics produced while loading `content`.
pub(super) fn locate_diagnostics(
    content: &str,
    diagnostics: Vec<String>,
) -> Vec<LocatedDiagnostic> {
    let source = ConfigSource::parse(content);
    diagnostics
        .into_iter()
        .map(|message| {
            let location = source
                .as_ref()
                .and_then(|source| locate_message(source, &message));
            LocatedDiagnostic { message, location }
        })
        .collect()
}

fn locate_message(source: &ConfigSource, message: &str) -> Option<ConfigLocation> {
    candidate_paths(message)
        .into_iter()
        .filter_map(|candidate| parse_path(&candidate))
        .find_map(|path| source.locate(&path))
}

/// Pull the key paths a diagnostic mentions, most specific first.
fn candidate_paths(message: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Unknown key/section diagnostics name a single path right after a fixed
    // prefix, and that path may be a bare top-level key with no dot in it.
    for prefix in ["unknown config key ", "unknown config section "] {
        if let Some(rest) = message.strip_prefix(prefix) {
            let token = rest.split(';').next().unwrap_or(rest).trim();
            // Section diagnostics quote the header, `[ui]` or `[[plugin]]`.
            let token = token
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_end_matches('?')
                .trim();
            if !token.is_empty() {
                candidates.push(token.to_string());
            }
        }
    }

    // Everything else embeds the field path inside a sentence.
    for token in message.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')'))
    {
        let token = token.trim_matches(|ch: char| matches!(ch, ':' | '.' | '=' | '"' | '\''));
        if token.len() < 3 || !token.contains('.') {
            continue;
        }
        if !token.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_') {
            continue;
        }
        candidates.push(token.to_string());
    }

    candidates.dedup();
    candidates
}

/// Split `keys.command[0].key` into segments, honouring TOML key quoting.
fn parse_path(candidate: &str) -> Option<Vec<Segment>> {
    let mut segments = Vec::new();
    let mut rest = candidate;

    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('[') {
            let (index, remainder) = tail.split_once(']')?;
            segments.push(Segment::Index(index.trim().parse().ok()?));
            rest = remainder.strip_prefix('.').unwrap_or(remainder);
            continue;
        }

        let end = rest.find('[').unwrap_or(rest.len());
        let (head, remainder) = rest.split_at(end);
        for key in toml_edit::Key::parse(head).ok()? {
            segments.push(Segment::Key(key.get().to_string()));
        }
        rest = remainder;
    }

    (!segments.is_empty()).then_some(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"onboarding = true

[ui]
sidebar_min_width = 90
sidebar_max_width = 40

[theme]
custom = "nope"

[[keys.command]]
key = "ctrl+q"
command = ""
"#;

    fn locate(message: &str) -> Option<ConfigLocation> {
        let source = ConfigSource::parse(SAMPLE).expect("sample parses");
        locate_message(&source, message)
    }

    #[test]
    fn locates_unknown_nested_key() {
        assert_eq!(
            locate("unknown config key theme.custom; ignoring key"),
            Some(ConfigLocation { line: 8, column: 1 })
        );
    }

    #[test]
    fn locates_unknown_top_level_section_header() {
        assert_eq!(
            locate("unknown config section [theme]; ignoring section")
                .map(|location| location.line),
            Some(7)
        );
        assert_eq!(
            locate("unknown config key theme; ignoring key").map(|location| location.line),
            Some(7)
        );
    }

    #[test]
    fn locates_dotted_index_segments() {
        assert_eq!(
            locate("unknown config key keys.command.0.descrption; ignoring key")
                .map(|location| location.line),
            Some(10)
        );
    }

    #[test]
    fn locates_value_diagnostic_mentioning_a_field_path() {
        assert_eq!(
            locate("ui.sidebar_min_width (90) is greater than sidebar_max_width (40)")
                .map(|location| location.line),
            Some(4)
        );
    }

    #[test]
    fn locates_indexed_keybinding_field() {
        assert_eq!(
            locate("empty custom command: keys.command[0].command; disabling custom command")
                .map(|location| location.line),
            Some(12)
        );
    }

    #[test]
    fn falls_back_to_the_longest_resolvable_prefix() {
        assert_eq!(
            locate("invalid keybinding: keys.command[0].missing = \"x\"; disabling binding")
                .map(|location| location.line),
            Some(10)
        );
    }

    #[test]
    fn leaves_unrelated_diagnostics_unlocated() {
        assert_eq!(locate("config read error: boom; using defaults"), None);
    }

    #[test]
    fn renders_without_a_location_when_unresolved() {
        let diagnostic = LocatedDiagnostic {
            message: "config read error: boom".to_string(),
            location: None,
        };
        assert_eq!(diagnostic.render("config.toml"), "config read error: boom");
    }

    #[test]
    fn tolerates_unparsable_sources() {
        let located = locate_diagnostics("[ui", vec!["unknown config key ui.x".to_string()]);
        assert_eq!(located.len(), 1);
        assert!(located[0].location.is_none());
    }
}
