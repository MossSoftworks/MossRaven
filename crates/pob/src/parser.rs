//! Thread-safe PoB XML parser.
//!
//! Wraps `PobHeadless` on a dedicated OS thread (mlua LuaJIT is `!Send`)
//! and communicates via channels.

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use tokio::sync::oneshot;

use crate::headless::PobHeadless;

/// Abstraction over the underlying PoB backend.
///
/// Production uses [`PobHeadless`] (mlua-backed, `!Send`). Tests inject a
/// fake to avoid loading the Lua VM. The backend lives exclusively on the
/// worker thread so it doesn't need `Send` itself — only the factory that
/// constructs it does (see [`PobParser::new_with_backend`]).
///
/// Promoted to `pub` (was `pub(crate)` upstream) so integration tests in
/// dependent crates can inject a fake without our crate-internal type.
/// See `docs/pob-deepdive.md`.
pub trait PobBackend {
    /// Load XML into the backend (state-mutating in the production impl).
    fn load_build_xml(&self, xml: &str) -> Result<(), PobParseError>;

    /// Run `calculate()` and return the serialized [`crate::headless::BuildStats`].
    fn calculate_json(&self) -> Result<Vec<u8>, PobParseError>;

    /// Dispatch a query against the currently-loaded build.
    fn query(&self, query: &PobQuery) -> Result<serde_json::Value, PobParseError>;
}

impl PobBackend for PobHeadless {
    fn load_build_xml(&self, xml: &str) -> Result<(), PobParseError> {
        PobHeadless::load_build_xml(self, xml)
            .map_err(|e| PobParseError::InvalidBuild(e.to_string()))
    }

    fn calculate_json(&self) -> Result<Vec<u8>, PobParseError> {
        let stats = self
            .calculate()
            .map_err(|e| PobParseError::InvalidBuild(e.to_string()))?;
        serde_json::to_vec(&stats).map_err(|e| PobParseError::InvalidBuild(e.to_string()))
    }

    fn query(&self, query: &PobQuery) -> Result<serde_json::Value, PobParseError> {
        dispatch_query(self, query).map_err(|e| PobParseError::InvalidBuild(e.to_string()))
    }
}

fn dispatch_query(
    pob: &PobHeadless,
    query: &PobQuery,
) -> Result<serde_json::Value, crate::headless::PobError> {
    match query {
        PobQuery::BuildStats => pob.query_build_stats(),
        PobQuery::SkillList => pob.query_skill_list(),
        PobQuery::Config => pob.query_config(),
        PobQuery::Item(ref slot) => pob.query_item(slot),
        PobQuery::Jewel(node_id) => pob.query_jewel(*node_id),
        PobQuery::PassiveTree => pob.query_passive_tree(),
        PobQuery::PassiveStats { ref stats, radius } => pob.query_passive_stats(stats, *radius),
        PobQuery::EquippedItems => pob.query_equipped_items(),
        PobQuery::UnallocatedAscendancy => pob.query_unallocated_ascendancy(),
        PobQuery::SkillBreakdown(ref skill) => pob.query_skill_breakdown(skill),
        PobQuery::GearModAnalysis(ref slot) => pob.query_gear_mod_analysis(slot),
        PobQuery::SearchGems {
            ref query,
            ref gem_type,
            ref tags,
        } => pob.query_search_gems(query.as_deref(), gem_type.as_deref(), tags),
        PobQuery::SearchUniques {
            ref query,
            ref slot,
            ref min_level,
            ref max_level,
        } => pob.query_search_uniques(query.as_deref(), slot.as_deref(), *min_level, *max_level),
        PobQuery::ListCharms => pob.query_list_charms(),
        PobQuery::SearchRunes {
            ref query,
            ref slot,
        } => pob.query_search_runes(query.as_deref(), slot.as_deref()),
        PobQuery::SearchBases {
            ref item_type,
            ref query,
        } => pob.query_search_bases(item_type.as_deref(), query.as_deref()),
        PobQuery::SearchMods {
            ref query,
            ref item_type_tag,
            ref mod_type,
        } => pob.query_search_mods(
            query.as_deref(),
            item_type_tag.as_deref(),
            mod_type.as_deref(),
        ),
        PobQuery::CreateItem {
            ref slot,
            ref item_text,
        } => pob.create_item(slot, item_text),
    }
}

