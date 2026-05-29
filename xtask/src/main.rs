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
    /// Bundle a game's C++ bot into a single self-contained `.cpp`
    /// file ready to paste into CodinGame's web editor. Runs the
    /// `cpp_flatten` binary on the game's `_cpp/main.cpp` entry.
    Bundle {
        /// Game name (e.g. `tron`, `tictactoe`). Resolved to the
        /// `<game>/<game>_cpp/main.cpp` entry point.
        game: String,
        /// Override the output path. Defaults to
        /// `target/codingame/<game>_bot.cpp`.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Convert a copy-pasted CodinGame statement (devtools HTML
    /// blob) into the dark-themed `instructions.html` next to the
    /// given game crate. Shells out to the `cg_statement` binary.
    ///
    /// Input comes from one of:
    ///   * `--input <file>` — read the paste from disk
    ///   * `--clipboard`     — pull from the system clipboard
    ///     (pbpaste / xclip / Get-Clipboard)
    ///   * otherwise         — stdin. When stdin is a TTY, the
    ///     command prints a prompt and waits
    ///     for Ctrl-D to terminate the input.
    Statement {
        /// Game name (e.g. `tron`, `tictactoe`). The output goes to
        /// `<game>/<game>_game/instructions.html` unless `--output`
        /// is set.
        game: String,
        /// Read paste from this file instead of stdin.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// Override the output path.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Read paste from the system clipboard.
        #[arg(short, long)]
        clipboard: bool,
    },
    /// Profile a tournament run with `samply`. Builds the workspace
    /// in release mode (so symbols and optimizations match what
    /// you'd actually deploy), records a profile, then opens it in
    /// the Firefox-profiler view via samply's built-in local server.
    ///
    /// Anything after `--` is forwarded verbatim to
    /// `tournament run`, so a typical invocation looks like:
    ///
    ///   cargo xtask profile -- --game tron \
    ///       --bot a=target/release/libtron_rs.dylib \
    ///       --bot b=target/release/libtron_cpp.dylib \
    ///       --rounds 2000 --parallel 1 \
    ///       --output /tmp/profile_run.jsonl
    Profile {
        /// Skip opening the UI (just record + save). Useful in CI.
        #[arg(long)]
        no_open: bool,
        /// Override the output path for the recorded profile.
        /// Defaults to `target/samply/profile.json.gz`.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Forwarded to `tournament run`. Use `--` to separate
        /// from xtask's own flags, e.g.
        /// `cargo xtask profile -- --game tron --bot a=… …`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        tournament_args: Vec<String>,
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
        Command::Bundle { game, output } => bundle(&game, output.as_deref())?,
        Command::Statement {
            game,
            input,
            output,
            clipboard,
        } => statement(&game, input.as_deref(), output.as_deref(), clipboard)?,
        Command::Profile {
            no_open,
            output,
            tournament_args,
        } => profile(no_open, output.as_deref(), &tournament_args)?,
    }

    Ok(())
}

/// Pipe a CodinGame statement paste through `cg_statement` and
/// write the result next to the named game crate. Input source
/// priority: explicit `--input` file > `--clipboard` > stdin.
/// Convert a snake/kebab-case game name into a human-readable title
/// for the HTML `<title>` element: `"fantastic_bits"` → `"Fantastic Bits"`.
fn title_case_game(game: &str) -> String {
    game.split(['_', '-'])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn statement(
    game: &str,
    input_path: Option<&Path>,
    output_override: Option<&Path>,
    clipboard: bool,
) -> Result<()> {
    use std::io::{IsTerminal, Read, Write};

    let s = Style::new();

    // Resolve the output path. Default lives next to the game
    // crate so it's easy to find from the source tree.
    let output: PathBuf = output_override.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from(game)
            .join(format!("{game}_game"))
            .join("instructions.html")
    });
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    // Read the paste from the chosen source.
    let paste = if let Some(p) = input_path {
        fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
    } else if clipboard {
        read_clipboard()?
    } else {
        // Stdin. If we're attached to a terminal, the user is
        // pasting interactively — show them how to end the input.
        if std::io::stdin().is_terminal() {
            eprintln!(
                "{} Paste your HTML, then press {} when done:",
                s.heading("→"),
                s.code(if cfg!(windows) {
                    "Ctrl-Z, Enter"
                } else {
                    "Ctrl-D"
                }),
            );
        }
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
        buf
    };

    if paste.trim().is_empty() {
        anyhow::bail!("empty paste — nothing to clean");
    }

    // Shell out to `cg_statement` (matches the bundle → cpp_flatten
    // pattern). Pipe the paste through its stdin; let it write the
    // file directly via --output so we don't have to round-trip the
    // (potentially large) cleaned HTML through this process.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let title = format!("{} - Game Statement", title_case_game(game));
    let mut child = std::process::Command::new(cargo)
        .args(["run", "--quiet", "-p", "cg_statement", "--"])
        .args(["--title", &title])
        .args(["--output"])
        .arg(&output)
        .stdin(std::process::Stdio::piped())
        // Inherit stderr so cg_statement's warnings reach the user
        // verbatim, and stdout (which it won't use since --output
        // is set) is fine to inherit too.
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .spawn()
        .context("spawning cg_statement")?;
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(paste.as_bytes())
            .context("writing paste to cg_statement stdin")?;
        // Dropping `stdin` here closes the pipe so cg_statement
        // sees EOF and starts processing.
    }
    let status = child.wait().context("waiting on cg_statement")?;
    anyhow::ensure!(status.success(), "cg_statement exited with {status}");

    println!(
        "{} Wrote {}",
        s.ok("✓"),
        s.path(&output.display().to_string()),
    );
    Ok(())
}

