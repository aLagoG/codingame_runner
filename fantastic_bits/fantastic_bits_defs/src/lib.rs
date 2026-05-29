// Treat `improper_ctypes` as an error. The `unsafe extern "C" { ... }` block
// below references `TurnInputFFI<'_>` and `TurnResult<TurnOutput>`; if either
// drops `#[repr(C)]` (or gains a non-FFI-safe field) the lint fires at the
// extern block — the closest thing Rust has to a "must be repr(C)" check.
#![deny(improper_ctypes)]

use std::{
    fmt::Display,
    io::{BufRead, Write},
    marker::PhantomData,
    str::FromStr,
};

use anyhow::{Context, bail};
use common::{
    Defs, NoInitialInput, NoInitialInputFfi, ReadFrom, SingleLine, TurnResult, WireInput,
    WireInputFfi, WireOutput, WriteTo,
};
use serde::{Deserialize, Serialize};

/// Bumped on any wire-type change. Plugins built against an older
/// `fantastic_bits_defs` export an older value; `PluginPlayer::load` reads it
/// and refuses mismatches before any UB-prone call lands.
pub const ABI_VERSION: u32 = 1;

// ============================================================
//  Wire-level data types
// ============================================================

/// Tags for entities the engine emits per tick.
#[repr(u8)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
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
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[repr(u8)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutput {
    pub primary: WizardAction,
    pub secondary: WizardAction,
}

/// Asserts `TurnOutput` satisfies the full bundled contract — see
/// [`common::WireOutput`].
impl WireOutput for TurnOutput {}

/// Per-tick input the engine hands each player. `entities` shrinks as
/// snaffles get scored; outer length matches `<num_entities>` on the wire.
pub struct TurnInput {
    pub my_score: i32,
    pub my_magic: i32,
    pub opp_score: i32,
    pub opp_magic: i32,
    pub entities: Vec<Entity>,
}

/// Borrowed view of [`TurnInput`] — what `decide` actually sees.
pub struct TurnRef<'a> {
    pub my_score: i32,
    pub my_magic: i32,
    pub opp_score: i32,
    pub opp_magic: i32,
    pub entities: &'a [Entity],
}

/// `#[repr(C)]` FFI mirror of [`TurnInput`]. Fields are private — the only
/// way to obtain a `TurnInputFFI<'a>` is via `TurnInput::as_ffi`, which
/// establishes the invariants `as_ref` relies on:
///   1. `entities` is a valid, properly-aligned pointer to a contiguous
///      array of `Entity`s.
///   2. The array has at least `num_entities` elements.
///   3. The memory is live for `'a` (enforced by lifetime + PhantomData).
#[repr(C)]
#[derive(Debug)]
pub struct TurnInputFFI<'a> {
    my_score: i32,
    my_magic: i32,
    opp_score: i32,
    opp_magic: i32,
    entities: *const Entity,
    num_entities: usize,
    _marker: PhantomData<&'a [Entity]>,
}

impl TurnInputFFI<'_> {
    pub fn my_score(&self) -> i32 {
        self.my_score
    }
    pub fn my_magic(&self) -> i32 {
        self.my_magic
    }
    pub fn opp_score(&self) -> i32 {
        self.opp_score
    }
    pub fn opp_magic(&self) -> i32 {
        self.opp_magic
    }
    pub fn num_entities(&self) -> usize {
        self.num_entities
    }
}

// ============================================================
//  FFI surface
// ============================================================

/// Marker type. Implementing [`common::Defs`] on it is the single line that
/// ratifies this crate's FFI surface — all of `WireInput`, `WireInputFfi`,
/// `WireOutput`, and `ABI_VERSION` are checked at this exact site.
pub struct Ffi;

impl Defs for Ffi {
    type InitialInput = NoInitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    const ABI_VERSION: u32 = ABI_VERSION;
}

// cbindgen reachability root — generates the `extern "C" { ... }` block in
// the C++ header. No symbols introduced into `_defs.rlib`; the real
// `initialize` / `take_turn` / `abi_version` are emitted by
// `common::ffi_bot!` in the bot crate. Keep in sync with that macro.
// `TurnResult` is generic over the per-game output; cbindgen monomorphises
// it into a concrete C++ struct.
unsafe extern "C" {
    pub fn initialize(input: NoInitialInputFfi<'_>);
    pub fn take_turn(input: TurnInputFFI<'_>) -> TurnResult<TurnOutput>;
    pub fn abi_version() -> u32;
}

// ============================================================
//  Wire-input glue
// ============================================================

impl WireInput for TurnInput {
    type Ffi<'a> = TurnInputFFI<'a>;
    type Ref<'a> = TurnRef<'a>;

    fn as_ffi(&self) -> TurnInputFFI<'_> {
        TurnInputFFI {
            my_score: self.my_score,
            my_magic: self.my_magic,
            opp_score: self.opp_score,
            opp_magic: self.opp_magic,
            entities: self.entities.as_ptr(),
            num_entities: self.entities.len(),
            _marker: PhantomData,
        }
    }

    fn as_ref(&self) -> TurnRef<'_> {
        TurnRef {
            my_score: self.my_score,
            my_magic: self.my_magic,
            opp_score: self.opp_score,
            opp_magic: self.opp_magic,
            entities: &self.entities,
        }
    }
}

