// Tron lightcycle bot — v2. Single-file C++20, paste-ready for CodinGame's
// editor. Sibling of ../v1/tron.cpp; design rationale in docs/v2-bot.md.
//
// What changed since v1:
//
//   1. Bug fixes.
//      * game_over() / count_live() now respect players killed *inside*
//        the search (v1 only respected players already dead at the root).
//      * No more `controls[0]` underflow on the first claim of a cell.
//      * The root no longer drops the previous cell value when undoing
//        a candidate move.
//      * Winner detection no longer returns "first can_move" — terminal
//        scoring goes directly off the live-player count.
//
//   2. Iterative deepening with a real time budget.
//      Search depth 1, then 2, then 3, … until ~90ms elapsed. The
//      deepest *completed* iteration's best move is the answer. Replaces
//      v1's hard-coded `max_depth=5`, which timed out on wide-open
//      boards and under-searched in tight endgames.
//
//   3. Zobrist-hashed transposition table.
//      Alpha-beta on tron has lots of in-ply transpositions (different
//      move orderings can reach the same board). Caching sub-search
//      values by hash gives an order-of-magnitude node-count reduction
//      on cramped positions. See the long comment block above the TT.
//
//   4. Move ordering.
//      Root: try the previous iteration's best move first ("PV-move
//      first"). Inner nodes: try moves whose destination has more open
//      neighbours first (a cheap freedom heuristic). Combined with the
//      TT, this is where most of the speedup comes from.
//
//   5. Isolation detection.
//      Once no opponent's BFS frontier ever overlaps mine, the game
//      becomes solo "fill as much of my region as possible". In that
//      mode we return `my_area * 4` from the heuristic so isolation
//      dominates contested-area scores at equal counts.
//
//   6. Better leaf score.
//      v1: `my_voronoi`. v2: `my_voronoi − max_opp_voronoi`, so
//      "I have 200, they have 50" beats "I have 200, they have 200".
//
//   7. Templated min/max combinator (no std::function).
//      Removes a virtual dispatch per AB node — the inner loop is the
//      hot path, this matters.
//
//   8. Numeric direction table; the direction names live in a parallel
//      array used only at output time.
//
// Open follow-ups (intentionally deferred — see docs/v2-bot.md):
//   * Bitboard board representation.
//   * Max⁻ instead of paranoid for 3–4 player games.
//   * History heuristic / killer moves.

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <iostream>
#include <queue>
#include <random>
#include <string>

using namespace std;
using namespace std::chrono;

// ============================================================
//  Constants
// ============================================================

constexpr int WIDTH = 30;
constexpr int HEIGHT = 20;
constexpr int CELLS = WIDTH * HEIGHT;
constexpr int MAX_PLAYERS = 4;

// CodinGame's per-turn budget is 100ms; we leave 10ms of headroom for
// stdin parsing, the iteration aborted in flight, and stdout flush.
constexpr auto TURN_BUDGET = milliseconds(90);

// Loose backstop on iterative-deepening. The real bound is
// `check_time()` firing once we've burned the budget; this is just
// belt-and-suspenders so the loop terminates if the time check ever
// goes wrong. With the BFS pre-allocation + tighter time-check
// frequency in place, we want to see how deep we actually get.
constexpr int MAX_DEPTH = 100;

constexpr int INF = 1'000'000'000;

// ============================================================
//  Board + players
// ============================================================
//
// board[i] holds the OWNER of cell i: 0 = empty, 1..MAX_PLAYERS = a
// player's trail. players[id] carries the player's current head + a
// `dead` flag. A "dead" player's trail still has its id stamped on the
// board, but `passable()` treats it as empty — that's why the search
// never has to physically scrub the board when a player dies.

uint8_t board[CELLS]{};

struct Player
{
    int id = 0;       // 1..MAX_PLAYERS; 0 if slot unused
    int x = 0, y = 0; // current head position
    bool dead = true;
};

Player players[MAX_PLAYERS + 1]; // 1-indexed; players[0] is the
                                 // "empty cell" sentinel and unused.
