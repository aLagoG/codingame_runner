//! Clean a copy-pasted CodinGame statement (HTML + surrounding
//! devtools-paste noise) into the dark-themed self-contained HTML
//! page we hand-tuned for tron. The look is fixed — `style.css` is
//! the single source of truth and is bundled at compile time via
//! `include_str!`.
//!
//! What the cleaner does, in order:
//!
//!   1. **Boundary slicing.** Find the first interesting element —
//!      either the green "Summary of new rules" callout (recognized
//!      by its `color: #7cc576` inline style) or the first
//!      `<div class="statement-...">`. Throw away everything before
//!      it; pass everything after it through verbatim. That handles
//!      the typical paste shape where a chunk of unrelated CSS from
//!      DevTools leads the document.
//!
//!   2. **Inline-style scrubbing.** Each `style="…"` attribute is
//!      parsed into a property list. Properties are matched against
//!      a known-good/known-bad allow/deny list (see [`rules`]). A
//!      property in the deny list is dropped. A property in the
//!      allow list is kept silently. Anything else is kept *and*
//!      reported as a [`Warning`] so the user can review and decide
//!      whether to extend the lists.
//!
//!   3. **Section-class auditing.** Class tokens starting with
//!      `statement-` are checked against the known set; unknowns
//!      raise a warning (the bundled CSS likely won't style them).
//!
//!   4. **Polish.** Bare `<img>` tags get `alt=""`; tabs inside
//!      `<pre>` blocks become spaces.
//!
//!   5. **Scaffold.** Wrap the cleaned body in DOCTYPE + head +
//!      `<style>` (the embedded CSS) + `<div class="statement-body">`.

use std::collections::BTreeSet;

use anyhow::Result;

pub mod rules;

const STYLE: &str = include_str!("style.css");

/// FNV-1a 64-bit fingerprint of the bundled stylesheet. Stable across
/// Rust versions (unlike `DefaultHasher`), so a fingerprint embedded in
/// an `instructions.html` produced on machine A reliably compares to
/// the current fingerprint on machine B. Used purely for traceability
/// and human-readable "which cleaner version produced this file" — not
/// for cache invalidation or skipping re-clean work (`--check` always
/// does a full byte compare).
const STYLE_SHA: u64 = fnv1a_64(STYLE.as_bytes());

const fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        i += 1;
    }
    hash
}

/// Hex fingerprint of the bundled CSS. See [`STYLE_SHA`].
pub fn style_sha() -> String {
    format!("{STYLE_SHA:016x}")
}

/// Output of [`clean`].
#[derive(Debug, Clone)]
pub struct Cleanup {
    /// The full HTML document, ready to write to a file.
    pub html: String,
    /// Anything noteworthy the cleaner saw and *kept* — caller can
    /// triage and either extend the rules or strip the offending
    /// markup by hand.
    pub warnings: Vec<Warning>,
}

/// Tunables for [`clean_with_options`]. Defaults match the no-args
/// [`clean`] entry point.
#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    /// HTML `<title>` for the generated document. `None` falls back
    /// to the generic `"CodinGame Statement"` so direct callers of
    /// `cg_statement` (no game name to hand) still get something
    /// sensible in the tab.
    pub title: Option<String>,
}

const DEFAULT_TITLE: &str = "CodinGame Statement";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Warning {
    /// An inline `style` property we don't have an opinion on. Kept
    /// in the output verbatim. Format mirrors what was in the
    /// source (no normalisation).
    UnknownInlineStyle { property: String, value: String },
    /// A class token beginning with `statement-` that isn't in the
    /// known set. The element is kept but the bundled CSS probably
    /// doesn't style it.
    UnknownStatementClass(String),
    /// A class token starting with `icon-` that the bundled CSS
    /// doesn't have a `background-image` rule for. The `<span>` stays
    /// in the markup but renders empty until rules.rs + style.css
    /// are extended.
    UnknownIconClass(String),
    /// No recognisable content boundary found — the cleaner emitted
    /// the whole input as the body. Usually means the paste didn't
    /// include any `.statement-*` divs or a green-callout marker.
    NoContentBoundary,
}

