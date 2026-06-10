use std::collections::VecDeque;
use std::num::NonZeroU8;

use tron_defs::{Direction, InitialInput, Pos, TurnInput, TurnOutput};

const WIDTH: usize = 30;
const HEIGHT: usize = 20;
const MAX_DEPTH: i32 = 5;
const MAX_PLAYERS: usize = 4;

// Order matches v1's iteration (UP, RIGHT, DOWN, LEFT). With a
// heuristic that often returns ties between moves, the first-tried
// direction wins, so this matters more than it looks.
const MOVES: [(Direction, Pos); 4] = [
    (Direction::Up, Pos::new(0, -1)),
    (Direction::Right, Pos::new(1, 0)),
    (Direction::Down, Pos::new(0, 1)),
    (Direction::Left, Pos::new(-1, 0)),
];

type ID = NonZeroU8;
// IDs are 1..=MAX_PLAYERS — slot number = player_number + 1.

pub struct GameState {
    // map[y][x] — HEIGHT rows × WIDTH columns, matching the engine
    // convention in games/tron/game/src/lib.rs.
    map: [[Option<ID>; WIDTH]; HEIGHT],
    n_players: u8,
    my_id: ID,
    // alive[id] / heads[id] for id in 1..=MAX_PLAYERS; index 0 unused.
    // heads[id] is only meaningful when alive[id].
    alive: [bool; MAX_PLAYERS + 1],
    heads: [Pos; MAX_PLAYERS + 1],
    heuristic_state: HeuristicState,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            map: [[None; WIDTH]; HEIGHT],
            n_players: 0,
            my_id: ID::new(1u8).unwrap(),
            alive: [false; MAX_PLAYERS + 1],
            heads: [Pos::new(0, 0); MAX_PLAYERS + 1],
            heuristic_state: HeuristicState::default(),
        }
    }
}

impl GameState {
    fn at(&self, pos: &Pos) -> Option<ID> {
        self.map[pos.y as usize][pos.x as usize]
    }

    fn set_pos(&mut self, id: ID, pos: &Pos) {
        self.map[pos.y as usize][pos.x as usize] = Some(id);
    }

    // Passable if in-bounds AND (empty OR owned by a dead player —
    // dead trails are erased by the engine, so they're free to walk on).
    fn is_pos_passable(&self, pos: &Pos) -> bool {
        is_valid_pos(pos)
            && self
                .at(pos)
                .is_none_or(|owner| !self.alive[owner.get() as usize])
    }

    fn alive_count(&self) -> usize {
        (1..=MAX_PLAYERS).filter(|&i| self.alive[i]).count()
    }

    fn is_game_over(&self) -> bool {
        self.alive_count() < 2
    }

    fn winner_id(&self) -> Option<ID> {
        if self.alive_count() == 1 {
            (1..=MAX_PLAYERS as u8)
                .find(|&i| self.alive[i as usize])
                .and_then(NonZeroU8::new)
        } else {
            None
        }
    }

    // scores[id] for id in 1..=MAX_PLAYERS. v1-style multiplicative
    // depth scaling: `(remaining_depth + 1) * ±2*W*H`. Winning sooner
    // is *much* better than winning later (factor of 6× at the root
    // with MAX_DEPTH=5). Magnitude matches the suicide penalty so
    // "I lose via game-over" and "I lose via suicide" score
    // consistently.
    fn game_over_score(&self, depth: i32) -> [i32; MAX_PLAYERS + 1] {
        let mut res = [0i32; MAX_PLAYERS + 1];
        let winner = self.winner_id();
        let magnitude = (WIDTH * HEIGHT) as i32 * 2 * (MAX_DEPTH - depth + 1);
        for i in 1..res.len() {
            res[i] = if winner.is_some_and(|p| p.get() as usize == i) {
                magnitude
            } else {
                -magnitude
            };
        }
        res
    }

    fn heuristic(&mut self) -> [i32; MAX_PLAYERS + 1] {
        self.heuristic_state.start_epoch();
        let queue = &mut self.heuristic_state.queue;
        queue.clear();
        for i in 1..=MAX_PLAYERS {
            if self.alive[i] {
                let id = ID::new(i as u8).unwrap();
                queue.push_back(HeuristicSearchNode::new(id, self.heads[i], 0));
            }
        }

        while let Some(current) = self.heuristic_state.queue.pop_front() {
            for (_, mv) in &MOVES {
                let moved = current.position + mv;
                if self.is_pos_passable(&moved)
                    && self
                        .heuristic_state
                        .set(&moved, current.player_id, current.distance + 1)
                {
                    self.heuristic_state.queue.push_back(HeuristicSearchNode::new(
                        current.player_id,
                        moved,
                        current.distance + 1,
                    ));
                }
            }
        }

        self.heuristic_state.scores
    }
}

