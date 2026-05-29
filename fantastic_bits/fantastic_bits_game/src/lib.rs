//! Skeleton engine — phase 1. Sets up entities, hands them to bots
//! perspective-relative, accepts (and currently ignores) outputs,
//! times out after 200 ticks as a draw. Phase 2 will add physics +
//! collision-aware movement; until then this exists so the wire
//! contract and harness round-trip end-to-end.
//!
//! Coordinate system (CodinGame Fantastic Bits):
//!   * Map: 16001 × 7501 units, (0, 0) top-left.
//!   * Team 0 goal at x ≈ 0, team 1 goal at x ≈ 16000.
//!   * Goal poles centered at y = 1750 and y = 5750 (4000 apart).
//!   * Radii: wizard 400, snaffle 150, bludger 200, pole 300.

use common::engine::{FfiGame, Game, GameRng, NoInitialInput, PlayerId};
use fantastic_bits_defs::{Entity, EntityKind, TurnInput, TurnOutput};
use rand::RngExt;

// All map / entity / spell constants taken from the CodinGame
// referee (Fantastic Bits, MultiReferee variant). Where the in-game
// statement disagrees with the referee, the referee wins — replay
// parity is the goal.
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

/// Snaffle counts in a match. The referee picks the pair count as
/// `2 + random.nextInt(2)` (i.e. 2 or 3 pairs) and adds a single
/// centerline snaffle on top → 5 or 7 total.
pub const MIN_SNAFFLES: u32 = 5;
pub const MAX_SNAFFLES: u32 = 7;

/// Vertical spacing between a team's two wizards at spawn (referee).
pub const SPACE_BETWEEN_POD: i32 = 3000;
/// Minimum distance between any two snaffle spawns. Referee uses
/// reject sampling against already-placed snaffles when laying out
/// the random pairs.
pub const MIN_SPACE_BETWEEN_SNAFFLES: i32 = 1250;

// Entity ids are globally auto-incremented in the referee, in
// construction order:
//   0..3  — wizards (P0 wiz 0, P0 wiz 1, P1 wiz 0, P1 wiz 1)
//   4..   — snaffles (pair_0_a, pair_0_b, pair_1_a, …, centerline last)
//   then — bludgers (e.g. 9–10 for 5-snaffle games, 11–12 for 7)
//   then — goal posts (created last; bots never see them)
// We match this layout so bots that hardcode ids work unchanged when
// pointed at the real CodinGame platform.

/// Continuous-space state. Positions/velocities are f64 internally; we
/// expose i32 to bots (per protocol) via rounding at `input_for` time.
/// Phase 1 keeps these zero-velocity since no physics runs yet — but the
/// shape is already in place for phase 2.
#[derive(Debug, Clone)]
pub struct DiscState {
    pub id: i32,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
}

#[derive(Debug, Clone)]
pub struct Wizard {
    pub disc: DiscState,
    /// `Some(snaffle_id)` if currently holding.
    pub holding: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct Bludger {
    pub disc: DiscState,
    /// Last wizard the bludger collided with (`-1` if none).
    pub last_victim: i32,
}

#[derive(Debug, Clone)]
pub struct Snaffle {
    pub disc: DiscState,
    /// `Some(wizard_id)` if currently held.
    pub held_by: Option<i32>,
}

pub struct FantasticBitsGame {
    tick: u32,
    score: [u32; 2],
    magic: [u32; 2],
    total_snaffles: u32,
    wizards: Vec<Wizard>,   // length 4: ids 0,1 = team 0; 2,3 = team 1
    bludgers: Vec<Bludger>, // length 2
    snaffles: Vec<Snaffle>, // shrinks as they're scored
    active: Vec<PlayerId>,  // [0, 1] until game ends
    outcome: Option<FantasticBitsOutcome>,
}

#[derive(Debug, Clone)]
pub struct FantasticBitsOutcome {
    pub winner: Option<PlayerId>,
    /// Final score per player, in id order.
    pub score: [u32; 2],
    /// Competition ranking (1,1 for ties; 1,2 / 2,1 otherwise).
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
    pub fn total_snaffles(&self) -> u32 {
        self.total_snaffles
    }
    /// First-to-this score wins. Matches the referee's
    /// `SCORE_TO_WIN = nbPairs + 1` → 3 for a 5-snaffle game, 4 for 7.
    pub fn score_to_win(&self) -> u32 {
        self.total_snaffles / 2 + 1
    }
}

impl Game for FantasticBitsGame {
    const NAME: &'static str = "fantastic_bits";