/// Which query to run against a loaded build.
#[derive(Debug, Clone)]
pub enum PobQuery {
    /// Extended stats (~40 fields) grouped by category.
    BuildStats,
    /// Per-skill DPS + gem links.
    SkillList,
    /// Configuration flags.
    Config,
    /// Item equipped in the given slot.
    Item(String),
    /// Jewel socketed in the given passive tree socket node.
    Jewel(i64),
    /// Allocated passive tree nodes.
    PassiveTree,
    /// Stat contribution from allocated passives and nearby unallocated nodes.
    PassiveStats { stats: Vec<String>, radius: u32 },
    /// All equipped items with compact mod summaries, jewels, and empty slots.
    EquippedItems,
    /// Ascendancy nodes: allocated vs available for primary and secondary ascendancies.
    UnallocatedAscendancy,
    /// Detailed DPS breakdown for a specific skill.
    SkillBreakdown(String),
    /// Gear mod analysis: tier info, roll quality, upgrade potential.
    GearModAnalysis(String),
    /// Search gem database by name, type, and/or tags.
    SearchGems {
        query: Option<String>,
        gem_type: Option<String>,
        tags: Vec<String>,
    },
    /// Search unique item database by name, slot, and/or level range.
    SearchUniques {
        query: Option<String>,
        slot: Option<String>,
        min_level: Option<u32>,
        max_level: Option<u32>,
    },
    /// List all charm bases with trigger, buff, duration, and charges.
    ListCharms,
    /// Search rune/soul core database by name, stat text, and/or slot.
    SearchRunes {
        query: Option<String>,
        slot: Option<String>,
    },
    /// Search item bases by type and/or name substring.
    SearchBases {
        item_type: Option<String>,
        query: Option<String>,
    },
    /// Search item mods by stat text, item type tag, and/or mod type.
    SearchMods {
        query: Option<String>,
        item_type_tag: Option<String>,
        mod_type: Option<String>,
    },
    /// Create an item from PoB text format, equip it in a slot, and return stat delta.
    CreateItem { slot: String, item_text: String },
}

/// Request sent to the dedicated parser thread.
enum PobRequest {
    Parse {
        xml: String,
        reply: oneshot::Sender<Result<Vec<u8>, PobParseError>>,
    },
    Query {
        xml: String,
        query: PobQuery,
        reply: oneshot::Sender<Result<serde_json::Value, PobParseError>>,
    },
}

/// Errors from build parsing.
#[derive(Debug, thiserror::Error)]
pub enum PobParseError {
    /// PoB couldn't parse the XML (bad data from the user).
    #[error("invalid build: {0}")]
    InvalidBuild(String),

    /// The parser thread died or is unreachable.
    #[error("parser unavailable")]
    Unavailable,
}

/// Thread-safe handle to a `PobHeadless` instance running on a dedicated OS thread.
///
/// `mlua::Lua` with LuaJIT is `!Send`, so we keep it pinned to one thread and
/// communicate via channels. This handle is `Send + Sync` and cheap to clone.
pub struct PobParser {
    sender: Option<mpsc::Sender<PobRequest>>,
    _thread: Option<thread::JoinHandle<()>>,
}

