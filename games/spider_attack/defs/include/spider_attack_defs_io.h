// Stdio wire-format helpers for spider_attack. Paired with `spider_attack_defs.h`
// (the type definitions). Stays in sync with the Rust `ReadFrom` /
// `WriteTo` impls in `spider_attack_defs/src/lib.rs` by hand; if you
// change the format in one place, change the other.
//
// Wire format:
//
//   <base_x> <base_y>                                     ← initial input
//   <heroes_per_player>
//
//   <my_health> <my_mana>                                 ← per-turn header
//   <opp_health> <opp_mana>
//   <entity_count>
//   <id> <type> <x> <y> <shield_life> <is_controlled>    ← N entity lines
//        <health> <vx> <vy> <near_base> <threat_for>
//   ...
//
// Output: 3 lines, one per hero, in hero-id order. Each is one of:
//
//   WAIT
//   MOVE <x> <y>
//   SPELL WIND <x> <y>
//   SPELL SHIELD <entity_id>
//   SPELL CONTROL <entity_id> <x> <y>

#pragma once

#include "spider_attack_defs.h"

#include <cstdint>
#include <iostream>
#include <vector>

namespace cgio {

// ---- InitialInput ----

struct InitialInput {
    int32_t base_x;
    int32_t base_y;
    int32_t heroes_per_player;
};

inline std::istream& operator>>(std::istream& in, InitialInput& v) {
    return in >> v.base_x >> v.base_y >> v.heroes_per_player;
}

// ---- Entity ----

inline std::istream& operator>>(std::istream& in, Entity& e) {
    int32_t kind_raw = 0;
    if (!(in >> e.id >> kind_raw >> e.x >> e.y >> e.shield_life
                 >> e.is_controlled >> e.health >> e.vx >> e.vy
                 >> e.near_base >> e.threat_for)) {
        return in;
    }
    e.kind = static_cast<EntityKind>(kind_raw);
    return in;
}

// ---- TurnInput ----

struct TurnInput {
    int32_t             my_health;
    int32_t             my_mana;
    int32_t             opp_health;
    int32_t             opp_mana;
    std::vector<Entity> entities;
};

inline std::istream& operator>>(std::istream& in, TurnInput& v) {
    if (!(in >> v.my_health >> v.my_mana)) return in;
    if (!(in >> v.opp_health >> v.opp_mana)) return in;
    int32_t n = 0;
    if (!(in >> n)) return in;
    if (n < 0) {
        in.setstate(std::ios::failbit);
        return in;
    }
    v.entities.resize(static_cast<size_t>(n));
    for (auto& e : v.entities) {
        if (!(in >> e)) return in;
    }
    return in;
}

// ---- HeroAction / TurnOutput ----

inline std::ostream& operator<<(std::ostream& out, const HeroAction& a) {
    switch (a.kind) {
        case ActionKind::Wait:    return out << "WAIT";
        case ActionKind::Move:    return out << "MOVE " << a.x << ' ' << a.y;
        case ActionKind::Wind:    return out << "SPELL WIND " << a.x << ' ' << a.y;
        case ActionKind::Shield:  return out << "SPELL SHIELD " << a.entity_id;
        case ActionKind::Control:
            return out << "SPELL CONTROL " << a.entity_id << ' ' << a.x << ' ' << a.y;
    }
    return out;
}

inline std::ostream& operator<<(std::ostream& out, const TurnOutput& o) {
    return out << o.actions[0] << '\n' << o.actions[1] << '\n' << o.actions[2];
}

}  // namespace cgio
