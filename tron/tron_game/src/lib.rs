use common::engine::{FfiGame, Game, GameRng, NoInitialInput, PlayerId};
use rand::RngExt;
use tron_defs::{Direction, Line, Pos, TurnInput, TurnOutput};

const WIDTH: i32 = 30;
const HEIGHT: i32 = 20;

const MIN_PLAYERS: u32 = 2;
const MAX_PLAYERS: u32 = 4;

const DEAD_LINE: Line = Line {
    start: Pos { x: -1, y: -1 },
    end: Pos { x: -1, y: -1 },
};

pub struct TronGame {
    num_players: u32,
    heads: Vec<Pos>,
    /// Where each player spawned. Reported to bots as the `start`
    /// of their `Line` every turn — matches CodinGame's `(X0, Y0)`
    /// in the `X0 Y0 X1 Y1` per-player input. Set once in `new`
    /// and never mutated after that.
    starting_points: Vec<Pos>,
    /// Cell-owner grid. `board[y][x]` is `None` for empty, or
    /// `Some(pid)` for `pid`'s trail. Outer Vec is HEIGHT rows;
    /// each inner Vec is WIDTH columns. Collision checks are O(1)
    /// instead of the old "scan every cell every other player
    /// has touched".
    board: Vec<Vec<Option<PlayerId>>>,
    alive: Vec<bool>,
    /// Whose turn it is this tick. Always either empty (game over)
    /// or holds exactly one player id — tron is sequential: 0 → 1
    /// → 2 → 0 → … skipping dead players. Kept as a one-element
    /// `Vec<PlayerId>` because `Game::active_players` returns
    /// `&[PlayerId]`.
    active: Vec<PlayerId>,
    /// Tick at which each player died, or None if still alive when
    /// the match ended. Populated by `step()` as `alive[i]` flips
    /// false. Drives standings: later death = better rank.
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
    pub standings: Vec<u32>,
}

impl TronGame {
    pub fn alive(&self) -> &[bool] {
        &self.alive
    }
    pub fn heads(&self) -> &[Pos] {
        &self.heads
    }
    /// HEIGHT rows × WIDTH columns. `board()[y][x]` is `None` for
    /// empty, `Some(pid)` for that player's trail.
    pub fn board(&self) -> &[Vec<Option<PlayerId>>] {
        &self.board
    }

    /// Build a `TronOutcome` from the current alive/death state, tagged
    /// with the given winner. Used at every game-ending branch of `step`.
    fn make_outcome(&self, winner: Option<PlayerId>) -> TronOutcome {
        TronOutcome {
            winner,
            standings: compute_standings(&self.alive, &self.death_tick),
        }
    }
}


// TODO: review all this code
impl Game for TronGame {
    const NAME: &'static str = "tron";

    type InitialInput = NoInitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    type Outcome = TronOutcome;

