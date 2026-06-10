use std::collections::VecDeque;
use std::num::NonZeroU8;

use tron_defs::{Direction, InitialInput, Pos, TurnInput, TurnOutput};

const WIDTH: usize = 30;
const HEIGHT: usize = 20;

const MAX_DEPTH: i32 = 5;
const MOVES: [(Direction, Pos); 4] = [
    (Direction::Down, Pos::new(0, 1)),
    (Direction::Right, Pos::new(1, 0)),
    (Direction::Up, Pos::new(0, -1)),
    (Direction::Left, Pos::new(-1, 0)),
];
const MAX_PLAYERS: usize = 4;

type ID = NonZeroU8;
// IDs are 1..=4 -> the player's slot number (player_number + 1).

pub struct GameState {
    // map[y][x] — outer Vec is HEIGHT rows of WIDTH columns, matching
    // the convention used by the engine in games/tron/game/src/lib.rs.
    pub map: [[Option<ID>; WIDTH]; HEIGHT],
    pub n_players: u8,
    pub my_id: ID,
    // alive[id] for id in 1..=MAX_PLAYERS. Index 0 is unused.
    pub alive: [bool; MAX_PLAYERS + 1],
    // Current head per player. heads[id] only meaningful when alive[id].
    pub heads: [Pos; MAX_PLAYERS + 1],
    pub heuristic_count: u32,
    pub heuristic_state: HeuristicState,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            map: [[None; WIDTH]; HEIGHT],
            n_players: 0,
            my_id: ID::new(1u8).unwrap(),
            alive: [false; MAX_PLAYERS + 1],
            heads: [Pos::new(0, 0); MAX_PLAYERS + 1],
            heuristic_count: 0,
            heuristic_state: HeuristicState::default(),
        }
    }
}

impl GameState {
    pub fn at_mut(&mut self, pos: &Pos) -> &mut Option<ID> {
        &mut self.map[pos.y as usize][pos.x as usize]
    }

    pub fn at(&self, pos: &Pos) -> Option<ID> {
        self.map[pos.y as usize][pos.x as usize]
    }

    // Passable if in-bounds AND (empty OR owned by a dead player —
    // dead trails are erased by the engine, so they're free to walk on).
    pub fn is_pos_passable(&self, pos: &Pos) -> bool {
        is_valid_pos(pos)
            && self
                .at(pos)
                .is_none_or(|owner| !self.alive[owner.get() as usize])
    }

    pub fn set_pos(&mut self, id: ID, pos: &Pos) {
        *self.at_mut(pos) = Some(id);
    }

    pub fn clear_pos(&mut self, pos: &Pos) {
        *self.at_mut(pos) = None;
    }

    pub fn alive_count(&self) -> usize {
        (1..=MAX_PLAYERS).filter(|&i| self.alive[i]).count()
    }

    pub fn is_game_over(&self) -> bool {
        self.alive_count() < 2
    }

    pub fn winner_id(&self) -> Option<ID> {
        if self.alive_count() == 1 {
            (1..=MAX_PLAYERS as u8)
                .find(|&i| self.alive[i as usize])
                .and_then(NonZeroU8::new)
        } else {
            None
        }
    }

    // scores[id] for id in 1..=MAX_PLAYERS. Winning sooner ⇒ higher
    // score; dying later ⇒ less-negative penalty.
    pub fn game_over_score(&self, depth: i32) -> [i32; MAX_PLAYERS + 1] {
        let mut res = [0i32; MAX_PLAYERS + 1];
        let winner = self.winner_id();
        for i in 1..res.len() {
            if winner.is_some_and(|p| p.get() as usize == i) {
                res[i] = (WIDTH * HEIGHT) as i32 * 2 - depth;
            } else {
                res[i] = -((WIDTH * HEIGHT) as i32) * 2 + depth;
            }
        }
        res
    }

