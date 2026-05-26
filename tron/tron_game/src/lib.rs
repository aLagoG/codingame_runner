use common::engine::{FfiGame, Game, NoInitialInput, PlayerId};
use tron_defs::{Direction, Line, Pos, TurnInput, TurnOutput};

const WIDTH: i32 = 30;
const HEIGHT: i32 = 20;

// One starting corner per supported seat. CodinGame's real Tron randomizes
// these; for now we hand them out deterministically.
const STARTS: [Pos; 4] = [
    Pos { x: 0, y: 0 },
    Pos {
        x: WIDTH - 1,
        y: HEIGHT - 1,
    },
    Pos {
        x: 0,
        y: HEIGHT - 1,
    },
    Pos {
        x: WIDTH - 1,
        y: 0,
    },
];

const DEAD_LINE: Line = Line {
    start: Pos { x: -1, y: -1 },
    end: Pos { x: -1, y: -1 },
};

pub struct TronGame {
    num_players: u32,
    heads: Vec<Pos>,
    prev_heads: Vec<Pos>,
    trails: Vec<Vec<Pos>>,
    alive: Vec<bool>,
    active: Vec<PlayerId>,
    last_moves: Vec<Option<Direction>>,
    /// Tick at which each player died, or None if still alive when
    /// the match ended. Populated by `step()` as `alive[i]` flips
    /// false. Drives placement: later death = better rank.
    death_tick: Vec<Option<u32>>,
    /// Number of completed steps so far. Used to stamp `death_tick`.
    tick: u32,
}

#[derive(Debug, Clone)]
pub struct TronOutcome {
    pub winner: Option<PlayerId>,
    /// Final ranking, 1-indexed, in player-id order. Survivors share
    /// rank 1; dead players are ranked by death tick descending
    /// (later death = better rank). Mutual same-tick deaths produce
    /// tied ranks via competition ranking (e.g. `[1, 1, 3, 3]`).
    pub placement: Vec<u32>,
    /// Trail length per player at game end, in player-id order.
    /// A natural continuous metric for tron — survives the rank
    /// tiebreaker case "two bots both made it to rank 1 but one
    /// carved a 200-cell snake and the other carved 50".
    pub trail_lengths: Vec<u32>,
}

impl TronGame {
    pub fn alive(&self) -> &[bool] {
        &self.alive
    }
    pub fn heads(&self) -> &[Pos] {
        &self.heads
    }
    pub fn trails(&self) -> &[Vec<Pos>] {
        &self.trails
    }
    /// What each player submitted last tick. `None` means they were inactive
    /// or failed to produce output. Before any step has run, all entries are
    /// `None`.
    pub fn last_moves(&self) -> &[Option<Direction>] {
        &self.last_moves
    }
}

// TODO: review all this code
impl Game for TronGame {
    const NAME: &'static str = "tron";

    type InitialInput = NoInitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    type Outcome = TronOutcome;

    fn new(num_players: u32, _seed: u64) -> Self {
        assert!(
            num_players >= 1 && (num_players as usize) <= STARTS.len(),
            "TronGame supports 1..={} players",
            STARTS.len()
        );
        let n = num_players as usize;
        let heads: Vec<Pos> = STARTS[..n].to_vec();
        let prev_heads = heads.clone();
        let trails: Vec<Vec<Pos>> = heads.iter().map(|p| vec![*p]).collect();
        let alive = vec![true; n];
        let active: Vec<PlayerId> = (0..num_players).collect();
        let last_moves = vec![None; n];
        let death_tick = vec![None; n];
        TronGame {
            num_players,
            heads,
            prev_heads,
            trails,
            alive,
            active,
            last_moves,
            death_tick,
            tick: 0,
        }
    }

    fn initial_input(&self, _player: PlayerId) -> NoInitialInput {
        NoInitialInput::default()
    }

    fn input_for(&self, player: PlayerId) -> TurnInput {
        let player_lines = (0..self.num_players as usize)
            .map(|i| {
                if self.alive[i] {
                    Line {
                        start: self.prev_heads[i],
                        end: self.heads[i],
                    }
                } else {
                    DEAD_LINE
                }
            })
            .collect();
        TurnInput {
            number_of_players: self.num_players as i32,
            player_number: player as i32,
            player_lines,
        }
    }