    type InitialInput = NoInitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    type Outcome = FantasticBitsOutcome;

    fn new(num_players: u32, rng: &mut GameRng) -> Self {
        assert_eq!(num_players, 2, "Fantastic Bits is always 2 players");

        // Referee picks the pair count first, then adds one centerline
        // snaffle. Total is always odd (5 or 7).
        let pair_count = 2 + rng.random_range(0u32..2);
        let total_snaffles = 2 * pair_count + 1;

        // Entity ids are globally auto-incremented, in spawn order.
        let mut next_id: i32 = 0;
        let wizards = place_wizards(&mut next_id);
        let snaffles = place_snaffles(pair_count, rng, &mut next_id);
        let bludgers = place_bludgers(&mut next_id);
        // Goal posts would claim the next ids (`*next_id .. *next_id + 4`),
        // but they're internal-only — bots never see them and phase-1
        // physics doesn't model them yet. Phase 2 will add them.

        FantasticBitsGame {
            tick: 0,
            score: [0, 0],
            magic: [0, 0],
            total_snaffles,
            wizards,
            bludgers,
            snaffles,
            active: vec![0, 1],
            outcome: None,
        }
    }

    fn initial_input(&self, _player: PlayerId) -> NoInitialInput {
        NoInitialInput::default()
    }

    fn input_for(&self, player: PlayerId) -> TurnInput {
        let my = player as usize;
        let opp = 1 - my;
        let mut entities: Vec<Entity> = Vec::new();

        // Wizards: relabel based on perspective.
        for w in &self.wizards {
            let team = wizard_team(w.disc.id);
            let kind = if team == my {
                EntityKind::Wizard
            } else {
                EntityKind::OpponentWizard
            };
            let state = if w.holding.is_some() { 1 } else { 0 };
            entities.push(disc_to_entity(&w.disc, kind, state));
        }
        for b in &self.bludgers {
            entities.push(disc_to_entity(
                &b.disc,
                EntityKind::Bludger,
                b.last_victim,
            ));
        }
        for s in &self.snaffles {
            let state = if s.held_by.is_some() { 1 } else { 0 };
            entities.push(disc_to_entity(&s.disc, EntityKind::Snaffle, state));
        }

        TurnInput {
            my_score: self.score[my] as i32,
            my_magic: self.magic[my] as i32,
            opp_score: self.score[opp] as i32,
            opp_magic: self.magic[opp] as i32,
            entities,
        }
    }

    fn step(&mut self, _outputs: &[Option<TurnOutput>]) -> Option<FantasticBitsOutcome> {
        // Phase 1: no physics, no scoring. Just count ticks and end as
        // a draw at the timeout. The bots still see valid input every
        // tick (entities don't move yet) so the harness round-trips.
        self.tick += 1;

        // Magic regenerates +1/tick, capped at 100. Already valid behaviour
        // even without the rest of the game; bots can observe it growing.
        for m in &mut self.magic {
            *m = (*m + 1).min(100);
        }

        if self.tick >= MAX_TICKS {
            let outcome = self.make_outcome();
            self.active.clear();
            self.outcome = Some(outcome.clone());
            return Some(outcome);
        }
        None
    }

    fn active_players(&self) -> &[PlayerId] {
        &self.active
    }

    fn standings(outcome: &FantasticBitsOutcome) -> Vec<u32> {
        outcome.standings.clone()
    }
}

impl FantasticBitsGame {
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
//  Setup helpers
// ============================================================

fn wizard_team(id: i32) -> usize {
    // Wizards are the first 4 entities, in spawn order:
    // ids 0,1 → team 0; ids 2,3 → team 1.
    if (id as usize) < NUM_WIZARDS_PER_PLAYER {
        0
    } else {
        1
    }
}

/// Wizard placement mirrors the referee's `generatePlayers` exactly:
///
/// ```text
/// for j in 0..2 (team):
///   for i in 0..2 (wizard within team):
///     x = j * (WIDTH - 2000) + 1000           // 1000 or 15000
///     sign = (j % 2 == 0) ? -1 : 1            // team 0 above center, team 1 below
///     y = HEIGHT/2 + sign * (SPACE_BETWEEN_POD*i - SPACE_BETWEEN_POD/2)
/// ```
///
/// For 2 wizards per team with `SPACE_BETWEEN_POD = 3000`, this lands
/// them at `y = 2250` and `y = 5250` (1500 above/below mid-court).
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
                    x,
                    y,
                    vx: 0.0,
                    vy: 0.0,
                },
                holding: None,
            });
            *next_id += 1;
        }
    }
    out
}

