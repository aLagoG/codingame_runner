use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use handlebars::Handlebars;
use serde::Serialize;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, InlineTable, Item, Table};

use bot_manifest::{BotManifest, now_rfc3339};

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
    /// Soft warnings / skip notices (non-fatal yellow).
    fn warn(&self, s: &str) -> String {
        self.paint(s, "33") // yellow
    }
}

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// `--game G --name N [--lang L]` cluster shared by every bot-targeting
/// verb (promote, retire, history, compact-history). `lang` is optional
/// because most callers auto-detect from which `<bot>_<lang>` dirs
/// actually exist; `Both` explicitly operates on both variants.
#[derive(clap::Args)]
struct BotTarget {
    /// Game the bot belongs to.
    #[arg(long)]
    game: String,
    /// Bot stem (e.g. `v1`, `baseline`) — without the `_<lang>` suffix.
    #[arg(long)]
    name: String,
    /// Which language variant. Auto-detected when only one exists;
    /// required when both rs and cpp variants exist. Pass `both` to
    /// operate on each language in turn.
    #[arg(long, value_enum)]
    lang: Option<BotLang>,
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
    /// Promote a candidate bot into its parent's slot. Rename the
    /// candidate to the parent's name, rewrite Cargo.toml + namespace
    /// tokens, move the champion bit if the parent had it.
    ///
    /// By default, the old parent is deleted outright. Pass `--archive`
    /// to keep it around under a timestamped name (`<parent>_archived_<ts>_<lang>`)
    /// so you can A/B against it later.
    ///
    /// Siblings (other bots whose `parent` matches this candidate's parent)
    /// are left alone by default. Pass `--cleanup-siblings` to also retire
    /// them — plus any descendants of those siblings, so you don't end
    /// up with orphans pointing at dead crates.
    Promote {
        #[command(flatten)]
        target: BotTarget,
        /// Keep the old parent as `<parent>_archived_<ts>_<lang>`
        /// instead of deleting it. The promoted bot's `parent` field
        /// points at this archived name so the lineage chain is
        /// preserved.
        #[arg(long)]
        archive: bool,
        /// Also retire the candidate's siblings (other bots with the
        /// same `parent`) and all their descendants. Use when you've
        /// branched several candidates off the same baseline and want
        /// to discard the ones that didn't win.
        #[arg(long)]
        cleanup_siblings: bool,
    },
    /// Retire (delete) a bot crate + remove its workspace member entry
    /// + drop its cached build artifacts. Inverse of `new-bot`.
    ///
    /// Refuses to retire a bot that is currently champion or that
    /// any other bot lists as its `parent` (would orphan descendants).
    /// Pass `--force` to skip these safety checks.
    Retire {
        #[command(flatten)]
        target: BotTarget,
        /// Skip the champion + has-children safety checks.
        #[arg(long)]
        force: bool,
    },
    /// Print the current champion bot(s) per (game, lang). Reads
    /// `champion = true` from each `bot.toml` under
    /// `games/<game>/bots/*/`. Useful as a sanity-check for the
    /// state `bundle`/`promote` rely on.
    Champion {
        /// Game name.
        game: String,
        /// Filter to one language. When omitted, lists champions for
        /// every lang that has one.
        #[arg(long, value_enum)]
        lang: Option<BotLang>,
    },
    /// Print a bot's `[[history]]` chronologically — the tournament
    /// outcomes recorded by `tournament compare --record-history`.
    History {
        #[command(flatten)]
        target: BotTarget,
    },
    /// Health-check every bot in a game and flag inconsistencies:
    /// multiple champions per lang, orphan parent refs, history
    /// entries pointing at deleted opponents, workspace members
    /// missing their directory (or vice versa), and bot dirs that
    /// forgot to write a `bot.toml`. Read-only; exit 1 if any
    /// findings, 0 if clean.
    Doctor {
        /// Game name to audit.
        game: String,
    },
    /// Truncate a bot's `[[history]]` to the most recent `--keep-last`
    /// entries. Use when bot.toml grows uncomfortably long after lots
    /// of iteration. Older entries are dropped silently — no undo, so
    /// commit first if you might want them back.
    CompactHistory {
        #[command(flatten)]
        target: BotTarget,
        /// Keep only the most recent K history entries; drop older.
        #[arg(long, default_value_t = 10)]
        keep_last: usize,
    },
    /// Bundle a bot into a single self-contained source file ready to
    /// paste into CodinGame's web editor.
    ///
    /// * C++ bots → `cpp_flatten` on `<game>/bots/<bot>_cpp/main.cpp`.
    /// * Rust bots → `flatten --vendor` on `<game>/bots/<bot>_rs/`
    ///   with the `--bin <crate_name>` selector. Vendoring is always
    ///   on for Rust bots: the bot's own `lib.rs`, workspace deps
    ///   (`tron_defs`, `bot_common`, …), and any non-CodinGame-shipped
    ///   transitive deps are inlined as `mod <crate>` blocks. The
    ///   CodinGame preset (anyhow, serde, …) stays as plain `use`s.
    ///
    /// Language is auto-detected from which bot directory exists. If
    /// both `<bot>_rs/` and `<bot>_cpp/` exist, `--lang` is required.
    Bundle {
        /// Game name (e.g. `tron`).
        game: String,
        /// Bot name (e.g. `baseline`, `v1`). Resolved to
        /// `<game>/bots/<bot>_<lang>/`. When omitted, resolves to the
        /// (game, lang) champion — the bot whose `bot.toml` has
        /// `champion = true`. Errors if no champion exists for the
        /// resolved lang.
        bot: Option<String>,
        /// Force a specific language when both variants exist (or
        /// when both langs have champions in the omit-bot mode).
        #[arg(long, value_enum)]
        lang: Option<BotLang>,
        /// Override the output path. Defaults to
        /// `target/codingame/<game>_<bot>_bot.{rs,cpp}` based on lang.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Rust only: additional dep to keep as a `use foo::…` reference
        /// rather than inlining it. Repeatable. Forwarded to `flatten
        /// --external`. The CodinGame preset is always applied, so this
        /// is only needed for deps outside that preset.
        #[arg(long = "external", value_name = "NAME", action = clap::ArgAction::Append)]
        external: Vec<String>,
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
        /// Game name (e.g. `tron`, `fantastic_bits`). The output goes
        /// to `<game>/game/instructions.html` unless `--output` is set.
        /// Optional only when `--upgrade-all` / `--check-all` is set.
        game: Option<String>,
        /// Read paste from this file instead of stdin.
        #[arg(short, long, conflicts_with_all = ["clipboard", "upgrade", "upgrade_all"])]
        input: Option<PathBuf>,
        /// Override the output path.
        #[arg(short, long, conflicts_with_all = ["upgrade_all", "check_all"])]
        output: Option<PathBuf>,
        /// Read paste from the system clipboard.
        #[arg(short, long, conflicts_with_all = ["input", "upgrade", "upgrade_all"])]
        clipboard: bool,
        /// Re-clean from the saved original at `games/<game>/game/instructions_original.html`.
        /// Combine with `--css-only` to fall back to re-cleaning the
        /// existing `instructions.html` when no saved original exists.
        #[arg(long, conflicts_with_all = ["input", "clipboard", "upgrade_all"])]
        upgrade: bool,
        /// Re-clean every game with a saved original.
        /// `<game>` is ignored. Per-file warning headers are printed.
        #[arg(long, conflicts_with_all = ["upgrade", "input", "clipboard", "output"])]
        upgrade_all: bool,
        /// Dry run — compare what would be written to what's on disk;
        /// exit non-zero on any mismatch. Pair with `--upgrade`,
        /// `--upgrade-all`, or any explicit source flag.
        #[arg(long)]
        check: bool,
        /// Check every game's `instructions.html` against what the
        /// current cleaner would produce (with `--css-only` fallback
        /// for games without a saved original). Exits non-zero on any
        /// drift. `<game>` is ignored.
        #[arg(long, conflicts_with_all = ["upgrade", "upgrade_all", "input", "clipboard", "output", "check"])]
        check_all: bool,
        /// Re-style only: extract the body from the existing
        /// `instructions.html` and re-run the rule passes. Works on
        /// games that don't have a saved original; the body itself
        /// is reused, only the embedded CSS + per-rule scrubbing is
        /// refreshed. Implies `--upgrade` semantics.
        #[arg(long)]
        css_only: bool,
        /// Don't copy the input paste to `instructions_original.html`.
        /// By default, every `--input` / `--clipboard` / stdin paste
        /// is saved alongside `instructions.html` so future
        /// `--upgrade` runs have a faithful source to re-clean from.
        #[arg(long)]
        no_save_original: bool,
    },
    /// Profile a tournament run with `samply`. Builds the workspace
    /// under the `profiling` cargo profile (release + full debug
    /// info, so samply can see inlined functions and per-line
    /// attribution), records a profile, then opens the
    /// Firefox-profiler view via samply's built-in local server.
    ///
    /// Anything after `--` is forwarded verbatim to
    /// `tournament run`, so a typical invocation looks like:
    ///
    ///   cargo xtask profile -- --game tron v1 v2 \
    ///       --rounds 50 --parallel 1 \
    ///       --output /tmp/profile_run.jsonl
    Profile {
        /// Override the output path for the recorded profile.
        /// Defaults to `target/samply/profile.json.gz`.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Sampling rate in Hz. Default 1000 is fine for most
        /// matches; bump to 4000–8000 for sharper line-level
        /// attribution on hot inner loops. Passed through to
        /// `samply record --rate`.
        #[arg(long, default_value_t = 1000)]
        rate: u32,
        /// Forwarded to `tournament run`. Use `--` to separate
        /// from xtask's own flags, e.g.
        /// `cargo xtask profile -- --game tron v1 v2 …`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        tournament_args: Vec<String>,
    },
    /// Build every bot crate in release mode and copy each binary
    /// into `.snapshots/stable/`, with a sidecar `.toml` recording
    /// the HEAD commit. Idempotent — re-running with no source
    /// changes is a no-op (cargo's incremental cache plus a binary-
    /// mtime check skips the copy). The post-commit hook installed
    /// by `cargo xtask install-hooks` calls this in the background.
    SnapshotBots {
        /// Suppress progress output. Used by the post-commit hook
        /// (which logs to `.snapshots/snapshot.log`).
        #[arg(long)]
        quiet: bool,
    },
    /// Install the `.git/hooks/post-commit` hook that snapshots all
    /// bots after each commit. Opt-in — refuses to overwrite an
    /// existing hook. Remove with `rm .git/hooks/post-commit`.
    InstallHooks {
        /// Replace an existing post-commit hook instead of bailing.
        #[arg(long)]
        force: bool,
    },
    /// Iteration loop: rebuild the working-tree copy of `<bot>` and
    /// play a focused tournament against its snapshotted `stable`
    /// counterpart (the binary the post-commit hook captured at
    /// HEAD). Prints a verdict identical to `tournament compare`.
    /// Requires `.snapshots/stable/<crate>` to exist — run
    /// `cargo xtask snapshot-bots` once (or install the hook) to
    /// populate it.
    Iterate {
        /// Game the bot belongs to.
        #[arg(long)]
        game: String,
        /// Bot stem (e.g. `baseline`, `v1`). When both rs and cpp
        /// variants exist, qualify with `--lang`.
        bot: String,
        /// Force a specific language when both variants exist.
        #[arg(long, value_enum)]
        lang: Option<BotLang>,
        /// Seeds per (combination × seat rotation). Default keeps
        /// the loop snappy; bump for tighter verdicts.
        #[arg(long, default_value_t = 50)]
        rounds: u32,
        /// Players per match. Default 2 for the standard "did my
        /// edit help" comparison.
        #[arg(long, default_value_t = 2)]
        bots_per_match: u32,
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
        Command::Retire { target, force } => {
            retire(&target.game, &target.name, target.lang, force)?
        }
        Command::Promote {
            target,
            archive,
            cleanup_siblings,
        } => promote(
            &target.game,
            &target.name,
            target.lang,
            archive,
            cleanup_siblings,
        )?,
        Command::Champion { game, lang } => champion(&game, lang)?,
        Command::History { target } => history(&target.game, &target.name, target.lang)?,
        Command::CompactHistory { target, keep_last } => {
            compact_history(&target.game, &target.name, target.lang, keep_last)?
        }
        Command::Doctor { game } => doctor(&game)?,
        Command::Bundle {
            game,
            bot,
            lang,
            output,
            external,
        } => bundle(&game, bot.as_deref(), lang, output.as_deref(), &external)?,
        Command::Statement {
            game,
            input,
            output,
            clipboard,
            upgrade,
            upgrade_all,
            check,
            check_all,
            css_only,
            no_save_original,
        } => statement(StatementArgs {
            game,
            input,
            output,
            clipboard,
            upgrade,
            upgrade_all,
            check,
            check_all,
            css_only,
            no_save_original,
        })?,
        Command::Profile {
            output,
            rate,
            tournament_args,
        } => profile(output.as_deref(), rate, &tournament_args)?,
        Command::SnapshotBots { quiet } => snapshot_bots(quiet)?,
        Command::InstallHooks { force } => install_hooks(force)?,
        Command::Iterate {
            game,
            bot,
            lang,
            rounds,
            bots_per_match,
        } => iterate(&game, &bot, lang, rounds, bots_per_match)?,
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

/// Bundled flags for the `statement` subcommand. Owns its strings
/// (vs. borrowing) so we can hand the struct to a per-game loop
/// without lifetime gymnastics.
struct StatementArgs {
    game: Option<String>,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    clipboard: bool,
    upgrade: bool,
    upgrade_all: bool,
    check: bool,
    check_all: bool,
    css_only: bool,
    no_save_original: bool,
}

fn statement(args: StatementArgs) -> Result<()> {
    // Dispatch on the explicit "all" modes first; they ignore <game>.
    if args.upgrade_all {
        return upgrade_all(args.check, args.css_only);
    }
    if args.check_all {
        return check_all(args.css_only);
    }

    let game = args
        .game
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing <game> argument"))?;

    if args.upgrade {
        return upgrade_one(game, args.css_only, args.check, args.output.as_deref());
    }
    if args.css_only {
        // Css-only without --upgrade also makes sense — re-style the
        // existing file in place. Same code path.
        return upgrade_one(game, true, args.check, args.output.as_deref());
    }

    // Otherwise: classic clean-from-paste flow.
    clean_from_paste(
        game,
        args.input.as_deref(),
        args.clipboard,
        args.output.as_deref(),
        args.check,
        args.no_save_original,
    )
}

/// Original behaviour: read a paste (file / clipboard / stdin), feed
/// it through `cg_statement::clean`, write `instructions.html`. New:
/// also save the paste to `instructions_original.html` so future
/// `--upgrade` calls have a faithful source. Set `--no-save-original`
/// to skip the copy.
fn clean_from_paste(
    game: &str,
    input_path: Option<&Path>,
    clipboard: bool,
    output_override: Option<&Path>,
    check: bool,
    no_save_original: bool,
) -> Result<()> {
    use std::io::{IsTerminal, Read};

    let s = Style::new();
    let output = resolve_output(game, output_override);
    ensure_parent_dir(&output)?;

    let paste = if let Some(p) = input_path {
        fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
    } else if clipboard {
        read_clipboard()?
    } else {
        // Stdin. If we're attached to a terminal, the user is pasting
        // interactively — show them how to end the input.
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

    let cleanup = cg_statement::clean_with_options(
        &paste,
        &cg_statement::CleanOptions {
            title: Some(format!("{} - Game Statement", title_case_game(game))),
        },
    )?;
    report_warnings(game, &cleanup.warnings);

    if check {
        bail_on_drift(&output, &cleanup.html)?;
        println!(
            "{} {} is up to date",
            s.ok("✓"),
            s.path(&output.display().to_string()),
        );
        return Ok(());
    }

    fs::write(&output, &cleanup.html)
        .with_context(|| format!("writing {}", output.display()))?;
    println!(
        "{} Wrote {}",
        s.ok("✓"),
        s.path(&output.display().to_string()),
    );

    // Save the paste alongside instructions.html for future
    // --upgrade runs (canonical name: instructions_original.html).
    // Skipped on --no-save-original or when the source already IS
    // that file (avoid no-op writes / fake "saved" messages).
    if !no_save_original {
        let original = original_path(game);
        let same_path = input_path.map(|p| paths_equal(p, &original)).unwrap_or(false);
        if !same_path {
            ensure_parent_dir(&original)?;
            fs::write(&original, &paste)
                .with_context(|| format!("writing {}", original.display()))?;
            println!(
                "{} Saved paste to {} (re-runnable with `cargo xtask statement {game} --upgrade`)",
                s.ok("✓"),
                s.path(&original.display().to_string()),
            );
        }
    }
    Ok(())
}

/// `--upgrade` for a single game. If `css_only` is false, expects the
/// saved original; if true (or as fallback), re-cleans the existing
/// `instructions.html` body.
fn upgrade_one(
    game: &str,
    css_only: bool,
    check: bool,
    output_override: Option<&Path>,
) -> Result<()> {
    let s = Style::new();
    let output = resolve_output(game, output_override);

    let cleanup = if css_only {
        let existing = fs::read_to_string(&output)
            .with_context(|| format!("reading {}", output.display()))?;
        cg_statement::re_clean_with_options(
            &existing,
            &cg_statement::CleanOptions {
                title: Some(format!("{} - Game Statement", title_case_game(game))),
            },
        )
        .with_context(|| format!("css-only re-clean of {}", output.display()))?
    } else {
        let original = original_path(game);
        if !original.exists() {
            anyhow::bail!(
                "no {original_path} — save the paste there first, or rerun with `--css-only` to re-clean the existing {output} body in place",
                original_path = original.display(),
                output = output.display(),
            );
        }
        let paste = fs::read_to_string(&original)
            .with_context(|| format!("reading {}", original.display()))?;
        cg_statement::clean_with_options(
            &paste,
            &cg_statement::CleanOptions {
                title: Some(format!("{} - Game Statement", title_case_game(game))),
            },
        )?
    };
    report_warnings(game, &cleanup.warnings);

    if check {
        bail_on_drift(&output, &cleanup.html)?;
        println!(
            "{} {} is up to date",
            s.ok("✓"),
            s.path(&output.display().to_string()),
        );
        return Ok(());
    }

    ensure_parent_dir(&output)?;
    fs::write(&output, &cleanup.html)
        .with_context(|| format!("writing {}", output.display()))?;
    println!(
        "{} Wrote {}{}",
        s.ok("✓"),
        s.path(&output.display().to_string()),
        if css_only { " (css-only)" } else { "" },
    );
    Ok(())
}

/// `--upgrade-all`: loop over every game with a generated
/// `instructions.html`. Use the saved original when present;
/// fall back to `--css-only` only when `css_only=true` is set
/// (otherwise skip with a warning).
fn upgrade_all(check: bool, css_only: bool) -> Result<()> {
    let games = discover_games_with_instructions()?;
    if games.is_empty() {
        anyhow::bail!("no games found under games/*/game/instructions.html");
    }
    let mut had_failure = false;
    let s = Style::new();
    for game in &games {
        let original = original_path(game);
        let has_original = original.exists();
        let effective_css_only = !has_original && css_only;

        if !has_original && !css_only {
            eprintln!(
                "{} {} skipped — no {} (run with `--css-only` to re-clean in place)",
                s.warn("⚠"),
                game,
                original.display(),
            );
            continue;
        }

        println!("{} {}", s.heading("•"), s.heading(game));
        match upgrade_one(game, effective_css_only || !has_original, check, None) {
            Ok(()) => {}
            Err(e) => {
                had_failure = true;
                eprintln!("{} {}: {e:#}", s.warn("✗"), game);
            }
        }
    }
    if had_failure {
        anyhow::bail!("one or more games failed to upgrade");
    }
    Ok(())
}

/// `--check-all`: dry-run equivalent of `--upgrade-all`. Always
/// falls back to css-only for games without a saved original so the
/// check is comprehensive — the alternative (skip them) would silently
/// miss drift on the largest games.
fn check_all(_css_only_flag: bool) -> Result<()> {
    // `--check-all` always allows the css-only fallback; the
    // alternative is silently skipping games without an original,
    // which defeats the point of a comprehensive check.
    upgrade_all(true, true)
}

// ---- helpers shared by every statement path ----

/// Default output path for a game: `games/<game>/game/instructions.html`.
fn resolve_output(game: &str, override_path: Option<&Path>) -> PathBuf {
    override_path.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("games")
            .join(game)
            .join("game")
            .join("instructions.html")
    })
}

fn original_path(game: &str) -> PathBuf {
    PathBuf::from("games")
        .join(game)
        .join("game")
        .join("instructions_original.html")
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    Ok(())
}

/// True when two paths refer to the same file. Falls back to lexical
/// equality if neither path exists yet (e.g. we're about to create
/// the original).
fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Glob `games/*/game/instructions.html` and return the game names
/// (the second path component). Sorted for deterministic output.
fn discover_games_with_instructions() -> Result<Vec<String>> {
    let games_dir = PathBuf::from("games");
    if !games_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&games_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let candidate = games_dir.join(&name).join("game").join("instructions.html");
        if candidate.is_file() {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Per-file warning header. Same layout the cg_statement binary used
/// to print directly; we just add the `[<game>]` prefix so
/// `--upgrade-all` output stays unambiguous when many games warn.
fn report_warnings(game: &str, warnings: &[cg_statement::Warning]) {
    if warnings.is_empty() {
        return;
    }
    use std::collections::HashMap;
    let mut counts: HashMap<&cg_statement::Warning, usize> = HashMap::new();
    for w in warnings {
        *counts.entry(w).or_insert(0) += 1;
    }
    let mut sorted: Vec<(&cg_statement::Warning, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| warning_text(a.0).cmp(&warning_text(b.0)))
    });
    eprintln!(
        "[{game}] {} warning(s) ({} unique):",
        warnings.len(),
        sorted.len(),
    );
    for (w, n) in &sorted {
        if *n > 1 {
            eprintln!("  (×{n}) {}", warning_text(w));
        } else {
            eprintln!("  {}", warning_text(w));
        }
    }
}

fn warning_text(w: &cg_statement::Warning) -> String {
    use cg_statement::Warning;
    match w {
        Warning::UnknownInlineStyle { property, value } => {
            format!("unknown inline style: {property}: {value} (kept; add to rules.rs to silence)")
        }
        Warning::UnknownStatementClass(c) => {
            format!("unknown statement class: .{c} (kept; bundled CSS may not style it)")
        }
        Warning::UnknownIconClass(c) => {
            format!("unknown icon class: .{c} (kept; bundled CSS may not draw it)")
        }
        Warning::NoContentBoundary => {
            "could not find a content boundary; emitting whole input as body".to_string()
        }
    }
}

/// `--check` core: compare what we would write to what's on disk
/// and `bail!` with a friendly message on mismatch. Normalises line
/// endings before comparing so a Windows checkout doesn't false-
/// positive.
fn bail_on_drift(output: &Path, would_write: &str) -> Result<()> {
    let on_disk = match fs::read_to_string(output) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "{} does not exist; would create on a non-check run",
                output.display(),
            );
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", output.display())),
    };
    let norm_disk = on_disk.replace("\r\n", "\n");
    let norm_new = would_write.replace("\r\n", "\n");
    if norm_disk != norm_new {
        anyhow::bail!(
            "{} is out of date — re-run without `--check` to update",
            output.display(),
        );
    }
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