/// Main entry point. Takes a raw paste, returns the cleaned
/// document + any warnings. Uses the default tab title.
pub fn clean(input: &str) -> Result<Cleanup> {
    clean_with_options(input, &CleanOptions::default())
}

/// Like [`clean`] but lets the caller override the generated `<title>`.
pub fn clean_with_options(input: &str, opts: &CleanOptions) -> Result<Cleanup> {
    let mut warnings = Vec::new();

    let body = slice_body(input, &mut warnings);
    let body = drop_sections(&body);
    let body = scrub_styles(&body, &mut warnings);
    let body = audit_statement_classes(&body, &mut warnings);
    let body = polish(&body);

    let title = opts.title.as_deref().unwrap_or(DEFAULT_TITLE);
    let html = wrap_in_scaffold(&body, title);
    Ok(Cleanup { html, warnings })
}

/// "CSS-only" upgrade path for games that don't have a saved
/// `instructions_original.html`. Extracts the body from a previously-
/// generated file (between the scaffold's outer `<div class="statement-body">`
/// and its matching close), then runs the same pipeline as [`clean`]
/// — *minus* `slice_body`, which would otherwise re-find the scaffold's
/// own wrapper as content and produce nested divs.
///
/// The whole rule set runs again, so new entries in `DENIED_STYLES`
/// / `ALLOWED_STYLES` / `KNOWN_*` lists take effect. Idempotent on
/// already-current input: re-running on `re_clean`'s own output yields
/// identical bytes (modulo CSS / rules changes).
///
/// Returns `Err` only if the input doesn't look like a previously
/// generated file (no `<div class="statement-body">` to anchor on);
/// in that case the caller should fall back to a full `clean()` from
/// a paste.
pub fn re_clean(input: &str) -> Result<Cleanup> {
    re_clean_with_options(input, &CleanOptions::default())
}

pub fn re_clean_with_options(input: &str, opts: &CleanOptions) -> Result<Cleanup> {
    let body = extract_body_from_cleaned(input).ok_or_else(|| {
        anyhow::anyhow!(
            "input doesn't look like a cleaner-generated file \
             (no <div class=\"statement-body\"> anchor found); \
             use `clean()` on the original paste instead",
        )
    })?;

    let mut warnings = Vec::new();
    let body = drop_sections(body);
    let body = scrub_styles(&body, &mut warnings);
    let body = audit_statement_classes(&body, &mut warnings);
    let body = polish(&body);

    let title = opts.title.as_deref().unwrap_or(DEFAULT_TITLE);
    let html = wrap_in_scaffold(&body, title);
    Ok(Cleanup { html, warnings })
}

/// Pull the inner content out of a cleaner-produced HTML file — the
/// bytes between `<div class="statement-body">` and its matching
/// `</div>`. Returns `None` if the anchor isn't present (the file
/// wasn't produced by this cleaner or someone has rewritten the
/// scaffold).
///
/// Reuses [`find_matching_div_close`] for the depth-counted close so
/// nested `<div>`s inside the body don't fool the search.
fn extract_body_from_cleaned(html: &str) -> Option<&str> {
    const ANCHOR: &str = r#"<div class="statement-body">"#;
    let start = html.find(ANCHOR)?;
    let body_start = start + ANCHOR.len();
    let after_close = find_matching_div_close(html, body_start);
    if after_close >= html.len() && !html[body_start..].contains("</div>") {
        return None;
    }
    let body_end = after_close.saturating_sub("</div>".len());
    Some(html[body_start..body_end].trim_matches(['\n', '\r']))
}

// ============================================================
//  1. Boundary slicing
// ============================================================