/// Snaffle placement matches the referee's `generateSnaffles`:
///   * Pick `pair_count` non-overlapping (x, y) points by reject sampling
///     against already-placed snaffles (min distance `MIN_SPACE_BETWEEN_SNAFFLES`).
///   * For each, also add the 180°-rotated mirror `(WIDTH-x, HEIGHT-y)`.
///   * Add a single centerline snaffle last, at exactly `(WIDTH/2, HEIGHT/2)`.
///
/// The min-distance check is against existing snaffles only (NOT against
/// the not-yet-placed mirror), faithfully reproducing the referee's quirk.
fn place_snaffles(pair_count: u32, rng: &mut GameRng, next_id: &mut i32) -> Vec<Snaffle> {
    // Referee bounds:
    //   x = 2000 + random.nextInt(WIDTH/2 - 3000)  → 2000..WIDTH/2-1000
    //   y =  500 + random.nextInt(HEIGHT - 1000)   →  500..HEIGHT-500
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
            let dx = s.disc.x - x;
            let dy = s.disc.y - y;
            dx * dx + dy * dy < min_dist_sq
        });
        if collides {
            continue;
        }
        out.push(Snaffle {
            disc: DiscState {
                id: *next_id,
                x,
                y,
                vx: 0.0,
                vy: 0.0,
            },
            held_by: None,
        });
        out.push(Snaffle {
            disc: DiscState {
                id: *next_id + 1,
                x: WIDTH as f64 - x,
                y: HEIGHT as f64 - y,
                vx: 0.0,
                vy: 0.0,
            },
            held_by: None,
        });
        *next_id += 2;
        placed += 1;
    }
    // Centerline snaffle, added last per referee.
    out.push(Snaffle {
        disc: DiscState {
            id: *next_id,
            x: (WIDTH as f64) / 2.0,
            y: (HEIGHT as f64) / 2.0,
            vx: 0.0,
            vy: 0.0,
        },
        held_by: None,
    });
    *next_id += 1;
    out
}

/// Bludgers spawn horizontally symmetric around the centerline at mid
/// height, separated by enough room for a snaffle to pass between them:
/// `WIDTH/2 ± (SNAFFLE_RADIUS + 2 * BLUDGER_RADIUS)`.
fn place_bludgers(next_id: &mut i32) -> Vec<Bludger> {
    let cx = WIDTH as f64 / 2.0;
    let cy = HEIGHT as f64 / 2.0;
    let dx = (SNAFFLE_RADIUS + 2 * BLUDGER_RADIUS) as f64;
    let mut out = Vec::with_capacity(NUM_BLUDGERS);
    for x in [cx - dx, cx + dx] {
        out.push(Bludger {
            disc: DiscState {
                id: *next_id,
                x,
                y: cy,
                vx: 0.0,
                vy: 0.0,
            },
            last_victim: -1,
        });
        *next_id += 1;
    }
    out
}

fn disc_to_entity(d: &DiscState, kind: EntityKind, state: i32) -> Entity {
    Entity {
        id: d.id,
        kind,
        x: round_half_away(d.x),
        y: round_half_away(d.y),
        vx: round_half_away(d.vx),
        vy: round_half_away(d.vy),
        state,
    }
}

/// "Round half away from zero" — CodinGame's rounding rule per the
/// expert rules. 23.5 → 24, -23.5 → -24. We'll apply this to per-tick
/// position/velocity updates in phase 2's physics; for now it only runs
/// at `input_for` time when projecting f64 state to the i32 wire.
fn round_half_away(v: f64) -> i32 {
    if v >= 0.0 {
        (v + 0.5).floor() as i32
    } else {
        (v - 0.5).ceil() as i32
    }
}

