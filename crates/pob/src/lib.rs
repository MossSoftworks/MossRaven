//! Path of Building 2 headless calc engine for MossRaven.
//!
//! **Fork-and-trim** of [poe2-agent](https://github.com/SFerenczy/poe2-agent)'s
//! `pob` and `pob_parser` modules (MIT, © Sándor Ferenczy). See [NOTICE](../NOTICE)
//! for attribution and [`docs/pob-deepdive.md`](../../docs/pob-deepdive.md)
//! for the extraction rationale and the catalogue of things deliberately
//! discarded (`agent`, `llm`, `tools`, `trade`, `trace`).
//!
//! ## Critical operational notes
//!
//! - `mlua::Lua` with LuaJIT is `!Send`. [`PobParser`] runs the engine on a
//!   dedicated OS thread and talks to it via mpsc + tokio oneshot.
//! - [`PobHeadless::init`] calls `std::env::set_current_dir(pob_src_path)` —
//!   process-global mutation. **One [`PobParser`] per process, ever.**
//! - The vendored PoB2 Lua loads a `lua-utf8` stub we provide because LuaJIT
//!   in safe mode can't load the real C module. Re-test after every
//!   `vendor/PathOfBuilding-PoE2` bump.
//! - Every [`BuildStats`] produced by this crate represents a snapshot under
//!   a specific PoB2 version. Stamp archive entries with the version
//!   (`ArchiveEntry::data_version`) so they can be invalidated after league
//!   patches.

pub mod headless;
pub mod parser;

#[cfg(test)]
mod test_support;

pub use headless::{BuildStats, PobError, PobHeadless};
pub use parser::{PobBackend, PobParseError, PobParser, PobQuery};
