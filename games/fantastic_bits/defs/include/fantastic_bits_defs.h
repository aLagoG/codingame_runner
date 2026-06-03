// Hand-written C++ mirror of games/fantastic_bits/defs/src/lib.rs.
// Both sides are independent (no cbindgen between them) — when you
// change the wire types here, change them there too.

#pragma once

#include <cstdint>

/// What kind of action a wizard is taking this tick.
enum class ActionKind : uint8_t {
    Move,
    Throw,
    Obliviate,
    Petrificus,
    Accio,
    Flipendo,
};

/// Tags for entities the engine emits per tick.
enum class EntityKind : uint8_t {
    Wizard,
    OpponentWizard,
    Snaffle,
    Bludger,
};

/// One wizard's action for one tick. Fields are kind-dependent — fill in
/// only the ones the kind needs; the rest are ignored on the wire.
struct WizardAction {
    ActionKind kind;
    /// MOVE/THROW: target x. Spells: ignored.
    int32_t x;
    /// MOVE/THROW: target y. Spells: ignored.
    int32_t y;
    /// MOVE: thrust [0, 150]. THROW: power [0, 500]. Spells: ignored.
    int32_t power;
    /// Spells: target entity id. MOVE/THROW: ignored.
    int32_t target_id;
};

/// Output for one tick: the two actions for the player's two wizards,
/// in wizard-id order (lower id first). Written as two lines on the wire.
struct TurnOutput {
    WizardAction primary;
    WizardAction secondary;
};

/// One row of the per-tick entity list. `state` is kind-dependent:
///   * Wizard: `1` if grabbing a Snaffle, else `0`.
///   * Snaffle: `1` if grabbed by a Wizard, else `0`.
///   * Bludger: `entityId` of last victim (-1 if none).
struct Entity {
    int32_t id;
    EntityKind kind;
    int32_t x;
    int32_t y;
    int32_t vx;
    int32_t vy;
    int32_t state;
};
