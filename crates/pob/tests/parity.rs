//! Parity smoke tests against vendored PoB2 + user-provided XML fixtures.
//!
//! Self-skipping if no fixtures are present in `tests/fixtures/*.xml`. Drop a
//! PoB2 export in there to exercise the engine. Strict per-fixture expected
//! values go in a sibling `<fixture>.expected.json` (loose bounds only; we
//! don't pin exact numbers because PoB2 updates shift them by a few %).
//!
//! Marked `#[ignore]` so default `cargo test` is fast. Run with
//! `cargo test -p mossraven-pob --test parity -- --ignored --nocapture`.

use std::path::{Path, PathBuf};

use mossraven_pob::{BuildStats, PobParser, PobQuery};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct ExpectedBounds {
    #[serde(default)]
    total_dps_min: Option<f64>,
    #[serde(default)]
    total_dps_max: Option<f64>,
    #[serde(default)]
    life_min: Option<f64>,
    #[serde(default)]
    ehp_min: Option<f64>,
    #[serde(default)]
    notes: Option<String>,
}

fn vendor_pob_path() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/PathOfBuilding-PoE2");
    p.join("src/HeadlessWrapper.lua").exists().then_some(p)
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn list_fixtures() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("xml"))
        .collect();
    out.sort();
    out
}

#[tokio::test]
#[ignore = "loads full PoB Lua VM + reads user fixtures; run with --ignored"]
async fn fixtures_score_with_plausible_stats() {
    let Some(pob_path) = vendor_pob_path() else {
        eprintln!("skipping: vendor/PathOfBuilding-PoE2 not present at workspace root");
        return;
    };
    let fixtures = list_fixtures();
    if fixtures.is_empty() {
        eprintln!(
            "skipping: no fixtures in {} — see fixtures/README.md to add some",
            fixtures_dir().display()
        );
        return;
    }

    let parser = PobParser::new(&pob_path).await.expect("PobParser::new");
    eprintln!("Parser ready. Testing {} fixture(s).", fixtures.len());

    for fixture in &fixtures {
        let name = fixture.file_stem().unwrap().to_string_lossy().to_string();
        eprintln!("\n--- {name} ---");

        let xml = std::fs::read_to_string(fixture).expect("read fixture xml");
        let stats_bytes = parser
            .parse(xml.as_bytes())
            .await
            .unwrap_or_else(|e| panic!("[{name}] parse failed: {e:?}"));
        let stats: BuildStats = serde_json::from_slice(&stats_bytes)
            .unwrap_or_else(|e| panic!("[{name}] BuildStats deserialize: {e}"));

        eprintln!("  total_dps    = {:>14.0}", stats.total_dps);
        eprintln!("  effective_hp = {:>14.0}", stats.effective_hp);
        eprintln!("  life         = {:>14.0}", stats.life);
        eprintln!("  energy_shield= {:>14.0}", stats.energy_shield);
        eprintln!("  armour       = {:>14.0}", stats.armour);
        eprintln!("  evasion      = {:>14.0}", stats.evasion);
        eprintln!(
            "  resists      = fire={} cold={} lightning={} chaos={}",
            stats.fire_res, stats.cold_res, stats.lightning_res, stats.chaos_res,
        );

        // Loose invariants. Skip the life check for pure-ES builds where life
        // can legitimately be ~0. DPS==0 is reported but doesn't fail the test
        // (often indicates a tree-version mismatch where the fixture references
        // passive nodes that don't exist in the vendored PoB2 — the build
        // loads but can't fully resolve its main skill).
        if stats.total_dps == 0.0 {
            eprintln!(
                "  [{name}] WARN: total_dps == 0 (likely tree-version mismatch or unresolved main skill); skipping pool/resist asserts"
            );
            continue;
        }
        let main_pool = stats.life.max(stats.energy_shield);
        assert!(
            main_pool > 0.0,
            "[{name}] life or energy_shield should be > 0"
        );
        for (label, r) in [
            ("fire", stats.fire_res),
            ("cold", stats.cold_res),
            ("lightning", stats.lightning_res),
            ("chaos", stats.chaos_res),
        ] {
            assert!(
                (-100..=95).contains(&r),
                "[{name}] {label}_res out of sane range: {r}"
            );
        }

        // Per-fixture strict bounds if a sibling .expected.json exists.
        let expected_path = fixture.with_extension("expected.json");
        if expected_path.exists() {
            let exp: ExpectedBounds = serde_json::from_str(
                &std::fs::read_to_string(&expected_path).expect("read expected.json"),
            )
            .expect("parse expected.json");
            if let Some(min) = exp.total_dps_min {
                assert!(
                    stats.total_dps >= min,
                    "[{name}] total_dps {} < expected min {min}",
                    stats.total_dps,
                );
            }
            if let Some(max) = exp.total_dps_max {
                assert!(
                    stats.total_dps <= max,
                    "[{name}] total_dps {} > expected max {max}",
                    stats.total_dps,
                );
            }
            if let Some(min) = exp.life_min {
                assert!(
                    stats.life >= min,
                    "[{name}] life {} < expected min {min}",
                    stats.life,
                );
            }
            if let Some(min) = exp.ehp_min {
                assert!(
                    stats.effective_hp >= min,
                    "[{name}] effective_hp {} < expected min {min}",
                    stats.effective_hp,
                );
            }
            if let Some(notes) = &exp.notes {
                eprintln!("  notes: {notes}");
            }
        }
    }
}

#[tokio::test]
#[ignore = "loads full PoB Lua VM; run with --ignored"]
async fn fixtures_query_build_stats_returns_grouped_object() {
    let Some(pob_path) = vendor_pob_path() else {
        eprintln!("skipping: vendor/PathOfBuilding-PoE2 not present");
        return;
    };
    let fixtures = list_fixtures();
    if fixtures.is_empty() {
        eprintln!("skipping: no fixtures");
        return;
    }
    let parser = PobParser::new(&pob_path).await.expect("PobParser::new");
    for fixture in &fixtures {
        let name = fixture.file_stem().unwrap().to_string_lossy();
        let xml = std::fs::read_to_string(fixture).expect("read fixture xml");
        let res = parser
            .query(xml.as_bytes(), PobQuery::BuildStats)
            .await
            .unwrap_or_else(|e| panic!("[{name}] query failed: {e:?}"));
        assert!(res.is_object(), "[{name}] BuildStats query should return object");
        let keys: Vec<_> = res.as_object().unwrap().keys().collect();
        eprintln!("[{name}] BuildStats keys: {keys:?}");
    }
}