/// Build the workspace under the `profiling` cargo profile (release
/// optimizations + full debug info), then drive the resulting tournament
/// binary under `samply record`. samply's local server opens the
/// Firefox-profiler view for the recorded trace.
///
/// We use a dedicated cargo profile rather than `release` for two
/// reasons:
///
///   1. Full debug info (`debug = 2`) lets samply attribute samples
///      to source lines AND resolve inlined helpers as their own
///      frames in the call tree — the difference between "96% in
///      leaf_heuristic" and "60% in passable, 30% in cell_idx,
///      6% elsewhere".
///   2. The bigger debug sections + slower link don't bleed into
///      normal `cargo build --release` runs (tournament compare /
///      run, CI), which stay on the lean `release` profile.
fn profile(output_override: Option<&Path>, rate: u32, tournament_args: &[String]) -> Result<()> {
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

    // 2. Build just the tournament binary under the `profiling`
    //    profile (release optimizations + full DWARF, see
    //    `[profile.profiling]` in the top-level Cargo.toml; cargo
    //    writes artifacts to `target/profiling/`).
    //
    //    Bots aren't built here — `tournament run --profile profiling`
    //    (invoked below) goes through `resolve_and_build`, which
    //    rebuilds each participating bot under the same profile on
    //    demand. Building the full workspace upfront would compile
    //    every game/bot variant whether the run uses them or not.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    println!(
        "{} Building tournament under the `profiling` cargo profile (release + full debug info)…",
        s.ok("→"),
    );
    let status = std::process::Command::new(&cargo)
        .args(["build", "--profile", "profiling", "-p", "tournament"])
        .status()
        .context("running cargo build")?;
    anyhow::ensure!(status.success(), "cargo build failed");

    // 3. Resolve paths. The `profiling` profile lives at
    //    `target/profiling/<bin>` (not `target/release/`).
    let target_dir =
        PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string()));
    let tournament_bin = target_dir.join("profiling").join("tournament");
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
    //
    //    `--fold-recursive-prefix`: tron/fb bots are deeply recursive
    //    (alpha-beta, BFS), so unfolded call trees are dozens of
    //    frames tall and the leaf (heuristic / decide body) gets
    //    buried. Folding collapses repeated base frames so the
    //    interesting work surfaces at usable depth in samply's UI.
    //
    //    samply already follows fork/exec by default on macOS/Linux,
    //    so each bot subprocess becomes its own thread in the profile
    //    automatically — no extra flag needed.
    //
    //    `--profile profiling` is forwarded to `tournament run` so
    //    the bots it rebuilds also get the full-debug treatment and
    //    samply can see inlined / source-line attribution inside the
    //    bot's hot functions.
    let mut cmd = std::process::Command::new("samply");
    cmd.arg("record")
        .arg("--fold-recursive-prefix")
        .args(["--rate", &rate.to_string()])
        .arg("--output")
        .arg(&output);
    cmd.arg("--");
    cmd.arg(&tournament_bin);
    cmd.args(["run", "--profile", "profiling"]);
    cmd.args(tournament_args);

    println!(
        "{} samply record → {}",
        s.ok("→"),
        s.path(&output.display().to_string()),
    );
    let status = cmd.status().context("running samply")?;
    anyhow::ensure!(status.success(), "samply exited with {status}");

    // samply's default behaviour is to boot a local server and open
    // the Firefox-profiler view in a browser tab after the recording
    // completes; the UI stays up until you Ctrl-C, at which point we
    // print where the saved profile lives so you can `samply load`
    // it later without re-recording.
    println!(
        "{} Profile saved to {}. Reopen with {}.",
        s.ok("✓"),
        s.path(&output.display().to_string()),
        s.code(&format!("samply load {}", output.display())),
    );
    Ok(())
}

