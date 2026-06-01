use proc_macro::TokenStream;

/// Attribute proc-macro: appends a `_marker` const to whatever item it
/// annotates. Used by --expand tests to verify Attr proc-macro inlining.
#[proc_macro_attribute]
pub fn marked(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let s = item.to_string();
    format!("{s}\npub const MARKER: bool = true;").parse().unwrap()
}

/// Item-position Bang macro: turns `make_const!(NAME, value);` into a
/// `pub const NAME: i32 = value;` item. Used by --expand tests to verify
/// Bang proc-macro inlining.
#[proc_macro]
pub fn make_const(input: TokenStream) -> TokenStream {
    let s = input.to_string();
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    let name = parts.first().copied().unwrap_or("UNKNOWN");
    let value = parts.get(1).copied().unwrap_or("0");
    format!("pub const {name}: i32 = {value};").parse().unwrap()
}

#[proc_macro_derive(Hello, attributes(hello))]
pub fn derive_hello(input: TokenStream) -> TokenStream {
    let s = input.to_string();
    let after = s
        .split_once("struct ")
        .or_else(|| s.split_once("enum "))
        .map(|(_, rest)| rest)
        .unwrap_or(&s);
    let name: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let name = if name.is_empty() { "Unknown".to_string() } else { name };
    let out = format!(
        "impl {name} {{ pub fn hello() -> &'static str {{ \"hello from {name}\" }} }}"
    );
    out.parse().unwrap()
}
