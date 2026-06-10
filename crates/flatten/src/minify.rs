//! Strip comments and collapse blank lines from a Rust source string.
//!
//! Operates on the byte-level source text rather than a syn AST, so it
//! preserves the original code structure (indentation, line layout,
//! macro_rules! token spacing) while removing the noise.
//!
//! What gets stripped:
//!   - Line comments (`// ...`, `/// ...`, `//! ...`)
//!   - Block comments (`/* ... */`, including nested ones, including
//!     `/** ... */` doc comments)
//!   - Runs of 3+ consecutive blank lines (collapsed to 2)
//!
//! What's preserved verbatim:
//!   - String literals (regular, raw, byte, byte raw, C, C raw)
//!   - Character literals
//!   - Lifetimes (distinguished from char literals by lookahead)
//!   - All other source bytes
//!
//! This is the entire scope of `--minify`. See `SHAKING.md` for why we
//! deliberately don't do source-level dead-code elimination.

/// Minify a Rust source string by stripping comments and collapsing
/// runs of blank lines. Always returns valid Rust source — comments and
/// blank lines have no semantic meaning.
pub fn minify(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Raw / byte / C string literals must be detected BEFORE the
        // generic `b == b'"'` check below — once we're inside a regular
        // `"..."` we'd misparse a `r#"..."#` literal.
        if let Some(consumed) = try_consume_raw_string_prefix(bytes, i) {
            out.extend_from_slice(&bytes[i..i + consumed]);
            i += consumed;
            i = copy_raw_string_body(bytes, i, &mut out, raw_hashes(bytes, i - consumed));
            continue;
        }

        match b {
            b'"' => {
                i = copy_string_literal(bytes, i, &mut out);
                continue;
            }
            b'\'' => {
                i = copy_char_or_lifetime(bytes, i, &mut out);
                continue;
            }
            b'/' if i + 1 < bytes.len() => match bytes[i + 1] {
                b'/' => {
                    // Line comment: skip to end of line. Don't consume the
                    // trailing \n (the outer loop copies it on the next iteration
                    // so the line break is preserved).
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'*' => {
                    // Block comment with nesting support.
                    i += 2;
                    let mut depth = 1usize;
                    while i < bytes.len() && depth > 0 {
                        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                            depth += 1;
                            i += 2;
                        } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            depth -= 1;
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    continue;
                }
                _ => {}
            },
            _ => {}
        }

        out.push(b);
        i += 1;
    }

    let after_comments = String::from_utf8(out).expect("minifier must produce valid UTF-8");
    collapse_blank_lines(&after_comments)
}

/// If `bytes[i..]` starts with one of the raw-string-literal prefixes
/// (`r"`, `r#`, `b`/`c` followed by raw form, etc.), return how many bytes
/// of prefix to copy (including the opening quote). Returns None if it's
/// not a raw-string prefix at this position.
fn try_consume_raw_string_prefix(bytes: &[u8], i: usize) -> Option<usize> {
    // The prefix must be at an ident boundary — if the previous byte is
    // an ident-continuation char (letter/digit/underscore), this `r`/`b`/`c`
    // is part of a longer ident, not a string prefix.
    if i > 0 && is_ident_continue(bytes[i - 1]) {
        return None;
    }

    let mut j = i;
    // Optional `b` or `c` modifier for byte / C string literals
    if matches!(bytes.get(j), Some(b'b' | b'c')) {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    // Zero or more `#` characters
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    // Must end at an opening `"`
    if bytes.get(j) == Some(&b'"') {
        Some(j - i + 1)
    } else {
        None
    }
}

/// Number of `#` characters between `r` and the opening quote in a raw
/// string at `bytes[i..]`. Caller has already verified this is a raw string.
fn raw_hashes(bytes: &[u8], i: usize) -> usize {
    let mut j = i;
    if matches!(bytes.get(j), Some(b'b' | b'c')) {
        j += 1;
    }
    debug_assert_eq!(bytes.get(j), Some(&b'r'));
    j += 1;
    let mut hashes = 0usize;
    while bytes.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    hashes
}

/// Copy raw-string body and closing delimiter (`"` followed by `hashes`
/// `#`s). Returns the position after the closing `#`s.
fn copy_raw_string_body(bytes: &[u8], mut i: usize, out: &mut Vec<u8>, hashes: usize) -> usize {
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut all_hashes = true;
            for k in 0..hashes {
                if bytes.get(i + 1 + k) != Some(&b'#') {
                    all_hashes = false;
                    break;
                }
            }
            if all_hashes {
                out.push(b'"');
                i += 1;
                for _ in 0..hashes {
                    out.push(b'#');
                    i += 1;
                }
                return i;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    i
}

/// Copy a regular `"..."` string literal, handling escape sequences.
/// Returns the position after the closing quote.
fn copy_string_literal(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    debug_assert_eq!(bytes[i], b'"');
    out.push(bytes[i]);
    i += 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            // Escape sequence: copy both bytes verbatim. Doesn't matter
            // exactly what the escape is — we just need to not treat the
            // following `"` as a string close.
            out.push(b);
            out.push(bytes[i + 1]);
            i += 2;
            continue;
        }
        out.push(b);
        i += 1;
        if b == b'"' {
            return i;
        }
    }
    i
}

