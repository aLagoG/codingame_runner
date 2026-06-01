//! Generic replay visualizer built on macroquad.
//!
//! A per-game crate implements [`Visualize`] for a marker type, then calls
//! [`run`] from a `#[macroquad::main]` entry point. `run` owns the playback
//! loop (play/pause/step/scrub, timing, keyboard shortcuts) and hands the
//! current frame back to the per-game `draw` implementation, which uses
//! macroquad directly to render the game state.
//!
//! Controls are drawn with `egui` (via `egui-macroquad`) and overlay a bottom
//! panel on top of the macroquad scene.

use anyhow::Context;
use macroquad::prelude::*;

// Re-exported so per-game viz crates can reference them through `viz::*`
// without adding their own deps, and stay in lockstep with whatever
// `egui-macroquad` / `common` pull in.
pub use common::engine::{Game, GameRng, GameRngSeed};
pub use egui_macroquad::egui;
pub use macroquad;

/// Viz-side replay with **typed** per-tick outputs. The engine's own
/// `common::engine::Replay` stores outputs as wire-format strings
/// (so per-game `TurnOutput` types stay free of `serde_derive` and
/// flattened bots stay vendor-clean for CodinGame); viz wants the
/// typed view because the per-game `draw` / `step` paths operate on
/// `G::Output`, not strings. `load_replay_from_argv` parses the
/// engine-side replay into this typed form at load time.
#[derive(Debug, Clone)]
pub struct Replay<O> {
    pub seed: u64,
    pub num_players: u32,
    pub outputs: Vec<Vec<Option<O>>>,
}

/// One game's bridge into the playback engine.
///
/// Implement on a marker type local to the per-game viz crate so the orphan
/// rules let you reference a foreign `Game` type (e.g. `TicTacToeGame` from
/// `tictactoe_game`).
pub trait Visualize {
    /// The game whose live state this visualizer renders. `viz::run` steps an
    /// instance of this type forward through the replay; render methods
    /// receive a `&Self::Game` showing the state at the currently-viewed tick.
    type Game: Game;

    /// Logical board size in cells. Used to fit the board to the window.
    fn grid_size() -> (u32, u32);

    /// Draw the game at the current tick. `grid` carries the pixel geometry
    /// the engine picked for this window size; use its helpers for
    /// cell-relative positioning.
    fn draw(game: &Self::Game, grid: &CellGrid);

    /// Short human-readable status string shown above the controls.
    fn status(_game: &Self::Game) -> String {
        String::new()
    }

    /// Right-side panel for game-specific stats (per-player chips, scores, …).
    /// Default is empty.
    fn side_panel(_game: &Self::Game, _ui: &mut egui::Ui) {}

    /// Bottom panel (above the controls) for per-tick per-player info — e.g.
    /// the move each player submitted this tick. Default is empty.
    ///
    /// `ctx` carries the displayed tick and the full replay so the panel can
    /// look at the just-played move (`ctx.replay.outputs[ctx.current_tick - 1]`)
    /// without TronGame having to retain a `last_moves` mirror.
    fn bottom_panel(_game: &Self::Game, _ctx: &VizCtx<'_, Self>, _ui: &mut egui::Ui) {}
}

/// Per-frame context passed to panel methods that need to look beyond the
/// current `Game` snapshot — typically the just-played move(s). Kept narrow
/// on purpose: only what panels actually need today (the tick the viz is
/// currently showing + the full replay it was built from).
pub struct VizCtx<'a, V: Visualize + ?Sized> {
    /// Number of steps applied to reach the displayed state. `0` means the
    /// pre-game initial state (no moves yet); `N` means after step N-1 has
    /// been applied. Use `current_tick.checked_sub(1)` to index into
    /// `replay.outputs` for the most recent move.
    pub current_tick: usize,
    pub replay: &'a Replay<<V::Game as Game>::Output>,
}

/// Pixel geometry of the rendered board for the current frame.
pub struct CellGrid {
    pub cell_px: f32,
    pub origin: Vec2,
    pub width: u32,
    pub height: u32,
}

