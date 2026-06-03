// Tron bot strategy — v2.
//
// Port of games/tron/bots/baseline_cpp/v2/tron.cpp onto the workspace's
// bot template. Strategy is intact line-for-line; the only differences
// from the snapshot are:
//   * Wrapped in `namespace tron_v2_cpp` so the bin doesn't ODR-collide
//     with v1's identically-named functions if both ever link together.
//   * `on_init` (called once per match) builds the Zobrist tables.
//   * `decide(TurnInput)` replaces the snapshot's `main()` per-tick body —
//     same wire format, but called by `main.cpp` once per tick instead
//     of looping on stdin itself.
//
// See the snapshot's prologue for the design notes (TT, move ordering,
// isolation detection, terminal-score scaling).

#pragma once

#include "../../defs/include/tron_defs_io.h"

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <queue>
#include <random>
#include <vector>

namespace tron_v2_cpp {

using std::array;
using std::chrono::milliseconds;
using std::chrono::steady_clock;

// ============================================================
//  Constants
// ============================================================

constexpr int WIDTH = 30;
constexpr int HEIGHT = 20;
constexpr int CELLS = WIDTH * HEIGHT;
constexpr int MAX_PLAYERS = 4;

constexpr auto TURN_BUDGET = milliseconds(90);
constexpr int MAX_DEPTH = 100;
constexpr int INF = 1'000'000'000;

// ============================================================
//  Board + players
// ============================================================

inline uint8_t board[CELLS]{};

struct Player {
    int id = 0;
    int x = 0, y = 0;
    bool dead = true;
};

inline Player players[MAX_PLAYERS + 1];
inline int my_id = 0;
inline int num_players = 0;
inline bool first_tick = true;

inline int cell_idx(int x, int y) { return y * WIDTH + x; }
inline bool in_bounds(int x, int y) { return x >= 0 && x < WIDTH && y >= 0 && y < HEIGHT; }

inline bool passable(int x, int y) {
    if (!in_bounds(x, y)) return false;
    uint8_t c = board[cell_idx(x, y)];
    return c == 0 || players[c].dead;
}

struct Move { int dx, dy; const char* name; Direction dir; };
inline constexpr Move MOVES[4] = {
    {0, -1, "UP",    Direction::Up},
    {1,  0, "RIGHT", Direction::Right},
    {0,  1, "DOWN",  Direction::Down},
    {-1, 0, "LEFT",  Direction::Left},
};

// ============================================================
//  Zobrist hash + transposition table
// ============================================================

inline uint64_t z_cell[CELLS][MAX_PLAYERS + 1];
inline uint64_t z_side[MAX_PLAYERS + 1];
inline uint64_t z_dead[MAX_PLAYERS + 1];
inline uint64_t cur_hash = 0;

inline void init_zobrist() {
    std::mt19937_64 rng(0xC0FFEEC0DE5EEDULL);
    for (int i = 0; i < CELLS; ++i)
        for (int p = 0; p <= MAX_PLAYERS; ++p)
            z_cell[i][p] = rng();
    for (int p = 0; p <= MAX_PLAYERS; ++p) {
        z_side[p] = rng();
        z_dead[p] = rng();
    }
}

inline void hash_xor_cell(int idx, uint8_t id) { cur_hash ^= z_cell[idx][id]; }
inline void hash_xor_side(int p) { cur_hash ^= z_side[p]; }
inline void hash_xor_dead(int p) { cur_hash ^= z_dead[p]; }

inline void recompute_hash(int side_to_move) {
    cur_hash = 0;
    for (int i = 0; i < CELLS; ++i)
        if (board[i] != 0)
            cur_hash ^= z_cell[i][board[i]];
    for (int p = 1; p <= MAX_PLAYERS; ++p)
        if (players[p].id != 0 && players[p].dead)
            cur_hash ^= z_dead[p];
    cur_hash ^= z_side[side_to_move];
}

enum TTFlag : uint8_t { TT_EXACT = 0, TT_LOWER = 1, TT_UPPER = 2 };

struct TTEntry {
    uint64_t hash = 0;
    int32_t value = 0;
    int8_t depth = -1;
    TTFlag flag = TT_EXACT;
};

constexpr int TT_BITS = 17;
constexpr int TT_SIZE = 1 << TT_BITS;
constexpr int TT_MASK = TT_SIZE - 1;

inline TTEntry tt[TT_SIZE];

inline void tt_clear() {
    for (auto& e : tt) e.depth = -1;
}

inline bool tt_probe(uint64_t h, int depth, int alpha, int beta, int& out) {
    const TTEntry& e = tt[h & TT_MASK];
    if (e.depth < depth || e.hash != h) return false;
    switch (e.flag) {
        case TT_EXACT: out = e.value; return true;
        case TT_LOWER: if (e.value >= beta) { out = e.value; return true; } break;
        case TT_UPPER: if (e.value <= alpha) { out = e.value; return true; } break;
    }
    return false;
}

inline void tt_store(uint64_t h, int depth, int value, TTFlag flag) {
    TTEntry& e = tt[h & TT_MASK];
    if (e.depth <= depth) {
        e.hash = h;
        e.depth = static_cast<int8_t>(depth);
        e.value = value;
        e.flag = flag;
    }
}

// ============================================================
//  Search-time mutation helpers
// ============================================================

inline uint8_t mark_cell(int x, int y, uint8_t id) {
    int i = cell_idx(x, y);
    uint8_t prev = board[i];
    if (prev != 0) hash_xor_cell(i, prev);
    board[i] = id;
    hash_xor_cell(i, id);
    return prev;
}

inline void unmark_cell(int x, int y, uint8_t prev) {
    int i = cell_idx(x, y);
    hash_xor_cell(i, board[i]);
    board[i] = prev;
    if (prev != 0) hash_xor_cell(i, prev);
}

// ============================================================
//  Player iteration
// ============================================================

inline int next_live(int p) {
    for (int step = 1; step <= MAX_PLAYERS; ++step) {
        int q = ((p - 1 + step) % MAX_PLAYERS) + 1;
        if (players[q].id != 0 && !players[q].dead) return q;
    }
    return p;
}

inline int count_live() {
    int n = 0;
    for (int p = 1; p <= MAX_PLAYERS; ++p)
        if (players[p].id != 0 && !players[p].dead) ++n;
    return n;
}

// ============================================================
//  Leaf heuristic — voronoi by multi-source BFS
// ============================================================

struct BFSItem { uint16_t idx; uint8_t owner; uint16_t dist; };

inline int leaf_heuristic() {
    static uint8_t owner[CELLS];
    static uint16_t dist[CELLS];
    static std::vector<BFSItem> q;
    if (q.capacity() < CELLS) q.reserve(CELLS);
    q.clear();
    size_t head = 0;

    std::memset(owner, 0, sizeof(owner));

    int counts[MAX_PLAYERS + 1] = {};
    bool in_contact[MAX_PLAYERS + 1] = {};

    for (int p = 1; p <= MAX_PLAYERS; ++p) {
        if (players[p].id == 0 || players[p].dead) continue;
        int i = cell_idx(players[p].x, players[p].y);
        owner[i] = static_cast<uint8_t>(p);
        dist[i] = 0;
        counts[p]++;
        q.push_back({static_cast<uint16_t>(i), static_cast<uint8_t>(p), 0});
    }

    while (head < q.size()) {
        BFSItem cur = q[head++];
        int x = cur.idx % WIDTH;
        int y = cur.idx / WIDTH;
        for (auto& m : MOVES) {
            int nx = x + m.dx, ny = y + m.dy;
            if (!passable(nx, ny)) continue;
            int ni = cell_idx(nx, ny);
            if (owner[ni] != 0) {
                uint8_t other = owner[ni];
                if (other != cur.owner) {
                    if (cur.owner == my_id) in_contact[other] = true;
                    else if (other == my_id) in_contact[cur.owner] = true;
                }
                continue;
            }
            owner[ni] = cur.owner;
            dist[ni] = static_cast<uint16_t>(cur.dist + 1);
            counts[cur.owner]++;
            q.push_back({static_cast<uint16_t>(ni), cur.owner,
                         static_cast<uint16_t>(cur.dist + 1)});
        }
    }

    int max_opp = 0;
    bool any_contact = false;
    for (int p = 1; p <= MAX_PLAYERS; ++p) {
        if (p == my_id || !in_contact[p]) continue;
        any_contact = true;
        if (counts[p] > max_opp) max_opp = counts[p];
    }
    return any_contact ? counts[my_id] - max_opp : counts[my_id];
}

// ============================================================
//  Terminal score
// ============================================================

inline int terminal_score(int depth) {
    int alive = count_live();
    if (alive == 0) return 0;
    int last = 0;
    for (int p = 1; p <= MAX_PLAYERS; ++p)
        if (players[p].id != 0 && !players[p].dead) { last = p; break; }
    int sign = (last == my_id) ? +1 : -1;
    return sign * CELLS * 4 * (depth + 1);
}

// ============================================================
//  Time check + search globals.
// ============================================================

inline steady_clock::time_point deadline;
inline bool aborted = false;
inline uint64_t nodes_searched = 0;

inline void check_time() {
    if ((nodes_searched & 0xFF) == 0 && steady_clock::now() > deadline)
        aborted = true;
}

// ============================================================
//  Alpha-beta (paranoid: I'm max, everyone else is min)
// ============================================================

inline int ab(int player_to_move, int depth, int alpha, int beta) {
    ++nodes_searched;
    check_time();
    if (aborted) return 0;

    if (count_live() <= 1) return terminal_score(depth);
    if (depth == 0) return leaf_heuristic();

    uint64_t h = cur_hash;
    int hit;
    if (tt_probe(h, depth, alpha, beta, hit)) return hit;

    const int orig_alpha = alpha;
    const int orig_beta = beta;
    const bool is_me = (player_to_move == my_id);
    int best = is_me ? -INF : +INF;

    Player& py = players[player_to_move];
    int next_p = next_live(player_to_move);

    array<int, 4> order = {0, 1, 2, 3};
    array<int, 4> freedom = {-1, -1, -1, -1};
    for (int i = 0; i < 4; ++i) {
        int nx = py.x + MOVES[i].dx;
        int ny = py.y + MOVES[i].dy;
        if (!passable(nx, ny)) continue;
        int f = 0;
        for (auto& nm : MOVES)
            if (passable(nx + nm.dx, ny + nm.dy)) ++f;
        freedom[i] = f;
    }
    std::sort(order.begin(), order.end(),
              [&](int a, int b) { return freedom[a] > freedom[b]; });

    int legal = 0;
    for (int oi : order) {
        if (freedom[oi] < 0) continue;
        ++legal;
        const auto& m = MOVES[oi];
        int nx = py.x + m.dx;
        int ny = py.y + m.dy;
        int old_x = py.x;
        int old_y = py.y;
        uint8_t prev = mark_cell(nx, ny, static_cast<uint8_t>(player_to_move));
        py.x = nx;
        py.y = ny;
        hash_xor_side(player_to_move);
        hash_xor_side(next_p);

        int v = ab(next_p, depth - 1, alpha, beta);

        hash_xor_side(next_p);
        hash_xor_side(player_to_move);
        py.x = old_x;
        py.y = old_y;
        unmark_cell(nx, ny, prev);

        if (aborted) return 0;

        if (is_me) {
            if (v > best) best = v;
            if (best > alpha) alpha = best;
        } else {
            if (v < best) best = v;
            if (best < beta) beta = best;
        }
        if (alpha >= beta) break;
    }

    if (legal == 0) {
        players[player_to_move].dead = true;
        hash_xor_dead(player_to_move);

        int v;
        if (is_me) {
            v = -CELLS * 4 * (depth + 1);
        } else {
            int alt_next = next_live(player_to_move);
            hash_xor_side(player_to_move);
            hash_xor_side(alt_next);
            v = ab(alt_next, depth - 1, alpha, beta);
            hash_xor_side(alt_next);
            hash_xor_side(player_to_move);
        }

        hash_xor_dead(player_to_move);
        players[player_to_move].dead = false;
        if (aborted) return 0;
        best = v;
    }

    TTFlag flag;
    if (best <= orig_alpha) flag = TT_UPPER;
    else if (best >= orig_beta) flag = TT_LOWER;
    else flag = TT_EXACT;
    tt_store(h, depth, best, flag);

    return best;
}

// ============================================================
//  Root search + iterative deepening
// ============================================================

struct RootResult { int score; int best_move_idx; bool completed; };

inline RootResult search_root(int depth, int hint_idx) {
    int alpha = -INF, beta = +INF;
    int best = -INF;
    int best_idx = -1;

    Player& me = players[my_id];
    int next_p = next_live(my_id);

    array<int, 4> order = {0, 1, 2, 3};
    if (hint_idx >= 0 && hint_idx < 4) {
        for (int i = 0; i < 4; ++i)
            if (order[i] == hint_idx) { std::swap(order[0], order[i]); break; }
    }

    for (int oi : order) {
        const auto& m = MOVES[oi];
        int nx = me.x + m.dx;
        int ny = me.y + m.dy;
        if (!passable(nx, ny)) continue;

        int old_x = me.x;
        int old_y = me.y;
        uint8_t prev = mark_cell(nx, ny, static_cast<uint8_t>(my_id));
        me.x = nx;
        me.y = ny;
        hash_xor_side(my_id);
        hash_xor_side(next_p);

        int v = ab(next_p, depth - 1, alpha, beta);

        hash_xor_side(next_p);
        hash_xor_side(my_id);
        me.x = old_x;
        me.y = old_y;
        unmark_cell(nx, ny, prev);

        if (aborted) return {best, best_idx, false};

        if (v > best) {
            best = v;
            best_idx = oi;
            if (best > alpha) alpha = best;
        }
    }
    return {best, best_idx, true};
}

inline int iterative_search() {
    aborted = false;
    nodes_searched = 0;
    int best_idx = -1;
    int hint = -1;
    for (int depth = 1; depth <= MAX_DEPTH; ++depth) {
        RootResult r = search_root(depth, hint);
        if (aborted) break;
        if (r.best_move_idx >= 0) {
            best_idx = r.best_move_idx;
            hint = best_idx;
        }
        if (std::abs(r.score) >= CELLS * 4) break;
    }
    return best_idx;
}

// ============================================================
//  Bot entry points
// ============================================================

// Tron has no per-match init payload. The runner spawns a fresh
// subprocess per match, so all the inline globals above start fresh
// every match. Zobrist tables need to be populated once
// (constructors don't fill `z_cell` etc); doing it here keeps the
// lifecycle obvious.
inline void on_init(const cgio::InitialInput& /*init*/) {
    init_zobrist();
}

inline TurnOutput decide(const cgio::TurnInput& turn) {
    int N = turn.number_of_players;
    int P = turn.player_number;

    if (first_tick) {
        num_players = N;
        my_id = P + 1;
        for (int p = 0; p <= MAX_PLAYERS; ++p) players[p] = Player{};
    }

    for (int i = 1; i <= N; ++i) {
        const auto& line = turn.player_lines[static_cast<size_t>(i - 1)];
        int X0 = line.start.x, Y0 = line.start.y;
        int X1 = line.end.x,   Y1 = line.end.y;

        players[i].id = i;
        if (X0 == -1) {
            players[i].dead = true;
            continue;
        }
        if (first_tick)
            board[cell_idx(X0, Y0)] = static_cast<uint8_t>(i);
        board[cell_idx(X1, Y1)] = static_cast<uint8_t>(i);
        players[i].x = X1;
        players[i].y = Y1;
        players[i].dead = false;
    }
    first_tick = false;

    deadline = steady_clock::now() + TURN_BUDGET;
    tt_clear();
    recompute_hash(my_id);

    int best = iterative_search();
    TurnOutput out{};
    out.direction = (best < 0) ? Direction::Up : MOVES[best].dir;
    return out;
}

}  // namespace tron_v2_cpp
