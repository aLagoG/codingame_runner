// Tron bot strategy — v1.
//
// Port of games/tron/bots/baseline_cpp/v1/tron.cpp onto the workspace's
// bot template: structurally identical to the original CodinGame paste,
// but wrapped in a namespace and driven by `cgio::TurnInput` / `TurnOutput`
// instead of stdin / stdout. The per-tick logic at the bottom of
// `decide()` mirrors the snapshot's `int main()` body line-for-line so a
// future bug-fix port stays a 5-minute diff. Bugs the v2 prologue calls
// out (controls[0] underflow, in-search dead-flag races, the root
// dropping the previous cell value on undo, "winner = first can_move")
// are preserved verbatim — fidelity > correctness here.

#pragma once

#include "../../defs/include/tron_defs_io.h"

#include <functional>
#include <limits>
#include <queue>
#include <vector>

namespace tron_v1_cpp {

constexpr int WIDTH = 30, HEIGHT = 20;

inline char g_map[WIDTH][HEIGHT]{};
inline int g_max_depth = 5;
inline bool g_first_tick = true;
inline unsigned int g_heuristic_count = 0;
// `dead_players[0]` doubles as the "empty cell" flag — `Position::empty()`
// reads `dead_players[map[x][y]]` and the board is 0-initialised, so
// index 0 being "passable" is what makes empty cells walkable. Preserved
// from the snapshot's `bool dead_players[5]{1}` initializer.
inline bool g_dead_players[5] = {true, false, false, false, false};

inline constexpr const int& my_max(const int& a, const int& b) { return a > b ? a : b; }
inline constexpr const int& my_min(const int& a, const int& b) { return a < b ? a : b; }

struct Position {
    char x, y;
    Position() : x(0), y(0) {}
    Position(char x, char y) : x(x), y(y) {}
    Position operator+(const Position& b) const { return Position(x + b.x, y + b.y); }
    Position operator-(const Position& b) const { return Position(x - b.x, y - b.y); }
    Position& operator+=(const Position& b) { x = static_cast<char>(x + b.x); y = static_cast<char>(y + b.y); return *this; }
    Position& operator-=(const Position& b) { x = static_cast<char>(x - b.x); y = static_cast<char>(y - b.y); return *this; }
    bool valid() const { return x < WIDTH && x >= 0 && y < HEIGHT && y >= 0; }
    bool empty() const { return valid() && g_dead_players[static_cast<int>(g_map[static_cast<int>(x)][static_cast<int>(y)])]; }
    char set(char id) const {
        char tmp = g_map[static_cast<int>(x)][static_cast<int>(y)];
        g_map[static_cast<int>(x)][static_cast<int>(y)] = id;
        return tmp;
    }
    void clear(char last = 0) const { g_map[static_cast<int>(x)][static_cast<int>(y)] = last; }
};

struct MoveEntry { const char* name; Position delta; Direction dir; };
inline MoveEntry MOVES[4] = {
    {"UP",    Position(0, -1), Direction::Up},
    {"RIGHT", Position(1, 0),  Direction::Right},
    {"DOWN",  Position(0, 1),  Direction::Down},
    {"LEFT",  Position(-1, 0), Direction::Left},
};

struct Player {
    char id, index;
    Position p;
    static std::vector<Player> list;
    static Position last_known[5];
    static char my_id, my_index;

    static Player& me() { return list[static_cast<size_t>(my_index)]; }
    static Player& next(const Player& py) {
        return list[static_cast<size_t>((py.index + 1) % list.size())];
    }
    static void clear_list() { list.clear(); }

    static void create(char x0, char y0, char x, char y, char id) {
        if (g_first_tick) {
            Position(x0, y0).set(id);
        }
        if (x == -1 || y == -1) {
            g_dead_players[static_cast<int>(id)] = true;
            return;
        }
        list.emplace_back(x, y, id, static_cast<char>(list.size()));
        list.back().p.set(id);
        last_known[static_cast<int>(id)] = list.back().p;
        if (id == my_id) {
            my_index = static_cast<char>(list.size() - 1);
        }
    }

    Player(char x, char y, char id, char index) : p(x, y), id(id), index(index) {}
    bool can_move() {
        for (auto& move : MOVES) {
            p += move.delta;
            if (p.empty()) {
                p -= move.delta;
                return true;
            }
            p -= move.delta;
        }
        return false;
    }
    bool operator==(const Player& other) const { return id == other.id; }
};
inline std::vector<Player> Player::list{};
inline Position Player::last_known[5]{};
inline char Player::my_id = 0;
inline char Player::my_index = 0;

struct SearchNode {
    char player; Position pos; unsigned int dist;
    SearchNode(char pl, Position pos_, unsigned int dist_) : player(pl), pos(pos_), dist(dist_) {}
};

struct ControlNode {
    unsigned int last_set = 0, value = 0;
    char best = 0;

    static unsigned int controls[5];
    static ControlNode distances[WIDTH][HEIGHT];

    static void reset_controls() {
        for (int i = 0; i < 5; ++i) controls[i] = 0;
    }

    char set(char id, unsigned int val) {
        if (last_set < g_heuristic_count) {
            last_set = g_heuristic_count;
            best = 0;
        }
        if (!best || value > val) {
            // v1 underflow: when `best == 0` this decrements controls[0].
            // v2's prologue calls it out; we preserve it verbatim.
            --controls[static_cast<int>(best)];
            ++controls[static_cast<int>(id)];
            best = id;
            value = val;
        }
        return best;
    }

