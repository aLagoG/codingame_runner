//! Hand-rolled 2D disc physics. Every formula here mirrors the CodinGame
//! Fantastic Bits referee (`Referee.java`) function-for-function — that's
//! the only way to get bit-for-bit replay parity.
//!
//! What this module owns:
//!   * 2D vector arithmetic.
//!   * Time-of-impact (TOI) between two moving discs, between a disc
//!     and an axis-aligned wall, and a snaffle-specific variant that
//!     ignores left/right walls inside the goal mouth.
//!   * Elastic-rebound impulse formulas, including CodinGame's
//!     min-impulse-100 floor for slow contacts (entity-entity only —
//!     wall bounces deliberately skip the floor, per the statement and
//!     `WallCollision.react`).
//!
//! What this module deliberately does NOT own:
//!   * Game state, spells, scoring, AI — all in `lib.rs`.
//!   * Goal-line scoring (it's a position check, not a collision; the
//!     game loop computes TOI to `x=0` / `x=WIDTH` itself).
//!
//! Boundary chosen so we could swap in `parry2d` later without touching
//! anything game-shaped.

#![allow(dead_code)] // some helpers are exercised only via the game loop.

/// CodinGame's slow-contact impulse floor for entity-entity collisions.
pub const MIN_IMPULSE: f64 = 100.0;

/// Small overlap-fix margin used by `EntityCollision.react` and
/// `DynamicEntityCollision.react`. Pushes overlapping bodies just past
/// touch so the next tick doesn't immediately re-trigger.
pub const EPSILON: f64 = 0.00001;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct V2 {
    pub x: f64,
    pub y: f64,
}

