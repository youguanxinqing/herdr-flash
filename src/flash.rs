//! Incremental-search picking, in the style of flash.nvim.
//!
//! Candidates are the literal occurrences of what you typed — nothing wider. An earlier version
//! grew each hit out to its enclosing whitespace-delimited token, which is meaningless once the
//! user selects the text by hand and actively wrong on CJK: there is no space to split on, so
//! "黄色，label" came back as one word and the highlight swallowed the whole clause.

use crate::hints::HINT_ALPHABET;

/// One occurrence of the query in the visible text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashCandidate {
    /// Zero-based logical line index.
    pub line: usize,
    /// UTF-8 byte offset where the hit starts; also where the cursor lands.
    pub start: usize,
    /// UTF-8 byte offset just past the hit; also where the label is drawn.
    pub end: usize,
}

/// One labeled candidate ready to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashLabel {
    pub label: char,
    pub candidate: FlashCandidate,
}

/// Finds every non-overlapping occurrence of `query`, in visible order.
///
/// Smartcase: an all-lowercase query ignores case, any uppercase makes the whole query literal.
/// Folding is ASCII-only on purpose — full `to_lowercase` can change a string's byte length
/// (`İ` becomes two chars), returning an offset that does not exist in the original.
pub fn find_query_matches(lines: &[String], query: &str) -> Vec<FlashCandidate> {
    if query.is_empty() {
        return Vec::new();
    }

    let fold = !query.chars().any(char::is_uppercase);
    let needle = if fold {
        query.to_ascii_lowercase()
    } else {
        query.to_string()
    };

    let mut candidates = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let haystack = if fold {
            line.to_ascii_lowercase()
        } else {
            line.clone()
        };
        let mut from = 0;
        while let Some(hit) = haystack.get(from..).and_then(|rest| rest.find(&needle)) {
            let start = from + hit;
            let end = start + needle.len();
            candidates.push(FlashCandidate {
                line: index,
                start,
                end,
            });
            from = end;
        }
    }
    candidates
}

/// Characters that would extend a live match if typed next.
///
/// This is what makes single-key labels unambiguous: a label never doubles as a character that
/// could have narrowed the search further, so pressing it can only mean "pick this one".
pub fn continuation_chars(lines: &[String], candidates: &[FlashCandidate]) -> Vec<char> {
    let mut chars: Vec<char> = candidates
        .iter()
        .filter_map(|candidate| {
            lines
                .get(candidate.line)?
                .get(candidate.end..)?
                .chars()
                .next()
                .map(|ch| ch.to_ascii_lowercase())
        })
        .collect();
    chars.sort_unstable();
    chars.dedup();
    chars
}

/// Labels candidates in visible order, skipping any character that could extend the query.
///
/// Labels stay one keystroke wide. Candidates past the usable alphabet keep their highlight but
/// get no label — two-character labels would cost more keys than they save once search has already
/// narrowed the field.
pub fn assign_labels(lines: &[String], candidates: &[FlashCandidate]) -> Vec<FlashLabel> {
    let blocked = continuation_chars(lines, candidates);
    let alphabet = HINT_ALPHABET.chars().filter(|ch| !blocked.contains(ch));

    candidates
        .iter()
        .copied()
        .zip(alphabet)
        .map(|(candidate, label)| FlashLabel { label, candidate })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| row.to_string()).collect()
    }

    fn texts(rows: &[String], candidates: &[FlashCandidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|candidate| rows[candidate.line][candidate.start..candidate.end].to_string())
            .collect()
    }

    #[test]
    fn a_match_never_grows_past_what_was_typed() {
        // The old token-widening behaviour turned this into the whole clause.
        let rows = lines(&["命中的 token 黄色，label 一格青底"]);
        let found = find_query_matches(&rows, "label");

        assert_eq!(texts(&rows, &found), vec!["label"]);
    }

    #[test]
    fn every_occurrence_on_a_line_is_its_own_candidate() {
        let rows = lines(&["label and label again", "third label"]);
        let found = find_query_matches(&rows, "label");

        assert_eq!(found.len(), 3);
        assert_eq!(found[0].line, 0);
        assert_eq!(found[1].line, 0);
        assert_eq!(found[2].line, 1);
        assert!(found[0].end <= found[1].start, "hits must not overlap");
    }

    #[test]
    fn smartcase_folds_until_the_query_has_uppercase() {
        let rows = lines(&["Cargo.toml"]);

        assert_eq!(find_query_matches(&rows, "cargo").len(), 1);
        assert_eq!(find_query_matches(&rows, "Cargo").len(), 1);
        assert_eq!(find_query_matches(&rows, "CARGO").len(), 0);
    }

    #[test]
    fn non_ascii_case_mapping_does_not_break_offsets() {
        // `İ` lowercases to two chars; a length-changing fold would hand back a bogus offset.
        let rows = lines(&["İstanbul path/to/file"]);
        let found = find_query_matches(&rows, "stan");

        assert_eq!(texts(&rows, &found), vec!["stan"]);
    }

    #[test]
    fn cjk_queries_land_on_character_boundaries() {
        let rows = lines(&["前面高亮的是不是也是 bug"]);
        let found = find_query_matches(&rows, "高亮");

        assert_eq!(texts(&rows, &found), vec!["高亮"]);
    }

    #[test]
    fn empty_query_selects_nothing() {
        let rows = lines(&["anything at all"]);
        assert!(find_query_matches(&rows, "").is_empty());
    }

    #[test]
    fn labels_never_reuse_a_character_that_could_extend_the_query() {
        let rows = lines(&["alpha alps"]);
        let found = find_query_matches(&rows, "alp");
        // "alpha" continues with 'h', "alps" with 's'.
        assert_eq!(continuation_chars(&rows, &found), vec!['h', 's']);

        let labels = assign_labels(&rows, &found);
        assert_eq!(labels.len(), 2);
        assert!(!labels.iter().any(|label| label.label == 'h'));
        assert!(!labels.iter().any(|label| label.label == 's'));
    }

    #[test]
    fn a_hit_at_end_of_line_blocks_nothing() {
        let rows = lines(&["ends with alp"]);
        let found = find_query_matches(&rows, "alp");

        assert!(continuation_chars(&rows, &found).is_empty());
    }

    #[test]
    fn exhausted_alphabet_leaves_later_candidates_unlabeled() {
        let row = (0..40).map(|_| "zz ").collect::<String>();
        let rows = lines(&[row.as_str()]);
        let found = find_query_matches(&rows, "zz");

        assert_eq!(found.len(), 40);
        assert!(assign_labels(&rows, &found).len() < 40);
    }
}
