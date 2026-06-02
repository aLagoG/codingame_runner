//! Pairwise statistical tests for two-bot comparisons.
//!
//! Three things the report (and `tournament compare`) want to say about
//! a pair (A, B) given `wins_a`, `wins_b`, `draws`, and total `n`:
//!
//!   1. **Win-rate ± CI**  — point estimate of A's pts/n, with a Wilson
//!      95% interval. We treat each game as a Bernoulli trial with
//!      payoff in `{0, 0.5, 1}` and use Wilson's formula on the
//!      effective win count `wins_a + 0.5 * draws`. Not strictly
//!      binomial (it's trinomial), but the cutechess convention and
//!      a fine first-order approximation at our sample sizes.
//!   2. **LOS** — Likelihood of Superiority, the cutechess-cli
//!      formula. Uses only decisive games; draws drop out. Tells you
//!      "probability A is the stronger player given the observed
//!      `wins_a` vs `wins_b`".
//!   3. **p-value** — two-sided binomial p testing the null `p = 0.5`,
//!      via the normal approximation `erfc(|z|/√2)`. Accurate for
//!      n ≳ 30 — fine for tournament sample sizes.
//!
//! `erf`/`erfc` are hand-rolled via Abramowitz & Stegun 7.1.26 to
//! keep the workspace dep-free. Accuracy `|err| ≤ 1.5e-7`.

/// Two-sided 95% normal critical value — pre-computed so callers
/// don't have to spell it out.
pub const Z_95: f64 = 1.959_963_984_540_054;

/// Wilson score interval for a proportion `p̂ = wins / n`. Closed-form,
/// well-behaved at extreme p (Wald-style intervals collapse there).
/// `wins` is `f64` so callers can pass the half-points convention
/// (`wins + 0.5 * draws`). `z` is the two-sided normal critical value
/// (e.g. [`Z_95`] for a 95% CI). Returns `(lo, hi)` clamped to `[0, 1]`.
pub fn wilson_ci(wins: f64, n: u32, z: f64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let n_f = n as f64;
    let p = (wins / n_f).clamp(0.0, 1.0);
    let z2 = z * z;
    let denom = 1.0 + z2 / n_f;
    let center = (p + z2 / (2.0 * n_f)) / denom;
    let margin = (z / denom) * (p * (1.0 - p) / n_f + z2 / (4.0 * n_f * n_f)).sqrt();
    ((center - margin).max(0.0), (center + margin).min(1.0))
}

/// Two-sided binomial p-value testing `H0: p = 0.5` given an effective
/// win count out of `n` games. Normal approximation via `erfc`.
///
/// With draws, pass `wins + 0.5 * draws` as `effective_wins` — same
/// convention as the Wilson CI above. Returns 1.0 when `n == 0`.
pub fn two_sided_p(effective_wins: f64, n: u32) -> f64 {
    if n == 0 {
        return 1.0;
    }
    let n_f = n as f64;
    // SE of `effective_wins - n/2` under H0 (binomial with p=0.5) is
    // sqrt(n/4). The same approximation holds in the trinomial case
    // when draw rate is moderate.
    let z = (effective_wins - n_f / 2.0).abs() / (n_f / 4.0).sqrt();
    erfc(z / std::f64::consts::SQRT_2)
}

/// Likelihood of Superiority — `P(A is the stronger player)` given
/// observed decisive games. Cutechess-cli formula:
///
/// ```text
///     LOS = 0.5 * erfc((wins_b - wins_a) / sqrt(2 * (wins_a + wins_b)))
///         = 0.5 * (1 + erf((wins_a - wins_b) / sqrt(2 * (wins_a + wins_b))))
/// ```
///
/// Draws drop out of both the numerator (cancel) and the denominator
/// (we condition on a decisive game). Returns 0.5 when there have
/// been no decisive games — no evidence either way.
pub fn los(wins_a: u32, wins_b: u32) -> f64 {
    let total = wins_a + wins_b;
    if total == 0 {
        return 0.5;
    }
    let diff = wins_a as f64 - wins_b as f64;
    let denom = (2.0 * total as f64).sqrt();
    0.5 * (1.0 + erf(diff / denom))
}

