// Spider Attack bot strategy — v1.
//
// Port of games/spider_attack/bots/baseline_cpp/v1/spider_attack.cpp onto
// the workspace's bot template: structurally identical to the original
// CodinGame paste, but wrapped in a namespace and driven by
// `cgio::TurnInput` / `TurnOutput` instead of stdin / stdout. The per-tick
// logic at the bottom of `decide()` mirrors the snapshot's `int main()`
// body line-for-line so a future bug-fix port stays a 5-minute diff.
//
// Strategy summary (snapshot):
//   * Init: compute three `heroSpots` at distance `baseVisRad + heroVisRad*0.65`
//     from the base, fanned 45° from the base-diagonal by an arc that
//     puts adjacent spots `1.8 * heroVisRad` apart.
//   * Each turn:
//       - Threats = monsters with threatFor == ME, sorted by distance to
//         our base.
//       - Assign each threat to the closest free hero (greedy).
//       - For each hero (slot 0..2):
//           * If holding a threat AND the threat is `nearBase` AND we're
//             within WIND range (1280) → cast WIND pointing away from
//             our base (target - base + hero.pos).
//           * Else if assigned a threat → MOVE to it.
//           * Else if any monster is within 1.5 * heroVisRad² of this
//             slot's guard spot → MOVE to that monster.
//           * Else → MOVE to the guard spot.
//
// No SHIELD, no CONTROL, no defence outside the base radius beyond drift
// of the guard spots. Preserved verbatim from the snapshot — fidelity >
// improvements here.

#pragma once

#include "../../defs/include/spider_attack_defs_io.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <iostream>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_set>
#include <vector>

namespace spider_attack_v1_cpp {

// ---- Constants ----

static constexpr double maxX = 17630;
static constexpr double maxY = 9000;

static constexpr double maxheroespeed = 800;
static constexpr double maxheroespeed2 = maxheroespeed * maxheroespeed;

static constexpr double monsterSpeed = 400;
static constexpr double monsterSpeed2 = monsterSpeed * monsterSpeed;

static constexpr double heroVisRad = 2200;
static constexpr double heroVisRad2 = heroVisRad * heroVisRad;
static constexpr double baseVisRad = 6000;
static constexpr double baseVisRad2 = baseVisRad * baseVisRad;

static constexpr double heroDmgRad = 800;
static constexpr double heroDmgRad2 = heroDmgRad * heroDmgRad;

static constexpr double baseRange = 5000;
static constexpr double baseRange2 = baseRange * baseRange;

inline float toRad(float deg) { return deg * static_cast<float>(M_PI) / 180.0f; }
inline float toDeg(float rad) { return rad * 180.0f / static_cast<float>(M_PI); }

// ---- Vec ----

struct Vec;
inline Vec operator+(Vec a, const Vec& b) noexcept;
inline Vec operator-(Vec a, const Vec& b) noexcept;
inline Vec operator*(Vec v, double d) noexcept;
inline Vec operator/(Vec v, double d) noexcept;
inline Vec& operator+=(Vec& a, const Vec& b) noexcept;
inline Vec& operator-=(Vec& a, const Vec& b) noexcept;
inline Vec& operator*=(Vec& v, double d) noexcept;
inline Vec& operator/=(Vec& v, double d) noexcept;

struct Vec {
    static const Vec center;

    double x;
    double y;

    Vec(double x_, double y_) : x(x_), y(y_) {}
    Vec(int x_, int y_) : x(x_), y(y_) {}

    inline double length2() const noexcept { return x * x + y * y; }
    inline double length() const noexcept { return std::sqrt(length2()); }
    inline double dist2(const Vec& other) const noexcept {
        return (other - *this).length2();
    }
    inline double dist(const Vec& other) const noexcept { return std::sqrt(dist2(other)); }

