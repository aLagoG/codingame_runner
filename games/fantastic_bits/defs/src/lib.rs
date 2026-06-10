use std::{
    fmt::Display,
    io::{self, BufRead, Write},
    str::FromStr,
};

use bot_common::{ReadFrom, SingleLine, WriteTo, invalid_data};

// ============================================================
//  Initial input
// ============================================================

/// Per-player init data, sent once at match start. Matches the
/// statement: `myTeamId = 0` → goal on the left; `myTeamId = 1` →
/// goal on the right.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InitialInput {
    pub my_team_id: i32,
}

impl Display for InitialInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.my_team_id)
    }
}

impl FromStr for InitialInput {
    type Err = io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(InitialInput {
            my_team_id: s.trim().parse().map_err(invalid_data)?,
        })
    }
}

impl SingleLine for InitialInput {}

// ============================================================
//  Wire-level data types
// ============================================================

/// Tags for entities the engine emits per tick.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub enum EntityKind {
    /// One of the receiving player's own wizards (perspective-relative —
    /// the engine relabels per `input_for(player)`).
    #[default]
    Wizard,
    OpponentWizard,
    Snaffle,
    Bludger,
}

/// One row of the per-tick entity list. `state` is kind-dependent:
///   * Wizard: `1` if grabbing a Snaffle, else `0`.
///   * Snaffle: `1` if grabbed by a Wizard, else `0`.
///   * Bludger: `entityId` of last victim (-1 if none).
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: i32,
    pub kind: EntityKind,
    pub x: i32,
    pub y: i32,
    pub vx: i32,
    pub vy: i32,
    pub state: i32,
}

/// What kind of action a wizard is taking this tick. Together with the
/// numeric fields on [`WizardAction`] this is the full output schema.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub enum ActionKind {
    /// `MOVE x y thrust` — apply thrust toward (x, y); thrust in [0, 150].
    #[default]
    Move,
    /// `THROW x y power` — throw held snaffle toward (x, y) at power
    /// in [0, 500]. Ignored if wizard isn't holding a snaffle.
    Throw,
    /// `OBLIVIATE id` — bludger ignores caster's team for 4 turns. Cost 5.
    Obliviate,
    /// `PETRIFICUS id` — zero target velocity for 1 turn. Cost 10.
    Petrificus,
    /// `ACCIO id` — pull target toward caster for 6 turns. Cost 15.
    Accio,
    /// `FLIPENDO id` — push target away from caster for 3 turns. Cost 20.
    Flipendo,
}

/// One wizard's action for one tick. Fields are kind-dependent — fill in
/// only the ones the kind needs; the rest are ignored on the wire.
///
/// Constructor helpers ([`WizardAction::move_to`], `throw_to`, `cast`)
/// build the right shape so callers don't need to remember which fields
/// each kind uses.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct WizardAction {
    pub kind: ActionKind,
    /// MOVE/THROW: target x. Spells: ignored.
    pub x: i32,
    /// MOVE/THROW: target y. Spells: ignored.
    pub y: i32,
    /// MOVE: thrust [0, 150]. THROW: power [0, 500]. Spells: ignored.
    pub power: i32,
    /// Spells: target entity id. MOVE/THROW: ignored.
    pub target_id: i32,
}

impl WizardAction {
    pub fn move_to(x: i32, y: i32, thrust: i32) -> Self {
        WizardAction {
            kind: ActionKind::Move,
            x,
            y,
            power: thrust,
            target_id: 0,
        }
    }

    pub fn throw_to(x: i32, y: i32, power: i32) -> Self {
        WizardAction {
            kind: ActionKind::Throw,
            x,
            y,
            power,
            target_id: 0,
        }
    }

    pub fn cast(kind: ActionKind, target_id: i32) -> Self {
        debug_assert!(
            !matches!(kind, ActionKind::Move | ActionKind::Throw),
            "use move_to / throw_to for MOVE / THROW",
        );
        WizardAction {
            kind,
            x: 0,
            y: 0,
            power: 0,
            target_id,
        }
    }
}

/// Output for one tick: the two actions for the player's two wizards, in
/// wizard-id order (lower id first). Written/read as two lines on the
/// wire — *not* `SingleLine`.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct TurnOutput {
    pub primary: WizardAction,
    pub secondary: WizardAction,
}

/// Per-tick input the engine hands each player. `entities` shrinks as
/// snaffles get scored; outer length matches `<num_entities>` on the wire.
pub struct TurnInput {
    pub my_score: i32,
    pub my_magic: i32,
    pub opp_score: i32,
    pub opp_magic: i32,
    pub entities: Vec<Entity>,
}

// ============================================================
//  Display impls
// ============================================================

impl Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EntityKind::Wizard => "WIZARD",
            EntityKind::OpponentWizard => "OPPONENT_WIZARD",
            EntityKind::Snaffle => "SNAFFLE",
            EntityKind::Bludger => "BLUDGER",
        })
    }
}

impl Display for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} {} {} {} {}",
            self.id, self.kind, self.x, self.y, self.vx, self.vy, self.state,
        )
    }
}