/// Abramowitz & Stegun 7.1.26 — polynomial approximation for the
/// error function. `|err| ≤ 1.5e-7` over all `x`. Used by `two_sided_p`
/// and `los`; hand-rolled to avoid a stats-crate dep.
pub fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let a1 = 0.254_829_592;
    let a2 = -0.284_496_736;
    let a3 = 1.421_413_741;
    let a4 = -1.453_152_027;
    let a5 = 1.061_405_429;
    // Horner-evaluated polynomial in t, times exp(-x²).
    let poly = ((((a5 * t + a4) * t + a3) * t + a2) * t + a1) * t;
    sign * (1.0 - poly * (-x * x).exp())
}

/// Complementary error function — `1 - erf(x)`. Pulled out so callers
/// reading "p = erfc(...)" don't have to translate.
pub fn erfc(x: f64) -> f64 {
    1.0 - erf(x)
}

/// Verdict shorthand consumed by the report renderer + `compare` CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// `p < 0.05` AND A's effective win-rate > 0.5 — A is significantly
    /// stronger than B.
    Better,
    /// `p < 0.05` AND A's effective win-rate < 0.5 — A is significantly
    /// weaker than B.
    Worse,
    /// `p >= 0.05` — sample doesn't reject the fair-coin null.
    Inconclusive,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Better => "significant (BETTER)",
            Verdict::Worse => "significant (WORSE)",
            Verdict::Inconclusive => "inconclusive",
        }
    }
}

/// One pair's bundled stats. Convenience for renderers that want
/// every number at once.
#[derive(Debug, Clone, Copy)]
pub struct PairStats {
    pub wins_a: u32,
    pub wins_b: u32,
    pub draws: u32,
    pub n: u32,
    /// Effective win-rate from A's perspective, `(wins_a + 0.5*draws)/n`.
    pub a_win_rate: f64,
    /// Wilson 95% CI on `a_win_rate`.
    pub a_ci_95: (f64, f64),
    pub los: f64,
    pub p_value: f64,
    pub verdict: Verdict,
}

impl PairStats {
    pub fn compute(wins_a: u32, wins_b: u32, draws: u32) -> PairStats {
        let n = wins_a + wins_b + draws;
        let effective_wins_a = wins_a as f64 + 0.5 * draws as f64;
        let a_win_rate = if n == 0 {
            0.5
        } else {
            effective_wins_a / n as f64
        };
        let a_ci_95 = wilson_ci(effective_wins_a, n, Z_95);
        let los_value = los(wins_a, wins_b);
        let p_value = two_sided_p(effective_wins_a, n);
        let verdict = if p_value < 0.05 {
            if a_win_rate > 0.5 {
                Verdict::Better
            } else {
                Verdict::Worse
            }
        } else {
            Verdict::Inconclusive
        };
        PairStats {
            wins_a,
            wins_b,
            draws,
            n,
            a_win_rate,
            a_ci_95,
            los: los_value,
            p_value,
            verdict,
        }
    }

