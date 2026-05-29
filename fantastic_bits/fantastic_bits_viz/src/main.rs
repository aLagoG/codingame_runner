use macroquad::prelude::*;
use fantastic_bits_defs::TurnOutput;
use fantastic_bits_game::FantasticBitsGame;
use viz::{CellGrid, Replay, Visualize};

struct FantasticBitsViz;

impl Visualize for FantasticBitsViz {
    type Game = FantasticBitsGame;

    fn grid_size() -> (u32, u32) {
        // TODO: return your board size in cells.
        (10, 10)
    }

    fn draw(_game: &FantasticBitsGame, grid: &CellGrid) {
        grid.draw_grid_lines(Color::new(0.2, 0.2, 0.3, 1.0), 1.0);
        // TODO: render game state from `_game`.
    }
}

/// Shown when the viz binary is launched with no replay path argument.
fn demo_replay() -> Replay<TurnOutput> {
    Replay {
        seed: 0,
        num_players: 1,
        outputs: vec![],
    }
}

viz::run_viz!(FantasticBitsViz, demo_replay());