    fn step(&mut self, outputs: &[Option<TurnOutput>]) -> Option<TronOutcome> {
        self.last_moves = outputs
            .iter()
            .map(|o| o.as_ref().map(|t| t.direction))
            .collect();

        // 1. Each active player picks a new head. Missing output = elimination.
        let mut new_heads: Vec<Option<Pos>> = vec![None; self.num_players as usize];
        for &p in &self.active {
            let idx = p as usize;
            let Some(out) = outputs[idx].as_ref() else {
                // No move from this player this tick → eliminate.
                self.alive[idx] = false;
                continue;
            };
            let next = apply_direction(self.heads[idx], out.direction);
            new_heads[idx] = Some(next);
        }

        // 2. Resolve collisions: out-of-bounds, into any existing trail, or
        //    head-on with another player landing on the same cell this tick.
        for i in 0..self.num_players as usize {
            let Some(next) = new_heads[i] else { continue };

            let dies = !in_bounds(next)
                || self.trails.iter().any(|t| t.iter().any(|&p| p == next))
                || (0..self.num_players as usize).any(|j| j != i && new_heads[j] == Some(next));

            if dies {
                self.alive[i] = false;
                new_heads[i] = None;
            }
        }

        // 3. Commit surviving moves.
        for i in 0..self.num_players as usize {
            if let Some(next) = new_heads[i] {
                self.prev_heads[i] = self.heads[i];
                self.heads[i] = next;
                self.trails[i].push(next);
            }
        }

        // 4. Recompute active set.
        self.active = (0..self.num_players)
            .filter(|&p| self.alive[p as usize])
            .collect();

        // 5. Stamp newly-dead players with this tick's number, so
        //    placement can rank them by death order at game-end.
        for i in 0..self.num_players as usize {
            if !self.alive[i] && self.death_tick[i].is_none() {
                self.death_tick[i] = Some(self.tick);
            }
        }
        self.tick += 1;

        // 6. Game ends when ≤ 1 alive.
        let make_outcome = |winner| TronOutcome {
            winner,
            placement: compute_placement(&self.alive, &self.death_tick),
            trail_lengths: self
                .trails
                .iter()
                .map(|t| t.len() as u32)
                .collect(),
        };
        match self.active.len() {
            0 => Some(make_outcome(None)),
            1 => Some(make_outcome(Some(self.active[0]))),
            _ => None,
        }
    }

    fn active_players(&self) -> &[PlayerId] {
        &self.active
    }

    fn placement(outcome: &TronOutcome) -> Vec<u32> {
        outcome.placement.clone()
    }

    fn scores(outcome: &TronOutcome) -> Option<Vec<f64>> {
        Some(outcome.trail_lengths.iter().map(|&n| n as f64).collect())
    }
}

/// Competition ranking from alive flags + death ticks. Survivors get
/// rank 1; dead players are ranked by death tick descending (later
/// death = better rank); ties share a rank with gaps (1, 1, 3 — not
/// 1, 1, 2). For player `i`, rank = 1 + (number of strictly-better
/// other players).
fn compute_placement(alive: &[bool], death_tick: &[Option<u32>]) -> Vec<u32> {
    let n = alive.len();
    let mut placement = vec![0u32; n];
    for i in 0..n {
        let mut strictly_better = 0u32;
        for j in 0..n {
            if i == j {
                continue;
            }
            let j_better = match (alive[i], alive[j]) {
                (true, true) => false,            // both alive → tie
                (true, false) => false,           // i alive > j dead
                (false, true) => true,            // j alive > i dead
                (false, false) => match (death_tick[i], death_tick[j]) {
                    (Some(ti), Some(tj)) => tj > ti, // j died later → j better
                    _ => false,
                },
            };
            if j_better {
                strictly_better += 1;
            }
        }
        placement[i] = 1 + strictly_better;
    }
    placement
}

fn in_bounds(p: Pos) -> bool {
    p.x >= 0 && p.x < WIDTH && p.y >= 0 && p.y < HEIGHT
}

fn apply_direction(p: Pos, d: Direction) -> Pos {
    match d {
        Direction::Up => Pos { x: p.x, y: p.y - 1 },
        Direction::Down => Pos { x: p.x, y: p.y + 1 },
        Direction::Left => Pos { x: p.x - 1, y: p.y },
        Direction::Right => Pos { x: p.x + 1, y: p.y },
    }
}

// Plugin glue: marks TronGame as FFI-playable and points at the `_defs`
// crate's Ffi marker. Everything else (FFI fn-pointer shape, ABI version,
// symbol names) flows from `tron_defs::Ffi`'s `Defs` impl.
impl FfiGame for TronGame {
    type Defs = tron_defs::Ffi;
}