impl CellGrid {
    pub fn cell_rect(&self, row: i32, col: i32) -> (f32, f32, f32, f32) {
        (
            self.origin.x + col as f32 * self.cell_px,
            self.origin.y + row as f32 * self.cell_px,
            self.cell_px,
            self.cell_px,
        )
    }

    pub fn cell_center(&self, row: i32, col: i32) -> Vec2 {
        vec2(
            self.origin.x + (col as f32 + 0.5) * self.cell_px,
            self.origin.y + (row as f32 + 0.5) * self.cell_px,
        )
    }

    pub fn draw_grid_lines(&self, color: Color, thickness: f32) {
        let w = self.cell_px * self.width as f32;
        let h = self.cell_px * self.height as f32;
        for i in 0..=self.width {
            let x = self.origin.x + i as f32 * self.cell_px;
            draw_line(x, self.origin.y, x, self.origin.y + h, thickness, color);
        }
        for i in 0..=self.height {
            let y = self.origin.y + i as f32 * self.cell_px;
            draw_line(self.origin.x, y, self.origin.x + w, y, thickness, color);
        }
    }
}

const CONTROLS_H: f32 = 110.0;
const PLAYERS_H: f32 = 90.0;
const SIDE_W: f32 = 220.0;
const MARGIN: f32 = 20.0;
const BG: Color = Color::new(0.08, 0.08, 0.12, 1.0);

struct State {
    tick: usize,
    playing: bool,
    fps: f32,
    accum: f32,
}

/// Run the playback loop. Call from inside a `#[macroquad::main]` entry point.
pub async fn run<V: Visualize>(replay: Replay<<V::Game as Game>::Output>) -> anyhow::Result<()> {
    // One displayable state per tick + the pre-game initial state at tick 0.
    let n_states = replay.outputs.len() + 1;

    let mut state = State {
        tick: 0,
        playing: false,
        fps: 4.0,
        accum: 0.0,
    };

    let mut game = build_game::<V>(&replay);
    let mut game_tick: usize = 0;

    // Intercept window-close so we can return cleanly instead of macroquad
    // calling exit(0) and skipping caller-side teardown.
    prevent_quit();

    loop {
        if is_quit_requested() {
            return Ok(());
        }
        clear_background(BG);
        advance(&mut state, n_states);
        handle_keys(&mut state, n_states);

        sync_game::<V>(&mut game, &mut game_tick, &replay, state.tick);

        let grid = fit_grid::<V>();
        V::draw(&game, &grid);

        let status = V::status(&game);
        egui_macroquad::ui(|ctx| {
            build_panels::<V>(ctx, &mut state, n_states, &game, &status, &replay);
        });
        egui_macroquad::draw();

        next_frame().await;
    }
}

/// Step the working `game` forward (or rebuild from scratch and step) until
/// it sits at `target_tick`. Forward scrubs are O(Δticks); backward scrubs
/// rebuild from `Game::new` and step from 0.
fn sync_game<V: Visualize>(
    game: &mut V::Game,
    current: &mut usize,
    replay: &Replay<<V::Game as Game>::Output>,
    target: usize,
) {
    if target < *current {
        *game = build_game::<V>(replay);
        *current = 0;
    }
    while *current < target {
        let _ = game.step(&replay.outputs[*current]);
        *current += 1;
    }
}

/// Build a fresh `V::Game` from a replay — same pattern the runner
/// uses in `run_match`: reconstruct an `StdRng` from the replay's
/// seed and pass it to `Game::new`. Centralised here so the two
/// call sites (initial build + backward-scrub rebuild) stay in
/// lockstep.
fn build_game<V: Visualize>(replay: &Replay<<V::Game as Game>::Output>) -> V::Game {
    let mut rng = GameRng::seed_from_u64(replay.seed);
    V::Game::new(replay.num_players, &mut rng)
}