int my_id = 0;
int num_players = 0;
bool first_tick = true;

inline int cell_idx(int x, int y) { return y * WIDTH + x; }
inline bool in_bounds(int x, int y) { return x >= 0 && x < WIDTH && y >= 0 && y < HEIGHT; }

inline bool passable(int x, int y)
{
    if (!in_bounds(x, y))
        return false;
    uint8_t c = board[cell_idx(x, y)];
    return c == 0 || players[c].dead;
}

struct Move
{
    int dx, dy;
    const char *name;
};
constexpr Move MOVES[4] = {
    {0, -1, "UP"},
    {1, 0, "RIGHT"},
    {0, 1, "DOWN"},
    {-1, 0, "LEFT"},
};

// ============================================================
//  Zobrist hash + transposition table
// ============================================================
//
// What is a transposition table?
//
//   Alpha-beta minimax explores a tree where many branches lead to the
//   same position (a "transposition"). For example, in a 2-player tron
//   game, "I go UP then opponent goes LEFT" can lead to the same board
//   as "I go LEFT then opponent goes UP" if neither move interferes.
//   Without a TT, we'd re-search that subtree from scratch every time.
//   With a TT, we look the position up by a hash key, find the value
//   we computed last time, and skip the redundant work.
//
// How Zobrist hashing works:
//
//   We assign a random 64-bit key to every (cell, owner) combination,
//   plus a key per "side to move" and per "player p is dead". The
//   hash of a position is the XOR of every key that's currently "on":
//
//       hash =  XOR over (cell, owner) where owner != 0
//            ⊕  z_side[player_to_move]
//            ⊕  XOR over p where players[p].dead
//
//   XOR has two properties that make this perfect:
//     1. Order-independent: A ⊕ B == B ⊕ A.
//     2. Self-inverse: A ⊕ A == 0.
//
//   So we can update the hash *incrementally*. When we mark cell i as
//   owned by id during the search, we XOR in z_cell[i][id]; when we
//   undo the move, we XOR z_cell[i][id] back out — and we're guaranteed
//   to get the original hash back. Same trick for side-to-move flips
//   and dead-flag flips. The cost per AB node is a handful of XORs
//   instead of re-hashing the whole board.
//
//   A 64-bit hash gives an effective collision rate of ~2^-32 over our
//   search sizes. A "production" engine would also store a second
//   verification key in the TT entry; for a CodinGame bot the single
//   hash is fine.
//
// What the TT entry stores:
//
//   * hash:  the full 64-bit key, used to confirm we hit the right
//            position (TT is indexed by `hash & MASK`, so different
//            positions can land in the same slot).
//   * depth: the search depth that produced this value. A value
//            computed at depth N is usable at depth ≤ N (deeper
//            searches need fresh work).
//   * value: the score.
//   * flag:  whether the value is
//              EXACT    — full search within (alpha, beta) returned it.
//              LOWER    — beta cutoff happened; true value ≥ stored.
//              UPPER    — alpha cutoff happened; true value ≤ stored.
//
// Probing:
//
//   For a (hash, depth, alpha, beta) query:
//     * Check the slot. If empty, miss.
//     * If the stored hash doesn't match, miss (collision).
//     * If the stored depth is shallower than we need, miss.
//     * Otherwise:
//         EXACT  → return stored value.
//         LOWER  → if stored ≥ beta, return stored (proves a beta
//                  cutoff would happen).
//         UPPER  → if stored ≤ alpha, return stored (proves an alpha
//                  cutoff would happen).
//         Else   → miss (the bound isn't tight enough for this window).
//
// Storage policy:
//
//   We use "always replace if our depth is at least as deep" — the
//   simplest scheme and good enough for our search sizes. A real
//   engine would use a two-tier (always-replace + depth-preferred)
//   buckets, but the gains are small for our depth/time budget.
//
// Lifetime:
//
//   We clear the TT at the start of each tick. Tron's board strictly
//   grows over time (every tick adds at least one trail cell), so
//   cross-tick reuse is effectively zero — the TT entries we'd keep
//   describe positions we'll never see again.

