//! Continuous-space viz for Fantastic Bits. Treats the macroquad grid
//! as a coarse world-to-screen scale (1 cell = 100 world units) and
//! converts manually for entity drawing.
//!
//! Status: score 0-0, magic per side, tick / max ticks.
//! Side panel: per-wizard state (position, velocity, holding, cooldown).
//! Bottom panel: most recent action for each of the four wizards.

use fantastic_bits_defs::{ActionKind, TurnOutput, WizardAction};
use fantastic_bits_game::{
    BLUDGER_RADIUS, FantasticBitsGame, GOAL_Y_BOTTOM, GOAL_Y_TOP, HEIGHT, POLE_RADIUS,
    SNAFFLE_RADIUS, WIDTH, WIZARD_RADIUS,
};
use macroquad::prelude::*;
use viz::{CellGrid, Replay, Visualize, VizCtx, color_chip, egui, to_egui};

/// Pixels per `WORLD_PER_CELL` world units. `grid_size()` returns the
/// playing field divided by this constant; the engine's `CellGrid`
/// then sizes cells to fit the window.
const WORLD_PER_CELL: f64 = 100.0;

const TEAM_COLOURS: [Color; 2] = [
    Color::new(0.30, 0.65, 1.00, 1.0), // blue
    Color::new(1.00, 0.40, 0.40, 1.0), // red
];
const SNAFFLE_COLOUR: Color = Color::new(1.00, 0.85, 0.30, 1.0); // gold
const BLUDGER_COLOUR: Color = Color::new(0.55, 0.55, 0.60, 1.0); // grey
const POST_COLOUR: Color = Color::new(0.85, 0.85, 0.95, 1.0);
const GOAL_MOUTH_COLOUR: Color = Color::new(1.00, 1.00, 1.00, 0.05);
const FIELD_OUTLINE: Color = Color::new(0.2, 0.25, 0.32, 1.0);

struct FantasticBitsViz;

impl Visualize for FantasticBitsViz {
    type Game = FantasticBitsGame;

    fn grid_size() -> (u32, u32) {
        (
            (WIDTH as f64 / WORLD_PER_CELL) as u32,
            (HEIGHT as f64 / WORLD_PER_CELL) as u32,
        )
    }

    fn draw(game: &FantasticBitsGame, grid: &CellGrid) {
        let scale = grid.cell_px / WORLD_PER_CELL as f32;
        let to_screen = |wx: f64, wy: f64| -> Vec2 {
            vec2(
                grid.origin.x + (wx as f32) * scale,
                grid.origin.y + (wy as f32) * scale,
            )
        };

        // Field outline.
        let tl = to_screen(0.0, 0.0);
        let br = to_screen(WIDTH as f64, HEIGHT as f64);
        draw_rectangle_lines(tl.x, tl.y, br.x - tl.x, br.y - tl.y, 2.0, FIELD_OUTLINE);

        // Goal mouths (semi-transparent strips on either end).
        let mouth_height = (GOAL_Y_BOTTOM - GOAL_Y_TOP) as f64;
        let left_tl = to_screen(0.0, GOAL_Y_TOP as f64);
        let mouth_w = (POLE_RADIUS as f32) * scale;
        let mouth_h = (mouth_height as f32) * scale;
        draw_rectangle(
            left_tl.x - mouth_w,
            left_tl.y,
            mouth_w,
            mouth_h,
            GOAL_MOUTH_COLOUR,
        );
        let right_tl = to_screen(WIDTH as f64, GOAL_Y_TOP as f64);
        draw_rectangle(right_tl.x, right_tl.y, mouth_w, mouth_h, GOAL_MOUTH_COLOUR);

        // Goal posts.
        for post in game.goal_posts() {
            let c = to_screen(post.pos.x, post.pos.y);
            draw_circle(c.x, c.y, (POLE_RADIUS as f32) * scale, POST_COLOUR);
        }

        // Snaffles (alive only). Held snaffles get a darker tint.
        for s in game.snaffles() {
            if !s.alive {
                continue;
            }
            let c = to_screen(s.disc.pos.x, s.disc.pos.y);
            let mut col = SNAFFLE_COLOUR;
            if s.held_by.is_some() {
                col.a = 0.45;
            }
            draw_circle(c.x, c.y, (SNAFFLE_RADIUS as f32) * scale, col);
        }

        // Bludgers.
        for b in game.bludgers() {
            let c = to_screen(b.disc.pos.x, b.disc.pos.y);
            draw_circle(c.x, c.y, (BLUDGER_RADIUS as f32) * scale, BLUDGER_COLOUR);
            draw_velocity_arrow(c, b.disc.vel.x, b.disc.vel.y, scale, BLUDGER_COLOUR);
        }

        // Wizards on top — coloured by team, with a thin holding marker.
        for w in game.wizards() {
            let team = if w.disc.id < 2 { 0 } else { 1 };
            let c = to_screen(w.disc.pos.x, w.disc.pos.y);
            let r = (WIZARD_RADIUS as f32) * scale;
            draw_circle(c.x, c.y, r, TEAM_COLOURS[team]);
            // Holding marker: a small white inner ring around the wizard.
            if w.holding.is_some() {
                draw_circle_lines(c.x, c.y, r * 0.55, 2.0, WHITE);
            }
            // Cooldown shade — a faint grey ring during the 3-tick wait
            // after release so you can see why a wizard isn't grabbing.
            if w.cooldown > 0 {
                draw_circle_lines(c.x, c.y, r + 4.0, 2.0, GRAY);
            }
            draw_velocity_arrow(c, w.disc.vel.x, w.disc.vel.y, scale, WHITE);
        }
    }

