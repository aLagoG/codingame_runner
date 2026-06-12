#include <algorithm>
#include <array>
#include <cmath>
#include <iostream>
#include <optional>
#include <string>
#include <unordered_set>
#include <vector>

using namespace std;

/**
 * Auto-generated code below aims at helping you parse
 * the standard input according to the problem statement.
 **/

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

float toRad(float deg) {
    return deg * M_PI / 180.0;
}

float toDeg(float rad) {
    return rad * 180.0 / M_PI;
}

struct Vec;
Vec operator+(Vec a, const Vec& b) noexcept;
Vec operator-(Vec a, const Vec& b) noexcept;
Vec operator*(Vec v, double d) noexcept;
Vec operator/(Vec v, double d) noexcept;
Vec& operator+=(Vec& a, const Vec& b) noexcept;
Vec& operator-=(Vec& a, const Vec& b) noexcept;
Vec& operator*=(Vec& v, double d) noexcept;
Vec& operator/=(Vec& v, double d) noexcept;

struct Vec {
    static const Vec center;

    double x;
    double y;

    Vec(double x, double y) : x(x), y(y){};
    Vec(int x, int y) : x(x), y(y){};

    inline double length2() const noexcept {
        return x * x + y * y;
    }

    inline double length() const noexcept {
        return sqrt(length2());
    }

    inline double dist2(const Vec& other) const noexcept {
        return (other - *this).length2();
    }

    inline double dist(const Vec& other) const noexcept {
        return sqrt(dist2(other));
    }

    inline Vec toPos() const noexcept {
        return (*this - center).truncate() + center;
    }

    inline Vec truncate() const noexcept {
        return {trunc(x), trunc(y)};
    }

    inline Vec normalize() const noexcept {
        auto len = length();
        return *this / len;
    }

    inline double dot(const Vec& other) const noexcept {
        return x * other.x + y * other.y;
    }

    inline double angle(const Vec& other) const noexcept {
        return acos(dot(other) / length() * other.length());
    }

    inline Vec closestPoint(const Vec& orig, const Vec& v) const noexcept {
        auto norm = v.normalize();
        auto dist = norm.dot(*this - orig);
        return orig + norm * dist;
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

        if (det < 0) {
            return {-1, -1};
        }

        det = sqrt(det);

        auto xp1 = D * d.y;
        auto xp2 = (d.y < 0 ? -1 : 1) * d.x * det;

        auto yp1 = -D * d.x;
        auto yp2 = abs(d.y) * det;

        Vec v1{(xp1 + xp2) / dr2, (yp1 + yp2) / dr2};
        Vec v2{(xp1 - xp2) / dr2, (yp1 - yp2) / dr2};

        return (v1.dist2(a0) < v2.dist2(a0) ? v1 : v2) + *this;
    }
};

const Vec Vec::center{maxX / 2.0, maxY / 2.0};

#pragma region compound assignment

Vec& operator+=(Vec& a, const Vec& b) noexcept {
    a.x += b.x;
    a.y += b.y;
    return a;
}

Vec& operator-=(Vec& a, const Vec& b) noexcept {
    a.x -= b.x;
    a.y -= b.y;
    return a;
}

Vec& operator*=(Vec& v, double d) noexcept {
    v.x *= d;
    v.y *= d;
    return v;
}

Vec& operator/=(Vec& v, double d) noexcept {
    v.x /= d;
    v.y /= d;
    return v;
}

#pragma endregion

#pragma region binary operators

Vec operator+(Vec a, const Vec& b) noexcept {
    a += b;
    return a;
}

Vec operator-(Vec a, const Vec& b) noexcept {
    a -= b;
    return a;
}

Vec operator*(Vec v, double d) noexcept {
    v *= d;
    return v;
}

Vec operator/(Vec v, double d) noexcept {
    v /= d;
    return v;
}

std::ostream& operator<<(std::ostream& os, Vec const& v) {
    return os << (int)v.x << " " << (int)v.y;
}

#pragma endregion

struct Player {
    int health;
    int mana;
};

enum class EntType { MONSTER = 0, HERO = 1, ENEMY_HERO = 2 };

enum class ThreatFor { NONE = 0, ME = 1, OPONENT = 2 };

enum class Action { MOVE, SPELL, WAIT };
enum class Spell { WIND, SHIELD, CONTROL };

inline void doMove(const Vec& target) {
    cout << "MOVE " << target << endl;
}

inline void doWait() {
    cout << "WAIT" << endl;
}

inline void doSpell(Spell spell, const Vec& target, int id = 0) {
    cout << "SPELL ";
    switch (spell) {
        case Spell::WIND:
            cout << "WIND " << target;
            break;
        case Spell::SHIELD:
            cout << "SHIELD " << id;
            break;
        case Spell::CONTROL:
            cout << "CONTROL " << target << " " << id;
            break;
    }
    cout << endl;
}

inline void doSpell(Spell spell, int id) {
    doSpell(spell, Vec::center, id);
}

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
    std::optional<Vec> target{nullopt};
    int targetId{-1};
    Action action{Action::WAIT};
};

void printEnts(std::string_view title, const std::vector<Ent>& vec) {
    cerr << title << ": ";
    for (const Ent& elm : vec) {
        cerr << elm.id << " ";
    }
    cerr << endl;
}

void printIds(std::string_view title, const std::vector<int>& vec) {
    cerr << title << ": ";
    for (auto elm : vec) {
        cerr << elm << " ";
    }
    cerr << endl;
}

