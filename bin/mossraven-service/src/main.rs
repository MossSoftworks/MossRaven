//! MossRaven orchestration service.
//!
//! Two run modes:
//!
//! 1. **Daemon (default)** — long-lived MCP server. tracing → stderr (ANSI off),
//!    stdout reserved for MCP JSON-RPC traffic. Driven by the WPF shell,
//!    Claude Code, Cowork, or any MCP client. Stays alive until parent closes
//!    stdin.
//!
//! 2. **Headless (`--headless`)** — one-shot end-to-end pipeline run for
//!    iteration without the UI. Parses `--concept` and `--generations`, runs
//!    the cascade evaluator N times, prints the archive snapshot, exits.

use std::sync::Arc;

use async_trait::async_trait;
use mossraven_archive::Archive;
use mossraven_core::{tier3::LocalBackend, SearchEngine};
use mossraven_dreamer::{AnthropicApiDriver, ExternalMcpDriver, TierOneDriver};
use mossraven_mcp_server::{ControlSurface, McpError};
use mossraven_pob::PobParser;
use mossraven_surrogate::{OpenAiCompatConfig, OpenAiCompatSurrogate, SurrogateProvider};
use parking_lot::Mutex;
use serde_json::{json, Value};

#[derive(Debug, Default)]
struct Args {
    headless: bool,
    concept: Option<String>,
    generations: u32,
    pob_path: Option<String>,
}