impl V2 {
    pub const ZERO: V2 = V2 { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Self {
        V2 { x, y }
    }
    pub fn len_sq(self) -> f64 {
        self.x * self.x + self.y * self.y
    }
    pub fn len(self) -> f64 {
        self.len_sq().sqrt()
    }
    pub fn dot(self, o: V2) -> f64 {
        self.x * o.x + self.y * o.y
    }
    pub fn add(self, o: V2) -> V2 {
        V2 {
            x: self.x + o.x,
            y: self.y + o.y,
        }
    }
    pub fn sub(self, o: V2) -> V2 {
        V2 {
            x: self.x - o.x,
            y: self.y - o.y,
        }
    }
    pub fn mul(self, s: f64) -> V2 {
        V2 {
            x: self.x * s,
            y: self.y * s,
        }
    }
    pub fn normalize(self) -> V2 {
        let l = self.len();
        if l == 0.0 {
            V2::ZERO
        } else {
            V2 {
                x: self.x / l,
                y: self.y / l,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallSide {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallHit {
    pub side: WallSide,
    pub t: f64,
}

/// Time-of-impact between two moving discs (or one moving + one
/// stationary — let one velocity be zero). Mirrors the referee's
/// `contactPosition(dv, d, dmin)`. Note `d` points from `b` to `a`
/// (`this - o` in the Java) — get the sign wrong here and every TOI
/// inverts.
///
/// ```text
/// dv = a.vel - b.vel
/// d  = a.pos - b.pos
/// a  = |dv|²,  b = d·dv,  c = |d|² - dmin²
/// delta = b² - a·c
/// t = min((-b ± √delta) / a)
/// ```
///
/// `target_dist` is the centre-to-centre distance at impact:
///   * disc / disc → `r_a + r_b`
///   * pod / snaffle (capture) → `wizard.radius - 1` (snaffle radius is
///     deliberately *not* added per the expert rules + referee)
///
/// Returns `None` when the quadratic has no real root or relative
/// velocity is zero. May return a negative `t` when the discs are
/// already overlapping and separating — the caller filters `t > 0`.
pub fn toi_disc_disc(a_pos: V2, a_vel: V2, b_pos: V2, b_vel: V2, target_dist: f64) -> Option<f64> {
    let dv = a_vel.sub(b_vel);
    let d = a_pos.sub(b_pos);
    let aa = dv.len_sq();
    if aa == 0.0 {
        return None;
    }
    let bb = d.x * dv.x + d.y * dv.y;
    let cc = d.len_sq() - target_dist * target_dist;
    let delta = bb * bb - aa * cc;
    if delta <= 0.0 {
        return None;
    }
    let rd = delta.sqrt();
    let t1 = (-bb - rd) / aa;
    let t2 = (-bb + rd) / aa;
    Some(t1.min(t2))
}

/// Time-of-impact between a moving disc and the four axis-aligned walls
/// of the playing field. Returns the earliest wall hit (smallest
/// non-negative t). Field is `[0, width] × [0, height]`; the disc
/// touches a wall when its centre is `radius` units from that wall.
pub fn toi_disc_wall(pos: V2, vel: V2, radius: f64, width: f64, height: f64) -> Option<WallHit> {
    let mut best: Option<WallHit> = None;

    let mut consider = |side: WallSide, t: f64| {
        if t < 0.0 {
            return;
        }
        match best {
            Some(b) if b.t <= t => {}
            _ => best = Some(WallHit { side, t }),
        }
    };

    if vel.x > 0.0 {
        consider(WallSide::Right, (width - radius - pos.x) / vel.x);
    } else if vel.x < 0.0 {
        consider(WallSide::Left, (radius - pos.x) / vel.x);
    }
    if vel.y > 0.0 {
        consider(WallSide::Bottom, (height - radius - pos.y) / vel.y);
    } else if vel.y < 0.0 {
        consider(WallSide::Top, (radius - pos.y) / vel.y);
    }
    best
}

/// Snaffle-only wall TOI: skips a left/right wall hit whose impact y is
/// inside the goal mouth `[goal_y_top, goal_y_bottom]`, so the snaffle
/// can pass through and be checked for scoring instead. Mirrors
/// `Snaffle.contactPositionWall` in the referee.
pub fn toi_snaffle_wall(
    pos: V2,
    vel: V2,
    radius: f64,
    width: f64,
    height: f64,
    goal_y_top: f64,
    goal_y_bottom: f64,
) -> Option<WallHit> {
    let mut best: Option<WallHit> = None;

    let mut consider = |side: WallSide, t: f64| {
        if t < 0.0 {
            return;
        }
        match best {
            Some(b) if b.t <= t => {}
            _ => best = Some(WallHit { side, t }),
        }
    };

    if vel.x > 0.0 {
        let t = (width - radius - pos.x) / vel.x;
        let y_at = pos.y + t * vel.y;
        if y_at < goal_y_top || y_at > goal_y_bottom {
            consider(WallSide::Right, t);
        }
    } else if vel.x < 0.0 {
        let t = (radius - pos.x) / vel.x;
        let y_at = pos.y + t * vel.y;
        if y_at < goal_y_top || y_at > goal_y_bottom {
            consider(WallSide::Left, t);
        }
    }
    if vel.y > 0.0 {
        consider(WallSide::Bottom, (height - radius - pos.y) / vel.y);
    } else if vel.y < 0.0 {
        consider(WallSide::Top, (radius - pos.y) / vel.y);
    }
    best
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreSide {
    /// Snaffle crossed `x = 0` → team 1 scored.
    Left,
    /// Snaffle crossed `x = width` → team 0 scored.
    Right,
}

/// TOI for a snaffle to cross either goal line (`x=0` or `x=width`).
/// Returns the earlier crossing if `vx ≠ 0`. Centre-crossing, not
/// edge — `Snaffle.checkGoalCollisions` checks `position.x >= WIDTH`
/// / `position.x <= 0` directly.
pub fn toi_snaffle_score(pos: V2, vel: V2, width: f64) -> Option<(f64, ScoreSide)> {
    if vel.x == 0.0 {
        return None;
    }
    let t_right = (width - pos.x) / vel.x;
    let t_left = -pos.x / vel.x;
    let candidates = [(t_right, ScoreSide::Right), (t_left, ScoreSide::Left)];
    candidates
        .into_iter()
        .filter(|(t, _)| *t > 0.0)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
}

// ============================================================
//  Collision response
// ============================================================

/// Result of resolving a dynamic-vs-dynamic disc collision: how the
/// two discs' velocities should change, plus an axial overlap-fix
/// (how far each centre should move along the separation normal to
/// guarantee they're no longer overlapping).
#[derive(Debug, Clone, Copy)]
pub struct DynDynResolution {
    /// Δvelocity for body A.
    pub dv_a: V2,
    /// Δvelocity for body B.
    pub dv_b: V2,
    /// Δposition for body A.
    pub dp_a: V2,
    /// Δposition for body B.
    pub dp_b: V2,
}

/// Elastic rebound for two moving discs, with the CodinGame
/// min-impulse-100 floor applied to slow contacts. Mirrors
/// `DynamicEntityCollision.react`. Returns the velocity *deltas* and
/// position-correction deltas (only non-zero when the two discs are
/// already overlapping at call time).
pub fn resolve_dyn_dyn(
    a_pos: V2,
    a_vel: V2,
    a_mass: f64,
    a_radius: f64,
    b_pos: V2,
    b_vel: V2,
    b_mass: f64,
    b_radius: f64,
) -> DynDynResolution {
    let mut normal = b_pos.sub(a_pos).normalize();
    if normal.x == 0.0 && normal.y == 0.0 {
        normal = V2::new(1.0, 0.0);
    }
    let rel_v = a_vel.sub(b_vel);
    let force = normal.dot(rel_v) / (1.0 / a_mass + 1.0 / b_mass);
    let repulse = force.max(MIN_IMPULSE);
    let total = force + repulse;

    let dv_a = normal.mul(-total / a_mass);
    let dv_b = normal.mul(total / b_mass);

    // Overlap fix: mirror of `DynamicEntityCollision.react`'s
    // `fixPosition` calls. Each centre moves half the penetration
    // (plus ε) along the normal.
    let dist = b_pos.sub(a_pos).len();
    let diff = dist - a_radius - b_radius;
    let (dp_a, dp_b) = if diff <= 0.0 {
        let shift = -diff / 2.0 + EPSILON;
        (normal.mul(-shift), normal.mul(shift))
    } else {
        (V2::ZERO, V2::ZERO)
    };

    DynDynResolution {
        dv_a,
        dv_b,
        dp_a,
        dp_b,
    }
}

/// Result of a dyn/static collision: Δvel for the moving body, Δpos
/// for any overlap correction (mirrors the referee's half-split fix,
/// which only moves the dynamic body even though that arguably leaves
/// the system overlapping — kept faithful for parity).
#[derive(Debug, Clone, Copy)]
pub struct DynStaticResolution {
    pub dv: V2,
    pub dp: V2,
}

/// Elastic rebound for a moving disc vs a static target (goal post,
/// any infinite-mass thing). Mirrors `EntityCollision.react`:
///
/// ```text
/// normal = (b.pos - a.pos).normalize();   // (1,0) if degenerate
/// force  = (normal · a.vel) * a.mass;
/// repulse = max(MIN_IMPULSE, force);
/// total   = force + repulse;
/// impulse = -normal * total;
/// a.vel += impulse / a.mass = -normal * (total / a.mass);
/// ```
pub fn resolve_dyn_static(
    a_pos: V2,
    a_vel: V2,
    a_mass: f64,
    a_radius: f64,
    b_pos: V2,
    b_radius: f64,
) -> DynStaticResolution {
    let mut normal = b_pos.sub(a_pos).normalize();
    if normal.x == 0.0 && normal.y == 0.0 {
        normal = V2::new(1.0, 0.0);
    }
    let force = normal.dot(a_vel) * a_mass;
    let repulse = force.max(MIN_IMPULSE);
    let total = force + repulse;
    let dv = normal.mul(-total / a_mass);

    // Overlap fix: only the dynamic body moves. Referee uses the same
    // `diff / 2` factor as dyn/dyn (intentionally only fixes half),
    // we mirror that quirk to stay bit-for-bit.
    let dist = b_pos.sub(a_pos).len();
    let diff = dist - a_radius - b_radius;
    let dp = if diff <= 0.0 {
        let shift = -diff / 2.0 + EPSILON;
        normal.mul(-shift)
    } else {
        V2::ZERO
    };

    DynStaticResolution { dv, dp }
}

/// Wall rebound: flips the velocity component along the wall's normal
/// and snaps the disc back inside the field if it overshot. NO
/// min-impulse floor (per the statement + `WallCollision.react`).
///
/// Returns the velocity AFTER the bounce + a position-correction
/// vector (Δpos) for the disc; callers add the Δpos to the disc's
/// current centre.
pub fn resolve_wall(
    side: WallSide,
    pos: V2,
    vel: V2,
    radius: f64,
    width: f64,
    height: f64,
) -> (V2, V2) {
    let mut new_vel = vel;
    let mut dp = V2::ZERO;
    match side {
        WallSide::Left | WallSide::Right => new_vel.x = -vel.x,
        WallSide::Top | WallSide::Bottom => new_vel.y = -vel.y,
    }
    // Penetration fix — mirror of WallCollision.react's fixPosition.
    if pos.x > width - radius {
        dp.x -= 2.0 * (pos.x - (width - radius));
    }
    if pos.x < radius {
        dp.x -= 2.0 * (pos.x - radius);
    }
    if pos.y > height - radius {
        dp.y -= 2.0 * (pos.y - (height - radius));
    }
    if pos.y < radius {
        dp.y -= 2.0 * (pos.y - radius);
    }
    (new_vel, dp)
}

/// Symmetric round (round half away from zero). CodinGame's
/// `symmetricRound`: `23.5 → 24`, `-23.5 → -24`. Applied at the END
/// of every tick to both positions and velocities (see `endRound`).
pub fn round_half_away(x: f64) -> i32 {
    if x >= 0.0 {
        (x + 0.5).floor() as i32
    } else {
        (x - 0.5).ceil() as i32
    }
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64) -> V2 {
        V2::new(x, y)
    }

    #[test]
    fn toi_head_on_collision() {
        // Two unit-radius discs 4 apart on the x axis, moving toward
        // each other at unit speed. They should touch (centre distance
        // 2) after t = 1.
        let t = toi_disc_disc(v(0.0, 0.0), v(1.0, 0.0), v(4.0, 0.0), v(-1.0, 0.0), 2.0).unwrap();
        assert!((t - 1.0).abs() < 1e-9);
    }

    #[test]
    fn toi_glancing_miss_returns_none() {
        // Parallel paths, never touch.
        let r = toi_disc_disc(v(0.0, 0.0), v(1.0, 0.0), v(0.0, 100.0), v(1.0, 0.0), 2.0);
        assert!(r.is_none());
    }

    #[test]
    fn toi_zero_relative_velocity_is_none() {
        // Both moving same direction at same speed — never collide.
        let r = toi_disc_disc(v(0.0, 0.0), v(5.0, 0.0), v(10.0, 0.0), v(5.0, 0.0), 2.0);
        assert!(r.is_none());
    }

    #[test]
    fn toi_overlapping_returns_negative() {
        // Already overlapping & separating. TOI should be negative
        // (caller filters t > 0).
        let t = toi_disc_disc(v(0.0, 0.0), v(-1.0, 0.0), v(1.0, 0.0), v(1.0, 0.0), 2.0).unwrap();
        assert!(t < 0.0);
    }

    #[test]
    fn wall_hit_picks_earliest_side() {
        // Moving up-right; should hit Top before Right since y is
        // closer to its wall.
        let r = toi_disc_wall(v(100.0, 200.0), v(10.0, -10.0), 50.0, 16000.0, 7500.0).unwrap();
        assert_eq!(r.side, WallSide::Top);
        // t = (50 - 200) / -10 = 15.
        assert!((r.t - 15.0).abs() < 1e-9);
    }

    #[test]
    fn snaffle_wall_passes_through_goal_mouth() {
        // Heading straight right, impact y = 3750 (centre of mouth).
        // Mouth = [1750, 5750]. Should NOT register a right wall hit.
        let r = toi_snaffle_wall(
            v(15000.0, 3750.0),
            v(100.0, 0.0),
            150.0,
            16000.0,
            7500.0,
            1750.0,
            5750.0,
        );
        assert!(r.is_none());
    }

    #[test]
    fn snaffle_wall_bounces_outside_mouth() {
        // Same x velocity but y outside mouth — should hit right wall.
        let r = toi_snaffle_wall(
            v(15000.0, 500.0),
            v(100.0, 0.0),
            150.0,
            16000.0,
            7500.0,
            1750.0,
            5750.0,
        );
        let hit = r.unwrap();
        assert_eq!(hit.side, WallSide::Right);
    }

    #[test]
    fn snaffle_score_right_goal() {
        let (t, side) = toi_snaffle_score(v(15000.0, 3750.0), v(500.0, 0.0), 16000.0).unwrap();
        assert_eq!(side, ScoreSide::Right);
        assert!((t - 2.0).abs() < 1e-9);
    }

    #[test]
    fn snaffle_score_left_goal() {
        let (t, side) = toi_snaffle_score(v(1000.0, 3750.0), v(-500.0, 0.0), 16000.0).unwrap();
        assert_eq!(side, ScoreSide::Left);
        assert!((t - 2.0).abs() < 1e-9);
    }

    #[test]
    fn elastic_dyn_dyn_equal_mass_swaps_velocities_when_above_floor() {
        // Two equal-mass discs head-on fast enough that the natural
        // impulse exceeds the min-impulse floor: should swap velocities.
        // At vel ±300, force = 600 / 2 = 300 > 100. Δv_a = -600 →
        // a.vel becomes -300, b.vel becomes +300.
        let r = resolve_dyn_dyn(
            v(0.0, 0.0),
            v(300.0, 0.0),
            1.0,
            400.0,
            v(800.0, 0.0),
            v(-300.0, 0.0),
            1.0,
            400.0,
        );
        assert!((r.dv_a.x - (-600.0)).abs() < 1e-9);
        assert!((r.dv_b.x - 600.0).abs() < 1e-9);
    }

    #[test]
    fn elastic_dyn_dyn_min_impulse_kicks_in_at_low_speed() {
        // At vel ±10, force = 10 < 100 → floor dominates.
        // Δv magnitude per body ≈ 110 = force + max(force, 100).
        let r = resolve_dyn_dyn(
            v(0.0, 0.0),
            v(10.0, 0.0),
            1.0,
            400.0,
            v(800.0, 0.0),
            v(-10.0, 0.0),
            1.0,
            400.0,
        );
        assert!((r.dv_a.x - (-110.0)).abs() < 1e-9);
        assert!((r.dv_b.x - 110.0).abs() < 1e-9);
    }

    #[test]
    fn elastic_dyn_static_flips_velocity_when_above_floor() {
        // Vel +300 at unit mass: force = 300, total = 600, Δv = -600.
        // Result vel = +300 + (-600) = -300 (full bounce).
        let r = resolve_dyn_static(v(0.0, 0.0), v(300.0, 0.0), 1.0, 400.0, v(800.0, 0.0), 400.0);
        assert!((r.dv.x - (-600.0)).abs() < 1e-9);
        assert!(r.dv.y.abs() < 1e-9);
    }

    #[test]
    fn wall_flips_normal_component() {
        let (new_vel, _) = resolve_wall(
            WallSide::Right,
            v(15990.0, 3750.0),
            v(50.0, 30.0),
            150.0,
            16000.0,
            7500.0,
        );
        assert_eq!(new_vel.x, -50.0);
        assert_eq!(new_vel.y, 30.0);
    }

    #[test]
    fn wall_corrects_penetration() {
        // Disc has overshot the right wall by 5 units; correction
        // pushes it 10 units left (2 * 5).
        let (_, dp) = resolve_wall(
            WallSide::Right,
            v(15855.0, 3750.0),
            v(50.0, 0.0),
            150.0,
            16000.0,
            7500.0,
        );
        assert!((dp.x - (-10.0)).abs() < 1e-9);
    }

    #[test]
    fn symmetric_round_matches_referee() {
        assert_eq!(round_half_away(23.5), 24);
        assert_eq!(round_half_away(-23.5), -24);
        assert_eq!(round_half_away(0.0), 0);
        assert_eq!(round_half_away(0.4), 0);
        assert_eq!(round_half_away(-0.4), 0);
        assert_eq!(round_half_away(0.5), 1);
        assert_eq!(round_half_away(-0.5), -1);
    }
}