fn fit_grid<V: Visualize>() -> CellGrid {
    let (gw, gh) = V::grid_size();
    // Game area = full window minus the side panel (right), the bottom panels
    // (controls + players), and a margin on each remaining side.
    let area_w = screen_width() - SIDE_W;
    let area_h = screen_height() - CONTROLS_H - PLAYERS_H;
    let avail_w = area_w - 2.0 * MARGIN;
    let avail_h = area_h - 2.0 * MARGIN;
    let cell_px = (avail_w / gw as f32).min(avail_h / gh as f32);
    let board_w = cell_px * gw as f32;
    let board_h = cell_px * gh as f32;
    let origin = vec2(
        MARGIN + (avail_w - board_w) / 2.0,
        MARGIN + (avail_h - board_h) / 2.0,
    );
    CellGrid {
        cell_px,
        origin,
        width: gw,
        height: gh,
    }
}

fn advance(s: &mut State, n: usize) {
    if !s.playing {
        s.accum = 0.0;
        return;
    }
    s.accum += get_frame_time();
    let dt = 1.0 / s.fps;
    while s.accum >= dt {
        s.accum -= dt;
        if s.tick + 1 < n {
            s.tick += 1;
        } else {
            s.playing = false;
            break;
        }
    }
}

fn handle_keys(s: &mut State, n: usize) {
    if is_key_pressed(KeyCode::Space) {
        s.playing = !s.playing;
    }
    if is_key_pressed(KeyCode::Left) && s.tick > 0 {
        s.tick -= 1;
        s.playing = false;
    }
    if is_key_pressed(KeyCode::Right) && s.tick + 1 < n {
        s.tick += 1;
        s.playing = false;
    }
    if is_key_pressed(KeyCode::R) {
        s.tick = 0;
        s.playing = false;
    }
    if is_key_pressed(KeyCode::Up) {
        s.fps = (s.fps * 1.5).min(60.0);
    }
    if is_key_pressed(KeyCode::Down) {
        s.fps = (s.fps / 1.5).max(0.5);
    }
}

fn build_panels<V: Visualize>(
    ctx: &egui::Context,
    s: &mut State,
    n: usize,
    game: &V::Game,
    status: &str,
    replay: &Replay<<V::Game as Game>::Output>,
) {
    // Order matters: each panel claims space from what's left. Bottom panels
    // first → they span the full window width. Then the side panel takes the
    // right edge of the remaining (game) area only.
    build_controls(ctx, s, n, status);
    build_bottom_panel::<V>(ctx, game, s.tick, replay);
    build_side_panel::<V>(ctx, game);
}

fn build_controls(ctx: &egui::Context, s: &mut State, n: usize, status: &str) {
    egui::TopBottomPanel::bottom("controls")
        .min_height(CONTROLS_H)
        .max_height(CONTROLS_H)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("⏮").on_hover_text("first tick").clicked() {
                    s.tick = 0;
                    s.playing = false;
                }
                if ui.button("◀").on_hover_text("step back").clicked() && s.tick > 0 {
                    s.tick -= 1;
                    s.playing = false;
                }
                if ui
                    .button("⏯")
                    .on_hover_text("play / pause (space)")
                    .clicked()
                {
                    s.playing = !s.playing;
                }
                if ui.button("▶|").on_hover_text("step forward").clicked() && s.tick + 1 < n {
                    s.tick += 1;
                    s.playing = false;
                }
                if ui.button("⏭").on_hover_text("last tick").clicked() {
                    s.tick = n - 1;
                    s.playing = false;
                }

                ui.separator();

                let max_tick = n.saturating_sub(1);
                let mut t = s.tick;
                if ui
                    .add(egui::Slider::new(&mut t, 0..=max_tick).text("tick"))
                    .changed()
                {
                    s.tick = t;
                    s.playing = false;
                }

                ui.add(
                    egui::Slider::new(&mut s.fps, 0.5..=60.0)
                        .logarithmic(true)
                        .text("fps"),
                );
            });

            ui.add_space(4.0);
            ui.label(egui::RichText::new(status).strong());
            ui.weak("[space] play/pause   [⬅ ⮕] step   [⬆ ⬇] speed   [R] reset");
        });
}