    fn new(num_players: u32, rng: &mut GameRng) -> Self {
        assert!(
            num_players >= MIN_PLAYERS && num_players <= MAX_PLAYERS,
            "TronGame supports {}..={} players",
            MIN_PLAYERS,
            MAX_PLAYERS
        );
        let n = num_players as usize;
        let mut board: Vec<Vec<Option<PlayerId>>> =
            vec![vec![None; WIDTH as usize]; HEIGHT as usize];
        let heads: Vec<Pos> = (0..num_players)
            .map(|pid| {
                // Reject sample loop until we land on an empty cell.
                // Cheap — the grid is huge relative to the 2-4 spawns.
                loop {
                    let x = rng.random_range(0..WIDTH);
                    let y = rng.random_range(0..HEIGHT);
                    if board[y as usize][x as usize].is_none() {
                        board[y as usize][x as usize] = Some(pid);
                        return Pos { x, y };
                    }
                }
            })
            .collect();
        let starting_points = heads.clone();
        let alive = vec![true; n];
        // Sequential play — only player 0 moves first.
        let active: Vec<PlayerId> = vec![0];
        let death_tick = vec![None; n];
        TronGame {
            num_players,
            heads,
            starting_points,
            board,
            alive,
            active,
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
                        start: self.starting_points[i],
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
        // Tron is sequential: exactly one player moves per tick.
        // `self.active` holds the player id whose turn this is.
        let Some(&p) = self.active.first() else {
            // No active player → game's already over. Defensive
            // no-op; the engine shouldn't be calling step() in this
            // state, but if it does we just return the same outcome
            // we'd have returned last time.
            return Some(self.make_outcome(lone_survivor(&self.alive)));
        };
        let pidx = p as usize;

        // 1. Read the active player's chosen direction (or note the
        //    absence — missing output means they forfeit this turn).
        let chosen = outputs[pidx].as_ref().map(|t| t.direction);

        // 2. Resolve the move. Three death modes:
        //      a. No output at all → forfeit.
        //      b. Steps out of bounds.
        //      c. Steps into any existing trail cell (own or other).
        //    Head-on collisions don't exist in turn-based play — only
        //    one player moves per tick.
        match chosen {
            None => {
                self.alive[pidx] = false;
            }
            Some(dir) => {
                let next = apply_direction(self.heads[pidx], dir);
                let dies = !in_bounds(next)
                    || self.board[next.y as usize][next.x as usize].is_some();
                if dies {
                    self.alive[pidx] = false;
                } else {
                    self.heads[pidx] = next;
                    self.board[next.y as usize][next.x as usize] = Some(p);
                }
            }
        }

        // 3. If this move killed the active player, stamp the
        //    death tick AND erase their trail from the board.
        //    Statement: "its light ribbon disappears" — survivors
        //    can pass through cells the dead player used to occupy.
        //
        //    O(grid) sweep on each death; deaths happen at most
        //    `num_players - 1` times per match so total cost is
        //    bounded. If this ever shows up in a profile, swap to
        //    the alive-aware-collision design: leave board cells
        //    stamped and change the collision check to
        //    `board[y][x].is_some_and(|owner| self.alive[owner])`.
        //    That makes the check O(1) at the cost of every reader
        //    needing to remember the alive lookup.
        if !self.alive[pidx] && self.death_tick[pidx].is_none() {
            self.death_tick[pidx] = Some(self.tick);
            for row in self.board.iter_mut() {
                for cell in row.iter_mut() {
                    if *cell == Some(p) {
                        *cell = None;
                    }
                }
            }
        }
        self.tick += 1;

        // 4. Advance the cursor + game-over check. Find the next
        //    alive player after `p` (wrapping). 0 alive → draw;
        //    1 alive → that player wins; otherwise the next alive
        //    player is the new active singleton.
        let alive_count = self.alive.iter().filter(|&&a| a).count();
        match alive_count {
            0 => {
                self.active.clear();
                Some(self.make_outcome(None))
            }
            1 => {
                self.active.clear();
                Some(self.make_outcome(lone_survivor(&self.alive)))
            }
            _ => {
                let next = next_alive_after(p, self.num_players, &self.alive)
                    .expect("alive_count > 1 implies a next alive player");
                self.active.clear();
                self.active.push(next);
                None
            }
        }
    }

    fn active_players(&self) -> &[PlayerId] {
        &self.active
    }

    fn standings(outcome: &TronOutcome) -> Vec<u32> {
        outcome.standings.clone()
    }

    // No `scores` override — `Game::scores` defaults to `None`,
    // which is correct for tron: every active player moves every
    // tick, so any plausible "score" (trail length, ticks alive,
    // …) is redundant with `standings` / `death_tick` and adds no
    // signal.
}

/// Competition ranking from alive flags + death ticks. Survivors get
/// rank 1; dead players are ranked by death tick descending (later
/// death = better rank); ties share a rank with gaps (1, 1, 3 — not
/// 1, 1, 2). For player `i`, rank = 1 + (number of strictly-better
/// other players).
/// Cursor advance: returns the next player id after `from`
/// (wrapping at `num_players`) whose `alive` slot is true. `None`
/// only when nobody is alive — callers handle game-over separately.
fn next_alive_after(from: PlayerId, num_players: u32, alive: &[bool]) -> Option<PlayerId> {
    for step in 1..=num_players {
        let candidate = (from + step) % num_players;
        if alive[candidate as usize] {
            return Some(candidate);
        }
    }
    None
}

/// Convenience: returns the single living player's id, or None if
/// 0 / >1 are alive. Used to label the winner when the match ends
/// with exactly one survivor.
fn lone_survivor(alive: &[bool]) -> Option<PlayerId> {
    let mut iter = alive
        .iter()
        .enumerate()
        .filter(|&(_, &a)| a)
        .map(|(i, _)| i as PlayerId);
    let first = iter.next()?;
    if iter.next().is_some() { None } else { Some(first) }
}

fn compute_standings(alive: &[bool], death_tick: &[Option<u32>]) -> Vec<u32> {
    let n = alive.len();
    let mut standings = vec![0u32; n];
    for i in 0..n {
        let mut strictly_better = 0u32;
        for j in 0..n {
            if i == j {
                continue;
            }
            let j_better = match (alive[i], alive[j]) {
                (true, true) => false,  // both alive → tie
                (true, false) => false, // i alive > j dead
                (false, true) => true,  // j alive > i dead
                (false, false) => match (death_tick[i], death_tick[j]) {
                    (Some(ti), Some(tj)) => tj > ti, // j died later → j better
                    _ => false,
                },
            };
            if j_better {
                strictly_better += 1;
            }
        }
        standings[i] = 1 + strictly_better;
    }
    standings
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
