// Fantastic Bits — v1.5 strategy.
//
// Single source of truth for the bot's per-turn logic, shared by:
//   * `bot.cpp` — FFI wrapper consumed by the runner / tournament.
//   * `main.cpp` — subprocess stdio loop AND the file that
//     `cpp_flatten` bundles for CodinGame submission (cpp_flatten
//     inlines this header into the bundled output).
//
// Started as a line-by-line port of the original C# v1 bot. Diffs
// from the C# original (preserve when porting further fixes):
//   * Two corrected Flipendo physics bugs noted at the call site
//     (symmetric wall radii + correct bounce x-formula). [from v1]
//   * Post-aware Flipendo: a cast is rejected unless the snaffle's
//     trajectory clears both goal posts by at least
//     `SNAFFLE_RADIUS + POLE_RADIUS + POST_PAD` (perpendicular
//     distance from the trajectory line to each post). v1 only
//     checked the y-crossing inside a tight goal-mouth window, which
//     accepted shots that grazed a post and bounced off. [new in v1.5]
//
// Decision order each turn:
//   1. FLIPENDO on a snaffle whose line-of-fire (direct or one wall
//      bounce) lands in the opponent's goal mouth.
//   2. ACCIO the last remaining snaffle when a wizard is on the
//      defending side of it.
//   3. PETRIFICUS a snaffle heading toward our goal that no wizard
//      can intercept in time.
//   4. Wizards still without a move: THROW if holding, else MOVE to
//      the closest snaffle (with simple anti-collision).

#pragma once

#include "../../defs/include/fantastic_bits_defs_io.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <limits>
#include <optional>
#include <utility>
#include <vector>

