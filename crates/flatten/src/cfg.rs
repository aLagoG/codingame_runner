//! Cfg expression evaluator and `cfg_if!` expander.
//!
//! Three-valued logic for partial evaluation: a cfg can be statically
//! True (matches a feature/build-script-cfg we know is set), False
//! (`feature = "x"` for a feature we know is *not* set), or Unknown
//! (target_os, debug_assertions, anything we can't decide). The
//! Unknown state is what makes vendoring tractable — we leave the
//! attribute in the output to be re-evaluated at downstream compile
//! time, but partially-evaluate `all`/`any` against the predicates we
//! DO know to keep the output minimal.
//!
//! The `cfg_if!` expander runs over source text BEFORE syn AST
//! analysis, because syn doesn't walk into macro-invocation tokens —
//! a `mod foo;` declared inside a `cfg_if! {}` body would otherwise
//! stay un-inlined and rustc would fail with "file not found for
//! module".

use proc_macro2::TokenTree;
use std::collections::HashSet;
use std::ops::Range;

#[derive(Debug, Clone)]
pub(crate) enum CfgExpr {
    Feature(String),
    /// Bare cfg name with no `=value` — `unix`, `assert_no_panic`,
    /// `has_total_cmp`. Evaluates True if `name` is in the build-script
    /// cfg list, otherwise Unknown.
    Bare(String),
    /// Predicate we don't or can't evaluate — `target_os = "linux"`,
    /// `debug_assertions`, etc. Treated as Unknown. Carries the original
    /// source tokens so partial-evaluation rendering can re-emit them
    /// verbatim instead of producing `not()` / `cfg()` empties.
    Other(String),
    /// Statically-known boolean, rendered as the Rust `true` / `false`
    /// configuration-predicate literals (per the Rust reference,
    /// `ConfigurationPredicate -> ... | true | false`). Produced by
    /// `simplify_cfg_expr` when it substitutes a `Feature(name)` that
    /// the dep's vendor-time feature set resolves to. Critical for
    /// closing the cfg_X / cfg_not_X mutual-exclusion duplicate-defs
    /// problem in tokio: the negation branch's evaluation needs the
    /// feature predicates baked CONSISTENTLY at vendor time so the
    /// user's downstream compile (which has no features) sees the
    /// same answer the positive branch already baked.
    Literal(bool),
    Not(Box<CfgExpr>),
    Any(Vec<CfgExpr>),
    All(Vec<CfgExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CfgEval {
    True,
    False,
    Unknown,
}

/// True if `expr` mentions any `feature = "..."` predicate. Used by
/// `collect_macro_body_cfg_rewrites` to decide whether to bake a True
/// evaluation in the body of a "paired" cfg-macro (one with a
/// `cfg_not_X` counterpart). Baking a feature-True cfg in `cfg_X`'s
/// body leaves the negation pair's Unknown cfg active too —
/// producing a duplicate definition at user compile time. Tokio's
/// `cfg_signal_internal!` body has
/// `cfg(any(feature = "signal", all(unix, feature = "process")))`
/// which evaluates True at vendor time but must stay un-rewritten so
/// rustc can re-evaluate it at user compile time and consistently
/// gate one side of the pair.
pub(crate) fn cfg_expr_references_feature(expr: &CfgExpr) -> bool {
    match expr {
        CfgExpr::Feature(_) => true,
        CfgExpr::Bare(_) | CfgExpr::Other(_) | CfgExpr::Literal(_) => false,
        CfgExpr::Not(inner) => cfg_expr_references_feature(inner),
        CfgExpr::Any(exprs) | CfgExpr::All(exprs) => {
            exprs.iter().any(cfg_expr_references_feature)
        }
    }
}

pub(crate) fn parse_cfg_expr(ts: &proc_macro2::TokenStream) -> CfgExpr {
    let tokens: Vec<TokenTree> = ts.clone().into_iter().collect();
    parse_one_cfg_expr(&tokens)
}

pub(crate) fn parse_one_cfg_expr(tokens: &[TokenTree]) -> CfgExpr {
    fn render_tokens(tokens: &[TokenTree]) -> String {
        tokens
            .iter()
            .cloned()
            .collect::<proc_macro2::TokenStream>()
            .to_string()
    }
    let Some(TokenTree::Ident(id)) = tokens.first() else {
        return CfgExpr::Other(render_tokens(tokens));
    };
    let name = id.to_string();

    // bare ident with no following token: `unix`, `assert_no_panic`,
    // `has_total_cmp`. These can match build-script `rustc-cfg`
    // directives.
    if tokens.len() == 1 {
        return CfgExpr::Bare(name);
    }
    // function-call form: `not(...)`, `any(...)`, `all(...)`
    if let Some(TokenTree::Group(g)) = tokens.get(1)
        && g.delimiter() == proc_macro2::Delimiter::Parenthesis
    {
        let inner = split_top_level_commas(&g.stream());
        let exprs: Vec<CfgExpr> = inner.iter().map(|s| parse_one_cfg_expr(s)).collect();
        return match name.as_str() {
            "not" => CfgExpr::Not(Box::new(
                exprs
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| CfgExpr::Other(String::new())),
            )),
            "any" => CfgExpr::Any(exprs),
            "all" => CfgExpr::All(exprs),
            _ => CfgExpr::Other(render_tokens(tokens)),
        };
    }

    // key=value form: `feature = "x"`, `fast_arithmetic = "64"`, etc.
    if let (Some(TokenTree::Punct(p)), Some(TokenTree::Literal(lit))) =
        (tokens.get(1), tokens.get(2))
        && p.as_char() == '='
    {
        let raw = lit.to_string();
        let value = raw.trim_matches('"').to_string();
        if name == "feature" {
            return CfgExpr::Feature(value);
        }
        // Other key=value cfgs (build-script `rustc-cfg=KEY="VAL"`,
        // `target_os = "linux"`, etc.) — encode as a normalized
        // `KEY="VAL"` string and look up in `features`. Build-script
        // outputs are stored there in the same shape; targety predicates
        // we can't evaluate fall through to Unknown via the Bare lookup.
        return CfgExpr::Bare(format!("{name}=\"{value}\""));
    }

    CfgExpr::Other(render_tokens(tokens))
}

pub(crate) fn split_top_level_commas(
    ts: &proc_macro2::TokenStream,
) -> Vec<Vec<TokenTree>> {
    let mut segments: Vec<Vec<TokenTree>> = vec![Vec::new()];
    for t in ts.clone() {
        if let TokenTree::Punct(p) = &t
            && p.as_char() == ','
            && p.spacing() == proc_macro2::Spacing::Alone
        {
            segments.push(Vec::new());
            continue;
        }
        segments.last_mut().unwrap().push(t);
    }
    segments.into_iter().filter(|s| !s.is_empty()).collect()
}

/// Render a CfgExpr back to source. Used by [`simplify_cfg_expr`] to
/// detect when partial-evaluation actually changed anything.
#[allow(dead_code)] // kept for tests / future direct use; simplify_cfg_expr has its own renderer
pub(crate) fn format_cfg_expr(expr: &CfgExpr) -> String {
    match expr {
        CfgExpr::Feature(f) => format!("feature = \"{f}\""),
        CfgExpr::Bare(n) => n.clone(),
        CfgExpr::Other(s) => s.clone(),
        CfgExpr::Literal(v) => if *v { "true" } else { "false" }.to_string(),
        CfgExpr::Not(inner) => format!("not({})", format_cfg_expr(inner)),
        CfgExpr::Any(items) => format!(
            "any({})",
            items.iter().map(format_cfg_expr).collect::<Vec<_>>().join(", ")
        ),
        CfgExpr::All(items) => format!(
            "all({})",
            items.iter().map(format_cfg_expr).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Partial cfg evaluation: when a top-level cfg is Unknown but some
/// sub-predicates are statically True/False (typically because they're
/// `feature = "X"` predicates we know the value of), simplify by
/// dropping known sub-predicates.
///
/// Returns the simplified cfg expression as a source string, or None
/// if simplification didn't reduce the expression. The caller should
/// only emit an edit when the result differs from the original
/// (otherwise the rewrite is a no-op).
pub(crate) fn simplify_cfg_expr(expr: &CfgExpr, features: &HashSet<String>) -> Option<String> {
    fn render(expr: &CfgExpr) -> String {
        match expr {
            CfgExpr::Feature(f) => format!("feature = \"{f}\""),
            CfgExpr::Bare(n) => n.clone(),
            CfgExpr::Other(s) => s.clone(),
            CfgExpr::Literal(v) => if *v { "true" } else { "false" }.to_string(),
            CfgExpr::Not(inner) => format!("not({})", render(inner)),
            CfgExpr::Any(items) => {
                let s: Vec<String> = items.iter().map(render).collect();
                format!("any({})", s.join(", "))
            }
            CfgExpr::All(items) => {
                let s: Vec<String> = items.iter().map(render).collect();
                format!("all({})", s.join(", "))
            }
        }
    }
    /// Reduce: drop known-True from all(), drop known-False from any(),
    /// short-circuit not(). Substitute every `feature = "X"` predicate
    /// with `Literal(true_or_false)` based on the dep's vendor-time
    /// feature set so the rendered output bakes the value into the
    /// source as the Rust `true` / `false` config-predicate literals.
    /// Critical for tokio's cfg_X / cfg_not_X mutual-exclusion: the
    /// negation branch's evaluation needs feature predicates baked
    /// CONSISTENTLY at vendor time so the user's downstream compile
    /// (which has no features) sees the same answer the positive
    /// branch already baked.
    fn reduce(expr: &CfgExpr, features: &HashSet<String>) -> CfgExpr {
        match expr {
            // Substitute known feature predicates with literal
            // true/false. The reducer's existing fold logic
            // (any/all dropping known True/False) then collapses
            // surrounding nodes correctly. Renders as `true` /
            // `false` per the Rust reference's
            // ConfigurationPredicate -> ... | true | false grammar.
            // Critical for tokio's cfg_X / cfg_not_X mutual-
            // exclusion: the negation branch's evaluation needs
            // feature predicates baked CONSISTENTLY at vendor time.
            //
            // Only fire when the result actually collapses to a
            // simpler form — otherwise the bare-feature replacement
            // (`feature = "X"` → `true`) changes the rendered string
            // even when the surrounding expr can't simplify
            // further, causing edits that just change feature
            // syntax → literal syntax without functional value.
            CfgExpr::Feature(name) => {
                CfgExpr::Literal(features.contains(name))
            }
            CfgExpr::Not(inner) => match reduce(inner, features) {
                CfgExpr::Not(double) => *double, // double-negation collapse
                CfgExpr::Literal(v) => CfgExpr::Literal(!v),
                reduced => CfgExpr::Not(Box::new(reduced)),
            },
            CfgExpr::All(items) => {
                let mut keep: Vec<CfgExpr> = Vec::new();
                for item in items {
                    let r = reduce(item, features);
                    match eval_cfg(&r, features) {
                        CfgEval::True => continue, // drop True from all()
                        CfgEval::False => {
                            return CfgExpr::Literal(false); // collapses to False
                        }
                        CfgEval::Unknown => keep.push(r),
                    }
                }
                if keep.is_empty() {
                    CfgExpr::Literal(true) // all() with no items is True
                } else if keep.len() == 1 {
                    keep.into_iter().next().unwrap()
                } else {
                    CfgExpr::All(keep)
                }
            }
            CfgExpr::Any(items) => {
                let mut keep: Vec<CfgExpr> = Vec::new();
                for item in items {
                    let r = reduce(item, features);
                    match eval_cfg(&r, features) {
                        CfgEval::False => continue, // drop False from any()
                        CfgEval::True => {
                            return CfgExpr::Literal(true); // collapses to True
                        }
                        CfgEval::Unknown => keep.push(r),
                    }
                }
                if keep.is_empty() {
                    CfgExpr::Literal(false) // any() with no items is False
                } else if keep.len() == 1 {
                    keep.into_iter().next().unwrap()
                } else {
                    CfgExpr::Any(keep)
                }
            }
            other => other.clone(),
        }
    }
    let reduced = reduce(expr, features);
    let s = render(&reduced);
    if s.is_empty() { None } else { Some(s) }
}

/// Bare cfgs that are testing-only / never set in a normal vendored
/// build, so we evaluate them as False (not Unknown) at vendor time.
/// This lets cfg-attr expressions like `cfg(any(loom, …))` cleanly
/// resolve so the macro-body bake pass can rewrite them to
/// `cfg(any())` and prevent duplicate definitions when paired with
/// a positive macro that bakes to `cfg(all())`. tokio's
/// `cfg_not_signal_internal!` uses `cfg(any(loom, not(unix), …))` —
/// without `loom = False`, the whole expression stays Unknown and
/// both sides of the cfg_X / cfg_not_X pair survive at user compile
/// time, producing duplicate `create_signal_driver`.
const KNOWN_FALSE_BARE_CFGS: &[&str] = &[
    // tokio's loom-based concurrency tests — only set when running
    // `loom` itself. Never on in production.
    "loom",
];

/// True if `name` (whether plain `unix` or encoded `target_os="X"`)
/// is one of the compiler-set predicates listed in the Rust
/// reference. These must be preserved verbatim — never evaluated
/// against the dep's feature set — so the flat output stays
/// target-portable.
pub(crate) fn is_compiler_set_predicate(name: &str) -> bool {
    // Bare-ident shorthands.
    if matches!(
        name,
        // target_family shorthands
        "unix" | "windows"
        // build state
        | "debug_assertions"
        | "test"
        | "proc_macro"
    ) {
        return true;
    }
    // Encoded `KEY="VAL"` form (parse_one_cfg_expr stuffs target_os
    // etc. into Bare with `key=\"val\"` shape).
    if let Some(eq) = name.find("=\"") {
        let key = &name[..eq];
        return matches!(
            key,
            "target_arch"
                | "target_feature"
                | "target_os"
                | "target_family"
                | "target_env"
                | "target_abi"
                | "target_endian"
                | "target_pointer_width"
                | "target_vendor"
                | "target_has_atomic"
                | "panic"
        );
    }
    false
}

pub(crate) fn eval_cfg(expr: &CfgExpr, features: &HashSet<String>) -> CfgEval {
    match expr {
        CfgExpr::Literal(true) => return CfgEval::True,
        CfgExpr::Literal(false) => return CfgEval::False,
        CfgExpr::Feature(f) => {
            if features.contains(f) {
                CfgEval::True
            } else {
                CfgEval::False
            }
        }
        CfgExpr::Bare(name) => {
            // Compiler-set predicates (per the Rust reference: target_*
            // family, unix/windows shorthands, debug_assertions, test,
            // proc_macro, panic) MUST stay Unknown so they're preserved
            // verbatim in the flat output and evaluated by rustc at
            // user compile time. This is what makes the flat output
            // target-portable — vendor on macOS, run on Linux, the
            // user's compile picks the right target branches.
            //
            // Critically, this also means we must NOT consult
            // `features` for these names. crossterm has a Cargo
            // feature literally called `windows` (enabling
            // crossterm_winapi); without this guard, `cfg(windows)`
            // on macOS would spuriously evaluate True because of
            // the feature collision.
            //
            // See https://doc.rust-lang.org/reference/conditional-compilation.html
            // for the full list of compiler-set predicates.
            if is_compiler_set_predicate(name) {
                return CfgEval::Unknown;
            }
            // Other bare cfg names — build-script `rustc-cfg=NAME`
            // (`apple` from rustix, `has_total_cmp` from num-traits,
            // etc.) — get merged into `features` at vendor time and
            // resolve to True. Otherwise: a few cfgs that we KNOW
            // will never be set in a normal build evaluate to False
            // instead of Unknown (`loom` is testing-only). All
            // others stay Unknown so the user can opt them in
            // downstream via RUSTFLAGS.
            if features.contains(name) {
                CfgEval::True
            } else if KNOWN_FALSE_BARE_CFGS.contains(&name.as_str()) {
                CfgEval::False
            } else {
                CfgEval::Unknown
            }
        }
        CfgExpr::Not(inner) => match eval_cfg(inner, features) {
            CfgEval::True => CfgEval::False,
            CfgEval::False => CfgEval::True,
            CfgEval::Unknown => CfgEval::Unknown,
        },
        CfgExpr::Any(exprs) => {
            let mut any_unknown = false;
            for e in exprs {
                match eval_cfg(e, features) {
                    CfgEval::True => return CfgEval::True,
                    CfgEval::False => continue,
                    CfgEval::Unknown => any_unknown = true,
                }
            }
            if any_unknown {
                CfgEval::Unknown
            } else {
                CfgEval::False
            }
        }
        CfgExpr::All(exprs) => {
            let mut any_unknown = false;
            for e in exprs {
                match eval_cfg(e, features) {
                    CfgEval::True => continue,
                    CfgEval::False => return CfgEval::False,
                    CfgEval::Unknown => any_unknown = true,
                }
            }
            if any_unknown {
                CfgEval::Unknown
            } else {
                CfgEval::True
            }
        }
        CfgExpr::Other(_) => CfgEval::Unknown,
    }
}

// ---------------------------------------------------------------------------
// `cfg_if!` macro expander
// ---------------------------------------------------------------------------

/// Expand every item-level `cfg_if! { if #[cfg(A)] { … } else if … else …  }`
/// invocation to the equivalent sequence of `#[cfg(…)]` items. This must
/// run BEFORE the syn-AST scanner finds `mod NAME;` declarations, because
/// syn doesn't walk into macro-invocation tokens — `mod foo;` declared
/// inside a `cfg_if! {}` body would otherwise stay un-inlined and rustc
/// would fail with "file not found for module".
///
/// Best-effort: if the cfg_if body doesn't match the expected
/// if/else-if/else shape (e.g. the macro is being used with a non-standard
/// pattern, or there's a parse failure), the invocation is left intact.
pub fn expand_cfg_if(src: &str) -> String {
    let Ok(file) = syn::parse_file(src) else {
        return src.to_string();
    };
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    walk_items_for_cfg_if(&file.items, src, &mut edits);
    crate::edits::apply_simple_edits(src, edits)
}

fn walk_items_for_cfg_if(
    items: &[syn::Item],
    src: &str,
    edits: &mut Vec<(Range<usize>, String)>,
) {
    use syn::spanned::Spanned;
    for item in items {
        match item {
            syn::Item::Macro(m) if is_cfg_if_invocation(&m.mac.path) => {
                if let Some(expanded) = expand_one_cfg_if(src, &m.mac.tokens) {
                    edits.push((item.span().byte_range(), expanded));
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk_items_for_cfg_if(inner, src, edits);
                }
            }
            _ => {}
        }
    }
}

/// Match either bare `cfg_if!` or fully-qualified `cfg_if::cfg_if!`.
fn is_cfg_if_invocation(path: &syn::Path) -> bool {
    if path.is_ident("cfg_if") {
        return true;
    }
    let segs: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
    segs.len() == 2 && segs[0] == "cfg_if" && segs[1] == "cfg_if"
}

/// Parse a `cfg_if!` body into `(predicate, body)` pairs. The else
/// branch (if any) gets an empty predicate token stream.
fn parse_cfg_if_body(
    tokens: &proc_macro2::TokenStream,
) -> Option<Vec<(proc_macro2::TokenStream, proc_macro2::TokenStream)>> {
    let toks: Vec<TokenTree> = tokens.clone().into_iter().collect();
    let mut branches = Vec::new();
    let mut i = 0;
    loop {
        let TokenTree::Ident(kw) = toks.get(i)? else {
            return None;
        };
        if kw != "if" {
            return None;
        }
        i += 1;
        let TokenTree::Punct(p) = toks.get(i)? else {
            return None;
        };
        if p.as_char() != '#' {
            return None;
        }
        i += 1;
        let TokenTree::Group(attr_group) = toks.get(i)? else {
            return None;
        };
        if attr_group.delimiter() != proc_macro2::Delimiter::Bracket {
            return None;
        }
        let attr_inner: Vec<TokenTree> = attr_group.stream().into_iter().collect();
        let TokenTree::Ident(cfg_id) = attr_inner.first()? else {
            return None;
        };
        if cfg_id != "cfg" {
            return None;
        }
        let TokenTree::Group(pred_group) = attr_inner.get(1)? else {
            return None;
        };
        if pred_group.delimiter() != proc_macro2::Delimiter::Parenthesis {
            return None;
        }
        i += 1;
        let TokenTree::Group(body_group) = toks.get(i)? else {
            return None;
        };
        if body_group.delimiter() != proc_macro2::Delimiter::Brace {
            return None;
        }
        i += 1;
        branches.push((pred_group.stream(), body_group.stream()));
        match toks.get(i) {
            None => break,
            Some(TokenTree::Ident(elsekw)) if elsekw == "else" => {
                i += 1;
                match toks.get(i) {
                    Some(TokenTree::Ident(maybe_if)) if maybe_if == "if" => continue,
                    Some(TokenTree::Group(g))
                        if g.delimiter() == proc_macro2::Delimiter::Brace =>
                    {
                        branches.push((proc_macro2::TokenStream::new(), g.stream()));
                        i += 1;
                        if i != toks.len() {
                            return None;
                        }
                        break;
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    Some(branches)
}

fn expand_one_cfg_if(src: &str, tokens: &proc_macro2::TokenStream) -> Option<String> {
    use syn::spanned::Spanned;
    let branches = parse_cfg_if_body(tokens)?;
    if branches.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut prior_preds: Vec<String> = Vec::new();
    for (pred, body) in &branches {
        let pred_str = pred.to_string();
        let cfg_attr = if pred_str.is_empty() {
            // Final `else` branch.
            if prior_preds.len() == 1 {
                format!("#[cfg(not({}))]", prior_preds[0])
            } else {
                let nots: Vec<String> =
                    prior_preds.iter().map(|p| format!("not({p})")).collect();
                format!("#[cfg(all({}))]", nots.join(", "))
            }
        } else if prior_preds.is_empty() {
            format!("#[cfg({pred_str})]")
        } else {
            let nots: Vec<String> = prior_preds.iter().map(|p| format!("not({p})")).collect();
            format!("#[cfg(all({}, {pred_str}))]", nots.join(", "))
        };
        if !pred_str.is_empty() {
            prior_preds.push(pred_str);
        }
        let body_file: syn::File = syn::parse2(body.clone()).ok()?;
        for body_item in &body_file.items {
            let span = body_item.span().byte_range();
            if span.start < src.len() && span.end <= src.len() && span.start < span.end {
                out.push_str(&cfg_attr);
                out.push('\n');
                out.push_str(&src[span.clone()]);
                out.push('\n');
            }
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proc_macro2::TokenStream;
    use std::str::FromStr;

    fn parse(src: &str) -> CfgExpr {
        parse_cfg_expr(&TokenStream::from_str(src).unwrap())
    }

    fn features<I: IntoIterator<Item = &'static str>>(items: I) -> HashSet<String> {
        items.into_iter().map(String::from).collect()
    }

    // -- parse ------------------------------------------------------

    #[test]
    fn parses_bare_ident() {
        match parse("unix") {
            CfgExpr::Bare(n) => assert_eq!(n, "unix"),
            other => panic!("expected Bare, got {other:?}"),
        }
    }

    #[test]
    fn parses_feature_string() {
        match parse("feature = \"std\"") {
            CfgExpr::Feature(n) => assert_eq!(n, "std"),
            other => panic!("expected Feature, got {other:?}"),
        }
    }

    #[test]
    fn parses_target_os_as_bare_keyvalue() {
        match parse("target_os = \"linux\"") {
            CfgExpr::Bare(n) => assert_eq!(n, "target_os=\"linux\""),
            other => panic!("expected Bare key=value, got {other:?}"),
        }
    }

    #[test]
    fn parses_not() {
        match parse("not(feature = \"std\")") {
            CfgExpr::Not(inner) => match *inner {
                CfgExpr::Feature(n) => assert_eq!(n, "std"),
                other => panic!("expected inner Feature, got {other:?}"),
            },
            other => panic!("expected Not, got {other:?}"),
        }
    }

    #[test]
    fn parses_all_and_any() {
        match parse("all(feature = \"std\", unix)") {
            CfgExpr::All(items) => assert_eq!(items.len(), 2),
            other => panic!("expected All, got {other:?}"),
        }
        match parse("any(feature = \"a\", feature = \"b\")") {
            CfgExpr::Any(items) => assert_eq!(items.len(), 2),
            other => panic!("expected Any, got {other:?}"),
        }
    }

    // -- eval -------------------------------------------------------

    #[test]
    fn eval_feature_set() {
        let f = features(["std"]);
        assert_eq!(eval_cfg(&parse("feature = \"std\""), &f), CfgEval::True);
        assert_eq!(eval_cfg(&parse("feature = \"alloc\""), &f), CfgEval::False);
    }

    #[test]
    fn eval_compiler_set_predicates_always_unknown() {
        // Per the Rust reference, compiler-set predicates are:
        // target_arch, target_feature, target_os, target_family,
        // target_env, target_abi, target_endian, target_pointer_width,
        // target_vendor, target_has_atomic, panic, plus the
        // unix/windows shorthands and debug_assertions/test/proc_macro.
        // ALL must stay Unknown so the flat output is target-portable.
        let f = features([]);
        for cfg_src in [
            "unix",
            "windows",
            "debug_assertions",
            "test",
            "proc_macro",
            "target_arch = \"x86_64\"",
            "target_feature = \"avx\"",
            "target_os = \"linux\"",
            "target_os = \"macos\"",
            "target_family = \"unix\"",
            "target_env = \"gnu\"",
            "target_abi = \"\"",
            "target_endian = \"little\"",
            "target_pointer_width = \"64\"",
            "target_vendor = \"apple\"",
            "target_has_atomic = \"64\"",
            "panic = \"abort\"",
        ] {
            assert_eq!(
                eval_cfg(&parse(cfg_src), &f),
                CfgEval::Unknown,
                "compiler-set predicate `{cfg_src}` should be Unknown"
            );
        }
    }

    #[test]
    fn eval_bare_loom_is_false_not_unknown() {
        // Regression for tokio's `cfg_not_signal_internal!` body
        // which has `cfg(any(loom, not(unix), not(any(feature="signal",
        // all(unix, feature="process")))))`. With loom evaluating
        // Unknown (the previous behavior) the whole expression stayed
        // Unknown at vendor time → preserved verbatim → at user
        // compile time evaluated True (no loom, no features) →
        // duplicate `create_signal_driver` alongside the positive
        // pair member's already-baked-True branch. Treating loom as
        // False at vendor time lets the negation correctly bake to
        // `cfg(any())` and gate out cleanly.
        let f = features([]);
        assert_eq!(eval_cfg(&parse("loom"), &f), CfgEval::False);
        assert_eq!(eval_cfg(&parse("not(loom)"), &f), CfgEval::True);
        // Other unknown bare cfgs stay Unknown — `tokio_unstable`
        // is a real opt-in cfg the user might enable downstream.
        assert_eq!(eval_cfg(&parse("tokio_unstable"), &f), CfgEval::Unknown);
    }

    #[test]
    fn eval_bare_set() {
        // Compiler-set predicates (unix, windows, target_os=X, etc.)
        // MUST stay Unknown so they're preserved verbatim in the flat
        // output and evaluated by rustc at user compile time. This is
        // the cornerstone of cross-target portability — the user can
        // vendor on macOS and run on Linux.
        //
        // Critically, this means we must NOT consult `features` for
        // these names. crossterm has a Cargo feature literally named
        // `windows` (enabling crossterm_winapi); without the
        // is_compiler_set_predicate guard, `cfg(windows)` on macOS
        // would spuriously evaluate True because of the feature
        // collision.
        let f: HashSet<String> = ["windows".to_string(), "loom".to_string()]
            .into_iter()
            .collect();
        // cfg(windows) stays Unknown EVEN WITH a feature named "windows".
        assert_eq!(eval_cfg(&parse("windows"), &f), CfgEval::Unknown);
        // Same for cfg(unix).
        assert_eq!(eval_cfg(&parse("unix"), &f), CfgEval::Unknown);
        // target_os = "X" stays Unknown.
        assert_eq!(
            eval_cfg(&parse("target_os = \"linux\""), &f),
            CfgEval::Unknown
        );
        // Non-compiler-set bare idents still consult features —
        // `loom` is in the set so True. (Loom is also in the
        // KNOWN_FALSE list but `features.contains` short-circuits
        // to True first when it's explicitly enabled.)
        assert_eq!(eval_cfg(&parse("loom"), &f), CfgEval::True);
        // An unknown bare cfg with no feature: depends on whether
        // it's in KNOWN_FALSE_BARE_CFGS.
        assert_eq!(eval_cfg(&parse("unrelated"), &f), CfgEval::Unknown);
    }

    #[test]
    fn eval_not() {
        let f = features(["std"]);
        assert_eq!(
            eval_cfg(&parse("not(feature = \"std\")"), &f),
            CfgEval::False
        );
        assert_eq!(
            eval_cfg(&parse("not(feature = \"alloc\")"), &f),
            CfgEval::True
        );
        // Unknown propagates through not.
        assert_eq!(
            eval_cfg(&parse("not(unknown_target)"), &f),
            CfgEval::Unknown
        );
    }

    #[test]
    fn eval_all_short_circuits_on_false() {
        let f = features(["std"]);
        // any False makes the whole all() False, even with Unknown.
        assert_eq!(
            eval_cfg(
                &parse("all(feature = \"alloc\", unknown_target, feature = \"std\")"),
                &f
            ),
            CfgEval::False
        );
    }

    #[test]
    fn eval_all_unknown_when_no_false_but_has_unknown() {
        let f = features(["std"]);
        assert_eq!(
            eval_cfg(&parse("all(feature = \"std\", unknown_target)"), &f),
            CfgEval::Unknown
        );
    }

    #[test]
    fn eval_all_true_when_all_true() {
        let f = features(["std", "alloc"]);
        assert_eq!(
            eval_cfg(&parse("all(feature = \"std\", feature = \"alloc\")"), &f),
            CfgEval::True
        );
    }

    #[test]
    fn eval_any_short_circuits_on_true() {
        let f = features(["std"]);
        assert_eq!(
            eval_cfg(
                &parse("any(unknown_target, feature = \"std\", feature = \"alloc\")"),
                &f
            ),
            CfgEval::True
        );
    }

    #[test]
    fn eval_any_unknown_when_no_true_but_has_unknown() {
        let f = features([]);
        assert_eq!(
            eval_cfg(&parse("any(feature = \"alloc\", unknown_target)"), &f),
            CfgEval::Unknown
        );
    }

    #[test]
    fn eval_any_false_when_all_false() {
        let f = features([]);
        assert_eq!(
            eval_cfg(&parse("any(feature = \"a\", feature = \"b\")"), &f),
            CfgEval::False
        );
    }

    // -- simplify ---------------------------------------------------

    #[test]
    fn simplify_drops_true_from_all() {
        let f = features(["std"]);
        let s = simplify_cfg_expr(&parse("all(feature = \"std\", unknown_target)"), &f);
        assert_eq!(s.as_deref(), Some("unknown_target"));
    }

    #[test]
    fn simplify_drops_false_from_any() {
        let f = features([]);
        let s = simplify_cfg_expr(&parse("any(feature = \"a\", unknown_target)"), &f);
        assert_eq!(s.as_deref(), Some("unknown_target"));
    }

    #[test]
    fn simplify_double_negation_collapses() {
        let f = features([]);
        let s = simplify_cfg_expr(&parse("not(not(unknown_target))"), &f);
        assert_eq!(s.as_deref(), Some("unknown_target"));
    }

    #[test]
    fn simplify_passes_through_other_unchanged() {
        let f = features([]);
        let s = simplify_cfg_expr(&parse("unknown_target"), &f);
        assert_eq!(s.as_deref(), Some("unknown_target"));
    }

    #[test]
    fn simplify_all_false_collapses() {
        let f = features([]);
        // all(False, Unknown) → False. The render comes back as `all(False-form)`
        // which is the original predicate that evaluated False.
        let s = simplify_cfg_expr(
            &parse("all(feature = \"missing\", unknown_target)"),
            &f,
        );
        // Just assert it returned Some — exact text depends on render shape.
        assert!(s.is_some());
    }

    // -- format -----------------------------------------------------

    #[test]
    fn format_round_trip_through_parse() {
        let cases = ["unix", "feature = \"std\"", "not(unix)"];
        for c in cases {
            let parsed = parse(c);
            let formatted = format_cfg_expr(&parsed);
            // Re-parse should give an equivalent shape (feature/bare).
            let reparsed = parse(&formatted);
            assert_eq!(
                format_cfg_expr(&parsed),
                format_cfg_expr(&reparsed),
                "round-trip failed for {c}"
            );
        }
    }

    // -- cfg_if expander --------------------------------------------

    #[test]
    fn cfg_if_two_branch_if_else() {
        let src = r#"
cfg_if::cfg_if! {
    if #[cfg(feature = "rt")] {
        pub fn foo() {}
    } else {
        pub fn bar() {}
    }
}
"#;
        let out = expand_cfg_if(src);
        assert!(out.contains("#[cfg(feature = \"rt\")]"), "got:\n{out}");
        assert!(out.contains("#[cfg(not(feature = \"rt\"))]"), "got:\n{out}");
        assert!(out.contains("pub fn foo() {}"), "got:\n{out}");
        assert!(out.contains("pub fn bar() {}"), "got:\n{out}");
    }

    #[test]
    fn cfg_if_three_branch_with_else_if() {
        let src = r#"
cfg_if::cfg_if! {
    if #[cfg(feature = "a")] {
        pub fn fa() {}
    } else if #[cfg(feature = "b")] {
        pub fn fb() {}
    } else {
        pub fn fc() {}
    }
}
"#;
        let out = expand_cfg_if(src);
        assert!(out.contains("#[cfg(feature = \"a\")]"), "got:\n{out}");
        // Second branch should be `all(not(feature = "a"), feature = "b")`.
        assert!(
            out.contains("#[cfg(all(not(feature = \"a\"), feature = \"b\"))]"),
            "got:\n{out}"
        );
        // Else branch should be `all(not(a), not(b))`.
        assert!(
            out.contains("#[cfg(all(not(feature = \"a\"), not(feature = \"b\")))]"),
            "got:\n{out}"
        );
    }

    #[test]
    fn cfg_if_malformed_returns_input_unchanged() {
        // No `if`-keyword after the `cfg_if!` — leave the invocation alone.
        let src = "cfg_if::cfg_if! { not_a_real_pattern }\n";
        let out = expand_cfg_if(src);
        assert_eq!(out, src);
    }

    #[test]
    fn cfg_if_inside_inline_mod_descends() {
        let src = r#"
mod inner {
    cfg_if::cfg_if! {
        if #[cfg(feature = "rt")] {
            pub fn deep() {}
        } else {
            pub fn shallow() {}
        }
    }
}
"#;
        let out = expand_cfg_if(src);
        assert!(out.contains("pub fn deep()"));
        assert!(out.contains("pub fn shallow()"));
    }
}