fn parse_args() -> Args {
    let mut out = Args {
        generations: 10,
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--headless" => out.headless = true,
            "--concept" => out.concept = it.next(),
            "--generations" | "-g" => {
                if let Some(n) = it.next().and_then(|s| s.parse().ok()) {
                    out.generations = n;
                }
            }
            "--pob-path" => out.pob_path = it.next(),
            "--help" | "-h" => {
                println!(
                    "mossraven-service v{}\n\nUSAGE:\n    \
                     mossraven-service              # daemon (MCP server on stdio)\n    \
                     mossraven-service --headless [--concept TEXT] [--generations N] [--pob-path PATH]\n\n\
                     ENV:\n    \
                     MOSSRAVEN_POB_PATH               PoB2 checkout (default: vendor/PathOfBuilding-PoE2)\n    \
                     MOSSRAVEN_ARCHIVE_PATH           Override archive.json location\n    \
                     MOSSRAVEN_SEED_XML_PATH          PoB XML the cascade mutates from. Defaults to crates/pob/tests/fixtures/seed.xml if present.\n    \
                     MOSSRAVEN_ANTHROPIC_API_KEY      Enables Mode A Tier-1 driver\n    \
                     MOSSRAVEN_ANTHROPIC_MODEL        Default: claude-sonnet-4-5\n    \
                     CEREBRAS_API_KEY               Enables Cerebras Tier-2 surrogate\n    \
                     CEREBRAS_MODEL                 Default: gpt-oss-120b\n    \
                     CEREBRAS_BASE_URL              Default: https://api.cerebras.ai/v1\n    \
                     RUST_LOG                       tracing filter (default: info)\n",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown arg: {other}  (--help for usage)");
                std::process::exit(2);
            }
        }
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let pob_path = resolve_pob_path(args.pob_path.as_deref());

    if args.headless {
        return run_headless(&args, &pob_path).await;
    }
    run_daemon(&pob_path).await
}

/// Find the PoB2 checkout. CWD-relative breaks when the WPF shell launches
/// the service with CWD = dist/, so we search multiple sensible locations
/// and return the first one that has PoB2's HeadlessWrapper.lua. If nothing
/// matches we return the original default and let init() report the path
/// it tried.
fn resolve_pob_path(cli_override: Option<&str>) -> String {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = cli_override {
        candidates.push(p.into());
    }
    if let Ok(p) = std::env::var("MOSSRAVEN_POB_PATH") {
        candidates.push(p.into());
    }
    candidates.push(exe_dir.join("vendor/PathOfBuilding-PoE2"));
    candidates.push(exe_dir.join("../vendor/PathOfBuilding-PoE2"));
    candidates.push(exe_dir.join("PathOfBuilding-PoE2"));
    candidates.push(std::path::PathBuf::from("vendor/PathOfBuilding-PoE2"));
    candidates.push(std::path::PathBuf::from("../vendor/PathOfBuilding-PoE2"));

    for cand in &candidates {
        if cand.join("src/HeadlessWrapper.lua").exists() {
            tracing::info!(path = ?cand, "PoB2 located via path probe");
            return cand.to_string_lossy().into_owned();
        }
    }
    // Nothing found — fall back to the default so init() reports a useful error.
    tracing::warn!(
        ?candidates,
        "PoB2 not found at any candidate path; service will report Tier-3 errors"
    );
    "vendor/PathOfBuilding-PoE2".to_string()
}

// ----- Engine construction (shared by daemon + headless) -----

struct Context {
    archive: Arc<Archive>,
    archive_path: std::path::PathBuf,
    engine: SearchEngine,
    dreamer: Arc<dyn TierOneDriver>,
    surrogate_active: bool,
    /// In-memory cache of the last hypothesis seeded — used as the concept
    /// label on subsequent run_search calls and for prompt-sampler diversity.
    last_hypothesis: Mutex<Option<mossraven_dreamer::Hypothesis>>,
    /// Fallback seed PoB XML loaded from disk at startup. When seed_hypothesis
    /// runs without a Tier-1 driver returning its own XML, this is what the
    /// engine mutates from.
    default_seed_xml: Option<String>,
}

async fn build_context(pob_path: &str) -> Context {
    let parser = match PobParser::new(std::path::Path::new(pob_path)).await {
        Ok(p) => {
            tracing::info!(?pob_path, "PobParser initialized");
            Some(Arc::new(p))
        }
        Err(e) => {
            tracing::warn!(
                ?pob_path,
                error = %e,
                "PobParser not initialized (PoB2 not found); Tier-3 will return errors. \
                 Set MOSSRAVEN_POB_PATH to a valid PathOfBuilding-PoE2 checkout to enable scoring."
            );
            None
        }
    };

    // Archive: persist across runs. Loads if file exists, starts empty otherwise.
    let archive_path = std::env::var("MOSSRAVEN_ARCHIVE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Archive::default_path());
    let archive = match Archive::load(&archive_path) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            tracing::warn!(?archive_path, error = %e, "archive load failed; starting empty");
            Arc::new(Archive::new())
        }
    };

    // Surrogate: Cerebras if CEREBRAS_API_KEY is set, mock otherwise.
    let (surrogate, surrogate_active): (Arc<dyn SurrogateProvider>, bool) =
        match std::env::var("CEREBRAS_API_KEY") {
            Ok(key) if !key.is_empty() => {
                let mut cfg = OpenAiCompatConfig::cerebras_default(key);
                if let Ok(url) = std::env::var("CEREBRAS_BASE_URL") {
                    cfg.base_url = url;
                }
                if let Ok(model) = std::env::var("CEREBRAS_MODEL") {
                    cfg.model = model;
                }
                tracing::info!(model = %cfg.model, base_url = %cfg.base_url, "Cerebras surrogate active");
                (Arc::new(OpenAiCompatSurrogate::new(cfg)), true)
            }
            _ => {
                tracing::info!("CEREBRAS_API_KEY not set; using MockSurrogate (deterministic stub)");
                (Arc::new(mossraven_surrogate::MockSurrogate), false)
            }
        };

    // Dreamer: Mode A if MOSSRAVEN_ANTHROPIC_API_KEY is set, external otherwise.
    let dreamer: Arc<dyn TierOneDriver> = match std::env::var("MOSSRAVEN_ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => {
            let model = std::env::var("MOSSRAVEN_ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-5".to_string());
            tracing::info!(model = %model, "AnthropicApiDriver active (Mode A)");
            Arc::new(AnthropicApiDriver::new(model, key))
        }
        _ => {
            tracing::info!("MOSSRAVEN_ANTHROPIC_API_KEY not set; Mode B (Claude Code drives via MCP)");
            Arc::new(ExternalMcpDriver)
        }
    };

    // Tier-3 backend selection: local PobParser pool, or no-op if PoB2 missing.
    // Pool size: MOSSRAVEN_POOL_SIZE env var, default 1, capped at min(num_cpus/2, 8).
    let tier3: Arc<dyn mossraven_core::tier3::Tier3Backend> = match parser {
        Some(p) => {
            let pool_size = std::env::var("MOSSRAVEN_POOL_SIZE")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1);
            if pool_size > 1 {
                let cap = std::thread::available_parallelism()
                    .map(|n| (n.get() / 2).clamp(1, 8))
                    .unwrap_or(1);
                let n = pool_size.min(cap);
                tracing::info!(
                    requested = pool_size,
                    effective = n,
                    cap = cap,
                    "scaling LocalBackend to {n}-worker pool (already have 1 worker; adding {})",
                    n.saturating_sub(1)
                );
                // We already paid for one PobParser via the eager `PobParser::new` above.
                // Spawn (n-1) more workers and assemble the pool. If any extra worker
                // fails, fall back to the single-worker backend.
                match build_pool_with_extras(p.clone(), pob_path, n).await {
                    Ok(pool) => Arc::new(pool) as Arc<dyn mossraven_core::tier3::Tier3Backend>,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to grow LocalBackend pool; falling back to single-worker");
                        Arc::new(LocalBackend::new(p))
                    }
                }
            } else {
                Arc::new(LocalBackend::new(p))
            }
        }
        None => Arc::new(NoopTier3),
    };
    let engine = SearchEngine::new(archive.clone(), surrogate, tier3);

    // Optional seed PoB XML — preload so engine.step has a real build to
    // mutate. Search order (first readable file wins):
    //   1. $MOSSRAVEN_SEED_XML_PATH (if set)
    //   2. {exe-dir}/seed.xml (production: shipped alongside MossRaven.exe in dist/)
    //   3. {exe-dir}/../crates/pob/tests/fixtures/seed.xml (dev: when running from target/release)
    //   4. crates/pob/tests/fixtures/seed.xml relative to CWD (dev: cargo run from workspace root)
    //   5. None — engine still wires up, just scores degenerate.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut seed_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("MOSSRAVEN_SEED_XML_PATH") {
        seed_paths.push(p.into());
    }
    seed_paths.push(exe_dir.join("seed.xml"));
    seed_paths.push(exe_dir.join("../crates/pob/tests/fixtures/seed.xml"));
    seed_paths.push(std::path::PathBuf::from("crates/pob/tests/fixtures/seed.xml"));
    let seed_xml = seed_paths.iter().find_map(|p| match std::fs::read_to_string(p) {
        Ok(s) => {
            tracing::info!(path = ?p, bytes = s.len(), "seed PoB XML loaded");
            Some(s)
        }
        Err(_) => None,
    });
    if seed_xml.is_none() {
        tracing::info!(
            "no seed PoB XML found (set MOSSRAVEN_SEED_XML_PATH or drop crates/pob/tests/fixtures/seed.xml). \
             Engine will run with an empty seed — every variant in a generation scores identically."
        );
    }

    Context {
        archive,
        archive_path,
        engine,
        dreamer,
        surrogate_active,
        last_hypothesis: Mutex::new(None),
        default_seed_xml: seed_xml,
    }
}