impl PobParser {
    /// Spawn the parser thread and initialize `PobHeadless`.
    ///
    /// Awaits until PoB is fully initialized. Returns an error if
    /// initialization fails so the server can fail-fast at startup.
    pub async fn new(pob_path: &Path) -> Result<Self, anyhow::Error> {
        let (tx, rx) = mpsc::channel::<PobRequest>();
        let (init_tx, init_rx) = oneshot::channel::<Result<(), String>>();

        let pob_path_abs = pob_path
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("pob_path {}: {e}", pob_path.display()))?;
        let pob_path_str_raw = pob_path_abs
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("pob_path is not valid UTF-8"))?;
        // Windows canonicalize() returns extended-length UNC form `\\?\C:\...`.
        // The literal `?` collides with Lua's package.path template (where `?`
        // is the module-name placeholder), so PoB2's `require("xml")` resolves
        // to `//xml/C:/...` and fails. Strip the prefix; Windows accepts both
        // forms for set_current_dir / file I/O.
        let pob_path_str = pob_path_str_raw
            .strip_prefix(r"\\?\")
            .unwrap_or(pob_path_str_raw)
            .to_owned();

        let handle = thread::spawn(move || {
            run_parser_thread(
                move || -> Result<Box<dyn PobBackend>, String> {
                    let mut pob = PobHeadless::new()
                        .map_err(|e| format!("failed to create Lua runtime: {e}"))?;
                    pob.init(&pob_path_str).map_err(|e| e.to_string())?;
                    Ok(Box::new(pob))
                },
                init_tx,
                rx,
            );
        });

        let init_result = init_rx
            .await
            .map_err(|_| anyhow::anyhow!("parser thread died during init"))?;

        init_result.map_err(|e| anyhow::anyhow!("PobHeadless init failed: {e}"))?;

        tracing::info!("PobParser ready");
        Ok(Self {
            sender: Some(tx),
            _thread: Some(handle),
        })
    }

    /// Test-only constructor. Spawns a parser thread backed by a fake.
    ///
    /// The `factory` closure runs on the parser thread and returns the
    /// backend (or an init error). Used by unit tests to exercise the
    /// threading / error-surfacing / shutdown logic without loading Lua.
    #[cfg(test)]
    pub(crate) async fn new_with_backend<F>(factory: F) -> Result<Self, anyhow::Error>
    where
        F: FnOnce() -> Result<Box<dyn PobBackend>, String> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<PobRequest>();
        let (init_tx, init_rx) = oneshot::channel::<Result<(), String>>();

        let handle = thread::spawn(move || {
            run_parser_thread(factory, init_tx, rx);
        });

        let init_result = init_rx
            .await
            .map_err(|_| anyhow::anyhow!("parser thread died during init"))?;
        init_result.map_err(|e| anyhow::anyhow!("backend init failed: {e}"))?;

        Ok(Self {
            sender: Some(tx),
            _thread: Some(handle),
        })
    }

    /// Parse a PoB XML export, returning the `BuildStats` as JSON bytes.
    pub async fn parse(&self, xml: &[u8]) -> Result<Vec<u8>, PobParseError> {
        let xml_str =
            std::str::from_utf8(xml).map_err(|e| PobParseError::InvalidBuild(e.to_string()))?;

        let (reply_tx, reply_rx) = oneshot::channel();

        self.sender
            .as_ref()
            .ok_or(PobParseError::Unavailable)?
            .send(PobRequest::Parse {
                xml: xml_str.to_owned(),
                reply: reply_tx,
            })
            .map_err(|_| PobParseError::Unavailable)?;

        reply_rx.await.map_err(|_| PobParseError::Unavailable)?
    }

    /// Run a query against a build. The build XML is loaded fresh each time
    /// to avoid interleaving problems with concurrent callers.
    pub async fn query(
        &self,
        xml: &[u8],
        query: PobQuery,
    ) -> Result<serde_json::Value, PobParseError> {
        let xml_str =
            std::str::from_utf8(xml).map_err(|e| PobParseError::InvalidBuild(e.to_string()))?;

        let (reply_tx, reply_rx) = oneshot::channel();

        self.sender
            .as_ref()
            .ok_or(PobParseError::Unavailable)?
            .send(PobRequest::Query {
                xml: xml_str.to_owned(),
                query,
                reply: reply_tx,
            })
            .map_err(|_| PobParseError::Unavailable)?;

        reply_rx.await.map_err(|_| PobParseError::Unavailable)?
    }
}

impl Drop for PobParser {
    fn drop(&mut self) {
        // Drop sender first to close the channel so the thread's recv loop exits.
        // Field auto-drop happens *after* drop() returns, so we must do this
        // explicitly -- otherwise join() deadlocks waiting for a channel that
        // won't close until after join() returns.
        self.sender.take();
        if let Some(handle) = self._thread.take() {
            let _ = handle.join();
        }
    }
}

