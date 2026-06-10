//! SPEC §1.1.1 viability gate — "easily and cleanly clears all content."
//!
//! Hard floors on PoB-scored stats. Reported, not filtering, in v1: a failing
//! cell stays in the archive (it's still discovery signal) but the frontier
//! API and every Tier-5 guide surface pass/fail + the verbatim failure list
//! so a build is never silently presented as endgame-ready.
//!
//! Floors are league-currency (v1 = PoE2 0.5 "Runes of Aldur") — revisit each
//! patch alongside the vendor PoB2 bump.

use mossraven_pob::BuildStats;
use serde::Serialize;

/// Community-sourced DPS bands (PoE2 0.5):
/// 50–100k = entry-pinnacle baseline; 500k+ = comfortably farming T0
/// pinnacles; 10M+ = min-maxed meta. SPEC §1.1.4 "easily and cleanly clears
/// all content" maps to the COMFORT band, so PASS requires ≥ 500k — but the
/// achieved band is always reported so a 90k build reads "entry-pinnacle
/// baseline", not just "FAIL".
pub const DPS_ENTRY: f64 = 50_000.0;
pub const DPS_CAPABLE: f64 = 100_000.0;
pub const MIN_DPS: f64 = 500_000.0; // PASS floor = comfort band
pub const DPS_META: f64 = 10_000_000.0;
pub const MIN_EHP: f64 = 5_000.0;
pub const RES_CAP: i32 = 75;
pub const MIN_CHAOS_RES: i32 = -30;
/// PoB warns above 8 ascendancy points per ascendancy (Build.lua `ascMax`).
pub const ASC_POINT_CAP: u32 = 8;

