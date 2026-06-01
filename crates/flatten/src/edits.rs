//! Byte-edit application for source-text rewrites.
//!
//! flatten's transformation passes produce lists of `(byte_range,
//! replacement_text)` tuples and apply them by walking the input back-
//! to-front (so earlier byte indices stay valid as later ones get
//! resized). Five+ sites across the codebase used to hand-roll the same
//! `sort + replace_range loop`; this module owns the one-true
//! implementation.
//!
//! ## Overlap policy
//!
//! [`apply_simple_edits`] assumes its inputs come from a single AST
//! walk that doesn't produce overlapping edits. It does not detect or
//! resolve overlaps — partial overlap will silently corrupt the output.
//!
//! Callers that need overlap merging or content-vs-strip prioritisation
//! (`expand/src/main.rs::apply_edits`, `vendor.rs::rewrite_for_vendoring`)
//! still implement their own resolver, because their inputs interleave
//! multiple AST walks with subtle ordering rules. Consolidating those
//! is tracked separately in ROADMAP "Larger refactors".

use std::ops::Range;

/// Apply a list of `(range, replacement)` edits to `src` and return the
/// rewritten string. Edits are sorted by `range.start` and applied
/// back-to-front so earlier indices remain valid. Returns `src.to_string()`
/// unchanged when `edits` is empty.
///
/// Caller is responsible for ensuring no two ranges overlap; see module
/// docs for the rationale.
pub fn apply_simple_edits(src: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    if edits.is_empty() {
        return src.to_string();
    }
    edits.sort_by_key(|(r, _)| r.start);
    let mut out = src.to_string();
    for (range, replacement) in edits.into_iter().rev() {
        out.replace_range(range, &replacement);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_edits_returns_unchanged() {
        assert_eq!(apply_simple_edits("hello", Vec::new()), "hello");
    }

    #[test]
    fn single_replacement() {
        let edits = vec![(0..5, "world".to_string())];
        assert_eq!(apply_simple_edits("hello there", edits), "world there");
    }

    #[test]
    fn multiple_non_overlapping_in_input_order() {
        // Pre-sort guarantees the apply order matches the input order
        // when iterating in reverse — so even if caller passes them out
        // of source order, the result is correct.
        let edits = vec![
            (6..11, "world".to_string()),
            (0..5, "HELLO".to_string()),
        ];
        assert_eq!(apply_simple_edits("hello there", edits), "HELLO world");
    }

    #[test]
    fn multiple_non_overlapping_passed_unsorted() {
        // Same edits as above, passed in source order. Should still
        // sort + apply back-to-front correctly.
        let edits = vec![
            (0..5, "HELLO".to_string()),
            (6..11, "world".to_string()),
        ];
        assert_eq!(apply_simple_edits("hello there", edits), "HELLO world");
    }

    #[test]
    fn empty_replacement_deletes() {
        let edits = vec![(0..6, String::new())];
        assert_eq!(apply_simple_edits("hello world", edits), "world");
    }

    #[test]
    fn replacement_with_different_length() {
        // Apply order is back-to-front so lengthening one edit does
        // not shift another's indices.
        let edits = vec![
            (0..5, "HI".to_string()),
            (6..11, "EVERYBODY".to_string()),
        ];
        assert_eq!(apply_simple_edits("hello world", edits), "HI EVERYBODY");
    }
}