impl<'a> WireInputFfi<'a> for TurnInputFFI<'a> {
    type Ref = TurnRef<'a>;

    /// Safe because every `TurnInputFFI<'a>` is constructed by `as_ffi`,
    /// which establishes the documented invariants.
    fn as_ref(&self) -> TurnRef<'a> {
        TurnRef {
            my_score: self.my_score,
            my_magic: self.my_magic,
            opp_score: self.opp_score,
            opp_magic: self.opp_magic,
            entities: unsafe { std::slice::from_raw_parts(self.entities, self.num_entities) },
        }
    }
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
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "WIZARD" => EntityKind::Wizard,
            "OPPONENT_WIZARD" => EntityKind::OpponentWizard,
            "SNAFFLE" => EntityKind::Snaffle,
            "BLUDGER" => EntityKind::Bludger,
            _ => bail!("Unrecognized entity kind {s:?}"),
        })
    }
}

impl FromStr for Entity {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split_whitespace();
        let id: i32 = it.next().context("missing id")?.parse()?;
        let kind: EntityKind = it.next().context("missing kind")?.parse()?;
        let x: i32 = it.next().context("missing x")?.parse()?;
        let y: i32 = it.next().context("missing y")?.parse()?;
        let vx: i32 = it.next().context("missing vx")?.parse()?;
        let vy: i32 = it.next().context("missing vy")?.parse()?;
        let state: i32 = it.next().context("missing state")?.parse()?;
        Ok(Entity {
            id,
            kind,
            x,
            y,
            vx,
            vy,
            state,
        })
    }
}

impl FromStr for ActionKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "MOVE" => ActionKind::Move,
            "THROW" => ActionKind::Throw,
            "OBLIVIATE" => ActionKind::Obliviate,
            "PETRIFICUS" => ActionKind::Petrificus,
            "ACCIO" => ActionKind::Accio,
            "FLIPENDO" => ActionKind::Flipendo,
            _ => bail!("Unrecognized action kind {s:?}"),
        })
    }
}

impl FromStr for WizardAction {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split_whitespace();
        let kind: ActionKind = it.next().context("missing action kind")?.parse()?;
        match kind {
            ActionKind::Move | ActionKind::Throw => {
                let x: i32 = it.next().context("missing x")?.parse()?;
                let y: i32 = it.next().context("missing y")?.parse()?;
                let power: i32 = it.next().context("missing power/thrust")?.parse()?;
                Ok(WizardAction {
                    kind,
                    x,
                    y,
                    power,
                    target_id: 0,
                })
            }
            _ => {
                let target_id: i32 = it.next().context("missing target id")?.parse()?;
                Ok(WizardAction {
                    kind,
                    x: 0,
                    y: 0,
                    power: 0,
                    target_id,
                })
            }
        }
    }
}

impl FromStr for TurnInput {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::read_from(&mut s.as_bytes())
    }
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
    fn read_from(r: &mut impl BufRead) -> anyhow::Result<Self> {
        let (my_score, my_magic) = read_two_ints(r).context("reading my score / magic")?;
        let (opp_score, opp_magic) = read_two_ints(r).context("reading opp score / magic")?;
        let num: i32 = read_one_int(r).context("reading entity count")?;
        if num < 0 {
            bail!("negative entity count {num}");
        }
        let mut entities = Vec::with_capacity(num as usize);
        for i in 0..num {
            let mut buf = String::new();
            r.read_line(&mut buf)
                .with_context(|| format!("reading entity {i}"))?;
            entities.push(buf.parse().with_context(|| format!("parsing entity {i}"))?);
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
    fn read_from(r: &mut impl BufRead) -> anyhow::Result<Self> {
        let mut a = String::new();
        r.read_line(&mut a).context("reading primary wizard line")?;
        let mut b = String::new();
        r.read_line(&mut b)
            .context("reading secondary wizard line")?;
        Ok(TurnOutput {
            primary: a.parse().context("parsing primary wizard action")?,
            secondary: b.parse().context("parsing secondary wizard action")?,
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

fn read_one_int(r: &mut impl BufRead) -> anyhow::Result<i32> {
    let mut buf = String::new();
    r.read_line(&mut buf)?;
    Ok(buf.trim().parse()?)
}

fn read_two_ints(r: &mut impl BufRead) -> anyhow::Result<(i32, i32)> {
    let mut buf = String::new();
    r.read_line(&mut buf)?;
    let (a, b) = buf
        .trim()
        .split_once(' ')
        .with_context(|| format!("expected two ints, got {buf:?}"))?;
    Ok((a.parse()?, b.parse()?))
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

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