    fn status(game: &FantasticBitsGame) -> String {
        format!(
            "Score {}-{}   Magic {}/{}   Tick {}/{}   (first to {})",
            game.score()[0],
            game.score()[1],
            game.magic()[0],
            game.magic()[1],
            game.tick(),
            fantastic_bits_game::MAX_TICKS,
            game.score_to_win(),
        )
    }

    fn side_panel(game: &FantasticBitsGame, ui: &mut egui::Ui) {
        ui.heading("Score");
        ui.label(format!(
            "Team 0 (blue): {}  ·  magic {}",
            game.score()[0],
            game.magic()[0],
        ));
        ui.label(format!(
            "Team 1 (red):  {}  ·  magic {}",
            game.score()[1],
            game.magic()[1],
        ));
        ui.separator();
        ui.heading("Wizards");
        for w in game.wizards() {
            let team = if w.disc.id < 2 { 0 } else { 1 };
            let color = match team {
                0 => egui::Color32::LIGHT_BLUE,
                _ => egui::Color32::LIGHT_RED,
            };
            ui.horizontal(|ui| {
                color_chip(ui, TEAM_COLOURS[team]);
                ui.colored_label(color, format!("W{}", w.disc.id));
                ui.label(format!(
                    "({:5.0},{:5.0})  v=({:5.0},{:5.0})",
                    w.disc.pos.x, w.disc.pos.y, w.disc.vel.x, w.disc.vel.y,
                ));
            });
            let mut tags = Vec::new();
            if let Some(sid) = w.holding {
                tags.push(format!("holding s{sid}"));
            }
            if w.cooldown > 0 {
                tags.push(format!("cooldown {}", w.cooldown));
            }
            if !tags.is_empty() {
                ui.weak(format!("  {}", tags.join("  ·  ")));
            }
        }
    }

    fn bottom_panel(game: &FantasticBitsGame, _ctx: &VizCtx<'_, Self>, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for w in game.wizards() {
                let team = if w.disc.id < 2 { 0 } else { 1 };
                color_chip(ui, TEAM_COLOURS[team]);
                ui.label(format!("W{}:", w.disc.id));
                let text = match w.last_action {
                    Some(a) => format_action(a),
                    None => "—".into(),
                };
                ui.strong(text);
                ui.add_space(16.0);
            }
        });
        ui.add_space(4.0);
        ui.weak(format!(
            "snaffles alive: {}",
            game.snaffles().iter().filter(|s| s.alive).count()
        ));
        let _ = to_egui(WHITE); // keep the import live; we may use it later for spell lines.
    }
}