/// Bundle a bot into a single paste-ready file. Dispatches on
/// language: `cpp_flatten` for C++ bots, `flatten` for Rust bots. The
/// xtask is a thin orchestrator — the actual flattening lives in the
/// respective crates so they're independently testable and usable.
fn bundle(
    game: &str,
    bot: Option<&str>,
    lang_override: Option<BotLang>,
    output_override: Option<&Path>,
    external: &[String],
) -> Result<()> {
    let bots_dir = bots_dir(game);

    // When no bot was named, resolve to the current champion(s) per
    // bot.toml. `--lang` filters which champion to pick when both langs
    // have one.
    let bot: String = match bot {
        Some(b) => b.to_string(),
        None => find_champion(game, lang_override)?,
    };
    let bot = bot.as_str();

    let rs_dir = bots_dir.join(format!("{bot}_rs"));
    let cpp_dir = bots_dir.join(format!("{bot}_cpp"));
    let lang = resolve_bundle_lang(lang_override, &rs_dir, &cpp_dir, game, bot)?;

    let default_ext = match lang {
        BotLang::Rust => "rs",
        BotLang::Cpp => "cpp",
        BotLang::Both => unreachable!("resolve_bundle_lang collapses Both"),
    };
    let output: PathBuf = output_override.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("target")
            .join("codingame")
            .join(format!("{game}_{bot}_bot.{default_ext}"))
    });
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let s = Style::new();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    match lang {
        BotLang::Cpp => {
            if !external.is_empty() {
                eprintln!(
                    "{} `--external` is Rust-only; ignored for C++ bundle.",
                    s.code("warn:"),
                );
            }
            let entry = cpp_dir.join("main.cpp");
            anyhow::ensure!(
                entry.exists(),
                "no C++ bot entry at {} — does the bot have a main.cpp?",
                entry.display(),
            );
            let status = std::process::Command::new(&cargo)
                .args(["run", "--quiet", "-p", "cpp_flatten", "--"])
                .arg(&entry)
                .arg("-o")
                .arg(&output)
                .status()
                .context("invoking cpp_flatten binary")?;
            anyhow::ensure!(status.success(), "cpp_flatten exited with {status}");
        }
        BotLang::Rust => {
            // The bin is a thin stdio shim that calls into the bot's
            // sibling `lib.rs`; `flatten --vendor` inlines that lib
            // plus the workspace deps (`tron_defs`, `bot_common`, …)
            // so the output compiles standalone. The CodinGame preset
            // is always applied so anyhow/serde/etc. stay as `use`s
            // (CG ships them).
            let crate_name = format!("{game}_{bot}_rs");
            let mut cmd = std::process::Command::new(&cargo);
            cmd.args(["run", "--quiet", "-p", "flatten", "--"])
                .arg(&rs_dir)
                .args(["--bin", &crate_name])
                .arg("-o")
                .arg(&output)
                .arg("--vendor")
                .args(["--external-preset", "codingame"])
                // Strip line + block comments from the bundled output.
                // The auto-generated banner at the top stays put
                // (flatten prepends it AFTER minifying).
                .arg("--minify");
            for ext in external {
                cmd.args(["--external", ext]);
            }
            let status = cmd.status().context("invoking flatten binary")?;
            anyhow::ensure!(status.success(), "flatten exited with {status}");
        }
        BotLang::Both => unreachable!(),
    }

    println!(
        "{} Bundled {} ({}, {}) → {}",
        s.ok("✓"),
        s.name(game),
        s.name(bot),
        s.code(match lang {
            BotLang::Rust => "rust",
            BotLang::Cpp => "cpp",
            BotLang::Both => "",
        }),
        s.path(&output.display().to_string()),
    );
    println!(
        "  Paste the contents of {} into CodinGame's editor.",
        s.path(&output.display().to_string()),
    );
    Ok(())
}

/// Pick which language to bundle. `--lang` wins if passed; otherwise
/// auto-detect from directory existence. Errors when neither (or both,
/// without `--lang`) exists.
fn resolve_bundle_lang(
    override_: Option<BotLang>,
    rs_dir: &Path,
    cpp_dir: &Path,
    game: &str,
    bot: &str,
) -> Result<BotLang> {
    if let Some(l) = override_ {
        let dir = match l {
            BotLang::Rust => rs_dir,
            BotLang::Cpp => cpp_dir,
            BotLang::Both => anyhow::bail!("`--lang both` makes no sense for bundle"),
        };
        anyhow::ensure!(
            dir.exists(),
            "no bot at {} — `--lang` selected a variant that doesn't exist",
            dir.display(),
        );
        return Ok(l);
    }
    match (rs_dir.exists(), cpp_dir.exists()) {
        (true, false) => Ok(BotLang::Rust),
        (false, true) => Ok(BotLang::Cpp),
        (true, true) => anyhow::bail!(
            "{game}/{bot} has both rs and cpp variants — pass `--lang rust|cpp` to pick one",
        ),
        (false, false) => anyhow::bail!(
            "no bot found at {} or {}",
            rs_dir.display(),
            cpp_dir.display(),
        ),
    }
}

fn new_game(name: &str) -> Result<()> {
    let vars = TemplateVars::new(name);
    let game_root = format!("games/{name}");
    // Engine bits: defs / game / viz under `<ws>/games/<name>/`.
    render_template("game", &game_root, &vars)?;
    // Baseline bots: games/<name>/bots/baseline_{rs,cpp}/. Uses the same
    // templates as `new-bot`, so the two paths stay in lockstep.
    let baseline_bot = "baseline";
    for (suffix, tmpl) in BotLang::Both.variants() {
        let bot_vars = BotTemplateVars::new(name, baseline_bot, suffix);
        let dest = format!("{game_root}/bots/{baseline_bot}_{suffix}");
        render_template(tmpl, &dest, &bot_vars)?;
    }

    // Engine crates + bot crates are members; only `defs` and `game`
    // are surfaced as workspace dependencies (everything else is a leaf).
    // Directory names drop the `<game>_` prefix (the parent dir
    // already namespaces); crate names keep it (they must be globally
    // unique across the workspace).
    for dir in ["defs", "game", "viz"] {
        let crate_path = format!("{game_root}/{dir}");
        add_workspace_member("Cargo.toml", &crate_path)?;
    }
    for (suffix, _) in BotLang::Both.variants() {
        let crate_path = format!("{game_root}/bots/{baseline_bot}_{suffix}");
        add_workspace_member("Cargo.toml", &crate_path)?;
    }
    for dir in ["defs", "game"] {
        let crate_name = format!("{name}_{dir}");
        let crate_path = format!("{game_root}/{dir}");
        add_workspace_dependency("Cargo.toml", &crate_name, &crate_path)?;
    }

    // Wire the new `_game` crate into every downstream that dispatches
    // by game name: runner (single-match CLI), tournament (multi-match
    // harness). Each gets the Cargo dep + match arm (+ `use` import
    // where needed). Idempotent — safe to re-run `new-game` on an
    // existing scaffold without duplicating arms.
    let game_crate = format!("{name}_game");
    add_cargo_dep("crates/runner/Cargo.toml", &game_crate)?;
    wire_game_registry("crates/runner/src/lib.rs", name, &vars.name_pascal)?;

    print_next_steps(name, &vars.name_pascal);
    Ok(())
}

