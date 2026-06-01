//! Parse `--external-file` payloads.
//!
//! Format: one crate name per line. Blank lines are ignored. Lines whose
//! first non-whitespace character is `#` are treated as comments and
//! ignored. Each surviving line is trimmed of leading/trailing whitespace
//! and used as a crate name.
//!
//! Inline trailing comments (`tokio  # the runtime`) are also recognised
//! — anything after a `#` on a content line is dropped.
//!
//! No validation of crate-name syntax: invalid names will be ignored
//! by `vendor_package` (with the existing unknown-name warning) just as
//! if they'd been passed via `--external`.

/// Parse the contents of an `--external-file` into crate names.
pub fn parse_external_file(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        // Strip an inline comment: everything from the first `#` onward.
        let body = match line.find('#') {
            Some(i) => &line[..i],
            None => line,
        };
        let trimmed = body.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_yields_nothing() {
        assert!(parse_external_file("").is_empty());
        assert!(parse_external_file("\n\n\n").is_empty());
    }

    #[test]
    fn single_name() {
        assert_eq!(parse_external_file("tokio\n"), vec!["tokio".to_string()]);
    }

    #[test]
    fn multiple_names() {
        let s = "tokio\nserde\nlog\n";
        assert_eq!(
            parse_external_file(s),
            vec!["tokio".to_string(), "serde".to_string(), "log".to_string()]
        );
    }

    #[test]
    fn ignores_blank_lines() {
        let s = "tokio\n\n\nserde\n\n";
        assert_eq!(
            parse_external_file(s),
            vec!["tokio".to_string(), "serde".to_string()]
        );
    }

    #[test]
    fn ignores_full_line_comments() {
        let s = "# this file lists my externals\n\
                 tokio\n\
                 # group two\n\
                 serde\n";
        assert_eq!(
            parse_external_file(s),
            vec!["tokio".to_string(), "serde".to_string()]
        );
    }

    #[test]
    fn strips_inline_trailing_comments() {
        let s = "tokio  # the runtime\n\
                 serde# serialization\n\
                 log\n";
        assert_eq!(
            parse_external_file(s),
            vec!["tokio".to_string(), "serde".to_string(), "log".to_string()]
        );
    }

    #[test]
    fn trims_whitespace_around_names() {
        let s = "   tokio   \n\t serde \t\n";
        assert_eq!(
            parse_external_file(s),
            vec!["tokio".to_string(), "serde".to_string()]
        );
    }

    #[test]
    fn comment_only_line_with_leading_whitespace_skipped() {
        let s = "   # spaced comment\ntokio\n";
        assert_eq!(parse_external_file(s), vec!["tokio".to_string()]);
    }

    #[test]
    fn infra_preset_parses_cleanly() {
        // Sanity check the bundled preset shipped with the binary.
        let content = include_str!("../presets/infra.txt");
        let names = parse_external_file(content);
        // Spot-check the genuinely-unvendorable crates the preset
        // ships with. parking_lot_core / signal-hook(-registry) /
        // zmij were removed in `7120ba8` after the holistic build-
        // script policy made them vendorable on their own.
        for required in [
            "windows-sys",
            "winapi",
            "windows-targets",
            "crossterm_winapi",
            "serde_core",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "infra preset missing `{required}`: {names:?}"
            );
        }
    }

    #[test]
    fn no_trailing_newline() {
        // Last line has no \n
        assert_eq!(parse_external_file("tokio"), vec!["tokio".to_string()]);
    }

    #[test]
    fn duplicates_preserved_caller_handles_dedup() {
        // The HashSet at the call site handles dedup; we just return the
        // raw list so the parser stays simple/predictable.
        assert_eq!(
            parse_external_file("tokio\ntokio\n"),
            vec!["tokio".to_string(), "tokio".to_string()]
        );
    }
}