uint64_t z_cell[CELLS][MAX_PLAYERS + 1];
uint64_t z_side[MAX_PLAYERS + 1];
uint64_t z_dead[MAX_PLAYERS + 1];
uint64_t cur_hash = 0;

void init_zobrist()
{
    // Fixed seed so two runs over the same input sequence produce the
    // same hashes — handy when bisecting search regressions.
    mt19937_64 rng(0xC0FFEEC0DE5EEDULL);
    for (int i = 0; i < CELLS; ++i)
        for (int p = 0; p <= MAX_PLAYERS; ++p)
            z_cell[i][p] = rng();
    for (int p = 0; p <= MAX_PLAYERS; ++p)
    {
        z_side[p] = rng();
        z_dead[p] = rng();
    }
}

inline void hash_xor_cell(int idx, uint8_t id) { cur_hash ^= z_cell[idx][id]; }
inline void hash_xor_side(int p) { cur_hash ^= z_side[p]; }
inline void hash_xor_dead(int p) { cur_hash ^= z_dead[p]; }

// Recompute the hash from scratch — called once per tick after we've
// finished parsing input.
void recompute_hash(int side_to_move)
{
    cur_hash = 0;
    for (int i = 0; i < CELLS; ++i)
        if (board[i] != 0)
            cur_hash ^= z_cell[i][board[i]];
    for (int p = 1; p <= MAX_PLAYERS; ++p)
        if (players[p].id != 0 && players[p].dead)
            cur_hash ^= z_dead[p];
    cur_hash ^= z_side[side_to_move];
}

enum TTFlag : uint8_t
{
    TT_EXACT = 0,
    TT_LOWER = 1,
    TT_UPPER = 2
};

struct TTEntry
{
    uint64_t hash = 0;
    int32_t value = 0;
    int8_t depth = -1; // -1 means "slot is empty"
    TTFlag flag = TT_EXACT;
};

constexpr int TT_BITS = 17; // 131,072 entries × 24B ≈ 3 MB
constexpr int TT_SIZE = 1 << TT_BITS;
constexpr int TT_MASK = TT_SIZE - 1;

TTEntry tt[TT_SIZE];

void tt_clear()
{
    for (auto &e : tt)
        e.depth = -1;
}

inline bool tt_probe(uint64_t h, int depth, int alpha, int beta, int &out)
{
    const TTEntry &e = tt[h & TT_MASK];
    if (e.depth < depth || e.hash != h)
        return false;
    switch (e.flag)
    {
    case TT_EXACT:
        out = e.value;
        return true;
    case TT_LOWER:
        if (e.value >= beta)
        {
            out = e.value;
            return true;
        }
        break;
    case TT_UPPER:
        if (e.value <= alpha)
        {
            out = e.value;
            return true;
        }
        break;
    }
    return false;
}

inline void tt_store(uint64_t h, int depth, int value, TTFlag flag)
{
    TTEntry &e = tt[h & TT_MASK];
    if (e.depth <= depth)
    { // depth-preferred replacement
        e.hash = h;
        e.depth = static_cast<int8_t>(depth);
        e.value = value;
        e.flag = flag;
    }
}

// ============================================================
//  Search-time mutation helpers
// ============================================================
//
// Every change to the board / side-to-move / dead flags goes through
// these helpers so the hash stays in lockstep. The pattern is always
// "mutate, recurse, unmutate" — we never leak partial mutations across
// the AB stack.

inline uint8_t mark_cell(int x, int y, uint8_t id)
{
    int i = cell_idx(x, y);
    uint8_t prev = board[i];
    if (prev != 0)
        hash_xor_cell(i, prev); // remove the old key
    board[i] = id;
    hash_xor_cell(i, id); // add the new key
    return prev;
}

inline void unmark_cell(int x, int y, uint8_t prev)
{
    int i = cell_idx(x, y);
    hash_xor_cell(i, board[i]); // remove the current key
    board[i] = prev;
    if (prev != 0)
        hash_xor_cell(i, prev); // add the old key back
}