// ----- Headless one-shot run -----

async fn run_headless(args: &Args, pob_path: &str) -> anyhow::Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        generations = args.generations,
        ?pob_path,
        "mossraven-service --headless"
    );

    let ctx = build_context(pob_path).await;

    let concept = match &args.concept {
        Some(c) if !c.is_empty() => c.clone(),
        _ => "cold DoT scaled through an obscure ailment interaction".to_string(),
    };

    // Tier 1 seed: real call if a driver is available, otherwise echo the concept.
    let hypothesis = match ctx.dreamer.seed(&concept).await {
        Ok(h) => {
            tracing::info!(?h, "Tier-1 seed returned");
            h
        }
        Err(e) => {
            tracing::warn!(error = %e, "Tier-1 seed unavailable; using raw concept as hypothesis");
            mossraven_dreamer::Hypothesis {
                concept: concept.clone(),
                rationale: None,
                initial_cell_focus: None,
                seed_pob_xml: None,
            }
        }
    };
    *ctx.last_hypothesis.lock() = Some(hypothesis.clone());

    // Push the hypothesis into engine state so engine.step() has something to mutate.
    // Seed XML precedence: hypothesis.seed_pob_xml (from Tier 1) → default loaded at startup.
    let seed_xml = hypothesis
        .seed_pob_xml
        .clone()
        .or_else(|| ctx.default_seed_xml.clone())
        .unwrap_or_default();
    ctx.engine.set_state(
        hypothesis.concept.clone(),
        hypothesis.rationale.clone(),
        hypothesis.initial_cell_focus.clone(),
        seed_xml,
    );

    let mut total = mossraven_core::GenerationReport::default();
    for i in 1..=args.generations {
        let report = ctx.engine.step().await?;
        tracing::info!(
            gen = i,
            of = args.generations,
            variants_proposed = report.variants_proposed,
            variants_pruned = report.variants_pruned,
            variants_scored = report.variants_scored,
            cells_filled_or_improved = report.cells_filled_or_improved,
            "generation"
        );
        total.variants_proposed += report.variants_proposed;
        total.variants_pruned += report.variants_pruned;
        total.variants_scored += report.variants_scored;
        total.cells_filled_or_improved += report.cells_filled_or_improved;
    }

    // Persist archive after the run.
    if let Err(e) = ctx.archive.save(&ctx.archive_path) {
        tracing::warn!(error = %e, "archive save failed");
    }

    let snapshot = ctx.archive.snapshot();
    let summary = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "concept": concept,
        "hypothesis": hypothesis,
        "generations_run": args.generations,
        "surrogate_active": ctx.surrogate_active,
        "totals": {
            "variants_proposed": total.variants_proposed,
            "variants_pruned": total.variants_pruned,
            "variants_scored": total.variants_scored,
            "cells_filled_or_improved": total.cells_filled_or_improved,
        },
        "archive": {
            "cells_filled": snapshot.len(),
        },
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

