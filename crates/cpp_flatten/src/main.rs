use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

/// Recursively inline local `#include "..."` directives into a single
/// translation unit. System (`<...>`) includes are preserved as text;
/// each local include is inlined at most once.
#[derive(Parser)]
#[command(name = "cpp_flatten")]
struct Args {
    /// Entry-point .cpp file to flatten.
    entry: PathBuf,

    /// Write the flattened source here; if omitted, prints to stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let flat = cpp_flatten::flatten(&args.entry)?;
    match args.output {
        Some(path) => {
            std::fs::write(&path, &flat).with_context(|| format!("writing {}", path.display()))?
        }
        None => print!("{flat}"),
    }
    Ok(())
}
