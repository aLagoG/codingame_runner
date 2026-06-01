//! CLI front-end for `cg_statement`. Reads a CodinGame statement
//! paste, applies the cleaner, writes the result. Warnings always
//! go to stderr; `--werror` flips them into errors.

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cg_statement::{CleanOptions, Warning, clean_with_options};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "cg_statement",
    about = "Clean up a copy-pasted CodinGame statement into a dark-themed standalone HTML page."
)]
struct Args {
    /// Read paste from this file. Stdin if omitted.
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// Write cleaned HTML here. Stdout if omitted.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Treat warnings as errors (exit non-zero if any are emitted).
    #[arg(long)]
    werror: bool,
    /// HTML tab title. Defaults to "CodinGame Statement" when omitted —
    /// xtask passes e.g. "Fantastic Bits - Game Statement" so the
    /// browser tab carries the game name.
    #[arg(long)]
    title: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let input = match &args.input {
        Some(p) => fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
        None => {
            let mut s = String::new();
            io::stdin()
                .read_to_string(&mut s)
                .context("reading stdin")?;
            s
        }
    };

    let opts = CleanOptions {
        title: args.title.clone(),
    };
    let result = clean_with_options(&input, &opts)?;

    match &args.output {
        Some(p) => {
            if let Some(parent) = p.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            fs::write(p, &result.html).with_context(|| format!("writing {}", p.display()))?;
        }
        None => {
            io::stdout()
                .write_all(result.html.as_bytes())
                .context("writing stdout")?;
        }
    }

    if !result.warnings.is_empty() {
        eprintln!("cg_statement: {} warning(s):", result.warnings.len());
        for w in &result.warnings {
            print_warning(w);
        }
        if args.werror {
            bail!("warnings present and --werror was set");
        }
    }

    Ok(())
}

fn print_warning(w: &Warning) {
    match w {
        Warning::UnknownInlineStyle { property, value } => {
            eprintln!(
                "  unknown inline style: {property}: {value} (kept; add to rules.rs to silence)"
            );
        }
        Warning::UnknownStatementClass(c) => {
            eprintln!("  unknown statement class: .{c} (kept; bundled CSS may not style it)");
        }
        Warning::NoContentBoundary => {
            eprintln!("  could not find a content boundary; emitting whole input as body");
        }
    }
}