// ----- Daemon (MCP server) -----

async fn run_daemon(pob_path: &str) -> anyhow::Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "mossraven-service starting (daemon)"
    );

    let ctx = Arc::new(build_context(pob_path).await);

    tracing::info!(
        archive_cells_filled = ctx.archive.filled_count(),
        archive_path = ?ctx.archive_path,
        "ready; serving MCP on stdio"
    );

    let surface = ServiceControlSurface { ctx: ctx.clone() };
    mossraven_mcp_server::serve_stdio(surface).await?;

    // Persist archive on clean shutdown.
    if let Err(e) = ctx.archive.save(&ctx.archive_path) {
        tracing::warn!(error = %e, "archive save on shutdown failed");
    }
    Ok(())
}

// ----- ControlSurface impl bridging MCP → engine -----

struct ServiceControlSurface {
    ctx: Arc<Context>,
}

#[async_trait]
impl ControlSurface for ServiceControlSurface {
    async fn seed_hypothesis(&self, concept: &str) -> Result<Value, McpError> {
        // Mode B (Claude Code drives MCP) returns DriverIsExternal — that's
        // not a failure, it just means we don't have a server-side Tier-1.
        // Use the user's concept verbatim as the hypothesis in that case.
        let hypothesis = match self.ctx.dreamer.seed(concept).await {
            Ok(h) => h,
            Err(mossraven_dreamer::DreamerError::DriverIsExternal) => {
                tracing::info!("dreamer is external; using user concept verbatim as hypothesis");
                mossraven_dreamer::Hypothesis {
                    concept: concept.to_string(),
                    rationale: None,
                    initial_cell_focus: None,
                    seed_pob_xml: None,
                }
            }
            Err(e) => return Err(McpError::ToolFailed(e.to_string())),
        };
        *self.ctx.last_hypothesis.lock() = Some(hypothesis.clone());
        // Seed XML precedence: dreamer's seed_pob_xml → service-loaded default → empty.
        let seed_xml = hypothesis
            .seed_pob_xml
            .clone()
            .or_else(|| self.ctx.default_seed_xml.clone())
            .unwrap_or_default();
        self.ctx.engine.set_state(
            hypothesis.concept.clone(),
            hypothesis.rationale.clone(),
            hypothesis.initial_cell_focus.clone(),
            seed_xml,
        );
        Ok(serde_json::to_value(hypothesis).unwrap())
    }

