//! Continuous-space viz for Spider Attack. Like Fantastic Bits, we
//! treat the macroquad grid as a coarse world-to-screen scale (1 cell
//! = 100 world units) and convert manually for entity drawing.
//!
//! Status bar: per-team base HP, mana, wild mana, current tick.
//! Side panel: per-hero position and last action.
//! Bottom panel: per-hero action this tick.

use spider_attack_defs::{HeroAction, TurnOutput};
use spider_attack_game::{
    BASE_VISION_RANGE, HERO_ATTACK_RANGE, HEROES_PER_PLAYER, HEIGHT, MAX_TICKS,
    MONSTER_DAMAGE_RANGE, MONSTER_TARGET_RANGE, SpiderAttackGame, V2, WIDTH,
};
use viz::{CellGrid, Replay, Visualize, VizCtx, color_chip, egui};

use macroquad::prelude::*;

const WORLD_PER_CELL: f64 = 100.0;

const TEAM_COLOURS: [Color; 2] = [
    Color::new(0.30, 0.65, 1.00, 1.0), // blue
    Color::new(1.00, 0.40, 0.40, 1.0), // red
];
const MONSTER_COLOUR: Color = Color::new(0.55, 0.85, 0.40, 1.0); // green
const BASE_RING_COLOUR: Color = Color::new(1.00, 1.00, 1.00, 0.10);
const TARGET_RING_COLOUR: Color = Color::new(1.00, 0.65, 0.30, 0.08);
const DAMAGE_RING_COLOUR: Color = Color::new(1.00, 0.30, 0.30, 0.18);
const FIELD_OUTLINE: Color = Color::new(0.2, 0.25, 0.32, 1.0);

const HERO_RADIUS: f64 = 200.0;
const MONSTER_RADIUS: f64 = 150.0;

struct SpiderAttackViz;

impl Visualize for SpiderAttackViz {
    type Game = SpiderAttackGame;

    fn grid_size() -> (u32, u32) {
        (
            (WIDTH as f64 / WORLD_PER_CELL) as u32,
            (HEIGHT as f64 / WORLD_PER_CELL) as u32,
        )
    }

    fn draw(game: &SpiderAttackGame, grid: &CellGrid) {
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

        // Base rings: vision (6000), target range (5000), damage (300).
        for team in 0..2 {
            let base = SpiderAttackGame::base_pos(team);
            let c = to_screen(base.x, base.y);
            draw_circle_lines(
                c.x,
                c.y,
                (BASE_VISION_RANGE as f32) * scale,
                1.0,
                BASE_RING_COLOUR,
            );
            draw_circle_lines(
                c.x,
                c.y,
                (MONSTER_TARGET_RANGE as f32) * scale,
                1.0,
                TARGET_RING_COLOUR,
            );
            draw_circle(
                c.x,
                c.y,
                (MONSTER_DAMAGE_RANGE as f32) * scale,
                DAMAGE_RING_COLOUR,
            );
            // Base marker.
            draw_circle(c.x, c.y, 8.0, TEAM_COLOURS[team]);
        }

        // Monsters.
        for m in game.monsters() {
            let c = to_screen(m.pos.x, m.pos.y);
            let mut col = MONSTER_COLOUR;
            // Targeted monsters tinted toward the threatened team.
            if let Some(t) = m.target_base {
                col = lerp_color(col, TEAM_COLOURS[t], 0.4);
            }
            draw_circle(c.x, c.y, (MONSTER_RADIUS as f32) * scale, col);
            if m.shield_life > 0 {
                draw_circle_lines(c.x, c.y, (MONSTER_RADIUS as f32) * scale * 1.3, 2.0, WHITE);
            }
            // Velocity arrow (scaled down so it doesn't dominate).
            draw_velocity_arrow(c, m.vel, scale, col);
            // HP bar above the monster.
            draw_hp_bar(c, m.health, 20, scale);
        }

        // Heroes.
        for h in game.heroes() {
            let c = to_screen(h.pos.x, h.pos.y);
            let r = (HERO_RADIUS as f32) * scale;
            draw_circle(c.x, c.y, r, TEAM_COLOURS[h.team]);
            // Attack range hint.
            draw_circle_lines(
                c.x,
                c.y,
                (HERO_ATTACK_RANGE as f32) * scale,
                1.0,
                Color::new(1.0, 1.0, 1.0, 0.10),
            );
            if h.shield_life > 0 {
                draw_circle_lines(c.x, c.y, r * 1.3, 2.0, WHITE);
            }
            if h.control_target.is_some() {
                draw_circle_lines(c.x, c.y, r * 1.5, 1.5, YELLOW);
            }
        }
    }

    fn status(game: &SpiderAttackGame) -> String {
        format!(
            "HP {}-{}   Mana {}/{}   Wild {}/{}   Tick {}/{}",
            game.health()[0],
            game.health()[1],
            game.mana()[0],
            game.mana()[1],
            game.wild_mana()[0],
            game.wild_mana()[1],
            game.tick(),
            MAX_TICKS,
        )
    }