fn build_bottom_panel<V: Visualize>(
    ctx: &egui::Context,
    game: &V::Game,
    current_tick: usize,
    replay: &Replay<<V::Game as Game>::Output>,
) {
    egui::TopBottomPanel::bottom("players")
        .min_height(PLAYERS_H)
        .max_height(PLAYERS_H)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.heading("This tick");
            ui.separator();
            let viz_ctx = VizCtx {
                current_tick,
                replay,
            };
            V::bottom_panel(game, &viz_ctx, ui);
        });
}

fn build_side_panel<V: Visualize>(ctx: &egui::Context, game: &V::Game) {
    egui::SidePanel::right("stats")
        .resizable(false)
        .min_width(SIDE_W)
        .max_width(SIDE_W)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Players");
            ui.separator();
            V::side_panel(game, ui);
        });
}

/// Default 4-player palette — blue, red, green, yellow. Games with > 4
/// players (or specific aesthetic needs) can ignore it and use their own.
pub const PALETTE: [Color; 4] = [
    Color::new(0.30, 0.65, 1.00, 1.0),
    Color::new(1.00, 0.40, 0.40, 1.0),
    Color::new(0.40, 0.90, 0.50, 1.0),
    Color::new(1.00, 0.85, 0.30, 1.0),
];

/// Convert a macroquad `Color` to an egui `Color32` (preserves alpha).
pub fn to_egui(c: Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (c.r * 255.0) as u8,
        (c.g * 255.0) as u8,
        (c.b * 255.0) as u8,
        (c.a * 255.0) as u8,
    )
}

/// Draw a small filled square in the current egui layout — handy for
/// "player N is this color" legends inside side / bottom panels.
pub fn color_chip(ui: &mut egui::Ui, color: Color) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, to_egui(color));
}

/// If `argv[1]` is set, read a framed replay from it; otherwise return `None`
/// so the caller can fall through to a built-in demo.
pub fn load_replay_from_argv<G: Game>() -> anyhow::Result<Option<Replay<G::Output>>> {
    let Some(path) = std::env::args().nth(1) else {
        return Ok(None);
    };
    let mut file = std::fs::File::open(&path).with_context(|| format!("opening replay {path}"))?;
    let raw = common::engine::read_replay::<G>(&mut file)?;
    let outputs = raw
        .parse_outputs::<G::Output>()
        .with_context(|| format!("parsing typed outputs out of replay {path}"))?;
    Ok(Some(Replay {
        seed: raw.seed,
        num_players: raw.num_players,
        outputs,
    }))
}

/// Per-game viz binaries collapse to:
///
/// ```ignore
/// viz::run_viz!(TronViz, demo_replay());
/// ```
///
/// Generates `fn main()` that creates a macroquad window titled with
/// `G::NAME`, loads the replay from `argv[1]` (or evaluates `$demo` if no
/// path was given), and runs the playback loop. Errors print to stderr and
/// exit non-zero.
#[macro_export]
macro_rules! run_viz {
    ($viz:ty, $demo:expr $(,)?) => {
        fn main() {
            $crate::macroquad::Window::new(
                <<$viz as $crate::Visualize>::Game as $crate::Game>::NAME,
                async move {
                    let replay = match $crate::load_replay_from_argv::<
                        <$viz as $crate::Visualize>::Game,
                    >() {
                        Ok(Some(r)) => r,
                        Ok(None) => $demo,
                        Err(e) => {
                            ::std::eprintln!("error: {e:#}");
                            ::std::process::exit(1);
                        }
                    };
                    if let Err(e) = $crate::run::<$viz>(replay).await {
                        ::std::eprintln!("viz error: {e:#}");
                        ::std::process::exit(1);
                    }
                },
            );
        }
    };
}
