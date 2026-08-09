//! What a card *chooses* to say, as opposed to what the fleet published.
//!
//! # Why this exists
//!
//! The card's title is the `doing` token — a free-text summary a fleet agent
//! writes about itself — and until now Herdr set it verbatim, on the rule that
//! "a title that does not read wants a better summary, and producing that
//! summary is the fleet's job rather than Herdr's". Two lines of 14 px type is
//! a small budget, and the captain's answer on 2026-08-09 moved that line:
//!
//! > *"herdr will just need to be better about what it chooses to display as
//! > current working summary."*
//!
//! So this module is the choosing. It never invents words and never shortens
//! one — every transform here either removes ink the card is *already*
//! carrying somewhere else, or drops a whole trailing clause. What survives is
//! always a subsequence of the publisher's own words.
//!
//! # The two halves, and why they are separate
//!
//! [`condense`] is the part that does not depend on how wide the card is:
//! whitespace, code fences, quotes, the "Currently working on" that the state
//! chip beside it already says, the full stop at the end of a fragment. That is
//! *always* redundant, so it is applied once where a card's content is built
//! and the clean string is what gets hashed, cached and sent to remote clients.
//!
//! [`candidates`] is the part that does: a ladder of progressively shorter
//! renderings, cheapest loss first, for the fitter to walk until one sets whole
//! in the column this particular card happens to have. Nothing here measures
//! text — the ladder is pure string policy, so it is testable without a font
//! and stays honest about *what* it gives up rather than about how many pixels
//! that saved.

/// Head phrases a summary is stripped of before it is ever measured.
///
/// Every one of these is on the card twice. The chip to the right of the title
/// says `WORKING`, so "Working on " spends a word and a half of a two-line
/// budget repeating it; "Currently " and "I am " are a publisher writing a
/// sentence where the card wants a label. `"task: "` and its neighbours are the
/// same thing from a publisher writing a field.
///
/// Deliberately short, and deliberately unambiguous. `"still "` is not here
/// even though it is filler, because "Still fixing the flake" says something
/// "Fixing the flake" does not; `"working "` is not here because it would turn
/// "Working through the backlog" into "through the backlog". A phrase earns a
/// place only when dropping it cannot change what the reader concludes.
///
/// Matched lowercase against a lowercased head, so the publisher's own casing
/// survives into what is drawn.
const REDUNDANT_HEADS: &[&str] = &[
    "currently ",
    "current: ",
    "now ",
    "i am ",
    "i'm ",
    "working on: ",
    "working on ",
    "work on ",
    "task: ",
    "doing: ",
    "status: ",
    "summary: ",
    "wip: ",
];

/// How many times the head is stripped before giving up.
///
/// "Currently working on " is two passes and is the realistic worst case; the
/// bound is here so a pathological string cannot spin, not because three is
/// meaningful.
const MAX_HEAD_STRIPS: usize = 3;

/// The shortest a stripped summary is allowed to get.
///
/// Below this the strip has eaten the summary rather than its preamble — a
/// `doing` of exactly `"working on"` should stay `"working on"` rather than
/// become an empty card.
const MIN_STRIPPED_CHARS: usize = 3;

/// Where a summary divides into "the main thing" and "the also thing".
///
/// Every entry carries its own spaces, and that is load-bearing: a comma with
/// no space after it is a number, an em dash with no spaces is a compound, and
/// a bare `-` is the hyphen in `pre-release`.
///
/// # Why the conjunctions are here and not only the punctuation
///
/// Because the punctuation alone does nothing for the summaries this actually
/// has to fit. The longest real fleet title —
/// *"Investigateing killed Okta corpus and Herdr work sessions"* — has no
/// punctuation at all, so a ladder built only out of commas and dashes had one
/// rung and the card fell straight through to a greedy wrap ending on the word
/// *"and"*: a sentence visibly sliced, which is the exact failure the captain
/// is looking at. Cutting at the conjunction instead leaves
/// *"Investigateing killed Okta corpus"*, which is a finished phrase.
///
/// These are the coordinators and subordinators that join two whole thoughts.
/// `" of "`, `" to "`, `" in "` and the rest of the prepositions are
/// deliberately absent — they join a phrase to its own object, and cutting
/// there leaves a fragment that reads as damage rather than as brevity.
const CLAUSE_BREAKS: &[&str] = &[
    " — ",
    " – ",
    " · ",
    "; ",
    " - ",
    ", ",
    " and ",
    " then ",
    " plus ",
    " but ",
    " while ",
    " before ",
    " after ",
    " because ",
    " so that ",
];