// ============================================================
//  Player iteration
// ============================================================

inline int next_live(int p)
{
    // Walks forward through player ids, wrapping around at MAX_PLAYERS,
    // skipping dead and unused slots.
    for (int step = 1; step <= MAX_PLAYERS; ++step)
    {
        int q = ((p - 1 + step) % MAX_PLAYERS) + 1;
        if (players[q].id != 0 && !players[q].dead)
            return q;
    }
    return p;
}

inline int count_live()
{
    int n = 0;
    for (int p = 1; p <= MAX_PLAYERS; ++p)
        if (players[p].id != 0 && !players[p].dead)
            ++n;
    return n;
}

// ============================================================
//  Leaf heuristic — voronoi by multi-source BFS
// ============================================================
//
// Enqueue every live player at distance 0 and BFS outward. Each cell
// records the first player to reach it (FIFO ties). We return
// `my_count − max(opp_count)`, except when the board has split into
// disjoint regions — see the isolation check below.

// Pulled out of leaf_heuristic() so the static FIFO buffer below can
// name the type at namespace scope.
struct BFSItem
{
    uint16_t idx;
    uint8_t owner;
    uint16_t dist;
};

int leaf_heuristic()
{
    static uint8_t owner[CELLS];
    static uint16_t dist[CELLS];
    // Hoisted FIFO. `std::queue<Item>` on the stack would heap-alloc
    // its internal deque chunks every call; a static vector + head
    // index keeps the same buffer alive across calls (and is flat in
    // memory, so the cache likes it better than deque's chunked
    // layout). BFS visits each cell at most once, so CELLS is a hard
    // upper bound on size — `reserve(CELLS)` once means push_back
    // never reallocates from here on.
    static vector<BFSItem> q;
    if (q.capacity() < CELLS)
        q.reserve(CELLS);
    q.clear();
    size_t head = 0; // local: fresh per call, distinct from the
                     // static buffer's persistent state.

    memset(owner, 0, sizeof(owner));

    int counts[MAX_PLAYERS + 1] = {};
    // Per-opponent contact flag. `in_contact[p]` flips true the
    // moment my BFS wave and player p's BFS wave try to claim cells
    // on either side of a shared boundary — i.e. we're actually
    // competing for the same territory. Opponents we never bump
    // into (different region, or the game is large and open) stay
    // out of the heuristic.
    bool in_contact[MAX_PLAYERS + 1] = {};

    for (int p = 1; p <= MAX_PLAYERS; ++p)
    {
        if (players[p].id == 0 || players[p].dead)
            continue;
        int i = cell_idx(players[p].x, players[p].y);
        owner[i] = static_cast<uint8_t>(p);
        dist[i] = 0;
        counts[p]++;
        q.push_back({static_cast<uint16_t>(i), static_cast<uint8_t>(p), 0});
    }

    while (head < q.size())
    {
        BFSItem cur = q[head++]; // pop_front via index advance
        int x = cur.idx % WIDTH;
        int y = cur.idx / WIDTH;
        for (auto &m : MOVES)
        {
            int nx = x + m.dx, ny = y + m.dy;
            if (!passable(nx, ny))
                continue;
            int ni = cell_idx(nx, ny);
            if (owner[ni] != 0)
            {
                // Already claimed → contact if it was the other
                // player's wave. Only record contact pairs that
                // involve me; everyone else's frontier interactions
                // are noise from my POV.
                uint8_t other = owner[ni];
                if (other != cur.owner)
                {
                    if (cur.owner == my_id)
                        in_contact[other] = true;
                    else if (other == my_id)
                        in_contact[cur.owner] = true;
                }
                continue;
            }
            owner[ni] = cur.owner;
            dist[ni] = static_cast<uint16_t>(cur.dist + 1);
            counts[cur.owner]++;
            q.push_back({static_cast<uint16_t>(ni),
                         cur.owner,
                         static_cast<uint16_t>(cur.dist + 1)});
        }
    }

    // Heuristic: `my - max(opp_count)` over opponents I'm actually
    // in contact with. If I'm isolated (no contact), fall back to
    // just `my` — the v1 score. The earlier `max over ALL opps`
    // form pushed me to chase distant rivals at my own expense;
    // restricting to contacted opponents keeps the relative-area
    // signal but only where it actually matters.
    int max_opp = 0;
    bool any_contact = false;
    for (int p = 1; p <= MAX_PLAYERS; ++p)
    {
        if (p == my_id || !in_contact[p])
            continue;
        any_contact = true;
        if (counts[p] > max_opp)
            max_opp = counts[p];
    }
    return any_contact ? counts[my_id] - max_opp : counts[my_id];
}

