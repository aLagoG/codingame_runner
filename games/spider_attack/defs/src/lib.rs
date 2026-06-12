//! Wire types for the Spider Attack game.
//!
//! Initial input (2 lines):
//!   <base_x> <base_y>
//!   <heroes_per_player>            (always 3)
//!
//! Per-turn input:
//!   <my_health> <my_mana>
//!   <opp_health> <opp_mana>
//!   <entity_count>
//!   <id> <type> <x> <y> <shield_life> <is_controlled> <health> <vx> <vy> <near_base> <threat_for>
//!   ... (entity_count rows)
//!
//! Per-turn output: 3 lines (one per hero), one of:
//!   WAIT
//!   MOVE <x> <y>
//!   SPELL WIND <x> <y>
//!   SPELL SHIELD <entity_id>
//!   SPELL CONTROL <entity_id> <x> <y>
//!
//! Optional trailing text is accepted (CodinGame's "label" feature for the
//! hero's name plate) but discarded.

use std::{
    fmt::Display,
    io::{self, BufRead, Write},
    str::FromStr,
};

use bot_common::{BotError, BotResult, ReadFrom, SingleLine, WriteTo, next_field, next_i32};

// ============================================================
//  Initial input
// ============================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InitialInput {
    pub base_x: i32,
    pub base_y: i32,
    /// Always 3 — kept on the wire because the statement specifies it,
    /// and so a future variant could grow this without a format change.
    pub heroes_per_player: i32,
}

impl ReadFrom for InitialInput {
    fn read_from(r: &mut impl BufRead) -> BotResult<Self> {
        let (base_x, base_y) = read_two_ints(r)?;
        let heroes_per_player = read_one_int(r)?;
        Ok(InitialInput {
            base_x,
            base_y,
            heroes_per_player,
        })
    }
}

impl WriteTo for InitialInput {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{} {}", self.base_x, self.base_y)?;
        writeln!(w, "{}", self.heroes_per_player)?;
        Ok(())
    }
}

// ============================================================
//  Entities
// ============================================================

/// Wire `type` field: 0 = monster, 1 = my hero, 2 = opponent hero.
/// Stored from the receiving player's perspective — the engine relabels
/// per `input_for(player)`.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub enum EntityKind {
    #[default]
    Monster,
    MyHero,
    OppHero,
}

impl EntityKind {
    pub fn to_wire(self) -> i32 {
        match self {
            EntityKind::Monster => 0,
            EntityKind::MyHero => 1,
            EntityKind::OppHero => 2,
        }
    }

    pub fn from_wire(v: i32) -> BotResult<Self> {
        Ok(match v {
            0 => EntityKind::Monster,
            1 => EntityKind::MyHero,
            2 => EntityKind::OppHero,
            other => return Err(format!("unknown entity type {other}").into()),
        })
    }
}

/// One row of the per-turn entity list. Monster-only fields (health,
/// vx, vy, near_base, threat_for) are -1 for heroes per the statement.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: i32,
    pub kind: EntityKind,
    pub x: i32,
    pub y: i32,
    pub shield_life: i32,
    pub is_controlled: i32,
    pub health: i32,
    pub vx: i32,
    pub vy: i32,
    pub near_base: i32,
    pub threat_for: i32,
}

impl Display for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} {} {} {} {} {} {} {} {}",
            self.id,
            self.kind.to_wire(),
            self.x,
            self.y,
            self.shield_life,
            self.is_controlled,
            self.health,
            self.vx,
            self.vy,
            self.near_base,
            self.threat_for,
        )
    }
}

impl FromStr for Entity {
    type Err = BotError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split_whitespace();
        Ok(Entity {
            id: next_i32(&mut it, "id")?,
            kind: EntityKind::from_wire(next_i32(&mut it, "type")?)?,
            x: next_i32(&mut it, "x")?,
            y: next_i32(&mut it, "y")?,
            shield_life: next_i32(&mut it, "shield_life")?,
            is_controlled: next_i32(&mut it, "is_controlled")?,
            health: next_i32(&mut it, "health")?,
            vx: next_i32(&mut it, "vx")?,
            vy: next_i32(&mut it, "vy")?,
            near_base: next_i32(&mut it, "near_base")?,
            threat_for: next_i32(&mut it, "threat_for")?,
        })
    }
}