fn new_bot(game: &str, bot: &str, lang: BotLang, from_existing: Option<&str>) -> Result<()> {
    // Game must already exist (defs crate is the canonical marker).
    let game_root = PathBuf::from("games").join(game);
    let game_defs_path = game_root.join("defs");
    anyhow::ensure!(
        game_defs_path.exists(),
        "game `{game}` not found (no {})",
        game_defs_path.display(),
    );

    let s = Style::new();
    let mut created: Vec<String> = Vec::new();
    for (suffix, tmpl) in lang.variants() {
        let dest_path = game_root.join("bots").join(format!("{bot}_{suffix}"));
        anyhow::ensure!(
            !dest_path.exists(),
            "bot already exists at {} — pick a different `--name` or delete it first",
            dest_path.display(),
        );
        let dest_str = dest_path.to_string_lossy().to_string();

        if let Some(src_bot) = from_existing {
            // Clone an existing bot crate of the same language. Crate
            // name substitutions throughout.
            let src_path = game_root.join("bots").join(format!("{src_bot}_{suffix}"));
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

        // Drop a bot.toml so retire / promote / compare can walk
        // lineage without grepping source. Fresh scaffolds get
        // `parent = None`; clones get the source bot's stem.
        let parent = from_existing.map(|s| s.to_string());
        let description = match &parent {
            Some(p) => format!("clone of {p}"),
            None => format!("{game} bot ({suffix})"),
        };
        let manifest = BotManifest {
            name: bot.to_string(),
            lang: suffix.to_string(),
            parent,
            created_at: None,
            description,
            // CodinGame submission info gets filled in by hand after
            // the user actually submits the bot.
            codingame_league: None,
            codingame_standing: None,
            // Champion bit is never set on creation — promote flips it.
            // A freshly-scaffolded bot has no tournament history yet.
            champion: false,
            history: vec![],
        };
        manifest.write(&BotManifest::path(game, bot, suffix))?;

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
            "cargo run -p codingame_runner -- --game {game} \\\n     target/release/{} target/release/<opponent>",
            created[0]
        )),
    );
    Ok(())
}

/// Delete a bot's crate(s) + drop their workspace member entries +
/// cargo-clean their build artifacts. Inverse of `new_bot`.
///
/// Resolution rules for which language(s) to retire:
///   * `--lang rust|cpp` — exactly that variant; errors if missing.
///   * `--lang both` — wipe both variants if present (each missing one
///     is silently skipped).
///   * (no `--lang`) — auto-detect: if exactly one variant exists retire
///     it; if both exist, error and demand an explicit `--lang`.
///
/// Safety checks (skip with `--force`):
///   * Bot's `bot.toml` has `champion = true`.
///   * Some other bot in the same game+lang lane lists this bot as its
///     `parent` (would orphan descendants).
fn retire(game: &str, bot: &str, lang_override: Option<BotLang>, force: bool) -> Result<()> {
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );

    let langs = resolve_bot_langs(&bots_dir, bot, lang_override)?;

    // Safety checks — collect all blockers up front before mutating
    // anything, so a partial retire on a mixed champion/non-champion
    // pair can't half-execute.
    if !force {
        let mut blockers: Vec<String> = Vec::new();
        for lang in &langs {
            let dir = bots_dir.join(format!("{bot}_{lang}"));
            let manifest_path = dir.join("bot.toml");
            if manifest_path.exists() {
                let m = BotManifest::read(&manifest_path)?;
                if m.champion {
                    blockers.push(format!(
                        "{bot}_{lang} is currently champion (set `champion = false` first, or pass --force)"
                    ));
                }
            }
            let children = find_children(&bots_dir, bot, lang)?;
            if !children.is_empty() {
                blockers.push(format!(
                    "{bot}_{lang} is parent of: {} (would orphan; pass --force to proceed)",
                    children.join(", "),
                ));
            }
        }
        if !blockers.is_empty() {
            anyhow::bail!("refusing to retire:\n  - {}", blockers.join("\n  - "));
        }
    }

    let s = Style::new();
    for lang in &langs {
        let dir = bots_dir.join(format!("{bot}_{lang}"));
        let member_path = dir.to_string_lossy().to_string();
        let crate_name = format!("{game}_{bot}_{lang}");

        // `cargo clean -p <crate>` BEFORE we remove the workspace
        // entry — cargo refuses to operate on packages it can't see in
        // the resolved graph.
        let _ = std::process::Command::new("cargo")
            .args(["clean", "-p", &crate_name])
            .output();

        remove_workspace_member("Cargo.toml", &member_path)?;
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;

        println!(
            "{} Retired {} ({}) — removed crate dir, workspace member, and target cache.",
            s.ok("✓"),
            s.name(&format!("{bot}_{lang}")),
            s.code(&crate_name),
        );
    }
    Ok(())
}

/// `games/<game>/bots/` — the per-game directory that holds every
/// `<bot>_<lang>/` crate. One place that knows the layout so the
/// `PathBuf::from("games").join(game).join("bots")` chain isn't
/// spelled out at every call site.
fn bots_dir(game: &str) -> PathBuf {
    PathBuf::from("games").join(game).join("bots")
}

/// `<bots_dir>/<bot>_<lang>/bot.toml`. Pairs with `read_bot_manifest`
/// at sites that need to write the manifest back later.
fn bot_manifest_path(bots_dir: &Path, bot: &str, lang: &str) -> PathBuf {
    bots_dir.join(format!("{bot}_{lang}")).join("bot.toml")
}

/// Read a bot's manifest. Returns a uniform error when the file is
/// missing — some legacy bots predate `bot.toml` landing and need a
/// hand-written one with at least `name`, `lang`, and (for promote)
/// `parent` fields.
fn read_bot_manifest(bots_dir: &Path, bot: &str, lang: &str) -> Result<BotManifest> {
    let path = bot_manifest_path(bots_dir, bot, lang);
    anyhow::ensure!(
        path.exists(),
        "no bot.toml at {} — was this bot scaffolded before bot.toml landed? \
         (hand-write a minimal manifest with `name`, `lang`, and `parent` to repair)",
        path.display(),
    );
    BotManifest::read(&path)
}

/// Find every bot under `bots_dir/*_<lang>/` whose `bot.toml` declares
/// `parent = <parent>`. Used by `retire`'s safety check to flag
/// orphans before they happen. Returns bare bot stems with their
/// lang suffix (e.g. `["v1_5_cpp", "v1_some_algo_cpp"]`).
fn find_children(bots_dir: &Path, parent: &str, lang: &str) -> Result<Vec<String>> {
    let mut children = Vec::new();
    let Ok(entries) = fs::read_dir(bots_dir) else {
        return Ok(children);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Skip dirs that don't end with `_<lang>` — we only walk the
        // same-lang lane (siblings).
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let suffix = format!("_{lang}");
        if !name.ends_with(&suffix) {
            continue;
        }
        // Skip the parent itself.
        if name.strip_suffix(&suffix) == Some(parent) {
            continue;
        }
        let manifest_path = dir.join("bot.toml");
        if !manifest_path.exists() {
            continue;
        }
        if let Ok(m) = BotManifest::read(&manifest_path)
            && m.parent.as_deref() == Some(parent)
        {
            children.push(format!("{}_{lang}", m.name));
        }
    }
    children.sort();
    Ok(children)
}

/// Walk `bots_dir/*/bot.toml` and collect every manifest with
/// `champion = true`. Returns `(name, lang)` pairs. Used by
/// `find_champion` and the `champion` print verb.
fn list_champions(bots_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(bots_dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("bot.toml");
        if !manifest_path.exists() {
            continue;
        }
        if let Ok(m) = BotManifest::read(&manifest_path)
            && m.champion
        {
            out.push((m.name, m.lang));
        }
    }
    // Stable sort: by lang then name, so output is reproducible.
    out.sort();
    Ok(out)
}

