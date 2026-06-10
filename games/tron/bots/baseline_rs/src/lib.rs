use std::{collections::VecDeque, i32, num::NonZeroU8, os::macos::raw::stat};

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
// IDs are 1..=4 -> the index of the player +1

pub struct GameState {
    pub map: [[Option<ID>; WIDTH]; HEIGHT],
    pub n_players: u8,
    pub first: bool,
    pub heuristic_count: u32,
    pub dead_players: [bool; MAX_PLAYERS + 1],
    pub my_id: ID,
    pub first_turn: bool,
    pub players: Vec<Player>,
    pub all_players: [Option<Player>; MAX_PLAYERS + 1],
    pub heuristic_state: HeuristicState,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            map: Default::default(),
            n_players: Default::default(),
            first: Default::default(),
            heuristic_count: Default::default(),
            dead_players: Default::default(),
            my_id: ID::new(1u8).unwrap(),
            first_turn: false,
            players: Default::default(),
            all_players: Default::default(),
            heuristic_state: Default::default(),
        }
    }
}

impl GameState {
    pub fn at_mut(&mut self, pos: &Pos) -> &mut Option<ID> {
        &mut self.map[pos.x as usize][pos.y as usize]
    }

    pub fn at(&self, pos: &Pos) -> &Option<ID> {
        &self.map[pos.x as usize][pos.y as usize]
    }

    pub fn is_pos_empty(&self, pos: &Pos) -> bool {
        is_valid_pos(pos)
            && self
                .at(pos)
                .is_none_or(|v| self.dead_players[v.get() as usize])
    }

    pub fn set_pos(&mut self, id: ID, pos: &Pos) -> Option<ID> {
        let tmp = *self.at(pos);
        *self.at_mut(pos) = Some(id);
        tmp
    }
    // Could replace this, it's too similar to set_pos
    pub fn clear_pos_with_id(&mut self, id: ID, pos: &Pos) {
        *self.at_mut(pos) = Some(id);
    }

    pub fn clear_pos(&mut self, pos: &Pos) {
        *self.at_mut(pos) = None;
    }

    pub fn is_game_over(&self) -> bool {
        self.players.iter().map(|p| p.can_move() as u8).sum::<u8>() < 2u8
    }

    pub fn winner(&self) -> Option<&Player> {
        self.players.iter().find(|p| p.can_move())
    }

    // Gives you the score for a given player
    pub fn game_over_score(&self, depth: i32) -> [i32; MAX_PLAYERS + 1] {
        let mut res = [0i32; MAX_PLAYERS + 1];
        let winner = self.winner();
        for i in 1..res.len() {
            if winner.is_some_and(|p| p.id.get() == i as u8) {
                // Winning faster is better, so the deeper you are, the smaller the score
                res[i] = (WIDTH * HEIGHT) as i32 * 2 - depth;
            } else {
                // Loosing later is better so the deeper you are the more lesser the penalty
                res[i] = (WIDTH * HEIGHT) as i32 * -2 + depth;
            }
        }

        res
    }