// ============================================================
//  Terminal score
// ============================================================
//
// Called when count_live() ≤ 1. The `depth` factor pushes the engine
// toward winning *sooner* and losing *later* — both are encoded by
// scaling the magnitude by (depth + 1) so a deeper-detected outcome
// loses to a shallower-detected one (of the same sign).

inline int terminal_score(int depth)
{
    int alive = count_live();
    if (alive == 0)
        return 0; // mutual death — tie
    int last = 0;
    for (int p = 1; p <= MAX_PLAYERS; ++p)
        if (players[p].id != 0 && !players[p].dead)
        {
            last = p;
            break;
        }
    int sign = (last == my_id) ? +1 : -1;
    return sign * CELLS * 4 * (depth + 1);
}

// ============================================================
//  Time check
// ============================================================

steady_clock::time_point deadline;
bool aborted = false;
uint64_t nodes_searched = 0;

inline void check_time()
{
    // Sample every 256 nodes. With a 10–20µs leaf BFS, that's roughly
    // a 2–5ms granularity on the abort — small enough that the abort
    // fires while we still have budget to flush the previous depth's
    // answer, but rare enough that the clock-read overhead is noise.
    // (Was 4096; that let depth 8+ blow the whole 100ms budget on an
    // open board before the first check fired.)
    if ((nodes_searched & 0xFF) == 0 && steady_clock::now() > deadline)
        aborted = true;
}

// ============================================================
//  Alpha-beta (paranoid: I'm max, everyone else is min)
// ============================================================

int ab(int player_to_move, int depth, int alpha, int beta)
{
    ++nodes_searched;
    check_time();
    if (aborted)
        return 0;

    if (count_live() <= 1)
        return terminal_score(depth);
    if (depth == 0)
        return leaf_heuristic();

    uint64_t h = cur_hash;
    int hit;
    if (tt_probe(h, depth, alpha, beta, hit))
        return hit;

    const int orig_alpha = alpha;
    const int orig_beta = beta;
    const bool is_me = (player_to_move == my_id);
    int best = is_me ? -INF : +INF;

    Player &py = players[player_to_move];
    int next_p = next_live(player_to_move);

    // Local move ordering: prefer moves whose destination has more
    // open neighbours. Cheap, no recursion needed. Indices into MOVES.
    array<int, 4> order = {0, 1, 2, 3};
    array<int, 4> freedom = {-1, -1, -1, -1};
    for (int i = 0; i < 4; ++i)
    {
        int nx = py.x + MOVES[i].dx;
        int ny = py.y + MOVES[i].dy;
        if (!passable(nx, ny))
            continue;
        int f = 0;
        for (auto &nm : MOVES)
            if (passable(nx + nm.dx, ny + nm.dy))
                ++f;
        freedom[i] = f;
    }
    sort(order.begin(), order.end(),
         [&](int a, int b)
         { return freedom[a] > freedom[b]; });

    int legal = 0;
    for (int oi : order)
    {
        if (freedom[oi] < 0)
            continue;
        ++legal;
        const auto &m = MOVES[oi];
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

        if (aborted)
            return 0;

        if (is_me)
        {
            if (v > best)
                best = v;
            if (best > alpha)
                alpha = best;
        }
        else
        {
            if (v < best)
                best = v;
            if (best < beta)
                beta = best;
        }
        if (alpha >= beta)
            break;
    }

    if (legal == 0)
    {
        // No legal moves → this player dies. Their trail stays on the
        // board; flipping `dead` flag is enough to let everyone walk
        // through it.
        players[player_to_move].dead = true;
        hash_xor_dead(player_to_move);

        int v;
        if (is_me)
        {
            // I die. Don't bother recursing; the position is lost.
            v = -CELLS * 4 * (depth + 1);
        }
        else
        {
            int alt_next = next_live(player_to_move);
            hash_xor_side(player_to_move);
            hash_xor_side(alt_next);
            v = ab(alt_next, depth - 1, alpha, beta);
            hash_xor_side(alt_next);
            hash_xor_side(player_to_move);
        }

        hash_xor_dead(player_to_move);
        players[player_to_move].dead = false;
        if (aborted)
            return 0;
        best = v;
    }

    // Translate the result into a TT flag based on whether we got an
    // alpha cutoff (fail-low → UPPER bound), beta cutoff (fail-high →
    // LOWER bound), or a full window (EXACT). See the TT comment.
    TTFlag flag;
    if (best <= orig_alpha)
        flag = TT_UPPER;
    else if (best >= orig_beta)
        flag = TT_LOWER;
    else
        flag = TT_EXACT;
    tt_store(h, depth, best, flag);

    return best;
}

