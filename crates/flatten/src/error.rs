//! Typed errors for flatten, with miette-friendly diagnostics where the
//! relevant source span is known.

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, FlattenError>;

#[derive(Debug, Error, Diagnostic)]
pub enum FlattenError {
    /// `syn::parse_file` rejected the input.
    #[error("Parse error: {message}")]
    #[diagnostic(code(flatten::parse_error))]
    ParseError {
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        span: SourceSpan,
        message: String,
    },

    /// Couldn't find the file backing a `mod NAME;` declaration.
    #[error("`mod {name}` not found in `{search_dir}`")]
    #[diagnostic(code(flatten::mod_not_found))]
    ModNotFound {
        name: String,
        search_dir: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("declared here")]
        span: SourceSpan,
        #[help]
        help: Option<String>,
    },

    /// Both `<name>.rs` and `<name>/mod.rs` exist — Rust forbids this.
    #[error("Ambiguous `mod {name}`: both `{foo_rs}` and `{foo_mod}` exist")]
    #[diagnostic(
        code(flatten::ambiguous_mod),
        help("delete one — Rust requires exactly one of `<name>.rs` or `<name>/mod.rs`")
    )]
    AmbiguousMod {
        name: String,
        foo_rs: String,
        foo_mod: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("ambiguous declaration")]
        span: SourceSpan,
    },

    /// `#[path = "..."]` resolved to a file that doesn't exist.
    #[error("`#[path = \"{rel}\"]` on `mod {name}` resolved to non-existent file `{resolved}`")]
    #[diagnostic(code(flatten::path_attr_missing))]
    PathAttrMissing {
        name: String,
        rel: String,
        resolved: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("here")]
        span: SourceSpan,
    },

    /// I/O failure with a path-context prefix.
    #[error("{context}")]
    #[diagnostic(code(flatten::io))]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// Catch-all for messages that don't carry a useful source span:
    /// path validation, manifest parsing, target selection, etc.
    #[error("{message}")]
    #[diagnostic(code(flatten::other))]
    Other { message: String },
}

impl FlattenError {
    /// Convenience constructor for the catch-all variant.
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other {
            message: message.into(),
        }
    }
}

/// Internal: resolution failure modes raised by the file lookup, with no
/// source-span info attached. Callers convert into [`FlattenError`] by
/// supplying the parent source string and span.
#[derive(Debug)]
pub(crate) enum ResolveErr {
    NotFound {
        name: String,
        search_dir: String,
    },
    Ambiguous {
        name: String,
        foo_rs: String,
        foo_mod: String,
    },
    PathAttrMissing {
        name: String,
        rel: String,
        resolved: String,
    },
}