/// Pull the current clipboard contents using whatever the platform's
/// CLI tool is. We shell out rather than take a `clipboard` crate
/// dep — it's one command per OS and avoids dragging in a new
/// runtime dependency.
fn read_clipboard() -> Result<String> {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbpaste", &[])
    } else if cfg!(target_os = "windows") {
        ("powershell", &["-NoProfile", "-Command", "Get-Clipboard"])
    } else {
        // Linux/BSD: prefer wl-paste if it exists (Wayland), else xclip.
        if std::process::Command::new("wl-paste")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            ("wl-paste", &[])
        } else {
            ("xclip", &["-selection", "clipboard", "-o"])
        }
    };
    let out = std::process::Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running `{cmd}` to read clipboard"))?;
    anyhow::ensure!(
        out.status.success(),
        "{cmd} exited with {} — is it installed and is the clipboard reachable?",
        out.status,
    );
    String::from_utf8(out.stdout).context("clipboard contents weren't valid UTF-8")
}

/// Build a release tournament binary, then drive it with `samply
/// record`. samply's local server opens the Firefox-profiler view
/// for the recorded trace unless `--no-open` was passed.
fn profile(
    no_open: bool,
    output_override: Option<&Path>,
    tournament_args: &[String],
) -> Result<()> {
    let s = Style::new();

    // 1. Verify samply is on PATH; point the user at the install
    //    command if not. `which`-style probe via running the binary
    //    with --version.
    let samply_ok = std::process::Command::new("samply")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    anyhow::ensure!(
        samply_ok,
        "samply not found on PATH — install with `cargo install samply` and try again",
    );

    // 2. Build the workspace in release. With the
    //    `[profile.release] debug = "line-tables-only"` setting in
    //    the top-level Cargo.toml, both Rust and (via cc-rs) C++
    //    end up with line-table symbols, which is what makes the
    //    profile actually navigable.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    println!(
        "{} Building workspace in release (with line-tables-only debug info)…",
        s.ok("→"),
    );
    let status = std::process::Command::new(&cargo)
        .args(["build", "--release", "--workspace"])
        .status()
        .context("running cargo build")?;
    anyhow::ensure!(status.success(), "cargo build failed");

    // 3. Resolve paths. The release tournament binary is the
    //    target we'll profile; the user supplied the rest of the
    //    args (game, bots, etc.).
    let target_dir =
        PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string()));
    let tournament_bin = target_dir.join("release").join("tournament");
    anyhow::ensure!(
        tournament_bin.exists(),
        "expected {} after build — was the build profile changed?",
        tournament_bin.display(),
    );

    let output: PathBuf = output_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_dir.join("samply").join("profile.json.gz"));
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    // 4. Spawn samply. With `--save-only` set we just record and
    //    exit; without it samply boots its local server and (by
    //    default) opens the UI in a browser tab.
    let mut cmd = std::process::Command::new("samply");
    cmd.arg("record");
    if no_open {
        cmd.args(["--save-only", "--no-open"]);
    }
    cmd.arg("--output").arg(&output);
    cmd.arg("--");
    cmd.arg(&tournament_bin);
    cmd.arg("run");
    cmd.args(tournament_args);

    println!(
        "{} samply record → {}",
        s.ok("→"),
        s.path(&output.display().to_string()),
    );
    let status = cmd.status().context("running samply")?;
    anyhow::ensure!(status.success(), "samply exited with {status}");

    if no_open {
        println!(
            "{} Profile saved to {}. Open later with {}.",
            s.ok("✓"),
            s.path(&output.display().to_string()),
            s.code(&format!("samply load {}", output.display())),
        );
    }
    Ok(())
}

/// Run `cpp_flatten` over `<game>/<game>_cpp/main.cpp` and write the
/// result somewhere paste-ready. We shell out to the binary instead of
/// linking the library so xtask stays a thin orchestrator — the flatten
/// logic, its tests, and its CLI all live in one crate.
fn bundle(game: &str, output_override: Option<&Path>) -> Result<()> {
    let entry = PathBuf::from(game)
        .join(format!("{game}_cpp"))
        .join("main.cpp");
    anyhow::ensure!(
        entry.exists(),
        "no C++ bot at {} — is `{}` a real game?",
        entry.display(),
        game,
    );

    let output: PathBuf = output_override.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("target")
            .join("codingame")
            .join(format!("{game}_bot.cpp"))
    });
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    // `CARGO` is set whenever xtask is invoked through `cargo xtask`.
    // Fall back to `cargo` on PATH for direct-binary invocations.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(cargo)
        .args(["run", "--quiet", "-p", "cpp_flatten", "--"])
        .arg(&entry)
        .arg("-o")
        .arg(&output)
        .status()
        .context("invoking cpp_flatten binary")?;
    anyhow::ensure!(status.success(), "cpp_flatten exited with {status}");

    let s = Style::new();
    println!(
        "{} Bundled {} → {}",
        s.ok("✓"),
        s.name(game),
        s.path(&output.display().to_string()),
    );
    println!(
        "  Paste the contents of {} into CodinGame's editor.",
        s.path(&output.display().to_string()),
    );
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
        } else if path.extension().is_some_and(|e| e == "hbs") {
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