/// Find the first byte offset that looks like the actual statement
/// content (vs. devtools/CSS noise). Returns the substring from
/// that offset to EOF, or the whole input + a warning if no marker
/// is found.
fn slice_body(input: &str, warnings: &mut Vec<Warning>) -> String {
    // The two markers we recognise. We take the earlier of the
    // two — either kind of opener is a legitimate start.
    let candidates: [&str; 3] = [
        // Goal/Rules/Protocol/etc. — the canonical CodinGame section.
        r#"<div class="statement-"#,
        // The green "Summary of new rules" callout, which precedes
        // the statement-* sections when present and is styled inline.
        r#"<div style="color: #7cc576"#,
        // Same as above with single quotes — rare but defensive.
        r#"<div style='color: #7cc576"#,
    ];

    let first = candidates.iter().filter_map(|m| input.find(m)).min();

    match first {
        Some(off) => input[off..].to_string(),
        None => {
            warnings.push(Warning::NoContentBoundary);
            input.to_string()
        }
    }
}

// ============================================================
//  1b. Drop entire subtrees by class token
// ============================================================

/// Excise any `<div class="… one-of-DROPPED_SECTIONS …">…</div>` from
/// the body, including its full nested subtree. Run before
/// `scrub_styles` / `audit_statement_classes` so we don't waste work
/// (or emit warnings) on content we're discarding.
fn drop_sections(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let Some(rel) = body[i..].find("<div") else {
            out.push_str(&body[i..]);
            break;
        };
        let div_start = i + rel;
        let Some(open_end_rel) = body[div_start..].find('>') else {
            // Malformed: open `<div` with no closing `>`. Emit verbatim
            // and bail; nothing useful left to do.
            out.push_str(&body[i..]);
            break;
        };
        let open_end = div_start + open_end_rel + 1;
        let open_tag = &body[div_start..open_end];

        if div_has_dropped_class(open_tag) {
            // Excise from div_start through the matching </div>. The
            // emit cursor copies content up to (but not including)
            // div_start, then jumps to just after the matching close.
            out.push_str(&body[i..div_start]);
            i = find_matching_div_close(body, open_end);
        } else {
            // Keep this `<div …>` and continue scanning after its
            // opening tag — nested drop candidates inside still get
            // their chance.
            out.push_str(&body[i..open_end]);
            i = open_end;
        }
    }
    out
}

/// True if `open_tag` (the bytes from `<div` through the matching `>`)
/// carries a `class="…"` attribute containing any token in
/// `rules::DROPPED_SECTIONS`.
fn div_has_dropped_class(open_tag: &str) -> bool {
    let Some(start) = open_tag.find("class=") else {
        return false;
    };
    let after = &open_tag[start + "class=".len()..];
    let Some(quote) = after.chars().next() else {
        return false;
    };
    if quote != '"' && quote != '\'' {
        return false;
    }
    let inner = &after[1..];
    let Some(end) = inner.find(quote) else {
        return false;
    };
    inner[..end]
        .split_whitespace()
        .any(rules::is_dropped_section)
}

/// Given the byte offset just past the `>` of an opening `<div …>`,
/// return the offset just past the matching `</div>`. Depth-counts so
/// nested divs don't cause a premature close. If the input is
/// malformed (more opens than closes), returns `body.len()` — the
/// caller treats that as "drop everything to EOF", which is the
/// least-surprising failure mode.
fn find_matching_div_close(body: &str, mut i: usize) -> usize {
    let mut depth: usize = 1;
    while i < body.len() {
        let rest = &body[i..];
        let next_open = rest.find("<div");
        let next_close = rest.find("</div>");
        match (next_open, next_close) {
            (None, None) | (Some(_), None) => return body.len(),
            (None, Some(c)) => {
                depth -= 1;
                let after_close = i + c + "</div>".len();
                if depth == 0 {
                    return after_close;
                }
                i = after_close;
            }
            (Some(o), Some(c)) => {
                if o < c {
                    // Found a nested <div first — skip past its `>`
                    // and bump depth.
                    depth += 1;
                    let Some(open_end_rel) = body[i + o..].find('>') else {
                        return body.len();
                    };
                    i += o + open_end_rel + 1;
                } else {
                    depth -= 1;
                    let after_close = i + c + "</div>".len();
                    if depth == 0 {
                        return after_close;
                    }
                    i = after_close;
                }
            }
        }
    }
    body.len()
}