/// Resolve the bot name to bundle when none was passed. `--lang` filter
/// disambiguates when more than one lang has a champion.
fn find_champion(game: &str, lang_filter: Option<BotLang>) -> Result<String> {
    let champions = list_champions(&bots_dir(game))?;
    anyhow::ensure!(
        !champions.is_empty(),
        "no bot in `{game}` has `champion = true` in its bot.toml — \
         pass the bot name explicitly, or set the bit on one bot first.",
    );
    let lang_str: Option<&str> = match lang_filter {
        Some(BotLang::Rust) => Some("rs"),
        Some(BotLang::Cpp) => Some("cpp"),
        Some(BotLang::Both) | None => None,
    };
    let filtered: Vec<&(String, String)> = champions
        .iter()
        .filter(|(_, l)| lang_str.is_none_or(|w| l == w))
        .collect();
    match filtered.as_slice() {
        [] => anyhow::bail!(
            "no champion in `{game}` for lang={:?}; champions found: {}",
            lang_str.unwrap_or("(any)"),
            champions
                .iter()
                .map(|(n, l)| format!("{n}_{l}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        [only] => Ok(only.0.clone()),
        many => anyhow::bail!(
            "multiple champions in `{game}` ({}) — pass --lang to pick",
            many.iter()
                .map(|(n, l)| format!("{n}_{l}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// `cargo xtask champion <game> [--lang L]` — read-only print of the
/// current champion(s) per (game, lang). Renders the bot's
/// description + last `[[history]]` entry inline so the user can see
/// what they'd be shipping at a glance.
fn champion(game: &str, lang_filter: Option<BotLang>) -> Result<()> {
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );
    let champions = list_champions(&bots_dir)?;
    let lang_str: Option<&str> = match lang_filter {
        Some(BotLang::Rust) => Some("rs"),
        Some(BotLang::Cpp) => Some("cpp"),
        Some(BotLang::Both) | None => None,
    };
    let filtered: Vec<&(String, String)> = champions
        .iter()
        .filter(|(_, l)| lang_str.is_none_or(|w| l == w))
        .collect();
    if filtered.is_empty() {
        if let Some(want) = lang_str {
            println!("No champion in {game} for lang={want}.");
        } else {
            println!("No champion in {game}. (No bot.toml has `champion = true`.)");
        }
        return Ok(());
    }
    let s = Style::new();
    for (name, lang) in &filtered {
        let dir = bots_dir.join(format!("{name}_{lang}"));
        let manifest = BotManifest::read(&dir.join("bot.toml"))?;
        println!(
            "{} {}_{}  {}",
            s.heading("★"),
            s.name(name),
            lang,
            s.code(&format!("({game}_{name}_{lang})")),
        );
        println!("    description: {}", manifest.description);
        if let Some(parent) = &manifest.parent {
            println!("    parent:      {parent}_{lang}");
        }
        if manifest.codingame_league.is_some() || manifest.codingame_standing.is_some() {
            let league = manifest.codingame_league.as_deref().unwrap_or("—");
            match manifest.codingame_standing {
                Some(rank) => println!("    submitted:   {league} (rank #{rank})"),
                None => println!("    submitted:   {league}"),
            }
        }
        if let Some(last) = manifest.history.last() {
            println!(
                "    last match:  vs {} @ {} — {} pts (vs {}), verdict={}",
                last.opponent, last.ran_at, last.pts, last.opponent_pts, last.verdict,
            );
        }
    }
    Ok(())
}

/// `cargo xtask doctor <game>` — walk every bot under
/// `games/<game>/bots/` and flag inconsistencies. No --fix mode
/// (yet); print all findings, exit 1 if any, 0 if clean.
///
/// Checks:
///   1. Multiple `champion = true` per (game, lang).
///   2. `parent = X` where `X_<lang>` doesn't exist.
///   3. History entries referencing a now-deleted opponent.
///   4. [workspace.members] entries pointing at non-existent dirs.
///   5. Bot dirs missing a `bot.toml`.
///   6. Bot dirs not registered in [workspace.members].
fn doctor(game: &str) -> Result<()> {
    use std::collections::{BTreeMap, BTreeSet};
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );

    // --- 1. Discover every (name, lang) bot dir + its manifest (if any).
    // BTreeMap for stable iteration → deterministic doctor output.
    let mut bots: BTreeMap<(String, String), Option<BotManifest>> = BTreeMap::new();
    let mut findings: Vec<String> = Vec::new();
    for entry in
        fs::read_dir(&bots_dir).with_context(|| format!("reading {}", bots_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Match `<stem>_(rs|cpp)`; skip dirs that don't follow the convention.
        let (stem, lang) = if let Some(s) = dir_name.strip_suffix("_rs") {
            (s, "rs")
        } else if let Some(s) = dir_name.strip_suffix("_cpp") {
            (s, "cpp")
        } else {
            continue;
        };
        let manifest_path = path.join("bot.toml");
        let manifest = if manifest_path.exists() {
            Some(BotManifest::read(&manifest_path)?)
        } else {
            findings.push(format!(
                "bot dir {dir_name} has no bot.toml (run a fresh `new-bot` or backfill by hand)",
            ));
            None
        };
        bots.insert((stem.to_string(), lang.to_string()), manifest);
    }

    // --- 2. Multiple champions per (lang).
    let mut champions_by_lang: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ((name, lang), m) in &bots {
        if let Some(m) = m
            && m.champion
        {
            champions_by_lang
                .entry(lang.clone())
                .or_default()
                .push(name.clone());
        }
    }
    for (lang, names) in &champions_by_lang {
        if names.len() > 1 {
            findings.push(format!(
                "multiple champions in lang={lang}: {} (only one should have `champion = true`)",
                names.join(", "),
            ));
        }
    }

    // --- 3. parent points to a bot that doesn't exist in the same lang lane.
    for ((name, lang), m) in &bots {
        if let Some(m) = m
            && let Some(parent) = &m.parent
            && !bots.contains_key(&(parent.clone(), lang.clone()))
        {
            findings.push(format!(
                "{name}_{lang}: parent = \"{parent}\" but no {parent}_{lang} exists \
                 (was the parent retired without promote? edit bot.toml to repair)",
            ));
        }
    }

    // --- 4. History entries referencing a now-deleted opponent (in same lang).
    for ((name, lang), m) in &bots {
        if let Some(m) = m {
            let known: BTreeSet<&String> = bots
                .keys()
                .filter(|(_, l)| l == lang)
                .map(|(n, _)| n)
                .collect();
            let mut dangling: BTreeSet<&String> = BTreeSet::new();
            for h in &m.history {
                if !known.contains(&h.opponent) {
                    dangling.insert(&h.opponent);
                }
            }
            for opp in dangling {
                findings.push(format!(
                    "{name}_{lang} has history vs {opp} but no {opp}_{lang} exists \
                     (cleaned up with `xtask retire`; history is preserved as a record)",
                ));
            }
        }
    }

    // --- 5 + 6. Cross-check against root Cargo.toml [workspace.members].
    let root_cargo = fs::read_to_string("Cargo.toml").context("reading workspace Cargo.toml")?;
    let doc = root_cargo
        .parse::<DocumentMut>()
        .context("parsing workspace Cargo.toml")?;
    let prefix = format!("games/{game}/bots/");
    let mut ws_entries: BTreeSet<String> = BTreeSet::new();
    if let Some(members) = doc["workspace"]["members"].as_array() {
        for m in members.iter() {
            if let Some(s) = m.as_str() {
                ws_entries.insert(s.to_string());
                if s.starts_with(&prefix) {
                    let p = PathBuf::from(s);
                    if !p.exists() {
                        findings.push(format!(
                            "[workspace.members] has {s} but the directory is missing",
                        ));
                    }
                }
            }
        }
    }
    for (name, lang) in bots.keys() {
        let expected = format!("games/{game}/bots/{name}_{lang}");
        if !ws_entries.contains(&expected) {
            findings.push(format!(
                "bot dir {name}_{lang} exists but isn't in [workspace.members] (cargo will ignore it)",
            ));
        }
    }

    // --- Print.
    let s = Style::new();
    if findings.is_empty() {
        println!(
            "{} doctor: no issues in {game} ({} bot{} checked).",
            s.ok("✓"),
            bots.len(),
            if bots.len() == 1 { "" } else { "s" },
        );
        Ok(())
    } else {
        println!("doctor: found {} issue(s) in {game}:", findings.len());
        for f in &findings {
            println!("  • {f}");
        }
        std::process::exit(1);
    }
}

/// `cargo xtask history --game G --name N [--lang L]` — render a
/// bot's `[[history]]` block chronologically. Each row is one
/// previous tournament outcome appended by `tournament compare
/// --record-history`. Empty history is a clean "no runs recorded
/// yet" message.
fn history(game: &str, bot: &str, lang_override: Option<BotLang>) -> Result<()> {
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );
    let langs = resolve_bot_langs(&bots_dir, bot, lang_override)?;
    let s = Style::new();
    for lang in &langs {
        let manifest = read_bot_manifest(&bots_dir, bot, lang)?;
        println!(
            "{} {}_{}  {}",
            s.heading("⏱"),
            s.name(bot),
            lang,
            s.code(&format!("({game}_{bot}_{lang})")),
        );
        if manifest.codingame_league.is_some() || manifest.codingame_standing.is_some() {
            let league = manifest.codingame_league.as_deref().unwrap_or("—");
            match manifest.codingame_standing {
                Some(rank) => println!("    submitted:   {league} (rank #{rank})"),
                None => println!("    submitted:   {league}"),
            }
        }
        if manifest.history.is_empty() {
            println!(
                "    (no tournament history recorded yet — run \
                      `tournament compare --record-history`)"
            );
            continue;
        }
        for entry in &manifest.history {
            println!(
                "    {at}  vs {opp:<10}  {pts:>5.1} pts  (opp {opp_pts:>5.1})  {rounds:>4} rounds  → {verdict}",
                at = entry.ran_at,
                opp = entry.opponent,
                pts = entry.pts,
                opp_pts = entry.opponent_pts,
                rounds = entry.rounds,
                verdict = entry.verdict,
            );
        }
    }
    Ok(())
}

/// `cargo xtask compact-history` — drop all but the most recent
/// `keep_last` `[[history]]` entries from the bot's `bot.toml`.
/// Idempotent: a bot with ≤ keep_last entries is a no-op + a
/// one-line report.
fn compact_history(
    game: &str,
    bot: &str,
    lang_override: Option<BotLang>,
    keep_last: usize,
) -> Result<()> {
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );
    let langs = resolve_bot_langs(&bots_dir, bot, lang_override)?;
    let s = Style::new();
    for lang in &langs {
        let mut manifest = read_bot_manifest(&bots_dir, bot, lang)?;
        let manifest_path = bot_manifest_path(&bots_dir, bot, lang);
        let before = manifest.history.len();
        if before <= keep_last {
            println!(
                "{}_{}: {} entries (≤ keep_last {}); nothing to compact.",
                bot, lang, before, keep_last,
            );
            continue;
        }
        let dropped = before - keep_last;
        // Keep the tail — most recent entries are at the end of the
        // vector since `record_history` only appends.
        manifest.history = manifest.history.split_off(dropped);
        manifest.write(&manifest_path)?;
        println!(
            "{} {}_{}: dropped {} older entries, kept the last {}.",
            s.ok("✓"),
            bot,
            lang,
            dropped,
            manifest.history.len(),
        );
    }
    Ok(())
}

/// Shared `--lang rust|cpp|both|(none)` → `Vec<lang_suffix>` resolver
/// used by both `retire` and `promote`. `bot` is the bare stem (no
/// `_<lang>` suffix); errors carry the directories actually probed.
fn resolve_bot_langs<'a>(
    bots_dir: &Path,
    bot: &str,
    lang_override: Option<BotLang>,
) -> Result<Vec<&'a str>> {
    let rs_dir = bots_dir.join(format!("{bot}_rs"));
    let cpp_dir = bots_dir.join(format!("{bot}_cpp"));
    let langs = match lang_override {
        Some(BotLang::Rust) => {
            anyhow::ensure!(rs_dir.exists(), "no rust variant at {}", rs_dir.display());
            vec!["rs"]
        }
        Some(BotLang::Cpp) => {
            anyhow::ensure!(cpp_dir.exists(), "no cpp variant at {}", cpp_dir.display());
            vec!["cpp"]
        }
        Some(BotLang::Both) => {
            let mut v = Vec::new();
            if rs_dir.exists() {
                v.push("rs");
            }
            if cpp_dir.exists() {
                v.push("cpp");
            }
            anyhow::ensure!(
                !v.is_empty(),
                "no bot at {} or {}",
                rs_dir.display(),
                cpp_dir.display(),
            );
            v
        }
        None => match (rs_dir.exists(), cpp_dir.exists()) {
            (true, false) => vec!["rs"],
            (false, true) => vec!["cpp"],
            (true, true) => anyhow::bail!(
                "`{bot}` has both rs and cpp variants — pass `--lang rust|cpp|both` to pick"
            ),
            (false, false) => {
                anyhow::bail!("no bot at {} or {}", rs_dir.display(), cpp_dir.display(),)
            }
        },
    };
    Ok(langs)
}

/// Walk a directory tree in-place, applying `content.replace(from, to)`
/// to every text file. Used by `promote` to rewrite the full crate name
/// — i.e. the post-Tier-0 namespace token — in Cargo.toml + source after
/// a rename. Skips binary files via the same `is_text_file` heuristic
/// `copy_dir_substituting` uses.
fn rewrite_dir_contents(dir: &Path, from: &str, to: &str) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            rewrite_dir_contents(&path, from, to)?;
        } else if is_text_file(&path) {
            let content =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let rewritten = content.replace(from, to);
            if rewritten != content {
                fs::write(&path, rewritten)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Find every descendant of `ancestor` in the same game+lang lane —
/// transitively. Used by `promote --cleanup-siblings` to retire not
/// just the candidate's siblings but anything those siblings parented,
/// so we don't leave orphan grandchildren pointing at dead crates.
/// Bounded by `MAX_DEPTH` (cycles would be a hand-edit bug, not a
/// legitimate state, but we guard regardless). Returns the bot stems
/// in topological order: deepest first, so retire-by-iteration safely
/// removes children before parents.
fn find_descendants(bots_dir: &Path, ancestor: &str, lang: &str) -> Result<Vec<String>> {
    const MAX_DEPTH: usize = 32;
    let mut out: Vec<String> = Vec::new();
    let mut frontier: Vec<(String, usize)> = vec![(ancestor.to_string(), 0)];
    while let Some((cur, depth)) = frontier.pop() {
        if depth > MAX_DEPTH {
            anyhow::bail!(
                "lineage walk exceeded depth {MAX_DEPTH} starting from {ancestor}_{lang} — \
                 cycle in bot.toml parent fields?",
            );
        }
        let children = find_children(bots_dir, &cur, lang)?;
        for child in children {
            // child is "v1_5_cpp" form; strip the suffix to get the stem
            let stem = child
                .strip_suffix(&format!("_{lang}"))
                .map(str::to_string)
                .unwrap_or(child);
            frontier.push((stem.clone(), depth + 1));
            out.push(stem);
        }
    }
    // Deepest-first via reverse: ensures we retire `v1_5_a_smaller`
    // before `v1_5_a` so retire's parent-of-X safety check (we use
    // --force, but still) doesn't complain.
    out.reverse();
    Ok(out)
}

/// Run the retire path on a single bot+lang variant, bypassing the
/// champion/parent safety checks (promote is the one deciding to
/// remove these). Thin wrapper for clarity — promote calls this
/// during sibling cleanup and during non-archive parent removal.
fn force_retire_one(game: &str, bot: &str, lang: &str) -> Result<()> {
    let dir = bots_dir(game).join(format!("{bot}_{lang}"));
    if !dir.exists() {
        return Ok(());
    }
    let crate_name = format!("{game}_{bot}_{lang}");
    let member_path = dir.to_string_lossy().to_string();
    let _ = std::process::Command::new("cargo")
        .args(["clean", "-p", &crate_name])
        .output();
    remove_workspace_member("Cargo.toml", &member_path)?;
    fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(())
}

/// Compact timestamp suitable for embedding in a directory name:
/// `20260601_220930`. UTC, derived from `now_rfc3339`. Used by
/// `promote --archive` to disambiguate snapshots of the same parent.
fn archive_timestamp() -> String {
    let s = now_rfc3339(); // "YYYY-MM-DDTHH:MM:SSZ"
    format!(
        "{}{}{}_{}{}{}",
        &s[0..4],
        &s[5..7],
        &s[8..10],
        &s[11..13],
        &s[14..16],
        &s[17..19],
    )
}

/// Promote a candidate bot into its parent's slot. See the `Promote`
/// CLI doc-comment for the full semantics; in short:
///
/// 1. Read `candidate/bot.toml` to discover the `parent`.
/// 2. (If `--cleanup-siblings`) retire every sibling of the candidate
///    plus all their descendants, deepest-first.
/// 3. Either archive (`--archive`) or retire the old parent.
/// 4. Rename the candidate dir → parent's slot, rewrite the full
///    crate-name token in Cargo.toml + source, swap workspace member
///    entries, and update bot.toml (`name`, `parent`, `champion`).
///
/// The champion bit only travels if the old parent had it; promoting
/// a non-champion-branch sibling doesn't disturb the current champion.
fn promote(
    game: &str,
    candidate: &str,
    lang_override: Option<BotLang>,
    archive: bool,
    cleanup_siblings: bool,
) -> Result<()> {
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );
    let langs = resolve_bot_langs(&bots_dir, candidate, lang_override)?;
    for lang in &langs {
        promote_one_lang(game, candidate, lang, archive, cleanup_siblings)?;
    }
    Ok(())
}

fn promote_one_lang(
    game: &str,
    candidate: &str,
    lang: &str,
    archive: bool,
    cleanup_siblings: bool,
) -> Result<()> {
    let bots_dir = bots_dir(game);
    let candidate_dir = bots_dir.join(format!("{candidate}_{lang}"));
    let candidate_manifest = read_bot_manifest(&bots_dir, candidate, lang)?;
    let parent_name = candidate_manifest.parent.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "{candidate}_{lang}'s bot.toml has `parent = null` — nothing to promote into. \
             (Originally-authored baselines aren't promotable; clone-and-tweak workflow only.)",
        )
    })?;

    let parent_dir = bots_dir.join(format!("{parent_name}_{lang}"));
    anyhow::ensure!(
        parent_dir.exists(),
        "parent {parent_name}_{lang} doesn't exist at {} — \
         was it already promoted away or hand-deleted?",
        parent_dir.display(),
    );
    let parent_manifest_path = parent_dir.join("bot.toml");
    let parent_manifest = if parent_manifest_path.exists() {
        BotManifest::read(&parent_manifest_path)?
    } else {
        // Synthesize a minimal manifest so the promote can proceed
        // even when the parent predates bot.toml landing.
        BotManifest {
            name: parent_name.clone(),
            lang: lang.to_string(),
            parent: None,
            created_at: None,
            description: format!("(no bot.toml — synthesized for promote of {candidate})"),
            codingame_league: None,
            codingame_standing: None,
            champion: false,
            history: vec![],
        }
    };
    let parent_was_champion = parent_manifest.champion;

    // Compute sibling sweep set (if requested).
    let mut to_retire: Vec<String> = Vec::new();
    if cleanup_siblings {
        let siblings = find_children(&bots_dir, &parent_name, lang)?
            .into_iter()
            .filter_map(|s| s.strip_suffix(&format!("_{lang}")).map(str::to_string))
            .filter(|stem| stem != candidate)
            .collect::<Vec<_>>();
        for sibling in &siblings {
            // Descendants first (deepest), then the sibling itself —
            // matches retire's "remove leaves first" ordering.
            to_retire.extend(find_descendants(&bots_dir, sibling, lang)?);
            to_retire.push(sibling.clone());
        }
    }

    let s = Style::new();
    println!(
        "{} Promote {} → {} ({}):",
        s.heading("→"),
        candidate,
        parent_name,
        lang
    );
    if !to_retire.is_empty() {
        println!(
            "  • retire siblings + descendants: {}",
            to_retire
                .iter()
                .map(|b| format!("{b}_{lang}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if archive {
        println!("  • archive old parent: {parent_name}_{lang} → <ts>_{lang}",);
    } else {
        println!("  • retire old parent: {parent_name}_{lang}");
    }
    println!("  • rename {candidate}_{lang} → {parent_name}_{lang}");
    if parent_was_champion {
        println!(
            "  • {} promoted bot becomes champion (parent had `champion = true`)",
            s.ok("✓")
        );
    }

    // -----------------------------------------------------------------
    // 1. Sibling sweep (deepest-first; we collected them in that order).
    for retiree in &to_retire {
        force_retire_one(game, retiree, lang)?;
    }

    // -----------------------------------------------------------------
    // 2. Archive or retire the old parent.
    let archived_parent_stem: Option<String> = if archive {
        let ts = archive_timestamp();
        let archived_stem = format!("{parent_name}_archived_{ts}");
        let archived_dir = bots_dir.join(format!("{archived_stem}_{lang}"));
        fs::rename(&parent_dir, &archived_dir).with_context(|| {
            format!(
                "renaming {} → {} (parent archive)",
                parent_dir.display(),
                archived_dir.display(),
            )
        })?;
        let old_crate = format!("{game}_{parent_name}_{lang}");
        let new_crate = format!("{game}_{archived_stem}_{lang}");
        rewrite_dir_contents(&archived_dir, &old_crate, &new_crate)?;
        remove_workspace_member("Cargo.toml", &parent_dir.to_string_lossy())?;
        add_workspace_member("Cargo.toml", &archived_dir.to_string_lossy())?;
        let mut m = parent_manifest.clone();
        m.name = archived_stem.clone();
        m.champion = false;
        m.write(&archived_dir.join("bot.toml"))?;
        let _ = std::process::Command::new("cargo")
            .args(["clean", "-p", &old_crate])
            .output();
        Some(archived_stem)
    } else {
        force_retire_one(game, &parent_name, lang)?;
        None
    };

    // -----------------------------------------------------------------
    // 3. Move candidate into the parent's slot.
    let new_dir = bots_dir.join(format!("{parent_name}_{lang}"));
    fs::rename(&candidate_dir, &new_dir).with_context(|| {
        format!(
            "renaming {} → {} (candidate → parent slot)",
            candidate_dir.display(),
            new_dir.display(),
        )
    })?;
    let old_crate = format!("{game}_{candidate}_{lang}");
    let new_crate = format!("{game}_{parent_name}_{lang}");
    rewrite_dir_contents(&new_dir, &old_crate, &new_crate)?;
    remove_workspace_member("Cargo.toml", &candidate_dir.to_string_lossy())?;
    add_workspace_member("Cargo.toml", &new_dir.to_string_lossy())?;

    // Update the promoted bot's manifest. Keep description + history;
    // re-anchor name + parent + champion per the design.
    let mut promoted_manifest = candidate_manifest.clone();
    promoted_manifest.name = parent_name.clone();
    promoted_manifest.parent = archived_parent_stem.clone();
    promoted_manifest.champion = parent_was_champion;
    promoted_manifest.write(&new_dir.join("bot.toml"))?;

    let _ = std::process::Command::new("cargo")
        .args(["clean", "-p", &old_crate])
        .output();

    println!(
        "{} Promoted {} into {} ({})",
        s.ok("✓"),
        s.name(&format!("{candidate}_{lang}")),
        s.name(&format!("{parent_name}_{lang}")),
        s.code(&new_crate),
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

fn copy_dir_substituting(src: &Path, dst: &Path, from: &str, to: &str) -> Result<()> {
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
            fs::copy(&s_path, &d_path).with_context(|| format!("copying {}", s_path.display()))?;
        }
    }
    Ok(())
}

fn is_text_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("rs")
            | Some("cpp")
            | Some("h")
            | Some("hpp")
            | Some("toml")
            | Some("md")
            | Some("txt")
    )
}

fn print_next_steps(name: &str, name_pascal: &str) {
    let s = Style::new();
    println!(
        "{} Created game {} in {} (5 crates incl. C++ bot) and updated workspace {}",
        s.ok("✓"),
        s.name(name),
        s.path(&format!("games/{name}/")),
        s.path("Cargo.toml"),
    );
    println!();
    println!("{}", s.heading("Next steps:"));
    println!(
        "  1. Fill in {} in {}",
        s.code("TurnInput/TurnOutput"),
        s.path(&format!("games/{name}/defs/src/lib.rs")),
    );
    println!(
        "     and the matching {} impls.",
        s.code("Display/FromStr/ReadFrom/WriteTo"),
    );
    println!(
        "  2. Implement {} and {} in {}.",
        s.code("Game::input_for"),
        s.code("Game::step"),
        s.path(&format!("games/{name}/game/src/lib.rs")),
    );
    println!(
        "  3. Implement {} in {}.",
        s.code("decide"),
        s.path(&format!("games/{name}/bots/baseline_rs/src/main.rs")),
    );
    println!(
        "  4. (auto-wired) Runner + tournament dispatch already updated — \
         the new game is callable as {} from both.",
        s.code(&format!("--game {name}")),
    );
    println!(
        "  5. Customise the visualiser in {}.",
        s.path(&format!("games/{name}/viz/src/main.rs")),
    );
    println!(
        "  6. (optional) C++ bot starter at {} — build with {} and pass",
        s.path(&format!("games/{name}/bots/baseline_cpp/strategy.h")),
        s.code(&format!("cargo build -p {name}_baseline_cpp")),
    );
    println!(
        "     {} to the runner.",
        s.path(&format!("target/debug/{name}_baseline_cpp")),
    );
    println!(
        "  7. Add more bots with {} (or {} to clone an existing one).",
        s.code(&format!(
            "cargo xtask new-bot --game {name} --name <bot_name> --lang both"
        )),
        s.code("--from-existing <other_bot>"),
    );
    // `name_pascal` is no longer referenced in the printed checklist
    // (step 4 used to mention `{name_pascal}Game`), but the parameter
    // is kept so future steps can reference it without churn.
    let _ = name_pascal;
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
    // xtask is run from workspace root; templates live under the
    // xtask crate at <ws>/crates/xtask/templates.
    PathBuf::from("crates/xtask/templates")
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

/// Remove a member from [workspace.members]. Counterpart to
/// `add_workspace_member` — needed by `retire` / `promote` so the
/// workspace doesn't accumulate dangling entries pointing at deleted
/// directories. Idempotent: missing entries are a no-op.
fn remove_workspace_member(workspace_toml: &str, member_path: &str) -> Result<()> {
    let content = fs::read_to_string(workspace_toml).context("reading workspace Cargo.toml")?;
    let mut doc = content
        .parse::<DocumentMut>()
        .context("parsing workspace Cargo.toml")?;

    let members = doc["workspace"]["members"]
        .as_array_mut()
        .context("[workspace.members] is not an array")?;

    let idx = members.iter().position(|v| v.as_str() == Some(member_path));
    if let Some(idx) = idx {
        members.remove(idx);
        fs::write(workspace_toml, doc.to_string())?;
    }
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
/// Insert `<crate_name>.workspace = true` into `[dependencies]` of an
/// arbitrary downstream Cargo.toml (runner, tournament, ...). Idempotent.
fn add_cargo_dep(cargo_toml: &str, crate_name: &str) -> Result<()> {
    let content =
        fs::read_to_string(cargo_toml).with_context(|| format!("reading {cargo_toml}"))?;
    let mut doc = content
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {cargo_toml}"))?;

    let deps = doc["dependencies"]
        .as_table_mut()
        .with_context(|| format!("[dependencies] not found in {cargo_toml}"))?;

    if !deps.contains_key(crate_name) {
        // `set_dotted(true)` on the inner Table tells toml_edit to render it
        // as `name.workspace = true` rather than `[dependencies.name]\nworkspace = true`.
        let mut inner = Table::new();
        inner.set_dotted(true);
        inner.insert("workspace", Item::Value(true.into()));
        deps.insert(crate_name, Item::Table(inner));
    }

    fs::write(cargo_toml, doc.to_string())?;
    Ok(())
}

/// Add a `$cb!("<name>", $crate::__games::<NamePascal>Game);` line +
/// the matching `pub use <name>_game::<NamePascal>Game;` into the
/// `for_each_game!` macro definition (and its `__games` re-export
/// block) in `crates/runner/src/lib.rs`. Idempotent: if the file
/// already references this game, no edit happens.
///
/// This is the single dispatch wiring site — both the runner CLI and
/// `tournament::run_match_named` expand `for_each_game!`, so adding
/// the entry here makes the new game visible to both.
fn wire_game_registry(lib_rs_path: &str, name: &str, name_pascal: &str) -> Result<()> {
    let src = fs::read_to_string(lib_rs_path).with_context(|| format!("reading {lib_rs_path}"))?;
    let cb_line = format!("        $cb!(\"{name}\", $crate::__games::{name_pascal}Game);");
    let use_line = format!("    pub use {name}_game::{name_pascal}Game;");
    if src.contains(&cb_line) && src.contains(&use_line) {
        return Ok(());
    }

    // The macro body's last `$cb!(...)` line and the `__games`
    // module's last `pub use ...;` line are the landmarks to insert
    // after. We find the closing `};` / `}` immediately following
    // each and insert just before it.
    let lines: Vec<&str> = src.lines().collect();
    let last_cb_idx = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("$cb!("))
        .context(
            "no `$cb!(...)` lines found inside `for_each_game!` in runner/src/lib.rs — \
             scaffolder landmark missing; wire the registry by hand",
        )?;
    let last_use_idx = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("pub use ") && l.contains("_game::"))
        .context(
            "no `pub use ..._game::...Game;` lines found inside `__games` in runner/src/lib.rs — \
             scaffolder landmark missing; wire the registry by hand",
        )?;

    let mut out = String::with_capacity(src.len() + cb_line.len() + use_line.len() + 2);
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        out.push('\n');
        if i == last_cb_idx && !src.contains(&cb_line) {
            out.push_str(&cb_line);
            out.push('\n');
        }
        if i == last_use_idx && !src.contains(&use_line) {
            out.push_str(&use_line);
            out.push('\n');
        }
    }

    fs::write(lib_rs_path, out)?;
    Ok(())
}

