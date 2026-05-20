# Replay Caching

When and how to cache materialized game state during visualization. Not
implemented — written down so future-us doesn't have to re-derive it.

## Background

`viz::run` keeps a single `V::Game` in memory and steps it to match the
currently-viewed tick:

- **Forward scrub** (and normal playback): O(Δticks) — incremental.
- **Backward scrub**: rebuild `G::new(seed)` and step from 0 → O(target tick).

This is the natural consequence of dropping `Game::snapshot()` in favor of
seed+outputs replays.

## Is this actually a problem?

Today, no. `TronGame::step` and `TicTacToeGame::step` are both microsecond-scale.
Rewinding 1000 ticks from scratch is ~1 ms — invisible at 60 fps. Until
profiling shows a hiccup during scrubbing, the right move is to do nothing.

It will become a problem when:

- A future game has expensive `step` (LLM inference, physics solve, neural net
  forward pass, …) where each call is milliseconds, **and**
- Replays are long enough that random scrubbing across the full timeline
  becomes painful.

## Options when it matters

All four options require `Game: Clone`. That's trivial to derive on
`TronGame`/`TicTacToeGame` (state is `Vec`s and `[Cell; 9]`). Worth flagging
because games whose state holds non-Cloneable resources (sockets, file handles
— unusual here) would need a wrapper.

### 1. Periodic checkpoints — recommended

Clone the game every `K` ticks into a `Vec<G>`. Backward scrub: find the
nearest checkpoint ≤ target, clone it, step forward.

```rust
struct CheckpointCache<G> {
    seed: u64,
    num_players: u32,
    stride: usize,            // K
    checkpoints: Vec<G>,      // checkpoints[i] = state at tick i * stride
}

impl<G: Game + Clone> CheckpointCache<G> {
    fn build(replay: &Replay<G>, stride: usize) -> Self {
        let mut game = G::new(replay.num_players, replay.seed);
        let mut checkpoints = vec![game.clone()];
        for (i, outputs) in replay.outputs.iter().enumerate() {
            let _ = game.step(outputs);
            if (i + 1) % stride == 0 {
                checkpoints.push(game.clone());
            }
        }
        Self {
            seed: replay.seed,
            num_players: replay.num_players,
            stride,
            checkpoints,
        }
    }

    fn restore(&self, replay: &Replay<G>, target: usize) -> G {
        let cp_idx = target / self.stride;
        let mut game = self.checkpoints[cp_idx].clone();
        let cp_tick = cp_idx * self.stride;
        for outputs in &replay.outputs[cp_tick..target] {
            let _ = game.step(outputs);
        }
        game
    }
}
```

- **Memory**: `O(n_ticks / K × sizeof(G))`. For a 1000-tick game with K=50,
  that's 20 cloned games.
- **Scrub cost**: ≤ K steps from the nearest checkpoint.
- **Tunable**: `K=50` is a sensible default. Lower K trades memory for speed.

Wire-in to `viz::run`: replace the bare `V::Game` instance with an
`Option<CheckpointCache<V::Game>>`; on scrub, restore from cache; on
forward-step, keep using incremental `step`.

### 2. Full upfront materialization — simple, heavy for trail games

```rust
let states: Vec<G> = materialize_all(&replay);  // requires Game: Clone
// scrub is O(1) — states[target_tick]
```

- **Memory**: `O(n_ticks × sizeof(G))`. For Tron, where trails grow by one
  cell per tick per player, individual game size is O(T) at tick T, so the
  whole vector is `O(T²)`. A 1000-tick 4-player game is ~16 MB of cached
  trails. Painful.
- **Scrub cost**: O(1).
- Fine for small-state games (tic-tac-toe is `[Cell; 9]` — negligible),
  bad for any game where state grows with tick count.

### 3. On-demand LRU

Keep the N most-recently-restored states in a small LRU. On scrub, if `target`
is in the cache use it directly, else find the nearest cached state ≤ target,
step forward.

- **Memory**: O(N × sizeof(G)).
- **Scrub cost**: O(distance to nearest cached) — bounded *in practice* by
  user behavior (scrubbing usually stays in one neighborhood) but unbounded
  in the worst case.
- More complex than checkpoints. Doesn't beat checkpoints unless access
  patterns are highly local.

### 4. Do nothing (current state)

- **Memory**: O(1).
- **Scrub cost**: O(T) on backward, O(1) on forward.
- Right answer until profiling proves otherwise.

## Comparison

| Strategy | Memory | Worst-case scrub | Game: Clone | Complexity |
|---|---|---|---|---|
| None (today) | O(1) | O(T) | ❌ | none |
| Checkpoints every K | O(T/K) clones | O(K) | ✅ | small |
| Full materialize | O(T) clones — `O(T²)` total for Tron | O(1) | ✅ | tiny |
| LRU of N | O(N) clones | O(T) worst | ✅ | medium |

## When to act

1. Add a frame-time HUD to `viz::run` (track `get_frame_time()` over a moving
   window, draw "5 ms / 200 fps" in the corner) so regressions show up
   immediately.
2. If scrubbing drops below ~30 fps on a real replay, switch to checkpoints
   with K=50. Don't bother with the other options unless the profile says so.
3. Skip full materialization for any game whose state grows with tick count
   (anything trail-based, history-based, append-only-log-based).