/// Everything about a summary that is redundant whatever width it is set in.
///
/// Idempotent: the output of this is a fixed point of it, which is what lets a
/// caller apply it at content-build time and a test apply it again without
/// having to reason about which one ran.
///
/// # Why nothing here changes a character's case
///
/// Stripping "Currently working on " leaves a summary that starts lowercase,
/// and the obvious tidy-up is to put the sentence case back. It was written and
/// then taken out: there is no lexical test that separates a verb from a name.
/// *"Working on herdr card trim"* and *"Working on fixing the flake"* have the
/// same shape — an all-lowercase alphabetic first word — and capitalising the
/// first turns the captain's project into a sentence, which is Herdr printing
/// something false on a card about someone else's work. A lowercase label reads
/// as a label; a renamed crate reads as a bug. So the rule is absolute and the
/// guarantee is exact: **what the card draws is the publisher's own characters,
/// with some of them left out.**
pub(super) fn condense(raw: &str) -> String {
    let flattened = flatten_whitespace(&raw.replace('`', ""));
    if flattened.is_empty() {
        return String::new();
    }
    let unquoted = strip_matched_quotes(&flattened);
    let stripped = strip_redundant_head(unquoted);
    let trimmed = strip_trailing_punctuation(stripped);
    if trimmed.is_empty() {
        return flattened;
    }
    trimmed.to_string()
}

/// Progressively shorter renderings of an already-[`condense`]d summary,
/// cheapest loss first.
///
/// The first entry is the summary itself, so a caller that walks this and stops
/// at the first thing that fits does nothing at all to a title that already
/// fits — which is the common case and has to stay free.
///
/// # The order is the whole design
///
/// 1. **As published.** Nothing given up.
/// 2. **Paths shortened to their last segment.** A coding agent's summary is
///    full of `src/ui/sidebar/image_card.rs`, which is forty pixels of
///    directory the reader already knows and eight of the file they do not.
///    This is the one transform that reliably buys a whole line back, and it
///    costs nothing a reader of *this* tree was using.
/// 3. **Parenthetical asides dropped.** An aside is marked as an aside by the
///    person who wrote it.
/// 4. **First sentence only.** A summary that is two sentences is a summary
///    whose second sentence is the part that can wait.
/// 5. **Trailing clauses dropped, one at a time.** Last, because this is where
///    the card starts losing something the publisher meant to say — and even
///    here it ends on a clause boundary, so what is drawn reads as a finished
///    phrase rather than as a sentence that was cut.
///
/// Never an ellipsis, and never a shortened word: a mid-word cut is the thing
/// the captain ruled out, and a trailing `…` is Herdr admitting it gave up on a
/// card whose whole job is to be read at a glance.
pub(super) fn candidates(summary: &str) -> Ladder {
    let mut rungs = vec![summary.to_string()];
    push_distinct(&mut rungs, shorten_paths(summary));
    let deparenthesised = drop_asides(last(&rungs));
    push_distinct(&mut rungs, deparenthesised);

    // Everything above this line only removed ink the card was carrying twice;
    // everything below it gives up something the publisher meant to say. The
    // boundary is recorded rather than counted, because a rung that changed
    // nothing is deduplicated away and a positional "the third one" would then
    // point at a rung that had already started cutting.
    let lossless = rungs.len() - 1;

    // Every content-losing step is measured against the *cheapest* rendering
    // rather than against the raw one, so a caller that reaches them has
    // already banked the free savings.
    let sentence = first_sentence(last(&rungs));
    push_distinct(&mut rungs, sentence);

    let mut clause = last(&rungs).to_string();
    while let Some(shorter) = drop_last_clause(&clause) {
        clause = shorter;
        push_distinct(&mut rungs, clause.clone());
    }
    Ladder { rungs, lossless }
}

