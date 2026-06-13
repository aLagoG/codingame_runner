use std::collections::VecDeque;
use std::num::NonZeroU8;

use tron_defs::{Direction, InitialInput, Pos, TurnInput, TurnOutput};

const WIDTH: usize = 30;
const HEIGHT: usize = 20;
const MAX_DEPTH: i32 = 5;
const MAX_PLAYERS: usize = 4;
const SLOTS: usize = MAX_PLAYERS + 1;

const MOVES: [(Direction, Pos); 4] = [
    (Direction::Up, Pos::new(0, -1)),
    (Direction::Right, Pos::new(1, 0)),
    (Direction::Down, Pos::new(0, 1)),
    (Direction::Left, Pos::new(-1, 0)),
];

type ID = NonZeroU8;
// IDs are 1..=MAX_PLAYERS — slot number = player_number + 1.

pub struct GameState {
    // map[y][x] — HEIGHT rows × WIDTH columns
    map: [[Option<ID>; WIDTH]; HEIGHT],
    n_players: u8,
    my_id: ID,
    // alive[id] / heads[id] for id in 1..=MAX_PLAYERS; index 0 unused.
    // heads[id] is only meaningful when alive[id].
    alive: [bool; SLOTS],
    heads: [Pos; SLOTS],
    heuristic_state: HeuristicState,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            map: [[None; WIDTH]; HEIGHT],
            n_players: 0,
            my_id: ID::new(1u8).unwrap(),
            alive: [false; SLOTS],
            heads: [Pos::new(0, 0); SLOTS],
            heuristic_state: HeuristicState::default(),
        }
    }
}

impl GameState {
    fn at(&self, pos: &Pos) -> Option<ID> {
        self.map[pos.y as usize][pos.x as usize]
    }

    fn at_mut(&mut self, pos: &Pos) -> &mut Option<ID> {
        &mut self.map[pos.y as usize][pos.x as usize]
    }

    fn set_pos(&mut self, id: ID, pos: &Pos) {
        *self.at_mut(pos) = Some(id);
    }

    // Passable if in-bounds AND (empty OR owned by a dead player).
    fn is_pos_passable(&self, pos: &Pos) -> bool {
        is_valid_pos(pos)
            && self
                .at(pos)
                .is_none_or(|owner| !self.alive[owner.get() as usize])
    }

    fn alive_ids(&self) -> impl Iterator<Item = ID> + '_ {
        (1..=MAX_PLAYERS as u8)
            .filter(move |&i| self.alive[i as usize])
            .map(|i| ID::new(i).unwrap())
    }

    fn alive_count(&self) -> usize {
        self.alive_ids().count()
    }

    fn is_game_over(&self) -> bool {
        self.alive_count() < 2
    }

    fn winner_id(&self) -> Option<ID> {
        let mut iter = self.alive_ids();
        let first = iter.next()?;
        iter.next().is_none().then_some(first)
    }

    // scores[id] for id in 1..=MAX_PLAYERS.
    fn game_over_score(&self, depth: i32) -> [i32; SLOTS] {
        // All players start scored as if they lost
        let mut res = [game_over_base_score(depth); SLOTS];
        if let Some(winner) = self.winner_id() {
            // Winner gets the score flipped
            res[winner.get() as usize] *= -1;
        }
        res
    }

    fn heuristic(&mut self) -> [i32; SLOTS] {
        self.heuristic_state.start_epoch();
        for i in 1..=MAX_PLAYERS {
            if self.alive[i] {
                self.heuristic_state
                    .queue
                    .push_back(HeuristicSearchNode::new(
                        ID::new(i as u8).unwrap(),
                        self.heads[i],
                        0,
                    ));
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
                    self.heuristic_state
                        .queue
                        .push_back(HeuristicSearchNode::new(
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
    scores: [i32; SLOTS],
    // Epoch counter; treats old marks in the board as empty if on different epoch
    epoch: u32,
    // Shared search queue, no need to re-allocate it every time
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
        self.queue.clear();
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
        let id_bit = 1u8 << id.get();
        let epoch = self.epoch;
        let node = &mut self.board[pos.y as usize][pos.x as usize];

        if node.epoch_last_set < epoch {
            // First visit this epoch — claim uniquely.
            node.epoch_last_set = epoch;
            node.distance = current_distance;
            node.owners = id_bit;
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
            let prev_id = node.owners.trailing_zeros() as usize;
            self.scores[prev_id] -= 1;
        }
        node.owners |= id_bit;

        // Still expand: cells beyond a contested junction may be
        // uniquely reachable through it for this player.
        true
    }
}

#[derive(Clone, Copy, Default)]
struct HeuristicNode {
    epoch_last_set: u32,
    distance: u32,
    // Bitmask: bit (i) set ⇒ player i reaches this cell at
    // `distance`. Cells with `owners.count_ones() == 1` are credited
    // in `scores`; cells with ≥2 bits set are neutral.
    owners: u8,
}

fn is_valid_pos(pos: &Pos) -> bool {
    (0..(WIDTH as i32)).contains(&pos.x) && (0..(HEIGHT as i32)).contains(&pos.y)
}

// Base score for a looser when the game is over (meaning it's negative)
fn game_over_base_score(depth: i32) -> i32 {
    (WIDTH * HEIGHT) as i32 * -2 * (MAX_DEPTH - depth + 1)
}

// Find the next alive player after `current`, wrapping. Callers only
// invoke this when `alive_count >= 2`, so a next alive always exists.
fn next_alive_after(state: &GameState, current: ID) -> ID {
    let n = state.n_players as usize;
    let idx = current.get() as usize - 1;
    for step in 1..n {
        let cand = ((idx + step) % n) + 1;
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
) -> ([i32; SLOTS], Option<Direction>) {
    if state.is_game_over() {
        return (state.game_over_score(depth), None);
    }
    if depth >= MAX_DEPTH {
        return (state.heuristic(), None);
    }

    let cp_idx = current_player.get() as usize;
    let head = state.heads[cp_idx];
    let mut best_scores = [i32::MIN; SLOTS];
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
        *state.at_mut(&next) = prev_owner;

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
        // our death is catastrophically bad — skip exploring further.
        let loss = game_over_base_score(depth);
        let mut s = [-loss; SLOTS];
        s[cp_idx] = loss;
        // We died, the direction doesn't matter
        return (s, Some(Direction::Up));
    }

    // Opponent dies — flip them out of the alive set and recurse.
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

    for (idx, line) in turn.player_lines.iter().enumerate() {
        let id = ID::new((idx + 1) as u8).unwrap();
        let slot = id.get() as usize;

        if !is_valid_pos(&line.end) {
            state.alive[slot] = false;
            continue;
        }

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

    // At depth 0 `search` always returns Some: the for-loop sets it
    // if any move is passable, and the suicide branch sets it as a
    // fallback otherwise. The terminal branches (game-over, depth
    // cap) can't fire at root.
    let direction = search(state, state.my_id, 0)
        .1
        .expect("search at root always returns a direction");

    TurnOutput { direction }
}
