use common::engine::{FfiGame, Game, NoInitialInput, PlayerId};
use tictactoe_defs::{BOARD_CELLS, BOARD_SIZE, Cell, Pos, TurnInput, TurnOutput};

pub struct TicTacToeGame {
    board: [Cell; BOARD_CELLS],
    next_player: PlayerId,
    active: Vec<PlayerId>,
    last_move: Option<(PlayerId, Pos)>,
    outcome: Option<TicTacToeOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicTacToeOutcome {
    /// `Some(p)` if `p` won, `None` for a draw.
    pub winner: Option<PlayerId>,
}

impl TicTacToeGame {
    pub fn board(&self) -> &[Cell; BOARD_CELLS] {
        &self.board
    }
    pub fn last_move(&self) -> Option<(PlayerId, Pos)> {
        self.last_move
    }
    pub fn outcome(&self) -> Option<&TicTacToeOutcome> {
        self.outcome.as_ref()
    }
    /// Whose move comes next, or `None` after the game has ended.
    pub fn next_player(&self) -> Option<PlayerId> {
        if self.outcome.is_some() {
            None
        } else {
            Some(self.next_player)
        }
    }
}

impl Game for TicTacToeGame {
    const NAME: &'static str = "tictactoe";

    type InitialInput = NoInitialInput;
    type Input = TurnInput;
    type Output = TurnOutput;
    type Outcome = TicTacToeOutcome;

    fn new(num_players: u32, _seed: u64) -> Self {
        assert!(
            num_players == 2,
            "TicTacToeGame is a 2-player game, got {num_players}"
        );
        TicTacToeGame {
            board: [Cell::Empty; BOARD_CELLS],
            next_player: 0,
            active: vec![0],
            last_move: None,
            outcome: None,
        }
    }

    fn initial_input(&self, _player: PlayerId) -> NoInitialInput {
        NoInitialInput::default()
    }

    fn input_for(&self, player: PlayerId) -> TurnInput {
        TurnInput {
            player_number: player as i32,
            board: self.board,
        }
    }

    fn step(&mut self, outputs: &[Option<TurnOutput>]) -> Option<TicTacToeOutcome> {
        let p = self.next_player;
        let opponent = 1 - p;

        // Missing or invalid output → current player loses.
        let Some(out) = outputs[p as usize].as_ref() else {
            return Some(self.finish(Some(opponent)));
        };

        if !in_bounds(out.pos) {
            return Some(self.finish(Some(opponent)));
        }
        let idx = (out.pos.row as usize) * BOARD_SIZE + (out.pos.col as usize);
        if self.board[idx] != Cell::Empty {
            return Some(self.finish(Some(opponent)));
        }

        self.board[idx] = Cell::for_player(p);
        self.last_move = Some((p, out.pos));

        if let Some(winner) = check_winner(&self.board) {
            return Some(self.finish(Some(winner)));
        }
        if self.board.iter().all(|c| *c != Cell::Empty) {
            return Some(self.finish(None));
        }

        self.next_player = opponent;
        self.active = vec![self.next_player];
        None
    }

    fn active_players(&self) -> &[PlayerId] {
        &self.active
    }
}

impl TicTacToeGame {
    fn finish(&mut self, winner: Option<PlayerId>) -> TicTacToeOutcome {
        let outcome = TicTacToeOutcome { winner };
        self.outcome = Some(outcome.clone());
        self.active.clear();
        outcome
    }
}

fn in_bounds(p: Pos) -> bool {
    p.row >= 0 && p.row < BOARD_SIZE as i32 && p.col >= 0 && p.col < BOARD_SIZE as i32
}

const LINES: [[usize; 3]; 8] = [
    [0, 1, 2],
    [3, 4, 5],
    [6, 7, 8],
    [0, 3, 6],
    [1, 4, 7],
    [2, 5, 8],
    [0, 4, 8],
    [2, 4, 6],
];

fn check_winner(board: &[Cell; BOARD_CELLS]) -> Option<PlayerId> {
    for line in LINES.iter() {
        let a = board[line[0]];
        if a != Cell::Empty && a == board[line[1]] && a == board[line[2]] {
            return Some(match a {
                Cell::X => 0,
                Cell::O => 1,
                Cell::Empty => unreachable!(),
            });
        }
    }
    None
}

// Plugin glue: marks TicTacToeGame as FFI-playable and points at the
// `_defs` crate's Ffi marker.
impl FfiGame for TicTacToeGame {
    type Defs = tictactoe_defs::Ffi;
}

#[cfg(test)]
mod test {
    use super::*;

    fn play(game: &mut TicTacToeGame, player: PlayerId, row: i32, col: i32) -> Option<TicTacToeOutcome> {
        let mut outputs: Vec<Option<TurnOutput>> = vec![None, None];
        outputs[player as usize] = Some(TurnOutput {
            pos: Pos { row, col },
        });
        game.step(&outputs)
    }

    #[test]
    fn x_wins_top_row() {
        let mut game = TicTacToeGame::new(2, 0);
        assert!(play(&mut game, 0, 0, 0).is_none());
        assert!(play(&mut game, 1, 1, 0).is_none());
        assert!(play(&mut game, 0, 0, 1).is_none());
        assert!(play(&mut game, 1, 1, 1).is_none());
        let outcome = play(&mut game, 0, 0, 2);
        assert!(outcome == Some(TicTacToeOutcome { winner: Some(0) }));
    }

    #[test]
    fn draw_when_board_full() {
        let mut game = TicTacToeGame::new(2, 0);
        // X O X
        // X O O
        // O X X
        let moves = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 0), (1, 2), (2, 1), (2, 0), (2, 2)];
        let mut outcome = None;
        for (i, &(r, c)) in moves.iter().enumerate() {
            outcome = play(&mut game, (i % 2) as PlayerId, r, c);
        }
        assert!(outcome == Some(TicTacToeOutcome { winner: None }));
    }

    #[test]
    fn invalid_move_loses() {
        let mut game = TicTacToeGame::new(2, 0);
        // Player 0 picks an out-of-bounds cell → player 1 wins.
        let outcome = play(&mut game, 0, 5, 5);
        assert!(outcome == Some(TicTacToeOutcome { winner: Some(1) }));
    }

    #[test]
    fn occupied_cell_loses() {
        let mut game = TicTacToeGame::new(2, 0);
        play(&mut game, 0, 1, 1);
        let outcome = play(&mut game, 1, 1, 1);
        assert!(outcome == Some(TicTacToeOutcome { winner: Some(0) }));
    }
}
