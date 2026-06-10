//! Empirical proof for `equip_unique`: swapping gear must change PoB's
//! scored stats on a real build — and gear costs ZERO passive points, so
//! the point-budget guard must stay quiet.
//!
//! The druid fixture's Weapon 1 is a rare Roaring Staff; equipping any
//! unique staff replaces its mods wholesale, so SOME scored stat moves.
//!
//! Run with:
//!   cargo test -p mossraven-core --test item_ops -- --ignored --nocapture

use mossraven_core::mutate::apply_ops_to_xml;
use mossraven_pob::{GemDb, PobHeadless, TreeDb, UniqueDb};
use mossraven_surrogate::MutationOp;
use std::path::{Path, PathBuf};

fn vendor_pob_path() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/PathOfBuilding-PoE2");
    p.join("src/HeadlessWrapper.lua").exists().then_some(p)
}

fn druid_fixture() -> Option<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/pob/tests/fixtures/druid-oracle-tornado.xml");
    std::fs::read_to_string(p).ok()
}

#[test]
#[ignore = "loads full PoB Lua VM — run with --ignored"]
fn equipping_a_unique_staff_changes_pob_scored_stats() {
    let Some(pob_path) = vendor_pob_path() else {
        eprintln!("skipping: vendor not present");
        return;
    };
    let Some(seed) = druid_fixture() else {
        eprintln!("skipping: druid fixture not pulled");
        return;
    };

    let unique_db = UniqueDb::load(&pob_path);
    let staves: Vec<_> = unique_db
        .of_kind("staff")
        .iter()
        .filter(|u| u.is_offense())
        .collect();
    assert!(!staves.is_empty(), "vendor must have offense staves");

    let mut pob = PobHeadless::new().expect("PobHeadless::new");
    pob.init(pob_path.to_str().unwrap()).expect("init");
    pob.load_build_xml(&seed).expect("load seed");
    let base = pob.calculate().expect("calc seed");
    eprintln!(
        "seed:  DPS={:.0} ES={:.0} points {}/{}",
        base.total_dps, base.energy_shield, base.points_used, base.points_budget
    );

    // Probe up to 3 staves — at least one must move a scored stat.
    let mut any_moved = false;
    for u in staves.iter().take(3) {
        let mutated = apply_ops_to_xml(
            &seed,
            &[MutationOp::EquipUnique {
                slot: "Weapon 1".into(),
                name: u.name.clone(),
            }],
            &GemDb::default(),
            &TreeDb::default(),
            &unique_db,
        );
        assert_ne!(seed, mutated, "op must rewrite the XML");
        assert!(
            mutated.contains(&format!("Rarity: UNIQUE\n{}", u.name)),
            "item block inserted"
        );

        pob.load_build_xml(&mutated).expect("load mutated");
        let s = pob.calculate().expect("calc mutated");
        eprintln!(
            "+{:<28} DPS={:.0} ES={:.0} points {}/{}",
            u.name, s.total_dps, s.energy_shield, s.points_used, s.points_budget
        );
        // Gear is point-free: budget accounting must not change.
        assert_eq!(s.points_used, base.points_used, "gear must cost no points");
        if (s.total_dps - base.total_dps).abs() > 0.01
            || (s.energy_shield - base.energy_shield).abs() > 0.01
            || (s.effective_hp - base.effective_hp).abs() > 0.01
        {
            any_moved = true;
        }
    }
    assert!(
        any_moved,
        "equipping unique staves changed NOTHING — the Items/Slot rewiring \
         is not reaching PoB's scoring"
    );
}

#[test]
#[ignore = "loads full PoB Lua VM — run with --ignored"]
fn unknown_unique_is_a_noop() {
    let Some(pob_path) = vendor_pob_path() else { return };
    let Some(seed) = druid_fixture() else { return };
    let unique_db = UniqueDb::load(&pob_path);
    let mutated = apply_ops_to_xml(
        &seed,
        &[MutationOp::EquipUnique {
            slot: "Weapon 1".into(),
            name: "Definitely Not A Real Unique".into(),
        }],
        &GemDb::default(),
        &TreeDb::default(),
        &unique_db,
    );
    assert!(
        !mutated.contains("Definitely Not A Real Unique"),
        "unknown unique must not insert an item"
    );
}