struct HeuristicSearchNode {
    player_id: ID,
    position: Pos,
    distance: u32,
}

impl HeuristicSearchNode {
    fn new(player_id: ID, position: Pos, distance: u32) -> Self {
        Self {
            player_id,
            position,
            distance,
        }
    }
}

struct HeuristicState {
    board: [[HeuristicNode; WIDTH]; HEIGHT],
    // How many squares each player controls in the current heuristic call.
    scores: [i32; MAX_PLAYERS + 1],
    // Epoch counter: lets us treat `board` as freshly zeroed every
    // heuristic call without actually zeroing it.
    epoch: u32,
    // BFS work queue, reused across heuristic calls to avoid per-leaf
    // allocations (the heuristic fires at every search leaf, ~1000×
    // per top-level move).
    queue: VecDeque<HeuristicSearchNode>,
}

impl Default for HeuristicState {
    fn default() -> Self {
        Self {
            board: Default::default(),
            scores: Default::default(),
            epoch: 0,
            queue: VecDeque::with_capacity(WIDTH * HEIGHT),
        }
    }
}

impl HeuristicState {
    fn start_epoch(&mut self) {
        self.epoch += 1;
        self.scores.fill(0);
    }

    // Returns true iff `id` should expand from `pos` further (i.e.
    // this call either claimed the cell for `id` or marked it as
    // contested with `id` newly participating).
    //
    // Tie handling: when two players reach a cell at the same
    // shortest distance, the cell becomes neutral — counted for
    // *neither*. `owners` is a bitmask of every player whose
    // shortest distance equals `distance`; only cells with exactly
    // one bit set contribute to `scores`.
    fn set(&mut self, pos: &Pos, id: ID, current_distance: u32) -> bool {
        let id_bit = 1u8 << (id.get() - 1);
        let epoch = self.epoch;
        let node = self.board[pos.y as usize][pos.x as usize];

        if node.epoch_last_set < epoch {
            // First visit this epoch — claim uniquely.
            self.board[pos.y as usize][pos.x as usize] = HeuristicNode {
                epoch_last_set: epoch,
                distance: current_distance,
                owners: id_bit,
            };
            self.scores[id.get() as usize] += 1;
            return true;
        }

        // Same epoch. Multi-source BFS visits cells in non-decreasing
        // distance order, so `current_distance >= node.distance` always.
        if current_distance > node.distance {
            return false;
        }

        if node.owners & id_bit != 0 {
            // Same player reaching this cell at the same distance via
            // a second BFS path — no state change, no re-enqueue.
            return false;
        }

        // Different player at the same shortest distance ⇒ contested.
        // If the cell was uniquely owned, flip its credit off. If it
        // was already contested (≥2 owners), no credit was held.
        if node.owners.count_ones() == 1 {
            let prev_id = (node.owners.trailing_zeros() + 1) as usize;
            self.scores[prev_id] -= 1;
        }
        self.board[pos.y as usize][pos.x as usize].owners |= id_bit;

        // Still expand: cells beyond a contested junction may be
        // uniquely reachable through it for this player.
        true
    }
}

#[derive(Clone, Copy, Default)]
struct HeuristicNode {
    epoch_last_set: u32,
    distance: u32,
    // Bitmask: bit (i-1) set ⇒ player i reaches this cell at
    // `distance`. Cells with `owners.count_ones() == 1` are credited
    // in `scores`; cells with ≥2 bits set are neutral.
    owners: u8,
}

fn is_valid_pos(pos: &Pos) -> bool {
    (0..(WIDTH as i32)).contains(&pos.x) && (0..(HEIGHT as i32)).contains(&pos.y)
}

// Find the next alive player after `current`, wrapping. Callers only
// invoke this when `alive_count >= 2`, so a next alive always exists.
fn next_alive_after(state: &GameState, current: ID) -> ID {
    let n = state.n_players as usize;
    for step in 1..=n {
        let cand = ((current.get() as usize - 1 + step) % n) + 1;
        if state.alive[cand] {
            return ID::new(cand as u8).unwrap();
        }
    }
    unreachable!("next_alive_after called with alive_count < 2")
}