    /// Approximate `n` needed to resolve the observed effect at `p < 0.05`
    /// if it were real. Useful for "INCONCLUSIVE — need ≈ X more games"
    /// epilogues in `compare`. Returns `None` if the gap is already
    /// significant (no more games needed) or the gap is exactly 0.5
    /// (no real effect to detect).
    pub fn rounds_needed_for_significance(&self) -> Option<u32> {
        if !matches!(self.verdict, Verdict::Inconclusive) {
            return None;
        }
        let gap = (self.a_win_rate - 0.5).abs();
        if gap < 1e-6 {
            return None;
        }
        // n ≈ (z_alpha)² * 0.25 / gap² for a binomial proportion test
        // against p = 0.5. Slightly conservative — uses the worst-
        // case variance.
        let n = (Z_95 * Z_95 * 0.25 / (gap * gap)).ceil();
        Some(n as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f64, expected: f64, tol: f64) -> bool {
        (actual - expected).abs() < tol
    }

    #[test]
    fn erf_known_values() {
        // Standard erf values, A&S 7.1.26 accuracy is ~1.5e-7.
        assert!(approx(erf(0.0), 0.0, 1e-7));
        assert!(approx(erf(0.5), 0.520_499_877, 1e-6));
        assert!(approx(erf(1.0), 0.842_700_793, 1e-6));
        assert!(approx(erf(2.0), 0.995_322_265, 1e-6));
        // Antisymmetry
        assert!(approx(erf(-1.0), -0.842_700_793, 1e-6));
    }

    #[test]
    fn wilson_ci_centered_at_p() {
        // 50/100 → 95% CI is approximately (0.404, 0.596).
        let (lo, hi) = wilson_ci(50.0, 100, Z_95);
        assert!(approx(lo, 0.404, 0.01), "lo = {lo}");
        assert!(approx(hi, 0.596, 0.01), "hi = {hi}");
        // Wider at small n.
        let (lo, hi) = wilson_ci(5.0, 10, Z_95);
        assert!(hi - lo > 0.4, "10-game CI width = {}", hi - lo);
    }

    #[test]
    fn wilson_ci_clamps_to_unit_interval() {
        let (lo, _) = wilson_ci(0.0, 10, Z_95);
        assert!(lo >= 0.0);
        let (_, hi) = wilson_ci(10.0, 10, Z_95);
        assert!(hi <= 1.0);
    }

    #[test]
    fn two_sided_p_fair_coin() {
        // 50/50 out of 100 → strong p = 1.0
        assert!(approx(two_sided_p(50.0, 100), 1.0, 0.01));
        // 60/40 out of 100 → p ≈ 0.0455 (just under significant)
        let p = two_sided_p(60.0, 100);
        assert!(approx(p, 0.0455, 0.01), "p = {p}");
        // The headline v1_5 result: 518 effective wins / 1000 → p ≈ 0.26
        let p = two_sided_p(518.0, 1000);
        assert!(approx(p, 0.255, 0.01), "p = {p}");
    }

    #[test]
    fn los_zero_difference() {
        assert!(approx(los(0, 0), 0.5, 1e-9));
        assert!(approx(los(500, 500), 0.5, 1e-9));
    }

    #[test]
    fn los_known_v1_5_vs_v1() {
        // v1_5 had 500 wins, v1 had 464 wins (excluding draws):
        // LOS(v1_5) = 0.5·(1 + erf(36 / √(2·964)))
        //           = 0.5·(1 + erf(36 / 43.91))
        //           = 0.5·(1 + erf(0.8198))
        //           ≈ 0.877
        let l = los(500, 464);
        assert!(approx(l, 0.877, 0.01), "los = {l}");
    }

    #[test]
    fn pair_stats_v1_5_vs_v1_actual_result() {
        // From the 1000-match tournament: v1_5 won 500, v1 won 464, draws 36.
        let s = PairStats::compute(500, 464, 36);
        assert_eq!(s.n, 1000);
        assert!(approx(s.a_win_rate, 0.518, 0.001));
        assert!(s.a_ci_95.0 < 0.518 && s.a_ci_95.1 > 0.518);
        assert!(approx(s.los, 0.877, 0.01));
        assert!(approx(s.p_value, 0.26, 0.05));
        assert_eq!(s.verdict, Verdict::Inconclusive);
    }

    #[test]
    fn pair_stats_clear_winner_is_significant() {
        // 700/300 should be significant.
        let s = PairStats::compute(700, 300, 0);
        assert_eq!(s.verdict, Verdict::Better);
        assert!(s.p_value < 0.05);
    }

    #[test]
    fn rounds_needed_for_inconclusive_v1_5() {
        let s = PairStats::compute(500, 464, 36);
        let needed = s.rounds_needed_for_significance();
        // Gap is 0.018; n ≈ 1.96²·0.25/0.018² ≈ 2965.
        assert!(needed.is_some());
        let n = needed.unwrap();
        assert!(n > 2000 && n < 5000, "needed = {n}");
    }
}
