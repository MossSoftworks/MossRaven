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

pub const MIN_DPS: f64 = 300_000.0;
pub const MIN_EHP: f64 = 5_000.0;
pub const RES_CAP: i32 = 75;
pub const MIN_CHAOS_RES: i32 = -30;

#[derive(Debug, Clone, Serialize)]
pub struct ViabilityReport {
    pub pass: bool,
    /// Human-readable failures, e.g. `"DPS 64,163 < 300,000 floor"`. Empty on pass.
    pub failures: Vec<String>,
}

pub fn check(stats: &BuildStats) -> ViabilityReport {
    let mut failures = Vec::new();

    if stats.total_dps < MIN_DPS {
        failures.push(format!(
            "DPS {:.0} < {:.0} floor (red-map/pinnacle comfort)",
            stats.total_dps, MIN_DPS
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

    ViabilityReport {
        pass: failures.is_empty(),
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
    fn endgame_ready_build_passes() {
        let r = check(&stats(450_000.0, 6_500.0, 75, 10));
        assert!(r.pass, "{:?}", r.failures);
    }

    #[test]
    fn current_archive_scale_fails_honestly() {
        // Today's best archive entry (75.6k DPS) must FAIL the gate — the
        // whole point of SPEC §1.1.4 is that we never oversell these.
        let r = check(&stats(75_580.0, 5_991.0, 75, 0));
        assert!(!r.pass);
        assert_eq!(r.failures.len(), 1);
        assert!(r.failures[0].contains("DPS"), "{:?}", r.failures);
    }

    #[test]
    fn every_floor_reports_its_own_failure() {
        let r = check(&stats(10.0, 10.0, 10, -60));
        assert!(!r.pass);
        // dps + ehp + 3 elemental res + chaos = 6 distinct failures
        assert_eq!(r.failures.len(), 6, "{:?}", r.failures);
    }
}
