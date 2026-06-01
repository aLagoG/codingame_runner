// Fantastic Bits — v1.
//
// Line-by-line C++ translation of the original C# bot. Standalone:
// paste this file into CodinGame's web editor as-is, no headers from
// this repo. Strategy intentionally matches the C# original tic for
// tic — the comments below reproduce the original's TODOs and quirks
// (including any latent bugs) so future iterations can diff cleanly.
//
// Decision order each turn:
//   1. FLIPENDO on a snaffle whose line-of-fire (direct or one wall
//      bounce) lands in the opponent goal mouth.
//   2. ACCIO the last remaining snaffle when a wizard is on the
//      defending side of it.
//   3. PETRIFICUS a snaffle heading toward our goal that no wizard
//      can intercept in time.
//   4. Wizards still without a move: THROW if holding (compensated
//      for current velocity), otherwise MOVE to the closest snaffle.

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <iostream>
#include <optional>
#include <sstream>
#include <string>
#include <vector>

using std::int64_t;
using std::string;
using std::vector;

namespace {

struct Entity {
    int id = -1;
    // 0 == wizard, 1 == opponent wizard, 2 == snaffle, 3 == bludger.
    int type = -1;
    int x = 0;
    int y = 0;
    float vx = 0.0f;
    float vy = 0.0f;
    bool has_snaffle = false;
    float friction = 0.0f;
    float mass = 0.0f;
    int radius = 0;

    // Naive next-tick position (no friction, no collisions). Used by
    // the original C# bot to roughly predict where the snaffle will be
    // when a Flipendo lands, etc.
    int x2 = 0;
    int y2 = 0;

    // Empty string ⇒ "no move chosen yet". Output verbatim at end of turn.
    string move;
    // -1 ⇒ no target. Targets are referenced by id rather than pointer
    // so we don't have to worry about vector reallocation invalidating
    // pointers across the per-turn rebuilds.
    int target_id = -1;

    Entity() = default;

    Entity(int id, const string& type_str, int x, int y, int vx, int vy, int state)
        : id(id),
          x(x),
          y(y),
          vx(static_cast<float>(vx)),
          vy(static_cast<float>(vy)),
          has_snaffle(state == 1) {
        x2 = x + vx;
        y2 = y + vy;
        if (type_str == "WIZARD") {
            type = 0;
            mass = 1.0f;
            radius = 400;
            friction = 0.75f;
        } else if (type_str == "OPPONENT_WIZARD") {
            type = 1;
            mass = 1.0f;
            radius = 400;
            friction = 0.75f;
        } else if (type_str == "SNAFFLE") {
            type = 2;
            mass = 0.5f;
            radius = 150;
            friction = 0.75f;
        } else if (type_str == "BLUDGER") {
            type = 3;
            mass = 8.0f;
            radius = 200;
            friction = 0.75f;
        }
    }
};

int square_dist(const Entity& a, const Entity& b) {
    return (a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y);
}

int square_dist(const Entity& a, int x, int y) {
    return (a.x - x) * (a.x - x) + (a.y - y) * (a.y - y);
}

// findTarget in the C# original: aim point compensated for the
// thrower's current velocity (so by next tick we're firing roughly at
// (destX, destY)).
std::pair<int, int> find_target(const Entity& source, int dest_x, int dest_y) {
    return {static_cast<int>(dest_x - source.vx),
            static_cast<int>(dest_y - source.vy)};
}

// Locate a snaffle in the per-turn list by id; -1 if no such snaffle
// is currently alive on the field.
int index_of_snaffle(const vector<Entity>& snaffles, int id) {
    for (size_t i = 0; i < snaffles.size(); ++i) {
        if (snaffles[i].id == id) {
            return static_cast<int>(i);
        }
    }
    return -1;
}

string format_move(const string& cmd, int x, int y, int power) {
    std::ostringstream os;
    os << cmd << ' ' << x << ' ' << y << ' ' << power;
    return os.str();
}

string format_spell(const string& cmd, int target_id) {
    std::ostringstream os;
    os << cmd << ' ' << target_id;
    return os.str();
}

}  // namespace

