use anyhow::Context;
use common::engine::Replay;
use macroquad::prelude::*;
use tron_defs::{Direction, TurnOutput};
use tron_game::TronGame;
use viz::{CellGrid, Visualize, egui};

const PALETTE: [Color; 4] = [
    Color::new(0.30, 0.65, 1.00, 1.0), // blue
    Color::new(1.00, 0.40, 0.40, 1.0), // red
    Color::new(0.40, 0.90, 0.50, 1.0), // green
    Color::new(1.00, 0.85, 0.30, 1.0), // yellow
];

struct TronViz;

impl Visualize for TronViz {
    type Game = TronGame;

    fn grid_size() -> (u32, u32) {
        (30, 20)
    }

    fn draw(game: &TronGame, grid: &CellGrid) {
        grid.draw_grid_lines(Color::new(0.15, 0.15, 0.2, 1.0), 1.0);

        // Trails first so heads draw on top.
        for (pid, trail) in game.trails().iter().enumerate() {
            let mut color = PALETTE[pid % PALETTE.len()];
            color.a = if game.alive()[pid] { 0.7 } else { 0.35 };
            for p in trail {
                let (x, y, w, h) = grid.cell_rect(p.y, p.x);
                draw_rectangle(x + 1.0, y + 1.0, w - 2.0, h - 2.0, color);
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
                    ui.weak(format!("trail {}", game.trails()[pid].len()));
                });
            });
        }
    }

    fn bottom_panel(game: &TronGame, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for pid in 0..game.last_moves().len() {
                color_chip(ui, PALETTE[pid % PALETTE.len()]);
                ui.label(format!("P{pid}:"));
                let text = match game.last_moves()[pid] {
                    Some(d) => format!("{d:?}").to_uppercase(),
                    None => "—".into(),
                };
                ui.strong(text);
                ui.add_space(16.0);
            }
        });
    }
}

fn color_chip(ui: &mut egui::Ui, color: Color) {
    let c = egui::Color32::from_rgba_unmultiplied(
        (color.r * 255.0) as u8,
        (color.g * 255.0) as u8,
        (color.b * 255.0) as u8,
        255,
    );
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, c);
}

/// Hand-rolled demo: 4 players snake inward from their corners.
fn demo_replay() -> Replay<TronGame> {
    use Direction::*;

    // Starts: 0=(0,0)  1=(29,19)  2=(0,19)  3=(29,0)
    let patterns: [&[Direction]; 4] = [
        &[Right, Right, Right, Right, Right, Down, Down, Down, Down, Down, Left, Left, Left, Left],
        &[Left, Left, Left, Left, Left, Up, Up, Up, Up, Up, Right, Right, Right, Right],
        &[Right, Right, Right, Right, Right, Up, Up, Up, Up, Up, Left, Left, Left, Left],
        &[Left, Left, Left, Left, Left, Down, Down, Down, Down, Down, Right, Right, Right, Right],
    ];

    let n = patterns[0].len();
    let outputs: Vec<Vec<Option<TurnOutput>>> = (0..n)
        .map(|i| {
            (0..4)
                .map(|p| {
                    Some(TurnOutput {
                        direction: patterns[p][i],
                    })
                })
                .collect()
        })
        .collect();

    Replay {
        seed: 0,
        num_players: 4,
        outputs,
    }
}

fn load_or_demo() -> anyhow::Result<Replay<TronGame>> {
    let Some(path) = std::env::args().nth(1) else {
        return Ok(demo_replay());
    };
    let bytes = std::fs::read(&path).with_context(|| format!("reading replay {path}"))?;
    bincode::deserialize::<Replay<TronGame>>(&bytes).context("deserializing replay")
}

#[macroquad::main("tron_viz")]
async fn main() {
    let replay = match load_or_demo() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    };
    viz::run::<TronViz>(replay).await.unwrap();
}
