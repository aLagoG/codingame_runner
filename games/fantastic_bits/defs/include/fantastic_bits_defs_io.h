// Hand-written stdio wire-format helpers for fantastic_bits — paired
// with the cbindgen-generated `fantastic_bits_defs.h`. Stays in sync
// with the Rust impls in `fantastic_bits_defs/src/lib.rs` by hand; if
// you change the format in one place, change the other.
//
// Wire format (matches the Rust `Display`/`FromStr` impls):
//
//   <my_team_id>                                          ← initial input (once)
//
//   <my_score> <my_magic>                                 ← per-turn header
//   <opp_score> <opp_magic>
//   <num_entities>
//   <id> <KIND> <x> <y> <vx> <vy> <state>                 ← N entity lines
//   ...
//
// Output:
//
//   <action_line>                                         ← primary wizard
//   <action_line>                                         ← secondary wizard
//
// where each action_line is one of:
//
//   MOVE <x> <y> <thrust>
//   THROW <x> <y> <power>
//   OBLIVIATE <id>
//   PETRIFICUS <id>
//   ACCIO <id>
//   FLIPENDO <id>
//
// See `docs/wire-codegen.md` for the plan to autogenerate this header
// from a shared schema once the count of games justifies it.

#pragma once

#include "fantastic_bits_defs.h"

#include <cstdint>
#include <iostream>
#include <span>
#include <string>
#include <vector>

namespace cgio {

// ---- InitialInput ----

struct InitialInput {
    int32_t my_team_id;
};

struct InitialInputRef {
    int32_t my_team_id;
};

inline InitialInputRef as_ref(const InitialInput& v) {
    return InitialInputRef{v.my_team_id};
}

inline InitialInputRef as_ref(const ::InitialInputFFI& ffi) {
    return InitialInputRef{ffi.my_team_id};
}

inline std::istream& operator>>(std::istream& in, InitialInput& v) {
    return in >> v.my_team_id;
}

// ---- EntityKind ----

inline const char* to_str(EntityKind k) {
    switch (k) {
        case EntityKind::Wizard:         return "WIZARD";
        case EntityKind::OpponentWizard: return "OPPONENT_WIZARD";
        case EntityKind::Snaffle:        return "SNAFFLE";
        case EntityKind::Bludger:        return "BLUDGER";
    }
    return "?";
}

inline bool from_str(const std::string& s, EntityKind& out) {
    if (s == "WIZARD")          { out = EntityKind::Wizard;         return true; }
    if (s == "OPPONENT_WIZARD") { out = EntityKind::OpponentWizard; return true; }
    if (s == "SNAFFLE")         { out = EntityKind::Snaffle;        return true; }
    if (s == "BLUDGER")         { out = EntityKind::Bludger;        return true; }
    return false;
}

inline std::istream& operator>>(std::istream& in, EntityKind& k) {
    std::string tok;
    if (!(in >> tok)) return in;
    if (!from_str(tok, k)) in.setstate(std::ios::failbit);
    return in;
}

// ---- Entity (one row of the per-tick list) ----

inline std::istream& operator>>(std::istream& in, Entity& e) {
    return in >> e.id >> e.kind >> e.x >> e.y >> e.vx >> e.vy >> e.state;
}

// ---- ActionKind / WizardAction (output) ----

inline std::ostream& operator<<(std::ostream& out, const WizardAction& a) {
    switch (a.kind) {
        case ActionKind::Move:
            return out << "MOVE " << a.x << ' ' << a.y << ' ' << a.power;
        case ActionKind::Throw:
            return out << "THROW " << a.x << ' ' << a.y << ' ' << a.power;
        case ActionKind::Obliviate:
            return out << "OBLIVIATE " << a.target_id;
        case ActionKind::Petrificus:
            return out << "PETRIFICUS " << a.target_id;
        case ActionKind::Accio:
            return out << "ACCIO " << a.target_id;
        case ActionKind::Flipendo:
            return out << "FLIPENDO " << a.target_id;
    }
    return out;
}

inline std::ostream& operator<<(std::ostream& out, const TurnOutput& o) {
    return out << o.primary << '\n' << o.secondary;
}

// ---- TurnInput / TurnRef ----

/// Borrowed view shared between the owning `TurnInput` (subprocess
/// transport) and the cbindgen-generated `::TurnInputFFI` (plugin
/// transport). Bot logic should take `const TurnRef&` so the same
/// `decide(...)` function works in both transports — mirrors the
/// `TurnRef`/`as_ref` pattern on the Rust side.
struct TurnRef {
    int32_t                 my_score;
    int32_t                 my_magic;
    int32_t                 opp_score;
    int32_t                 opp_magic;
    std::span<const Entity> entities;
};

/// Owning C++ form of the per-tick input — the FFI-facing `TurnInputFFI`
/// is a borrowed view, which doesn't fit the subprocess transport. This
/// type is what subprocess bots actually read from stdin.
struct TurnInput {
    int32_t             my_score;
    int32_t             my_magic;
    int32_t             opp_score;
    int32_t             opp_magic;
    std::vector<Entity> entities;

    TurnRef as_ref() const {
        return TurnRef{my_score, my_magic, opp_score, opp_magic,
                       std::span<const Entity>(entities)};
    }
};

inline TurnRef as_ref(const ::TurnInputFFI& ffi) {
    return TurnRef{
        ffi.my_score,
        ffi.my_magic,
        ffi.opp_score,
        ffi.opp_magic,
        std::span<const Entity>(ffi.entities, static_cast<size_t>(ffi.num_entities)),
    };
}

inline std::istream& operator>>(std::istream& in, TurnInput& v) {
    if (!(in >> v.my_score >> v.my_magic)) return in;
    if (!(in >> v.opp_score >> v.opp_magic)) return in;
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

}  // namespace cgio