impl SingleLine for Entity {}

// ============================================================
//  Hero actions (output)
// ============================================================

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub enum HeroAction {
    #[default]
    Wait,
    Move { x: i32, y: i32 },
    Wind { x: i32, y: i32 },
    Shield { entity_id: i32 },
    Control { entity_id: i32, x: i32, y: i32 },
}

impl Display for HeroAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            HeroAction::Wait => write!(f, "WAIT"),
            HeroAction::Move { x, y } => write!(f, "MOVE {x} {y}"),
            HeroAction::Wind { x, y } => write!(f, "SPELL WIND {x} {y}"),
            HeroAction::Shield { entity_id } => write!(f, "SPELL SHIELD {entity_id}"),
            HeroAction::Control { entity_id, x, y } => {
                write!(f, "SPELL CONTROL {entity_id} {x} {y}")
            }
        }
    }
}

impl FromStr for HeroAction {
    type Err = BotError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split_whitespace();
        let cmd = next_field(&mut it, "action kind")?;
        match cmd {
            "WAIT" => Ok(HeroAction::Wait),
            "MOVE" => Ok(HeroAction::Move {
                x: next_i32(&mut it, "x")?,
                y: next_i32(&mut it, "y")?,
            }),
            "SPELL" => {
                let spell = next_field(&mut it, "spell kind")?;
                match spell {
                    "WIND" => Ok(HeroAction::Wind {
                        x: next_i32(&mut it, "x")?,
                        y: next_i32(&mut it, "y")?,
                    }),
                    "SHIELD" => Ok(HeroAction::Shield {
                        entity_id: next_i32(&mut it, "entity_id")?,
                    }),
                    "CONTROL" => Ok(HeroAction::Control {
                        entity_id: next_i32(&mut it, "entity_id")?,
                        x: next_i32(&mut it, "x")?,
                        y: next_i32(&mut it, "y")?,
                    }),
                    other => Err(format!("unknown spell {other:?}").into()),
                }
            }
            other => Err(format!("unknown command {other:?}").into()),
        }
    }
}

impl SingleLine for HeroAction {}

// ============================================================
//  TurnInput / TurnOutput
// ============================================================

#[derive(Debug, Default, Clone)]
pub struct TurnInput {
    pub my_health: i32,
    pub my_mana: i32,
    pub opp_health: i32,
    pub opp_mana: i32,
    pub entities: Vec<Entity>,
}

impl Display for TurnInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{} {}", self.my_health, self.my_mana)?;
        writeln!(f, "{} {}", self.opp_health, self.opp_mana)?;
        writeln!(f, "{}", self.entities.len())?;
        for e in &self.entities {
            writeln!(f, "{e}")?;
        }
        Ok(())
    }
}

impl ReadFrom for TurnInput {
    fn read_from(r: &mut impl BufRead) -> BotResult<Self> {
        let (my_health, my_mana) = read_two_ints(r)?;
        let (opp_health, opp_mana) = read_two_ints(r)?;
        let count = read_one_int(r)?;
        if count < 0 {
            return Err(format!("negative entity count {count}").into());
        }
        let mut entities = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut buf = String::new();
            r.read_line(&mut buf)?;
            entities.push(buf.parse()?);
        }
        Ok(TurnInput {
            my_health,
            my_mana,
            opp_health,
            opp_mana,
            entities,
        })
    }
}

impl WriteTo for TurnInput {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{} {}", self.my_health, self.my_mana)?;
        writeln!(w, "{} {}", self.opp_health, self.opp_mana)?;
        writeln!(w, "{}", self.entities.len())?;
        for e in &self.entities {
            writeln!(w, "{e}")?;
        }
        Ok(())
    }
}

/// Three actions per turn, one per hero, in hero-id order (lower id
/// first). Three lines on the wire — explicitly NOT `SingleLine`.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct TurnOutput {
    pub actions: [HeroAction; 3],
}