// ============================================================
//  2. Inline-style scrubbing
// ============================================================

/// Scan for `style="…"` attributes; for each, split into properties,
/// drop those on the deny list, keep the rest, and warn for any
/// property that isn't on either list.
fn scrub_styles(body: &str, warnings: &mut Vec<Warning>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        // Look for the next style= attribute. Quote may be " or '.
        let rest = &body[i..];
        let Some(attr_start) = rest.find("style=") else {
            out.push_str(rest);
            break;
        };
        // Emit everything up to the attribute start.
        out.push_str(&rest[..attr_start]);
        let after = &rest[attr_start + "style=".len()..];
        let Some(quote) = after.chars().next() else {
            out.push_str("style=");
            break;
        };
        if quote != '"' && quote != '\'' {
            // Not a quoted attribute (e.g. `style=foo`); leave alone.
            out.push_str("style=");
            i += attr_start + "style=".len();
            continue;
        }
        let after = &after[1..];
        let Some(close_rel) = after.find(quote) else {
            // Unterminated — emit verbatim and stop trying.
            out.push_str(&rest[attr_start..]);
            break;
        };
        let raw = &after[..close_rel];
        let cleaned = clean_style_attribute(raw, warnings);
        if cleaned.is_empty() {
            // Drop the entire `style="…"` attribute including a
            // leading space if there was one (cosmetic; otherwise
            // we'd leave a double space behind).
            if out.ends_with(' ') {
                out.pop();
            }
        } else {
            out.push_str("style=");
            out.push(quote);
            out.push_str(&cleaned);
            out.push(quote);
        }
        i += attr_start + "style=".len() + 1 + close_rel + 1;
    }
    out
}

/// Returns the trimmed, semicolon-joined remainder after applying
/// rules. Empty string means "drop the whole attribute".
fn clean_style_attribute(raw: &str, warnings: &mut Vec<Warning>) -> String {
    let mut kept = Vec::new();
    for prop in split_style_props(raw) {
        let prop = prop.trim();
        if prop.is_empty() {
            continue;
        }
        let Some((name, value)) = prop.split_once(':') else {
            // Malformed property; keep as-is so we don't silently
            // mangle weird markup.
            kept.push(prop.to_string());
            continue;
        };
        let name = name.trim().to_lowercase();
        let value = value.trim();

        let normalised = format!("{name}: {value}");
        if rules::is_denied(&name, value) {
            continue;
        }
        if !rules::is_allowed(&name, value) {
            warnings.push(Warning::UnknownInlineStyle {
                property: name.clone(),
                value: value.to_string(),
            });
        }
        kept.push(normalised);
    }
    kept.join("; ")
}

/// Split a CSS style attribute body on `;` boundaries, skipping `;`
/// that terminates an HTML entity (`&name;`, `&#NNN;`, `&#xHH;`).
/// Without this, `font-family: &quot;Courier New&quot;, sans-serif`
/// would parse as three properties because each `&quot;` ends in
/// `;` — and the second pseudo-property (`Courier New&quot`) has no
/// colon, so the cleaner kept a malformed fragment and emitted a
/// truncated warning of `font-family: &quot`.
///
/// Heuristic: track an `in_entity` flag set by `&`; clear it on `;`
/// (consuming that `;` as part of the entity, not as a separator) or
/// on the first byte that can't appear in a valid entity name
/// (anything outside `[A-Za-z0-9#]`). Conservative: a bare `&` later
/// followed by a real separator just behaves like the old splitter.
fn split_style_props(raw: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut start = 0usize;
    let mut in_entity = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'&' => in_entity = true,
            b';' if in_entity => in_entity = false,
            b';' => {
                out.push(&raw[start..i]);
                start = i + 1;
            }
            b if !b.is_ascii_alphanumeric() && b != b'#' => in_entity = false,
            _ => {}
        }
    }
    if start < raw.len() {
        out.push(&raw[start..]);
    }
    out
}