int main() {
    std::ios_base::sync_with_stdio(false);

    // TODO: mejorar la simulacion de hechizos y movimiento — i.e.
    // tomar en cuenta friccion y eso.
    int my_team;
    std::cin >> my_team;
    // if 0 you need to score on the right of the map, if 1 the left.
    int other_team = my_team == 0 ? 1 : 0;
    int goals[2][2] = {{16000, 3750}, {0, 3750}};
    const int GOAL_SIZE = 4000 - 500;
    int petr = 0;

    while (true) {
        vector<Entity> wizards;
        vector<Entity> opponents;
        vector<Entity> snaffles;
        vector<Entity> bludgers;
        wizards.reserve(2);
        opponents.reserve(2);
        bludgers.reserve(2);

        int my_score, my_magic;
        std::cin >> my_score >> my_magic;
        int opponent_score, opponent_magic;
        std::cin >> opponent_score >> opponent_magic;
        int entity_count;
        std::cin >> entity_count;

        for (int i = 0; i < entity_count; ++i) {
            int id, x, y, vx, vy, state;
            string type;
            std::cin >> id >> type >> x >> y >> vx >> vy >> state;
            Entity tmp(id, type, x, y, vx, vy, state);
            switch (tmp.type) {
                case 0:
                    wizards.push_back(tmp);
                    break;
                case 1:
                    opponents.push_back(tmp);
                    break;
                case 2:
                    snaffles.push_back(tmp);
                    break;
                case 3:
                    bludgers.push_back(tmp);
                    break;
            }
        }

        // snafflesFromEnemyGoal: sorted by distance to the goal we
        // *score on* (goals[my_team]). Misnamed in the original but
        // kept faithful here.
        vector<Entity> snaffles_from_enemy_goal = snaffles;
        std::sort(snaffles_from_enemy_goal.begin(),
                  snaffles_from_enemy_goal.end(),
                  [&](const Entity& a, const Entity& b) {
                      return square_dist(a, goals[my_team][0], goals[my_team][1]) <
                             square_dist(b, goals[my_team][0], goals[my_team][1]);
                  });
        vector<Entity> snaffles_from_my_goal = snaffles;
        std::sort(snaffles_from_my_goal.begin(),
                  snaffles_from_my_goal.end(),
                  [&](const Entity& a, const Entity& b) {
                      return square_dist(a, goals[other_team][0], goals[other_team][1]) <
                             square_dist(b, goals[other_team][0], goals[other_team][1]);
                  });
        // TODO: maybe from each player.

        // ---- FLIPENDO (cost 20) ----
        if (my_magic >= 20) {
            // TODO: basic rebound on top and bottom and don't waste
            // Flipendo if it isn't going to do much (too far away); if
            // an enemy has it, calculate as if velocity was max
            // toward the center of my goal.
            for (const auto& sn : snaffles_from_enemy_goal) {
                // Sort wizards by distance to this snaffle; skip ones
                // that already chose a move this turn.
                vector<Entity*> wizards_by_distance;
                for (auto& w : wizards) {
                    wizards_by_distance.push_back(&w);
                }
                std::sort(wizards_by_distance.begin(),
                          wizards_by_distance.end(),
                          [&](Entity* a, Entity* b) {
                              return square_dist(*a, sn) < square_dist(*b, sn);
                          });

                for (Entity* wiz : wizards_by_distance) {
                    if (!wiz->move.empty()) {
                        continue;
                    }
                    float dx = static_cast<float>(sn.x2 - wiz->x2);
                    if (dx == 0.0f || dx > 5000.0f ||
                        (wiz->has_snaffle && wiz->x == sn.x && wiz->y == sn.y)) {
                        continue;
                    }
                    float dy = static_cast<float>(sn.y2 - wiz->y2);
                    bool pushing_toward_my_goal =
                        (dx > 0 && my_team == 0) || (dx < 0 && my_team == 1);
                    if (!pushing_toward_my_goal) {
                        continue;
                    }
                    float goal_dx = goals[my_team][0] - sn.x2;
                    float slope = dy / dx;
                    float dest_y = sn.y2 + slope * goal_dx;
                    if (std::fabs(dest_y - goals[my_team][1]) < GOAL_SIZE / 2.0f) {
                        wiz->move = format_spell("FLIPENDO", sn.id);
                        wiz->target_id = sn.id;
                        break;
                    } else if (dest_y > 7500.0f || dest_y < 0.0f) {
                        dest_y = dest_y > 0 ? 7350.0f : 200.0f;
                        // NOTE: the C# original's bounce X formula is
                        // ported verbatim. It does not derive from
                        // similar triangles cleanly; faithfulness wins
                        // over fixing it here.
                        float dest_x = dy * goal_dx / (dest_y - sn.y2) + sn.x2;
                        goal_dx = goals[my_team][0] - dest_x;
                        slope = -dy / dx;
                        dest_y = dest_y + slope * goal_dx;
                        if (std::fabs(dest_y - goals[my_team][1]) < GOAL_SIZE / 2.0f) {
                            wiz->move = format_spell("FLIPENDO", sn.id);
                            wiz->target_id = sn.id;
                            break;
                        }
                    }
                }
            }
            // If both wizards picked a Flipendo, the lower-priority one
            // (further from the goal) backs off when we can't afford
            // two casts (40 magic).
            if (wizards[0].target_id != -1 && wizards[1].target_id != -1) {
                if (my_magic < 40) {
                    int idx0 = index_of_snaffle(snaffles_from_enemy_goal,
                                                wizards[0].target_id);
                    int idx1 = index_of_snaffle(snaffles_from_enemy_goal,
                                                wizards[1].target_id);
                    if (idx0 > idx1) {
                        wizards[0].target_id = -1;
                        wizards[0].move.clear();
                    } else {
                        wizards[1].target_id = -1;
                        wizards[1].move.clear();
                    }
                    my_magic -= 20;
                } else {
                    my_magic -= 40;
                }
            } else if (wizards[0].target_id != -1 || wizards[1].target_id != -1) {
                my_magic -= 20;
            }
        }

        // ---- ACCIO (cost 15) ----
        if (my_magic >= 15) {
            // TODO: algo mas general.
            if (snaffles.size() == 1) {
                const Entity& sn = snaffles[0];
                vector<Entity*> wizs;
                for (auto& w : wizards) {
                    wizs.push_back(&w);
                }
                std::sort(wizs.begin(), wizs.end(),
                          [&](Entity* a, Entity* b) {
                              return square_dist(*a, sn) < square_dist(*b, sn);
                          });
                for (Entity* wiz : wizs) {
                    if (wiz->has_snaffle) {
                        continue;
                    }
                    float dx = static_cast<float>(sn.x - wiz->x2);
                    bool defending_side =
                        (dx <= 0 && my_team == 0) || (dx >= 0 && my_team == 1);
                    if (defending_side) {
                        wiz->move = format_spell("ACCIO", sn.id);
                        wiz->target_id = sn.id;
                        my_magic -= 15;
                        break;
                    }
                }
            }
        }

        // ---- PETRIFICUS (cost 10) ----
        // Two-tick guard: once we cast Petrificus, skip the next
        // turn's Petrificus check so we don't immediately re-petrify
        // the same snaffle that's now frozen.
        petr = petr == 1 ? 2 : 0;
        if (my_magic >= 10) {
            bool any_idle = false;
            for (const auto& w : wizards) {
                if (w.move.empty()) {
                    any_idle = true;
                    break;
                }
            }
            if (any_idle) {
                for (const auto& sn : snaffles_from_my_goal) {
                    // Only worry about snaffles heading toward us.
                    if ((sn.vx > 0 && my_team == 0) ||
                        (sn.vx < 0 && my_team == 1)) {
                        continue;
                    }
                    // Skip if a wizard is already in interception range.
                    bool interceptable = false;
                    for (const auto& w : wizards) {
                        if (std::fabs(w.x + w.vx - sn.x) < 400 &&
                            std::fabs(w.y + w.vy - sn.y) < 400) {
                            interceptable = true;
                            break;
                        }
                    }
                    if (interceptable) {
                        continue;
                    }
                    // Project 5 ticks ahead (clamped to remaining
                    // distance to the goal line).
                    float goal_dx =
                        std::fabs(static_cast<float>(goals[other_team][0] - sn.x)) >
                                std::fabs(sn.vx * 5.0f)
                            ? sn.vx * 5.0f
                            : static_cast<float>(goals[other_team][0] - sn.x);
                    float slope = sn.vy / sn.vx;
                    float dest_y = sn.y + slope * goal_dx;
                    float dest_x = sn.x + goal_dx;
                    bool past_our_goal_line =
                        (dest_x <= 0 && my_team == 0) ||
                        (dest_x >= goals[other_team][0] && my_team == 1);
                    if (petr == 0 && past_our_goal_line &&
                        std::fabs(goal_dx) > std::fabs(sn.vx * 2.0f) &&
                        std::fabs(dest_y - goals[other_team][1]) < GOAL_SIZE / 2.0f) {
                        // Pick the idle wizard closest to our own goal.
                        Entity* chosen = nullptr;
                        int best = std::numeric_limits<int>::max();
                        for (auto& w : wizards) {
                            if (!w.move.empty()) {
                                continue;
                            }
                            int d = square_dist(w, goals[other_team][0],
                                                goals[other_team][1]);
                            if (d < best) {
                                best = d;
                                chosen = &w;
                            }
                        }
                        if (chosen != nullptr) {
                            chosen->move = format_spell("PETRIFICUS", sn.id);
                            chosen->target_id = sn.id;
                            petr = 1;
                            break;
                        }
                    }
                }
            }
        }

        // ---- OBLIVIATE (cost 5) ----
        // TODO: not yet implemented in the original C# bot — left as a
        // hook so future iterations have a place to slot it.
        // if (my_magic >= 5) { ... }

        // ---- Default actions: THROW if holding, else MOVE ----
        for (auto& wiz : wizards) {
            if (!wiz.move.empty() || !wiz.has_snaffle) {
                continue;
            }
            auto [tx, ty] = find_target(wiz, goals[my_team][0], goals[my_team][1]);
            wiz.move = format_move("THROW", tx, ty, 500);
        }

        // For idle, empty-handed wizards: pick the closest snaffle each.
        // If both pick the same one, the further wizard takes the
        // second-closest snaffle (when available).
        struct WizPlan {
            Entity* wiz;
            vector<Entity*> snaffles;  // up to 2 closest, by distance
        };
        vector<WizPlan> snaffles_from_wizards;
        for (auto& wiz : wizards) {
            if (!wiz.move.empty() || wiz.has_snaffle) {
                continue;
            }
            vector<Entity*> sorted;
            for (auto& s : snaffles) {
                sorted.push_back(&s);
            }
            std::sort(sorted.begin(), sorted.end(),
                      [&](Entity* a, Entity* b) {
                          return square_dist(*a, wiz) < square_dist(*b, wiz);
                      });
            WizPlan plan;
            plan.wiz = &wiz;
            for (size_t i = 0; i < sorted.size() && i < 2; ++i) {
                plan.snaffles.push_back(sorted[i]);
            }
            snaffles_from_wizards.push_back(std::move(plan));
        }

        if (snaffles_from_wizards.size() == 2 && snaffles.size() > 1 &&
            snaffles_from_wizards[0].snaffles[0] ==
                snaffles_from_wizards[1].snaffles[0]) {
            // Tie-break: closer wizard keeps the contested snaffle,
            // further wizard goes for their second-closest.
            Entity* contested = snaffles_from_wizards[0].snaffles[0];
            if (square_dist(wizards[0], *contested) <=
                square_dist(wizards[1], *contested)) {
                snaffles_from_wizards[1].snaffles.erase(
                    snaffles_from_wizards[1].snaffles.begin());
            } else {
                snaffles_from_wizards[0].snaffles.erase(
                    snaffles_from_wizards[0].snaffles.begin());
            }
        }
        for (auto& plan : snaffles_from_wizards) {
            if (plan.snaffles.empty()) {
                continue;
            }
            const Entity* sn = plan.snaffles[0];
            auto [tx, ty] = find_target(*plan.wiz, sn->x, sn->y);
            plan.wiz->move = format_move("MOVE", tx, ty, 150);
            plan.wiz->target_id = sn->id;
        }

        std::cout << wizards[0].move << '\n';
        std::cout << wizards[1].move << std::endl;
    }
}
