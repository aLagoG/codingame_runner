#include <functional>
#include <iostream>
#include <limits>
#include <queue>
#include <string>
#include <vector>
// optimized 169

using namespace std;

const int width = 30, height = 20;
char map[width][height]{};
char n_players;
int max_depth = 5;
bool first = 1;
unsigned int heuristic_count = 0;
bool dead_players[5]{1};

inline constexpr const int& _max(const int& a, const int& b) {
    return a > b ? a : b;
}

inline constexpr const int& _min(const int& a, const int& b) {
    return a < b ? a : b;
}

struct position {
    char x, y;
    position() : x(0), y(0) {}
    position(char x, char y) : x(x), y(y) {}
    position operator+(const position& b) const {
        return position(x + b.x, y + b.y);
    }
    position operator-(const position& b) const {
        return position(x - b.x, y - b.y);
    }
    position& operator+=(const position& b) {
        x += b.x;
        y += b.y;
        return *this;
    }
    position& operator-=(const position& b) {
        x -= b.x;
        y -= b.y;
        return *this;
    }
    inline bool valid() const {
        return x < width && x >= 0 && y < height && y >= 0;
    }
    inline bool empty() const {
        return valid() && dead_players[map[x][y]];
    }
    inline char set(char id) const {
        auto tmp = map[x][y];
        map[x][y] = id;
        return tmp;
    }
    inline void clear(char last = 0) const {
        map[x][y] = last;
    }
    friend ostream& operator<<(ostream& os, const position& p);
};

ostream& operator<<(ostream& os, const position& p) {
    os << '(' << int(p.x) << ", " << int(p.y) << ')';
    return os;
}

pair<string, position> moves[] = {{"UP", position(0, -1)},
                                  {"RIGHT", position(1, 0)},
                                  {"DOWN", position(0, 1)},
                                  {"LEFT", position(-1, 0)}};

struct player {
    char id, index;
    position p;
    static vector<player> list;
    static position last_known[5];
    static char my_id, my_index;
    inline static player& me() {
        return list[my_index];
    }
    static player& next(const player& py) {
        return list[(py.index + 1) % list.size()];
    }
    static void clear() {
        list.clear();
    }
    static void create(char x0, char y0, char x, char y, char id) {
        if (first) {
            position(x0, y0).set(id);
        }
        if (x == -1 || y == -1) {
            dead_players[id] = 1;
            return;
        }
        list.emplace_back(x, y, id, list.size());
        list.back().p.set(id);
        last_known[id] = list.back().p;
        if (id == my_id) {
            my_index = list.size() - 1;
        }
    }
    player(char x, char y, char id, char index)
        : p(x, y), id(id), index(index) {}
    bool can_move() {
        for (auto& move : moves) {
            p += move.second;
            if (p.empty()) {
                p -= move.second;
                return true;
            }
            p -= move.second;
        }
        return false;
    }
    bool operator==(const player& other) const {
        return id == other.id;
    }
    friend ostream& operator<<(ostream& os, const player& p);
};
ostream& operator<<(ostream& os, const player& p) {
    os << int(p.id) << ": (" << int(p.p.x) << ", " << int(p.p.y) << ')';
    return os;
}
vector<player> player::list{};
char player::my_id = 0;
char player::my_index = 0;
position player::last_known[5]{};

void printMap() {
    for (int j = 0; j < height; ++j) {
        for (int i = 0; i < width; ++i) {
            cerr << int(map[i][j]) << ' ';
        }
        cerr << endl;
    }
}

struct search_node {
    char player;
    position pos;
    unsigned int dist;
    search_node(char pl, position pos, unsigned int dist)
        : player(pl), pos(pos), dist(dist) {}
};

struct control_node {
    unsigned int last_set = 0, value = 0;
    char best = 0;

    static unsigned int controls[5];
    static control_node distances[width][height];

    static void reset() {
        for (int i = 0; i < 5; ++i) {
            controls[i] = 0;
        }
    }
    static void printMap() {
        for (int j = 0; j < height; ++j) {
            for (int i = 0; i < width; ++i) {
                cerr << distances[i][j];
            }
            cerr << endl;
        }
    }
    static void printScores() {
        cerr << "[0 ";
        for (int i = 1; i < 5; ++i) {
            cerr << controls[i] << ' ';
        }
        cerr << "] ";
    }

    char set(char id, unsigned int val) {
        if (last_set < heuristic_count) {
            last_set = heuristic_count;
            best = 0;
        }
        if (!best || value > val) {
            --controls[best];
            ++controls[id];
            best = id;
            value = val;
        }
        return best;
    }
    bool visited_this_round(char id) {
        if (best == id && last_set >= heuristic_count) {
            return true;
        }
        return false;
    }
    friend ostream& operator<<(ostream& os, const control_node& n);
};
ostream& operator<<(ostream& os, const control_node& n) {
    if (n.last_set < heuristic_count) {
        os << "- ";
        return os;
    }
    os << int(n.best) << ' ';
    return os;
}
unsigned int control_node::controls[5]{};
control_node control_node::distances[width][height]{};

