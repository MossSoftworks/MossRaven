//! Empirical proof for `allocate_notable`: pathed tree allocations must
//! change PoB's scored stats on a real build.
//!
//! Uses the druid-oracle-tornado fixture (ES chassis). "Insightfulness"
//! (+18% increased maximum Energy Shield) sits 4 hops from the fixture's
//! NORMAL-MODE allocation set — after the op, PoB-reported ES must strictly
//! increase.
//!
//! History: the naive 1-hop anchor for this notable was node 6715, which is a
//! `<WeaponSet2>` allocation. PoB's `CanPathThroughAllocMode` refuses to walk
//! a normal-mode node through a set-mode neighbor, and BuildAllDependsAndPaths
//! orphan-pruned the appended node silently (alloc=true → DeallocSingleNode).
//! The applier now anchors only on mode-0 nodes and treats WS nodes as walls.
//!
//! Run with:
//!   cargo test -p mossraven-core --test tree_alloc -- --ignored --nocapture

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
fn allocated_notable_changes_pob_scored_stats() {
    let Some(pob_path) = vendor_pob_path() else {
        eprintln!("skipping: vendor not present");
        return;
    };
    let Some(seed) = druid_fixture() else {
        eprintln!("skipping: druid fixture not pulled");
        return;
    };

    let tree_db = TreeDb::load(&pob_path);
    let gem_db = GemDb::default();

    let mutated = apply_ops_to_xml(
        &seed,
        &[MutationOp::AllocateNotable {
            name: "Insightfulness".into(),
        }],
        &gem_db,
        &tree_db,
        &UniqueDb::default(),
    );
    assert_ne!(seed, mutated, "op must rewrite Spec nodes");
    // The path was appended — node count strictly grew.
    let count = |x: &str| {
        x.split("nodes=\"").nth(1).unwrap().split('"').next().unwrap().split(',').count()
    };
    assert!(count(&mutated) > count(&seed), "allocation set grew");

    let mut pob = PobHeadless::new().expect("PobHeadless::new");
    pob.init(pob_path.to_str().unwrap()).expect("init");

    // What did the applier actually append?
    let nodes_of = |x: &str| {
        x.split("nodes=\"").nth(1).unwrap().split('"').next().unwrap().to_string()
    };
    let seed_nodes = nodes_of(&seed);
    let mut_nodes = nodes_of(&mutated);
    eprintln!("appended: {}", &mut_nodes[seed_nodes.len()..]);

    // CONTROL: gut 30 allocated nodes. If PoB's score does NOT drop, the
    // Spec edit channel itself is broken (PoB ignores our rewrite) and the
    // notable assertion below is meaningless.
    let gutted_csv: Vec<&str> = seed_nodes.split(',').skip(30).collect();
    let gutted = seed.replace(&seed_nodes, &gutted_csv.join(","));

    pob.load_build_xml(&seed).expect("load seed");
    let base = pob.calculate().expect("calc seed");
    pob.load_build_xml(&gutted).expect("load gutted");
    let g = pob.calculate().expect("calc gutted");
    eprintln!(
        "control (30 nodes removed): ES={:.0} (base {:.0}) DPS={:.0} (base {:.0})",
        g.energy_shield, base.energy_shield, g.total_dps, base.total_dps
    );
    assert!(
        g.energy_shield < base.energy_shield || g.total_dps < base.total_dps,
        "CONTROL FAILED: removing 30 allocated nodes changed nothing — \
         the Spec edit channel is not reaching PoB"
    );

    pob.load_build_xml(&mutated).expect("load mutated");
    let with_notable = pob.calculate().expect("calc mutated");

    eprintln!(
        "seed:    ES={:.0} DPS={:.0} EHP={:.0}",
        base.energy_shield, base.total_dps, base.effective_hp
    );
    eprintln!(
        "+Insightfulness: ES={:.0} DPS={:.0} EHP={:.0}",
        with_notable.energy_shield, with_notable.total_dps, with_notable.effective_hp
    );
    eprintln!(
        "points: seed {}/{} (lvl {}, asc {}+{}, ws {}/{}) → mutated {}/{}",
        base.points_used,
        base.points_budget,
        base.character_level,
        base.ascendancy_points_used,
        base.secondary_ascendancy_points_used,
        base.weapon_set_points_used,
        base.weapon_set_points_budget,
        with_notable.points_used,
        with_notable.points_budget,
    );
    assert!(base.points_budget > 0, "point budget must be measured");
    // The 4-hop path costs ≤4 REAL points (PoB's CountAllocNodes skips
    // isFreeAllocate nodes — 2 of these 4 measured free on 0.5 data).
    assert!(
        with_notable.points_used > base.points_used
            && with_notable.points_used <= base.points_used + 4,
        "path cost out of range: {} → {}",
        base.points_used,
        with_notable.points_used
    );
    // Honesty check that motivated the whole budget feature: this fixture
    // sits at EXACTLY budget (123/123 @ 98), so ANY costed growth is
    // unbuildable in-game and the engine guard must be dropping it.
    assert!(
        with_notable.points_used > with_notable.points_budget,
        "expected over-budget after growth: {}/{}",
        with_notable.points_used,
        with_notable.points_budget
    );

    assert!(
        with_notable.energy_shield > base.energy_shield,
        "+18% max ES notable must raise PoB-scored ES \
         (base {:.0} → {:.0})",
        base.energy_shield,
        with_notable.energy_shield
    );
}

#[test]
#[ignore = "loads full PoB Lua VM — run with --ignored"]
fn unreachable_notable_is_a_noop() {
    let Some(pob_path) = vendor_pob_path() else { return };
    let Some(seed) = druid_fixture() else { return };
    let tree_db = TreeDb::load(&pob_path);
    let mutated = apply_ops_to_xml(
        &seed,
        &[MutationOp::AllocateNotable {
            name: "Definitely Not A Real Notable".into(),
        }],
        &GemDb::default(),
        &tree_db,
        &UniqueDb::default(),
    );
    // Only the Skills-defaults flip (any non-empty op list triggers it) may
    // differ; the Spec nodes must be untouched.
    let nodes = |x: &str| x.split("nodes=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    assert_eq!(nodes(&seed), nodes(&mutated), "unknown notable must not touch the tree");
}