// ============================================================
//  Root search + iterative deepening
// ============================================================

struct RootResult
{
    int score;
    int best_move_idx; // index into MOVES, or -1 if no legal move
    bool completed;    // true if the iteration finished before deadline
};

RootResult search_root(int depth, int hint_idx)
{
    int alpha = -INF, beta = +INF;
    int best = -INF;
    int best_idx = -1;

    Player &me = players[my_id];
    int next_p = next_live(my_id);

    array<int, 4> order = {0, 1, 2, 3};
    if (hint_idx >= 0 && hint_idx < 4)
    {
        // Move the previous iteration's best to the front.
        for (int i = 0; i < 4; ++i)
            if (order[i] == hint_idx)
            {
                swap(order[0], order[i]);
                break;
            }
    }

    for (int oi : order)
    {
        const auto &m = MOVES[oi];
        int nx = me.x + m.dx;
        int ny = me.y + m.dy;
        if (!passable(nx, ny))
            continue;

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

        if (aborted)
            return {best, best_idx, false};

        if (v > best)
        {
            best = v;
            best_idx = oi;
            if (best > alpha)
                alpha = best;
        }
    }
    return {best, best_idx, true};
}

int iterative_search()
{
    aborted = false;
    nodes_searched = 0;
    int best_idx = -1;
    int hint = -1;
    for (int depth = 1; depth <= MAX_DEPTH; ++depth)
    {
        RootResult r = search_root(depth, hint);
        if (aborted)
            break;
        if (r.best_move_idx >= 0)
        {
            best_idx = r.best_move_idx;
            hint = best_idx;
        }
        // Forced-outcome short circuit: if the score saturated the
        // terminal-magnitude bound, deeper search just confirms.
        if (std::abs(r.score) >= CELLS * 4)
            break;
    }
    return best_idx;
}

// ============================================================
//  Main loop — wire format identical to v1
// ============================================================

int main()
{
    init_zobrist();
    while (true)
    {
        int N, P;
        if (!(cin >> N >> P))
            break;
        cin.ignore();

        if (first_tick)
        {
            num_players = N;
            my_id = P + 1;
            for (int p = 0; p <= MAX_PLAYERS; ++p)
                players[p] = Player{};
        }

        for (int i = 1; i <= N; ++i)
        {
            int X0, Y0, X1, Y1;
            cin >> X0 >> Y0 >> X1 >> Y1;
            cin.ignore();

            players[i].id = i;
            if (X0 == -1)
            {
                // Dead. Their trail (if any) stays on the board; the
                // `dead` flag makes `passable()` walk through it.
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
        if (best < 0)
        {
            // No legal move — output something deterministic; we lose
            // this turn regardless.
            cout << "UP" << endl;
        }
        else
        {
            cout << MOVES[best].name << endl;
        }
    }
    return 0;
}
