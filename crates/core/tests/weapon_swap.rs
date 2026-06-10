//! Handoff [6]: validate `SetActiveWeaponSet` on a REAL dual-loadout build.
//!
//! The druid-oracle-tornado fixture carries two distinct staves
//! (`Weapon 1` = Apocalypse Pile, Roaring Staff; `Weapon 1 Swap` =
//! Behemoth Beam, Reaping Staff). Flipping `useSecondWeaponSet` on the
//! active ItemSet must make PoB score the OTHER staff — different weapon
//! mods → different BuildStats. PoB's load path reads exactly this attr
//! (ItemsTab.lua:1122 `node.attrib.useSecondWeaponSet == "true"`).
//!
//! Run with:
//!   cargo test -p mossraven-core --test weapon_swap -- --ignored --nocapture

use mossraven_core::mutate::apply_ops_to_xml;
use mossraven_pob::{GemDb, PobHeadless, TreeDb, UniqueDb};
use mossraven_surrogate::MutationOp;
use std::path::{Path, PathBuf};

fn vendor_pob_path() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/PathOfBuilding-PoE2");
    p.join("src/HeadlessWrapper.lua").exists().then_some(p)
}

fn dual_loadout_fixture() -> Option<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/pob/tests/fixtures/druid-oracle-tornado.xml");
    std::fs::read_to_string(p).ok()
}

#[test]
#[ignore = "loads full PoB Lua VM — run with --ignored"]
fn weapon_set_swap_changes_the_scored_loadout() {
    let Some(pob_path) = vendor_pob_path() else {
        eprintln!("skipping: vendor/PathOfBuilding-PoE2 not present");
        return;
    };
    let Some(seed) = dual_loadout_fixture() else {
        eprintln!("skipping: druid-oracle-tornado fixture not pulled (see scripts/pull-pob-fixtures.py)");
        return;
    };
    // Preconditions: the fixture really is dual-loadout (both weapon slots
    // populated with different items). Guards against a future fixture
    // refresh silently downgrading this test to a no-op.
    assert!(
        seed.contains(r#"itemId="9" itemPbURL="" name="Weapon 1 Swap""#)
            || (seed.contains(r#"name="Weapon 1 Swap""#) && !seed.contains(r#"itemId="0" itemPbURL="" name="Weapon 1 Swap""#)),
        "fixture no longer has a populated Weapon 1 Swap slot"
    );

    let gem_db = GemDb::load(&pob_path);
    let set1 = seed.clone();
    let set2 = apply_ops_to_xml(
        &seed,
        &[MutationOp::SetActiveWeaponSet { use_second: true }],
        &gem_db,
        &TreeDb::default(),
        &UniqueDb::default(),
    );
    assert_ne!(set1, set2, "op must rewrite the XML");
    assert!(
        set2.contains(r#"useSecondWeaponSet="true""#),
        "active ItemSet flag flipped"
    );

    let mut pob = PobHeadless::new().expect("PobHeadless::new");
    pob.init(pob_path.to_str().unwrap()).expect("init");

    pob.load_build_xml(&set1).expect("load set1");
    let s1 = pob.calculate().expect("calc set1");
    pob.load_build_xml(&set2).expect("load set2");
    let s2 = pob.calculate().expect("calc set2");

    eprintln!(
        "set1 (Roaring Staff):  DPS={:.2} EHP={:.2}",
        s1.total_dps, s1.effective_hp
    );
    eprintln!(
        "set2 (Reaping Staff):  DPS={:.2} EHP={:.2}",
        s2.total_dps, s2.effective_hp
    );

    // The two staves carry different mods — SOME scored number must move.
    // (Direction unspecified: the swap staff may be better or worse.)
    let moved = (s1.total_dps - s2.total_dps).abs() > 0.01
        || (s1.effective_hp - s2.effective_hp).abs() > 0.01
        || (s1.energy_shield - s2.energy_shield).abs() > 0.01;
    assert!(
        moved,
        "weapon-set swap did not change any scored stat — \
         PoB is not honoring useSecondWeaponSet from the XML \
         (set1: {s1:?}, set2: {s2:?})"
    );
}