namespace fantastic_bits_v1_cpp {

// Bot-wide state, set by `on_init` (called once at match start) and
// read by `decide` every tick. Owned by the strategy so both bot.cpp
// and main.cpp see the same values without re-declaring globals.
inline int32_t g_my_team_id = 0;
inline int g_petr = 0;  // two-tick Petrificus guard, preserved across turns.

inline void on_init(const cgio::InitialInputRef& init) {
    g_my_team_id = init.my_team_id;
    g_petr = 0;
}

// Internal mirror of the v1 `Entity` struct so the strategy code below
// stays a line-for-line port of v1. Built from the FFI `Entity` at the
// top of `decide`.
struct LocalEntity {
    int id = -1;
    int type = -1;  // 0=wizard 1=opp 2=snaffle 3=bludger
    int x = 0;
    int y = 0;
    float vx = 0.0f;
    float vy = 0.0f;
    bool has_snaffle = false;
    int radius = 0;
    int x2 = 0;
    int y2 = 0;
    std::optional<WizardAction> action;
    int target_id = -1;
};

inline LocalEntity from_ffi(const Entity& e) {
    LocalEntity le;
    le.id = e.id;
    le.x = e.x;
    le.y = e.y;
    le.vx = static_cast<float>(e.vx);
    le.vy = static_cast<float>(e.vy);
    le.has_snaffle = e.state == 1;
    le.x2 = e.x + e.vx;
    le.y2 = e.y + e.vy;
    switch (e.kind) {
        case EntityKind::Wizard:
            le.type = 0;
            le.radius = 400;
            break;
        case EntityKind::OpponentWizard:
            le.type = 1;
            le.radius = 400;
            break;
        case EntityKind::Snaffle:
            le.type = 2;
            le.radius = 150;
            break;
        case EntityKind::Bludger:
            le.type = 3;
            le.radius = 200;
            break;
    }
    return le;
}

inline int square_dist(const LocalEntity& a, const LocalEntity& b) {
    return (a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y);
}

inline int square_dist(const LocalEntity& a, int x, int y) {
    return (a.x - x) * (a.x - x) + (a.y - y) * (a.y - y);
}

inline std::pair<int, int> find_target(const LocalEntity& source, int dest_x, int dest_y) {
    return {static_cast<int>(dest_x - source.vx),
            static_cast<int>(dest_y - source.vy)};
}

inline int index_of_snaffle(const std::vector<LocalEntity>& snaffles, int id) {
    for (size_t i = 0; i < snaffles.size(); ++i) {
        if (snaffles[i].id == id) {
            return static_cast<int>(i);
        }
    }
    return -1;
}

inline WizardAction make_move(int x, int y, int power) {
    return WizardAction{ActionKind::Move, x, y, power, 0};
}

inline WizardAction make_throw(int x, int y, int power) {
    return WizardAction{ActionKind::Throw, x, y, power, 0};
}

inline WizardAction make_spell(ActionKind kind, int target_id) {
    return WizardAction{kind, 0, 0, 0, target_id};
}

// --- Post-aware Flipendo geometry (v1.5) ---
// Mirrors the engine: goal posts are solid circular bumpers at the
// four mouth corners (radius `POLE_RADIUS`); snaffles collide with a
// post when their centres come within `SNAFFLE_RADIUS + POLE_RADIUS`.
// The Flipendo check below rejects any cast whose trajectory line
// passes closer to a target-side post than that distance plus a
// small safety pad. Constants kept in sync with the Rust engine
// (`fantastic_bits_game::lib.rs`).
inline constexpr int   POLE_RADIUS    = 300;
inline constexpr int   SNAFFLE_RADIUS = 150;
inline constexpr int   POST_PAD       = 60;   // safety wiggle room
inline constexpr int   GOAL_Y_TOP     = 1750;
inline constexpr int   GOAL_Y_BOTTOM  = 5750;
inline constexpr float POST_CLEAR     =
    static_cast<float>(SNAFFLE_RADIUS + POLE_RADIUS + POST_PAD);

// Perpendicular distance from the LINE through (sx, sy) with
// direction (dx, dy) to point (px, py). Returns +inf for a
// degenerate direction so callers can compare against `POST_CLEAR`
// without special-casing.
inline float perp_dist(float sx, float sy, float dx, float dy, float px, float py) {
    const float len = std::hypot(dx, dy);
    if (len < 1e-3f) return std::numeric_limits<float>::infinity();
    return std::fabs(dx * (py - sy) - dy * (px - sx)) / len;
}

// True iff the trajectory line through (sx, sy) with direction
// (dx, dy) clears BOTH posts of the goal at x = `goal_x`. Used by
// both Flipendo branches (direct + post-bounce) — the bounce branch
// passes the reflected segment's origin and slope.
inline bool flipendo_clears_posts(float sx, float sy, float dx, float dy, int goal_x) {
    const float gx = static_cast<float>(goal_x);
    return perp_dist(sx, sy, dx, dy, gx, GOAL_Y_TOP)    > POST_CLEAR
        && perp_dist(sx, sy, dx, dy, gx, GOAL_Y_BOTTOM) > POST_CLEAR;
}

// The strategy. Pure: takes the parsed entity lists + score/magic +
// stateful guards (my_team, petr), returns the two-wizard output.
inline TurnOutput decide_from_entities(std::vector<LocalEntity>& wizards,
                                       std::vector<LocalEntity>& /*opponents*/,
                                       std::vector<LocalEntity>& snaffles,
                                       std::vector<LocalEntity>& /*bludgers*/,
                                       int my_magic) {
    const int my_team = g_my_team_id;
    const int other_team = my_team == 0 ? 1 : 0;
    const int goals[2][2] = {{16000, 3750}, {0, 3750}};
    const int GOAL_SIZE = 4000 - 500;

    // snafflesFromEnemyGoal: sorted by distance to the goal we score on.
    std::vector<LocalEntity> snaffles_from_enemy_goal = snaffles;
    std::sort(snaffles_from_enemy_goal.begin(),
              snaffles_from_enemy_goal.end(),
              [&](const LocalEntity& a, const LocalEntity& b) {
                  return square_dist(a, goals[my_team][0], goals[my_team][1]) <
                         square_dist(b, goals[my_team][0], goals[my_team][1]);
              });
    std::vector<LocalEntity> snaffles_from_my_goal = snaffles;
    std::sort(snaffles_from_my_goal.begin(),
              snaffles_from_my_goal.end(),
              [&](const LocalEntity& a, const LocalEntity& b) {
                  return square_dist(a, goals[other_team][0], goals[other_team][1]) <
                         square_dist(b, goals[other_team][0], goals[other_team][1]);
              });

    // ---- FLIPENDO (cost 20) ----
    if (my_magic >= 20) {
        for (const auto& sn : snaffles_from_enemy_goal) {
            std::vector<LocalEntity*> wizards_by_distance;
            for (auto& w : wizards) {
                wizards_by_distance.push_back(&w);
            }
            std::sort(wizards_by_distance.begin(),
                      wizards_by_distance.end(),
                      [&](LocalEntity* a, LocalEntity* b) {
                          return square_dist(*a, sn) < square_dist(*b, sn);
                      });

            for (LocalEntity* wiz : wizards_by_distance) {
                if (wiz->action.has_value()) {
                    continue;
                }
                float dx = static_cast<float>(sn.x2 - wiz->x2);
                if (dx == 0.0f || dx > 5000.0f ||
                    (wiz->has_snaffle && wiz->x == sn.x && wiz->y == sn.y)) {
                    continue;
                }
                float dy = static_cast<float>(sn.y2 - wiz->y2);
                bool pushing_toward_enemy_goal =
                    (dx > 0 && my_team == 0) || (dx < 0 && my_team == 1);
                if (!pushing_toward_enemy_goal) {
                    continue;
                }
                float goal_dx = goals[my_team][0] - sn.x2;
                float slope = dy / dx;
                float dest_y = sn.y2 + slope * goal_dx;
                // v1.5: replace v1's tight `|dest_y - goal_y| < 1750`
                // check (which let through shots grazing a post) with
                // "trajectory crosses inside the mouth AND clears
                // both posts by `POST_CLEAR`". Same intent, but the
                // perp-distance check catches steep angles where v1's
                // y-only check was wrong.
                const bool in_mouth =
                    dest_y > GOAL_Y_TOP && dest_y < GOAL_Y_BOTTOM;
                if (in_mouth &&
                    flipendo_clears_posts(sn.x2, sn.y2, dx, dy,
                                          goals[my_team][0])) {
                    wiz->action = make_spell(ActionKind::Flipendo, sn.id);
                    wiz->target_id = sn.id;
                    break;
                } else if (dest_y > 7500.0f || dest_y < 0.0f) {
                    // Snaffle radius is 150 → centre rebounds 150 in
                    // from each wall. v1 used 200 for the top wall,
                    // which is asymmetric with the (correct) 7350 for
                    // the bottom; this fix makes them symmetric.
                    dest_y = dest_y > 0 ? 7350.0f : 150.0f;
                    // Avoid dividing by zero on a near-horizontal
                    // trajectory — those won't hit the wall in a
                    // useful way for a bounce-into-goal check.
                    if (std::fabs(dy) < 0.001f) {
                        continue;
                    }
                    // Correct wall-x: solve y = sn.y2 + (dy/dx)(x − sn.x2)
                    // for x at y = dest_y → x = sn.x2 + (dest_y − sn.y2)·dx/dy.
                    // v1's version flipped the ratio AND used goal_dx
                    // where it should have used dx, so it both fired
                    // some no-score Flipendos and skipped some real ones.
                    float dest_x = sn.x2 + (dest_y - sn.y2) * dx / dy;
                    goal_dx = goals[my_team][0] - dest_x;
                    slope = -dy / dx;
                    const float seg_b_sx = dest_x;
                    const float seg_b_sy = dest_y;
                    const float seg_b_dx = dx;          // x-direction preserved
                    const float seg_b_dy = -dy;         // y-direction reflected by the wall
                    dest_y = dest_y + slope * goal_dx;
                    // v1.5: same post-clear check as the direct branch,
                    // applied to the post-bounce segment B (from the
                    // wall-bounce point with the reflected slope).
                    const bool in_mouth =
                        dest_y > GOAL_Y_TOP && dest_y < GOAL_Y_BOTTOM;
                    if (in_mouth &&
                        flipendo_clears_posts(seg_b_sx, seg_b_sy,
                                              seg_b_dx, seg_b_dy,
                                              goals[my_team][0])) {
                        wiz->action = make_spell(ActionKind::Flipendo, sn.id);
                        wiz->target_id = sn.id;
                        break;
                    }
                }
            }
        }
        // If both wizards picked a Flipendo, the lower-priority one
        // (further from the goal) backs off when we can't afford two
        // casts (40 magic).
        if (wizards[0].target_id != -1 && wizards[1].target_id != -1) {
            if (my_magic < 40) {
                int idx0 = index_of_snaffle(snaffles_from_enemy_goal,
                                            wizards[0].target_id);
                int idx1 = index_of_snaffle(snaffles_from_enemy_goal,
                                            wizards[1].target_id);
                if (idx0 > idx1) {
                    wizards[0].target_id = -1;
                    wizards[0].action.reset();
                } else {
                    wizards[1].target_id = -1;
                    wizards[1].action.reset();
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
        if (snaffles.size() == 1) {
            const LocalEntity& sn = snaffles[0];
            std::vector<LocalEntity*> wizs;
            for (auto& w : wizards) {
                wizs.push_back(&w);
            }
            std::sort(wizs.begin(), wizs.end(),
                      [&](LocalEntity* a, LocalEntity* b) {
                          return square_dist(*a, sn) < square_dist(*b, sn);
                      });
            for (LocalEntity* wiz : wizs) {
                if (wiz->has_snaffle) {
                    continue;
                }
                float dx = static_cast<float>(sn.x - wiz->x2);
                bool defending_side =
                    (dx <= 0 && my_team == 0) || (dx >= 0 && my_team == 1);
                if (defending_side) {
                    wiz->action = make_spell(ActionKind::Accio, sn.id);
                    wiz->target_id = sn.id;
                    my_magic -= 15;
                    break;
                }
            }
        }
    }

    // ---- PETRIFICUS (cost 10) ----
    g_petr = g_petr == 1 ? 2 : 0;
    if (my_magic >= 10) {
        bool any_idle = false;
        for (const auto& w : wizards) {
            if (!w.action.has_value()) {
                any_idle = true;
                break;
            }
        }
        if (any_idle) {
            for (const auto& sn : snaffles_from_my_goal) {
                if ((sn.vx > 0 && my_team == 0) ||
                    (sn.vx < 0 && my_team == 1)) {
                    continue;
                }
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
                if (g_petr == 0 && past_our_goal_line &&
                    std::fabs(goal_dx) > std::fabs(sn.vx * 2.0f) &&
                    std::fabs(dest_y - goals[other_team][1]) < GOAL_SIZE / 2.0f) {
                    LocalEntity* chosen = nullptr;
                    int best = std::numeric_limits<int>::max();
                    for (auto& w : wizards) {
                        if (w.action.has_value()) {
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
                        chosen->action = make_spell(ActionKind::Petrificus, sn.id);
                        chosen->target_id = sn.id;
                        g_petr = 1;
                        break;
                    }
                }
            }
        }
    }

    // ---- OBLIVIATE: not yet implemented in the v1 strategy. ----

    // ---- Default: THROW if holding, else MOVE ----
    for (auto& wiz : wizards) {
        if (wiz.action.has_value() || !wiz.has_snaffle) {
            continue;
        }
        auto [tx, ty] = find_target(wiz, goals[my_team][0], goals[my_team][1]);
        wiz.action = make_throw(tx, ty, 500);
    }

    // Idle, empty-handed wizards pick their closest snaffle; if they
    // collide on the same one, the further wizard takes second-closest.
    struct WizPlan {
        LocalEntity* wiz;
        std::vector<LocalEntity*> snaffles;
    };
    std::vector<WizPlan> snaffles_from_wizards;
    for (auto& wiz : wizards) {
        if (wiz.action.has_value() || wiz.has_snaffle) {
            continue;
        }
        std::vector<LocalEntity*> sorted;
        for (auto& s : snaffles) {
            sorted.push_back(&s);
        }
        std::sort(sorted.begin(), sorted.end(),
                  [&](LocalEntity* a, LocalEntity* b) {
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
        LocalEntity* contested = snaffles_from_wizards[0].snaffles[0];
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
        const LocalEntity* sn = plan.snaffles[0];
        auto [tx, ty] = find_target(*plan.wiz, sn->x, sn->y);
        plan.wiz->action = make_move(tx, ty, 150);
        plan.wiz->target_id = sn->id;
    }

    // Fallback for any wizard that still ended up without an action
    // (e.g. no snaffles left on the field): idle MOVE to mid-court.
    for (auto& wiz : wizards) {
        if (!wiz.action.has_value()) {
            wiz.action = make_move(16000 / 2, 3750, 0);
        }
    }

    return TurnOutput{*wizards[0].action, *wizards[1].action};
}

// Single bot entry point — both `bot.cpp` (FFI) and `main.cpp`
// (subprocess stdio) call this. Builds the per-kind entity vectors
// from `turn.entities` then delegates to `decide_from_entities`.
inline TurnOutput decide(const cgio::TurnRef& turn) {
    std::vector<LocalEntity> wizards;
    std::vector<LocalEntity> opponents;
    std::vector<LocalEntity> snaffles;
    std::vector<LocalEntity> bludgers;
    wizards.reserve(2);
    opponents.reserve(2);
    bludgers.reserve(2);

    for (const auto& e : turn.entities) {
        LocalEntity le = from_ffi(e);
        switch (le.type) {
            case 0: wizards.push_back(le); break;
            case 1: opponents.push_back(le); break;
            case 2: snaffles.push_back(le); break;
            case 3: bludgers.push_back(le); break;
        }
    }

    return decide_from_entities(wizards, opponents, snaffles, bludgers, turn.my_magic);
}

}  // namespace fantastic_bits_v1_cpp
