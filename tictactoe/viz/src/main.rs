use macroquad::prelude::*;
use tictactoe_defs::{Cell, Pos, TurnOutput};
use tictactoe_game::TicTacToeGame;
use viz::{CellGrid, Replay, Visualize, VizCtx, color_chip, egui, to_egui};

const X_COLOR: Color = Color::new(0.30, 0.70, 1.00, 1.0);
const O_COLOR: Color = Color::new(1.00, 0.40, 0.40, 1.0);

struct TicTacToeViz;

impl Visualize for TicTacToeViz {
    type Game = TicTacToeGame;

    fn grid_size() -> (u32, u32) {
        (3, 3)
    }

    fn draw(game: &TicTacToeGame, grid: &CellGrid) {
        grid.draw_grid_lines(Color::new(0.4, 0.4, 0.5, 1.0), 3.0);

        let mark_r = grid.cell_px * 0.32;
        let board = game.board();
        for row in 0..3 {
            for col in 0..3 {
                let c = grid.cell_center(row, col);
                match board[(row * 3 + col) as usize] {
                    Cell::Empty => {}
                    Cell::X => draw_x(c, mark_r),
                    Cell::O => draw_o(c, mark_r),
                }
            }
        }

        if let Some((_, pos)) = game.last_move() {
            let (x, y, w, h) = grid.cell_rect(pos.row, pos.col);
            draw_rectangle_lines(x, y, w, h, 5.0, YELLOW);
        }
    }

    fn status(game: &TicTacToeGame) -> String {
        if let Some(outcome) = game.outcome() {
            return match outcome.winner {
                Some(0) => "X wins".into(),
                Some(1) => "O wins".into(),
                Some(p) => format!("player {p} wins"),
                None => "draw".into(),
            };
        }
        match game.next_player() {
            Some(0) => "X to play".into(),
            Some(1) => "O to play".into(),
            Some(p) => format!("player {p} to play"),
            None => String::new(),
        }
    }

    fn side_panel(game: &TicTacToeGame, ui: &mut egui::Ui) {
        let board = game.board();
        let x_count = board.iter().filter(|&&c| c == Cell::X).count();
        let o_count = board.iter().filter(|&&c| c == Cell::O).count();
        let empty = board.iter().filter(|&&c| c == Cell::Empty).count();

        for (pid, count) in [(0u32, x_count), (1, o_count)] {
            ui.horizontal(|ui| {
                ui.colored_label(to_egui(player_color(pid)), "■");
                ui.label(player_label(pid));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.strong(format!("{count}"));
                });
            });
        }
        ui.separator();
        ui.weak(format!("{empty} empty"));
    }

    fn bottom_panel(game: &TicTacToeGame, _ctx: &VizCtx<'_, Self>, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for pid in 0..2u32 {
                color_chip(ui, player_color(pid));
                ui.label(format!("{}:", player_label(pid)));
                let played = match game.last_move() {
                    Some((p, pos)) if p == pid => format!("({}, {})", pos.row, pos.col),
                    _ => "—".into(),
                };
                ui.strong(played);
                ui.add_space(24.0);
            }
        });
    }
}

fn player_color(pid: u32) -> Color {
    match pid {
        0 => X_COLOR,
        _ => O_COLOR,
    }
}

fn player_label(pid: u32) -> &'static str {
    match pid {
        0 => "X (P0)",
        _ => "O (P1)",
    }
}

fn draw_x(c: Vec2, r: f32) {
    draw_line(c.x - r, c.y - r, c.x + r, c.y + r, 8.0, X_COLOR);
    draw_line(c.x - r, c.y + r, c.x + r, c.y - r, 8.0, X_COLOR);
}

fn draw_o(c: Vec2, r: f32) {
    draw_circle_lines(c.x, c.y, r, 8.0, O_COLOR);
}

/// Demo replay: X plays the top row, O blocks down the middle column — X wins.
fn demo_replay() -> Replay<TurnOutput> {
    let moves = [(0, 0), (1, 1), (0, 1), (2, 1), (0, 2)];
    let outputs: Vec<Vec<Option<TurnOutput>>> = moves
        .iter()
        .enumerate()
        .map(|(i, &(row, col))| {
            let mut tick = vec![None, None];
            tick[i % 2] = Some(TurnOutput {
                pos: Pos { row, col },
            });
            tick
        })
        .collect();

    Replay {
        seed: 0,
        num_players: 2,
        outputs,
    }
}

viz::run_viz!(TicTacToeViz, demo_replay());