fn format_action(a: WizardAction) -> String {
    match a.kind {
        ActionKind::Move => format!("MOVE ({},{}) p={}", a.x, a.y, a.power),
        ActionKind::Throw => format!("THROW ({},{}) p={}", a.x, a.y, a.power),
        ActionKind::Obliviate => format!("OBLIVIATE #{}", a.target_id),
        ActionKind::Petrificus => format!("PETRIFICUS #{}", a.target_id),
        ActionKind::Accio => format!("ACCIO #{}", a.target_id),
        ActionKind::Flipendo => format!("FLIPENDO #{}", a.target_id),
    }
}

fn draw_velocity_arrow(centre: Vec2, vx: f64, vy: f64, scale: f32, color: Color) {
    // Scale the velocity arrow down so it doesn't dominate the field
    // — vel can hit 600+ units/tick at terminal pod speed.
    let len_scale = 0.3;
    let dx = (vx as f32) * scale * len_scale;
    let dy = (vy as f32) * scale * len_scale;
    if dx.abs() < 1.0 && dy.abs() < 1.0 {
        return;
    }
    draw_line(centre.x, centre.y, centre.x + dx, centre.y + dy, 1.5, color);
}

/// Built-in demo: short greedy-vs-greedy script. Reuses the same
/// nearest-snaffle / throw-at-goal policy the `_rs` bot uses so the
/// demo doesn't need a real bot to be runnable.
fn demo_replay() -> Replay<TurnOutput> {
    use common::engine::{Game, GameRng, GameRngSeed};

    const SEED: u64 = 3; // Seed 3 produces at least one goal under greedy bots.
    let mut rng = GameRng::seed_from_u64(SEED);
    let mut game = FantasticBitsGame::new(2, &mut rng);
    let mut outputs_per_tick: Vec<Vec<Option<TurnOutput>>> = Vec::new();

    while !game.active_players().is_empty() {
        let p0 = greedy_output(&game, 0);
        let p1 = greedy_output(&game, 1);
        let outputs = vec![Some(p0), Some(p1)];
        let outcome = game.step(&outputs);
        outputs_per_tick.push(outputs);
        if outcome.is_some() {
            break;
        }
    }

    Replay {
        seed: SEED,
        num_players: 2,
        outputs: outputs_per_tick,
    }
}

fn greedy_output(game: &FantasticBitsGame, player: usize) -> TurnOutput {
    use fantastic_bits_defs::EntityKind;
    let input = <FantasticBitsGame as common::engine::Game>::input_for(game, player as u32);
    let mine: Vec<&fantastic_bits_game::Wizard> = game
        .wizards()
        .iter()
        .filter(|w| {
            let team = if w.disc.id < 2 { 0 } else { 1 };
            team == player
        })
        .collect();
    // Opp goal is determined by team, not by current x. (Matches the
    // bot's post-InitialInput-fix behavior.)
    let opp_goal_x = if player == 0 { WIDTH } else { 0 };
    let act = |wx: f64, wy: f64, holding: bool| -> WizardAction {
        if holding {
            return WizardAction::throw_to(opp_goal_x, HEIGHT / 2, 500);
        }
        let target = input
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Snaffle && e.state == 0)
            .min_by_key(|s| {
                let dx = s.x as f64 - wx;
                let dy = s.y as f64 - wy;
                (dx * dx + dy * dy) as i64
            });
        match target {
            Some(s) => WizardAction::move_to(s.x, s.y, 150),
            None => WizardAction::move_to(WIDTH / 2, HEIGHT / 2, 0),
        }
    };
    TurnOutput {
        primary: act(
            mine[0].disc.pos.x,
            mine[0].disc.pos.y,
            mine[0].holding.is_some(),
        ),
        secondary: act(
            mine[1].disc.pos.x,
            mine[1].disc.pos.y,
            mine[1].holding.is_some(),
        ),
    }
}

viz::run_viz!(FantasticBitsViz, demo_replay());
