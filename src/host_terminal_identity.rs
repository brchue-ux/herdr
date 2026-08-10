//! Asking the terminal what it is, in band, with XTVERSION (`CSI > q`).
//!
//! herdr's other route to this fact is `TERM_PROGRAM` / `TERM` /
//! `KITTY_WINDOW_ID`, read from the client process's own environment
//! (`crate::kitty_graphics::host_terminal_report_from_env`). Those variables
//! are set by the terminal in the shell it spawns, and they do not cross an
//! SSH hop: a client attached over SSH sees no `TERM_PROGRAM` at all and a
//! generic `TERM`, so it classifies as `HostTerminalKind::Other` however real
//! its terminal is. Measured on a live fleet, where *every* client — local and
//! remote alike — logged `classified_kind=Other`
//! (`data/herdr-rio-render-capability-research-20260810/report.md` §4,
//! firstmate home). No allowlist can reach a terminal that was never named.
//!
//! XTVERSION travels the other way round. herdr writes `CSI > q` down the same
//! pty it writes every other escape to, and the terminal answers on that pty's
//! input side — so the answer survives exactly the hops the rendering itself
//! survives, which is the property the environment lacks. Replies measured
//! against real binaries in the report above:
//!
//! | terminal | reply |
//! |---|---|
//! | Rio 0.5.19 | `DCS > \| Rio 0.5.19 ST` |
//! | kitty 0.45.0 | `DCS > \| kitty(0.45.0) ST` |
//!
//! Both name-and-version shapes are in the wild — `name version` and
//! `name(version)` — so both are parsed here. The version is carried rather
//! than discarded so that a per-terminal capability policy *can* tell one
//! release from another if it ever needs to. Note what it cannot do: the Rio
//! compositing and animation-frame bugs that once made
//! `HostTerminalKind::draws_ambient_wash` refuse Rio are fixed in a private
//! downstream build rather than an upstream release, and that build still
//! answers `Rio 0.5.19` — identical to an unpatched 0.5.19. So the wash gate
//! allows Rio by name alone; no version here distinguishes the two.

/// XTVERSION. A terminal that does not implement it simply does not answer,
/// which the caller treats as "keep the environment's guess" rather than
/// something this module has to detect.
pub(crate) const HOST_TERMINAL_VERSION_QUERY_SEQUENCE: &str = "\x1b[>q";

/// What the terminal called itself when asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostTerminalIdentity {
    name: String,
    version: Option<String>,
}

impl HostTerminalIdentity {
    /// The terminal's self-reported name, exactly as it wrote it — `Rio`,
    /// `kitty`, `WezTerm`. Compare case-insensitively; the capitalisation is
    /// the terminal's own branding and is not a stable key.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The version string as reported, unparsed. `None` when the terminal
    /// named itself without one.
    pub(crate) fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn new(name: &str, version: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            version: version.map(str::to_owned),
        }
    }
}

/// Parses an XTVERSION reply — `DCS > | <name and version> ST` — into the
/// terminal's identity.
///
/// Returns `None` for anything that is not a well-formed XTVERSION reply
/// carrying a name, so the caller can fall through to the other escape
/// parsers exactly as it does for the Kitty capability probe. Accepts either
/// ST form: `ESC \` as the spec writes it, and the single-byte `BEL` that
/// terminals commonly accept and occasionally emit.
pub(crate) fn parse_host_terminal_version_response(sequence: &str) -> Option<HostTerminalIdentity> {
    let body = sequence.strip_prefix('\u{1b}')?.strip_prefix('P')?;
    let body = body
        .strip_suffix("\u{1b}\\")
        .or_else(|| body.strip_suffix('\u{7}'))?;
    // `>|` is the XTVERSION response marker. A DCS carrying anything else is
    // some other terminal report and is not ours to read.
    let text = body.strip_prefix(">|")?.trim();
    split_name_and_version(text)
}

/// Splits `kitty(0.45.0)` and `Rio 0.5.19` alike into name and version.
///
/// A bare name with no version is a valid identity — the name alone is what
/// classification keys on. A bare version with no name is not, and neither is
/// an empty reply.
fn split_name_and_version(text: &str) -> Option<HostTerminalIdentity> {
    if let Some(open) = text.find('(') {
        if let Some(inner) = text.strip_suffix(')') {
            let (name, version) = inner.split_at(open);
            let name = name.trim();
            // `version` still carries the `(`.
            let version = version[1..].trim();
            if !name.is_empty() {
                return Some(HostTerminalIdentity {
                    name: name.to_owned(),
                    version: (!version.is_empty()).then(|| version.to_owned()),
                });
            }
        }
    }

    let (name, version) = match text.split_once(char::is_whitespace) {
        Some((name, version)) => (name, Some(version.trim())),
        None => (text, None),
    };
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(HostTerminalIdentity {
        name: name.to_owned(),
        version: version.filter(|value| !value.is_empty()).map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both replies are transcripts of real binaries answering `CSI > q` under
    /// Xvfb, not constructed examples — see the module doc.
    #[test]
    fn parses_the_two_measured_replies() {
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|Rio 0.5.19\x1b\\"),
            Some(HostTerminalIdentity::new("Rio", Some("0.5.19")))
        );
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|kitty(0.45.0)\x1b\\"),
            Some(HostTerminalIdentity::new("kitty", Some("0.45.0")))
        );
    }

    #[test]
    fn parses_other_terminals_in_the_wild() {
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|WezTerm 20240203-110809\x1b\\"),
            Some(HostTerminalIdentity::new(
                "WezTerm",
                Some("20240203-110809")
            ))
        );
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|XTerm(370)\x1b\\"),
            Some(HostTerminalIdentity::new("XTerm", Some("370")))
        );
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|ghostty 1.0.0\x1b\\"),
            Some(HostTerminalIdentity::new("ghostty", Some("1.0.0")))
        );
    }

    #[test]
    fn accepts_bel_terminated_replies() {
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|Rio 0.5.19\x07"),
            Some(HostTerminalIdentity::new("Rio", Some("0.5.19")))
        );
    }

    #[test]
    fn a_name_without_a_version_is_still_an_identity() {
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|tmux\x1b\\"),
            Some(HostTerminalIdentity::new("tmux", None))
        );
    }

    #[test]
    fn rejects_sequences_that_are_not_xtversion_replies() {
        // A DCS that is some other terminal report.
        assert_eq!(
            parse_host_terminal_version_response("\x1bP1$r0m\x1b\\"),
            None
        );
        // The Kitty capability probe's own reply, which shares the framing.
        assert_eq!(
            parse_host_terminal_version_response("\x1b_Gi=1;OK\x1b\\"),
            None
        );
        // Unterminated.
        assert_eq!(
            parse_host_terminal_version_response("\x1bP>|Rio 0.5.19"),
            None
        );
        // Answered the query with no name at all.
        assert_eq!(parse_host_terminal_version_response("\x1bP>|\x1b\\"), None);
    }

    #[test]
    fn the_query_is_the_xtversion_sequence() {
        assert_eq!(HOST_TERMINAL_VERSION_QUERY_SEQUENCE.as_bytes(), b"\x1b[>q");
    }
}