    fn side_panel(game: &SpiderAttackGame, ui: &mut egui::Ui) {
        ui.heading("Bases");
        for team in 0..2 {
            ui.horizontal(|ui| {
                color_chip(ui, TEAM_COLOURS[team]);
                ui.label(format!(
                    "P{} — HP {}  mana {}  wild {}",
                    team,
                    game.health()[team],
                    game.mana()[team],
                    game.wild_mana()[team],
                ));
            });
        }
        ui.separator();
        ui.heading("Heroes");
        for h in game.heroes() {
            ui.horizontal(|ui| {
                color_chip(ui, TEAM_COLOURS[h.team]);
                ui.label(format!(
                    "H{} ({:5.0},{:5.0})",
                    h.id, h.pos.x, h.pos.y,
                ));
            });
            let mut tags = Vec::new();
            if h.shield_life > 0 {
                tags.push(format!("shield {}", h.shield_life));
            }
            if h.control_target.is_some() {
                tags.push("controlled".to_string());
            }
            if !tags.is_empty() {
                ui.weak(format!("  {}", tags.join("  ·  ")));
            }
        }
        ui.separator();
        ui.heading("Monsters");
        ui.label(format!("alive: {}", game.monsters().len()));
    }

    fn bottom_panel(game: &SpiderAttackGame, _ctx: &VizCtx<'_, Self>, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for h in game.heroes() {
                color_chip(ui, TEAM_COLOURS[h.team]);
                ui.label(format!("H{}:", h.id));
                let text = match h.last_action {
                    Some(a) => format_action(a),
                    None => "—".into(),
                };
                ui.strong(text);
                ui.add_space(16.0);
            }
        });
    }
}

fn format_action(a: HeroAction) -> String {
    match a {
        HeroAction::Wait => "WAIT".into(),
        HeroAction::Move { x, y } => format!("MOVE ({x},{y})"),
        HeroAction::Wind { x, y } => format!("WIND ({x},{y})"),
        HeroAction::Shield { entity_id } => format!("SHIELD #{entity_id}"),
        HeroAction::Control { entity_id, x, y } => {
            format!("CONTROL #{entity_id} → ({x},{y})")
        }
    }
}

fn draw_velocity_arrow(centre: Vec2, vel: V2, scale: f32, color: Color) {
    let len_scale = 0.4;
    let dx = (vel.x as f32) * scale * len_scale;
    let dy = (vel.y as f32) * scale * len_scale;
    if dx.abs() < 1.0 && dy.abs() < 1.0 {
        return;
    }
    draw_line(centre.x, centre.y, centre.x + dx, centre.y + dy, 1.5, color);
}

fn draw_hp_bar(centre: Vec2, hp: i32, max_hp: i32, scale: f32) {
    let w = 30.0 * scale.max(0.5);
    let h = 3.0;
    let frac = (hp as f32 / max_hp as f32).clamp(0.0, 1.0);
    let top = centre.y - (MONSTER_RADIUS as f32) * scale - 6.0;
    draw_rectangle(centre.x - w / 2.0, top, w, h, Color::new(0.0, 0.0, 0.0, 0.5));
    draw_rectangle(
        centre.x - w / 2.0,
        top,
        w * frac,
        h,
        Color::new(0.6, 1.0, 0.4, 0.9),
    );
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        1.0,
    )
}

/// Built-in demo: greedy-vs-greedy script. Reuses the same nearest-
/// threat / guard-post policy the `_rs` baseline uses so the demo
/// doesn't need a built bot binary.
fn demo_replay() -> Replay<TurnOutput> {
    use common::engine::{Game, GameRng, GameRngSeed};

    const SEED: u64 = 7;
    let mut rng = GameRng::seed_from_u64(SEED);
    let mut game = SpiderAttackGame::new(2, &mut rng);
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

fn greedy_output(game: &SpiderAttackGame, player: usize) -> TurnOutput {
    let base = SpiderAttackGame::base_pos(player);
    let dir_x = if base.x == 0.0 { 1.0 } else { -1.0 };
    let dir_y = if base.y == 0.0 { 1.0 } else { -1.0 };
    let guard = [
        V2::new(base.x + dir_x * 5000.0, base.y + dir_y * 1500.0),
        V2::new(base.x + dir_x * 3500.0, base.y + dir_y * 3500.0),
        V2::new(base.x + dir_x * 1500.0, base.y + dir_y * 5000.0),
    ];

    let mut threats: Vec<&spider_attack_game::Monster> = game
        .monsters()
        .iter()
        .filter(|m| {
            m.target_base == Some(player) || m.pos.sub(base).len() <= BASE_VISION_RANGE
        })
        .collect();
    threats.sort_by(|a, b| {
        let da = a.pos.sub(base).len();
        let db = b.pos.sub(base).len();
        da.partial_cmp(&db).unwrap()
    });

    let mut actions = [HeroAction::Wait; 3];
    let my_heroes: Vec<&spider_attack_game::Hero> = game
        .heroes()
        .iter()
        .filter(|h| h.team == player)
        .collect();
    for (i, h) in my_heroes.iter().enumerate().take(HEROES_PER_PLAYER) {
        let target = if let Some(m) = threats.get(i) {
            m.pos
        } else {
            guard[i.min(2)]
        };
        let slot = (h.id as usize) - player * HEROES_PER_PLAYER;
        actions[slot] = HeroAction::Move {
            x: target.x as i32,
            y: target.y as i32,
        };
    }
    TurnOutput { actions }
}

viz::run_viz!(SpiderAttackViz, demo_replay());