    inline Vec toPos() const noexcept { return (*this - center).truncate() + center; }
    inline Vec truncate() const noexcept { return {std::trunc(x), std::trunc(y)}; }
    inline Vec normalize() const noexcept {
        auto len = length();
        return *this / len;
    }
    inline double dot(const Vec& other) const noexcept { return x * other.x + y * other.y; }
    inline double angle(const Vec& other) const noexcept {
        return std::acos(dot(other) / length() * other.length());
    }
    inline Vec closestPoint(const Vec& orig, const Vec& v) const noexcept {
        auto norm = v.normalize();
        auto d = norm.dot(*this - orig);
        return orig + norm * d;
    }
    inline Vec intersect(double r, const Vec& origin, const Vec& v) noexcept {
        auto a0 = origin - *this;
        auto b0 = a0 + v;
        auto d = b0 - a0;
        auto dr2 = d.length2();
        auto r2 = r * r;
        auto D = a0.x * b0.y - b0.x * a0.y;
        auto D2 = D * D;
        auto det = r2 * dr2 - D2;
        if (det < 0) return {-1, -1};
        det = std::sqrt(det);
        auto xp1 = D * d.y;
        auto xp2 = (d.y < 0 ? -1 : 1) * d.x * det;
        auto yp1 = -D * d.x;
        auto yp2 = std::abs(d.y) * det;
        Vec v1{(xp1 + xp2) / dr2, (yp1 + yp2) / dr2};
        Vec v2{(xp1 - xp2) / dr2, (yp1 - yp2) / dr2};
        return (v1.dist2(a0) < v2.dist2(a0) ? v1 : v2) + *this;
    }
};

// C++17 inline definition: same shape as the snapshot's out-of-class
// `const Vec Vec::center{...}` but ODR-safe in a header.
inline const Vec Vec::center{maxX / 2.0, maxY / 2.0};

inline Vec& operator+=(Vec& a, const Vec& b) noexcept { a.x += b.x; a.y += b.y; return a; }
inline Vec& operator-=(Vec& a, const Vec& b) noexcept { a.x -= b.x; a.y -= b.y; return a; }
inline Vec& operator*=(Vec& v, double d) noexcept { v.x *= d; v.y *= d; return v; }
inline Vec& operator/=(Vec& v, double d) noexcept { v.x /= d; v.y /= d; return v; }
inline Vec operator+(Vec a, const Vec& b) noexcept { a += b; return a; }
inline Vec operator-(Vec a, const Vec& b) noexcept { a -= b; return a; }
inline Vec operator*(Vec v, double d) noexcept { v *= d; return v; }
inline Vec operator/(Vec v, double d) noexcept { v /= d; return v; }

// ---- Strategy enums ----
//
// Mirror the wire-format values so we can `static_cast` from `EntityKind`
// (cgio) → `EntType` (strategy) without a branch. `ThreatFor` likewise.

enum class EntType { MONSTER = 0, HERO = 1, ENEMY_HERO = 2 };
enum class ThreatFor { NONE = 0, ME = 1, OPONENT = 2 };
enum class Action { MOVE, SPELL, WAIT };
enum class Spell { WIND, SHIELD, CONTROL };

// Build a HeroAction in the cgio format, in the shape the snapshot's
// `doMove` / `doSpell` produced as text. Wait is the default-fill so
// hero slots that fall off the bot's threat list still emit a valid line.

inline HeroAction make_wait() {
    return HeroAction{ActionKind::Wait, 0, 0, 0};
}
inline HeroAction make_move(const Vec& target) {
    return HeroAction{ActionKind::Move,
                      static_cast<int32_t>(target.x),
                      static_cast<int32_t>(target.y),
                      0};
}
inline HeroAction make_wind(const Vec& target) {
    return HeroAction{ActionKind::Wind,
                      static_cast<int32_t>(target.x),
                      static_cast<int32_t>(target.y),
                      0};
}

// ---- Ent (per-turn entity view) ----

struct Ent {
    int id;
    EntType type;
    Vec position;
    int shieldLife;
    bool isControlled;
    // monster section
    int health;
    Vec speed;
    bool nearBase;
    ThreatFor threatFor;
    // Extras
    std::optional<Vec> target{std::nullopt};
    int targetId{-1};
    Action action{Action::WAIT};
};

// Snapshot's debug helpers — emitted on stderr; the runner forwards
// them so you still see the per-turn triage in match logs. Cheap and
// load-bearing for debugging the threat-assignment bug or two that
// inevitably show up.
inline void printEnts(std::string_view title, const std::vector<Ent>& vec) {
    std::cerr << title << ": ";
    for (const Ent& elm : vec) std::cerr << elm.id << " ";
    std::cerr << std::endl;
}

// ---- Match-scoped state (set in on_init, used in decide) ----

struct State {
    Vec base{0, 0};
    int heroes_per_player = 3;
    // Three guard positions our heroes default to when no threats are
    // visible. Computed once in `on_init` and reused every tick.
    std::vector<Vec> heroSpots;
};

inline State& state() {
    static State s;
    return s;
}

// ---- Bot entry points ----

inline void on_init(const cgio::InitialInput& init) {
    auto& st = state();
    st.base = Vec{init.base_x, init.base_y};
    st.heroes_per_player = init.heroes_per_player;
    st.heroSpots.clear();

    // Snapshot geometry, line-for-line: spots sit on a circle of radius
    // `guardSpotLen` around the base, centered on the base-diagonal at
    // 45°, with neighbours separated by an arc producing chord length
    // `guardSpotDist = 1.8 * heroVisRad`.
    auto centerAngle = toRad(45.0);
    auto guardSpotDist = 1.8 * heroVisRad;
    auto guardSpotLen = baseVisRad + heroVisRad * 0.65;
    // Law of cosines: cos(C) = (a² + b² - c²) / 2ab. With a = b = guardSpotLen:
    // cos(C) = 1 - c² / (2a²).
    auto diffAngle = std::acos(1.0 - (guardSpotDist * guardSpotDist) /
                                         (2.0 * guardSpotLen * guardSpotLen));
    for (int i = 0; i < st.heroes_per_player; ++i) {
        Vec basePoint{
            guardSpotLen * std::cos(centerAngle + (-1 * (i - 1)) * (diffAngle)),
            guardSpotLen * std::sin(centerAngle + (-1 * (i - 1)) * (diffAngle))};
        // Mirror for the bottom-right base so the spots fan into the
        // playing field instead of outside it.
        if (init.base_x != 0) {
            basePoint = st.base - basePoint;
        }
        st.heroSpots.emplace_back(basePoint);
    }
}

inline TurnOutput decide(const cgio::TurnInput& turn) {
    auto& st = state();
    const Vec& base = st.base;
    const int n_heroes = st.heroes_per_player;

    // Re-bucket cgio entities into the snapshot's monster / hero /
    // enemy vectors so the rest of this function reads like the
    // original `int main()` body.
    std::vector<Ent> monsters, heroes, enemies;
    for (const auto& e : turn.entities) {
        Ent ent{
            e.id,
            static_cast<EntType>(static_cast<int>(e.kind)),
            Vec{e.x, e.y},
            e.shield_life,
            e.is_controlled != 0,
            e.health,
            Vec{e.vx, e.vy},
            e.near_base != 0,
            static_cast<ThreatFor>(e.threat_for),
        };
        switch (ent.type) {
            case EntType::MONSTER:    monsters.emplace_back(std::move(ent)); break;
            case EntType::HERO:       heroes.emplace_back(std::move(ent)); break;
            case EntType::ENEMY_HERO: enemies.emplace_back(std::move(ent)); break;
        }
    }

    std::sort(monsters.begin(), monsters.end(), [&](Ent& a, Ent& b) {
        return a.position.dist2(base) < b.position.dist2(base);
    });
    printEnts("M", monsters);

    std::vector<Ent> threats;
    std::copy_if(monsters.begin(), monsters.end(), std::back_inserter(threats),
                 [&](Ent& m) { return m.threatFor == ThreatFor::ME; });
    printEnts("th", threats);

    std::vector<Ent> inBase;
    std::copy_if(threats.begin(), threats.end(), std::back_inserter(inBase),
                 [&](Ent& m) { return m.nearBase; });
    printEnts("iB", inBase);

    std::unordered_set<int> usedHeroes;
    for (size_t i = 0; i < threats.size() && usedHeroes.size() < 3; ++i) {
        auto closest = std::min_element(
            heroes.begin(), heroes.end(), [&](Ent& a, Ent& b) {
                if (usedHeroes.find(b.id) != usedHeroes.end()) return true;
                if (usedHeroes.find(a.id) != usedHeroes.end()) return false;
                return a.position.dist2(threats[i].position) <
                       b.position.dist2(threats[i].position);
            });
        if (closest != heroes.end() &&
            usedHeroes.find(closest->id) == usedHeroes.end()) {
            usedHeroes.insert(closest->id);
            closest->target = threats[i].position;
            closest->targetId = threats[i].id;
            std::cerr << "th: " << threats[i].id << " h: " << closest->id
                      << std::endl;
        }
    }

    // Output: three actions in hero-id order. cgio's entity stream is
    // sorted by id, so `heroes[i]` already matches `out.actions[i]`.
    TurnOutput out{};
    for (auto& a : out.actions) a = make_wait();

    for (int i = 0; i < n_heroes; ++i) {
        if (i >= static_cast<int>(heroes.size())) {
            // Defensive — happens only if the engine ever drops one of
            // our own heroes from the input stream. Snapshot would have
            // segfaulted; we WAIT.
            out.actions[i] = make_wait();
            continue;
        }
        Ent& h = heroes[i];
        if (h.target.has_value()) {
            auto& target = h.target.value();
            bool iB = false;
            for (auto& t : inBase) {
                if (t.id == h.targetId) { iB = true; break; }
            }
            if (iB && h.position.dist2(target) <= 1280 * 1280) {
                // Push the monster *away* from our base: the destination
                // is (monster_pos - base + hero_pos) — the same vector
                // arithmetic as the snapshot's `doSpell(WIND, ...)`.
                out.actions[i] = make_wind(target - base + h.position);
                std::cerr << i << "threat WIND" << std::endl;
                continue;
            }
            out.actions[i] = make_move(target);
            std::cerr << i << "threat" << std::endl;
            continue;
        }

        // TODO (carried from snapshot): also pick the globally-closest
        // monster, and if it isn't too far, still attack it.
        std::vector<Ent> inRange;
        std::copy_if(monsters.begin(), monsters.end(), std::back_inserter(inRange),
                     [&](auto& m) {
                         return m.position.dist2(st.heroSpots[i]) <=
                                heroVisRad2 * 1.5;
                     });
        printEnts("iR", inRange);

        if (!inRange.empty()) {
            out.actions[i] = make_move(inRange[0].position);
            std::cerr << i << " inRange" << std::endl;
            continue;
        }

        out.actions[i] = make_move(st.heroSpots[i]);
        std::cerr << i << " default" << std::endl;
    }
    return out;
}

}  // namespace spider_attack_v1_cpp
