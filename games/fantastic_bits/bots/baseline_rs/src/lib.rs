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

use fantastic_bits_defs::{Entity, EntityKind, InitialInputRef, TurnOutput, TurnRef, WizardAction};

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

/// Init handler invoked once by the `ffi_bot!` plumbing (or by the
/// subprocess `main`). Stashes `my_team_id` into the static cell.
pub fn on_init(init: InitialInputRef<'_>) {
    let _ = MY_TEAM_ID.set(init.my_team_id);
}

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    let my_team = MY_TEAM_ID.get().copied().unwrap_or(0);
    decide_with_team(turn, my_team)
}

/// Pure variant used by tests so we don't depend on the global cell.
pub fn decide_with_team(turn: TurnRef<'_>, my_team: i32) -> TurnOutput {
    let my_wizards: Vec<&Entity> = turn
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Wizard)
        .collect();
    let primary = act_for(my_wizards.first().copied(), turn.entities, my_team);
    let secondary = act_for(my_wizards.get(1).copied(), turn.entities, my_team);
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

bot_common::ffi_bot!(fantastic_bits_defs::Ffi, decide, on_init);

#[cfg(test)]
mod tests {
    use super::*;
    use fantastic_bits_defs::ActionKind;

    /// Builds an entity tagged as the bot's own wizard. Tests always
    /// construct entity rows from the *receiving* bot's perspective —
    /// the engine's `input_for` does that relabeling already.
    fn my_wizard(id: i32, x: i32, y: i32, holding: bool) -> Entity {
        Entity {
            id,
            kind: EntityKind::Wizard,
            x,
            y,
            vx: 0,
            vy: 0,
            state: if holding { 1 } else { 0 },
        }
    }

    fn snaffle(id: i32, x: i32, y: i32, held: bool) -> Entity {
        Entity {
            id,
            kind: EntityKind::Snaffle,
            x,
            y,
            vx: 0,
            vy: 0,
            state: if held { 1 } else { 0 },
        }
    }

    fn turn(entities: &[Entity]) -> TurnRef<'_> {
        TurnRef {
            my_score: 0,
            my_magic: 0,
            opp_score: 0,
            opp_magic: 0,
            entities,
        }
    }

    #[test]
    fn team_0_throws_at_right_goal_from_own_half() {
        let entities = vec![
            my_wizard(0, 1000, GOAL_Y, true),
            my_wizard(1, 2000, GOAL_Y, false),
        ];
        let out = decide_with_team(turn(&entities), 0);
        assert_eq!(out.primary.kind, ActionKind::Throw);
        assert_eq!(out.primary.x, WIDTH);
        assert_eq!(out.primary.y, GOAL_Y);
        assert_eq!(out.primary.power, 500);
    }

    /// Demonstrated the position-based heuristic bug pre-fix. Now passes
    /// because the bot trusts `my_team_id`, not the wizard's current x.
    #[test]
    fn team_0_throws_at_right_goal_even_past_midline() {
        let entities = vec![
            my_wizard(0, 10_000, GOAL_Y, true), // past midline, holding
            my_wizard(1, 1_000, GOAL_Y, false),
        ];
        let out = decide_with_team(turn(&entities), 0);
        assert_eq!(out.primary.kind, ActionKind::Throw);
        assert_eq!(out.primary.x, WIDTH, "team-0 always attacks the right goal");
    }

    #[test]
    fn team_1_throws_at_left_goal() {
        let entities = vec![
            my_wizard(2, 15_000, GOAL_Y, true),
            my_wizard(3, 14_000, GOAL_Y, false),
        ];
        let out = decide_with_team(turn(&entities), 1);
        assert_eq!(out.primary.kind, ActionKind::Throw);
        assert_eq!(out.primary.x, 0);
    }

    #[test]
    fn moves_to_nearest_free_snaffle() {
        let entities = vec![
            my_wizard(0, 1000, GOAL_Y, false),
            my_wizard(1, 2000, GOAL_Y, false),
            snaffle(10, 9000, GOAL_Y, false),
            snaffle(11, 5000, GOAL_Y, false), // closer to wizard 0
            snaffle(12, 12000, GOAL_Y, true), // held — skipped
        ];
        let out = decide_with_team(turn(&entities), 0);
        assert_eq!(out.primary.kind, ActionKind::Move);
        assert_eq!(out.primary.x, 5000);
        assert_eq!(out.primary.power, 150);
    }
}