int heuristic() {
    ++heuristic_count;
    queue<search_node> q;
    for (player& py : player::list) {
        if (dead_players[py.id]) {
            continue;
        }
        q.emplace(py.id, py.p, 0);
    }
    while (q.size()) {
        auto current = q.front();
        q.pop();
        // cerr << int(current.player) << current.pos << current.dist << endl;
        for (auto& move : moves) {
            current.pos += move.second;
            if (current.pos.empty() &&
                !control_node::distances[current.pos.x][current.pos.y]
                     .visited_this_round(current.player) &&
                control_node::distances[current.pos.x][current.pos.y].set(
                    current.player, current.dist + 1) == current.player) {
                q.emplace(current.player, current.pos, current.dist + 1);
            }
            current.pos -= move.second;
        }
    }
    // cerr << endl;
    // control_node::printMap();
    // cerr << endl;
    // control_node::printScores();
    // cerr << endl;
    // cerr << "h: " << control_node::controls[player::me().id] << endl;
    auto res = control_node::controls[player::me().id];
    control_node::reset();
    return res;
}

bool game_over() {
    int cnt = 0;
    for (auto& player : player::list) {
        if (player.can_move()) {
            ++cnt;
        }
    }
    return cnt <= 1;
}

player& winner() {
    for (auto& player : player::list) {
        if (player.can_move()) {
            return player;
        }
    }
    return player::list.back();
}

int game_over_score() {
    return winner() == player::me() ? width * height * 2 : -width * height * 2;
}

int AB(player& player_idx, int depth, int alpha, int beta);

inline int search_step(player& py,
                       int depth,
                       int value,
                       int& alpha,
                       int& beta,
                       int& current,
                       function<const int&(const int&, const int&)> fn) {
    player& next = player::next(py);
    int initial_value = value;
    for (auto& move : moves) {
        py.p += move.second;
        if (!py.p.empty()) {
            py.p -= move.second;
            continue;
        }
        auto last = py.p.set(py.id);
        int res = AB(next, depth - 1, alpha, beta);
        py.p.clear(last);
        py.p -= move.second;
        value = fn(res, value);
        current = fn(value, current);
        // cerr << "a: " << &alpha << " b: " << &beta << " c: " << &current
        //      << endl;
        if (alpha >= beta) {
            break;
        }
    }
    if (initial_value == value) {
        dead_players[py.id] = 1;
        auto res = py == player::me() ? -2 * width * height * (depth + 1)
                                      : AB(next, depth - 1, alpha, beta);
        dead_players[py.id] = 0;
        return res;
    }
    return value;
}

int AB(player& py, int depth, int alpha, int beta) {
    if (game_over()) {
        // cerr << ' ' << int(winner().id) << ' ' << depth << " game_over ("
        //      << game_over_score() << ')' << endl;
        return (depth + 1) * game_over_score();
    }
    if (!depth) {
        return heuristic();
    }
    if (py == player::me()) {
        return search_step(py, depth, numeric_limits<int>::min(), alpha, beta,
                           alpha, _max);
    } else {
        return search_step(py, depth, numeric_limits<int>::max(), alpha, beta,
                           beta, _min);
    }
}

/**
 * Auto-generated code below aims at helping you parse
 * the standard input according to the problem statement.
 **/
int main() {
    // game loop
    while (1) {
        int N;  // total number of players (2 to 4).
        int P;  // your player number (0 to 3).
        cin >> N >> P;
        n_players = N;
        player::my_id = P + 1;
        cin.ignore();
        for (int i = 0; i < N; i++) {
            int X0;  // starting X coordinate of lightcycle (or -1)
            int Y0;  // starting Y coordinate of lightcycle (or -1)
            int X1;  // starting X coordinate of lightcycle (can be the same
                     // as X0 if you play before this player)
            int Y1;  // starting Y coordinate of lightcycle (can be the same
                     // as Y0 if you play before this player)
            cin >> X0 >> Y0 >> X1 >> Y1;
            cin.ignore();
            player::create(X0, Y0, X1, Y1, i + 1);
        }
        if (first) {
            first = 0;
        }

        // printMap();

        // Write an action using cout. DON'T FORGET THE "<< endl"
        // To debug: cerr << "Debug messages..." << endl;
        int val = numeric_limits<int>::min();
        string best_move = "";
        player& next = player::next(player::me());
        cerr << player::me().p << endl;
        for (auto& move : moves) {
            player::me().p += move.second;
            cerr << move.first << ": " << player::me().p;
            if (!player::me().p.empty()) {
                player::me().p -= move.second;
                cerr << " -> N/A" << endl;
                continue;
            }
            player::me().p.set(player::me().id);
            int res = AB(next, max_depth, numeric_limits<int>::min(),
                         numeric_limits<int>::max());
            cerr << " -> " << res << endl;
            player::me().p.clear();
            player::me().p -= move.second;
            if (res > val) {
                val = res;
                best_move = move.first;
            }
        }
        cout << best_move << endl;
        player::clear();
        cerr << heuristic_count << endl;
    }
}

