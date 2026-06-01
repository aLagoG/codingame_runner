//! Fantastic Bits engine. Continuous-space physics with collision-aware
//! integration; spell, magic, and grab/throw state machine; bludger AI.
//!
//! Coordinate system + constants come from the CodinGame referee
//! (`Referee.java`) — when the in-game statement disagrees, the
//! referee wins (replay parity is the goal). Map: 16000 × 7500 units,
//! (0, 0) top-left. Team 0 goal at `x = 0`, team 1 at `x = 16000`.
//!
//! Physics primitives live in `physics`. Game-level logic (spell
//! state, magic accounting, AI, scoring, end conditions) lives here.

use common::engine::{FfiGame, Game, GameRng, PlayerId};
use fantastic_bits_defs::{
    ActionKind, Entity, EntityKind, InitialInput, TurnInput, TurnOutput, WizardAction,
};
use rand::RngExt;

mod physics;

use physics::{V2, WallSide, round_half_away};

// ============================================================
//  Constants (all match the referee)
// ============================================================

pub const WIDTH: i32 = 16000;
pub const HEIGHT: i32 = 7500;
pub const GOAL_SIZE: i32 = 4000;
pub const GOAL_Y_TOP: i32 = (HEIGHT - GOAL_SIZE) / 2; // 1750
pub const GOAL_Y_BOTTOM: i32 = (HEIGHT + GOAL_SIZE) / 2; // 5750
pub const POLE_RADIUS: i32 = 300;

pub const WIZARD_RADIUS: i32 = 400;
pub const SNAFFLE_RADIUS: i32 = 150;
pub const BLUDGER_RADIUS: i32 = 200;

pub const MAX_TICKS: u32 = 200;
pub const NUM_WIZARDS_PER_PLAYER: usize = 2;
pub const NUM_BLUDGERS: usize = 2;
pub const MIN_SNAFFLES: u32 = 5;
pub const MAX_SNAFFLES: u32 = 7;
pub const SPACE_BETWEEN_POD: i32 = 3000;
pub const MIN_SPACE_BETWEEN_SNAFFLES: i32 = 1250;

// Masses (referee).
pub const POD_MASS: f64 = 1.0;
pub const SNAFFLE_MASS: f64 = 0.5;
pub const BLUDGER_MASS: f64 = 8.0;

// Friction is the velocity DROP per tick: `speed *= 1 - FRICTION_X`.
pub const FRICTION_POD: f64 = 0.25;
pub const FRICTION_SNAFFLE: f64 = 0.25;
pub const FRICTION_BLUDGER: f64 = 0.10;

// Powers / thrusts.
pub const MAX_POD_THRUST: i32 = 150;
pub const MAX_THROW_POWER: i32 = 500;
pub const BLUDGER_THRUST: f64 = 1000.0;

// Spell costs and durations (referee, puzzle variant — matches the
// statement we have).
pub const OBLIVIATE_COST: u32 = 5;
pub const PETRIFICUS_COST: u32 = 10;
pub const ACCIO_COST: u32 = 15;
pub const FLIPENDO_COST: u32 = 20;

pub const OBLIVIATE_DURATION: u32 = 4;
pub const PETRIFICUS_DURATION: u32 = 1;
pub const ACCIO_DURATION: u32 = 6;
pub const FLIPENDO_DURATION: u32 = 3;

pub const MAX_MAGIC: u32 = 100;

/// Turns a wizard must wait after grabbing before they can grab again.
pub const CAPTURE_COOLDOWN: u32 = 3;

// ============================================================
//  State types
// ============================================================

/// Continuous f64 disc state. Wire format projects this to i32 via
/// `round_half_away` only when emitting `input_for`.
#[derive(Debug, Clone, Copy)]
pub struct DiscState {
    pub id: i32,
    pub pos: V2,
    pub vel: V2,
}

#[derive(Debug, Clone)]
pub struct Wizard {
    pub disc: DiscState,
    /// `Some(snaffle_id)` if currently holding.
    pub holding: Option<i32>,
    /// Per-spell-kind active slots (target + countdown). Re-casting
    /// the same kind overwrites the slot on the next step's promote.
    pub spells: PodSpells,
    /// Set to `CAPTURE_COOLDOWN` whenever a held snaffle is processed
    /// at parse time (i.e. the wizard had a snaffle entering the
    /// tick). Decremented once per `step()` after release. Capture is
    /// only allowed when `cooldown == 0`.
    pub cooldown: u32,
    /// What the bot picked this tick — used by viz and `last_action`.
    pub last_action: Option<WizardAction>,
}

#[derive(Debug, Default, Clone)]
pub struct PodSpells {
    pub obliviate: Option<SpellSlot>,
    pub petrificus: Option<SpellSlot>,
    pub accio: Option<SpellSlot>,
    pub flipendo: Option<SpellSlot>,
    /// Cast this tick; promoted to the matching slot at the START of
    /// the next step. Mirrors the referee's `doXxx` flag pattern.
    pub pending: Option<PendingSpell>,
}