/// Progressively shorter renderings of one summary, and where they stop being
/// free.
pub(super) struct Ladder {
    rungs: Vec<String>,
    /// Index of the last rung that gave up no content.
    lossless: usize,
}

impl Ladder {
    pub(super) fn rungs(&self) -> impl Iterator<Item = &str> {
        self.rungs.iter().map(String::as_str)
    }

    /// The rung a caller falls back to when nothing on the ladder sets whole.
    ///
    /// The cheapest rendering that gave up no content. Past that point every
    /// rung only removes a *tail*, and a tail a wrap that already overflowed
    /// was never going to reach: the words that actually get drawn are the same
    /// either way, so falling back to the deepest rung would buy nothing and
    /// would claim the card had made a choice it did not make.
    pub(super) fn lossless(&self) -> &str {
        self.rungs.get(self.lossless).map_or("", String::as_str)
    }
}

fn last(rungs: &[String]) -> &str {
    rungs.last().map_or("", String::as_str)
}

fn push_distinct(ladder: &mut Vec<String>, candidate: String) {
    if candidate.is_empty() || ladder.contains(&candidate) {
        return;
    }
    ladder.push(candidate);
}

/// Every run of whitespace — including the newlines and tabs a publisher's
/// heredoc leaves behind — collapsed to one space.
fn flatten_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One layer of matched wrapping quotes, when the whole string is inside them.
///
/// Matched only: a summary that opens a quote and does not close it is a
/// summary quoting something, and cutting the opener there would be editing it.
fn strip_matched_quotes(text: &str) -> &str {
    for (open, close) in [('"', '"'), ('\'', '\''), ('“', '”'), ('‘', '’')] {
        if let Some(inner) = text.strip_prefix(open).and_then(|t| t.strip_suffix(close)) {
            if !inner.is_empty() {
                return inner.trim();
            }
        }
    }
    text
}

fn strip_redundant_head(text: &str) -> &str {
    let mut head = text;
    for _ in 0..MAX_HEAD_STRIPS {
        let lowered = head.to_lowercase();
        let Some(matched) = REDUNDANT_HEADS
            .iter()
            .find(|phrase| lowered.starts_with(**phrase))
        else {
            return head;
        };
        let rest = head[matched.len()..].trim_start();
        if rest.chars().count() < MIN_STRIPPED_CHARS {
            return head;
        }
        head = rest;
    }
    head
}

/// The full stop at the end of a fragment, and any separator left dangling.
///
/// `?` and `!` stay: a publisher that ended on one meant it. `…` goes, because
/// a publisher's own ellipsis is the same "there is more" the card is already
/// unable to show and reads as Herdr's truncation rather than as theirs.
fn strip_trailing_punctuation(text: &str) -> &str {
    text.trim_end_matches(['.', '…', ',', ';', ':', '-', '–', '—'])
        .trim_end()
}

/// Path-like words reduced to their last segment.
///
/// A word qualifies when it holds a `/` with something either side and no
/// character a path does not have. The last segment has to be non-empty — a
/// trailing slash means the word *is* the directory, and `src/` shortened to
/// nothing would delete it.
fn shorten_paths(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let core = word.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | ',' | '.'));
            if !looks_like_path(core) {
                return word.to_string();
            }
            let Some(last) = core.rsplit('/').next().filter(|s| !s.is_empty()) else {
                return word.to_string();
            };
            word.replacen(core, last, 1)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a word is a filesystem path rather than a word with a slash in it.