// Plugin glue: marks this game as FFI-playable and points at the `_defs`
// crate's `Ffi` marker. Everything else (FFI fn-pointer shape, ABI version,
// symbol names) flows from `fantastic_bits_defs::Ffi`'s `Defs` impl.
impl FfiGame for FantasticBitsGame {
    type Defs = fantastic_bits_defs::Ffi;
}

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
        assert!(g.snaffles.len() == 5 || g.snaffles.len() == 7);
        assert!(g.score_to_win() == 3 || g.score_to_win() == 4);

        // The last snaffle is the centerline one, at exactly (W/2, H/2).
        let center = g.snaffles.last().unwrap();
        assert_eq!(center.disc.x, (WIDTH as f64) / 2.0);
        assert_eq!(center.disc.y, (HEIGHT as f64) / 2.0);

        // The remaining snaffles come in 180°-rotated pairs:
        // for each (x, y) there must exist a partner at (W-x, H-y).
        for i in (0..g.snaffles.len() - 1).step_by(2) {
            let a = &g.snaffles[i].disc;
            let b = &g.snaffles[i + 1].disc;
            assert_eq!(a.x + b.x, WIDTH as f64);
            assert_eq!(a.y + b.y, HEIGHT as f64);
        }
    }

    #[test]
    fn wizard_spawn_matches_referee() {
        let g = game(0);
        // referee: x = j * (WIDTH - 2000) + 1000; y = H/2 ± 1500
        // (with NUM_WIZARDS_PER_PLAYER = 2 and SPACE_BETWEEN_POD = 3000)
        let positions: Vec<(f64, f64)> = g.wizards.iter().map(|w| (w.disc.x, w.disc.y)).collect();
        assert_eq!(positions[0], (1000.0, 5250.0)); // P0 wiz 0
        assert_eq!(positions[1], (1000.0, 2250.0)); // P0 wiz 1
        assert_eq!(positions[2], (15000.0, 2250.0)); // P1 wiz 0
        assert_eq!(positions[3], (15000.0, 5250.0)); // P1 wiz 1
    }

    #[test]
    fn bludger_spawn_matches_referee() {
        let g = game(0);
        let cx = WIDTH as f64 / 2.0;
        let cy = HEIGHT as f64 / 2.0;
        let off = (SNAFFLE_RADIUS + 2 * BLUDGER_RADIUS) as f64;
        let positions: Vec<(f64, f64)> = g.bludgers.iter().map(|b| (b.disc.x, b.disc.y)).collect();
        assert_eq!(positions[0], (cx - off, cy));
        assert_eq!(positions[1], (cx + off, cy));
    }

    #[test]
    fn entity_ids_match_referee_spawn_order() {
        let g = game(0);
        // Wizards always claim 0..3 — those are the first entities created.
        for (i, w) in g.wizards.iter().enumerate() {
            assert_eq!(w.disc.id, i as i32);
        }
        // Snaffles are next, contiguous, starting at 4. Bludgers follow.
        let first_snaffle_id = 4;
        for (i, s) in g.snaffles.iter().enumerate() {
            assert_eq!(s.disc.id, first_snaffle_id + i as i32);
        }
        let first_bludger_id = first_snaffle_id + g.snaffles.len() as i32;
        for (i, b) in g.bludgers.iter().enumerate() {
            assert_eq!(b.disc.id, first_bludger_id + i as i32);
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

        // Player 1's own wizards should be the ones player 0 sees as
        // opponents — match by id.
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
    fn step_times_out_as_draw() {
        let mut g = game(0);
        let outputs = vec![None, None];
        let mut last: Option<FantasticBitsOutcome> = None;
        for _ in 0..MAX_TICKS + 5 {
            last = g.step(&outputs).or(last);
            if !g.active_players().is_empty() {
                continue;
            }
            break;
        }
        let outcome = last.expect("game should have ended");
        assert_eq!(outcome.score, [0, 0]);
        assert!(outcome.winner.is_none());
        assert_eq!(outcome.standings, vec![1, 1]);
    }

    #[test]
    fn magic_regenerates_and_caps() {
        let mut g = game(0);
        let outputs = vec![None, None];
        for _ in 0..50 {
            g.step(&outputs);
        }
        assert_eq!(g.magic(), [50, 50]);
        for _ in 0..100 {
            g.step(&outputs);
        }
        assert_eq!(g.magic(), [100, 100]);
    }

    #[test]
    fn round_half_away_from_zero() {
        assert_eq!(round_half_away(0.4), 0);
        assert_eq!(round_half_away(0.5), 1);
        assert_eq!(round_half_away(23.5), 24);
        assert_eq!(round_half_away(-0.5), -1);
        assert_eq!(round_half_away(-23.5), -24);
        assert_eq!(round_half_away(-0.4), 0);
    }
}