impl ReadFrom for TurnOutput {
    fn read_from(r: &mut impl BufRead) -> BotResult<Self> {
        let mut actions = [HeroAction::Wait; 3];
        for slot in actions.iter_mut() {
            let mut buf = String::new();
            r.read_line(&mut buf)?;
            *slot = buf.parse()?;
        }
        Ok(TurnOutput { actions })
    }
}

impl WriteTo for TurnOutput {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        for a in &self.actions {
            writeln!(w, "{a}")?;
        }
        Ok(())
    }
}

// ============================================================
//  Helpers
// ============================================================

fn read_one_int(r: &mut impl BufRead) -> BotResult<i32> {
    let mut buf = String::new();
    r.read_line(&mut buf)?;
    buf.trim().parse().map_err(Into::into)
}

fn read_two_ints(r: &mut impl BufRead) -> BotResult<(i32, i32)> {
    let mut buf = String::new();
    r.read_line(&mut buf)?;
    let mut it = buf.split_whitespace();
    let a = next_i32(&mut it, "first int")?;
    let b = next_i32(&mut it, "second int")?;
    Ok((a, b))
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_input_round_trip() -> BotResult<()> {
        let init = InitialInput {
            base_x: 0,
            base_y: 0,
            heroes_per_player: 3,
        };
        let mut buf = Vec::new();
        init.write_to(&mut buf)?;
        let parsed = InitialInput::read_from(&mut buf.as_slice())?;
        assert_eq!(parsed, init);
        Ok(())
    }

    #[test]
    fn entity_round_trip() -> BotResult<()> {
        let e = Entity {
            id: 3,
            kind: EntityKind::Monster,
            x: 1500,
            y: 2200,
            shield_life: 0,
            is_controlled: 0,
            health: 10,
            vx: -200,
            vy: 300,
            near_base: 1,
            threat_for: 1,
        };
        let s = e.to_string();
        let parsed: Entity = s.parse()?;
        assert_eq!(parsed, e);
        Ok(())
    }

    #[test]
    fn hero_action_round_trip() -> BotResult<()> {
        let cases = [
            HeroAction::Wait,
            HeroAction::Move { x: 100, y: 200 },
            HeroAction::Wind { x: 5000, y: 5000 },
            HeroAction::Shield { entity_id: 7 },
            HeroAction::Control {
                entity_id: 12,
                x: 8000,
                y: 4500,
            },
        ];
        for a in cases {
            let s = a.to_string();
            let parsed: HeroAction = s.parse()?;
            assert_eq!(parsed, a);
        }
        Ok(())
    }

    #[test]
    fn turn_input_round_trip() -> BotResult<()> {
        let input = TurnInput {
            my_health: 3,
            my_mana: 42,
            opp_health: 2,
            opp_mana: 0,
            entities: vec![
                Entity {
                    id: 0,
                    kind: EntityKind::MyHero,
                    x: 5000,
                    y: 1000,
                    shield_life: 0,
                    is_controlled: 0,
                    health: -1,
                    vx: -1,
                    vy: -1,
                    near_base: -1,
                    threat_for: -1,
                },
                Entity {
                    id: 9,
                    kind: EntityKind::Monster,
                    x: 8000,
                    y: 4500,
                    shield_life: 0,
                    is_controlled: 1,
                    health: 12,
                    vx: -400,
                    vy: 0,
                    near_base: 0,
                    threat_for: 1,
                },
            ],
        };
        let mut buf = Vec::new();
        input.write_to(&mut buf)?;
        let parsed = TurnInput::read_from(&mut buf.as_slice())?;
        assert_eq!(parsed.my_health, input.my_health);
        assert_eq!(parsed.my_mana, input.my_mana);
        assert_eq!(parsed.entities, input.entities);
        Ok(())
    }

    #[test]
    fn turn_output_round_trip() -> BotResult<()> {
        let out = TurnOutput {
            actions: [
                HeroAction::Move { x: 100, y: 200 },
                HeroAction::Wind { x: 5000, y: 5000 },
                HeroAction::Wait,
            ],
        };
        let mut buf = Vec::new();
        out.write_to(&mut buf)?;
        let parsed = TurnOutput::read_from(&mut buf.as_slice())?;
        assert_eq!(parsed, out);
        Ok(())
    }
}
