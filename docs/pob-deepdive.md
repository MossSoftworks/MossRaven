# Deep-dive: poe2-agent's `pob` module

**Subject:** [SFerenczy/poe2-agent](https://github.com/SFerenczy/poe2-agent) @ shallow-cloned 2026-06-08, `src/pob.rs` (3017 lines) + `src/pob_parser.rs` (512 lines).

**Question:** Fork-and-trim into our `crates/pob/` (Option A), or cleanroom mlua wrapper (Option B)?

**Verdict: Option A — fork-and-trim, with very high confidence.** Salvage is enormous, cost is hours not weeks, and the modules are essentially already designed for extraction.

---

## What's actually there

### `src/pob.rs` — the Lua engine wrapper

- **Imports:** `mlua::{Lua, Result}`, `std::collections::{HashMap, HashSet, VecDeque}`, `std::path::Path`, `thiserror::Error`. **That's the entire dep list.** No HTTP client, no LLM, no agent code, no reqwest.
- **Public surface (4 items):**
  - `PobError` (thiserror enum: Lua / NotInitialized / CalculationFailed / Io)
  - `PobHeadless` struct (wraps `mlua::Lua` + init state + PoB src path)
  - `BuildStats` struct (serde-derived: total_dps, effective_hp, life, ES, armour, evasion, 4× resists)
  - `impl PobHeadless` — 1280 lines of methods, lines 137–1417
- **Embedded Lua corpus (~1300 lines):** 9 hand-crafted Lua query templates as Rust `const &str`:
  - `LUA_SKILL_BREAKDOWN`, `LUA_SEARCH_BASES`, `LUA_SEARCH_MODS`, `LUA_SEARCH_GEMS`, `LUA_SEARCH_UNIQUES`, `LUA_LIST_CHARMS`, `LUA_CREATE_ITEM`, `LUA_SEARCH_RUNES`, `LUA_GEAR_MOD_ANALYSIS`
  - These wrap PoB2's internal Lua API to extract structured JSON. **Months of work to rewrite.**
- **Helpers:** three macros (`lua_get!`, `lua_json_insert!`, `lua_json_map!`) for ergonomic Lua-table → serde_json conversion; helper struct `ItemFields`; freestanding fns for reading mod lines / node stats / pattern matching.
- **Zero `unsafe`.** Entirely safe Rust via mlua's API.
- **Initialization gotcha** ([pob.rs:157](src/pob.rs#L157)): `init()` does `std::env::set_current_dir(pob_src_path)` because PoB2's Lua files use relative `dofile()` calls. This mutates **process-global** CWD. The PobParser dedicated-thread pattern is what makes this safe — one PobParser per process, ever. Document this loudly.
- **Lua-utf8 stub:** PoB2 needs a `lua-utf8` C module that can't load in mlua's safe mode; pob.rs provides a pure-Lua stub (`package.preload['lua-utf8']`). Useful precedent — don't break it.

### `src/pob_parser.rs` — the thread-safety wrapper

- **Imports:** `std::path`, `std::sync::mpsc`, `std::thread`, `tokio::sync::oneshot`, `crate::pob::PobHeadless`. Also clean.
- **`PobBackend` trait (`pub(crate)`):** abstraction over the engine so tests can inject a fake without loading Lua. Three methods: `load_build_xml`, `calculate_json`, `query`. **This is the right seam for our mock — keep it, possibly promote to `pub` if we want external mocks.**
- **`PobQuery` enum (18 variants):** the full set of structured queries the engine answers — BuildStats, SkillList, Config, Item, Jewel, PassiveTree, PassiveStats, EquippedItems, UnallocatedAscendancy, SkillBreakdown, GearModAnalysis, SearchGems, SearchUniques, ListCharms, SearchRunes, SearchBases, SearchMods, CreateItem. **All of these are useful for mutation operators in our search loop**, not just BuildStats — e.g. SearchGems/SearchUniques tells the surrogate what's even legal to mutate to.
- **`PobParser` struct:** the public async handle. `Send + Sync`. Spawns a dedicated OS thread on `new()`, sends requests over `mpsc::Sender<PobRequest>`, replies come back over `tokio::oneshot`. Has a proper `Drop` impl that signals the thread to exit and joins.
- **Awaited init:** `PobParser::new(pob_path).await` only resolves after the worker thread has successfully loaded PoB2. Fail-fast on bad install.

---

## Why fork-and-trim wins decisively

| Dimension | Fork-and-trim (A) | Cleanroom (B) |
|---|---|---|
| Time to first parity-check vs desktop PoB2 | hours (copy files, fix module paths, add Cargo deps) | weeks (rewrite ~1300 lines of debugged Lua + mlua plumbing) |
| Risk of subtle calc divergence | low (working code) | high (every Lua query is a re-derivation) |
| Unwanted deps inherited | **none** — pob module imports mlua + std + thiserror, all stuff we'd use anyway | n/a |
| LLM/HTTP coupling to scrub | **none** at the pob layer | n/a |
| License burden | MIT — preserve LICENSE + add NOTICE crediting SFerenczy | none |
| Forward maintenance | rebase against upstream occasionally if they fix PoB2 patch breakage | we own the patch-tracking cost |
| Risk of inheriting their style | low — pob/pob_parser are small, idiomatic, well-commented | n/a |

The cleanroom version of B's "we own every line" argument doesn't survive contact with the actual code. The thing we'd be "owning" is the exact same shape — single dedicated thread, mpsc + oneshot, PobBackend trait — because that's the only sane design when `mlua::Lua` is `!Send`. We'd write the same code, just slower and buggier.

---

## What to discard from poe2-agent

| File | Action | Reason |
|---|---|---|
| `src/pob.rs` | **Keep.** Copy into `crates/pob/src/headless.rs` (or split: macros into `lua_macros.rs`, lua corpus into a `lua_queries/` submod). | The whole point. |
| `src/pob_parser.rs` | **Keep.** Copy into `crates/pob/src/parser.rs`. Adjust `crate::pob::PobHeadless` import to `crate::headless::PobHeadless`. | Thread-safety wrapper we'd otherwise have to rewrite. |
| `src/agent.rs` | Discard | ReAct LLM loop. Replaced by our orchestration core. |
| `src/llm.rs` | Discard | OpenAI Responses client. Spec bans OpenAI. |
| `src/tools/` | Discard | LLM tool schemas. Tier 1 invents its own. |
| `src/trade.rs` | Defer (don't copy into v1, may revisit) | Useful for economy grounding but we use mcpmarket/poe2 MCP for that. |
| `src/trace.rs` | Discard | We use plain `tracing` crate, no custom event format. |
| `src/test_support.rs` | Skim once, then discard | Mostly OpenAI mocks. Their PobBackend fake pattern is worth copying; we'll write our own. |
| `Cargo.toml` deps | Trim to: mlua (with `luajit` + `vendored`), tokio (with `rt` + `sync`), thiserror, anyhow, tracing, serde, serde_json. | Drop: reqwest, async-stream, futures-core, futures-lite, async-trait. |

---

## Attribution + license plan

- Repo `LICENSE` of poe2-agent is MIT (Copyright (c) Sándor Ferenczy).
- Action: ship a `NOTICE` file in `crates/pob/` that includes the upstream MIT notice verbatim and credits the salvage. Keep MIT for our crate.
- The `pob.rs` file-level doc comment becomes a module-level comment in our `crates/pob/src/lib.rs` with a `Salvaged from poe2-agent (MIT, Sándor Ferenczy)` line.

---

## Things to log / be careful of after the fork

1. **Process-global CWD mutation in `init()`** — write a loud module-level doc warning. One `PobParser` per process. Don't spawn two.
2. **`mlua` 0.10 with `luajit` + `vendored` features** — `vendored` builds LuaJIT from source. CI/build needs a C toolchain (cmake/perl on Windows). Note in README.
3. **`lua-utf8` stub** — only handles ASCII fast-path. If any newly-vendored PoB2 update relies on real utf8 handling for an item/skill name, will silently misbehave. Re-test after every PoB2 vendor bump.
4. **PoB2 version coupling** — every salvaged Lua query embeds assumptions about PoB2's internal API. PoB2 bumps may break individual queries. **Version-stamp every archive entry** with `PoB2_version` per the spec, and add a smoke-test that runs each `PobQuery` variant against a fixed test build after any vendor bump.
5. **The PobBackend trait is `pub(crate)`** — promote to `pub` in our crate so consumer crates (`core`, integration tests) can inject a fake without our crate-internal type. Small API decision.
6. **Sandbox surface** — `init()` does `set_current_dir`, loads arbitrary Lua from disk, and overrides `package.path`. This is fine for trusted vendored PoB2 in our own process but means the engine **must not** ever run user-supplied Lua. Document.

---

## Final extraction recipe (executable in the scaffold step)

```
crates/pob/
├── Cargo.toml                (mlua "luajit","vendored" + tokio sync + thiserror + serde)
├── NOTICE                    (upstream MIT + attribution)
├── src/
│   ├── lib.rs                (mod declarations + re-exports)
│   ├── lua_macros.rs         (lua_get!, lua_json_insert!, lua_json_map!)
│   ├── headless.rs           (PobHeadless, PobError, BuildStats — from upstream pob.rs)
│   ├── parser.rs             (PobParser, PobBackend, PobQuery, PobParseError — from upstream pob_parser.rs)
│   └── lua_queries/          (the 9 LUA_* const &str blocks, one per file, for readability)
│       ├── skill_breakdown.lua
│       ├── search_bases.lua
│       ├── search_mods.lua
│       ├── search_gems.lua
│       ├── search_uniques.lua
│       ├── list_charms.lua
│       ├── create_item.lua
│       ├── search_runes.lua
│       └── gear_mod_analysis.lua
└── tests/
    └── parity_smoke.rs       (load a known PoB XML, assert stats match desktop PoB2)
```

The `lua_queries/` reorganization is the only meaningful refactor in the lift — embedding 1300 lines of Lua as Rust string literals in a single `.rs` file makes navigation miserable. Pulling them into `.lua` files included via `include_str!` is a 30-minute mechanical lift and a permanent win for legibility. (Optional for first commit; can defer to a second pass.)

**Estimated extraction effort: 2–4 hours.** Cleanroom alternative: weeks, with worse correctness.

---

## Post-extraction status (2026-06-08, updated end-of-day)

The fork-and-trim landed and `cargo build --workspace --release` is clean with LuaJIT. The init smoke test ([`crates/pob/tests/init_smoke.rs`](../crates/pob/tests/init_smoke.rs)) instantiates `PobHeadless`, calls `init()` against `vendor/PathOfBuilding-PoE2`, and exits successfully — PoB2 fully bootstraps under LuaJIT (Launch.lua → passive tree load → uniques + rares loaded → `Startup time: 0 ms`).

### LuaJIT-on-Windows: the actual fix

Upstream `poe2-agent` uses `mlua` with the `luajit` feature. On Windows hosts that block CWD execute-resolution (this can come from Group Policy, WDAC, or a kernel-level `NoDefaultCurrentDirectoryInExePath` policy that is **independent of the env var of the same name**), LuaJIT's MSVC bootstrap fails:

```
msvcbuild.bat:
  cl /c host\minilua.c            → minilua.obj
  link /out:minilua.exe minilua.obj  → minilua.exe (lives in CWD)
  minilua ..\dynasm\dynasm.lua ...   ← FAILS: 'minilua' is not recognized
```

The bare-name invocation `minilua` triggers the CWD-search policy. **The env var doesn't help** — empirically tested: clearing `NoDefaultCurrentDirectoryInExePath` via bash prefix, cargo `[env]` config, and `cmd /c "set NoDefaultCurrentDirectoryInExePath="` all leave the bare invocation failing. Even with the env var verifiably empty inside cmd, bare `minilua` is blocked, while `.\minilua` and absolute paths work.

**Fix:** [`scripts/patch-luajit-msvc.ps1`](../scripts/patch-luajit-msvc.ps1). Patches both `extras/msvcbuild.bat` and `luajit2/src/msvcbuild.bat` in the cargo registry to prefix `minilua` and `buildvm` invocations with `.\`. Idempotent. Run once after a fresh checkout or after `cargo update` bumps `luajit-src`.

```powershell
.\scripts\patch-luajit-msvc.ps1
cargo clean -p mlua-sys     # if a prior unpatched build was cached
cargo build --workspace --release
```

**Durable fix path (not implemented yet):** fork the `luajit-src` crate with the `.\` prefix baked in, pin via `[patch.crates-io]` in workspace `Cargo.toml`. That removes the need for the manual script and makes the workspace self-bootstrap on any Windows host.

### Lua compatibility shim — now benign, kept as fallback

While diagnosing the LuaJIT build, the workspace temporarily ran on `lua54` and then `lua51`/`lua52` features. Both required a shim block in [`headless.rs`](../crates/pob/src/headless.rs) covering PoB2's LuaJIT-isms. With the LuaJIT build now working, the shim is **inert** — every shim is `if X == nil then ...` and LuaJIT provides all of `jit`, `bit`, `unpack`, `loadstring`, `table.maxn`, lenient `string.format` natively. The block stays in place as a documented safety net if anyone ever needs to swap mlua's feature to non-LuaJIT for diagnostic reasons.

### Shim trade-off matrix (reference)

| mlua feature | `jit` | `bit` | `unpack`/`loadstring`/`setfenv`/`table.maxn` | `goto` syntax | `string.format("%d", float)` | Windows-bootstrap pain |
|---|---|---|---|---|---|---|
| `luajit` (**current**) | native | native | native | native | lenient | needs patch-luajit-msvc.ps1 |
| `lua51` | shim | shim | native | **PARSE ERROR** (PoB2 needs 5.2+) | lenient | none |
| `lua52` | shim | `bit32` alias | shim | native | lenient | none |
| `lua53` | shim | native ops | shim | native | strict (need string.format shim) | none |
| `lua54` | shim | native ops | shim | native | strict (need string.format shim) | none |

LuaJIT wins on every axis except the one-time Windows bootstrap fix.

### Production deployment

End-user binaries built on this Windows host now ship with LuaJIT statically linked (via [`dist/mossraven-service.exe`](../dist/mossraven-service.exe)). The `NoDefaultCurrentDirectoryInExePath` policy only affects the *build*; runtime execution of the linked binary is unaffected. End users never touch the patch script.

### Consequence: Lua-5.1 → Lua-5.4 compatibility shim

PoB2 is Lua-5.1-flavored (matches LuaJIT's runtime semantics). Lua 5.4 differs in several places that bite PoB2's code. [`headless.rs`](../crates/pob/src/headless.rs) embeds a shim block right after `lua-utf8` setup that handles the known hazards:

| Shim | Why | Status |
|---|---|---|
| `jit` table (single `jit.opt.start` call) | Lua 5.4 has no `jit` global | ✅ stubbed |
| `unpack` (now `table.unpack`) | Removed in 5.2 | ✅ aliased |
| `loadstring` (now `load`) | Removed in 5.2 | ✅ aliased |
| `table.maxn` | Removed in 5.2 | ✅ polyfilled |
| `bit` library (BitOp API) | Lua 5.4 uses native operators | ✅ mapped to native ops |
| **`string.format("%d", float)` strictness** | Lua 5.3+ errors when `%d` gets a non-integer; LuaJIT coerced silently | ❌ open — see CalcDefence.lua:2155 |
| `setfenv` / `getfenv` (PoB2's Export tooling only) | Removed in 5.2 | ⏸ not shimmed; calc path doesn't use it |

### What "open" means for `string.format`

PoB2 passes floats to `%d` specifiers in numerous places. The fix is either:
1. **Monkey-patch `string.format`** to floor numeric args when `%d`/`%x`/`%X` is in the spec. Maintenance: medium, brittle if PoB2 starts depending on the strict behavior.
2. **Use mlua's `lua51` or `lua52` feature instead.** Those Lua versions don't have integer-strictness. `lua51` would also restore `unpack`/`loadstring`/`setfenv` natively. Need to confirm `lua-src` crate ships those.
3. **Solve the LuaJIT-on-Windows build** properly: fork the [`luajit-src`](https://crates.io/crates/luajit-src) crate, patch `src/lib.rs:242` to invoke `.\minilua.exe` instead of bare `minilua`, point our Cargo.toml at the fork via `[patch.crates-io]`. Or build on Linux/WSL and ship the binary.

**Recommended:** option 2 (`mlua` `lua51` feature) if available — closest semantic match to LuaJIT, eliminates the entire shim block, no Windows build hassle. If `lua51` isn't supported by `mlua` + `lua-src` on Windows, option 3 with a forked `luajit-src` is the long-term win and a small enough fork to maintain.

### Production deployment

End-user binaries can be built on Linux (where LuaJIT compiles fine) and cross-compiled to Windows — the runtime LuaJIT statically linked into the final `.exe` doesn't care about the host's `NoDefaultCurrentDirectoryInExePath`. So the dev-vs-distribution split is:
- **Dev builds on Windows:** Lua 5.4 + shim (current state)
- **Release builds for Windows users:** LuaJIT + no shim, built on a Linux machine

Document the Linux release-build path in the production runbook once we ship binaries.