/// Copy either a char literal (`'a'`, `'\n'`) or a lifetime (`'a`,
/// `'static`). Disambiguates by looking for a closing `'` after the inner
/// content. Returns the position after the literal/lifetime.
fn copy_char_or_lifetime(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    debug_assert_eq!(bytes[i], b'\'');
    out.push(bytes[i]);
    i += 1;
    if i >= bytes.len() {
        return i;
    }

    // Escape sequence — definitely a char literal.
    if bytes[i] == b'\\' && i + 1 < bytes.len() {
        out.push(bytes[i]);
        out.push(bytes[i + 1]);
        i += 2;
        // Greedily copy further escape characters (e.g. `'\u{1F600}'`).
        while i < bytes.len() && bytes[i] != b'\'' && is_char_literal_byte(bytes[i]) {
            out.push(bytes[i]);
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'\'' {
            out.push(bytes[i]);
            i += 1;
        }
        return i;
    }

    // First char after the `'`. If it's followed immediately by another
    // `'`, it's a single-char literal. Otherwise it's a lifetime.
    out.push(bytes[i]);
    i += 1;
    if i < bytes.len() && bytes[i] == b'\'' {
        out.push(bytes[i]);
        i += 1;
        return i;
    }

    // It's a lifetime. Continue copying ident-continuation bytes; the
    // outer loop will resume at the first non-ident char.
    while i < bytes.len() && is_ident_continue(bytes[i]) {
        out.push(bytes[i]);
        i += 1;
    }
    i
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Within a char literal, allowable bytes between the opening `\` and the
/// closing `'`. Used after we've consumed the first two escape bytes to
/// keep going through e.g. `\u{1F600}`.
fn is_char_literal_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'{' || b == b'}'
}

/// Collapse runs of 3+ consecutive newlines down to 2 (one blank line),
/// and trim leading newlines so a file's first line is the first real line.
// Collapse runs of blank lines to at most one. Lines that are
// entirely whitespace count as blank — comment stripping leaves
// behind the original leading indentation, and without this they'd
// register as "non-blank" content and defeat the collapser. Trailing
// whitespace on emitted lines is also trimmed.
fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_pending = false;
    let mut written_any = false;
    for line in s.lines() {
        if line.chars().all(char::is_whitespace) {
            blank_pending = written_any;
        } else {
            if blank_pending {
                out.push('\n');
                blank_pending = false;
            }
            out.push_str(line.trim_end());
            out.push('\n');
            written_any = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_comments() {
        let src = "fn foo() {} // a trailing comment\nfn bar() {}\n";
        let m = minify(src);
        assert!(!m.contains("trailing"), "got:\n{m}");
        assert!(m.contains("fn foo() {}"));
        assert!(m.contains("fn bar() {}"));
    }

    #[test]
    fn strips_doc_comments() {
        let src = "/// docs for foo\n/// more docs\nfn foo() {}\n";
        let m = minify(src);
        assert!(!m.contains("docs"), "got:\n{m}");
        assert!(m.contains("fn foo() {}"));
    }

    #[test]
    fn strips_inner_doc_comments() {
        let src = "//! crate-level docs\nfn foo() {}\n";
        let m = minify(src);
        assert!(!m.contains("crate-level"));
        assert!(m.contains("fn foo() {}"));
    }

    #[test]
    fn strips_block_comments() {
        let src = "/* before */ fn foo() /* mid */ {} /* after */\n";
        let m = minify(src);
        assert!(!m.contains("before"));
        assert!(!m.contains("mid"));
        assert!(!m.contains("after"));
        assert!(m.contains("fn foo()"));
    }

    #[test]
    fn handles_nested_block_comments() {
        let src = "/* outer /* inner */ still outer */ fn foo() {}\n";
        let m = minify(src);
        assert!(!m.contains("outer"));
        assert!(!m.contains("inner"));
        assert!(m.contains("fn foo()"));
    }

    #[test]
    fn preserves_double_slash_inside_string() {
        let src = "let s = \"http://example.com/foo\";\n";
        let m = minify(src);
        assert!(m.contains(r#"http://example.com/foo"#));
    }

    #[test]
    fn preserves_block_comment_marker_inside_string() {
        let src = "let s = \"contains /* not a comment */\";\n";
        let m = minify(src);
        assert!(m.contains("/* not a comment */"));
    }

    #[test]
    fn handles_string_with_escaped_quote() {
        let src = "let s = \"says \\\"hi\\\" loudly\"; // dropped\n";
        let m = minify(src);
        assert!(m.contains("\\\"hi\\\""), "got:\n{m}");
        assert!(!m.contains("dropped"));
    }

    #[test]
    fn handles_raw_string_with_inner_quote() {
        let src = "let s = r#\"contains \"quotes\" inside\"#;\n";
        let m = minify(src);
        assert!(m.contains("r#\"contains \"quotes\" inside\"#"), "got:\n{m}");
    }

    #[test]
    fn handles_raw_string_with_inner_comment_marker() {
        let src = "let s = r\"// not a comment\";\n";
        let m = minify(src);
        assert!(m.contains("r\"// not a comment\""), "got:\n{m}");
    }

    #[test]
    fn handles_raw_string_with_multiple_hashes() {
        let src = "let s = r##\"contains \"# inside\"##;\n";
        let m = minify(src);
        assert!(m.contains("r##\"contains \"# inside\"##"), "got:\n{m}");
    }

    #[test]
    fn handles_byte_strings_and_byte_raw_strings() {
        let src = "let a = b\"bytes\"; let b = br\"// not a comment\"; let c = br#\"with #\"#;\n";
        let m = minify(src);
        assert!(m.contains("b\"bytes\""));
        assert!(m.contains("br\"// not a comment\""));
        assert!(m.contains("br#\"with #\"#"));
    }

    #[test]
    fn does_not_misparse_ident_starting_with_r() {
        // `red` is just an ident, not a raw string prefix
        let src = "let red = 1; let result = red + 1;\n";
        let m = minify(src);
        assert!(m.contains("red"));
        assert!(m.contains("result"));
    }

    #[test]
    fn distinguishes_char_literal_from_lifetime() {
        let src = "fn f<'a>(c: char) { let x: &'a str = \"hi\"; if c == 'a' { return; } }\n";
        let m = minify(src);
        assert!(m.contains("'a>"), "lifetime preserved: {m}");
        assert!(m.contains("&'a str"));
        assert!(m.contains("'a'"), "char literal preserved: {m}");
    }

    #[test]
    fn handles_static_lifetime() {
        let src = "fn f() -> &'static str { \"hi\" }\n";
        let m = minify(src);
        assert!(m.contains("&'static str"));
    }

    #[test]
    fn handles_unicode_escape_in_char_literal() {
        let src = "let smile = '\\u{1F600}'; // a smiling face\n";
        let m = minify(src);
        assert!(m.contains("'\\u{1F600}'"));
        assert!(!m.contains("smiling"));
    }

    #[test]
    fn handles_escaped_char_literals() {
        let src = "let nl = '\\n'; let bs = '\\\\'; let quote = '\\'';\n";
        let m = minify(src);
        assert!(m.contains("'\\n'"));
        assert!(m.contains("'\\\\'"));
        assert!(m.contains("'\\''"));
    }

    #[test]
    fn collapses_blank_lines() {
        let src = "fn a() {}\n\n\n\n\nfn b() {}\n";
        let m = minify(src);
        // 5 newlines collapse to 2 (one blank line).
        assert_eq!(m, "fn a() {}\n\nfn b() {}\n");
    }

    #[test]
    fn preserves_two_consecutive_newlines() {
        let src = "fn a() {}\n\nfn b() {}\n";
        let m = minify(src);
        assert_eq!(m, src);
    }

    #[test]
    fn empty_input() {
        assert_eq!(minify(""), "");
    }

    #[test]
    fn only_comments() {
        let src = "// just a comment\n/* and another */\n";
        let m = minify(src);
        // Comments stripped, newlines preserved (then collapsed if needed).
        assert!(!m.contains("comment"));
        assert!(!m.contains("another"));
    }

    #[test]
    fn stripped_indented_comments_dont_leave_whitespace_blank_lines() {
        // The scanner copies the leading whitespace of a comment line
        // to the output buffer before recognising the `//`, so a
        // stripped indented comment leaves behind a whitespace-only
        // line. `collapse_blank_lines` must treat those as blank or
        // runs of stripped doc comments survive as visual blank-line
        // gaps in the bundled output.
        let src = "fn a() {}\n    // c1\n    // c2\n    // c3\nfn b() {}\n";
        let m = minify(src);
        // The three stripped comment lines collapse to a single blank
        // line between the two functions (max one blank between
        // content runs, just like a real source-level blank).
        assert_eq!(m, "fn a() {}\n\nfn b() {}\n");
    }

    #[test]
    fn comment_at_end_of_file_no_newline() {
        let src = "fn x() {} // trailing";
        let m = minify(src);
        assert!(!m.contains("trailing"));
        assert!(m.contains("fn x() {}"));
    }

    #[test]
    fn unterminated_block_comment_does_not_loop_forever() {
        let src = "fn x() {} /* never closed";
        let m = minify(src);
        // We should at least terminate. Output may drop the rest; that's fine.
        assert!(m.contains("fn x() {}"));
    }

    #[test]
    fn measurably_smaller_for_doc_heavy_input() {
        let src = "/// This is a doc comment.\n\
                   /// Lots of explanation about the function below.\n\
                   /// Multiple lines of prose that we don't need to keep.\n\
                   pub fn x() -> i32 {\n    // implementation note\n    42\n}\n";
        let m = minify(src);
        assert!(
            m.len() < src.len() / 2,
            "minified: {} bytes vs original: {}",
            m.len(),
            src.len()
        );
        assert!(m.contains("pub fn x() -> i32"));
        assert!(m.contains("42"));
    }
}