// ============================================================
//  Tests
// ============================================================
//  Snapshot + iterate (the `cargo xtask iterate` workflow)
// ============================================================
//
// Three commands cooperate to give a fast edit-build-compare loop:
//   * `snapshot-bots`  — captures every bot binary as the "stable"
//                        baseline (one set, always reflecting HEAD).
//   * `install-hooks`  — opt-in post-commit hook that fires
//                        `snapshot-bots --quiet` in the background.
//   * `iterate`        — rebuilds the working-tree copy of one bot,
//                        plays it against the stable counterpart.
//
// Storage layout (gitignored — see `.gitignore`):
//   .snapshots/
//   ├── stable/
//   │   ├── <crate_name>           # release binary
//   │   └── <crate_name>.toml      # commit + built_at sidecar
//   └── snapshot.log               # background-hook output

/// `<repo>/.snapshots/` — local-only cache. Lives outside `target/`
/// so `cargo clean` doesn't blow the stable baseline away. Always
/// repo-root-relative; the caller is expected to run from the
/// workspace root (every other xtask verb assumes this too).
fn snapshots_dir() -> PathBuf {
    PathBuf::from(".snapshots")
}

/// `<repo>/.snapshots/stable/` — one binary + sidecar per bot crate.
fn snapshots_stable_dir() -> PathBuf {
    snapshots_dir().join("stable")
}