int main() {
    int base_x;  // The corner of the map representing your base
    int base_y;
    cin >> base_x >> base_y;
    cin.ignore();
    int heroes_per_player;  // Always 3
    cin >> heroes_per_player;
    cin.ignore();

    Vec base{base_x, base_y};
    // Calculate hero spots
    std::vector<Vec> heroSpots;
    // Angle is wrong, it needs to make the points be about heroVisRad * 2 away
    auto centerAngle = toRad(45.0);
    auto guardSpotDist = 1.8 * heroVisRad;
    auto guardSpotLen = baseVisRad + heroVisRad * 0.65;  // Maybe sub smth
    // Cos law = cos(C) = (a^2+b^2-c^2)/2ab --- a == b so = 1-(c^2/2a^2)
    auto diffAngle = acos(1.0 - (guardSpotDist * guardSpotDist) /
                                    (2.0 * guardSpotLen * guardSpotLen));
    for (int i = 0; i < heroes_per_player; ++i) {
        Vec basePoint{
            guardSpotLen * cos(centerAngle + (-1 * (i - 1)) * (diffAngle)),
            guardSpotLen * sin(centerAngle + (-1 * (i - 1)) * (diffAngle))};
        if (base_x != 0) {
            basePoint = base - basePoint;
        }
        heroSpots.emplace_back(basePoint);
    }

    // game loop
    while (1) {
        std::array<Player, 2> players{};
        for (int i = 0; i < 2; i++) {
            int health;  // Your base health
            int mana;    // Ignore in the first league; Spend ten mana to cast a
                         // spell
            cin >> health >> mana;
            cin.ignore();
            players[i].health = health;
            players[i].mana = mana;
        }
        int entity_count;  // Amount of heroes and monsters you can see
        cin >> entity_count;
        cin.ignore();

        std::vector<Ent> monsters, heroes, enemies;
        for (int i = 0; i < entity_count; i++) {
            int id;    // Unique identifier
            int type;  // 0=monster, 1=your hero, 2=opponent hero
            int x;     // Position of this entity
            int y;
            int shield_life;  // Ignore for this league; Count down until shield
                              // spell fades
            int is_controlled;  // Ignore for this league; Equals 1 when this
                                // entity is under a control spell
            int health;         // Remaining health of this monster
            int vx;             // Trajectory of this monster
            int vy;
            int near_base;  // 0=monster with no target yet, 1=monster targeting
                            // a base
            int threat_for;  // Given this monster's trajectory, is it a threat
                             // to 1=your base, 2=your opponent's base,
                             // 0=neither
            cin >> id >> type >> x >> y >> shield_life >> is_controlled >>
                health >> vx >> vy >> near_base >> threat_for;
            cin.ignore();
            auto ent =
                Ent{id,          (EntType)type,       {x, y},
                    shield_life, (bool)is_controlled, health,
                    {vx, vy},    (bool)near_base,     (ThreatFor)threat_for};
            switch (ent.type) {
                case EntType::MONSTER:
                    monsters.emplace_back(std::move(ent));
                    break;
                case EntType::HERO:
                    heroes.emplace_back(std::move(ent));
                    break;
                case EntType::ENEMY_HERO:
                    enemies.emplace_back(std::move(ent));
                    break;
            }
        }

        std::sort(monsters.begin(), monsters.end(), [&](Ent& a, Ent& b) {
            return a.position.dist2(base) < b.position.dist2(base);
        });
        printEnts("M", monsters);

        std::vector<Ent> threats;
        std::copy_if(monsters.begin(), monsters.end(),
                     std::back_inserter(threats),
                     [&](Ent& m) { return m.threatFor == ThreatFor::ME; });
        printEnts("th", threats);

        std::vector<Ent> inBase;
        std::copy_if(threats.begin(), threats.end(), std::back_inserter(inBase),
                     [&](Ent& m) { return m.nearBase; });
        printEnts("iB", inBase);

        std::unordered_set<int> usedHeroes;
        for (int i = 0; i < threats.size() && usedHeroes.size() < 3; ++i) {
            auto closest = std::min_element(
                heroes.begin(), heroes.end(), [&](Ent& a, Ent& b) {
                    if (usedHeroes.find(b.id) != usedHeroes.end()) {
                        return true;
                    }
                    if (usedHeroes.find(a.id) != usedHeroes.end()) {
                        return false;
                    }
                    return a.position.dist2(threats[i].position) <
                           b.position.dist2(threats[i].position);
                });
            if (closest != heroes.end() &&
                usedHeroes.find(closest->id) == usedHeroes.end()) {
                usedHeroes.insert(closest->id);
                closest->target = threats[i].position;
                closest->targetId = threats[i].id;
                cerr << "th: " << threats[i].id << " h: " << closest->id
                     << endl;
            }
        }

        std::vector<bool> handled(threats.size(), 0);
        for (int i = 0; i < heroes_per_player; i++) {
            if (heroes[i].target.has_value()) {
                auto& target = heroes[i].target.value();
                bool iB = false;
                for (auto& t : inBase) {
                    if (t.id == heroes[i].targetId) {
                        iB = true;
                        break;
                    }
                }
                if (iB && heroes[i].position.dist2(target) <= 1280 * 1280) {
                    doSpell(Spell::WIND, target - base + heroes[i].position);
                    cerr << i << "threat WIND" << endl;
                    continue;
                }
                doMove(target);
                cerr << i << "threat" << endl;
                continue;
            }

            // TODO: Also get the closest in general, and if it's not too far
            // away still attack it
            std::vector<Ent> inRange;
            std::copy_if(monsters.begin(), monsters.end(),
                         std::back_inserter(inRange), [&](auto& m) {
                             return m.position.dist2(heroSpots[i]) <=
                                    heroVisRad2 * 1.5;
                         });
            printEnts("iR", inRange);

            if (!inRange.empty()) {
                doMove(inRange[0].position);
                cerr << i << " inRange" << endl;
                continue;
            }

            doMove(heroSpots[i]);
            cerr << i << " default" << endl;
        }
    }
}