// ============================================================
//  3. Section-class auditing
// ============================================================

fn audit_statement_classes(body: &str, warnings: &mut Vec<Warning>) -> String {
    // Scan for `class="…"` attributes; for each token starting with
    // `statement-` or `icon-`, check membership in the matching
    // known set. We don't *modify* the markup here — just observe.
    // BTreeSets dedup so we emit each unknown class once even if it
    // recurs (e.g. icon-example appears on every `<h2>` heading).
    let mut unknown_sections: BTreeSet<String> = BTreeSet::new();
    let mut unknown_icons: BTreeSet<String> = BTreeSet::new();
    let mut i = 0usize;
    while let Some(off) = body[i..].find("class=") {
        let start = i + off + "class=".len();
        let after = &body[start..];
        let Some(quote) = after.chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            i = start;
            continue;
        }
        let inner = &after[1..];
        let Some(end_rel) = inner.find(quote) else {
            break;
        };
        for token in inner[..end_rel].split_whitespace() {
            if token.starts_with("statement-") && !rules::is_known_section(token) {
                unknown_sections.insert(token.to_string());
            } else if token.starts_with("icon-") && !rules::is_known_icon(token) {
                unknown_icons.insert(token.to_string());
            }
        }
        i = start + 1 + end_rel + 1;
    }
    for s in unknown_sections {
        warnings.push(Warning::UnknownStatementClass(s));
    }
    for c in unknown_icons {
        warnings.push(Warning::UnknownIconClass(c));
    }
    body.to_string()
}

// ============================================================
//  4. Polish
// ============================================================

fn polish(body: &str) -> String {
    let body = add_alt_to_imgs(body);
    detab_pre_blocks(&body)
}

fn add_alt_to_imgs(body: &str) -> String {
    // Cheap and correct enough: for each `<img ` that doesn't have
    // ` alt=` before its `>`, insert ` alt=""` right after `<img`.
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(off) = body[i..].find("<img") {
        let abs = i + off;
        out.push_str(&body[i..abs]);
        // Find the closing `>` of this tag.
        let after = &body[abs..];
        let Some(close_rel) = after.find('>') else {
            out.push_str(after);
            break;
        };
        let tag = &after[..=close_rel];
        if tag.contains(" alt=") || tag.contains("\talt=") {
            out.push_str(tag);
        } else {
            // Insert ` alt=""` after `<img`.
            out.push_str("<img alt=\"\"");
            out.push_str(&tag["<img".len()..]);
        }
        i = abs + close_rel + 1;
    }
    out.push_str(&body[i..]);
    out
}

fn detab_pre_blocks(body: &str) -> String {
    // Replace tabs only inside `<pre>...</pre>` ranges. Outside
    // <pre> any tabs are usually indentation in the source we
    // don't want to touch.
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(start_rel) = body[i..].find("<pre") {
        let start = i + start_rel;
        // Find the end of the opening tag, then the closing </pre>.
        let after = &body[start..];
        let Some(open_end_rel) = after.find('>') else {
            out.push_str(&body[i..]);
            return out;
        };
        let open_end = start + open_end_rel + 1;
        let Some(close_rel) = body[open_end..].find("</pre>") else {
            out.push_str(&body[i..]);
            return out;
        };
        let close = open_end + close_rel;
        out.push_str(&body[i..open_end]);
        out.push_str(&body[open_end..close].replace('\t', "    "));
        i = close;
    }
    out.push_str(&body[i..]);
    out
}

// ============================================================
//  5. Scaffold
// ============================================================