// `current_player` is the player whose turn it is. Try every move
// they have, recurse to the next alive player, and return the
// max^n score vector (each player maximizes their own slot) along
// with the direction that achieved it. The direction is `None` at
// terminal nodes (game over, depth cap, or current_player has no
// legal moves); only the top-level caller cares about it.
fn search(
    state: &mut GameState,
    current_player: ID,
    depth: i32,
) -> ([i32; MAX_PLAYERS + 1], Option<Direction>) {
    if state.is_game_over() {
        return (state.game_over_score(depth), None);
    }
    if depth >= MAX_DEPTH {
        return (state.heuristic(), None);
    }

    let cp_idx = current_player.get() as usize;
    let head = state.heads[cp_idx];
    let mut best_scores = [i32::MIN; MAX_PLAYERS + 1];
    let mut best_dir: Option<Direction> = None;

    for (dir, mv) in MOVES {
        let next = head + mv;
        if !state.is_pos_passable(&next) {
            continue;
        }

        // Save prior occupant so we can restore it on backtrack —
        // matters when `next` was owned by a dead player.
        let prev_owner = state.at(&next);
        state.set_pos(current_player, &next);
        state.heads[cp_idx] = next;

        let next_player = next_alive_after(state, current_player);
        let (scores, _) = search(state, next_player, depth + 1);

        state.heads[cp_idx] = head;
        state.map[next.y as usize][next.x as usize] = prev_owner;

        if scores[cp_idx] > best_scores[cp_idx] {
            best_scores = scores;
            best_dir = Some(dir);
        }
    }

    if best_dir.is_some() {
        return (best_scores, best_dir);
    }

    // No legal move ⇒ this player dies.

    if current_player == state.my_id {
        // v1-style suicide short-circuit: my death is catastrophically
        // bad — skip exploring further. Magnitude matches v1's
        // `-2*W*H * (remaining_depth + 1)`, so dying near the root is
        // punished more than dying near a leaf.
        //
        // The mirrored +penalty on every alive opponent is required
        // because we're in max^n, not minimax: without it, the parent
        // opponent would read `scores[opp] = 0` here and prefer some
        // *other* branch where the heuristic returned a positive
        // cell-count — i.e., they'd actively avoid killing us.
        let mut s = [0i32; MAX_PLAYERS + 1];
        let penalty = (WIDTH * HEIGHT) as i32 * 2 * (MAX_DEPTH - depth + 1);
        s[cp_idx] = -penalty;
        for i in 1..=MAX_PLAYERS {
            if i != cp_idx && state.alive[i] {
                s[i] = penalty;
            }
        }
        return (s, None);
    }

    // Opponent dies — flip them out of the alive set and recurse.
    // No need to scrub the map: `is_pos_passable` already treats
    // cells owned by a dead player as passable.
    state.alive[cp_idx] = false;
    let scores = if state.is_game_over() {
        state.game_over_score(depth)
    } else {
        let next_player = next_alive_after(state, current_player);
        search(state, next_player, depth + 1).0
    };
    state.alive[cp_idx] = true;
    (scores, None)
}

// Tron has no per-match init payload (`InitialInput = ()`); this is
// a no-op kept for shape symmetry with init-shipping games like
// fantastic_bits.
pub fn on_init(_init: &InitialInput, _state: &mut GameState) {}

pub fn decide(turn: &TurnInput, state: &mut GameState) -> TurnOutput {
    state.n_players = turn.number_of_players as u8;
    state.my_id = ID::new((turn.player_number + 1) as u8).unwrap();

    // Rebuild alive/heads from this turn's input. The map persists
    // across turns so trails accumulate — we only ever see line.end
    // each turn, so old heads stay marked. Dead players' stale trail
    // cells are harmless: `is_pos_passable` keys on `alive[]`, which
    // we reset here.
    state.alive.fill(false);
    for (idx, line) in turn.player_lines.iter().enumerate() {
        if !is_valid_pos(&line.end) {
            continue;
        }
        let id = ID::new((idx + 1) as u8).unwrap();
        let slot = id.get() as usize;
        state.alive[slot] = true;
        state.heads[slot] = line.end;
        state.set_pos(id, &line.end);
        // Mark the spawn cell too — covers the first turn we see a
        // player, where line.start == line.end. Skip if already owned
        // (some later trail cell happens to coincide).
        if is_valid_pos(&line.start) && state.at(&line.start).is_none() {
            state.set_pos(id, &line.start);
        }
    }

    let (_, best_dir) = search(state, state.my_id, 0);

    // `best_dir` is `None` only when we had no legal moves at depth
    // 0. Pick an in-bounds direction so we still emit something the
    // engine accepts (and crash into a wall instead of forfeiting).
    let direction = best_dir.unwrap_or_else(|| {
        let my_head = state.heads[state.my_id.get() as usize];
        MOVES
            .iter()
            .find(|(_, mv)| is_valid_pos(&(my_head + mv)))
            .map(|(d, _)| *d)
            .unwrap_or(Direction::Down)
    });

    TurnOutput { direction }
}