impl Display for ActionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ActionKind::Move => "MOVE",
            ActionKind::Throw => "THROW",
            ActionKind::Obliviate => "OBLIVIATE",
            ActionKind::Petrificus => "PETRIFICUS",
            ActionKind::Accio => "ACCIO",
            ActionKind::Flipendo => "FLIPENDO",
        })
    }
}

impl Display for WizardAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ActionKind::Move => write!(f, "MOVE {} {} {}", self.x, self.y, self.power),
            ActionKind::Throw => write!(f, "THROW {} {} {}", self.x, self.y, self.power),
            ActionKind::Obliviate => write!(f, "OBLIVIATE {}", self.target_id),
            ActionKind::Petrificus => write!(f, "PETRIFICUS {}", self.target_id),
            ActionKind::Accio => write!(f, "ACCIO {}", self.target_id),
            ActionKind::Flipendo => write!(f, "FLIPENDO {}", self.target_id),
        }
    }
}

impl Display for TurnInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{} {}", self.my_score, self.my_magic)?;
        writeln!(f, "{} {}", self.opp_score, self.opp_magic)?;
        writeln!(f, "{}", self.entities.len())?;
        for e in &self.entities {
            writeln!(f, "{e}")?;
        }
        Ok(())
    }
}

// ============================================================
//  FromStr impls
// ============================================================

impl FromStr for EntityKind {
    type Err = io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "WIZARD" => EntityKind::Wizard,
            "OPPONENT_WIZARD" => EntityKind::OpponentWizard,
            "SNAFFLE" => EntityKind::Snaffle,
            "BLUDGER" => EntityKind::Bludger,
            other => return Err(invalid_data(format!("Unrecognized entity kind {other:?}"))),
        })
    }
}

impl FromStr for Entity {
    type Err = io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split_whitespace();
        Ok(Entity {
            id: next_i32(&mut it, "id")?,
            kind: next_field(&mut it, "kind")?.parse()?,
            x: next_i32(&mut it, "x")?,
            y: next_i32(&mut it, "y")?,
            vx: next_i32(&mut it, "vx")?,
            vy: next_i32(&mut it, "vy")?,
            state: next_i32(&mut it, "state")?,
        })
    }
}

impl FromStr for ActionKind {
    type Err = io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "MOVE" => ActionKind::Move,
            "THROW" => ActionKind::Throw,
            "OBLIVIATE" => ActionKind::Obliviate,
            "PETRIFICUS" => ActionKind::Petrificus,
            "ACCIO" => ActionKind::Accio,
            "FLIPENDO" => ActionKind::Flipendo,
            other => return Err(invalid_data(format!("Unrecognized action kind {other:?}"))),
        })
    }
}

impl FromStr for WizardAction {
    type Err = io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split_whitespace();
        let kind: ActionKind = next_field(&mut it, "action kind")?.parse()?;
        match kind {
            ActionKind::Move | ActionKind::Throw => Ok(WizardAction {
                kind,
                x: next_i32(&mut it, "x")?,
                y: next_i32(&mut it, "y")?,
                power: next_i32(&mut it, "power/thrust")?,
                target_id: 0,
            }),
            _ => Ok(WizardAction {
                kind,
                x: 0,
                y: 0,
                power: 0,
                target_id: next_i32(&mut it, "target id")?,
            }),
        }
    }
}

impl FromStr for TurnInput {
    type Err = io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::read_from(&mut s.as_bytes())
    }
}

// Helpers for the whitespace-split parsing pattern used by the
// `FromStr` impls above. Without these, every field becomes a
// 3-method chain (`.next().ok_or_else(…).?.parse().map_err(…)?`).
fn next_field<'a>(it: &mut std::str::SplitWhitespace<'a>, field: &str) -> io::Result<&'a str> {
    it.next()
        .ok_or_else(|| invalid_data(format!("missing {field}")))
}

fn next_i32(it: &mut std::str::SplitWhitespace, field: &str) -> io::Result<i32> {
    next_field(it, field)?.parse().map_err(invalid_data)
}

// ============================================================
//  SingleLine markers
// ============================================================

impl SingleLine for EntityKind {}
impl SingleLine for Entity {}
impl SingleLine for ActionKind {}
impl SingleLine for WizardAction {}

// ============================================================
//  Multi-line wire format
// ============================================================

impl ReadFrom for TurnInput {
    fn read_from(r: &mut impl BufRead) -> io::Result<Self> {
        let (my_score, my_magic) = read_two_ints(r)?;
        let (opp_score, opp_magic) = read_two_ints(r)?;
        let num: i32 = read_one_int(r)?;
        if num < 0 {
            return Err(invalid_data(format!("negative entity count {num}")));
        }
        let mut entities = Vec::with_capacity(num as usize);
        for _ in 0..num {
            let mut buf = String::new();
            r.read_line(&mut buf)?;
            entities.push(buf.parse()?);
        }
        Ok(TurnInput {
            my_score,
            my_magic,
            opp_score,
            opp_magic,
            entities,
        })
    }
}