/// Which community band a DPS number lands in.
pub fn dps_band(dps: f64) -> &'static str {
    if dps >= DPS_META {
        "meta (10M+)"
    } else if dps >= MIN_DPS {
        "comfort farm (500k+, SPEC pass)"
    } else if dps >= DPS_CAPABLE {
        "endgame capable (100k–500k)"
    } else if dps >= DPS_ENTRY {
        "entry-pinnacle baseline (50k–100k)"
    } else {
        "below entry (<50k)"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ViabilityReport {
    pub pass: bool,
    /// Community DPS band the build lands in, regardless of pass/fail.
    pub dps_band: &'static str,
    /// Human-readable failures, e.g. `"DPS 64,163 < 500,000 floor"`. Empty on pass.
    pub failures: Vec<String>,
}

pub fn check(stats: &BuildStats) -> ViabilityReport {
    let mut failures = Vec::new();

    if stats.total_dps < MIN_DPS {
        failures.push(format!(
            "DPS {:.0} < {:.0} floor — lands in '{}' band",
            stats.total_dps,
            MIN_DPS,
            dps_band(stats.total_dps)
        ));
    }
    if stats.effective_hp < MIN_EHP {
        failures.push(format!(
            "EHP {:.0} < {:.0} floor (endgame burst survival)",
            stats.effective_hp, MIN_EHP
        ));
    }
    for (name, v) in [
        ("fire", stats.fire_res),
        ("cold", stats.cold_res),
        ("lightning", stats.lightning_res),
    ] {
        if v < RES_CAP {
            failures.push(format!("{name} res {v} < {RES_CAP} cap"));
        }
    }
    if stats.chaos_res < MIN_CHAOS_RES {
        failures.push(format!(
            "chaos res {} < {} floor (0.5 chaos-heavy endgame)",
            stats.chaos_res, MIN_CHAOS_RES
        ));
    }

    // Passive point legality — PoB calculates over-budget trees without
    // complaint, so an evolutionary loop would otherwise inflate trees
    // forever. Budgets come from PoB's own model (level-scaled, see
    // BuildStats docs). `points_budget == 0` = stats predate the field or
    // headless couldn't measure: skip rather than fail history.
    if stats.points_budget > 0 {
        if stats.points_used > stats.points_budget {
            failures.push(format!(
                "passive points {} > {} budget at level {} — not buildable in-game",
                stats.points_used, stats.points_budget, stats.character_level
            ));
        }
        if stats.ascendancy_points_used > ASC_POINT_CAP {
            failures.push(format!(
                "ascendancy points {} > {} cap",
                stats.ascendancy_points_used, ASC_POINT_CAP
            ));
        }
        if stats.secondary_ascendancy_points_used > ASC_POINT_CAP {
            failures.push(format!(
                "secondary ascendancy points {} > {} cap",
                stats.secondary_ascendancy_points_used, ASC_POINT_CAP
            ));
        }
        if stats.weapon_set_points_used > stats.weapon_set_points_budget {
            failures.push(format!(
                "weapon-set points {} > {} budget",
                stats.weapon_set_points_used, stats.weapon_set_points_budget
            ));
        }
    }

    ViabilityReport {
        pass: failures.is_empty(),
        dps_band: dps_band(stats.total_dps),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(dps: f64, ehp: f64, res: i32, chaos: i32) -> BuildStats {
        BuildStats {
            total_dps: dps,
            effective_hp: ehp,
            fire_res: res,
            cold_res: res,
            lightning_res: res,
            chaos_res: chaos,
            ..Default::default()
        }
    }

    #[test]
    fn comfort_band_build_passes() {
        let r = check(&stats(650_000.0, 6_500.0, 75, 10));
        assert!(r.pass, "{:?}", r.failures);
        assert!(r.dps_band.contains("comfort"), "{}", r.dps_band);
    }

    #[test]
    fn capable_band_fails_pass_but_reports_band() {
        // 450k clears most content but isn't the SPEC "easily and cleanly"
        // comfort band — FAIL with the honest band label.
        let r = check(&stats(450_000.0, 6_500.0, 75, 10));
        assert!(!r.pass);
        assert!(r.dps_band.contains("endgame capable"), "{}", r.dps_band);
    }

    #[test]
    fn current_archive_scale_fails_honestly() {
        // Today's best archive entry (75.6k DPS) must FAIL the gate — the
        // whole point of SPEC §1.1.4 is that we never oversell these.
        let r = check(&stats(75_580.0, 5_991.0, 75, 0));
        assert!(!r.pass);
        assert_eq!(r.failures.len(), 1);
        assert!(r.failures[0].contains("DPS"), "{:?}", r.failures);
        assert!(r.dps_band.contains("entry-pinnacle"), "{}", r.dps_band);
    }

    #[test]
    fn every_floor_reports_its_own_failure() {
        let r = check(&stats(10.0, 10.0, 10, -60));
        assert!(!r.pass);
        // dps + ehp + 3 elemental res + chaos = 6 distinct failures
        assert_eq!(r.failures.len(), 6, "{:?}", r.failures);
    }

    #[test]
    fn over_budget_tree_fails_legality() {
        let mut s = stats(650_000.0, 6_500.0, 75, 10);
        s.points_used = 130;
        s.points_budget = 121;
        s.character_level = 98;
        let r = check(&s);
        assert!(!r.pass);
        assert!(
            r.failures.iter().any(|f| f.contains("not buildable")),
            "{:?}",
            r.failures
        );
    }

    #[test]
    fn zero_budget_means_unmeasured_and_skips_point_gates() {
        // Pre-existing archive entries deserialize with all point fields 0 —
        // they must not retroactively fail on legality they never measured.
        let mut s = stats(650_000.0, 6_500.0, 75, 10);
        s.points_used = 999;
        let r = check(&s);
        assert!(r.pass, "{:?}", r.failures);
    }

    #[test]
    fn weapon_set_and_ascendancy_overruns_each_report() {
        let mut s = stats(650_000.0, 6_500.0, 75, 10);
        s.points_budget = 121;
        s.points_used = 100;
        s.ascendancy_points_used = 9;
        s.secondary_ascendancy_points_used = 9;
        s.weapon_set_points_used = 30;
        s.weapon_set_points_budget = 24;
        let r = check(&s);
        assert_eq!(r.failures.len(), 3, "{:?}", r.failures);
    }
}
