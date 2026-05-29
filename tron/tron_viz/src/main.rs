use macroquad::prelude::*;
use tron_defs::{Direction, TurnOutput};
use tron_game::TronGame;
use viz::{CellGrid, PALETTE, Replay, VizCtx, Visualize, color_chip, egui};

struct TronViz;

impl Visualize for TronViz {
    type Game = TronGame;

    fn grid_size() -> (u32, u32) {
        (30, 20)
    }

    fn draw(game: &TronGame, grid: &CellGrid) {
        grid.draw_grid_lines(Color::new(0.15, 0.15, 0.2, 1.0), 1.0);

        // Trails first so heads draw on top. Walk the board
        // row-major. Dead players' cells are cleared by the engine
        // on death (see `tron_game::step`), so we never see them
        // here — every `Some(pid)` is an alive player.
        for (y, row) in game.board().iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let Some(pid) = *cell else { continue };
                let pid_us = pid as usize;
                let mut color = PALETTE[pid_us % PALETTE.len()];
                color.a = 0.7;
                let (rx, ry, rw, rh) = grid.cell_rect(y as i32, x as i32);
                draw_rectangle(rx + 1.0, ry + 1.0, rw - 2.0, rh - 2.0, color);
            }
        }

        // Heads — full-bright square + a contrasting border. Dead players
        // get a white X at the last position instead.
        for (pid, head) in game.heads().iter().enumerate() {
            if !game.alive()[pid] {
                let c = grid.cell_center(head.y, head.x);
                let r = grid.cell_px * 0.3;
                draw_line(c.x - r, c.y - r, c.x + r, c.y + r, 3.0, WHITE);
                draw_line(c.x - r, c.y + r, c.x + r, c.y - r, 3.0, WHITE);
                continue;
            }
            let color = PALETTE[pid % PALETTE.len()];
            let (x, y, w, h) = grid.cell_rect(head.y, head.x);
            draw_rectangle(x, y, w, h, color);
            draw_rectangle_lines(x, y, w, h, 2.0, WHITE);
        }
    }

    fn status(game: &TronGame) -> String {
        let alive: Vec<usize> = (0..game.alive().len())
            .filter(|&i| game.alive()[i])
            .collect();
        match alive.len() {
            0 => "draw".into(),
            1 => format!("player {} wins", alive[0]),
            n => format!("{n} alive"),
        }
    }

    fn side_panel(game: &TronGame, ui: &mut egui::Ui) {
        for pid in 0..game.alive().len() {
            ui.horizontal(|ui| {
                color_chip(ui, PALETTE[pid % PALETTE.len()]);
                ui.label(format!("Player {pid}"));
                if game.alive()[pid] {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "alive");
                } else {
                    ui.colored_label(egui::Color32::LIGHT_RED, "dead");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Only alive players have any cells on the
                    // board — the engine clears a dead player's
                    // trail at death, so "trail 0" for a dead pid
                    // would be misleading. Skip the count entirely.
                    if game.alive()[pid] {
                        let pid_u32 = pid as u32;
                        let cells: usize = game
                            .board()
                            .iter()
                            .map(|row| row.iter().filter(|c| **c == Some(pid_u32)).count())
                            .sum();
                        ui.weak(format!("trail {cells}"));
                    }
                });
            });
        }
    }

    fn bottom_panel(_game: &TronGame, ctx: &VizCtx<'_, Self>, ui: &mut egui::Ui) {
        // Tron is sequential — exactly one player moves per tick — so the
        // most recent move is the unique non-None entry in the previous
        // tick's output row. No walk-back over ticks, no per-player mirror
        // on the game struct.
        let Some(prev_tick) = ctx.current_tick.checked_sub(1) else {
            return;
        };
        let Some((pid, output)) = ctx.replay.outputs[prev_tick]
            .iter()
            .enumerate()
            .find_map(|(i, o)| o.as_ref().map(|o| (i, o)))
        else {
            return;
        };
        ui.horizontal(|ui| {
            color_chip(ui, PALETTE[pid % PALETTE.len()]);
            ui.label(format!("P{pid}:"));
            ui.strong(format!("{:?}", output.direction).to_uppercase());
        });
    }
}

/// Built-in demo. We can't hard-code per-player move scripts because
/// `TronGame::new` picks random spawn positions (deterministic per seed,
/// but not necessarily at the corners). Instead, we run a real engine
/// match here with a trivial "first safe direction" policy and record
/// its outputs. The same `SEED` is stored in the resulting `Replay`, so
/// `build_game` in viz reconstructs the identical starting board.
fn demo_replay() -> Replay<TurnOutput> {
    use Direction::*;
    use viz::{Game, GameRng, GameRngSeed};

    const SEED: u64 = 0;
    const NUM_PLAYERS: u32 = 4;
    const WIDTH: i32 = 30;
    const HEIGHT: i32 = 20;
    // Per-player direction preference. The active player picks the first
    // safe entry; differing orderings keep the four trails visually
    // distinct instead of all hugging the same edge.
    const PREFS: [[Direction; 4]; 4] = [
        [Right, Down, Left, Up],
        [Left, Up, Right, Down],
        [Right, Up, Left, Down],
        [Left, Down, Right, Up],
    ];

    let mut rng = GameRng::seed_from_u64(SEED);
    let mut game = TronGame::new(NUM_PLAYERS, &mut rng);
    let mut outputs_per_tick: Vec<Vec<Option<TurnOutput>>> = Vec::new();

    while let Some(&p) = game.active_players().first() {
        let pidx = p as usize;
        let head = game.heads()[pidx];
        let board = game.board();
        let chosen = PREFS[pidx].iter().copied().find(|&d| {
            let (dx, dy) = match d {
                Up => (0, -1),
                Down => (0, 1),
                Left => (-1, 0),
                Right => (1, 0),
            };
            let nx = head.x + dx;
            let ny = head.y + dy;
            nx >= 0
                && nx < WIDTH
                && ny >= 0
                && ny < HEIGHT
                && board[ny as usize][nx as usize].is_none()
        });
        let mut tick_out: Vec<Option<TurnOutput>> =
            (0..NUM_PLAYERS).map(|_| None).collect();
        tick_out[pidx] = chosen.map(|d| TurnOutput { direction: d });
        let outcome = game.step(&tick_out);
        outputs_per_tick.push(tick_out);
        if outcome.is_some() {
            break;
        }
    }

    Replay {
        seed: SEED,
        num_players: NUM_PLAYERS,
        outputs: outputs_per_tick,
    }
}

viz::run_viz!(TronViz, demo_replay());