impl WriteTo for TurnInput {
    fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "{} {}", self.my_score, self.my_magic)?;
        writeln!(w, "{} {}", self.opp_score, self.opp_magic)?;
        writeln!(w, "{}", self.entities.len())?;
        for e in &self.entities {
            writeln!(w, "{e}")?;
        }
        Ok(())
    }
}

/// TurnOutput is two lines — explicitly not `SingleLine`, so we hand-roll
/// the wire glue rather than getting the blanket impls.
impl ReadFrom for TurnOutput {
    fn read_from(r: &mut impl BufRead) -> io::Result<Self> {
        let mut a = String::new();
        r.read_line(&mut a)?;
        let mut b = String::new();
        r.read_line(&mut b)?;
        Ok(TurnOutput {
            primary: a.parse()?,
            secondary: b.parse()?,
        })
    }
}

impl WriteTo for TurnOutput {
    fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.primary)?;
        writeln!(w, "{}", self.secondary)?;
        Ok(())
    }
}

fn read_one_int(r: &mut impl BufRead) -> io::Result<i32> {
    let mut buf = String::new();
    r.read_line(&mut buf)?;
    buf.trim().parse().map_err(invalid_data)
}

fn read_two_ints(r: &mut impl BufRead) -> io::Result<(i32, i32)> {
    let mut buf = String::new();
    r.read_line(&mut buf)?;
    let (a, b) = buf
        .trim()
        .split_once(' ')
        .ok_or_else(|| invalid_data(format!("expected two ints, got {buf:?}")))?;
    Ok((a.parse().map_err(invalid_data)?, b.parse().map_err(invalid_data)?))
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Result;

    #[test]
    fn initial_input_round_trip() -> Result<()> {
        for tid in [0, 1] {
            let init = InitialInput { my_team_id: tid };
            let parsed: InitialInput = init.to_string().parse()?;
            assert_eq!(parsed, init);
        }
        Ok(())
    }

    #[test]
    fn entity_round_trip() -> Result<()> {
        let e = Entity {
            id: 3,
            kind: EntityKind::Snaffle,
            x: 100,
            y: 200,
            vx: -5,
            vy: 7,
            state: 1,
        };
        let parsed: Entity = e.to_string().parse()?;
        assert_eq!(parsed, e);
        Ok(())
    }

    #[test]
    fn wizard_action_move_round_trip() -> Result<()> {
        let a = WizardAction::move_to(8000, 3750, 150);
        let parsed: WizardAction = a.to_string().parse()?;
        assert_eq!(parsed, a);
        assert_eq!(a.to_string(), "MOVE 8000 3750 150");
        Ok(())
    }

    #[test]
    fn wizard_action_throw_round_trip() -> Result<()> {
        let a = WizardAction::throw_to(16000, 3750, 500);
        let parsed: WizardAction = a.to_string().parse()?;
        assert_eq!(parsed, a);
        assert_eq!(a.to_string(), "THROW 16000 3750 500");
        Ok(())
    }

    #[test]
    fn wizard_action_spell_round_trip() -> Result<()> {
        for kind in [
            ActionKind::Obliviate,
            ActionKind::Petrificus,
            ActionKind::Accio,
            ActionKind::Flipendo,
        ] {
            let a = WizardAction::cast(kind, 7);
            let parsed: WizardAction = a.to_string().parse()?;
            assert_eq!(parsed, a);
        }
        assert_eq!(
            WizardAction::cast(ActionKind::Flipendo, 3).to_string(),
            "FLIPENDO 3",
        );
        Ok(())
    }

    #[test]
    fn turn_input_round_trip() -> Result<()> {
        let input = TurnInput {
            my_score: 1,
            my_magic: 42,
            opp_score: 0,
            opp_magic: 50,
            entities: vec![
                Entity {
                    id: 0,
                    kind: EntityKind::Wizard,
                    x: 1000,
                    y: 1750,
                    vx: 0,
                    vy: 0,
                    state: 0,
                },
                Entity {
                    id: 1,
                    kind: EntityKind::Wizard,
                    x: 1000,
                    y: 5750,
                    vx: 0,
                    vy: 0,
                    state: 1,
                },
                Entity {
                    id: 5,
                    kind: EntityKind::Snaffle,
                    x: 8000,
                    y: 3750,
                    vx: 12,
                    vy: -7,
                    state: 0,
                },
            ],
        };
        let mut buf = Vec::new();
        input.write_to(&mut buf)?;
        let parsed = TurnInput::read_from(&mut buf.as_slice())?;
        assert_eq!(parsed.my_score, input.my_score);
        assert_eq!(parsed.my_magic, input.my_magic);
        assert_eq!(parsed.opp_score, input.opp_score);
        assert_eq!(parsed.opp_magic, input.opp_magic);
        assert_eq!(parsed.entities, input.entities);
        Ok(())
    }

    #[test]
    fn turn_output_round_trip() -> Result<()> {
        let out = TurnOutput {
            primary: WizardAction::move_to(8000, 3750, 150),
            secondary: WizardAction::cast(ActionKind::Flipendo, 5),
        };
        let mut buf = Vec::new();
        out.write_to(&mut buf)?;
        let parsed = TurnOutput::read_from(&mut buf.as_slice())?;
        assert_eq!(parsed, out);
        Ok(())
    }
}
