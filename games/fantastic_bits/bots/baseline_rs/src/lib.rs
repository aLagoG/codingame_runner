//! Phase-1 baseline bot.
//!   * On init: stash `my_team_id` from the engine.
//!   * Per turn, for each wizard:
//!     - If holding a Snaffle → THROW it at the center of the
//!       opponent's goal at max power.
//!     - Otherwise → MOVE toward the closest free Snaffle at max thrust.
//!     - Falls back to the map center if no Snaffles exist.
//!
//! No spells, no Bludger avoidance, no defence. Exists so the engine +
//! runner round-trip end-to-end and so we have a punching bag once
//! smarter bots come online.

use std::sync::OnceLock;

use fantastic_bits_defs::{Entity, EntityKind, InitialInput, TurnInput, TurnOutput, WizardAction};

/// Map / goal constants — duplicated here (instead of depending on
/// `fantastic_bits_game`) so the bot stays a standalone leaf crate
/// shippable as a CodinGame submission.
const WIDTH: i32 = 16000;
const GOAL_Y: i32 = 3750;

/// `my_team_id` cached from `on_init`. Set once at match start; read
/// by `decide` on every tick. We don't use a default — if it's ever
/// unset by the time `decide` runs, the bot still works (it'll guess
/// team 0) but the heuristic for opp goal will be wrong for team 1.
static MY_TEAM_ID: OnceLock<i32> = OnceLock::new();

pub fn on_init(init: &InitialInput) {
    let _ = MY_TEAM_ID.set(init.my_team_id);
}

pub fn decide(turn: &TurnInput) -> TurnOutput {
    let my_team = MY_TEAM_ID.get().copied().unwrap_or(0);
    decide_with_team(turn, my_team)
}

/// Pure variant used by tests so we don't depend on the global cell.
pub fn decide_with_team(turn: &TurnInput, my_team: i32) -> TurnOutput {
    let my_wizards: Vec<&Entity> = turn
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Wizard)
        .collect();
    let primary = act_for(my_wizards.first().copied(), &turn.entities, my_team);
    let secondary = act_for(my_wizards.get(1).copied(), &turn.entities, my_team);
    TurnOutput { primary, secondary }
}

fn act_for(wizard: Option<&Entity>, all: &[Entity], my_team: i32) -> WizardAction {
    let Some(w) = wizard else {
        return WizardAction::move_to(WIDTH / 2, GOAL_Y, 0);
    };

    // Goal direction is determined by team, not current position. Team 0
    // defends x=0, attacks x=WIDTH; team 1 the other way around.
    let opp_goal_x = if my_team == 0 { WIDTH } else { 0 };

    if w.state == 1 {
        return WizardAction::throw_to(opp_goal_x, GOAL_Y, 500);
    }

    let target = all
        .iter()
        .filter(|e| e.kind == EntityKind::Snaffle && e.state == 0)
        .min_by_key(|s| sq_dist(w.x, w.y, s.x, s.y));

    match target {
        Some(s) => WizardAction::move_to(s.x, s.y, 150),
        None => WizardAction::move_to(WIDTH / 2, GOAL_Y, 0),
    }
}

fn sq_dist(ax: i32, ay: i32, bx: i32, by: i32) -> i64 {
    let dx = (ax - bx) as i64;
    let dy = (ay - by) as i64;
    dx * dx + dy * dy
}
