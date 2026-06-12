//! Allow/deny lists for cleanup decisions. Edit this file when a
//! new CodinGame game introduces patterns we want to silence or
//! suppress — adding to a list never changes behaviour for already-
//! recognised inputs.
//!
//! Three lists:
//!   * [`DENIED_STYLES`] — inline style properties+values that
//!     fight the bundled dark theme. Stripped silently from the
//!     output.
//!   * [`ALLOWED_STYLES`] — properties+values that are intentionally
//!     part of the look (e.g. the green "Summary of new rules"
//!     callout's color). Kept silently.
//!   * [`KNOWN_SECTIONS`] — every `statement-<X>` class token that
//!     the bundled CSS knows how to style. Unknown tokens get
//!     warnings so the user can extend the CSS.
//!   * [`KNOWN_ICONS`] — every `icon-<X>` class token the bundled
//!     CSS has a `background-image` for. Unknown ones warn so we
//!     know to add the corresponding SVG to `style.css`.
//!
//! Matching normalises both the property name (lowercased) and the
//! value (via [`normalize_value`]). Normalisation collapses common
//! lexical variants so e.g. `rgba(124, 197, 118, 0.1)` matches the
//! same rule as `rgba(124, 197, 118, .1)` — without it, every
//! formatting variant needs its own entry.

/// Strip these properties from `style="…"` attributes — they fight
/// the dark theme or override our flex layout.
///
/// Values here are matched against the *normalised* value of the
/// inline style (see [`normalize_value`]). Add the canonical form —
/// don't list every formatting variant.
pub const DENIED_STYLES: &[(&str, &str)] = &[
    // White panel backgrounds on the input/output example panels.
    ("background-color", "white"),
    ("background-color", "#fff"),
    ("background-color", "#ffffff"),
    ("background", "white"),
    ("background", "#fff"),
    ("background", "#ffffff"),
    // Hard 50% widths fight our flex layout.
    ("width", "50%"),
    // Muted-grey title color that doesn't read on dark bg.
    ("color", "#989898"),
    // Zero-padding override on titles inside inout panels.
    ("padding", "0"),
    // Subsection <h3> inline styles ("The map", "Heroes", "Monsters",
    // "Spells" in Spider Attack). The bundled CSS now styles
    // descendant h3s via `.statement-section :not(.statement-section) h3`,
    // so these inline overrides would only re-assert defaults the
    // theme already provides. Stripping keeps the markup terse and
    // ensures a theme tweak takes effect everywhere.
    ("font-size", "24px"),
    ("margin-top", "20px"),
    ("margin-bottom", "10px"),
    ("font-weight", "500"),
    ("line-height", "1.1"),
    // Table cell padding — the bundled `th, td` rule sets the same
    // 5px so the inline copy is noise.
    ("padding", "5px"),
    // Monospace font-family override on inline <const> tags. The
    // bundled `const { font-family: Menlo, ... }` rule already wins
    // visually; the inline copy is just CodinGame echoing the same
    // intent. The `&quot;` form matches the HTML-escaped quotes the
    // paste uses; we don't decode entities in normalize_value yet so
    // the entry has to be written verbatim.
    ("font-family", "&quot;Courier New&quot;, Courier, monospace"),
];

/// Keep these silently — intentional callouts that the bundled
/// CSS expects to see and doesn't try to restyle.
///
/// As with [`DENIED_STYLES`], values are matched after normalisation.
/// One entry covers all spacing/zero-pad/decimal-leading-zero variants.
pub const ALLOWED_STYLES: &[(&str, &str)] = &[
    // Green "Summary of new rules" callout palette. The normaliser
    // collapses `0.1` → `.1` so the `0.1` paste variant matches too.
    ("color", "#7cc576"),
    ("background-color", "rgba(124, 197, 118, .1)"),
    // Common layout polish that's safe on dark theme.
    ("text-align", "center"),
    ("text-align", "left"),
    ("font-weight", "700"),
    ("font-weight", "bold"),
    // Margin/padding tweaks on the callout. Safe to keep.
    ("padding", "20px"),
    ("padding", "16px 20px"),
    ("margin-right", "15px"),
    ("margin-left", "15px"),
    // `margin-bottom: 10px` intentionally NOT here — it's in DENIED
    // for the subsection-h3 cleanup. The green callout's identical
    // value would have been silently kept, but stripping it is
    // harmless: the callout's other margins still apply.
    ("margin-bottom", "6px"),
    ("margin", "0 0 18px 0"),
    ("border-radius", "3px"),
];