    bool visited_this_round(char id) const {
        return best == id && last_set >= g_heuristic_count;
    }
};
inline unsigned int ControlNode::controls[5]{};
inline ControlNode ControlNode::distances[WIDTH][HEIGHT]{};

inline int heuristic() {
    ++g_heuristic_count;
    std::queue<SearchNode> q;
    for (Player& py : Player::list) {
        if (g_dead_players[static_cast<int>(py.id)]) continue;
        q.emplace(py.id, py.p, 0u);
    }
    while (!q.empty()) {
        SearchNode current = q.front();
        q.pop();
        for (auto& move : MOVES) {
            current.pos += move.delta;
            if (current.pos.empty() &&
                !ControlNode::distances[static_cast<int>(current.pos.x)][static_cast<int>(current.pos.y)]
                     .visited_this_round(current.player) &&
                ControlNode::distances[static_cast<int>(current.pos.x)][static_cast<int>(current.pos.y)]
                     .set(current.player, current.dist + 1) == current.player) {
                q.emplace(current.player, current.pos, current.dist + 1);
            }
            current.pos -= move.delta;
        }
    }
    unsigned int res = ControlNode::controls[static_cast<int>(Player::me().id)];
    ControlNode::reset_controls();
    return static_cast<int>(res);
}

inline bool game_over() {
    int cnt = 0;
    for (auto& player : Player::list) if (player.can_move()) ++cnt;
    return cnt <= 1;
}

inline Player& winner() {
    for (auto& player : Player::list) if (player.can_move()) return player;
    return Player::list.back();
}

inline int game_over_score() {
    return winner() == Player::me() ? WIDTH * HEIGHT * 2 : -WIDTH * HEIGHT * 2;
}

inline int AB(Player& py, int depth, int alpha, int beta);

inline int search_step(Player& py, int depth, int value, int& alpha, int& beta,
                       int& current,
                       std::function<const int&(const int&, const int&)> fn) {
    Player& next = Player::next(py);
    int initial_value = value;
    for (auto& move : MOVES) {
        py.p += move.delta;
        if (!py.p.empty()) {
            py.p -= move.delta;
            continue;
        }
        char last = py.p.set(py.id);
        int res = AB(next, depth - 1, alpha, beta);
        py.p.clear(last);
        py.p -= move.delta;
        value = fn(res, value);
        current = fn(value, current);
        if (alpha >= beta) break;
    }
    if (initial_value == value) {
        g_dead_players[static_cast<int>(py.id)] = true;
        int res = py == Player::me() ? -2 * WIDTH * HEIGHT * (depth + 1)
                                     : AB(next, depth - 1, alpha, beta);
        g_dead_players[static_cast<int>(py.id)] = false;
        return res;
    }
    return value;
}

inline int AB(Player& py, int depth, int alpha, int beta) {
    if (game_over()) {
        return (depth + 1) * game_over_score();
    }
    if (!depth) {
        return heuristic();
    }
    if (py == Player::me()) {
        return search_step(py, depth, std::numeric_limits<int>::min(), alpha, beta,
                           alpha, my_max);
    } else {
        return search_step(py, depth, std::numeric_limits<int>::max(), alpha, beta,
                           beta, my_min);
    }
}

// ----------------------------------------------------------------
//  Bot entry points
// ----------------------------------------------------------------

// Tron has no per-match init payload. The runner spawns a fresh
// subprocess per match, so all the inline globals above start fresh
// every match with no reset work needed here.
inline void on_init(const cgio::InitialInput& /*init*/) {}

inline TurnOutput decide(const cgio::TurnInput& turn) {
    int N = turn.number_of_players;
    int P = turn.player_number;
    Player::my_id = static_cast<char>(P + 1);

    for (int i = 0; i < N; ++i) {
        const auto& line = turn.player_lines[static_cast<size_t>(i)];
        Player::create(static_cast<char>(line.start.x), static_cast<char>(line.start.y),
                       static_cast<char>(line.end.x),   static_cast<char>(line.end.y),
                       static_cast<char>(i + 1));
    }
    if (g_first_tick) g_first_tick = false;

    int val = std::numeric_limits<int>::min();
    int best_idx = 0;
    Player& next = Player::next(Player::me());
    for (int mi = 0; mi < 4; ++mi) {
        auto& move = MOVES[mi];
        Player::me().p += move.delta;
        if (!Player::me().p.empty()) {
            Player::me().p -= move.delta;
            continue;
        }
        Player::me().p.set(Player::me().id);
        int res = AB(next, g_max_depth, std::numeric_limits<int>::min(),
                     std::numeric_limits<int>::max());
        // v1 quirk preserved: `clear()` defaults to writing 0, even
        // though the original cell value here is always already 0
        // (we wouldn't have entered the block if it weren't `.empty()`).
        // v2 calls this out as a bug — it'd matter if we ever extended
        // `empty()` to consider non-zero "walk-through" markers.
        Player::me().p.clear();
        Player::me().p -= move.delta;
        if (res > val) {
            val = res;
            best_idx = mi;
        }
    }

    TurnOutput out{};
    out.direction = MOVES[best_idx].dir;
    // v1 clears the per-tick player list and rebuilds it from scratch
    // every tick. We do the same so the next tick's `Player::create`
    // calls land in a clean list.
    Player::clear_list();
    return out;
}

}  // namespace tron_v1_cpp
