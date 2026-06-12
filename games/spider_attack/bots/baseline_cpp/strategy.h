// spider_attack bot strategy — baseline.
//
// Mirror of the Rust baseline in `games/spider_attack/bots/baseline_rs/src/lib.rs`:
// each hero walks toward the nearest visible threat (a monster either
// targeting our base or inside our 6000-unit base radius), or to a
// fixed guard post when no threats are visible. No spells.

#pragma once

#include "../../defs/include/spider_attack_defs_io.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <vector>

namespace spider_attack_baseline_cpp {

namespace {

constexpr int32_t WIDTH = 17630;
constexpr int32_t HEIGHT = 9000;
constexpr int64_t BASE_VISION = 6000;

struct State {
    int32_t base_x = 0;
    int32_t base_y = 0;
};

inline State& state() {
    static State s;
    return s;
}

inline int64_t sq_dist(int32_t ax, int32_t ay, int32_t bx, int32_t by) {
    int64_t dx = ax - bx;
    int64_t dy = ay - by;
    return dx * dx + dy * dy;
}

inline size_t hero_slot_for(int32_t id) {
    int32_t off = (state().base_x == 0) ? 0 : 3;
    int32_t s = id - off;
    if (s < 0) s = 0;
    if (s > 2) s = 2;
    return static_cast<size_t>(s);
}

}  // namespace

inline void on_init(const cgio::InitialInput& init) {
    state().base_x = init.base_x;
    state().base_y = init.base_y;
}

inline TurnOutput decide(const cgio::TurnInput& turn) {
    auto& st = state();
    int32_t dir_x = (st.base_x == 0) ? 1 : -1;
    int32_t dir_y = (st.base_y == 0) ? 1 : -1;
    std::array<std::pair<int32_t, int32_t>, 3> guard_posts{
        std::make_pair(st.base_x + dir_x * 5000, st.base_y + dir_y * 1500),
        std::make_pair(st.base_x + dir_x * 3500, st.base_y + dir_y * 3500),
        std::make_pair(st.base_x + dir_x * 1500, st.base_y + dir_y * 5000),
    };

    std::vector<const Entity*> heroes;
    std::vector<const Entity*> threats;
    for (const auto& e : turn.entities) {
        if (e.kind == EntityKind::MyHero) {
            heroes.push_back(&e);
        } else if (e.kind == EntityKind::Monster) {
            // Threats: anything targeting our base OR within base vision.
            bool inside = sq_dist(e.x, e.y, st.base_x, st.base_y)
                          <= BASE_VISION * BASE_VISION;
            if (e.threat_for == 1 || inside) {
                threats.push_back(&e);
            }
        }
    }
    std::sort(threats.begin(), threats.end(),
              [&](const Entity* a, const Entity* b) {
                  return sq_dist(a->x, a->y, st.base_x, st.base_y)
                       < sq_dist(b->x, b->y, st.base_x, st.base_y);
              });

    TurnOutput out{};
    // Default everything to WAIT so missing heroes don't emit garbage.
    for (auto& a : out.actions) {
        a.kind = ActionKind::Wait;
        a.x = 0;
        a.y = 0;
        a.entity_id = 0;
    }

    size_t n_heroes = std::min<size_t>(heroes.size(), 3);
    for (size_t i = 0; i < n_heroes; ++i) {
        const Entity* hero = heroes[i];
        std::pair<int32_t, int32_t> tgt;
        if (i < threats.size()) {
            tgt = {threats[i]->x, threats[i]->y};
        } else {
            tgt = guard_posts[std::min<size_t>(i, 2)];
        }
        int32_t tx = std::clamp(tgt.first, 0, WIDTH);
        int32_t ty = std::clamp(tgt.second, 0, HEIGHT);
        size_t slot = hero_slot_for(hero->id);
        out.actions[slot].kind = ActionKind::Move;
        out.actions[slot].x = tx;
        out.actions[slot].y = ty;
    }
    return out;
}

}  // namespace spider_attack_baseline_cpp
