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
    /// Scaffold a new game (engine + viz + a baseline_rs + baseline_cpp bot).
    NewGame {
        /// Name of the game (snake_case).
        name: String,
    },
    /// Scaffold a new bot for an existing game. Creates
    /// `<game>/bots/<name>_<lang>/` with crate name `<game>_<name>_<lang>`.
    /// Supports `--from-existing <other_bot>` to clone an existing bot
    /// crate (with crate-name substitutions) instead of using the empty
    /// template.
    NewBot {
        /// Game name the bot plays. Must already exist.
        #[arg(long)]
        game: String,
        /// Bot name (snake_case). Becomes the directory + crate suffix.
        #[arg(long)]
        name: String,
        /// Language(s) to scaffold. `both` produces a Rust *and* a C++
        /// crate sharing the bot name.
        #[arg(long, value_enum)]
        lang: BotLang,
        /// Clone the named bot's source instead of using the empty
        /// template. Substitutes the crate name throughout. Useful for
        /// "v2 = tweak of v1" workflows.
        #[arg(long)]
        from_existing: Option<String>,
    },
    /// Bundle a game's C++ bot into a single self-contained `.cpp`
    /// file ready to paste into CodinGame's web editor. Runs the
    /// `cpp_flatten` binary on the bot's `main.cpp` entry.
    Bundle {
        /// Game name (e.g. `tron`).
        game: String,
        /// Bot name (e.g. `baseline`, `v1`). Resolved to
        /// `<game>/bots/<bot>_cpp/main.cpp`.
        bot: String,
        /// Override the output path. Defaults to
        /// `target/codingame/<game>_<bot>_bot.cpp`.
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
        /// `<game>/game/instructions.html` unless `--output`
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
    ///       --bot a=target/release/libtron_baseline_rs.dylib \
    ///       --bot b=target/release/libtron_baseline_cpp.dylib \
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

/// Variables available in game-scaffolding templates (`templates/game/`).
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

/// Variables available in bot-scaffolding templates (`templates/bot/`).
#[derive(Serialize)]
struct BotTemplateVars {
    game: String,
    game_pascal: String,
    bot: String,
    crate_name: String,
}

impl BotTemplateVars {
    fn new(game: &str, bot: &str, lang_suffix: &str) -> Self {
        Self {
            game: game.to_string(),
            game_pascal: to_pascal_case(game),
            bot: bot.to_string(),
            crate_name: format!("{game}_{bot}_{lang_suffix}"),
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum BotLang {
    Rust,
    Cpp,
    Both,
}

impl BotLang {
    /// `_rs` / `_cpp` suffixes; for `Both`, the caller iterates.
    fn variants(self) -> &'static [(&'static str, &'static str)] {
        // (suffix, template subdir)
        match self {
            BotLang::Rust => &[("rs", "bot/rust")],
            BotLang::Cpp => &[("cpp", "bot/cpp")],
            BotLang::Both => &[("rs", "bot/rust"), ("cpp", "bot/cpp")],
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::NewGame { name } => new_game(&name)?,
        Command::NewBot {
            game,
            name,
            lang,
            from_existing,
        } => new_bot(&game, &name, lang, from_existing.as_deref())?,
        Command::Bundle { game, bot, output } => bundle(&game, &bot, output.as_deref())?,
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
        PathBuf::from(game).join("game").join("instructions.html")
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

/// Run `cpp_flatten` over `<game>/bots/<bot>_cpp/main.cpp` and write
/// the result somewhere paste-ready. We shell out to the binary
/// instead of linking the library so xtask stays a thin orchestrator
/// — the flatten logic, its tests, and its CLI all live in one crate.
fn bundle(game: &str, bot: &str, output_override: Option<&Path>) -> Result<()> {
    let entry = PathBuf::from(game)
        .join("bots")
        .join(format!("{bot}_cpp"))
        .join("main.cpp");
    anyhow::ensure!(
        entry.exists(),
        "no C++ bot stdio entry at {} — does the bot have a main.cpp?",
        entry.display(),
    );

    let output: PathBuf = output_override.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("target")
            .join("codingame")
            .join(format!("{game}_{bot}_bot.cpp"))
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
        "{} Bundled {} ({}) → {}",
        s.ok("✓"),
        s.name(game),
        s.name(bot),
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
    // Engine bits: defs / game / viz under `<game>/`.
    render_template("game", name, &vars)?;
    // Baseline bots: <game>/bots/baseline_{rs,cpp}/. Uses the same
    // templates as `new-bot`, so the two paths stay in lockstep.
    let baseline_bot = "baseline";
    for (suffix, tmpl) in BotLang::Both.variants() {
        let bot_vars = BotTemplateVars::new(name, baseline_bot, suffix);
        let dest = format!("{name}/bots/{baseline_bot}_{suffix}");
        render_template(tmpl, &dest, &bot_vars)?;
    }

    // Engine crates + bot crates are members; only `defs` and `game`
    // are surfaced as workspace dependencies (everything else is a leaf).
    // Directory names drop the `<game>_` prefix (the parent dir
    // already namespaces); crate names keep it (they must be globally
    // unique across the workspace).
    for dir in ["defs", "game", "viz"] {
        let crate_path = format!("{name}/{dir}");
        add_workspace_member("Cargo.toml", &crate_path)?;
    }
    for (suffix, _) in BotLang::Both.variants() {
        let crate_path = format!("{name}/bots/{baseline_bot}_{suffix}");
        add_workspace_member("Cargo.toml", &crate_path)?;
    }
    for dir in ["defs", "game"] {
        let crate_name = format!("{name}_{dir}");
        let crate_path = format!("{name}/{dir}");
        add_workspace_dependency("Cargo.toml", &crate_name, &crate_path)?;
    }

    // Wire the `_game` crate into the runner so the manual checklist only
    // needs to cover the `use` import and the dispatch arm.
    add_runner_dep("runner/Cargo.toml", &format!("{name}_game"))?;

    print_next_steps(name, &vars.name_pascal);
    Ok(())
}

fn new_bot(
    game: &str,
    bot: &str,
    lang: BotLang,
    from_existing: Option<&str>,
) -> Result<()> {
    // Game must already exist (defs crate is the canonical marker).
    let game_defs_path = PathBuf::from(game).join(format!("{game}_defs"));
    anyhow::ensure!(
        game_defs_path.exists(),
        "game `{game}` not found (no {})",
        game_defs_path.display(),
    );

    let s = Style::new();
    let mut created: Vec<String> = Vec::new();
    for (suffix, tmpl) in lang.variants() {
        let dest_path = PathBuf::from(game)
            .join("bots")
            .join(format!("{bot}_{suffix}"));
        anyhow::ensure!(
            !dest_path.exists(),
            "bot already exists at {} — pick a different `--name` or delete it first",
            dest_path.display(),
        );
        let dest_str = dest_path.to_string_lossy().to_string();

        if let Some(src_bot) = from_existing {
            // Clone an existing bot crate of the same language. Crate
            // name substitutions throughout.
            let src_path = PathBuf::from(game)
                .join("bots")
                .join(format!("{src_bot}_{suffix}"));
            anyhow::ensure!(
                src_path.exists(),
                "--from-existing source not found at {}",
                src_path.display(),
            );
            clone_bot(&src_path, &dest_path, game, src_bot, bot, suffix)?;
        } else {
            let vars = BotTemplateVars::new(game, bot, suffix);
            render_template(tmpl, &dest_str, &vars)?;
        }
        let crate_name = format!("{game}_{bot}_{suffix}");
        add_workspace_member("Cargo.toml", &dest_str)?;
        created.push(crate_name);
    }

    println!(
        "{} Created bot {} for {} ({}) and updated workspace {}",
        s.ok("✓"),
        s.name(bot),
        s.name(game),
        created.join(", "),
        s.path("Cargo.toml"),
    );
    println!();
    println!("{}", s.heading("Next steps:"));
    println!(
        "  1. Implement {} (and `on_init`, if relevant) in the new crate(s).",
        s.code("decide"),
    );
    println!(
        "  2. Verify with {}.",
        s.code(&format!("cargo build -p {}", created[0])),
    );
    println!(
        "  3. Play a match: {}.",
        s.code(&format!(
            "cargo run -p codingame_runner -- --game {game} \\\n     target/release/lib{}.dylib ...",
            created[0]
        )),
    );
    Ok(())
}

/// Copy `src` → `dst` recursively, rewriting `<game>_<src_bot>_<suffix>`
/// → `<game>_<dst_bot>_<suffix>` in every text file's *content*. Filenames
/// are preserved as-is (none reference the bot name in this layout).
fn clone_bot(
    src: &Path,
    dst: &Path,
    game: &str,
    src_bot: &str,
    dst_bot: &str,
    suffix: &str,
) -> Result<()> {
    let src_crate = format!("{game}_{src_bot}_{suffix}");
    let dst_crate = format!("{game}_{dst_bot}_{suffix}");
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    copy_dir_substituting(src, dst, &src_crate, &dst_crate)?;
    Ok(())
}

fn copy_dir_substituting(
    src: &Path,
    dst: &Path,
    from: &str,
    to: &str,
) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let s_path = entry.path();
        let d_path = dst.join(entry.file_name());
        if s_path.is_dir() {
            fs::create_dir_all(&d_path)
                .with_context(|| format!("creating {}", d_path.display()))?;
            copy_dir_substituting(&s_path, &d_path, from, to)?;
        } else if is_text_file(&s_path) {
            let content = fs::read_to_string(&s_path)
                .with_context(|| format!("reading {}", s_path.display()))?;
            let rewritten = content.replace(from, to);
            fs::write(&d_path, rewritten)
                .with_context(|| format!("writing {}", d_path.display()))?;
        } else {
            fs::copy(&s_path, &d_path)
                .with_context(|| format!("copying {}", s_path.display()))?;
        }
    }
    Ok(())
}

fn is_text_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("rs") | Some("cpp") | Some("h") | Some("hpp") | Some("toml") | Some("md") | Some("txt")
    )
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
        s.path(&format!("{name}/defs/src/lib.rs")),
    );
    println!(
        "     and the matching {} impls.",
        s.code("Display/FromStr/ReadFrom/WriteTo"),
    );
    println!(
        "  2. Implement {} and {} in {}.",
        s.code("Game::input_for"),
        s.code("Game::step"),
        s.path(&format!("{name}/game/src/lib.rs")),
    );
    println!(
        "  3. Implement {} in {}.",
        s.code("decide"),
        s.path(&format!("{name}/bots/baseline_rs/src/lib.rs")),
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
        s.path(&format!("{name}/viz/src/main.rs")),
    );
    println!(
        "  6. (optional) C++ bot starter at {} — build with {} and pass",
        s.path(&format!("{name}/bots/baseline_cpp/bot.cpp")),
        s.code(&format!("cargo build -p {name}_baseline_cpp")),
    );
    println!(
        "     {} to the runner.",
        s.path(&format!("target/debug/lib{name}_baseline_cpp.dylib")),
    );
    println!(
        "  7. Add more bots with {} (or {} to clone an existing one).",
        s.code(&format!(
            "cargo xtask new-bot --game {name} --name <bot_name> --lang both"
        )),
        s.code("--from-existing <other_bot>"),
    );
    println!();
    println!(
        "Run {} to confirm the skeleton compiles.",
        s.code("cargo check --workspace"),
    );
}

/// Renders all `.hbs` files from a template directory into the destination,
/// preserving subdirectory structure and stripping the `.hbs` extension.
/// Generic over the variable struct so callers can use either
/// `TemplateVars` (game scaffolding) or `BotTemplateVars` (bot scaffolding).
fn render_template<V: Serialize>(template_name: &str, dest: &str, vars: &V) -> Result<()> {
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

fn walk_and_render<V: Serialize>(
    hbs: &Handlebars,
    base: &Path,
    current: &Path,
    dest_base: &Path,
    vars: &V,
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