/// Entry point for the dedicated parser thread.
///
/// `factory` runs on the worker thread and must produce a [`PobBackend`]
/// or an init error. This indirection lets tests inject a fake backend
/// without loading the Lua VM.
fn run_parser_thread<F>(
    factory: F,
    init_tx: oneshot::Sender<Result<(), String>>,
    rx: mpsc::Receiver<PobRequest>,
) where
    F: FnOnce() -> Result<Box<dyn PobBackend>, String>,
{
    let backend = match factory() {
        Ok(b) => b,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    let _ = init_tx.send(Ok(()));

    // Process requests until the channel is closed.
    for req in &rx {
        match req {
            PobRequest::Parse { xml, reply } => {
                let result = parse_one(backend.as_ref(), &xml);
                let _ = reply.send(result);
            }
            PobRequest::Query { xml, query, reply } => {
                let result = load_and_query(backend.as_ref(), &xml, &query);
                let _ = reply.send(result);
            }
        }
    }

    tracing::info!("parser thread shutting down");
}

/// Execute a single parse: load XML -> calculate -> serialize.
fn parse_one(backend: &dyn PobBackend, xml: &str) -> Result<Vec<u8>, PobParseError> {
    backend.load_build_xml(xml)?;
    backend.calculate_json()
}

/// Load a build and run a query against it.
fn load_and_query(
    backend: &dyn PobBackend,
    xml: &str,
    query: &PobQuery,
) -> Result<serde_json::Value, PobParseError> {
    backend.load_build_xml(xml)?;
    backend.query(query)
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fake_parser, fake_parser_with, FakeBackend};

    #[tokio::test]
    async fn init_error_surfaces_to_caller() {
        let result = PobParser::new_with_backend(|| Err("boom".to_owned())).await;
        let err = match result {
            Ok(_) => panic!("factory error should surface"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn query_returns_backend_response() {
        let parser = fake_parser_with(|q| match q {
            PobQuery::BuildStats => Ok(serde_json::json!({"dps": 1234})),
            _ => Ok(serde_json::json!({})),
        })
        .await;

        let result = parser
            .query(b"<any xml>", PobQuery::BuildStats)
            .await
            .unwrap();
        assert_eq!(result["dps"], 1234);
    }

    #[tokio::test]
    async fn query_surfaces_backend_errors() {
        let parser =
            fake_parser_with(|_| Err(PobParseError::InvalidBuild("bad data".to_owned()))).await;

        let err = parser
            .query(b"<xml>", PobQuery::Config)
            .await
            .expect_err("backend error should surface");

        match err {
            PobParseError::InvalidBuild(msg) => assert!(msg.contains("bad data")),
            other => panic!("expected InvalidBuild, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parse_delegates_to_backend() {
        let parser = fake_parser().await;
        let result = parser.parse(b"<xml>").await.unwrap();
        assert_eq!(result, b"{}");
    }

    #[tokio::test]
    async fn query_rejects_non_utf8_xml() {
        let parser = fake_parser().await;
        // 0xFF is never valid UTF-8 as a leading byte.
        let err = parser
            .query(&[0xFFu8], PobQuery::BuildStats)
            .await
            .expect_err("invalid UTF-8 should surface");
        match err {
            PobParseError::InvalidBuild(_) => {}
            other => panic!("expected InvalidBuild, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drop_shuts_down_worker_cleanly() {
        // Scope the parser so Drop runs inside the test.
        let handle_joined = {
            let parser = fake_parser().await;
            // Issue a query to confirm the thread is alive and responsive.
            parser.query(b"<xml>", PobQuery::Config).await.unwrap();
            // Drop happens at scope end — if shutdown deadlocks the test
            // times out and fails.
            parser.sender.is_some()
        };
        assert!(handle_joined, "sender present before drop");
    }

    #[tokio::test]
    async fn query_after_shutdown_returns_unavailable() {
        // Simulate a dead parser by constructing one with no sender.
        let parser = PobParser {
            sender: None,
            _thread: None,
        };
        let err = parser.query(b"<xml>", PobQuery::Config).await.unwrap_err();
        assert!(matches!(err, PobParseError::Unavailable));

        let err = parser.parse(b"<xml>").await.unwrap_err();
        assert!(matches!(err, PobParseError::Unavailable));
    }

    #[test]
    fn pob_query_variants_are_clone_and_debug() {
        // Lightweight coverage for the public enum.
        let q = PobQuery::PassiveStats {
            stats: vec!["life".into()],
            radius: 2,
        };
        let q2 = q.clone();
        let _ = format!("{q:?} {q2:?}");

        let q = PobQuery::SearchGems {
            query: Some("fireball".into()),
            gem_type: Some("active".into()),
            tags: vec!["fire".into()],
        };
        let _ = format!("{:?}", q.clone());
    }

    #[test]
    fn pob_parse_error_display() {
        let e = PobParseError::InvalidBuild("bad".into());
        assert_eq!(e.to_string(), "invalid build: bad");
        let e = PobParseError::Unavailable;
        assert_eq!(e.to_string(), "parser unavailable");
    }

    #[tokio::test]
    async fn fake_backend_default_returns_empty_object() {
        let b = FakeBackend::default_ok();
        assert!(b.load_build_xml("x").is_ok());
        assert_eq!(b.calculate_json().unwrap(), b"{}");
        assert_eq!(b.query(&PobQuery::Config).unwrap(), serde_json::json!({}));
    }
}
