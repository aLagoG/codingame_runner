//! Baseline Spider Attack bot.
//!
//! Each turn, for each hero, walk toward the nearest visible threat —
//! a monster either inside our base radius (6000) or one already
//! targeting our base. If no threats exist, patrol a fixed guard point
//! near our base.
//!
//! No spells, no offence. Exists so the engine + runner round-trip end
//! to end and so smarter bots have something to beat.

use spider_attack_defs::{Entity, EntityKind, HeroAction, InitialInput, TurnInput, TurnOutput};

/// Map / vision constants — duplicated here (instead of depending on
/// `spider_attack_game`) so the bot stays a standalone leaf crate
/// shippable as a CodinGame submission.
const WIDTH: i32 = 17630;
const HEIGHT: i32 = 9000;
const BASE_VISION: i64 = 6000;

#[derive(Default)]
pub struct GameState {
    pub base_x: i32,
    pub base_y: i32,
}

pub fn on_init(init: &InitialInput, state: &mut GameState) {
    state.base_x = init.base_x;
    state.base_y = init.base_y;
}

pub fn decide(turn: &TurnInput, state: &mut GameState) -> TurnOutput {
    // Three guard points near our base — heroes default here when no
    // threats are visible. Mirrored for player 1 by walking from the
    // far corner instead of the origin.
    let dir_x = if state.base_x == 0 { 1 } else { -1 };
    let dir_y = if state.base_y == 0 { 1 } else { -1 };
    let guard_posts = [
        (state.base_x + dir_x * 5000, state.base_y + dir_y * 1500),
        (state.base_x + dir_x * 3500, state.base_y + dir_y * 3500),
        (state.base_x + dir_x * 1500, state.base_y + dir_y * 5000),
    ];

    let heroes: Vec<&Entity> = turn
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::MyHero)
        .collect();

    // Threats ranked by closeness to our base (the most-imminent first).
    // A threat is any visible monster either targeting our base or
    // simply inside the base radius — the latter so heroes engage early
    // rather than wait for the explicit threatFor flag to flip.
    let mut threats: Vec<&Entity> = turn
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Monster)
        .filter(|m| {
            m.threat_for == 1 || sq_dist(m.x, m.y, state.base_x, state.base_y) <= BASE_VISION * BASE_VISION
        })
        .collect();
    threats.sort_by_key(|m| sq_dist(m.x, m.y, state.base_x, state.base_y));

    let mut actions = [HeroAction::Wait; 3];
    for (i, hero) in heroes.iter().enumerate().take(3) {
        // Assign threats round-robin: hero 0 takes threat 0, hero 1
        // threat 1, etc. Falls back to a guard post otherwise.
        let target = threats
            .get(i)
            .map(|m| (m.x, m.y))
            .unwrap_or(guard_posts[i.min(2)]);
        actions[hero_slot_for(hero.id, state)] = HeroAction::Move {
            x: target.0.clamp(0, WIDTH),
            y: target.1.clamp(0, HEIGHT),
        };
    }
    TurnOutput { actions }
}

/// Hero ids are pre-allocated globally (0-2 = team 0, 3-5 = team 1).
/// The runner expects three actions per output in hero-id order
/// matching our own team, so map `hero.id` to slot 0..3.
fn hero_slot_for(id: i32, state: &GameState) -> usize {
    let base_off = if state.base_x == 0 { 0 } else { 3 };
    ((id - base_off).max(0) as usize).min(2)
}

fn sq_dist(ax: i32, ay: i32, bx: i32, by: i32) -> i64 {
    let dx = (ax - bx) as i64;
    let dy = (ay - by) as i64;
    dx * dx + dy * dy
}