fn wrap_in_scaffold(body: &str, title: &str) -> String {
    let title = escape_title(title);
    // The `cg_statement` comment carries an FNV-1a fingerprint of the
    // bundled CSS so a future re-run / `--check` can show which
    // version of the cleaner produced this file. Single-line so simple
    // string surgery (e.g. external tooling) can spot it.
    format!(
        "<!DOCTYPE html>
<!-- cg_statement: style-sha={STYLE_SHA:016x}; do not edit by hand — re-run `cargo xtask statement <game> --upgrade`. -->
<html lang=\"en\">
<head>
<meta charset=\"UTF-8\">
<title>{title}</title>
<style>
{STYLE}</style>
</head>
<body>
<div class=\"statement-body\">
{body}
</div>
</body>
</html>
",
    )
}

/// Minimal HTML escape for the `<title>` element. We only inject in
/// one place; restricted to the characters that actually matter inside
/// `<title>...</title>` (no attribute escaping needed).
fn escape_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            c => out.push(c),
        }
    }
    out
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_picks_earliest_marker() {
        let input = r#"
            <style>.foo {color: red}</style>
            <div class="statement-goal">goal</div>
            <div style="color: #7cc576">callout</div>
        "#;
        let body = slice_body(input, &mut vec![]);
        assert!(
            body.trim_start()
                .starts_with(r#"<div class="statement-goal""#)
        );
    }

    #[test]
    fn slice_uses_callout_when_it_comes_first() {
        let input = r#"
            <div style="color: #7cc576">callout</div>
            <div class="statement-goal">goal</div>
        "#;
        let body = slice_body(input, &mut vec![]);
        assert!(
            body.trim_start()
                .starts_with(r#"<div style="color: #7cc576""#)
        );
    }

    #[test]
    fn slice_warns_when_no_marker() {
        let mut w = vec![];
        let body = slice_body("<p>nothing relevant</p>", &mut w);
        assert_eq!(body, "<p>nothing relevant</p>");
        assert_eq!(w, vec![Warning::NoContentBoundary]);
    }

    #[test]
    fn drop_sections_excises_story_background() {
        let body = r#"<p>before</p><div class="statement-story-background"><div class="statement-story"><h1>x</h1></div></div><p>after</p>"#;
        let out = drop_sections(body);
        assert_eq!(out, "<p>before</p><p>after</p>");
    }

    #[test]
    fn drop_sections_handles_nested_unrelated_divs_inside_target() {
        // Depth counter has to skip the inner unrelated <div> when
        // closing the dropped section.
        let body =
            r#"a<div class="statement-story-background"><div>noise<div>deep</div></div></div>b"#;
        let out = drop_sections(body);
        assert_eq!(out, "ab");
    }

    #[test]
    fn drop_sections_leaves_other_divs_alone() {
        let body =
            r#"<div class="statement-goal">keep</div><div class="statement-story">drop</div>"#;
        let out = drop_sections(body);
        assert_eq!(out, r#"<div class="statement-goal">keep</div>"#);
    }

    #[test]
    fn drop_sections_handles_multiclass_attribute() {
        let body = r#"<div class="statement-section statement-story">drop</div><p>keep</p>"#;
        let out = drop_sections(body);
        assert_eq!(out, "<p>keep</p>");
    }

    #[test]
    fn scrub_drops_denied_keeps_allowed() {
        let mut w = vec![];
        // `transform` is on neither list → kept + warning. `bg white`
        // is denied → stripped. `color: #7cc576` is the green callout
        // → allowed silently.
        let input = r#"<div style="background-color: white; color: #7cc576; transform: rotate(5deg)">x</div>"#;
        let out = scrub_styles(input, &mut w);
        assert!(!out.contains("background-color: white"));
        assert!(out.contains("color: #7cc576"));
        assert!(out.contains("transform: rotate(5deg)"));
        assert!(w.iter().any(
            |w| matches!(w, Warning::UnknownInlineStyle { property, .. } if property == "transform")
        ));
    }

    #[test]
    fn scrub_strips_table_cell_padding() {
        // The bundled CSS now styles `th, td { padding: 5px }`, so
        // the inline copy is redundant and `padding: 5px` is on the
        // deny list. Verify it's stripped silently.
        let mut w = vec![];
        let input = r#"<td style="padding: 5px">cell</td>"#;
        let out = scrub_styles(input, &mut w);
        assert!(!out.contains("padding: 5px"));
        assert!(w.is_empty(), "stripping a denied property should not warn");
    }

    #[test]
    fn scrub_strips_subsection_h3_inline_styles() {
        // All 5 properties are on the deny list — the whole `style="…"`
        // attribute drops out and our bundled `.statement-section h3`
        // rule takes over.
        let mut w = vec![];
        let input = r#"<h3 style="font-size: 24px; margin-top: 20px; margin-bottom: 10px; font-weight: 500; line-height: 1.1">The map</h3>"#;
        let out = scrub_styles(input, &mut w);
        assert!(!out.contains("style="));
        assert!(w.is_empty());
    }

    #[test]
    fn scrub_handles_entity_in_value() {
        // `font-family: &quot;Courier New&quot;, monospace` — the
        // `;` inside `&quot;` must NOT be treated as a property
        // separator. Pre-fix this produced a malformed warning of
        // `font-family: &quot` and the rest got mangled.
        let mut w = vec![];
        let input =
            r#"<span style="font-family: &quot;Courier New&quot;, monospace">x</span>"#;
        let out = scrub_styles(input, &mut w);
        assert!(out.contains("&quot;Courier New&quot;, monospace"));
        // Exactly ONE warning, with the full value.
        let style_warns: Vec<_> = w
            .iter()
            .filter(|w| matches!(w, Warning::UnknownInlineStyle { .. }))
            .collect();
        assert_eq!(style_warns.len(), 1, "expected exactly one warning, got {:?}", style_warns);
        if let Warning::UnknownInlineStyle { value, .. } = style_warns[0] {
            assert!(
                value.contains("Courier New"),
                "warning value should carry the full string, got {value:?}",
            );
        }
    }

    #[test]
    fn scrub_removes_whole_attr_when_all_denied() {
        let mut w = vec![];
        let input = r#"<div style="background-color: white; width: 50%">x</div>"#;
        let out = scrub_styles(input, &mut w);
        assert!(!out.contains("style="));
        assert!(w.is_empty()); // both properties were on the deny list
    }

    #[test]
    fn audit_warns_on_unknown_section() {
        let mut w = vec![];
        audit_statement_classes(r#"<div class="statement-novel">x</div>"#, &mut w);
        assert_eq!(
            w,
            vec![Warning::UnknownStatementClass("statement-novel".into())],
        );
    }

    #[test]
    fn audit_silent_on_known_section() {
        let mut w = vec![];
        audit_statement_classes(r#"<div class="statement-goal">x</div>"#, &mut w);
        assert!(w.is_empty());
    }

    #[test]
    fn audit_silent_on_known_icon() {
        let mut w = vec![];
        audit_statement_classes(
            r#"<span class="icon icon-goal">&nbsp;</span>"#,
            &mut w,
        );
        assert!(w.is_empty());
    }

    #[test]
    fn audit_warns_on_unknown_icon() {
        let mut w = vec![];
        audit_statement_classes(
            r#"<span class="icon icon-novel">&nbsp;</span>"#,
            &mut w,
        );
        assert_eq!(
            w,
            vec![Warning::UnknownIconClass("icon-novel".into())],
        );
    }

    #[test]
    fn audit_dedups_repeated_icons() {
        // Spider Attack's `icon-example` appears on two `<h2>` headings.
        // Even if it weren't known, the audit should warn ONCE.
        let mut w = vec![];
        audit_statement_classes(
            r#"<span class="icon icon-novel">a</span><span class="icon icon-novel">b</span>"#,
            &mut w,
        );
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn polish_adds_alt_to_bare_img() {
        let out = add_alt_to_imgs(r#"<img src="foo.png"><img src="bar.png" alt="bar">"#);
        assert!(out.contains(r#"<img alt="" src="foo.png">"#));
        // Already-alt'd image untouched (the `alt="bar"` is preserved).
        assert!(out.contains(r#"alt="bar""#));
    }

    #[test]
    fn polish_detabs_only_inside_pre() {
        let out = detab_pre_blocks("before\ttab<pre>inside\ttab</pre>after\ttab");
        assert_eq!(out, "before\ttab<pre>inside    tab</pre>after\ttab");
    }

    #[test]
    fn scaffold_includes_style_block_and_body() {
        let html = wrap_in_scaffold("<p>hi</p>", "Test Title");
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<style>"));
        assert!(html.contains("<title>Test Title</title>"));
        // The body content lives inside the statement-body wrapper.
        assert!(html.contains(r#"<div class="statement-body">"#));
        assert!(html.contains("<p>hi</p>"));
    }

    #[test]
    fn scaffold_escapes_title() {
        let html = wrap_in_scaffold("<p>x</p>", "A <bad> & ugly title");
        assert!(html.contains("<title>A &lt;bad&gt; &amp; ugly title</title>"));
    }

    #[test]
    fn clean_with_options_threads_title_through() {
        let opts = CleanOptions {
            title: Some("Fantastic Bits - Game Statement".into()),
        };
        let out = clean_with_options("<p>x</p>", &opts).unwrap();
        assert!(
            out.html
                .contains("<title>Fantastic Bits - Game Statement</title>")
        );
    }

    #[test]
    fn scaffold_stamps_style_sha() {
        let html = wrap_in_scaffold("<p>x</p>", "T");
        let expected_marker = format!("style-sha={}", style_sha());
        assert!(html.contains(&expected_marker), "missing style-sha stamp");
    }

    #[test]
    fn style_sha_is_stable_per_build() {
        // Cheap regression guard: the sha must not change unless
        // STYLE actually does. Two calls in the same process must
        // match.
        assert_eq!(style_sha(), style_sha());
    }

    #[test]
    fn extract_body_round_trips_through_re_clean() {
        // clean → re_clean → clean produces byte-equal output. The
        // first clean turns the paste into a cleaned file. re_clean
        // strips the scaffold + reruns rules; same scaffold goes back
        // on. Idempotent.
        let paste = r#"<div class="statement-goal"><h1>Goal</h1><p>do the thing</p></div>"#;
        let once = clean(paste).unwrap().html;
        let twice = re_clean(&once).unwrap().html;
        assert_eq!(once, twice);
    }

    #[test]
    fn re_clean_picks_up_new_deny_rules() {
        // Simulate the case where someone hand-wrote a body div with
        // an inline style currently on DENIED. After re_clean the
        // style should be stripped FROM THE BODY. The bundled CSS
        // still references `font-size: 24px` in the h3 subsection
        // rule, so we must look at the body specifically.
        let raw = r#"<!DOCTYPE html><html><head><style></style></head><body>
<div class="statement-body">
<h3 style="font-size: 24px; color: red">x</h3>
</div></body></html>"#;
        let out = re_clean(raw).unwrap().html;
        let body = extract_body_from_cleaned(&out).expect("re_clean must emit a scaffolded body");
        assert!(
            !body.contains("font-size: 24px"),
            "denied font-size: 24px leaked into body: {body}",
        );
        assert!(body.contains("color: red"), "unknown color should be kept");
    }

    #[test]
    fn re_clean_errors_on_non_scaffold_input() {
        // No anchor → returns Err so the caller can suggest a full
        // `clean()` from the original paste.
        let err = re_clean("<p>not a generated file</p>").unwrap_err();
        assert!(
            err.to_string().contains("doesn't look like"),
            "unexpected error: {err}",
        );
    }
}
