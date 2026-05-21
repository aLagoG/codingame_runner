use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use handlebars::Handlebars;
use serde::Serialize;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, InlineTable, Item, Table};

/// Minimal ANSI helper for the scaffolder's printed instructions. Respects
/// `NO_COLOR` and falls back to plain text when stdout isn't a terminal
/// (e.g. piped into a file or another process).
struct Style {
    enabled: bool,
}

impl Style {
    fn new() -> Self {
        let enabled = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
        Self { enabled }
    }

    fn paint(&self, s: &str, code: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// File paths.
    fn path(&self, s: &str) -> String {
        self.paint(s, "36") // cyan
    }
    /// Code snippets (identifiers, source lines).
    fn code(&self, s: &str) -> String {
        self.paint(s, "33") // yellow
    }
    /// Names and headings.
    fn name(&self, s: &str) -> String {
        self.paint(s, "1;32") // bold green
    }
    fn heading(&self, s: &str) -> String {
        self.paint(s, "1") // bold
    }
    fn ok(&self, s: &str) -> String {
        self.paint(s, "32") // green
    }
}

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new feature module
    NewGame {
        /// Name of the feature (snake_case)
        name: String,
    },
}

/// Variables available in all templates
#[derive(Serialize)]
struct TemplateVars {
    name: String,
    name_pascal: String,
    name_upper: String,
}

impl TemplateVars {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            name_pascal: to_pascal_case(name),
            name_upper: name.to_uppercase(),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::NewGame { name } => new_game(&name)?,
    }

    Ok(())
}

fn new_game(name: &str) -> Result<()> {
    let vars = TemplateVars::new(name);
    render_template("game", name, &vars)?;

    // All five crates are members; only `_defs` and `_game` are surfaced as
    // workspace dependencies (the `_rs` / `_cpp` cdylibs and the `_viz`
    // binary are leaves).
    for suffix in ["_defs", "_game", "_rs", "_viz", "_cpp"] {
        let crate_path = format!("{name}/{name}{suffix}");
        add_workspace_member("Cargo.toml", &crate_path)?;
    }
    for suffix in ["_defs", "_game"] {
        let crate_name = format!("{name}{suffix}");
        let crate_path = format!("{name}/{crate_name}");
        add_workspace_dependency("Cargo.toml", &crate_name, &crate_path)?;
    }

    // Wire the `_game` crate into the runner so the manual checklist only
    // needs to cover the `use` import and the dispatch arm.
    add_runner_dep("runner/Cargo.toml", &format!("{name}_game"))?;

    print_next_steps(name, &vars.name_pascal);
    Ok(())
}

fn print_next_steps(name: &str, name_pascal: &str) {
    let s = Style::new();
    println!(
        "{} Created game {} in {} (5 crates incl. C++ bot) and updated workspace {}",
        s.ok("✓"),
        s.name(name),
        s.path(&format!("{name}/")),
        s.path("Cargo.toml"),
    );
    println!();
    println!("{}", s.heading("Next steps:"));
    println!(
        "  1. Fill in {} in {}",
        s.code("TurnInput/TurnOutput"),
        s.path(&format!("{name}/{name}_defs/src/lib.rs")),
    );
    println!(
        "     and the matching {} impls.",
        s.code("Display/FromStr/ReadFrom/WriteTo"),
    );
    println!(
        "  2. Implement {} and {} in {}.",
        s.code("Game::input_for"),
        s.code("Game::step"),
        s.path(&format!("{name}/{name}_game/src/lib.rs")),
    );
    println!(
        "  3. Implement {} in {}.",
        s.code("decide"),
        s.path(&format!("{name}/{name}_rs/src/lib.rs")),
    );
    println!(
        "  4. Wire the game into {} ({} dep already added):",
        s.path("runner/src/main.rs"),
        s.path("runner/Cargo.toml"),
    );
    println!(
        "       - add {}",
        s.code(&format!("use {name}_game::{name_pascal}Game;")),
    );
    println!(
        "       - add {} to the dispatch match",
        s.code(&format!(
            "\"{name}\" => run_for_game::<{name_pascal}Game>(args.bots, args.save_replay),"
        )),
    );
    println!(
        "       - update the {} bail message to mention {}",
        s.code("unknown game"),
        s.name(name),
    );
    println!(
        "  5. Customise the visualiser in {}.",
        s.path(&format!("{name}/{name}_viz/src/main.rs")),
    );
    println!(
        "  6. (optional) C++ bot starter at {} — build with {} and pass",
        s.path(&format!("{name}/{name}_cpp/bot.cpp")),
        s.code(&format!("cargo build -p {name}_cpp")),
    );
    println!(
        "     {} to the runner.",
        s.path(&format!("target/debug/lib{name}_cpp.dylib")),
    );
    println!();
    println!(
        "Run {} to confirm the skeleton compiles.",
        s.code("cargo check --workspace"),
    );
}