    pub fn heuristic(&mut self) -> [i32; MAX_PLAYERS + 1] {
        self.heuristic_count += 1;
        self.heuristic_state.scores.fill(0);

        let mut queue: VecDeque<HeuristicSearchNode> = VecDeque::new();
        for player in &self.players {
            queue.push_back(HeuristicSearchNode::new(player.id, player.head, 0));
        }

        while !queue.is_empty() {
            let current = queue.pop_front().unwrap();

            for (_, mv) in &MOVES {
                let moved = current.position + mv;
                // IF the postion is empty, and it actually chagned the board
                if self.is_pos_empty(&moved)
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

#[derive(Clone, Copy)]
pub struct Player {
    pub id: ID,
    pub head: Pos,
}

impl Player {
    pub fn can_move(&self) -> bool {
        MOVES
            .iter()
            .map(|(_, mv)| self.head + mv)
            .any(|pos| is_valid_pos(&pos))
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
    // How many squares does each player control
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
        &mut self.board[pos.x as usize][pos.y as usize]
    }

    pub fn at(&self, pos: &Pos) -> &HeuristicNode {
        &self.board[pos.x as usize][pos.y as usize]
    }

    pub fn set(&mut self, pos: &Pos, id: ID, current_distance: u32, heuristic_count: u32) -> bool {
        let mut update = false;

        let HeuristicNode {
            distance,
            best,
            epoch_last_set,
        } = self.at(pos);
        let best = *best;
        let distance = *distance;
        let epoch_last_set = *epoch_last_set;

        if epoch_last_set < heuristic_count {
            self.at_mut(pos).epoch_last_set = heuristic_count;
            // Will update because it's a new round
            update = true;
        }

        // TODO: we could mark ties somehow when the distance is the same. Maybe mark it as owned by no one?
        if !update && current_distance < distance {
            // Will update because the new player is closer
            update = true;
            self.scores[best
                .expect("Can't have a distance if best is not set")
                .get() as usize] -= 1;
        }

        if update {
            self.scores[id.get() as usize] += 1;
            let node = self.at_mut(pos);
            node.distance = current_distance;
            node.best = Some(id)
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

fn get_next_player(state: &GameState, current_player: ID) -> ID {
    let mut idx = current_player.get() as usize % state.players.len();
    loop {
        let next_player = &state.players[idx];
        if !state.dead_players[next_player.id.get() as usize] {
            return next_player.id;
        }
        idx = (idx + 1) % state.players.len();
    }
}

fn search(state: &mut GameState, player_id: ID, pos: Pos, depth: i32) -> [i32; MAX_PLAYERS + 1] {
    if state.is_game_over() {
        return state.game_over_score(depth);
    }

    if depth == MAX_DEPTH {
        return state.heuristic();
    }

    // Do the actual search
    let mut best_scores = [i32::MIN; MAX_PLAYERS + 1];
    let next_player = get_next_player(state, player_id);
    for (_, mv) in MOVES {
        let next = mv + pos;
        if !state.is_pos_empty(&next) {
            continue;
        }

        // For the next alive player we probably want to use the all players thing and go round and round.
        // Otherwise if we just use players, killing them mid way woudl be weird. But actually we could just use the alive ones and still check dead for the middle of the search deadness.
        // set the value, search, return it, then do A/B
        state.set_pos(next_player, &next);
        let scores = search(state, next_player, next, depth + 1);
        state.clear_pos(&next);
        //TODO: It's missing current, gotta check on that
        if scores[next_player.get() as usize] > best_scores[next_player.get() as usize] {
            best_scores = scores;
        }
    }

    best_scores
}

// Tron has no per-match init payload (`InitialInput = ()`); this is
// a no-op kept for shape symmetry with init-shipping games like
// fantastic_bits.
pub fn on_init(_init: &InitialInput, _state: &mut GameState) {}

pub fn decide(turn: &TurnInput, state: &mut GameState) -> TurnOutput {
    eprintln!(
        "players={} me={} lines={}",
        turn.number_of_players,
        turn.player_number,
        turn.player_lines.len()
    );
    state.n_players = turn.number_of_players as u8;
    state.my_id = ID::new((turn.player_number + 1) as u8).unwrap();

    for (idx, line) in turn.player_lines.iter().enumerate() {
        let id = idx + 1;
        let real_id = ID::new(id as u8).unwrap();
        let player = Player {
            id: real_id,
            head: line.end,
        };
        if is_valid_pos(&line.end) {
            state.set_pos(real_id, &line.end);
            state.players.push(player);
        } else {
            state.dead_players[id] = true;
        }
        state.all_players[id] = Some(player);
    }

    let mut best_score = i32::MIN;
    let mut best_move = Direction::Down;
    let my_pos = state.all_players[(state.my_id.get() + 1) as usize]
        .expect("We should be alive if we are getting called for a turn")
        .head;
    for (dir, mv) in &MOVES {
        let scores = search(state, state.my_id, my_pos + mv, 0);
        if let score = scores[state.my_id.get() as usize]
            && score > best_score
        {
            best_move = *dir;
            best_score = score;
        }
    }

    state.players.clear();
    TurnOutput {
        direction: best_move,
    }
}