/// One discovered bot crate. `crate_name` (`<game>_<bot>_<lang>`)
/// matches what cargo produces under `target/release/`. The
/// game/name/lang fields are kept alongside even though only
/// `crate_name` is read today — future verbs (per-game snapshots,
/// filtering by lang, etc.) will want them.
#[allow(dead_code)]
struct BotCrate {
    game: String,
    name: String,
    lang: String,
    crate_name: String,
}

/// Walk `games/*/bots/*/bot.toml` and collect every well-formed bot
/// crate. Used by `snapshot-bots` to build + copy the whole fleet in
/// one pass. Manifests that fail to read are silently skipped — they
/// surface as missing binaries in the snapshot output anyway.
fn discover_bot_crates() -> Result<Vec<BotCrate>> {
    let mut out = Vec::new();
    let games_dir = Path::new("games");
    let Ok(game_entries) = fs::read_dir(games_dir) else {
        return Ok(out);
    };
    for game_entry in game_entries.flatten() {
        let game_path = game_entry.path();
        if !game_path.is_dir() {
            continue;
        }
        let game = match game_path.file_name().and_then(|n| n.to_str()) {
            Some(g) => g.to_string(),
            None => continue,
        };
        let bots_dir = game_path.join("bots");
        let Ok(bot_entries) = fs::read_dir(&bots_dir) else {
            continue;
        };
        for bot_entry in bot_entries.flatten() {
            let bot_path = bot_entry.path();
            if !bot_path.is_dir() {
                continue;
            }
            let manifest_path = bot_path.join("bot.toml");
            if !manifest_path.exists() {
                continue;
            }
            let Ok(m) = BotManifest::read(&manifest_path) else {
                continue;
            };
            let crate_name = format!("{game}_{}_{}", m.name, m.lang);
            out.push(BotCrate {
                game: game.clone(),
                name: m.name,
                lang: m.lang,
                crate_name,
            });
        }
    }
    out.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
    Ok(out)
}

/// `git rev-parse --short HEAD`. Surfaces a recognisable short SHA
/// in the snapshot sidecar so `iterate` can warn when the working
/// tree's HEAD has moved past the snapshot.
fn git_head_short() -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .context("running `git rev-parse --short HEAD`")?;
    anyhow::ensure!(
        output.status.success(),
        "`git rev-parse --short HEAD` failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Hand-rolled lookup against the trivial 2-key TOML sidecar (no
/// need to drag in a TOML parser when we control both producer and
/// consumer). Format: `key = "value"` per line.
fn parse_meta_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = \"");
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

fn snapshot_bots(quiet: bool) -> Result<()> {
    let s = Style::new();
    let crates = discover_bot_crates()?;
    anyhow::ensure!(
        !crates.is_empty(),
        "no bots discovered under games/*/bots/ — nothing to snapshot",
    );

    let commit = git_head_short().unwrap_or_else(|_| "unknown".into());
    let built_at = now_rfc3339();

    let stable_dir = snapshots_stable_dir();
    fs::create_dir_all(&stable_dir)
        .with_context(|| format!("creating {}", stable_dir.display()))?;

    if !quiet {
        eprintln!("Building {} bot crates (release)...", crates.len());
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = std::process::Command::new(&cargo);
    cmd.args(["build", "--release"]);
    for c in &crates {
        cmd.arg("-p").arg(&c.crate_name);
    }
    if quiet {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }
    // Don't bail on a non-zero exit — partial success is normal
    // when one bot's source is broken mid-iteration. We snapshot
    // whatever binaries actually exist below.
    let _ = cmd.status().context("invoking cargo build")?;

    let target_release = PathBuf::from("target").join("release");
    let mut copied = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for c in &crates {
        let src = target_release.join(format!("{}{}", c.crate_name, std::env::consts::EXE_SUFFIX));
        let dst = stable_dir.join(format!("{}{}", c.crate_name, std::env::consts::EXE_SUFFIX));
        if !src.exists() {
            skipped.push(c.crate_name.clone());
            continue;
        }
        fs::copy(&src, &dst)
            .with_context(|| format!("copying {} → {}", src.display(), dst.display()))?;
        let meta = format!("commit = \"{commit}\"\nbuilt_at = \"{built_at}\"\n");
        let meta_path = stable_dir.join(format!("{}.toml", c.crate_name));
        fs::write(&meta_path, meta)
            .with_context(|| format!("writing {}", meta_path.display()))?;
        copied += 1;
    }

    if !quiet {
        eprintln!(
            "{} snapshotted {copied} bot(s) → {} (commit {commit})",
            s.ok("✓"),
            stable_dir.display(),
        );
        if !skipped.is_empty() {
            eprintln!(
                "{} skipped (binary missing — likely build failed): {}",
                s.code("warn:"),
                skipped.join(", "),
            );
        }
    }
    Ok(())
}

const POST_COMMIT_HOOK: &str = "\
#!/bin/sh
# `cargo xtask iterate` snapshot hook.
# Installed by `cargo xtask install-hooks`.
# Rebuilds every bot in release mode and copies the binaries to
# .snapshots/stable/ so `iterate` can play the working tree against
# the last committed version. Logs go to .snapshots/snapshot.log so
# commits stay silent.
mkdir -p .snapshots
nohup cargo xtask snapshot-bots --quiet >> .snapshots/snapshot.log 2>&1 &
exit 0
";

fn install_hooks(force: bool) -> Result<()> {
    let s = Style::new();
    let hook_path = PathBuf::from(".git").join("hooks").join("post-commit");
    let parent = hook_path
        .parent()
        .expect("hook path always has a parent dir");
    fs::create_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;

    if hook_path.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to replace it (or rm it manually)",
            hook_path.display(),
        );
    }
    fs::write(&hook_path, POST_COMMIT_HOOK)
        .with_context(|| format!("writing {}", hook_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms)?;
    }

    println!(
        "{} wrote {} (post-commit snapshot hook)",
        s.ok("✓"),
        s.path(&hook_path.display().to_string()),
    );
    println!(
        "  Each commit triggers `cargo xtask snapshot-bots --quiet` in the background."
    );
    println!("  Logs: {}", s.path(".snapshots/snapshot.log"));
    println!(
        "  First snapshot doesn't run automatically — kick it off once with `cargo xtask snapshot-bots`.",
    );
    Ok(())
}

