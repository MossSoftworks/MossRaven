//! Smoke test: can our Lua runtime (currently Lua 5.4) load PoB2's Lua source?
//!
//! Tests the critical compatibility question that the fork-and-trim raised:
//! upstream poe2-agent uses LuaJIT (which PoB2 was designed for); on Windows
//! hosts with `NoDefaultCurrentDirectoryInExePath=1` the LuaJIT MSVC bootstrap
//! breaks, so MossRaven's workspace uses Lua 5.4 instead. This test answers
//! "does PoB2 even initialize" — if PoB2 needs LuaJIT-only features (bit lib,
//! FFI, integer semantics), `init()` fails here and we know to invest in
//! fixing the LuaJIT build path.
//!
//! Self-skipping if vendor/PathOfBuilding-PoE2 isn't present at the workspace
//! root (clean checkouts in CI).

use std::path::{Path, PathBuf};

fn vendor_pob_path() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/PathOfBuilding-PoE2");
    p.join("src/HeadlessWrapper.lua").exists().then_some(p)
}

#[test]
#[ignore = "loads full PoB Lua VM; run with --ignored to verify Lua-runtime compatibility"]
fn pob_headless_initializes_against_vendored_pob2() {
    let Some(pob_path) = vendor_pob_path() else {
        eprintln!(
            "skipping: vendor/PathOfBuilding-PoE2 not present at workspace root — \
             clone it to exercise this test"
        );
        return;
    };

    let mut pob = mossraven_pob::PobHeadless::new().expect("PobHeadless::new should construct a fresh Lua VM");
    pob.init(pob_path.to_str().expect("vendor path must be valid UTF-8"))
        .expect("PobHeadless::init should bootstrap PoB2 — if this fails on a missing module, \
                 PoB2 likely needs LuaJIT-only features that Lua 5.4 can't provide");
    // Reaching here means: Lua VM live + PoB2 Lua loaded + xml.lua/etc. resolvable +
    // lua-utf8 stub accepted. No build loaded yet — that needs a fixture.
}