/// Class tokens that mark a `<div>` whose entire subtree should be
/// dropped from the output. Matching is exact on a single class
/// token within the element's `class="…"` attribute. Nested
/// occurrences are handled by depth counting — the outer match
/// always wins, so listing both an outer wrapper (e.g.
/// `statement-story-background`) and its child (`statement-story`)
/// is fine; the child entry is just a fallback for pastes that omit
/// the wrapper.
pub const DROPPED_SECTIONS: &[&str] = &["statement-story-background", "statement-story"];

pub fn is_dropped_section(token: &str) -> bool {
    DROPPED_SECTIONS.contains(&token)
}

/// All `statement-X` class tokens the bundled CSS knows how to
/// style. Tron's statement covers all of these; future games may
/// introduce new ones and trigger a warning.
pub const KNOWN_SECTIONS: &[&str] = &[
    "statement-section",
    "statement-body",
    "statement-goal",
    "statement-goal-content",
    "statement-rules",
    "statement-rules-content",
    "statement-protocol",
    "statement-victory-conditions",
    "statement-lose-conditions",
    "statement-expertrules",
    "statement-expert-rules-content",
    "statement-inout",
    "statement-inout-in",
    "statement-inout-out",
    "statement-lineno",
    "statement-example",
    "statement-example-container",
    "statement-example-empty",
    "statement-examples",
    "statement-examples-text",
    "statement-league-alert-content",
];

/// `icon-X` class tokens the bundled CSS has a `background-image`
/// SVG for. An unknown `icon-X` token means the bundled theme
/// doesn't know how to draw that icon — the `<span>` will render
/// empty until rules.rs + style.css are extended together.
///
/// Section-strip icons (`victory`, `lose` on `.statement-victory-conditions`
/// / `.statement-lose-conditions`) are intentionally *not* in this
/// list — they don't use the `icon-` prefix, so the audit ignores
/// them.
pub const KNOWN_ICONS: &[&str] = &[
    "icon-goal",
    "icon-rules",
    "icon-protocol",
    "icon-example",
    "icon-expertrules",
];

pub fn is_denied(property: &str, value: &str) -> bool {
    let v = normalize_value(value);
    DENIED_STYLES
        .iter()
        .any(|(p, rv)| *p == property && normalize_value(rv) == v)
}

pub fn is_allowed(property: &str, value: &str) -> bool {
    let v = normalize_value(value);
    ALLOWED_STYLES
        .iter()
        .any(|(p, rv)| *p == property && normalize_value(rv) == v)
}

pub fn is_known_section(token: &str) -> bool {
    KNOWN_SECTIONS.contains(&token)
}

pub fn is_known_icon(token: &str) -> bool {
    KNOWN_ICONS.contains(&token)
}