/// Renders all `.hbs` files from a template directory into the destination,
/// preserving subdirectory structure and stripping the `.hbs` extension.
fn render_template(template_name: &str, dest: &str, vars: &TemplateVars) -> Result<()> {
    let mut hbs = Handlebars::new();
    hbs.set_strict_mode(true);

    let template_dir = templates_dir().join(template_name);
    anyhow::ensure!(
        template_dir.exists(),
        "Template directory not found: {}",
        template_dir.display()
    );

    let dest = PathBuf::from(dest);
    walk_and_render(&hbs, &template_dir, &template_dir, &dest, vars)
}

fn walk_and_render(
    hbs: &Handlebars,
    base: &Path,
    current: &Path,
    dest_base: &Path,
    vars: &TemplateVars,
) -> Result<()> {
    for entry in fs::read_dir(current).context("reading template dir")? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(base)?;

        if path.is_dir() {
            walk_and_render(hbs, base, &path, dest_base, vars)?;
        } else if path.extension().map_or(false, |e| e == "hbs") {
            // Strip .hbs extension for output filename
            let out_name = relative.with_extension("");
            // Also template-expand the filename itself (for {{timestamp}}_foo.rs etc.)
            let out_name_str = out_name.to_string_lossy();
            let rendered_name = hbs
                .render_template(&out_name_str, vars)
                .unwrap_or_else(|_| out_name_str.to_string());

            let out_path = dest_base.join(&rendered_name);
            fs::create_dir_all(out_path.parent().unwrap())?;

            let template_content = fs::read_to_string(&path)?;
            let rendered = hbs
                .render_template(&template_content, vars)
                .with_context(|| format!("rendering {}", path.display()))?;

            anyhow::ensure!(
                !out_path.exists(),
                "File already exists: {} (refusing to overwrite)",
                out_path.display()
            );
            fs::write(&out_path, rendered)?;
        }
    }
    Ok(())
}

fn templates_dir() -> PathBuf {
    // xtask binary is run from workspace root, templates are relative to xtask/
    PathBuf::from("xtask/templates")
}

fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect()
}

/// Add a crate to [workspace.members]
fn add_workspace_member(workspace_toml: &str, member_path: &str) -> Result<()> {
    let content = fs::read_to_string(workspace_toml).context("reading workspace Cargo.toml")?;
    let mut doc = content
        .parse::<DocumentMut>()
        .context("parsing workspace Cargo.toml")?;

    let members = doc["workspace"]["members"]
        .as_array_mut()
        .context("[workspace.members] is not an array")?;

    // Don't add duplicates
    let already_exists = members.iter().any(|v| v.as_str() == Some(member_path));
    if !already_exists {
        // Match the existing multi-line `members = [\n    "x",\n    "y",\n]` shape.
        let mut v = toml_edit::Value::from(member_path);
        v.decor_mut().set_prefix("\n    ");
        members.push_formatted(v);
    }

    fs::write(workspace_toml, doc.to_string())?;
    Ok(())
}

/// Add a dependency to [workspace.dependencies]
fn add_workspace_dependency(workspace_toml: &str, crate_name: &str, path: &str) -> Result<()> {
    let content = fs::read_to_string(workspace_toml)?;
    let mut doc = content.parse::<DocumentMut>()?;

    // Ensure [workspace.dependencies] exists
    let ws = doc["workspace"]
        .as_table_mut()
        .context("[workspace] not found")?;
    if !ws.contains_key("dependencies") {
        ws.insert("dependencies", Item::Table(Table::new()));
    }
    let deps = ws["dependencies"]
        .as_table_mut()
        .context("[workspace.dependencies] is not a table")?;

    // Add { path = "..." } inline table
    if !deps.contains_key(crate_name) {
        let mut dep = InlineTable::new();
        dep.insert("path", path.into());
        deps.insert(crate_name, toml_edit::value(dep));
    }

    fs::write(workspace_toml, doc.to_string())?;
    Ok(())
}

/// Add `<crate_name>.workspace = true` (dotted-key form) to the runner's
/// `[dependencies]`. Matches the style of the other entries in that file.
fn add_runner_dep(runner_toml: &str, crate_name: &str) -> Result<()> {
    let content = fs::read_to_string(runner_toml).context("reading runner Cargo.toml")?;
    let mut doc = content
        .parse::<DocumentMut>()
        .context("parsing runner Cargo.toml")?;

    let deps = doc["dependencies"]
        .as_table_mut()
        .context("[dependencies] not found in runner/Cargo.toml")?;

    if !deps.contains_key(crate_name) {
        // `set_dotted(true)` on the inner Table tells toml_edit to render it
        // as `name.workspace = true` rather than `[dependencies.name]\nworkspace = true`.
        let mut inner = Table::new();
        inner.set_dotted(true);
        inner.insert("workspace", Item::Value(true.into()));
        deps.insert(crate_name, Item::Table(inner));
    }

    fs::write(runner_toml, doc.to_string())?;
    Ok(())
}
