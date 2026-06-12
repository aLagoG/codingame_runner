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
//!
//! Matching is *exact* on the (property, value) pair after the
//! cleaner normalises both (lowercased property, trimmed value).
//! That's intentional: a fuzzy match would surprise people in
//! both directions. Use multiple entries for spelling variants
//! (e.g. `#fff` vs `white`).

/// Strip these properties from `style="…"` attributes — they fight
/// the dark theme or override our flex layout.
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
];

/// Keep these silently — intentional callouts that the bundled
/// CSS expects to see and doesn't try to restyle.
pub const ALLOWED_STYLES: &[(&str, &str)] = &[
    // Green "Summary of new rules" callout palette.
    ("color", "#7cc576"),
    ("background-color", "rgba(124, 197, 118,.1)"),
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
    ("margin-bottom", "10px"),
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

pub fn is_denied(property: &str, value: &str) -> bool {
    DENIED_STYLES
        .iter()
        .any(|(p, v)| *p == property && *v == value)
}

pub fn is_allowed(property: &str, value: &str) -> bool {
    ALLOWED_STYLES
        .iter()
        .any(|(p, v)| *p == property && *v == value)
}

pub fn is_known_section(token: &str) -> bool {
    KNOWN_SECTIONS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_lists_are_consistent() {
        // Same property shouldn't be both denied and allowed at the
        // same value — that would be a footgun.
        for (p, v) in DENIED_STYLES {
            assert!(
                !ALLOWED_STYLES.iter().any(|(ap, av)| ap == p && av == v),
                "{p}: {v} is in both DENIED and ALLOWED — pick one",
            );
        }
    }
}
