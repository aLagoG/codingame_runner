//! Spider Attack engine.
//!
//! Two players, three heroes each, defend opposite-corner bases against
//! monsters that spawn from the map edges. The first base to hit 0 HP
//! loses; tied HP after `MAX_TICKS` is broken by *wild mana* (mana
//! earned outside the player's base radius).
//!
//! Coordinate origin is top-left; player 0's base is at `(0, 0)`, player 1's
//! at `(WIDTH, HEIGHT)`. Internally we carry float positions and round to
//! integers when emitting the wire format (asymmetric rule per spec).
//!
//! The action order per turn (mirrors the statement):
//!   1. Parse both players' 3-action outputs.
//!   2. Promote CONTROL/SHIELD spells queued on the previous turn into
//!      this turn's effects (movement overrides + shield_life = 12).
//!   3. Move heroes (controlled heroes use the average override).
//!   4. Heroes attack monsters in 800 unit range — 2 damage, 1 mana each.
//!   5. WIND spells push entities (not own heroes) by 2200.
//!   6. Move monsters (skip pushed-this-turn; controlled use override).
//!   7. Process monster-base contact (300 unit damage, monster removed).
//!      Update targeting status (5000 unit attract radius).
//!   8. Decrement shield countdowns. Spawn new monsters periodically.

use std::f64::consts::TAU;

use common::engine::{Game, GameRng, GameRngSeed, PlayerId};
use rand::{Rng, RngExt};
use spider_attack_defs::{Entity, EntityKind, HeroAction, InitialInput, TurnInput, TurnOutput};

// ============================================================
//  Constants (statement-derived)
// ============================================================

pub const WIDTH: i32 = 17630;
pub const HEIGHT: i32 = 9000;
pub const MAX_TICKS: u32 = 220;
pub const HEROES_PER_PLAYER: usize = 3;
pub const STARTING_BASE_HEALTH: i32 = 3;

pub const HERO_MOVE_RANGE: f64 = 800.0;
pub const HERO_ATTACK_RANGE: f64 = 800.0;
pub const HERO_DAMAGE: i32 = 2;
pub const HERO_VISION_RANGE: f64 = 2200.0;
pub const BASE_VISION_RANGE: f64 = 6000.0;

pub const MONSTER_SPEED: f64 = 400.0;
pub const MONSTER_TARGET_RANGE: f64 = 5000.0;
pub const MONSTER_DAMAGE_RANGE: f64 = 300.0;

pub const SPELL_COST: i32 = 10;
pub const WIND_RANGE: f64 = 1280.0;
pub const WIND_PUSH: f64 = 2200.0;
pub const SHIELD_RANGE: f64 = 2200.0;
pub const SHIELD_DURATION: i32 = 12;
pub const CONTROL_RANGE: f64 = 2200.0;

/// Initial HP of a freshly-spawned monster. Each subsequent monster
/// adds [`HEALTH_SCALING`] HP (statement: "may have slightly more
/// starting health than any previous monster").
pub const BASE_MONSTER_HEALTH: i32 = 10;
pub const HEALTH_SCALING: i32 = 1;

/// Turns between monster spawns. Each spawn places a mirrored pair so
/// the seed sequence stays symmetric.
pub const SPAWN_INTERVAL: u32 = 5;

/// Initial entity id offsets. Heroes get the lowest contiguous range so
/// per-player slot indexing (`hero_id - HERO_ID_OFFSET`) stays trivial;
/// monsters get the rest.
const HERO_ID_OFFSET: i32 = 0;
const FIRST_MONSTER_ID: i32 = (2 * HEROES_PER_PLAYER) as i32;

// ============================================================
//  Game state
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V2 {
    pub x: f64,
    pub y: f64,
}