///
/// A single slash between two bare words is not enough: `and/or`, `he/she` and
/// `CI/CD` all pass that test, and shortening them to their tail deletes half
/// the meaning. One of three stronger marks has to be there —
///
/// * an anchor the writer typed on purpose (`/`, `./`, `../`, `~/`),
/// * two or more separators, which no prose idiom has, or
/// * a last segment with a file extension on it.
///
/// A URL is excluded outright even though it passes all three: its last segment
/// alone is a bare number or slug, and the host is the part a reader can use.
fn looks_like_path(word: &str) -> bool {
    if word.contains("://") {
        return false;
    }
    let slashes = word.matches('/').count();
    if slashes == 0 {
        return false;
    }
    let path_chars = word
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+'));
    if !path_chars {
        return false;
    }
    let anchored = word.starts_with('/')
        || word.starts_with("./")
        || word.starts_with("../")
        || word.starts_with("~/");
    let extension = word
        .rsplit('/')
        .next()
        .and_then(|tail| tail.rsplit_once('.'))
        .is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=5).contains(&ext.chars().count())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
    anchored || slashes >= 2 || extension
}

/// Bracketed asides removed, with the space they leave behind.
///
/// Unmatched brackets are left exactly as they are: a `(` with no `)` is not an
/// aside, it is a summary that mentions a bracket, and dropping to the end of
/// the string there would be the truncation this module exists to avoid.
fn drop_asides(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(['(', '[']) {
        let closer = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(close) = rest[open..].find(closer) else {
            break;
        };
        out.push_str(&rest[..open]);
        rest = &rest[open + close + closer.len_utf8()..];
    }
    out.push_str(rest);
    flatten_whitespace(&out)
}

/// Everything up to the first sentence end, when there is a second sentence.
///
/// A sentence ends at `.`/`?`/`!` followed by a space and a capital — which is
/// the pattern that does not fire on `0.7.3`, on `e.g.` or on a file extension,
/// because none of those has a capital after the space.
fn first_sentence(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    for (index, window) in bytes.windows(3).enumerate() {
        let [end, space, next] = window else { continue };
        if matches!(end, '.' | '?' | '!') && *space == ' ' && next.is_uppercase() {
            let head: String = bytes[..=index].iter().collect();
            let head = strip_trailing_punctuation(&head).to_string();
            if !head.is_empty() {
                return head;
            }
        }
    }
    text.to_string()
}