fn iterate(
    game: &str,
    bot: &str,
    lang_override: Option<BotLang>,
    rounds: u32,
    bots_per_match: u32,
) -> Result<()> {
    let s = Style::new();
    let bots_dir = bots_dir(game);
    anyhow::ensure!(
        bots_dir.exists(),
        "no game at {} — is `{game}` the right name?",
        bots_dir.display(),
    );

    // Resolve which lang to play with. Same precedence as `bundle`'s
    // `resolve_bundle_lang` — explicit override wins, otherwise auto
    // by directory existence; ambiguous demands `--lang`.
    let rs_exists = bots_dir.join(format!("{bot}_rs")).exists();
    let cpp_exists = bots_dir.join(format!("{bot}_cpp")).exists();
    let lang = match (lang_override, rs_exists, cpp_exists) {
        (Some(BotLang::Rust), true, _) => "rs",
        (Some(BotLang::Cpp), _, true) => "cpp",
        (Some(BotLang::Rust), false, _) => {
            anyhow::bail!("{bot}:rs not found under {}", bots_dir.display())
        }
        (Some(BotLang::Cpp), _, false) => {
            anyhow::bail!("{bot}:cpp not found under {}", bots_dir.display())
        }
        (Some(BotLang::Both), _, _) => anyhow::bail!("`--lang both` is invalid for iterate"),
        (None, true, false) => "rs",
        (None, false, true) => "cpp",
        (None, true, true) => anyhow::bail!(
            "{bot} has both rs and cpp variants — qualify with `--lang rust` or `--lang cpp`",
        ),
        (None, false, false) => anyhow::bail!(
            "no bot at {bot}_rs or {bot}_cpp under {}",
            bots_dir.display(),
        ),
    };

    let crate_name = format!("{game}_{bot}_{lang}");
    let stable_bin = snapshots_stable_dir().join(format!("{crate_name}{}", std::env::consts::EXE_SUFFIX));
    anyhow::ensure!(
        stable_bin.exists(),
        "no stable snapshot at {} — run `cargo xtask snapshot-bots` once \
         (or install the hook via `cargo xtask install-hooks`) before iterating.",
        stable_bin.display(),
    );

    // Drift check — surfaces "hook didn't fire" / "you forgot to
    // commit" / "stable is way behind" without forcing a specific
    // rule. Just shows both SHAs and lets the user judge.
    let meta_path = snapshots_stable_dir().join(format!("{crate_name}.toml"));
    if let Ok(text) = fs::read_to_string(&meta_path) {
        let stable_commit = parse_meta_field(&text, "commit").unwrap_or_else(|| "?".into());
        let head = git_head_short().unwrap_or_else(|_| "?".into());
        if stable_commit == head {
            println!(
                "stable: commit {} (matches HEAD)",
                s.code(&stable_commit),
            );
        } else {
            // Could be either direction — hook hasn't fired yet
            // (stable behind), or you reset / checked out past it
            // (stable ahead). Show both SHAs and let the user judge.
            println!(
                "{} stable: commit {} | HEAD: {} — snapshot doesn't match HEAD",
                s.code("note:"),
                s.code(&stable_commit),
                s.code(&head),
            );
        }
    }

    // Build the working-tree binary so tournament `--no-build` can
    // pick it up from `target/release/`.
    println!("Building working-tree {crate_name} (release)...");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = std::process::Command::new(&cargo)
        .args(["build", "--release", "-p", &crate_name])
        .status()
        .context("invoking cargo build")?;
    anyhow::ensure!(status.success(), "cargo build failed");

    // Shell out to the tournament binary using its `@stable`
    // qualifier so the resolver looks under `.snapshots/stable/` instead
    // of `target/release/` for the stable variant.
    println!(
        "Running tournament: {bot}:{lang} vs {bot}:{lang}@stable, {rounds} round(s) × seat-rotation...",
    );
    let bot_spec = format!("{bot}:{lang}");
    let stable_spec = format!("{bot_spec}@stable");
    let status = std::process::Command::new(&cargo)
        .args(["run", "--release", "--quiet", "-p", "tournament", "--"])
        .arg("compare")
        .args(["--game", game])
        .args(["--rounds", &rounds.to_string()])
        .args(["--bots-per-match", &bots_per_match.to_string()])
        .arg("--no-build")
        .arg(&bot_spec)
        .arg(&stable_spec)
        .status()
        .context("invoking tournament")?;
    anyhow::ensure!(status.success(), "tournament failed");
    Ok(())
}

// ============================================================
//  Tests
// ============================================================
//
// Tests target the read-only lineage helpers (`find_children`,
// `find_descendants`, `list_champions`, `find_champion`) and
// the content-rewriting helper (`rewrite_dir_contents`). The verb
// orchestrators themselves (retire/promote/compare) shell out to
// `cargo clean` and mutate workspace files, which is awkward to
// reproduce in a unit test — but those orchestrators are mostly
// glue over these helpers, so testing the helpers covers the
// load-bearing logic.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a bots-dir layout in a tempdir from a list of
    /// `(dir_name, bot.toml contents)` tuples. Returns the
    /// owning TempDir so the caller controls cleanup.
    fn fixture(bots: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, body) in bots {
            let d = dir.path().join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("bot.toml"), body).unwrap();
        }
        dir
    }

    fn manifest(name: &str, lang: &str, parent: Option<&str>, champion: bool) -> String {
        let mut s = format!("name = \"{name}\"\nlang = \"{lang}\"\n");
        if let Some(p) = parent {
            s.push_str(&format!("parent = \"{p}\"\n"));
        }
        s.push_str(&format!(
            "description = \"\"\nchampion = {}\n",
            if champion { "true" } else { "false" },
        ));
        s
    }

    // ---- find_children -----------------------------------------

    #[test]
    fn find_children_empty() {
        let t = fixture(&[("baseline_cpp", &manifest("baseline", "cpp", None, true))]);
        let kids = find_children(t.path(), "baseline", "cpp").unwrap();
        assert!(kids.is_empty());
    }

    #[test]
    fn find_children_one_lang_lane_only() {
        // v1_cpp has two cpp children + one rs "sibling" that should
        // be ignored (different lang lane).
        let t = fixture(&[
            ("v1_cpp", &manifest("v1", "cpp", None, true)),
            ("v1_5_cpp", &manifest("v1_5", "cpp", Some("v1"), false)),
            (
                "v1_some_algo_cpp",
                &manifest("v1_some_algo", "cpp", Some("v1"), false),
            ),
            // Different lang — shouldn't appear in cpp children of v1.
            ("v1_rs", &manifest("v1", "rs", None, false)),
            ("v1_5_rs", &manifest("v1_5", "rs", Some("v1"), false)),
        ]);
        let mut kids = find_children(t.path(), "v1", "cpp").unwrap();
        kids.sort();
        assert_eq!(kids, vec!["v1_5_cpp", "v1_some_algo_cpp"]);
    }

    #[test]
    fn find_children_skips_dirs_without_manifest() {
        let t = TempDir::new().unwrap();
        std::fs::create_dir_all(t.path().join("v1_cpp")).unwrap();
        std::fs::write(
            t.path().join("v1_cpp/bot.toml"),
            manifest("v1", "cpp", None, true),
        )
        .unwrap();
        // A directory matching the naming convention but with no bot.toml
        // — should be silently skipped, not error.
        std::fs::create_dir_all(t.path().join("orphan_cpp")).unwrap();
        let kids = find_children(t.path(), "v1", "cpp").unwrap();
        assert!(kids.is_empty());
    }

    // ---- find_descendants --------------------------------------

    #[test]
    fn find_descendants_transitive_deepest_first() {
        // Tree:
        //   v1 — v1_a — v1_a_smaller
        //      \ v1_b
        let t = fixture(&[
            ("v1_cpp", &manifest("v1", "cpp", None, true)),
            ("v1_a_cpp", &manifest("v1_a", "cpp", Some("v1"), false)),
            (
                "v1_a_smaller_cpp",
                &manifest("v1_a_smaller", "cpp", Some("v1_a"), false),
            ),
            ("v1_b_cpp", &manifest("v1_b", "cpp", Some("v1"), false)),
        ]);
        let descs = find_descendants(t.path(), "v1", "cpp").unwrap();
        // Deepest-first ordering: v1_a_smaller must come before v1_a.
        let smaller = descs.iter().position(|s| s == "v1_a_smaller").unwrap();
        let a = descs.iter().position(|s| s == "v1_a").unwrap();
        assert!(
            smaller < a,
            "deepest-first ordering: smaller={smaller} a={a}"
        );
        // All three direct + indirect descendants present.
        let set: std::collections::BTreeSet<&str> = descs.iter().map(String::as_str).collect();
        assert!(set.contains("v1_a"));
        assert!(set.contains("v1_a_smaller"));
        assert!(set.contains("v1_b"));
    }

    // ---- list_champions ----------------------------------------

    #[test]
    fn list_champions_one_per_lang() {
        let t = fixture(&[
            ("baseline_rs", &manifest("baseline", "rs", None, true)),
            ("v1_cpp", &manifest("v1", "cpp", None, true)),
            ("v1_5_cpp", &manifest("v1_5", "cpp", Some("v1"), false)),
        ]);
        let champs = list_champions(t.path()).unwrap();
        // Sorted lexicographically on (name, lang) — "baseline" < "v1".
        assert_eq!(
            champs,
            vec![
                ("baseline".to_string(), "rs".to_string()),
                ("v1".to_string(), "cpp".to_string()),
            ],
        );
    }

    #[test]
    fn list_champions_none() {
        let t = fixture(&[("v1_cpp", &manifest("v1", "cpp", None, false))]);
        let champs = list_champions(t.path()).unwrap();
        assert!(champs.is_empty());
    }

    // ---- rewrite_dir_contents -------------------------------------

    #[test]
    fn rewrite_dir_contents_text_files_only() {
        let t = TempDir::new().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "use fb_v1_cpp::decide;\n// fb_v1_cpp::on_init\n",
        )
        .unwrap();
        std::fs::write(src.join("config.toml"), "name = \"fb_v1_cpp\"\n").unwrap();
        // A non-text file — bytes that aren't valid UTF-8.
        std::fs::write(src.join("blob.bin"), [0xff_u8, 0x00, 0xff]).unwrap();

        rewrite_dir_contents(&src, "fb_v1_cpp", "fb_v2_cpp").unwrap();

        let lib = std::fs::read_to_string(src.join("lib.rs")).unwrap();
        assert_eq!(lib, "use fb_v2_cpp::decide;\n// fb_v2_cpp::on_init\n");
        let toml = std::fs::read_to_string(src.join("config.toml")).unwrap();
        assert_eq!(toml, "name = \"fb_v2_cpp\"\n");
        // Binary blob untouched (skipped by is_text_file).
        let blob = std::fs::read(src.join("blob.bin")).unwrap();
        assert_eq!(blob, vec![0xff, 0x00, 0xff]);
    }

    #[test]
    fn rewrite_dir_contents_noop_when_no_match() {
        let t = TempDir::new().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("x.txt"), "unrelated content\n").unwrap();
        rewrite_dir_contents(&src, "absent", "other").unwrap();
        assert_eq!(
            std::fs::read_to_string(src.join("x.txt")).unwrap(),
            "unrelated content\n",
        );
    }

    // ---- archive_timestamp ----------------------------------------

    #[test]
    fn archive_timestamp_shape() {
        let ts = archive_timestamp();
        // YYYYMMDD_HHMMSS — 15 chars exactly, with an underscore at index 8.
        assert_eq!(ts.len(), 15, "ts = {ts}");
        assert_eq!(ts.chars().nth(8), Some('_'));
        // All non-underscore positions are digits.
        for (i, c) in ts.chars().enumerate() {
            if i == 8 {
                continue;
            }
            assert!(c.is_ascii_digit(), "non-digit at {i}: {c}");
        }
    }
}