    pub fn heuristic(&mut self) -> [i32; MAX_PLAYERS + 1] {
        self.heuristic_count += 1;
        self.heuristic_state.scores.fill(0);

        let mut queue: VecDeque<HeuristicSearchNode> = VecDeque::new();
        for i in 1..=MAX_PLAYERS {
            if self.alive[i] {
                let id = ID::new(i as u8).unwrap();
                queue.push_back(HeuristicSearchNode::new(id, self.heads[i], 0));
            }
        }

        while let Some(current) = queue.pop_front() {
            for (_, mv) in &MOVES {
                let moved = current.position + mv;
                if self.is_pos_passable(&moved)
                    && self.heuristic_state.set(
                        &moved,
                        current.player_id,
                        current.distance + 1,
                        self.heuristic_count,
                    )
                {
                    queue.push_back(HeuristicSearchNode::new(
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
    pub player_id: ID,
    pub position: Pos,
    pub distance: u32,
}

impl HeuristicSearchNode {
    pub fn new(player_id: ID, position: Pos, distance: u32) -> Self {
        Self {
            player_id,
            position,
            distance,
        }
    }
}

pub struct HeuristicState {
    pub board: [[HeuristicNode; WIDTH]; HEIGHT],
    // How many squares each player controls in the current heuristic call.
    pub scores: [i32; MAX_PLAYERS + 1],
}

impl Default for HeuristicState {
    fn default() -> Self {
        Self {
            board: Default::default(),
            scores: Default::default(),
        }
    }
}

impl HeuristicState {
    pub fn at_mut(&mut self, pos: &Pos) -> &mut HeuristicNode {
        &mut self.board[pos.y as usize][pos.x as usize]
    }

    pub fn at(&self, pos: &Pos) -> &HeuristicNode {
        &self.board[pos.y as usize][pos.x as usize]
    }

    // Returns true iff this call changed who owns the cell — caller
    // uses that to decide whether to enqueue the neighbour for further
    // BFS expansion. The `epoch_last_set` field lets us treat the
    // board as freshly zeroed every heuristic call without actually
    // zeroing it.
    pub fn set(&mut self, pos: &Pos, id: ID, current_distance: u32, heuristic_count: u32) -> bool {
        let node = self.at(pos);
        let prev_best = node.best;
        let prev_distance = node.distance;
        let prev_epoch = node.epoch_last_set;

        let first_visit_this_epoch = prev_epoch < heuristic_count;
        let mut update = first_visit_this_epoch;

        if !update && current_distance < prev_distance {
            // Shouldn't happen in correct multi-source BFS (cells are
            // visited in non-decreasing distance order), but keep the
            // arm for robustness.
            update = true;
            if let Some(prev) = prev_best {
                self.scores[prev.get() as usize] -= 1;
            }
        }

        if update {
            self.scores[id.get() as usize] += 1;
            let node = self.at_mut(pos);
            node.distance = current_distance;
            node.best = Some(id);
            node.epoch_last_set = heuristic_count;
        }

        update
    }
}

pub struct HeuristicNode {
    pub epoch_last_set: u32,
    pub distance: u32,
    pub best: Option<ID>,
}

impl Default for HeuristicNode {
    fn default() -> Self {
        Self {
            epoch_last_set: 0,
            distance: 0,
            best: None,
        }
    }
}

fn is_valid_pos(pos: &Pos) -> bool {
    (0..(WIDTH as i32)).contains(&pos.x) && (0..(HEIGHT as i32)).contains(&pos.y)
}

// Find the next alive player after `current`, wrapping around the
// 1..=n_players range. None only if literally nobody else is alive.
fn next_alive_after(state: &GameState, current: ID) -> Option<ID> {
    let n = state.n_players as usize;
    if n == 0 {
        return None;
    }
    for step in 1..=n {
        let cand = ((current.get() as usize - 1 + step) % n) + 1;
        if state.alive[cand] {
            return ID::new(cand as u8);
        }
    }
    None
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

        let next_player = next_alive_after(state, current_player)
            .expect("alive_count >= 2 ⇒ another alive player exists");
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
        // v1-style suicide short-circuit: my death is catastrophically
        // bad — skip exploring further. Magnitude matches v1's
        // `-2*W*H * (remaining_depth + 1)`, so dying near the root is
        // punished more than dying near a leaf (the bot prefers to
        // postpone death when death is inevitable).
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

    // Opponent dies — erase their trail (the engine erases dead
    // players' ribbons) so the rest of the search sees a passable
    // board, recurse to the next player, then restore on backtrack.
    state.alive[cp_idx] = false;
    let mut erased: Vec<Pos> = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if state.map[y][x] == Some(current_player) {
                state.map[y][x] = None;
                erased.push(Pos::new(x as i32, y as i32));
            }
        }
    }

    let scores = if state.is_game_over() {
        state.game_over_score(depth)
    } else {
        let next_player = next_alive_after(state, current_player)
            .expect("alive_count >= 2 after death ⇒ someone else alive");
        search(state, next_player, depth + 1).0
    };

    for p in &erased {
        state.map[p.y as usize][p.x as usize] = Some(current_player);
    }
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
    // across turns so trails accumulate (we only ever see line.end
    // each turn — old heads stay marked).
    for i in 0..state.alive.len() {
        state.alive[i] = false;
    }
    for (idx, line) in turn.player_lines.iter().enumerate() {
        let id = (idx + 1) as u8;
        let id_nz = ID::new(id).unwrap();
        if is_valid_pos(&line.end) {
            state.alive[id as usize] = true;
            state.heads[id as usize] = line.end;
            state.set_pos(id_nz, &line.end);
            // Mark the spawn cell too — covers the first turn we see
            // a player, where line.start == line.end.
            if is_valid_pos(&line.start) {
                let prev = state.at(&line.start);
                if prev.is_none() {
                    state.set_pos(id_nz, &line.start);
                }
            }
        } else {
            // Engine sends (-1,-1) for dead players. Clear any trail
            // cells they still own — the engine has erased them.
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    if state.map[y][x] == Some(id_nz) {
                        state.map[y][x] = None;
                    }
                }
            }
        }
    }

    let my_head = state.heads[state.my_id.get() as usize];
    let (_, best_dir) = search(state, state.my_id, 0);

    // `best_dir` is `None` only when we have no legal moves — pick
    // an in-bounds direction so we still emit something the engine
    // accepts (and crash into a wall instead of forfeiting silently).
    let direction = best_dir.unwrap_or_else(|| {
        MOVES
            .iter()
            .find(|(_, mv)| is_valid_pos(&(my_head + mv)))
            .map(|(d, _)| *d)
            .unwrap_or(Direction::Down)
    });

    TurnOutput { direction }
}