/// The text with its final clause removed, or `None` when it has only one.
fn drop_last_clause(text: &str) -> Option<String> {
    let cut = CLAUSE_BREAKS
        .iter()
        .filter_map(|sep| text.rfind(sep))
        .max()?;
    let head = strip_trailing_punctuation(&text[..cut]);
    (!head.is_empty()).then(|| head.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The publisher's own words survive. Every word the card draws has to be
    /// a word the fleet wrote, in the order it wrote them — this module chooses
    /// what to *omit* and is never allowed to compose.
    ///
    /// `contains` rather than equality in either direction because a path is
    /// shortened to a piece of itself and a trailing full stop is trimmed, so a
    /// surviving word can be a substring of the published one or the other way
    /// round. Case is compared exactly: nothing here is allowed to change it.
    fn is_subsequence_of_words(candidate: &str, source: &str) -> bool {
        let mut source_words = source.split_whitespace();
        candidate
            .split_whitespace()
            .all(|word| source_words.any(|from| from.contains(word) || word.contains(from)))
    }

    const REAL_SUMMARIES: &[&str] = &[
        "Investigateing killed Okta corpus and Herdr work sessions",
        "Fixing card rendering and truncation issues in herdr",
        "Currently working on the sidebar card trim",
        "Working on: rasterising the sidebar at the terminal's real cell size",
        "Fixing src/ui/sidebar/image_card.rs so the trim composes with the floor",
        "Trimmed the cards by 20%. Now verifying against a real terminal.",
        "Validating FM_HOME anchor fix (and shipping the PR) before handoff",
        "Establish home_budget_app secondmate operations",
    ];

    #[test]
    fn condensing_only_ever_omits_the_publishers_own_words() {
        for summary in REAL_SUMMARIES {
            let condensed = condense(summary);
            assert!(
                is_subsequence_of_words(&condensed, summary),
                "{condensed:?} invented words that are not in {summary:?}"
            );
        }
    }

    #[test]
    fn every_rung_of_the_ladder_only_ever_omits_words() {
        for summary in REAL_SUMMARIES {
            let condensed = condense(summary);
            let ladder = candidates(&condensed);
            for rung in ladder.rungs() {
                assert!(
                    is_subsequence_of_words(rung, summary),
                    "{rung:?} invented words that are not in {summary:?}"
                );
            }
        }
    }

    /// The ladder never gets longer as it descends. A "shorter" rendering that
    /// is wider than the one above it would make the fitter's walk meaningless.
    #[test]
    fn the_ladder_never_climbs() {
        for summary in REAL_SUMMARIES {
            let ladder = candidates(&condense(summary));
            let rungs: Vec<&str> = ladder.rungs().collect();
            for pair in rungs.windows(2) {
                assert!(
                    pair[1].chars().count() <= pair[0].chars().count(),
                    "{:?} is longer than the rung above it, {:?}",
                    pair[1],
                    pair[0]
                );
            }
        }
    }

    #[test]
    fn condense_is_its_own_fixed_point() {
        for summary in REAL_SUMMARIES {
            let once = condense(summary);
            assert_eq!(
                condense(&once),
                once,
                "condensing {summary:?} twice moved it"
            );
        }
    }

    #[test]
    fn the_chips_own_words_are_not_said_twice() {
        assert_eq!(
            condense("Currently working on the card trim"),
            "the card trim"
        );
        assert_eq!(condense("Working on: card trim"), "card trim");
        assert_eq!(condense("I'm fixing the flake"), "fixing the flake");
        assert_eq!(condense("wip: card trim"), "card trim");
    }

    /// Nothing is ever re-cased, in either direction.
    ///
    /// The tempting tidy-up after stripping a preamble is to restore sentence
    /// case, and there is no test that separates the verb in "Working on fixing
    /// the flake" from the project name in "Working on herdr card trim". So the
    /// card leaves every character exactly as the publisher typed it — see the
    /// note on [`condense`].
    #[test]
    fn a_name_is_never_recased_into_a_sentence() {
        assert_eq!(
            condense("Working on herdr card trim"),
            "herdr card trim",
            "a bare project name must not be capitalised into a sentence"
        );
        assert_eq!(
            condense("Working on home_budget_app operations"),
            "home_budget_app operations"
        );
        // And across the whole corpus, every surviving word appears in the
        // published string with its case untouched.
        for summary in REAL_SUMMARIES {
            for word in condense(summary).split_whitespace() {
                assert!(
                    summary
                        .split_whitespace()
                        .any(|from| from.contains(word) || word.contains(from)),
                    "{word:?} is not in {summary:?} with the case the publisher used"
                );
            }
        }
    }

    /// Stripping the preamble must never strip the whole summary.
    #[test]
    fn a_summary_that_is_only_a_preamble_survives_it() {
        assert_eq!(condense("Working on"), "Working on");
        assert_eq!(condense("currently"), "currently");
        assert_eq!(condense("Now"), "Now");
    }

    #[test]
    fn whitespace_fences_and_full_stops_go() {
        assert_eq!(
            condense("  Fixing\tthe\n  card   trim.  "),
            "Fixing the card trim"
        );
        assert_eq!(condense("`cargo nextest` is red"), "cargo nextest is red");
        assert_eq!(condense("\"Fixing the card trim\""), "Fixing the card trim");
        assert_eq!(condense("Fixing the card trim…"), "Fixing the card trim");
    }

    /// A question mark is the publisher's, not punctuation to tidy away.
    #[test]
    fn a_deliberate_ending_is_kept() {
        assert_eq!(
            condense("Why is the card clipped?"),
            "Why is the card clipped?"
        );
        assert_eq!(condense("The suite is green!"), "The suite is green!");
    }

    #[test]
    fn a_path_keeps_the_part_the_reader_does_not_know() {
        let ladder = candidates("Fixing src/ui/sidebar/image_card.rs so the trim composes");
        let rungs: Vec<&str> = ladder.rungs().collect();
        assert!(
            rungs.contains(&"Fixing image_card.rs so the trim composes"),
            "no rung shortened the path: {rungs:?}"
        );
    }

    /// A version number, a decimal and a bare word with a slash in it are not
    /// paths, and a URL's last segment is not the readable part of it.
    #[test]
    fn what_is_not_a_path_is_left_alone() {
        for text in [
            "Bumped herdr to 0.7.3",
            "Merged and/or rebased the branch",
            "Reading https://github.com/herdrdev/herdr/pull/2554",
        ] {
            assert_eq!(
                shorten_paths(text),
                text,
                "{text:?} was shortened as though it held a path"
            );
        }
    }

    #[test]
    fn an_aside_is_dropped_before_a_clause_is() {
        let ladder =
            candidates("Validating FM_HOME anchor fix (and shipping the PR) before handoff");
        let rungs: Vec<&str> = ladder.rungs().collect();
        let aside = rungs
            .iter()
            .position(|rung| !rung.contains("shipping"))
            .expect("no rung dropped the aside");
        let clause = rungs
            .iter()
            .position(|rung| !rung.contains("handoff"))
            .unwrap_or(usize::MAX);
        assert!(
            aside < clause,
            "a clause was given up before an aside: {rungs:?}"
        );
    }

    /// An unmatched bracket is a bracket, not the start of an aside — dropping
    /// to the end of the string there is exactly the truncation this avoids.
    #[test]
    fn an_unmatched_bracket_is_not_an_aside() {
        assert_eq!(
            drop_asides("Fixing the card (and the tray"),
            "Fixing the card (and the tray"
        );
    }

    #[test]
    fn the_second_sentence_goes_before_the_first_one_is_cut() {
        let ladder = candidates(&condense(
            "Trimmed the cards by 20%. Now verifying against a real terminal.",
        ));
        let rungs: Vec<&str> = ladder.rungs().collect();
        assert!(
            rungs.contains(&"Trimmed the cards by 20%"),
            "no rung kept exactly the first sentence: {rungs:?}"
        );
    }

    /// A decimal is not a sentence end.
    #[test]
    fn a_version_number_does_not_end_a_sentence() {
        assert_eq!(
            first_sentence("Bumped to 0.7.3 for the Release"),
            "Bumped to 0.7.3 for the Release"
        );
    }

    #[test]
    fn a_clause_is_dropped_at_its_boundary_and_reads_finished() {
        assert_eq!(
            drop_last_clause("Trimming the cards, then verifying on a real terminal"),
            Some("Trimming the cards".to_string())
        );
        assert_eq!(drop_last_clause("Trimming the cards"), None);
    }

    /// A hyphen inside a word is not a clause break.
    #[test]
    fn a_compound_word_is_not_two_clauses() {
        assert_eq!(drop_last_clause("Running the pre-release audit"), None);
    }

    #[test]
    fn the_fallback_rung_gives_up_no_content() {
        let ladder = candidates(&condense(
            "Fixing src/ui/sidebar/image_card.rs (the pixel path) so the trim composes",
        ));
        let fallback = ladder.lossless();
        assert!(
            fallback.contains("trim composes"),
            "the fallback rung dropped a clause: {fallback:?}"
        );
        assert!(
            fallback.contains("image_card.rs") && !fallback.contains("src/ui"),
            "the fallback rung did not bank the free savings: {fallback:?}"
        );
    }

    /// Nothing here may hand back an empty title. A card with no words is a
    /// card the reader cannot tell from a broken one.
    #[test]
    fn no_input_produces_an_empty_rung() {
        for text in ["", "   ", "``", "\"\"", ".", "…", "Working on", "(x)"] {
            let condensed = condense(text);
            for rung in candidates(&condensed).rungs() {
                if condensed.is_empty() {
                    continue;
                }
                assert!(!rung.is_empty(), "{text:?} produced an empty rung");
            }
        }
    }
}
