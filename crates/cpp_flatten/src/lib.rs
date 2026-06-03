//! Recursively inline local `#include "..."` directives in a C++ file
//! and emit a single flat translation unit. The output is what you get
//! from running the preprocessor with `-E`, except (a) only quoted
//! includes are touched — `<system>` includes are left as text, and
//! (b) no macro expansion / conditional evaluation happens. The goal is
//! "give me a single .cpp I can paste into CodinGame's web editor",
//! not "be a real preprocessor".
//!
//! Each local include is inlined at most once — the second occurrence
//! is dropped, mimicking `#pragma once`. Belt-and-suspenders safety
//! for any header that forgets the guard.
//!
//! ## Known limitations
//!
//! * Conditional includes (`#ifdef WIN32 \n #include "win.h" \n #else
//!   \n #include "posix.h" \n #endif`) inline *both* branches because
//!   we don't evaluate `#if`. Avoid this pattern in flattenable sources.
//! * Include lines inside `/* ... */` block comments are also followed.
//!   Single-line `//` comments aren't followed because the line is then
//!   not a bare `#include "..."`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Flatten the C++ translation unit rooted at `entry`. Returns the
/// inlined source as a single `String`.
pub fn flatten(entry: &Path) -> Result<String> {
    let mut seen = HashSet::new();
    let mut out = String::new();
    flatten_into(entry, &mut seen, &mut out)
        .with_context(|| format!("flattening {}", entry.display()))?;
    Ok(out)
}

fn flatten_into(path: &Path, seen: &mut HashSet<PathBuf>, out: &mut String) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    if !seen.insert(canonical.clone()) {
        // Second occurrence — silently skip, like `#pragma once`.
        return Ok(());
    }

    let content = std::fs::read_to_string(&canonical)
        .with_context(|| format!("reading {}", canonical.display()))?;
    let parent = canonical.parent().unwrap_or_else(|| Path::new("."));

    // `split_inclusive` keeps the trailing '\n', so we preserve the
    // file's exact line breaks instead of rewriting them.
    for line in content.split_inclusive('\n') {
        if let Some(rel) = parse_local_include(line) {
            let nested = parent.join(rel);
            flatten_into(&nested, seen, out).with_context(|| {
                format!("inlining `#include \"{rel}\"` from {}", canonical.display(),)
            })?;
        } else if is_pragma_once(line) {
            // Path-based dedup above gives the same guarantee; leaving
            // the line in trips `-Wpragma-once-outside-header` once
            // we're flat. Drop it.
        } else {
            out.push_str(line);
        }
    }
    // Files that don't end in '\n' would otherwise glue their last line
    // onto whatever comes next in the parent.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(())
}

fn is_pragma_once(line: &str) -> bool {
    let s = line.trim_start();
    let Some(s) = s.strip_prefix('#') else {
        return false;
    };
    let s = s.trim_start();
    let Some(s) = s.strip_prefix("pragma") else {
        return false;
    };
    if !s.starts_with(|c: char| c.is_whitespace()) {
        return false;
    }
    s.trim().starts_with("once")
}

/// If `line` is `[whitespace] # [whitespace] include [whitespace]
/// "name" [...]`, return `name`. Otherwise `None`. Trailing comments or
/// stray characters after the closing quote are allowed but ignored —
/// the C preprocessor accepts them too.
fn parse_local_include(line: &str) -> Option<&str> {
    let s = line.trim_start();
    let s = s.strip_prefix('#')?.trim_start();
    let s = s.strip_prefix("include")?;
    // Must be followed by whitespace — guards against e.g. `#includeXY`.
    if !s.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let s = s.trim_start();
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(&s[..end])
}

#[cfg(test)]
mod parse_tests {
    use super::{is_pragma_once, parse_local_include};

    #[test]
    fn pragma_once_variants() {
        assert!(is_pragma_once("#pragma once"));
        assert!(is_pragma_once("  #pragma once\n"));
        assert!(is_pragma_once("# pragma once"));
        assert!(is_pragma_once("#pragma once // header guard"));
        assert!(!is_pragma_once("#pragma pack(1)"));
        assert!(!is_pragma_once("#include \"foo.h\""));
        assert!(!is_pragma_once("// #pragma once"));
    }

    #[test]
    fn plain_include() {
        assert_eq!(parse_local_include("#include \"foo.h\"\n"), Some("foo.h"));
    }

    #[test]
    fn leading_whitespace() {
        assert_eq!(parse_local_include("   #include \"foo.h\""), Some("foo.h"));
    }

    #[test]
    fn space_after_hash() {
        assert_eq!(parse_local_include("# include \"foo.h\""), Some("foo.h"));
    }

    #[test]
    fn trailing_comment() {
        assert_eq!(
            parse_local_include("#include \"foo.h\" // trailer"),
            Some("foo.h"),
        );
    }

    #[test]
    fn relative_path() {
        assert_eq!(
            parse_local_include("#include \"../sub/foo.h\""),
            Some("../sub/foo.h"),
        );
    }

    #[test]
    fn system_include_ignored() {
        assert_eq!(parse_local_include("#include <iostream>"), None);
    }

    #[test]
    fn not_an_include() {
        assert_eq!(parse_local_include("int main() {}"), None);
        assert_eq!(parse_local_include("#define X 1"), None);
        assert_eq!(parse_local_include("#includeXY \"foo.h\""), None);
    }

    #[test]
    fn empty_line() {
        assert_eq!(parse_local_include(""), None);
        assert_eq!(parse_local_include("\n"), None);
    }
}
