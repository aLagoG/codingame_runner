//! Find external `mod foo;` declarations in a Rust source file via syn.
//!
//! The scanner produces [`ModDecl`]s annotated with byte ranges (so the
//! splicing layer can rewrite the original source) and the chain of inline
//! `mod` blocks the declaration is nested inside.

use std::ops::Range;
use syn::{Attribute, Expr, File, Item, Lit, Meta};
use proc_macro2::TokenTree;

/// One external `mod foo;` declaration discovered in a source file.
#[derive(Debug, Clone)]
pub(crate) struct ModDecl {
    /// The module's identifier (e.g. `foo` for `mod foo;`).
    pub name: String,
    /// Byte range of the trailing `;`, used for splicing.
    pub semi_range: Range<usize>,
    /// Byte range covering `mod NAME;` (without preceding attributes), used
    /// for diagnostic labels pointing at the declaration.
    pub item_range: Range<usize>,
    /// Candidate paths from `#[path = "..."]` and any
    /// `#[cfg_attr(PRED, path = "...")]` attributes, in attribute
    /// order. `pred` is None for plain `#[path]`, `Some(tokens)` for
    /// `#[cfg_attr(PRED, path = ...)]`. Multiple candidates list
    /// per-platform alternatives; the resolver emits ONE cfg-gated
    /// `mod NAME { contents }` block per existing candidate so the
    /// flat output is target-portable. Empty for declarations with
    /// no path attrs.
    pub path_attrs: Vec<(Option<proc_macro2::TokenStream>, String)>,
    /// Byte range covering all preceding `#[cfg_attr(_, path = ...)]`
    /// attributes attached to this `mod NAME;` declaration. Used to
    /// replace the entire `[#[cfg_attr(...)]]* mod NAME;` sequence
    /// with multi-cfg-mod splice. None if there are no path-form
    /// attributes (i.e. `path_attrs` is empty).
    pub path_attrs_range: Option<Range<usize>>,
    /// True if the declaration carries any `#[cfg]` or `#[cfg_attr]`. When
    /// the resolved file is missing for such a mod we warn and skip rather
    /// than failing.
    pub has_cfg: bool,
    /// Names of inline `mod` blocks containing this declaration, outermost
    /// first. Empty when the declaration is at the file's top level.
    pub inline_path: Vec<String>,
    /// Visibility prefix on the `mod NAME;` declaration as Rust
    /// source text — `pub`, `pub(crate)`, `pub(super)`, `pub(in
    /// path)`, or empty for inherited visibility. Captured so the
    /// multi-cfg-mod splice can apply the same visibility to each
    /// cfg-gated `mod NAME { ... }` block it emits. Without this,
    /// rustix's `pub(crate) mod c;` would lose its `pub(crate)`
    /// when the declaration is replaced with our cfg-gated blocks.
    pub vis: String,
}

/// Walk a Rust source string and return every external `mod NAME;` it
/// contains, including those nested inside inline `mod NAME { ... }` blocks.
/// Returns the underlying [`syn::Error`] on parse failure so the caller can
/// attach source context for nicer diagnostics.
pub(crate) fn scan_external_mods(src: &str) -> Result<Vec<ModDecl>, syn::Error> {
    let file: File = syn::parse_file(src)?;
    let mut out = Vec::new();
    let mut inline_path = Vec::new();
    walk_items(&file.items, &mut inline_path, &mut out);
    Ok(out)
}

fn walk_items(items: &[Item], inline_path: &mut Vec<String>, out: &mut Vec<ModDecl>) {
    use syn::spanned::Spanned;
    for item in items {
        let Item::Mod(m) = item else { continue };
        if let Some((_, inner)) = &m.content {
            inline_path.push(m.ident.to_string());
            walk_items(inner, inline_path, out);
            inline_path.pop();
        } else if let Some(semi) = &m.semi {
            let semi_range = semi.spans[0].byte_range();
            let item_start = m.mod_token.span.byte_range().start;
            let item_range = item_start..semi_range.end;
            let path_attrs = extract_path_attrs(&m.attrs);
            // Compute the byte range covering ALL path-form attrs
            // (those whose extract returned Some). The splicer needs
            // this to replace the entire `[#[cfg_attr(...)]]* mod
            // NAME;` sequence when emitting multi-cfg-mod blocks.
            let path_attrs_range = if path_attrs.is_empty() {
                None
            } else {
                let mut min_start = usize::MAX;
                let mut max_end = 0;
                for attr in &m.attrs {
                    if !is_path_form_attr(attr) {
                        continue;
                    }
                    let r = attr.span().byte_range();
                    min_start = min_start.min(r.start);
                    max_end = max_end.max(r.end);
                }
                if min_start == usize::MAX {
                    None
                } else {
                    Some(min_start..max_end)
                }
            };
            let vis = render_visibility(&m.vis);
            out.push(ModDecl {
                name: m.ident.to_string(),
                semi_range,
                item_range,
                path_attrs,
                path_attrs_range,
                has_cfg: m.attrs.iter().any(is_cfg_or_cfg_attr),
                inline_path: inline_path.clone(),
                vis,
            });
        }
    }
}

