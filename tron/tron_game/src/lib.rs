use common::engine::{FfiGame, Game, PlayerError, PlayerId};
use tron_defs::{
    BotStatus, Direction, Line, Pos, TurnInput, TurnInputFFI, TurnOutput, TurnResult,
};

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
}

#[derive(Debug, Clone)]
pub struct TronOutcome {
    pub winner: Option<PlayerId>,
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
    type InitialInput = ();
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
        TronGame {
            num_players,
            heads,
            prev_heads,
            trails,
            alive,
            active,
            last_moves,
        }
    }

    fn initial_input(&self, _player: PlayerId) {}

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

        // 5. Game ends when ≤ 1 alive.
        match self.active.len() {
            0 => Some(TronOutcome { winner: None }),
            1 => Some(TronOutcome {
                winner: Some(self.active[0]),
            }),
            _ => None,
        }
    }

    fn active_players(&self) -> &[PlayerId] {
        &self.active
    }
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

// Plugin glue: tell the generic `PluginPlayer<G>` in `common::engine` how to
// talk to a tron bot. `PluginPlayer<TronGame>` is the concrete type the runner
// uses; we don't need a tron-specific player struct.
impl FfiGame for TronGame {
    type Symbol = for<'a> unsafe extern "C" fn(TurnInputFFI<'a>) -> TurnResult;
    // Tron has no per-bot init; the symbol below is never loaded because tron
    // bots don't export `initialize`. The type only exists to satisfy the trait.
    type InitSymbol = unsafe extern "C" fn();

    const SYMBOL_NAME: &'static [u8] = b"take_turn";
    const INIT_SYMBOL_NAME: &'static [u8] = b"initialize";

    unsafe fn call(sym: Self::Symbol, input: &TurnInput) -> Result<TurnOutput, PlayerError> {
        let result = unsafe { sym(input.as_ffi()) };
        match result.status {
            BotStatus::Ok => Ok(result.output),
            BotStatus::Panic => Err(PlayerError::Panic),
        }
    }

    unsafe fn call_init(_sym: Self::InitSymbol, _input: &()) -> Result<(), PlayerError> {
        // Unreachable in practice: tron bots don't export `initialize`, so
        // `PluginPlayer` never has a non-`None` `init_sym` for tron.
        Ok(())
    }
}