#[derive(Debug, Clone, Copy)]
pub struct SpellSlot {
    pub target_id: i32,
    pub remaining: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct PendingSpell {
    pub kind: SpellKind,
    pub target_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpellKind {
    Obliviate,
    Petrificus,
    Accio,
    Flipendo,
}

impl SpellKind {
    fn from_action(k: ActionKind) -> Option<Self> {
        match k {
            ActionKind::Obliviate => Some(SpellKind::Obliviate),
            ActionKind::Petrificus => Some(SpellKind::Petrificus),
            ActionKind::Accio => Some(SpellKind::Accio),
            ActionKind::Flipendo => Some(SpellKind::Flipendo),
            _ => None,
        }
    }

    fn cost(self) -> u32 {
        match self {
            SpellKind::Obliviate => OBLIVIATE_COST,
            SpellKind::Petrificus => PETRIFICUS_COST,
            SpellKind::Accio => ACCIO_COST,
            SpellKind::Flipendo => FLIPENDO_COST,
        }
    }

    fn duration(self) -> u32 {
        match self {
            SpellKind::Obliviate => OBLIVIATE_DURATION,
            SpellKind::Petrificus => PETRIFICUS_DURATION,
            SpellKind::Accio => ACCIO_DURATION,
            SpellKind::Flipendo => FLIPENDO_DURATION,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snaffle {
    pub disc: DiscState,
    /// `Some(wizard_id)` if currently held.
    pub held_by: Option<i32>,
    /// `false` once scored. Dead snaffles stay in the list (their ids
    /// are valid game-wide) but don't appear in bot input or physics.
    pub alive: bool,
    /// Throw force set this tick by a THROW action (caster releases on
    /// release; we add this to the snaffle's velocity after release).
    pub thrust_force: V2,
    /// Other snaffle ids to skip in collision detection until they
    /// physically separate. Populated when a held snaffle overlaps
    /// another snaffle — prevents double-grab and clipping.
    pub ignore_collision: Vec<i32>,
}

#[derive(Debug, Clone)]
pub struct Bludger {
    pub disc: DiscState,
    /// `-1` if none; otherwise the wizard id the bludger last
    /// collided with — it'll skip that target on the next AI pass.
    pub last_victim: i32,
}

#[derive(Debug, Clone)]
pub struct GoalPost {
    pub id: i32,
    pub pos: V2,
}

pub struct FantasticBitsGame {
    tick: u32,
    score: [u32; 2],
    magic: [u32; 2],
    total_snaffles: u32,
    wizards: Vec<Wizard>,
    bludgers: Vec<Bludger>,
    snaffles: Vec<Snaffle>,
    goal_posts: Vec<GoalPost>,
    active: Vec<PlayerId>,
    outcome: Option<FantasticBitsOutcome>,
}

#[derive(Debug, Clone)]
pub struct FantasticBitsOutcome {
    pub winner: Option<PlayerId>,
    pub score: [u32; 2],
    pub standings: Vec<u32>,
}

impl FantasticBitsGame {
    pub fn tick(&self) -> u32 {
        self.tick
    }
    pub fn score(&self) -> [u32; 2] {
        self.score
    }
    pub fn magic(&self) -> [u32; 2] {
        self.magic
    }
    pub fn wizards(&self) -> &[Wizard] {
        &self.wizards
    }
    pub fn bludgers(&self) -> &[Bludger] {
        &self.bludgers
    }
    pub fn snaffles(&self) -> &[Snaffle] {
        &self.snaffles
    }
    pub fn goal_posts(&self) -> &[GoalPost] {
        &self.goal_posts
    }
    pub fn total_snaffles(&self) -> u32 {
        self.total_snaffles
    }
    pub fn score_to_win(&self) -> u32 {
        self.total_snaffles / 2 + 1
    }
}

impl Game for FantasticBitsGame {
    const NAME: &'static str = "fantastic_bits";

    type InitialInput = InitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    type Outcome = FantasticBitsOutcome;

    fn new(num_players: u32, rng: &mut GameRng) -> Self {
        assert_eq!(num_players, 2, "Fantastic Bits is always 2 players");

        let pair_count = 2 + rng.random_range(0u32..2);
        let total_snaffles = 2 * pair_count + 1;

        let mut next_id: i32 = 0;
        let wizards = place_wizards(&mut next_id);
        let snaffles = place_snaffles(pair_count, rng, &mut next_id);
        let bludgers = place_bludgers(&mut next_id);
        let goal_posts = place_goal_posts(&mut next_id);

        FantasticBitsGame {
            tick: 0,
            score: [0, 0],
            magic: [0, 0],
            total_snaffles,
            wizards,
            bludgers,
            snaffles,
            goal_posts,
            active: vec![0, 1],
            outcome: None,
        }
    }

    fn initial_input(&self, player: PlayerId) -> InitialInput {
        InitialInput {
            my_team_id: player as i32,
        }
    }

    fn input_for(&self, player: PlayerId) -> TurnInput {
        let my = player as usize;
        let opp = 1 - my;
        let mut entities: Vec<Entity> = Vec::new();

        for w in &self.wizards {
            let team = wizard_team(w.disc.id);
            let kind = if team == my {
                EntityKind::Wizard
            } else {
                EntityKind::OpponentWizard
            };
            let state = i32::from(w.holding.is_some());
            entities.push(disc_to_entity(&w.disc, kind, state));
        }
        for s in &self.snaffles {
            if !s.alive {
                continue;
            }
            let state = i32::from(s.held_by.is_some());
            entities.push(disc_to_entity(&s.disc, EntityKind::Snaffle, state));
        }
        for b in &self.bludgers {
            entities.push(disc_to_entity(&b.disc, EntityKind::Bludger, b.last_victim));
        }

        TurnInput {
            my_score: self.score[my] as i32,
            my_magic: self.magic[my] as i32,
            opp_score: self.score[opp] as i32,
            opp_magic: self.magic[opp] as i32,
            entities,
        }
    }

    fn step(&mut self, outputs: &[Option<TurnOutput>]) -> Option<FantasticBitsOutcome> {
        // Defensive: if we've already reported an outcome, repeat it.
        if let Some(o) = &self.outcome {
            return Some(o.clone());
        }

        // 1. Activate spells cast last tick.
        self.promote_pending_spells();

        // 2. Reset per-tick scratch on every entity.
        for w in &mut self.wizards {
            w.last_action = None;
        }
        // Snapshot snaffle positions for the ignore-list cleanup so we
        // don't try to borrow `self.snaffles` twice.
        let snaffle_positions: Vec<(i32, V2)> = self
            .snaffles
            .iter()
            .map(|s| (s.disc.id, s.disc.pos))
            .collect();
        for s in &mut self.snaffles {
            s.thrust_force = V2::ZERO;
            let this_pos = s.disc.pos;
            s.ignore_collision
                .retain(|sid| snaffles_still_overlap_pos(*sid, this_pos, &snaffle_positions));
        }

        // 3. Parse outputs: per wizard, set thrust / throw force /
        //    pending spell. Magic deducted here.
        let intents = self.parse_outputs(outputs);

        // 4. Apply pod thrust + release held snaffles + accio cancels
        //    + decrement cooldowns. Set cooldown=3 if pod was holding.
        self.apply_pod_intents(&intents);

        // 5. Apply snaffle throw forces (set above).
        for s in &mut self.snaffles {
            if !s.alive {
                continue;
            }
            s.disc.vel = s.disc.vel.add(s.thrust_force.mul(1.0 / SNAFFLE_MASS));
        }

        // 6. Bludger AI: pick target, set acceleration, apply to vel.
        self.apply_bludger_ai();

        // 7. Spells in referee order: Petrificus → Accio → Flipendo.
        self.apply_petrificus();
        self.apply_accio();
        self.apply_flipendo();

        // 8. +1 magic per player.
        for m in &mut self.magic {
            *m = (*m + 1).min(MAX_MAGIC);
        }

        // 9. Collision-aware physics integration over t ∈ [0, 1].
        self.run_physics_loop();

        // 10. End-of-tick: friction + symmetric-round on all entities.
        for w in &mut self.wizards {
            apply_friction_and_round(&mut w.disc, FRICTION_POD);
        }
        for b in &mut self.bludgers {
            apply_friction_and_round(&mut b.disc, FRICTION_BLUDGER);
        }
        for s in &mut self.snaffles {
            if !s.alive {
                continue;
            }
            apply_friction_and_round(&mut s.disc, FRICTION_SNAFFLE);
        }

        // 11. Decrement active spell durations; cull expired.
        self.decrement_spells();

        // 12. Tick + end check.
        self.tick += 1;
        if let Some(o) = self.check_end() {
            self.active.clear();
            self.outcome = Some(o.clone());
            return Some(o);
        }
        None
    }

    fn active_players(&self) -> &[PlayerId] {
        &self.active
    }

    fn standings(outcome: &FantasticBitsOutcome) -> Vec<u32> {
        outcome.standings.clone()
    }

    fn scores(outcome: &FantasticBitsOutcome) -> Option<Vec<f64>> {
        Some(outcome.score.iter().map(|s| *s as f64).collect())
    }
}

impl FfiGame for FantasticBitsGame {
    type Defs = fantastic_bits_defs::Ffi;
}

// ============================================================
//  Intent parsing
// ============================================================

#[derive(Debug, Clone, Copy)]
struct WizardIntent {
    /// Thrust vector to add to the wizard's velocity this tick.
    /// `V2::ZERO` if no thrust (spell or invalid output).
    thrust: V2,
    /// Pre-release "did this wizard have a snaffle at parse time?"
    /// (Mirrors the referee setting `cooldown = TIME_BEFORE_CAPTURE_AGAIN`
    /// when `pod.snaffle != null` after parsing.)
    was_holding: bool,
}

impl FantasticBitsGame {
    fn parse_outputs(&mut self, outputs: &[Option<TurnOutput>]) -> Vec<WizardIntent> {
        // 4 wizards in fixed id order.
        let mut intents = vec![
            WizardIntent {
                thrust: V2::ZERO,
                was_holding: false,
            };
            self.wizards.len()
        ];

        for player_id in 0..2usize {
            let Some(out) = outputs.get(player_id).and_then(|o| o.as_ref()) else {
                continue;
            };
            for (slot, action) in [out.primary, out.secondary].iter().enumerate() {
                // Player 0 controls wizard ids 0,1; player 1 controls 2,3.
                let wiz_id = player_id * NUM_WIZARDS_PER_PLAYER + slot;
                intents[wiz_id] = self.parse_wizard_action(player_id, wiz_id, *action);
            }
        }
        intents
    }

    fn parse_wizard_action(
        &mut self,
        player_id: usize,
        wiz_id: usize,
        action: WizardAction,
    ) -> WizardIntent {
        self.wizards[wiz_id].last_action = Some(action);
        let was_holding = self.wizards[wiz_id].holding.is_some();

        match action.kind {
            ActionKind::Move => {
                let power = action.power.clamp(0, MAX_POD_THRUST) as f64;
                let thrust =
                    compute_thrust_toward(self.wizards[wiz_id].disc.pos, action.x, action.y, power);
                WizardIntent {
                    thrust,
                    was_holding,
                }
            }
            ActionKind::Throw => {
                // Only valid if currently holding. Referee throws an
                // InvalidInputException; we soft-fail and treat as IDLE.
                if let Some(snaffle_id) = self.wizards[wiz_id].holding {
                    let power = action.power.clamp(0, MAX_THROW_POWER) as f64;
                    let snaffle_idx = self
                        .snaffle_index(snaffle_id)
                        .expect("held snaffle id must resolve");
                    let from = self.wizards[wiz_id].disc.pos;
                    let force = compute_thrust_toward(from, action.x, action.y, power);
                    self.snaffles[snaffle_idx].thrust_force = force;
                }
                WizardIntent {
                    thrust: V2::ZERO,
                    was_holding,
                }
            }
            spell_action @ (ActionKind::Obliviate
            | ActionKind::Petrificus
            | ActionKind::Accio
            | ActionKind::Flipendo) => {
                let kind = SpellKind::from_action(spell_action).unwrap();
                let cost = kind.cost();
                if self.magic[player_id] >= cost
                    && self.spell_target_valid(player_id, kind, action.target_id)
                {
                    self.magic[player_id] -= cost;
                    self.wizards[wiz_id].spells.pending = Some(PendingSpell {
                        kind,
                        target_id: action.target_id,
                    });
                }
                WizardIntent {
                    thrust: V2::ZERO,
                    was_holding,
                }
            }
        }
    }

    /// Replicates the referee's per-spell target validation:
    ///   * Obliviate → target must be a bludger.
    ///   * Accio → target must NOT be a wizard, and alive.
    ///   * Petrificus / Flipendo → target alive; if wizard, must not
    ///     be same-team as caster.
    ///
    /// On invalid target the spell is silently dropped (referee
    /// throws; we drop, to keep the engine resilient to noisy bots).
    fn spell_target_valid(&self, caster_player: usize, kind: SpellKind, target_id: i32) -> bool {
        let target_kind = self.lookup_entity_kind(target_id);
        match (kind, target_kind) {
            (SpellKind::Obliviate, Some(EntityRef::Bludger)) => true,
            (SpellKind::Obliviate, _) => false,
            (SpellKind::Accio, Some(EntityRef::Snaffle | EntityRef::Bludger)) => {
                self.target_alive(target_id)
            }
            (SpellKind::Accio, _) => false,
            (
                SpellKind::Petrificus | SpellKind::Flipendo,
                Some(EntityRef::Snaffle | EntityRef::Bludger),
            ) => self.target_alive(target_id),
            (SpellKind::Petrificus | SpellKind::Flipendo, Some(EntityRef::Wizard)) => {
                let target_team = wizard_team(target_id);
                target_team != caster_player
            }
            _ => false,
        }
    }

    fn target_alive(&self, id: i32) -> bool {
        self.snaffle_index(id)
            .map(|i| self.snaffles[i].alive)
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityRef {
    Wizard,
    Snaffle,
    Bludger,
}

impl FantasticBitsGame {
    fn lookup_entity_kind(&self, id: i32) -> Option<EntityRef> {
        if self.wizards.iter().any(|w| w.disc.id == id) {
            Some(EntityRef::Wizard)
        } else if self.snaffles.iter().any(|s| s.disc.id == id) {
            Some(EntityRef::Snaffle)
        } else if self.bludgers.iter().any(|b| b.disc.id == id) {
            Some(EntityRef::Bludger)
        } else {
            None
        }
    }

    fn snaffle_index(&self, id: i32) -> Option<usize> {
        self.snaffles.iter().position(|s| s.disc.id == id)
    }
}

fn compute_thrust_toward(from: V2, tx: i32, ty: i32, power: f64) -> V2 {
    let dir = V2::new(tx as f64 - from.x, ty as f64 - from.y).normalize();
    dir.mul(power)
}

// ============================================================
//  Pod intents → physics state
// ============================================================

impl FantasticBitsGame {
    fn apply_pod_intents(&mut self, intents: &[WizardIntent]) {
        // Mirror of the referee's `for (Pod p : pods) { ... }` block
        // in updateGame: thrust, release-held, accio-cancel-on-self,
        // accio-cancel-if-target-dead, cooldown.
        for (wiz_id, intent) in intents.iter().enumerate() {
            // Apply thrust.
            let thrust = intent.thrust;
            self.wizards[wiz_id].disc.vel = self.wizards[wiz_id]
                .disc
                .vel
                .add(thrust.mul(1.0 / POD_MASS));

            // Bump cooldown if we entered the tick holding (matches the
            // referee setting it during handlePlayerOutput).
            if intent.was_holding {
                self.wizards[wiz_id].cooldown = CAPTURE_COOLDOWN;
            }

            // Release any held snaffle. While we're at it, cancel an
            // accio targeting the just-held snaffle (statement: "When
            // a wizard grabs a Snaffle using ACCIO, the spell effect
            // stops"). And update the ignore-collision list against
            // any currently-overlapping snaffles.
            if let Some(snaffle_id) = self.wizards[wiz_id].holding.take() {
                if let Some(accio) = self.wizards[wiz_id].spells.accio
                    && accio.target_id == snaffle_id
                {
                    self.wizards[wiz_id].spells.accio = None;
                }

                if let Some(idx) = self.snaffle_index(snaffle_id) {
                    let held_pos = self.snaffles[idx].disc.pos;
                    let radii = (SNAFFLE_RADIUS + SNAFFLE_RADIUS) as f64;
                    // Find other snaffles currently overlapping this
                    // one — they go into both ignore lists so we
                    // don't trigger re-grab chains.
                    let mut overlap_ids: Vec<i32> = Vec::new();
                    for (j, other) in self.snaffles.iter().enumerate() {
                        if j == idx || !other.alive {
                            continue;
                        }
                        if held_pos.sub(other.disc.pos).len() < radii {
                            overlap_ids.push(other.disc.id);
                        }
                    }
                    let held_id = self.snaffles[idx].disc.id;
                    for other_id in &overlap_ids {
                        if !self.snaffles[idx].ignore_collision.contains(other_id) {
                            self.snaffles[idx].ignore_collision.push(*other_id);
                        }
                        if let Some(other_idx) = self.snaffle_index(*other_id)
                            && !self.snaffles[other_idx].ignore_collision.contains(&held_id)
                        {
                            self.snaffles[other_idx].ignore_collision.push(held_id);
                        }
                    }

                    self.snaffles[idx].held_by = None;
                    // Snaffle inherits the wizard's *current* velocity
                    // (the thrust we just applied is included).
                    self.snaffles[idx].disc.vel = self.wizards[wiz_id].disc.vel;
                    self.snaffles[idx].disc.pos = self.wizards[wiz_id].disc.pos;
                }
            }

            // Cancel accio when its target died (scored).
            if let Some(accio) = self.wizards[wiz_id].spells.accio
                && let Some(idx) = self.snaffle_index(accio.target_id)
                && !self.snaffles[idx].alive
            {
                self.wizards[wiz_id].spells.accio = None;
            }

            // Cooldown decrement (referee dec'd unconditionally if > 0).
            if self.wizards[wiz_id].cooldown > 0 {
                self.wizards[wiz_id].cooldown -= 1;
            }
        }
    }
}

// ============================================================
//  Spells (per-pod state machine + force application)
// ============================================================

impl FantasticBitsGame {
    fn promote_pending_spells(&mut self) {
        // At the start of step (mirrors prepareRound): any spell cast
        // last turn (now in `pending`) becomes active in its slot
        // with a fresh full-duration countdown.
        for w in &mut self.wizards {
            if let Some(p) = w.spells.pending.take() {
                let slot = SpellSlot {
                    target_id: p.target_id,
                    remaining: p.kind.duration(),
                };
                match p.kind {
                    SpellKind::Obliviate => w.spells.obliviate = Some(slot),
                    SpellKind::Petrificus => w.spells.petrificus = Some(slot),
                    SpellKind::Accio => w.spells.accio = Some(slot),
                    SpellKind::Flipendo => w.spells.flipendo = Some(slot),
                }
            }
        }
    }

    fn decrement_spells(&mut self) {
        for w in &mut self.wizards {
            for slot in [
                &mut w.spells.obliviate,
                &mut w.spells.petrificus,
                &mut w.spells.accio,
                &mut w.spells.flipendo,
            ] {
                if let Some(s) = slot {
                    if s.remaining > 0 {
                        s.remaining -= 1;
                    }
                    if s.remaining == 0 {
                        *slot = None;
                    }
                }
            }
        }
    }

    fn apply_petrificus(&mut self) {
        for wiz_id in 0..self.wizards.len() {
            let Some(slot) = self.wizards[wiz_id].spells.petrificus else {
                continue;
            };
            self.set_target_velocity(slot.target_id, V2::ZERO);
        }
    }

    fn apply_accio(&mut self) {
        for wiz_id in 0..self.wizards.len() {
            let Some(slot) = self.wizards[wiz_id].spells.accio else {
                continue;
            };
            let caster_pos = self.wizards[wiz_id].disc.pos;
            // Force = (caster - target).normalize * power; power = min(3000 / (dist/1000)², 1000).
            let target_pos = self.target_pos(slot.target_id);
            let Some(target_pos) = target_pos else {
                continue;
            };
            let d = caster_pos.sub(target_pos);
            let dist_sq = d.len_sq();
            if dist_sq == 0.0 {
                continue;
            }
            let power = (3_000.0 * (1_000.0 * 1_000.0 / dist_sq)).min(1_000.0);
            let force = d.normalize().mul(power);
            self.apply_force_to(slot.target_id, force);
        }
    }

    fn apply_flipendo(&mut self) {
        for wiz_id in 0..self.wizards.len() {
            let Some(slot) = self.wizards[wiz_id].spells.flipendo else {
                continue;
            };
            let caster_pos = self.wizards[wiz_id].disc.pos;
            let target_pos = self.target_pos(slot.target_id);
            let Some(target_pos) = target_pos else {
                continue;
            };
            if target_pos == caster_pos {
                continue;
            }
            let d = target_pos.sub(caster_pos);
            let dist_sq = d.len_sq();
            let power = (6_000.0 * (1_000.0 * 1_000.0 / dist_sq)).min(1_000.0);
            let force = d.normalize().mul(power);
            self.apply_force_to(slot.target_id, force);
        }
    }

    fn target_pos(&self, id: i32) -> Option<V2> {
        if let Some(idx) = self.snaffle_index(id) {
            if !self.snaffles[idx].alive {
                return None;
            }
            return Some(self.snaffles[idx].disc.pos);
        }
        if let Some(b) = self.bludgers.iter().find(|b| b.disc.id == id) {
            return Some(b.disc.pos);
        }
        if let Some(w) = self.wizards.iter().find(|w| w.disc.id == id) {
            return Some(w.disc.pos);
        }
        None
    }

    fn set_target_velocity(&mut self, id: i32, vel: V2) {
        if let Some(idx) = self.snaffle_index(id) {
            if self.snaffles[idx].alive {
                self.snaffles[idx].disc.vel = vel;
            }
            return;
        }
        if let Some(b) = self.bludgers.iter_mut().find(|b| b.disc.id == id) {
            b.disc.vel = vel;
            return;
        }
        if let Some(w) = self.wizards.iter_mut().find(|w| w.disc.id == id) {
            w.disc.vel = vel;
        }
    }

    fn apply_force_to(&mut self, id: i32, force: V2) {
        // `applyForce` in the referee: `speed += force / mass`.
        if let Some(idx) = self.snaffle_index(id) {
            if self.snaffles[idx].alive {
                self.snaffles[idx].disc.vel = self.snaffles[idx]
                    .disc
                    .vel
                    .add(force.mul(1.0 / SNAFFLE_MASS));
            }
            return;
        }
        if let Some(b) = self.bludgers.iter_mut().find(|b| b.disc.id == id) {
            b.disc.vel = b.disc.vel.add(force.mul(1.0 / BLUDGER_MASS));
            return;
        }
        if let Some(w) = self.wizards.iter_mut().find(|w| w.disc.id == id) {
            w.disc.vel = w.disc.vel.add(force.mul(1.0 / POD_MASS));
        }
    }
}

// ============================================================
//  Bludger AI
// ============================================================

impl FantasticBitsGame {
    fn apply_bludger_ai(&mut self) {
        for b_idx in 0..self.bludgers.len() {
            let bludger_id = self.bludgers[b_idx].disc.id;
            let bludger_pos = self.bludgers[b_idx].disc.pos;
            let last_victim = self.bludgers[b_idx].last_victim;

            let mut closest: Option<(usize, f64)> = None;
            for (i, w) in self.wizards.iter().enumerate() {
                if w.disc.id == last_victim {
                    continue;
                }
                if self.bludger_obliviated_by_team(bludger_id, wizard_team(w.disc.id)) {
                    continue;
                }
                let d2 = w.disc.pos.sub(bludger_pos).len_sq();
                match closest {
                    Some((_, best_d2)) if best_d2 <= d2 => {}
                    _ => closest = Some((i, d2)),
                }
            }
            if let Some((i, _)) = closest {
                let dir = self.wizards[i].disc.pos.sub(bludger_pos).normalize();
                let force = dir.mul(BLUDGER_THRUST);
                self.bludgers[b_idx].disc.vel = self.bludgers[b_idx]
                    .disc
                    .vel
                    .add(force.mul(1.0 / BLUDGER_MASS));
            }
        }
    }

    fn bludger_obliviated_by_team(&self, bludger_id: i32, team: usize) -> bool {
        // True iff any wizard on `team` has an active Obliviate
        // targeting this bludger.
        self.wizards.iter().any(|w| {
            wizard_team(w.disc.id) == team
                && w.spells
                    .obliviate
                    .is_some_and(|s| s.target_id == bludger_id)
        })
    }
}

// ============================================================
//  Physics loop
// ============================================================

#[derive(Debug, Clone, Copy)]
enum CollisionEvent {
    Wall {
        kind: EntityRef,
        id: i32,
        side: WallSide,
        time: f64,
    },
    EntityEntity {
        a_kind: EntityRef,
        a_id: i32,
        b_kind: EntityRef,
        b_id: i32,
        time: f64,
    },
    EntityGoalPost {
        kind: EntityRef,
        id: i32,
        post_id: i32,
        time: f64,
    },
    SnaffleCapture {
        snaffle_id: i32,
        wizard_id: i32,
        time: f64,
    },
    SnaffleScore {
        snaffle_id: i32,
        scoring_player: usize,
        time: f64,
    },
}

impl CollisionEvent {
    fn time(&self) -> f64 {
        match self {
            CollisionEvent::Wall { time, .. }
            | CollisionEvent::EntityEntity { time, .. }
            | CollisionEvent::EntityGoalPost { time, .. }
            | CollisionEvent::SnaffleCapture { time, .. }
            | CollisionEvent::SnaffleScore { time, .. } => *time,
        }
    }
}

impl FantasticBitsGame {
    fn run_physics_loop(&mut self) {
        let mut t = 0.0;
        // Cap iterations defensively — referee has no cap, but if our
        // numerics ever stutter we'd rather end the tick than spin.
        for _ in 0..64 {
            if t >= 1.0 {
                break;
            }
            let dt = self.physics_substep(t);
            t += dt;
            if dt <= 0.0 {
                // Numerical floor: nothing happened, escape rather than loop.
                break;
            }
        }
    }

    /// Advance entities up to the earliest collision in `[t, 1]`,
    /// resolve all simultaneous collisions, return the delta consumed.
    fn physics_substep(&mut self, t: f64) -> f64 {
        let remaining = 1.0 - t;
        let mut events = self.collect_collisions(t, remaining);
        if events.is_empty() {
            self.advance_all(remaining);
            return remaining;
        }
        events.sort_by(|a, b| a.time().partial_cmp(&b.time()).unwrap());
        let first_time = events[0].time();
        let dt = first_time - t;
        if dt > 0.0 {
            self.advance_all(dt);
        }
        // Resolve every event whose time is within ε of the earliest.
        for ev in &events {
            if ev.time() > first_time + physics::EPSILON {
                break;
            }
            self.resolve_event(*ev);
        }
        // `endRound` runs only once per tick (outside this loop), so
        // returning dt (could be 0) is fine — the outer loop's
        // `t >= 1` exit handles termination.
        (first_time - t).max(physics::EPSILON.min(remaining))
    }

    fn advance_all(&mut self, dt: f64) {
        for w in &mut self.wizards {
            w.disc.pos = w.disc.pos.add(w.disc.vel.mul(dt));
        }
        for b in &mut self.bludgers {
            b.disc.pos = b.disc.pos.add(b.disc.vel.mul(dt));
        }
        for s in &mut self.snaffles {
            if !s.alive {
                continue;
            }
            s.disc.pos = s.disc.pos.add(s.disc.vel.mul(dt));
        }
    }

    fn collect_collisions(&self, t: f64, remaining: f64) -> Vec<CollisionEvent> {
        let mut out = Vec::new();

        // Pods: walls, goal posts, other pods, snaffle capture.
        for (i, w) in self.wizards.iter().enumerate() {
            push_wall_hit(
                &mut out,
                EntityRef::Wizard,
                &w.disc,
                WIZARD_RADIUS as f64,
                t,
                remaining,
                None,
            );
            for post in &self.goal_posts {
                push_disc_static_toi(
                    &mut out,
                    EntityRef::Wizard,
                    &w.disc,
                    WIZARD_RADIUS as f64,
                    post.id,
                    post.pos,
                    POLE_RADIUS as f64,
                    t,
                    remaining,
                );
            }
            for (j, w2) in self.wizards.iter().enumerate().take(i) {
                push_disc_disc_toi(
                    &mut out,
                    EntityRef::Wizard,
                    &w.disc,
                    WIZARD_RADIUS as f64,
                    EntityRef::Wizard,
                    &w2.disc,
                    WIZARD_RADIUS as f64,
                    t,
                    remaining,
                );
                let _ = j;
            }
            // Snaffle capture: only if not currently holding AND
            // cooldown is 0 (after the dec we did in apply_pod_intents).
            if w.holding.is_none() && w.cooldown == 0 {
                self.push_snaffle_capture_events(&mut out, i, t, remaining);
            }
        }

        // Snaffles: goal posts, walls (mouth-aware), other snaffles, score.
        for (i, s) in self.snaffles.iter().enumerate() {
            if !s.alive || s.held_by.is_some() {
                continue;
            }
            for post in &self.goal_posts {
                push_disc_static_toi(
                    &mut out,
                    EntityRef::Snaffle,
                    &s.disc,
                    SNAFFLE_RADIUS as f64,
                    post.id,
                    post.pos,
                    POLE_RADIUS as f64,
                    t,
                    remaining,
                );
            }
            // Snaffle wall: skip left/right walls inside the goal mouth.
            if let Some(hit) = physics::toi_snaffle_wall(
                s.disc.pos,
                s.disc.vel,
                SNAFFLE_RADIUS as f64,
                WIDTH as f64,
                HEIGHT as f64,
                GOAL_Y_TOP as f64,
                GOAL_Y_BOTTOM as f64,
            ) && hit.t > 0.0
                && hit.t <= remaining
            {
                out.push(CollisionEvent::Wall {
                    kind: EntityRef::Snaffle,
                    id: s.disc.id,
                    side: hit.side,
                    time: t + hit.t,
                });
            }
            // Snaffle vs other snaffles.
            for (j, s2) in self.snaffles.iter().enumerate().take(i) {
                if !s2.alive || s2.held_by.is_some() || s.ignore_collision.contains(&s2.disc.id) {
                    continue;
                }
                push_disc_disc_toi(
                    &mut out,
                    EntityRef::Snaffle,
                    &s.disc,
                    SNAFFLE_RADIUS as f64,
                    EntityRef::Snaffle,
                    &s2.disc,
                    SNAFFLE_RADIUS as f64,
                    t,
                    remaining,
                );
                let _ = j;
            }
            // Score line: snaffle crosses x=0 or x=WIDTH.
            if let Some((dt, side)) =
                physics::toi_snaffle_score(s.disc.pos, s.disc.vel, WIDTH as f64)
                && dt <= remaining
            {
                let scoring = match side {
                    physics::ScoreSide::Right => 0, // team 0's right goal
                    physics::ScoreSide::Left => 1,  // team 1's left goal
                };
                out.push(CollisionEvent::SnaffleScore {
                    snaffle_id: s.disc.id,
                    scoring_player: scoring,
                    time: t + dt,
                });
            }
            // Also fire immediately if the snaffle's centre is already past
            // the line (mirrors `Snaffle.checkGoalCollisions`).
            if s.disc.pos.x >= WIDTH as f64 {
                out.push(CollisionEvent::SnaffleScore {
                    snaffle_id: s.disc.id,
                    scoring_player: 0,
                    time: t,
                });
            } else if s.disc.pos.x <= 0.0 {
                out.push(CollisionEvent::SnaffleScore {
                    snaffle_id: s.disc.id,
                    scoring_player: 1,
                    time: t,
                });
            }
        }

        // Bludgers: walls, goal posts, other bludgers, all pods, all snaffles.
        for (i, b) in self.bludgers.iter().enumerate() {
            push_wall_hit(
                &mut out,
                EntityRef::Bludger,
                &b.disc,
                BLUDGER_RADIUS as f64,
                t,
                remaining,
                None,
            );
            for post in &self.goal_posts {
                push_disc_static_toi(
                    &mut out,
                    EntityRef::Bludger,
                    &b.disc,
                    BLUDGER_RADIUS as f64,
                    post.id,
                    post.pos,
                    POLE_RADIUS as f64,
                    t,
                    remaining,
                );
            }
            for (j, b2) in self.bludgers.iter().enumerate().take(i) {
                push_disc_disc_toi(
                    &mut out,
                    EntityRef::Bludger,
                    &b.disc,
                    BLUDGER_RADIUS as f64,
                    EntityRef::Bludger,
                    &b2.disc,
                    BLUDGER_RADIUS as f64,
                    t,
                    remaining,
                );
                let _ = j;
            }
            for w in &self.wizards {
                push_disc_disc_toi(
                    &mut out,
                    EntityRef::Bludger,
                    &b.disc,
                    BLUDGER_RADIUS as f64,
                    EntityRef::Wizard,
                    &w.disc,
                    WIZARD_RADIUS as f64,
                    t,
                    remaining,
                );
            }
            for s in &self.snaffles {
                if !s.alive || s.held_by.is_some() {
                    continue;
                }
                push_disc_disc_toi(
                    &mut out,
                    EntityRef::Bludger,
                    &b.disc,
                    BLUDGER_RADIUS as f64,
                    EntityRef::Snaffle,
                    &s.disc,
                    SNAFFLE_RADIUS as f64,
                    t,
                    remaining,
                );
            }
            let _ = i;
        }

        out
    }

    fn push_snaffle_capture_events(
        &self,
        out: &mut Vec<CollisionEvent>,
        wiz_idx: usize,
        t: f64,
        remaining: f64,
    ) {
        let w = &self.wizards[wiz_idx];
        let capture_range = (WIZARD_RADIUS as f64) - 1.0;
        let cap_sq = capture_range * capture_range;

        // (a) Snaffle already inside capture range → fire at t.
        let mut closest: Option<(i32, f64)> = None;
        for s in &self.snaffles {
            if !s.alive || s.held_by.is_some() {
                continue;
            }
            let d2 = s.disc.pos.sub(w.disc.pos).len_sq();
            if d2 <= cap_sq {
                match closest {
                    Some((_, best)) if best <= d2 => {}
                    _ => closest = Some((s.disc.id, d2)),
                }
            }
        }
        if let Some((sid, _)) = closest {
            out.push(CollisionEvent::SnaffleCapture {
                snaffle_id: sid,
                wizard_id: w.disc.id,
                time: t,
            });
        }

        // (b) TOI capture: snaffle would enter capture range during (0, remaining].
        for s in &self.snaffles {
            if !s.alive || s.held_by.is_some() {
                continue;
            }
            if Some(s.disc.id) == closest.map(|(id, _)| id) {
                continue;
            }
            let Some(toi) = physics::toi_disc_disc(
                w.disc.pos,
                w.disc.vel,
                s.disc.pos,
                s.disc.vel,
                capture_range,
            ) else {
                continue;
            };
            if toi > 0.0 && toi <= remaining {
                out.push(CollisionEvent::SnaffleCapture {
                    snaffle_id: s.disc.id,
                    wizard_id: w.disc.id,
                    time: t + toi,
                });
            }
        }
    }
}

fn push_wall_hit(
    out: &mut Vec<CollisionEvent>,
    kind: EntityRef,
    disc: &DiscState,
    radius: f64,
    t: f64,
    remaining: f64,
    _: Option<()>,
) {
    if let Some(hit) =
        physics::toi_disc_wall(disc.pos, disc.vel, radius, WIDTH as f64, HEIGHT as f64)
        && hit.t > 0.0
        && hit.t <= remaining
    {
        out.push(CollisionEvent::Wall {
            kind,
            id: disc.id,
            side: hit.side,
            time: t + hit.t,
        });
    }
}

fn push_disc_disc_toi(
    out: &mut Vec<CollisionEvent>,
    a_kind: EntityRef,
    a: &DiscState,
    a_radius: f64,
    b_kind: EntityRef,
    b: &DiscState,
    b_radius: f64,
    t: f64,
    remaining: f64,
) {
    let target_dist = a_radius + b_radius;
    let Some(toi) = physics::toi_disc_disc(a.pos, a.vel, b.pos, b.vel, target_dist) else {
        return;
    };
    if toi > 0.0 && toi <= remaining {
        out.push(CollisionEvent::EntityEntity {
            a_kind,
            a_id: a.id,
            b_kind,
            b_id: b.id,
            time: t + toi,
        });
    }
}

fn push_disc_static_toi(
    out: &mut Vec<CollisionEvent>,
    kind: EntityRef,
    disc: &DiscState,
    radius: f64,
    post_id: i32,
    post_pos: V2,
    post_radius: f64,
    t: f64,
    remaining: f64,
) {
    let target_dist = radius + post_radius;
    let Some(toi) = physics::toi_disc_disc(disc.pos, disc.vel, post_pos, V2::ZERO, target_dist)
    else {
        return;
    };
    if toi > 0.0 && toi <= remaining {
        out.push(CollisionEvent::EntityGoalPost {
            kind,
            id: disc.id,
            post_id,
            time: t + toi,
        });
    }
}

// ============================================================
//  Collision resolution
// ============================================================

impl FantasticBitsGame {
    fn resolve_event(&mut self, ev: CollisionEvent) {
        match ev {
            CollisionEvent::Wall { kind, id, side, .. } => self.resolve_wall(kind, id, side),
            CollisionEvent::EntityEntity {
                a_kind,
                a_id,
                b_kind,
                b_id,
                ..
            } => self.resolve_entity_entity(a_kind, a_id, b_kind, b_id),
            CollisionEvent::EntityGoalPost {
                kind, id, post_id, ..
            } => self.resolve_entity_goalpost(kind, id, post_id),
            CollisionEvent::SnaffleCapture {
                snaffle_id,
                wizard_id,
                ..
            } => self.resolve_snaffle_capture(snaffle_id, wizard_id),
            CollisionEvent::SnaffleScore {
                snaffle_id,
                scoring_player,
                ..
            } => self.resolve_snaffle_score(snaffle_id, scoring_player),
        }
    }

    fn resolve_wall(&mut self, kind: EntityRef, id: i32, side: WallSide) {
        let (pos, vel, radius) = match kind {
            EntityRef::Wizard => {
                let w = self.wizards.iter().find(|w| w.disc.id == id).unwrap();
                (w.disc.pos, w.disc.vel, WIZARD_RADIUS as f64)
            }
            EntityRef::Snaffle => {
                let idx = self.snaffle_index(id).unwrap();
                if !self.snaffles[idx].alive || self.snaffles[idx].held_by.is_some() {
                    return;
                }
                (
                    self.snaffles[idx].disc.pos,
                    self.snaffles[idx].disc.vel,
                    SNAFFLE_RADIUS as f64,
                )
            }
            EntityRef::Bludger => {
                let b = self.bludgers.iter().find(|b| b.disc.id == id).unwrap();
                (b.disc.pos, b.disc.vel, BLUDGER_RADIUS as f64)
            }
        };
        let (new_vel, dp) =
            physics::resolve_wall(side, pos, vel, radius, WIDTH as f64, HEIGHT as f64);
        self.write_disc(kind, id, |d| {
            d.vel = new_vel;
            d.pos = d.pos.add(dp);
        });
    }

    fn resolve_entity_entity(
        &mut self,
        a_kind: EntityRef,
        a_id: i32,
        b_kind: EntityRef,
        b_id: i32,
    ) {
        let (a_pos, a_vel, a_mass, a_radius) = self.disc_view(a_kind, a_id);
        let (b_pos, b_vel, b_mass, b_radius) = self.disc_view(b_kind, b_id);
        let r = physics::resolve_dyn_dyn(
            a_pos, a_vel, a_mass, a_radius, b_pos, b_vel, b_mass, b_radius,
        );
        self.write_disc(a_kind, a_id, |d| {
            d.vel = d.vel.add(r.dv_a);
            d.pos = d.pos.add(r.dp_a);
        });
        self.write_disc(b_kind, b_id, |d| {
            d.vel = d.vel.add(r.dv_b);
            d.pos = d.pos.add(r.dp_b);
        });
        // Track bludger last-victim for the AI.
        if a_kind == EntityRef::Bludger && b_kind == EntityRef::Wizard {
            let b = self
                .bludgers
                .iter_mut()
                .find(|b| b.disc.id == a_id)
                .unwrap();
            b.last_victim = b_id;
        } else if b_kind == EntityRef::Bludger && a_kind == EntityRef::Wizard {
            let b = self
                .bludgers
                .iter_mut()
                .find(|b| b.disc.id == b_id)
                .unwrap();
            b.last_victim = a_id;
        }
    }

    fn resolve_entity_goalpost(&mut self, kind: EntityRef, id: i32, post_id: i32) {
        let (pos, vel, mass, radius) = self.disc_view(kind, id);
        let post = self.goal_posts.iter().find(|p| p.id == post_id).unwrap();
        let r = physics::resolve_dyn_static(pos, vel, mass, radius, post.pos, POLE_RADIUS as f64);
        self.write_disc(kind, id, |d| {
            d.vel = d.vel.add(r.dv);
            d.pos = d.pos.add(r.dp);
        });
    }

    fn resolve_snaffle_capture(&mut self, snaffle_id: i32, wizard_id: i32) {
        let wiz_idx = self
            .wizards
            .iter()
            .position(|w| w.disc.id == wizard_id)
            .unwrap();
        if self.wizards[wiz_idx].holding.is_some() {
            return;
        }
        // Tie-breaker (referee): if any other unheld pod is closer to
        // this snaffle, they win the capture instead — so skip ours.
        let snaffle_idx = self.snaffle_index(snaffle_id).unwrap();
        if !self.snaffles[snaffle_idx].alive || self.snaffles[snaffle_idx].held_by.is_some() {
            return;
        }
        let snaffle_pos = self.snaffles[snaffle_idx].disc.pos;
        let our_d2 = snaffle_pos.sub(self.wizards[wiz_idx].disc.pos).len_sq() + physics::EPSILON;
        for (j, other) in self.wizards.iter().enumerate() {
            if j == wiz_idx || other.holding.is_some() {
                continue;
            }
            let their_d2 = snaffle_pos.sub(other.disc.pos).len_sq();
            if our_d2 >= their_d2 {
                return;
            }
        }

        // Capture: lock snaffle to wizard.
        self.wizards[wiz_idx].holding = Some(snaffle_id);
        self.snaffles[snaffle_idx].held_by = Some(wizard_id);
        self.snaffles[snaffle_idx].disc.pos = self.wizards[wiz_idx].disc.pos;
        self.snaffles[snaffle_idx].disc.vel = self.wizards[wiz_idx].disc.vel;
    }

    fn resolve_snaffle_score(&mut self, snaffle_id: i32, scoring_player: usize) {
        let idx = self.snaffle_index(snaffle_id).unwrap();
        if !self.snaffles[idx].alive {
            return;
        }
        self.snaffles[idx].alive = false;
        self.snaffles[idx].held_by = None;
        self.score[scoring_player] += 1;
    }

    fn disc_view(&self, kind: EntityRef, id: i32) -> (V2, V2, f64, f64) {
        match kind {
            EntityRef::Wizard => {
                let w = self.wizards.iter().find(|w| w.disc.id == id).unwrap();
                (w.disc.pos, w.disc.vel, POD_MASS, WIZARD_RADIUS as f64)
            }
            EntityRef::Snaffle => {
                let idx = self.snaffle_index(id).unwrap();
                let s = &self.snaffles[idx];
                (s.disc.pos, s.disc.vel, SNAFFLE_MASS, SNAFFLE_RADIUS as f64)
            }
            EntityRef::Bludger => {
                let b = self.bludgers.iter().find(|b| b.disc.id == id).unwrap();
                (b.disc.pos, b.disc.vel, BLUDGER_MASS, BLUDGER_RADIUS as f64)
            }
        }
    }

    fn write_disc<F: FnOnce(&mut DiscState)>(&mut self, kind: EntityRef, id: i32, f: F) {
        match kind {
            EntityRef::Wizard => {
                let w = self.wizards.iter_mut().find(|w| w.disc.id == id).unwrap();
                f(&mut w.disc);
                // A held snaffle tracks its wizard.
                if let Some(sid) = w.holding {
                    let pos = w.disc.pos;
                    let vel = w.disc.vel;
                    if let Some(idx) = self.snaffle_index(sid) {
                        self.snaffles[idx].disc.pos = pos;
                        self.snaffles[idx].disc.vel = vel;
                    }
                }
            }
            EntityRef::Snaffle => {
                let idx = self.snaffle_index(id).unwrap();
                f(&mut self.snaffles[idx].disc);
            }
            EntityRef::Bludger => {
                let b = self.bludgers.iter_mut().find(|b| b.disc.id == id).unwrap();
                f(&mut b.disc);
            }
        }
    }
}

// ============================================================
//  End check / outcome
// ============================================================

impl FantasticBitsGame {
    fn check_end(&self) -> Option<FantasticBitsOutcome> {
        let alive_snaffles = self.snaffles.iter().filter(|s| s.alive).count();
        let max_score = self.score[0].max(self.score[1]);
        if alive_snaffles == 0 || max_score >= self.score_to_win() || self.tick >= MAX_TICKS {
            return Some(self.make_outcome());
        }
        None
    }

    fn make_outcome(&self) -> FantasticBitsOutcome {
        let (winner, standings) = match self.score[0].cmp(&self.score[1]) {
            std::cmp::Ordering::Greater => (Some(0), vec![1, 2]),
            std::cmp::Ordering::Less => (Some(1), vec![2, 1]),
            std::cmp::Ordering::Equal => (None, vec![1, 1]),
        };
        FantasticBitsOutcome {
            winner,
            score: self.score,
            standings,
        }
    }
}

// ============================================================
//  Friction + symmetric round
// ============================================================

fn apply_friction_and_round(disc: &mut DiscState, friction: f64) {
    let damp = 1.0 - friction;
    disc.vel = V2::new(disc.vel.x * damp, disc.vel.y * damp);
    disc.pos = V2::new(
        round_half_away(disc.pos.x) as f64,
        round_half_away(disc.pos.y) as f64,
    );
    disc.vel = V2::new(
        round_half_away(disc.vel.x) as f64,
        round_half_away(disc.vel.y) as f64,
    );
}

// ============================================================
//  Setup
// ============================================================

fn wizard_team(id: i32) -> usize {
    if (id as usize) < NUM_WIZARDS_PER_PLAYER {
        0
    } else {
        1
    }
}

fn place_wizards(next_id: &mut i32) -> Vec<Wizard> {
    let mut out = Vec::with_capacity(2 * NUM_WIZARDS_PER_PLAYER);
    for j in 0..2usize {
        let team_sign: f64 = if j % 2 == 0 { -1.0 } else { 1.0 };
        let x = (j as i32 * (WIDTH - 2000) + 1000) as f64;
        for i in 0..NUM_WIZARDS_PER_PLAYER {
            let offset = SPACE_BETWEEN_POD as f64 * i as f64
                - (SPACE_BETWEEN_POD as f64 * (NUM_WIZARDS_PER_PLAYER as f64 - 1.0)) / 2.0;
            let y = (HEIGHT as f64) / 2.0 + team_sign * offset;
            out.push(Wizard {
                disc: DiscState {
                    id: *next_id,
                    pos: V2::new(x, y),
                    vel: V2::ZERO,
                },
                holding: None,
                spells: PodSpells::default(),
                cooldown: 0,
                last_action: None,
            });
            *next_id += 1;
        }
    }
    out
}

fn place_snaffles(pair_count: u32, rng: &mut GameRng, next_id: &mut i32) -> Vec<Snaffle> {
    let x_lo = 2000;
    let x_hi = WIDTH / 2 - 1000;
    let y_lo = 500;
    let y_hi = HEIGHT - 500;
    let min_dist_sq = (MIN_SPACE_BETWEEN_SNAFFLES as f64).powi(2);

    let mut out: Vec<Snaffle> = Vec::with_capacity((2 * pair_count + 1) as usize);
    let mut placed = 0u32;
    while placed < pair_count {
        let x = rng.random_range(x_lo..x_hi) as f64;
        let y = rng.random_range(y_lo..y_hi) as f64;
        let collides = out.iter().any(|s| {
            let dx = s.disc.pos.x - x;
            let dy = s.disc.pos.y - y;
            dx * dx + dy * dy < min_dist_sq
        });
        if collides {
            continue;
        }
        out.push(new_snaffle(*next_id, V2::new(x, y)));
        out.push(new_snaffle(
            *next_id + 1,
            V2::new(WIDTH as f64 - x, HEIGHT as f64 - y),
        ));
        *next_id += 2;
        placed += 1;
    }
    out.push(new_snaffle(
        *next_id,
        V2::new((WIDTH as f64) / 2.0, (HEIGHT as f64) / 2.0),
    ));
    *next_id += 1;
    out
}

fn new_snaffle(id: i32, pos: V2) -> Snaffle {
    Snaffle {
        disc: DiscState {
            id,
            pos,
            vel: V2::ZERO,
        },
        held_by: None,
        alive: true,
        thrust_force: V2::ZERO,
        ignore_collision: Vec::new(),
    }
}

fn place_bludgers(next_id: &mut i32) -> Vec<Bludger> {
    let cx = WIDTH as f64 / 2.0;
    let cy = HEIGHT as f64 / 2.0;
    let dx = (SNAFFLE_RADIUS + 2 * BLUDGER_RADIUS) as f64;
    let mut out = Vec::with_capacity(NUM_BLUDGERS);
    for x in [cx - dx, cx + dx] {
        out.push(Bludger {
            disc: DiscState {
                id: *next_id,
                pos: V2::new(x, cy),
                vel: V2::ZERO,
            },
            last_victim: -1,
        });
        *next_id += 1;
    }
    out
}

fn place_goal_posts(next_id: &mut i32) -> Vec<GoalPost> {
    let positions = [
        V2::new(0.0, GOAL_Y_TOP as f64),
        V2::new(0.0, GOAL_Y_BOTTOM as f64),
        V2::new(WIDTH as f64, GOAL_Y_TOP as f64),
        V2::new(WIDTH as f64, GOAL_Y_BOTTOM as f64),
    ];
    let mut out = Vec::with_capacity(4);
    for pos in positions {
        out.push(GoalPost { id: *next_id, pos });
        *next_id += 1;
    }
    out
}

fn snaffles_still_overlap_pos(other_id: i32, this_pos: V2, positions: &[(i32, V2)]) -> bool {
    let Some(&(_, other_pos)) = positions.iter().find(|(id, _)| *id == other_id) else {
        return false;
    };
    let radii = (SNAFFLE_RADIUS + SNAFFLE_RADIUS) as f64;
    this_pos.sub(other_pos).len() < radii
}

fn disc_to_entity(d: &DiscState, kind: EntityKind, state: i32) -> Entity {
    Entity {
        id: d.id,
        kind,
        x: round_half_away(d.pos.x),
        y: round_half_away(d.pos.y),
        vx: round_half_away(d.vel.x),
        vy: round_half_away(d.vel.y),
        state,
    }
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use common::engine::GameRngSeed;

    fn game(seed: u64) -> FantasticBitsGame {
        let mut rng = GameRng::seed_from_u64(seed);
        FantasticBitsGame::new(2, &mut rng)
    }

    #[test]
    fn placement_invariants() {
        let g = game(0);
        assert_eq!(g.wizards.len(), 4);
        assert_eq!(g.bludgers.len(), 2);
        assert_eq!(g.goal_posts.len(), 4);
        assert!(g.snaffles.len() == 5 || g.snaffles.len() == 7);
        assert!(g.score_to_win() == 3 || g.score_to_win() == 4);

        let center = g.snaffles.last().unwrap();
        assert_eq!(center.disc.pos.x, (WIDTH as f64) / 2.0);
        assert_eq!(center.disc.pos.y, (HEIGHT as f64) / 2.0);
        for i in (0..g.snaffles.len() - 1).step_by(2) {
            let a = &g.snaffles[i].disc;
            let b = &g.snaffles[i + 1].disc;
            assert_eq!(a.pos.x + b.pos.x, WIDTH as f64);
            assert_eq!(a.pos.y + b.pos.y, HEIGHT as f64);
        }
    }

    #[test]
    fn wizard_spawn_matches_referee() {
        let g = game(0);
        let positions: Vec<(f64, f64)> = g
            .wizards
            .iter()
            .map(|w| (w.disc.pos.x, w.disc.pos.y))
            .collect();
        assert_eq!(positions[0], (1000.0, 5250.0));
        assert_eq!(positions[1], (1000.0, 2250.0));
        assert_eq!(positions[2], (15000.0, 2250.0));
        assert_eq!(positions[3], (15000.0, 5250.0));
    }

    #[test]
    fn bludger_spawn_matches_referee() {
        let g = game(0);
        let cx = WIDTH as f64 / 2.0;
        let cy = HEIGHT as f64 / 2.0;
        let off = (SNAFFLE_RADIUS + 2 * BLUDGER_RADIUS) as f64;
        let positions: Vec<(f64, f64)> = g
            .bludgers
            .iter()
            .map(|b| (b.disc.pos.x, b.disc.pos.y))
            .collect();
        assert_eq!(positions[0], (cx - off, cy));
        assert_eq!(positions[1], (cx + off, cy));
    }

    #[test]
    fn entity_ids_match_referee_spawn_order() {
        let g = game(0);
        for (i, w) in g.wizards.iter().enumerate() {
            assert_eq!(w.disc.id, i as i32);
        }
        let first_snaffle_id = 4;
        for (i, s) in g.snaffles.iter().enumerate() {
            assert_eq!(s.disc.id, first_snaffle_id + i as i32);
        }
        let first_bludger_id = first_snaffle_id + g.snaffles.len() as i32;
        for (i, b) in g.bludgers.iter().enumerate() {
            assert_eq!(b.disc.id, first_bludger_id + i as i32);
        }
        let first_post_id = first_bludger_id + g.bludgers.len() as i32;
        for (i, p) in g.goal_posts.iter().enumerate() {
            assert_eq!(p.id, first_post_id + i as i32);
        }
    }

    #[test]
    fn input_perspective_flips_wizard_labels() {
        let g = game(0);
        let p0 = g.input_for(0);
        let p1 = g.input_for(1);
        let p0_own = p0
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Wizard)
            .count();
        let p0_opp = p0
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::OpponentWizard)
            .count();
        assert_eq!(p0_own, 2);
        assert_eq!(p0_opp, 2);
        let p1_own_ids: Vec<i32> = p1
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Wizard)
            .map(|e| e.id)
            .collect();
        let p0_opp_ids: Vec<i32> = p0
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::OpponentWizard)
            .map(|e| e.id)
            .collect();
        assert_eq!(p1_own_ids, p0_opp_ids);
    }

    #[test]
    fn step_terminates_with_idle_bots() {
        // Idle bots (None outputs) still produce a game-ending state
        // eventually — bludgers hunt and bash wizards/snaffles around,
        // sometimes scoring. The contract here is just "the engine
        // doesn't hang and produces a valid outcome by tick 200."
        let mut g = game(0);
        let outputs = vec![None, None];
        let mut last: Option<FantasticBitsOutcome> = None;
        for _ in 0..MAX_TICKS + 5 {
            last = g.step(&outputs).or(last);
            if g.active_players().is_empty() {
                break;
            }
        }
        let outcome = last.expect("game should have ended");
        // Standings must be a valid 2-player competition ranking.
        assert!(
            outcome.standings == vec![1, 1]
                || outcome.standings == vec![1, 2]
                || outcome.standings == vec![2, 1]
        );
        // Sanity: scores in [0, total_snaffles].
        for s in outcome.score {
            assert!(s <= g.total_snaffles);
        }
    }

    /// Scripted scenario: pick the centerline snaffle, walk one of P0's
    /// wizards toward it, grab, throw at the right goal. Asserts that
    /// somewhere in the next ~50 ticks P0 scores.
    #[test]
    fn move_grab_throw_can_score() {
        let mut g = game(0);
        // Find the wizard closest to the centerline snaffle (always the
        // last entry per placement contract).
        let snaffle = g.snaffles.last().unwrap().clone();
        let wiz = g
            .wizards
            .iter()
            .filter(|w| wizard_team(w.disc.id) == 0)
            .min_by(|a, b| {
                let da = a.disc.pos.sub(snaffle.disc.pos).len_sq();
                let db = b.disc.pos.sub(snaffle.disc.pos).len_sq();
                da.partial_cmp(&db).unwrap()
            })
            .unwrap()
            .disc
            .id;
        let other_wiz = if wiz == 0 { 1 } else { 0 };

        for _ in 0..100 {
            // Build P0's two-wizard output: chase + throw for `wiz`,
            // idle for the other. Decide based on whether `wiz` is
            // currently holding.
            let holding = g
                .wizards
                .iter()
                .find(|w| w.disc.id == wiz)
                .unwrap()
                .holding
                .is_some();
            let action = if holding {
                WizardAction::throw_to(WIDTH, HEIGHT / 2, MAX_THROW_POWER)
            } else {
                // Move toward whatever snaffle is closest right now.
                let target = g
                    .snaffles
                    .iter()
                    .filter(|s| s.alive && s.held_by.is_none())
                    .min_by(|a, b| {
                        let wp = g
                            .wizards
                            .iter()
                            .find(|w| w.disc.id == wiz)
                            .unwrap()
                            .disc
                            .pos;
                        let da = a.disc.pos.sub(wp).len_sq();
                        let db = b.disc.pos.sub(wp).len_sq();
                        da.partial_cmp(&db).unwrap()
                    })
                    .unwrap();
                WizardAction::move_to(
                    target.disc.pos.x as i32,
                    target.disc.pos.y as i32,
                    MAX_POD_THRUST,
                )
            };
            let idle = WizardAction::move_to(0, HEIGHT / 2, 0);
            // Order outputs by team wizard slot (wiz id 0 → primary, etc.).
            let p0 = if wiz == 0 {
                TurnOutput {
                    primary: action,
                    secondary: idle,
                }
            } else {
                TurnOutput {
                    primary: idle,
                    secondary: action,
                }
            };
            let p1_idle = TurnOutput {
                primary: idle,
                secondary: idle,
            };
            let _ = other_wiz;
            let outcome = g.step(&[Some(p0), Some(p1_idle)]);
            if g.score()[0] > 0 || outcome.is_some() {
                break;
            }
        }
        assert!(
            g.score()[0] > 0,
            "expected P0 to score within 100 ticks; got {:?}",
            g.score()
        );
    }

    /// Inline greedy bot vs itself. Same policy the `_rs` crate uses;
    /// runs both sides through the engine and asserts the game ends
    /// with some scoring (not 0-0). Catches regressions where the
    /// capture/throw/score path silently breaks.
    #[test]
    fn greedy_vs_greedy_scores() {
        fn act_for(input: &TurnInput, wx: i32, wy: i32, holding: bool) -> WizardAction {
            let opp_goal_x = if wx < WIDTH / 2 { WIDTH } else { 0 };
            if holding {
                return WizardAction::throw_to(opp_goal_x, HEIGHT / 2, MAX_THROW_POWER);
            }
            let target = input
                .entities
                .iter()
                .filter(|e| e.kind == EntityKind::Snaffle && e.state == 0)
                .min_by_key(|s| {
                    let dx = s.x - wx;
                    let dy = s.y - wy;
                    (dx as i64) * (dx as i64) + (dy as i64) * (dy as i64)
                });
            match target {
                Some(s) => WizardAction::move_to(s.x, s.y, MAX_POD_THRUST),
                None => WizardAction::move_to(WIDTH / 2, HEIGHT / 2, 0),
            }
        }
        fn output_for_player(g: &FantasticBitsGame, player: usize) -> TurnOutput {
            let input = g.input_for(player as u32);
            let mine: Vec<&Wizard> = g
                .wizards
                .iter()
                .filter(|w| wizard_team(w.disc.id) == player)
                .collect();
            let primary = act_for(
                &input,
                mine[0].disc.pos.x as i32,
                mine[0].disc.pos.y as i32,
                mine[0].holding.is_some(),
            );
            let secondary = act_for(
                &input,
                mine[1].disc.pos.x as i32,
                mine[1].disc.pos.y as i32,
                mine[1].holding.is_some(),
            );
            TurnOutput { primary, secondary }
        }

        let mut any_scored = false;
        for seed in 0..5u64 {
            let mut g = game(seed);
            for _ in 0..MAX_TICKS {
                if g.active_players().is_empty() {
                    break;
                }
                let p0 = output_for_player(&g, 0);
                let p1 = output_for_player(&g, 1);
                g.step(&[Some(p0), Some(p1)]);
            }
            eprintln!("seed={seed} final score={:?}", g.score());
            if g.score()[0] + g.score()[1] > 0 {
                any_scored = true;
            }
        }
        assert!(
            any_scored,
            "no scoring across 5 seeds — likely an engine bug"
        );
    }

    #[test]
    fn magic_regenerates_and_caps() {
        let mut g = game(0);
        let outputs = vec![None, None];
        for _ in 0..50 {
            g.step(&outputs);
        }
        assert_eq!(g.magic(), [50, 50]);
        for _ in 0..150 {
            g.step(&outputs);
        }
        // The game ends at tick 200 → magic capped at 100 well before then.
        assert_eq!(g.magic(), [100, 100]);
    }
}