    async fn run_search(&self, generations: u32, _region: Option<String>) -> Result<Value, McpError> {
        let mut total = mossraven_core::GenerationReport::default();
        for i in 1..=generations {
            let report = self
                .ctx
                .engine
                .step()
                .await
                .map_err(|e| McpError::ToolFailed(format!("engine step failed: {e}")))?;
            total.variants_proposed += report.variants_proposed;
            total.variants_pruned += report.variants_pruned;
            total.variants_scored += report.variants_scored;
            total.cells_filled_or_improved += report.cells_filled_or_improved;
            tracing::info!(gen = i, of = generations, ?report, "generation");
        }
        // Persist after the run so a crash doesn't lose progress.
        if let Err(e) = self.ctx.archive.save(&self.ctx.archive_path) {
            tracing::warn!(error = %e, "archive save failed after run_search");
        }
        Ok(json!({
            "generations_run": generations,
            "totals": {
                "variants_proposed": total.variants_proposed,
                "variants_pruned": total.variants_pruned,
                "variants_scored": total.variants_scored,
                "cells_filled_or_improved": total.cells_filled_or_improved,
            },
            "archive_cells_filled": self.ctx.archive.filled_count(),
        }))
    }

    async fn read_archive(&self) -> Result<Value, McpError> {
        let snap = self.ctx.archive.snapshot();
        Ok(json!({
            "cells_filled": snap.len(),
            "entries": snap.into_iter().map(|(coords, entry)| json!({
                "coords": coords,
                "variant_id": entry.variant_id,
                "stats": entry.stats,
                "origin_hypothesis": entry.origin_hypothesis,
                "data_version": entry.data_version,
            })).collect::<Vec<_>>(),
        }))
    }

    async fn inspect_cell(&self, coords: Value) -> Result<Value, McpError> {
        let coords: mossraven_archive::CellCoords = serde_json::from_value(coords)
            .map_err(|e| McpError::Protocol(format!("inspect_cell coords: {e}")))?;
        match self.ctx.archive.read(&coords) {
            Some(entry) => Ok(serde_json::to_value(entry).unwrap()),
            None => Ok(json!({ "empty": true, "coords": coords })),
        }
    }

    async fn get_frontier(&self) -> Result<Value, McpError> {
        // v1: Pareto frontier is "all filled cells sorted by total_dps". The
        // real impl will compute a multi-objective frontier over
        // (novelty × power × cost). Cost requires economy data (poe2-mcp /
        // mcpmarket-poe2 integration) which lands later.
        let mut snap = self.ctx.archive.snapshot();
        snap.sort_by(|a, b| {
            b.1.stats
                .total_dps
                .partial_cmp(&a.1.stats.total_dps)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(json!({
            "frontier": snap.into_iter().take(10).map(|(coords, entry)| json!({
                "coords": coords,
                "total_dps": entry.stats.total_dps,
                "effective_hp": entry.stats.effective_hp,
                "origin_hypothesis": entry.origin_hypothesis,
            })).collect::<Vec<_>>(),
        }))
    }
}

/// Build a LocalBackend with `total` workers given the first parser already
/// constructed (so we don't waste the cost of the eager init done at startup).
async fn build_pool_with_extras(
    first: Arc<mossraven_pob::PobParser>,
    pob_path: &str,
    total: usize,
) -> anyhow::Result<LocalBackend> {
    let mut pool = vec![first];
    for i in 1..total {
        tracing::info!(worker = i, total, "initializing extra PobParser worker");
        pool.push(Arc::new(
            mossraven_pob::PobParser::new(std::path::Path::new(pob_path)).await?,
        ));
    }
    Ok(LocalBackend::from_pool(pool))
}

// ----- No-op Tier-3 for when PoB2 isn't reachable -----

struct NoopTier3;

#[async_trait]
impl mossraven_core::tier3::Tier3Backend for NoopTier3 {
    async fn score(
        &self,
        variants: Vec<(String, String)>,
    ) -> Result<
        Vec<(String, Result<mossraven_pob::BuildStats, String>)>,
        mossraven_core::tier3::Tier3Error,
    > {
        Ok(variants
            .into_iter()
            .map(|(id, _)| (id, Err("no-op Tier-3 (PoB2 not configured)".to_string())))
            .collect())
    }
}