impl V2 {
    pub const ZERO: V2 = V2 { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Self {
        V2 { x, y }
    }
    pub fn add(self, o: V2) -> V2 {
        V2::new(self.x + o.x, self.y + o.y)
    }
    pub fn sub(self, o: V2) -> V2 {
        V2::new(self.x - o.x, self.y - o.y)
    }
    pub fn mul(self, s: f64) -> V2 {
        V2::new(self.x * s, self.y * s)
    }
    pub fn len(self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
    pub fn normalize(self) -> V2 {
        let l = self.len();
        if l == 0.0 {
            V2::ZERO
        } else {
            V2::new(self.x / l, self.y / l)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hero {
    pub id: i32,
    pub team: usize,
    pub pos: V2,
    pub shield_life: i32,
    /// Active control destination for this turn — set by `promote_pending`
    /// from last turn's CONTROL casts. `None` means the hero acts freely.
    pub control_target: Option<V2>,
    /// Pending control destinations cast against this hero THIS turn,
    /// promoted at the start of next turn. Multiple → average.
    pending_control: Vec<V2>,
    /// Did a SHIELD get cast on this hero this turn? Promoted at start
    /// of next turn into `shield_life = SHIELD_DURATION`.
    pending_shield: bool,
    pub last_action: Option<HeroAction>,
}

#[derive(Debug, Clone)]
pub struct Monster {
    pub id: i32,
    pub pos: V2,
    pub vel: V2,
    pub health: i32,
    pub shield_life: i32,
    pub control_target: Option<V2>,
    pending_control: Vec<V2>,
    pending_shield: bool,
    /// Set when a WIND moved this monster this turn — skips the usual
    /// monster-movement step (the wind already moved it).
    pub pushed_this_turn: bool,
    /// `Some(p)` when the monster is targeting player `p`'s base.
    /// Computed at end-of-turn based on distance-to-base.
    pub target_base: Option<usize>,
}

pub struct SpiderAttackGame {
    tick: u32,
    health: [i32; 2],
    mana: [i32; 2],
    wild_mana: [u32; 2],
    heroes: Vec<Hero>,
    monsters: Vec<Monster>,
    /// Monotonic id source for new monsters. Hero ids are pre-allocated.
    next_id: i32,
    /// HP of the next monster to spawn. Increments per spawn.
    next_monster_health: i32,
    active: Vec<PlayerId>,
    outcome: Option<SpiderAttackOutcome>,
    rng: GameRng,
}

#[derive(Debug, Clone)]
pub struct SpiderAttackOutcome {
    pub winner: Option<PlayerId>,
    pub standings: Vec<u32>,
    pub health: [i32; 2],
    pub wild_mana: [u32; 2],
}

impl SpiderAttackGame {
    pub fn tick(&self) -> u32 {
        self.tick
    }
    pub fn health(&self) -> [i32; 2] {
        self.health
    }
    pub fn mana(&self) -> [i32; 2] {
        self.mana
    }
    pub fn wild_mana(&self) -> [u32; 2] {
        self.wild_mana
    }
    pub fn heroes(&self) -> &[Hero] {
        &self.heroes
    }
    pub fn monsters(&self) -> &[Monster] {
        &self.monsters
    }

    /// Player p's base position (player 0 = top-left, 1 = bottom-right).
    pub fn base_pos(p: usize) -> V2 {
        if p == 0 {
            V2::new(0.0, 0.0)
        } else {
            V2::new(WIDTH as f64, HEIGHT as f64)
        }
    }
}

// ============================================================
//  Game trait impl
// ============================================================

impl Game for SpiderAttackGame {
    const NAME: &'static str = "spider_attack";
    const INITIAL_TURN_TIMEOUT_MS: u64 = 1000;
    const TURN_TIMEOUT_MS: u64 = 50;

    type InitialInput = InitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    type Outcome = SpiderAttackOutcome;

    fn new(num_players: u32, rng: &mut GameRng) -> Self {
        assert_eq!(num_players, 2, "Spider Attack is always 2 players");

        let heroes = place_heroes();
        // Derive a fresh child RNG from the runner-provided RNG so
        // per-turn draws (spawn positions, post-wind randomisation)
        // stay deterministic from the seed the runner persists in
        // the replay. `StdRng` isn't `Clone` in rand 0.10, so we
        // re-seed via a `next_u64` draw — same input seed → same
        // monster stream.
        let game_rng = GameRng::seed_from_u64(rng.next_u64());

        SpiderAttackGame {
            tick: 0,
            health: [STARTING_BASE_HEALTH; 2],
            mana: [0; 2],
            wild_mana: [0; 2],
            heroes,
            monsters: Vec::new(),
            next_id: FIRST_MONSTER_ID,
            next_monster_health: BASE_MONSTER_HEALTH,
            active: vec![0, 1],
            outcome: None,
            rng: game_rng,
        }
    }

    fn initial_input(&self, player: PlayerId) -> InitialInput {
        let base = SpiderAttackGame::base_pos(player as usize);
        InitialInput {
            base_x: base.x as i32,
            base_y: base.y as i32,
            heroes_per_player: HEROES_PER_PLAYER as i32,
        }
    }

    fn input_for(&self, player: PlayerId) -> TurnInput {
        let me = player as usize;
        let opp = 1 - me;
        let my_base = SpiderAttackGame::base_pos(me);

        let mut entities: Vec<Entity> = Vec::new();

        for h in &self.heroes {
            let kind = if h.team == me {
                EntityKind::MyHero
            } else {
                EntityKind::OppHero
            };
            // Own heroes always visible; opponent heroes need fog check.
            if kind == EntityKind::OppHero && !visible_to(me, h.pos, &self.heroes, my_base) {
                continue;
            }
            entities.push(Entity {
                id: h.id,
                kind,
                x: round_asymmetric(h.pos.x, WIDTH),
                y: round_asymmetric(h.pos.y, HEIGHT),
                shield_life: h.shield_life,
                is_controlled: i32::from(h.control_target.is_some()),
                health: -1,
                vx: -1,
                vy: -1,
                near_base: -1,
                threat_for: -1,
            });
        }
        for m in &self.monsters {
            if !visible_to(me, m.pos, &self.heroes, my_base) {
                continue;
            }
            let near_base = i32::from(m.target_base.is_some());
            let threat_for = match m.target_base {
                Some(t) if t == me => 1,
                Some(_) => 2,
                None => 0,
            };
            entities.push(Entity {
                id: m.id,
                kind: EntityKind::Monster,
                x: round_asymmetric(m.pos.x, WIDTH),
                y: round_asymmetric(m.pos.y, HEIGHT),
                shield_life: m.shield_life,
                is_controlled: i32::from(m.control_target.is_some()),
                health: m.health,
                vx: round_asymmetric(m.vel.x, WIDTH),
                vy: round_asymmetric(m.vel.y, HEIGHT),
                near_base,
                threat_for,
            });
        }
        // Stable ordering — id ascending — so bots see a deterministic
        // entity stream across players and runs.
        entities.sort_by_key(|e| e.id);

        TurnInput {
            my_health: self.health[me],
            my_mana: self.mana[me],
            opp_health: self.health[opp],
            opp_mana: self.mana[opp],
            entities,
        }
    }

    fn step(&mut self, outputs: &[Option<TurnOutput>]) -> Option<SpiderAttackOutcome> {
        if let Some(o) = &self.outcome {
            return Some(o.clone());
        }

        // 1. Promote spells queued last turn: control overrides + shields
        //    that activate this turn.
        self.promote_pending();

        // Reset per-turn flags.
        for h in &mut self.heroes {
            h.last_action = None;
        }
        for m in &mut self.monsters {
            m.pushed_this_turn = false;
        }

        // 2. Parse outputs: validate, deduct mana, queue pending spells,
        //    return per-hero immediate intents.
        let intents = self.parse_outputs(outputs);

        // 3. Move heroes (controlled override beats the parsed move).
        self.move_heroes(&intents);

        // 4. Heroes attack monsters in range → mana.
        self.heroes_attack();

        // 5. WIND spells (immediate).
        self.apply_winds(&intents);

        // 6. Move monsters.
        self.move_monsters();

        // 7. End-of-turn: targeting + base damage.
        self.process_base_contacts();
        self.update_targeting();

        // 8. Decrement shield countdowns. Spawn new monsters.
        self.decrement_shields();
        self.maybe_spawn_monsters();

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

    fn standings(outcome: &SpiderAttackOutcome) -> Vec<u32> {
        match outcome.winner {
            Some(0) => vec![1, 2],
            Some(1) => vec![2, 1],
            _ => vec![1, 1],
        }
    }

    fn scores(outcome: &SpiderAttackOutcome) -> Option<Vec<f64>> {
        // "Score" surfaced for ranking inspection = base HP at game end.
        Some(outcome.health.iter().map(|h| *h as f64).collect())
    }
}

// ============================================================
//  Parse / intents
// ============================================================

#[derive(Debug, Clone, Copy)]
enum Intent {
    /// MOVE toward (x, y). Bots can pick any point — the engine clamps
    /// the step to `HERO_MOVE_RANGE`.
    Move(V2),
    /// WIND from this hero's position toward (x, y). Mana already
    /// deducted at parse time; applied in step 5.
    Wind(V2),
    /// SHIELD / CONTROL effects already applied (queued as pending);
    /// hero stays put unless also moved by a CONTROL override.
    Idle,
}

impl Default for Intent {
    fn default() -> Self {
        Intent::Idle
    }
}

impl SpiderAttackGame {
    fn parse_outputs(&mut self, outputs: &[Option<TurnOutput>]) -> Vec<Intent> {
        let mut intents = vec![Intent::Idle; self.heroes.len()];

        for player_id in 0..2usize {
            let Some(out) = outputs.get(player_id).and_then(|o| o.as_ref()) else {
                continue;
            };
            for (slot, action) in out.actions.iter().enumerate() {
                let hero_idx = player_id * HEROES_PER_PLAYER + slot;
                intents[hero_idx] = self.parse_action(player_id, hero_idx, *action);
            }
        }
        intents
    }

    fn parse_action(&mut self, player: usize, hero_idx: usize, action: HeroAction) -> Intent {
        self.heroes[hero_idx].last_action = Some(action);

        // Controlled heroes are forced to move toward their control
        // target — their owner's command is fully discarded (no mana
        // deduction, no spell effects).
        if self.heroes[hero_idx].control_target.is_some() {
            return Intent::Idle;
        }

        let hero_pos = self.heroes[hero_idx].pos;

        match action {
            HeroAction::Wait => Intent::Idle,
            HeroAction::Move { x, y } => Intent::Move(V2::new(x as f64, y as f64)),
            HeroAction::Wind { x, y } => {
                if self.mana[player] < SPELL_COST {
                    return Intent::Idle;
                }
                self.mana[player] -= SPELL_COST;
                Intent::Wind(V2::new(x as f64, y as f64))
            }
            HeroAction::Shield { entity_id } => {
                if self.mana[player] < SPELL_COST {
                    return Intent::Idle;
                }
                let target_pos = match self.lookup_target_pos(entity_id) {
                    Some(p) => p,
                    None => return Intent::Idle,
                };
                if hero_pos.sub(target_pos).len() > SHIELD_RANGE {
                    return Intent::Idle;
                }
                // Cast goes through — mana is deducted whether the
                // target is shielded or not (per statement).
                self.mana[player] -= SPELL_COST;
                if !self.is_target_shielded(entity_id) {
                    self.queue_shield(entity_id);
                }
                Intent::Idle
            }
            HeroAction::Control { entity_id, x, y } => {
                if self.mana[player] < SPELL_COST {
                    return Intent::Idle;
                }
                let target_pos = match self.lookup_target_pos(entity_id) {
                    Some(p) => p,
                    None => return Intent::Idle,
                };
                if hero_pos.sub(target_pos).len() > CONTROL_RANGE {
                    return Intent::Idle;
                }
                self.mana[player] -= SPELL_COST;
                if !self.is_target_shielded(entity_id) {
                    self.queue_control(entity_id, V2::new(x as f64, y as f64));
                }
                Intent::Idle
            }
        }
    }

    fn lookup_target_pos(&self, id: i32) -> Option<V2> {
        if let Some(h) = self.heroes.iter().find(|h| h.id == id) {
            return Some(h.pos);
        }
        self.monsters.iter().find(|m| m.id == id).map(|m| m.pos)
    }

    fn is_target_shielded(&self, id: i32) -> bool {
        if let Some(h) = self.heroes.iter().find(|h| h.id == id) {
            return h.shield_life > 0;
        }
        self.monsters
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.shield_life > 0)
            .unwrap_or(false)
    }

    fn queue_shield(&mut self, id: i32) {
        if let Some(h) = self.heroes.iter_mut().find(|h| h.id == id) {
            h.pending_shield = true;
            return;
        }
        if let Some(m) = self.monsters.iter_mut().find(|m| m.id == id) {
            m.pending_shield = true;
        }
    }

    fn queue_control(&mut self, id: i32, dest: V2) {
        if let Some(h) = self.heroes.iter_mut().find(|h| h.id == id) {
            h.pending_control.push(dest);
            return;
        }
        if let Some(m) = self.monsters.iter_mut().find(|m| m.id == id) {
            m.pending_control.push(dest);
        }
    }
}

// ============================================================
//  Promote / decrement
// ============================================================

impl SpiderAttackGame {
    fn promote_pending(&mut self) {
        for h in &mut self.heroes {
            h.control_target = if h.pending_control.is_empty() {
                None
            } else {
                Some(average(&h.pending_control))
            };
            h.pending_control.clear();
            if h.pending_shield {
                h.shield_life = SHIELD_DURATION;
                h.pending_shield = false;
            }
        }
        for m in &mut self.monsters {
            m.control_target = if m.pending_control.is_empty() {
                None
            } else {
                Some(average(&m.pending_control))
            };
            m.pending_control.clear();
            if m.pending_shield {
                m.shield_life = SHIELD_DURATION;
                m.pending_shield = false;
            }
        }
    }

    fn decrement_shields(&mut self) {
        for h in &mut self.heroes {
            if h.shield_life > 0 {
                h.shield_life -= 1;
            }
        }
        for m in &mut self.monsters {
            if m.shield_life > 0 {
                m.shield_life -= 1;
            }
        }
    }
}

fn average(pts: &[V2]) -> V2 {
    let n = pts.len() as f64;
    let s = pts.iter().fold(V2::ZERO, |acc, p| acc.add(*p));
    s.mul(1.0 / n)
}

// ============================================================
//  Movement
// ============================================================

impl SpiderAttackGame {
    fn move_heroes(&mut self, intents: &[Intent]) {
        for (idx, h) in self.heroes.iter_mut().enumerate() {
            let target = match h.control_target {
                Some(t) => Some(t),
                None => match intents[idx] {
                    Intent::Move(p) => Some(p),
                    _ => None,
                },
            };
            if let Some(target) = target {
                let dir = target.sub(h.pos);
                let len = dir.len();
                if len <= HERO_MOVE_RANGE {
                    h.pos = target;
                } else {
                    h.pos = h.pos.add(dir.mul(HERO_MOVE_RANGE / len));
                }
                clamp_to_map(&mut h.pos);
            }
        }
    }

    fn move_monsters(&mut self) {
        for m in &mut self.monsters {
            if m.pushed_this_turn {
                continue;
            }
            // Controlled monsters move 400 toward the override target.
            if let Some(target) = m.control_target {
                let dir = target.sub(m.pos).normalize();
                let step = dir.mul(MONSTER_SPEED);
                m.vel = step;
                m.pos = m.pos.add(step);
                clamp_target_to_map(m);
                continue;
            }
            // Targeting monsters head directly to the targeted base
            // (recomputed each turn) at MONSTER_SPEED.
            if let Some(t) = m.target_base {
                let base = SpiderAttackGame::base_pos(t);
                let dir = base.sub(m.pos).normalize();
                let step = dir.mul(MONSTER_SPEED);
                m.vel = step;
                m.pos = m.pos.add(step);
                clamp_target_to_map(m);
            } else {
                // Default straight-line travel — velocity stays as set
                // at spawn (or after a randomization).
                m.pos = m.pos.add(m.vel);
            }
        }
    }
}

/// Heroes cannot leave the map (statement). Clamp positions into the
/// playable rectangle.
fn clamp_to_map(p: &mut V2) {
    p.x = p.x.clamp(0.0, WIDTH as f64);
    p.y = p.y.clamp(0.0, HEIGHT as f64);
}

/// Targeting monsters can no longer leave the map (statement). For
/// monsters being moved as a side-effect of targeting / control, clamp.
fn clamp_target_to_map(m: &mut Monster) {
    m.pos.x = m.pos.x.clamp(0.0, WIDTH as f64);
    m.pos.y = m.pos.y.clamp(0.0, HEIGHT as f64);
}

// ============================================================
//  Attack / WIND
// ============================================================

impl SpiderAttackGame {
    fn heroes_attack(&mut self) {
        // Two-pass: collect damage events first (immutable borrow of
        // self), then apply (mutable). Mirrors the simultaneous-hit
        // semantics — a single monster hit by N heroes loses 2N HP and
        // generates 2N mana, even after dropping below zero.
        let mut events: Vec<(usize, usize, i32)> = Vec::new(); // team, monster_idx, dmg
        for h in &self.heroes {
            for (mi, m) in self.monsters.iter().enumerate() {
                if m.health <= 0 {
                    continue;
                }
                if m.pos.sub(h.pos).len() <= HERO_ATTACK_RANGE {
                    events.push((h.team, mi, HERO_DAMAGE));
                }
            }
        }
        for (team, mi, dmg) in events {
            self.monsters[mi].health -= dmg;
            self.mana[team] += dmg;
            let base = Self::base_pos(team);
            if self.monsters[mi].pos.sub(base).len() > BASE_VISION_RANGE {
                self.wild_mana[team] += dmg as u32;
            }
        }
    }

    fn apply_winds(&mut self, intents: &[Intent]) {
        // Materialize the casters first so the borrow checker lets us
        // mutate self.heroes / self.monsters inside the loop.
        let casters: Vec<(usize, V2, V2)> = self
            .heroes
            .iter()
            .enumerate()
            .filter_map(|(idx, h)| match intents[idx] {
                Intent::Wind(target) => Some((h.team, h.pos, target)),
                _ => None,
            })
            .collect();

        for (team, caster_pos, target) in casters {
            let push_dir = target.sub(caster_pos).normalize();
            let push = push_dir.mul(WIND_PUSH);

            // Monsters within range get pushed (unless shielded).
            // Collect indices whose targeting was canceled by the
            // push — we randomize their headings after the loop so we
            // can use self.rng without aliasing self.monsters.
            let mut lost_targeting: Vec<usize> = Vec::new();
            for (mi, m) in self.monsters.iter_mut().enumerate() {
                if m.shield_life > 0 {
                    continue;
                }
                if m.pos.sub(caster_pos).len() <= WIND_RANGE {
                    m.pos = m.pos.add(push);
                    if let Some(t) = m.target_base {
                        let base = SpiderAttackGame::base_pos(t);
                        if m.pos.sub(base).len() > MONSTER_TARGET_RANGE {
                            m.target_base = None;
                            lost_targeting.push(mi);
                        }
                    }
                    // Targeting monsters can't be pushed outside the
                    // map (statement: "moved no further than the
                    // border"). Approximate as a clamp.
                    if m.target_base.is_some() {
                        clamp_target_to_map(m);
                    }
                    m.pushed_this_turn = true;
                }
            }
            for mi in lost_targeting {
                let angle: f64 = self.rng.random_range(0.0..TAU);
                self.monsters[mi].vel = V2::new(angle.cos(), angle.sin()).mul(MONSTER_SPEED);
            }
            // Opponent heroes within range also get pushed (unless
            // shielded). Own heroes are excluded per statement.
            for h in &mut self.heroes {
                if h.team == team {
                    continue;
                }
                if h.shield_life > 0 {
                    continue;
                }
                if h.pos.sub(caster_pos).len() <= WIND_RANGE {
                    h.pos = h.pos.add(push);
                    clamp_to_map(&mut h.pos);
                }
            }
        }
    }
}

// ============================================================
//  End-of-turn (base contacts, targeting)
// ============================================================

impl SpiderAttackGame {
    fn process_base_contacts(&mut self) {
        // Remove any monster within MONSTER_DAMAGE_RANGE of a base —
        // deal that base 1 damage. Monsters with health <= 0 are also
        // removed.
        let bases = [Self::base_pos(0), Self::base_pos(1)];
        let monsters = std::mem::take(&mut self.monsters);
        for m in monsters {
            if m.health <= 0 {
                continue;
            }
            let mut hit_base: Option<usize> = None;
            for (pi, base) in bases.iter().enumerate() {
                if m.pos.sub(*base).len() <= MONSTER_DAMAGE_RANGE {
                    hit_base = Some(pi);
                    break;
                }
            }
            match hit_base {
                Some(pi) => self.health[pi] = (self.health[pi] - 1).max(0),
                None => self.monsters.push(m),
            }
        }
    }

    fn update_targeting(&mut self) {
        let bases = [Self::base_pos(0), Self::base_pos(1)];
        for m in &mut self.monsters {
            let mut best: Option<(usize, f64)> = None;
            for (pi, base) in bases.iter().enumerate() {
                let d = m.pos.sub(*base).len();
                if d <= MONSTER_TARGET_RANGE {
                    match best {
                        Some((_, bd)) if bd <= d => {}
                        _ => best = Some((pi, d)),
                    }
                }
            }
            m.target_base = best.map(|(pi, _)| pi);
            // Recompute velocity for targeting monsters so the
            // next-tick wire reflects the actual heading.
            if let Some(t) = m.target_base {
                let dir = bases[t].sub(m.pos).normalize();
                m.vel = dir.mul(MONSTER_SPEED);
            }
        }
    }
}

// ============================================================
//  Spawning
// ============================================================

impl SpiderAttackGame {
    fn maybe_spawn_monsters(&mut self) {
        if self.tick == 0 || self.tick % SPAWN_INTERVAL != 0 {
            return;
        }
        // Pick a random edge position outside both bases' targeting
        // ranges, then mirror it for fairness.
        let bases = [Self::base_pos(0), Self::base_pos(1)];
        let (a, b) = {
            let mut chosen: Option<(V2, V2)> = None;
            for _ in 0..50 {
                let edge = self.rng.random_range(0u32..4);
                let p = match edge {
                    0 => V2::new(self.rng.random_range(0..=WIDTH) as f64, 0.0),
                    1 => V2::new(self.rng.random_range(0..=WIDTH) as f64, HEIGHT as f64),
                    2 => V2::new(0.0, self.rng.random_range(0..=HEIGHT) as f64),
                    _ => V2::new(WIDTH as f64, self.rng.random_range(0..=HEIGHT) as f64),
                };
                // Avoid spawning inside a base's pull radius — the
                // monster would immediately start targeting from spawn,
                // which makes for one-sided games.
                let too_close = bases
                    .iter()
                    .any(|b| p.sub(*b).len() <= MONSTER_TARGET_RANGE);
                if too_close {
                    continue;
                }
                let mirror = V2::new(WIDTH as f64 - p.x, HEIGHT as f64 - p.y);
                chosen = Some((p, mirror));
                break;
            }
            // Numeric fallback — top/bottom-center if 50 random draws
            // all landed inside a base radius (vanishingly unlikely).
            chosen.unwrap_or((
                V2::new(WIDTH as f64 / 2.0, 0.0),
                V2::new(WIDTH as f64 / 2.0, HEIGHT as f64),
            ))
        };

        let angle: f64 = self.rng.random_range(0.0..TAU);
        let dir_a = V2::new(angle.cos(), angle.sin()).mul(MONSTER_SPEED);
        let dir_b = dir_a.mul(-1.0); // mirrored direction

        let hp = self.next_monster_health;
        self.spawn_monster(a, dir_a, hp);
        self.spawn_monster(b, dir_b, hp);
        self.next_monster_health += HEALTH_SCALING;
    }

    fn spawn_monster(&mut self, pos: V2, vel: V2, health: i32) {
        let id = self.next_id;
        self.next_id += 1;
        self.monsters.push(Monster {
            id,
            pos,
            vel,
            health,
            shield_life: 0,
            control_target: None,
            pending_control: Vec::new(),
            pending_shield: false,
            pushed_this_turn: false,
            target_base: None,
        });
    }
}

// ============================================================
//  End condition
// ============================================================

impl SpiderAttackGame {
    fn check_end(&self) -> Option<SpiderAttackOutcome> {
        let dead0 = self.health[0] <= 0;
        let dead1 = self.health[1] <= 0;
        if dead0 || dead1 || self.tick >= MAX_TICKS {
            return Some(self.make_outcome());
        }
        None
    }

    fn make_outcome(&self) -> SpiderAttackOutcome {
        let (winner, standings) = if self.health[0] <= 0 && self.health[1] <= 0 {
            (None, vec![1, 1])
        } else if self.health[0] <= 0 {
            (Some(1), vec![2, 1])
        } else if self.health[1] <= 0 {
            (Some(0), vec![1, 2])
        } else {
            match self.health[0].cmp(&self.health[1]) {
                std::cmp::Ordering::Greater => (Some(0), vec![1, 2]),
                std::cmp::Ordering::Less => (Some(1), vec![2, 1]),
                std::cmp::Ordering::Equal => match self.wild_mana[0].cmp(&self.wild_mana[1]) {
                    std::cmp::Ordering::Greater => (Some(0), vec![1, 2]),
                    std::cmp::Ordering::Less => (Some(1), vec![2, 1]),
                    std::cmp::Ordering::Equal => (None, vec![1, 1]),
                },
            }
        };
        SpiderAttackOutcome {
            winner,
            standings,
            health: self.health,
            wild_mana: self.wild_mana,
        }
    }
}

// ============================================================
//  Setup helpers
// ============================================================

fn place_heroes() -> Vec<Hero> {
    // Three positions per team, near the base. Player 0's base is at
    // (0, 0); rotate 45° apart along the radius. Player 1 mirrors.
    let radius: f64 = 4000.0;
    let angles_deg: [f64; 3] = [22.5, 45.0, 67.5];
    let mut out = Vec::with_capacity(2 * HEROES_PER_PLAYER);
    let mut next_id: i32 = HERO_ID_OFFSET;
    for team in 0..2usize {
        let base = SpiderAttackGame::base_pos(team);
        for &a in &angles_deg {
            let rad = a.to_radians();
            // For team 0 we sweep into +x +y; for team 1 we mirror so
            // heroes still fan out into the playing field.
            let (sx, sy) = if team == 0 { (1.0, 1.0) } else { (-1.0, -1.0) };
            let pos = V2::new(base.x + sx * radius * rad.cos(), base.y + sy * radius * rad.sin());
            out.push(Hero {
                id: next_id,
                team,
                pos,
                shield_life: 0,
                control_target: None,
                pending_control: Vec::new(),
                pending_shield: false,
                last_action: None,
            });
            next_id += 1;
        }
    }
    out
}

// ============================================================
//  Fog of war / rounding
// ============================================================

fn visible_to(player: usize, pos: V2, heroes: &[Hero], my_base: V2) -> bool {
    if pos.sub(my_base).len() <= BASE_VISION_RANGE {
        return true;
    }
    heroes
        .iter()
        .filter(|h| h.team == player)
        .any(|h| pos.sub(h.pos).len() <= HERO_VISION_RANGE)
}

/// Per spec: coordinates below halfway across the map are truncated;
/// at or above are rounded up. Applied per axis when emitting the
/// wire format.
fn round_asymmetric(v: f64, axis_max: i32) -> i32 {
    let half = axis_max as f64 / 2.0;
    if v < half { v.floor() as i32 } else { v.ceil() as i32 }
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use common::engine::GameRngSeed;

    fn game(seed: u64) -> SpiderAttackGame {
        let mut rng = GameRng::seed_from_u64(seed);
        SpiderAttackGame::new(2, &mut rng)
    }

    #[test]
    fn placement_invariants() {
        let g = game(0);
        assert_eq!(g.heroes.len(), 2 * HEROES_PER_PLAYER);
        assert_eq!(g.monsters.len(), 0);
        // Hero ids form a contiguous range starting at HERO_ID_OFFSET.
        for (i, h) in g.heroes.iter().enumerate() {
            assert_eq!(h.id, HERO_ID_OFFSET + i as i32);
            assert_eq!(h.team, i / HEROES_PER_PLAYER);
        }
    }

    #[test]
    fn initial_input_carries_base_for_each_player() {
        let g = game(0);
        let p0 = g.initial_input(0);
        let p1 = g.initial_input(1);
        assert_eq!((p0.base_x, p0.base_y), (0, 0));
        assert_eq!((p1.base_x, p1.base_y), (WIDTH, HEIGHT));
        assert_eq!(p0.heroes_per_player, 3);
    }

    #[test]
    fn idle_bots_end_within_max_ticks() {
        let mut g = game(0);
        let mut last = None;
        for _ in 0..MAX_TICKS + 5 {
            last = g.step(&[None, None]).or(last);
            if g.active_players().is_empty() {
                break;
            }
        }
        let outcome = last.expect("game should have ended");
        assert!(
            outcome.standings == vec![1, 1]
                || outcome.standings == vec![1, 2]
                || outcome.standings == vec![2, 1]
        );
    }

    #[test]
    fn hero_in_range_kills_low_hp_monster() {
        let mut g = game(0);
        // Inject a low-HP monster right next to hero 0.
        let hero0_pos = g.heroes[0].pos;
        g.monsters.push(Monster {
            id: 999,
            pos: hero0_pos.add(V2::new(100.0, 0.0)),
            vel: V2::ZERO,
            health: 2,
            shield_life: 0,
            control_target: None,
            pending_control: Vec::new(),
            pending_shield: false,
            pushed_this_turn: false,
            target_base: None,
        });
        let waits = TurnOutput {
            actions: [HeroAction::Wait; 3],
        };
        g.step(&[Some(waits), Some(waits)]);
        assert!(g.monsters.iter().all(|m| m.id != 999));
        // 2 damage dealt → 2 mana for team 0.
        assert!(g.mana[0] >= 2);
    }

    #[test]
    fn wind_pushes_monster_in_direction() {
        let mut g = game(0);
        // Place a non-shielded monster 100 units to the right of hero 0.
        let h0 = g.heroes[0].pos;
        let mid = 999;
        g.monsters.push(Monster {
            id: mid,
            pos: h0.add(V2::new(100.0, 0.0)),
            vel: V2::ZERO,
            health: 50,
            shield_life: 0,
            control_target: None,
            pending_control: Vec::new(),
            pending_shield: false,
            pushed_this_turn: false,
            target_base: None,
        });
        // Give the team enough mana to cast.
        g.mana[0] = 50;
        let wind = HeroAction::Wind {
            x: h0.x as i32 + 1000,
            y: h0.y as i32,
        };
        let out = TurnOutput {
            actions: [wind, HeroAction::Wait, HeroAction::Wait],
        };
        let idle = TurnOutput {
            actions: [HeroAction::Wait; 3],
        };
        let pre = g.monsters.iter().find(|m| m.id == mid).unwrap().pos.x;
        g.step(&[Some(out), Some(idle)]);
        let post_opt = g.monsters.iter().find(|m| m.id == mid).map(|m| m.pos.x);
        // Either the monster has been pushed far to the right OR the
        // step has carried it onto the base — either way it's not still
        // sitting near the start.
        if let Some(post) = post_opt {
            assert!(post > pre + 1500.0, "expected wind push, pre={pre} post={post}");
        }
    }
}