/// Collapse the lexical variants CSS treats as equivalent so the
/// allow/deny lists don't need an entry per formatting style.
///
/// Currently handles:
///   * Whitespace trimming + lowercasing the whole value.
///   * Single-space-after-comma in function args (so `rgba(1,2,3,.1)`,
///     `rgba(1, 2, 3, .1)`, and `rgba(1,  2,  3,  .1)` all collapse
///     to `rgba(1, 2, 3, .1)`). The pass runs on every value, not
///     just inside parens — harmless since non-function values rarely
///     contain commas.
///   * Leading-zero decimals: `0.1` → `.1` (so `rgba(.., 0.1)` and
///     `rgba(.., .1)` collapse to one rule).
///   * Zero with length unit: `0px` → `0` (CSS treats zero as
///     unitless; an inline `padding-bottom: 0px` and a deny rule of
///     `padding-bottom: 0` should match).
///
/// Numbers are detected by scanning for runs of `[0-9.+\-]`; the
/// unit (if any) follows immediately. The transform only fires on
/// exact-zero forms — `0.5px` stays `.5px` after the leading-zero
/// pass, `10px` stays `10px`.
pub fn normalize_value(value: &str) -> String {
    let lower = value.trim().to_ascii_lowercase();

    // Comma pass: always exactly one space after each comma, no
    // matter how the original was formatted.
    let mut comma_norm = String::with_capacity(lower.len());
    let mut chars = lower.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            comma_norm.push(',');
            comma_norm.push(' ');
            while chars.peek().is_some_and(|n| n.is_whitespace()) {
                chars.next();
            }
        } else {
            comma_norm.push(c);
        }
    }

    // Numeric pass: walk byte-wise; safe because every char we care
    // about (digits, '.', ASCII letters for units) is single-byte.
    let mut out = String::with_capacity(comma_norm.len());
    let bytes = comma_norm.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let is_number_start =
            b.is_ascii_digit() || (b == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit());
        if !is_number_start {
            out.push(b as char);
            i += 1;
            continue;
        }
        // Greedy: scan a numeric token (digits + optional `.`), then
        // a trailing unit identifier (alphabetic only — `px`, `em`,
        // `rem`; we deliberately don't try to capture `%` since the
        // 0%-vs-0 collapse is unsound for grids).
        let num_start = i;
        let mut saw_dot = false;
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_digit() {
                i += 1;
            } else if c == b'.' && !saw_dot {
                saw_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        let num_end = i;
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit_end = i;
        // Slice the *post-comma-pass* string — indices `num_start`,
        // `num_end`, `unit_start`, `unit_end` are byte offsets into
        // `comma_norm`. Slicing `lower` here was a leftover from
        // before the comma pass existed and went out of bounds when
        // the pass added bytes.
        let number = &comma_norm[num_start..num_end];
        let unit = &comma_norm[unit_start..unit_end];

        // 0.X → .X
        let canon_number = if let Some(rest) = number.strip_prefix("0.") {
            format!(".{rest}")
        } else {
            number.to_string()
        };

        // 0<unit> → 0 (drop the unit when the value is exactly zero).
        let drop_unit = unit == "px" && is_numeric_zero(&canon_number);
        out.push_str(&canon_number);
        if !drop_unit {
            out.push_str(unit);
        }
    }
    out
}

fn is_numeric_zero(s: &str) -> bool {
    // "0", "00", "0.0", ".0" all parse to zero; we cover the common forms.
    s.chars()
        .all(|c| c == '0' || c == '.')
        && s.chars().any(|c| c == '0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_lists_are_consistent() {
        // Same property shouldn't be both denied and allowed at the
        // same NORMALIZED value — that would be a footgun. Use
        // normalize_value so e.g. `padding: 0` and `padding: 0px`
        // don't slip past as separate strings.
        for (p, v) in DENIED_STYLES {
            let nv = normalize_value(v);
            assert!(
                !ALLOWED_STYLES
                    .iter()
                    .any(|(ap, av)| ap == p && normalize_value(av) == nv),
                "{p}: {v} is in both DENIED and ALLOWED — pick one",
            );
        }
    }

    #[test]
    fn normalize_collapses_zero_decimal() {
        assert_eq!(normalize_value("0.1"), ".1");
        assert_eq!(normalize_value("rgba(124, 197, 118, 0.1)"), "rgba(124, 197, 118, .1)");
    }

    #[test]
    fn normalize_collapses_zero_px() {
        assert_eq!(normalize_value("0px"), "0");
        assert_eq!(normalize_value("padding-top: 0px"), "padding-top: 0");
    }

    #[test]
    fn normalize_keeps_non_zero_units() {
        assert_eq!(normalize_value("24px"), "24px");
        assert_eq!(normalize_value("0.5em"), ".5em");
        assert_eq!(normalize_value("10px"), "10px");
    }

    #[test]
    fn normalize_is_case_insensitive() {
        assert_eq!(normalize_value("RGBA(0, 0, 0, 1)"), "rgba(0, 0, 0, 1)");
    }

    #[test]
    fn rgba_0_dot_1_matches_dot_1_rule() {
        assert!(is_allowed("background-color", "rgba(124, 197, 118, 0.1)"));
        assert!(is_allowed("background-color", "rgba(124, 197, 118, .1)"));
        // The tron paste's variant — no space after the last comma.
        assert!(is_allowed("background-color", "rgba(124, 197, 118,.1)"));
    }

    #[test]
    fn normalize_collapses_comma_spacing() {
        // All three should produce the same canonical form.
        let a = normalize_value("rgba(124, 197, 118,.1)");
        let b = normalize_value("rgba(124, 197, 118, .1)");
        let c = normalize_value("rgba(124,197,118,0.1)");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn zero_px_matches_zero_rule() {
        // Both forms should hit the same denied entry once we add
        // `padding-top: 0` to DENIED. For now just verify normalize
        // would let them match if added.
        assert_eq!(normalize_value("0"), normalize_value("0px"));
    }
}
