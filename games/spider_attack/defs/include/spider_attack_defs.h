// Hand-written C++ mirror of spider_attack_defs/src/lib.rs. Both sides are
// independent — when you change the wire types here, change them
// there too.

#pragma once

#include <cstdint>

/// Wire `type` field: 0 = monster, 1 = my hero, 2 = opponent hero.
/// Encoded as a plain enum so the integer literal you see on the wire
/// matches the value of the constant in C++.
enum class EntityKind : int32_t {
    Monster = 0,
    MyHero = 1,
    OppHero = 2,
};

/// Action kind for the hero output line. The wire form is text
/// (WAIT / MOVE / SPELL WIND / SPELL SHIELD / SPELL CONTROL) — this
/// enum + the `HeroAction` payload below are what the bot constructs
/// and the IO layer renders.
enum class ActionKind : uint8_t {
    Wait,
    Move,
    Wind,
    Shield,
    Control,
};

/// One hero's action for one tick. Fields are kind-dependent — only
/// the ones the kind needs are read at output time.
struct HeroAction {
    ActionKind kind;
    /// MOVE / WIND / CONTROL: target x. SHIELD / WAIT: ignored.
    int32_t x;
    /// MOVE / WIND / CONTROL: target y. SHIELD / WAIT: ignored.
    int32_t y;
    /// SHIELD / CONTROL: target entity id. MOVE / WIND / WAIT: ignored.
    int32_t entity_id;
};

/// Output for one tick: three actions, one per hero, in hero-id order
/// (lower id first). Written as three lines on the wire.
struct TurnOutput {
    HeroAction actions[3];
};

/// One row of the per-tick entity list. Monster-only fields (health,
/// vx, vy, near_base, threat_for) are -1 for heroes.
struct Entity {
    int32_t id;
    EntityKind kind;
    int32_t x;
    int32_t y;
    int32_t shield_life;
    int32_t is_controlled;
    int32_t health;
    int32_t vx;
    int32_t vy;
    int32_t near_base;
    int32_t threat_for;
};
