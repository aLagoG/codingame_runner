//! End-to-end: feed a representative paste fixture (devtools-style
//! CSS noise + the tron statement structure) through `clean()` and
//! check the output passes the structural invariants the hand-tuned
//! `tron/tron_game/instructions.html` satisfies.
//!
//! The fixture lives in `tests/fixtures/paste.html`; it mirrors the
//! shape of the real paste (white panel backgrounds, hard 50%
//! widths, the green callout, unknown statement classes absent).
//! Edit the fixture when you want to widen coverage; the test only
//! relies on the patterns it exercises, not specific text.

use std::path::PathBuf;

use cg_statement::{Warning, clean};

fn paste() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("paste.html");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn vscode_icon_css_is_stripped() {
    let out = clean(&paste()).unwrap().html;
    // The VS Code paste embeds dozens of `--vscode-icon-*` custom
    // properties before the statement content. Slicing should kill
    // every one of them.
    assert!(
        !out.contains("--vscode-icon-"),
        "VS Code icon CSS leaked into the output",
    );
}

#[test]
fn all_known_sections_survive() {
    let out = clean(&paste()).unwrap().html;
    for c in [
        "statement-goal",
        "statement-rules",
        "statement-protocol",
        "statement-victory-conditions",
        "statement-inout",
    ] {
        assert!(out.contains(c), "expected class {c} to be in the output");
    }
}

#[test]
fn bundled_css_is_injected() {
    let out = clean(&paste()).unwrap().html;
    assert!(out.contains("<style>"), "missing <style> block");
    // A distinctive bit of our hand-tuned CSS.
    assert!(
        out.contains("#252e38"),
        "missing the page background colour"
    );
    assert!(
        out.contains("statement-victory-conditions"),
        "missing victory rule"
    );
}

#[test]
fn denied_inline_styles_removed() {
    let out = clean(&paste()).unwrap().html;
    // The tron paste's inout panels are full of `background-color: white`
    // and `width: 50%`; both are on the deny list.
    assert!(
        !out.contains("background-color: white"),
        "white panel backgrounds should have been stripped",
    );
    assert!(
        !out.contains("width: 50%"),
        "50% widths should have been stripped",
    );
}

#[test]
fn green_callout_kept() {
    let out = clean(&paste()).unwrap().html;
    // The intentional "Summary of new rules" callout palette is on
    // the allow list — must survive.
    assert!(out.contains("#7cc576"), "green callout color was stripped");
}

#[test]
fn tron_paste_produces_no_warnings() {
    // The deny/allow lists were built specifically from this paste.
    // If a warning shows up here it means we missed a property or
    // section class we should already know about.
    let result = clean(&paste()).unwrap();
    if !result.warnings.is_empty() {
        // Dump for debuggability before failing.
        for w in &result.warnings {
            eprintln!("UNEXPECTED WARNING: {w:?}");
        }
        match &result.warnings[0] {
            Warning::UnknownInlineStyle { property, value } => {
                panic!("unknown inline style on tron paste: {property}: {value}",);
            }
            Warning::UnknownStatementClass(c) => {
                panic!("unknown statement class on tron paste: {c}");
            }
            Warning::NoContentBoundary => panic!("no content boundary on tron paste"),
        }
    }
}

#[test]
fn bare_img_gets_alt() {
    let out = clean(&paste()).unwrap().html;
    // The league-badge img in the source paste has no alt; the
    // cleaner should inject `alt=""`.
    let img_count = out.matches("<img").count();
    assert!(img_count > 0, "expected at least one <img>");
    let alt_count = out.matches("<img alt=").count();
    assert_eq!(
        alt_count, img_count,
        "every <img> should have an alt= attribute"
    );
}
