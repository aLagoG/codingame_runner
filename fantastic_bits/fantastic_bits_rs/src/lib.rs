//! Phase-1 baseline bot. For each of our wizards:
//!   * If we're holding a Snaffle → THROW it at the centre of the
//!     opponent's goal at max power.
//!   * Otherwise → MOVE toward the closest free Snaffle at max thrust.
//!   * Falls back to moving toward the map centre if no Snaffles exist
//!     (only happens after the last has been scored — won't be observed
//!     in normal play but keeps the bot from panicking).
//!
//! No spells, no Bludger avoidance, no defence. Exists so the engine +
//! runner round-trip end-to-end and so we have a punching bag once
//! physics + spell logic land.

use fantastic_bits_defs::{Entity, EntityKind, TurnOutput, TurnRef, WizardAction};

/// Map / goal constants — duplicated here (instead of depending on
/// `fantastic_bits_game`) so the bot stays a standalone leaf crate
/// shippable as a CodinGame submission.
const WIDTH: i32 = 16001;
const GOAL_Y: i32 = 3750;

pub fn decide(turn: TurnRef<'_>) -> TurnOutput {
    let my_wizards: Vec<&Entity> = turn
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Wizard)
        .collect();
    // Should always be exactly 2 per protocol; defensively handle the
    // off-nominal case so the bot can't panic on malformed input.
    let primary = act_for(my_wizards.first().copied(), turn.entities);
    let secondary = act_for(my_wizards.get(1).copied(), turn.entities);
    TurnOutput { primary, secondary }
}

fn act_for(wizard: Option<&Entity>, all: &[Entity]) -> WizardAction {
    let Some(w) = wizard else {
        // No wizard? Issue a no-op MOVE to the centre at zero thrust.
        return WizardAction::move_to(WIDTH / 2, GOAL_Y, 0);
    };

    // Opponent goal is whichever end we're farthest from.
    let opp_goal_x = if w.x < WIDTH / 2 { WIDTH - 1 } else { 0 };

    // Holding a Snaffle? Throw it at the opp goal at max power.
    if w.state == 1 {
        return WizardAction::throw_to(opp_goal_x, GOAL_Y, 500);
    }

    // Otherwise head toward the closest free Snaffle.
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

common::ffi_bot!(fantastic_bits_defs::Ffi, decide);

#[cfg(test)]
mod tests {
    use super::*;
    use fantastic_bits_defs::ActionKind;

    fn wizard(id: i32, team: i32, x: i32, y: i32, holding: bool) -> Entity {
        Entity {
            id,
            kind: if team == 0 {
                EntityKind::Wizard
            } else {
                EntityKind::OpponentWizard
            },
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

    #[test]
    fn throws_at_opp_goal_when_holding() {
        let entities = vec![
            wizard(0, 0, 1000, GOAL_Y, true),
            wizard(1, 0, 2000, GOAL_Y, false),
            snaffle(10, 8000, GOAL_Y, true),
        ];
        let out = decide(TurnRef {
            my_score: 0,
            my_magic: 0,
            opp_score: 0,
            opp_magic: 0,
            entities: &entities,
        });
        assert_eq!(out.primary.kind, ActionKind::Throw);
        assert_eq!(out.primary.x, WIDTH - 1);
        assert_eq!(out.primary.y, GOAL_Y);
        assert_eq!(out.primary.power, 500);
    }

    #[test]
    fn moves_to_nearest_free_snaffle() {
        let entities = vec![
            wizard(0, 0, 1000, GOAL_Y, false),
            wizard(1, 0, 2000, GOAL_Y, false),
            snaffle(10, 9000, GOAL_Y, false),
            snaffle(11, 5000, GOAL_Y, false), // closer to wizard 0
            snaffle(12, 12000, GOAL_Y, true), // held — skipped
        ];
        let out = decide(TurnRef {
            my_score: 0,
            my_magic: 0,
            opp_score: 0,
            opp_magic: 0,
            entities: &entities,
        });
        assert_eq!(out.primary.kind, ActionKind::Move);
        assert_eq!(out.primary.x, 5000);
        assert_eq!(out.primary.power, 150);
    }
}