fn is_path_form_attr(attr: &Attribute) -> bool {
    if attr.path().is_ident("path") {
        return matches!(&attr.meta, Meta::NameValue(_));
    }
    if attr.path().is_ident("cfg_attr")
        && let Meta::List(list) = &attr.meta
    {
        return extract_path_from_cfg_attr_tokens(&list.tokens).is_some();
    }
    false
}

fn extract_path_attrs(
    attrs: &[Attribute],
) -> Vec<(Option<proc_macro2::TokenStream>, String)> {
    let mut out = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("path") {
            if let Meta::NameValue(nv) = &attr.meta
                && let Expr::Lit(lit) = &nv.value
                && let Lit::Str(s) = &lit.lit
            {
                out.push((None, s.value()));
            }
        } else if attr.path().is_ident("cfg_attr")
            && let Meta::List(list) = &attr.meta
            && let Some((pred, p)) = extract_path_from_cfg_attr_tokens(&list.tokens)
        {
            // Keep ALL candidates regardless of host evaluation —
            // the splicer will emit one cfg-gated `mod NAME {
            // contents }` block per existing file so the flat
            // output is target-portable. socket2's `mod sys;` with
            // `#[cfg_attr(unix, path = "sys/unix.rs")] #[cfg_attr(
            // windows, path = "sys/windows.rs")]` becomes BOTH
            // unix and windows mods at downstream time.
            out.push((Some(pred), p));
        }
    }
    out
}

/// Walk the token stream inside `cfg_attr(...)`. Returns the cfg PRED
/// (everything before the first top-level comma) and the value of a
/// `path = "P"` segment, if present. Caller can use PRED to filter
/// platform-specific candidates.
fn extract_path_from_cfg_attr_tokens(
    stream: &proc_macro2::TokenStream,
) -> Option<(proc_macro2::TokenStream, String)> {
    let toks: Vec<TokenTree> = stream.clone().into_iter().collect();
    // Split at first top-level comma into PRED, ATTR_BODY.
    let mut pred = proc_macro2::TokenStream::new();
    let mut after: Vec<TokenTree> = Vec::new();
    let mut seen_comma = false;
    for tt in toks {
        if !seen_comma
            && matches!(&tt, TokenTree::Punct(p) if p.as_char() == ',')
        {
            seen_comma = true;
            continue;
        }
        if seen_comma {
            after.push(tt);
        } else {
            pred.extend(std::iter::once(tt));
        }
    }
    for j in 0..after.len().saturating_sub(2) {
        if let TokenTree::Ident(id) = &after[j]
            && id == "path"
            && let Some(TokenTree::Punct(p)) = after.get(j + 1)
            && p.as_char() == '='
            && let Some(TokenTree::Literal(lit)) = after.get(j + 2)
        {
            let raw = lit.to_string();
            return raw
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .map(|p| (pred, p.to_string()));
        }
    }
    None
}

fn is_cfg_or_cfg_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr")
}

/// Render a syn::Visibility as Rust source text. Returns `""` for
/// inherited visibility, `"pub"` / `"pub(crate)"` / `"pub(super)"`
/// / `"pub(in PATH)"` for the explicit forms.
fn render_visibility(vis: &syn::Visibility) -> String {
    match vis {
        syn::Visibility::Inherited => String::new(),
        syn::Visibility::Public(_) => "pub".to_string(),
        syn::Visibility::Restricted(r) => {
            // `pub(crate)`, `pub(super)`, `pub(in path)` — extract
            // the path inside the parens via syn::Path's segments.
            let path: Vec<String> = r
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let path_str = path.join("::");
            if r.in_token.is_some() {
                format!("pub(in {path_str})")
            } else {
                format!("pub({path_str})")
            }
        }
    }
}
