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
    /// One-shot MCP tool call. The service initializes the engine, runs the
    /// named tool against ServiceControlSurface, prints the JSON result to
    /// stdout, and exits. Lets external scripts (bash, python, claude code,
    /// or me — invoking it via Bash tool) drive the engine without going
    /// through the JSON-RPC stdio framer.
    tool: Option<String>,
    /// JSON object containing the tool's arguments. Default: `{}`.
    tool_args: Option<String>,
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
            "--tool" => out.tool = it.next(),
            "--tool-args" => out.tool_args = it.next(),
            "--help" | "-h" => {
                println!(
                    "mossraven-service v{}\n\nUSAGE:\n    \
                     mossraven-service              # daemon (MCP server on stdio)\n    \
                     mossraven-service --headless [--concept TEXT] [--generations N]\n    \
                     mossraven-service --tool NAME [--tool-args JSON]   # one-shot tool call\n\n\
                     TOOLS:\n    \
                     seed_hypothesis  args: {{\"concept\": \"...\"}}\n    \
                     run_search       args: {{\"generations\": N, \"region\": \"...\"}}\n    \
                     read_archive     args: {{}}\n    \
                     inspect_cell     args: {{\"damage_type\": \"...\", \"defense_layer\": \"...\", ...}}\n    \
                     get_frontier     args: {{}}\n    \
                     synthesize_finalists args: {{}}  # Tier 5: Claude curates frontier → narrated finalists\n    \
                     save_finalists   args: {{\"finalists\": [...]}}  # persist curated finalists (Mode B write-back)\n\n\
                     ENV:\n    \
                     MOSSRAVEN_POB_PATH               PoB2 checkout (default: vendor/PathOfBuilding-PoE2)\n    \
                     MOSSRAVEN_ARCHIVE_PATH           Override archive.json location\n    \
                     MOSSRAVEN_SEED_XML_PATH          PoB XML the cascade mutates from. Defaults to crates/pob/tests/fixtures/seed.xml if present.\n    \
                     MOSSRAVEN_POOL_SIZE              Local Tier-3 PobParser workers (default 1, cap min(cores/2, 8))\n    \
                     MOSSRAVEN_NODE_URLS              Comma-separated mossraven-node URLs — switches Tier-3 to REMOTE\n    \
                     MOSSRAVEN_NODE_BEARER            Bearer for remote nodes (default: dev bearer)\n    \
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

    // Set up logging: stderr (for WPF capture / interactive use) + a per-run
    // log file in %APPDATA%/Moss/MossRaven/logs/. Lets a future Claude comb
    // through prior cascade runs without the user re-pasting console output.
    let log_path = open_session_log_file();
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match log_path.as_ref() {
        Some((path, file)) => {
            // Fan out to BOTH stderr and the log file. The file gets the same
            // structured trace events; stderr stays interactive so the WPF's
            // existing stderr-capture pipeline keeps working.
            use tracing_subscriber::fmt::writer::MakeWriterExt;
            let file_writer = std::sync::Mutex::new(file.try_clone().expect("clone log fd"));
            let tee = std::io::stderr.and(move || file_writer.lock().unwrap().try_clone().unwrap());
            tracing_subscriber::fmt()
                .with_writer(tee)
                .with_ansi(false)
                .with_env_filter(env_filter)
                .init();
            tracing::info!(log_file = %path.display(), "session log file opened");
            prune_old_session_logs(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
        }
        None => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_env_filter(env_filter)
                .init();
            tracing::warn!("could not open session log file; logging to stderr only");
        }
    }

    let pob_path = resolve_pob_path(args.pob_path.as_deref());

    if let Some(tool) = args.tool.clone() {
        let tool_args = args.tool_args.clone().unwrap_or_else(|| "{}".to_string());
        return run_tool_call(&tool, &tool_args, &pob_path).await;
    }
    if args.headless {
        return run_headless(&args, &pob_path).await;
    }
    run_daemon(&pob_path).await
}

// ----- One-shot tool call (CLI) -----

async fn run_tool_call(tool: &str, args_json: &str, pob_path: &str) -> anyhow::Result<()> {
    tracing::info!(tool, args = %args_json, "mossraven-service --tool (one-shot)");

    let ctx = Arc::new(build_context(pob_path).await);

    let args: Value = serde_json::from_str(args_json)
        .map_err(|e| anyhow::anyhow!("--tool-args is not valid JSON: {e}"))?;

    let surface = ServiceControlSurface { ctx: ctx.clone() };

    let result = match tool {
        "seed_hypothesis" => {
            let concept = args
                .get("concept")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("seed_hypothesis: missing 'concept'"))?;
            surface.seed_hypothesis(concept).await
        }
        "run_search" => {
            let generations = args
                .get("generations")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as u32;
            let region = args
                .get("region")
                .and_then(|v| v.as_str())
                .map(String::from);
            surface.run_search(generations, region).await
        }
        "read_archive" => surface.read_archive().await,
        "inspect_cell" => surface.inspect_cell(args).await,
        "get_frontier" => surface.get_frontier().await,
        "synthesize_finalists" => surface.synthesize_finalists().await,
        "save_finalists" => surface.save_finalists(args).await,
        other => return Err(anyhow::anyhow!("unknown tool: {other}")),
    };

    // Persist archive on the way out (run_search already does it; this catches
    // the case where seed/inspect/etc. ran first).
    if let Err(e) = ctx.archive.save(&ctx.archive_path) {
        tracing::warn!(error = %e, "archive save failed");
    }

    let value = result.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Open a per-invocation log file under `%APPDATA%/Moss/MossRaven/logs/`.
/// Each service spawn creates a fresh file named with the unix timestamp;
/// we keep the last 20 so a future Claude can browse `[--tool, --headless,
/// daemon]` runs without the user re-pasting console output. Returns None
/// if we can't create the dir (e.g. headless CI, locked filesystem) — the
/// caller falls back to stderr-only logging.
fn open_session_log_file() -> Option<(std::path::PathBuf, std::fs::File)> {
    let base = directories::ProjectDirs::from("", "Moss", "MossRaven")
        .map(|p| p.data_dir().to_path_buf())
        .or_else(|| std::env::var_os("APPDATA").map(|a| {
            std::path::PathBuf::from(a).join("Moss").join("MossRaven")
        }))?;
    let logs = base.join("logs");
    std::fs::create_dir_all(&logs).ok()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Include the process id so two services launched in the same second
    // (e.g. WPF + a parallel `--tool` invocation) don't clobber each other.
    let pid = std::process::id();
    let path = logs.join(format!("service-{ts}-{pid}.log"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&path)
        .ok()?;
    Some((path, file))
}

/// Keep the last N session log files (oldest deleted first). N=20 = roughly
/// the last day of dev iteration without runaway disk use.
fn prune_old_session_logs(logs_dir: &std::path::Path) {
    const KEEP: usize = 20;
    let Ok(rd) = std::fs::read_dir(logs_dir) else { return; };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("service-")
        })
        .filter_map(|e| {
            let path = e.path();
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, path))
        })
        .collect();
    if files.len() <= KEEP { return; }
    // Sort newest first; delete the tail.
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(path);
    }
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

/// Best-effort PoB2 version from `manifest.xml` (`<Version number="0.19.0" />`).
/// Same logic as mossraven-node; duplicated to keep the binaries dependency-light.
fn read_pob2_version(pob_path: &std::path::Path) -> String {
    let Ok(manifest) = std::fs::read_to_string(pob_path.join("manifest.xml")) else {
        return "unknown".to_string();
    };
    const NEEDLE: &str = "Version number=\"";
    manifest
        .find(NEEDLE)
        .and_then(|i| {
            let rest = &manifest[i + NEEDLE.len()..];
            rest.find('"').map(|j| rest[..j].to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

// ----- Engine construction (shared by daemon + headless) -----

struct Context {
    archive: Arc<Archive>,
    archive_path: std::path::PathBuf,
    /// session.json path next to archive.json. Persists the active hypothesis
    /// across separate `--tool` process invocations so a `seed_hypothesis`
    /// call followed by a `run_search` call (each its own process) operates
    /// on the same engine state.
    session_path: std::path::PathBuf,
    engine: SearchEngine,
    dreamer: Arc<dyn TierOneDriver>,
    surrogate_active: bool,
    last_hypothesis: Mutex<Option<mossraven_dreamer::Hypothesis>>,
    default_seed_xml: Option<String>,
}

/// Sticky engine state — what the engine is currently mutating from.
/// Loaded on startup, written after every set_state.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct SessionState {
    concept: String,
    rationale: Option<String>,
    initial_cell_focus: Option<String>,
    seed_pob_xml: String,
}

impl SessionState {
    fn load(path: &std::path::Path) -> Option<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).ok(),
            Err(_) => None,
        }
    }
    fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap())?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
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
    let session_path = archive_path
        .parent()
        .map(|p| p.join("session.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("session.json"));
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

    // Tier-3 backend selection, in priority order (SPEC §4.3):
    //   1. REMOTE — MOSSRAVEN_NODE_URLS (comma-separated mossraven-node base
    //      URLs) + MOSSRAVEN_NODE_BEARER. Power-user / farm mode.
    //   2. LOCAL — in-process PobParser pool; MOSSRAVEN_POOL_SIZE workers
    //      (default 1, capped at min(cores/2, 8)).
    //   3. NO-OP — PoB2 not found; every variant errors with guidance.
    let remote_nodes: Vec<String> = std::env::var("MOSSRAVEN_NODE_URLS")
        .map(|urls| {
            urls.split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let tier3: Arc<dyn mossraven_core::tier3::Tier3Backend> = if !remote_nodes.is_empty() {
        let bearer = std::env::var("MOSSRAVEN_NODE_BEARER")
            .unwrap_or_else(|_| "dev-bearer-change-me".to_string());
        tracing::info!(
            nodes = remote_nodes.len(),
            urls = ?remote_nodes,
            "Tier-3 REMOTE backend active (mossraven-node pool)"
        );
        Arc::new(mossraven_core::tier3::RemoteBackend::new(remote_nodes, bearer))
    } else {
        match parser {
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
        }
    };
    let engine = SearchEngine::new(archive.clone(), surrogate, tier3);

    // Stamp every archive entry with the live PoB2 version (SPEC §9:
    // versioning — entries silently rot across league patches otherwise).
    // StepConfig defaults to "pob2:unknown"; manifest.xml is authoritative.
    {
        let v = read_pob2_version(std::path::Path::new(pob_path));
        tracing::info!(pob2_version = %v, "archive entries stamped with this data version");
        engine.state.lock().config.data_version = format!("pob2:{v}");
    }

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

    // Restore previously-seeded session state if present. Lets `--tool
    // seed_hypothesis` from one invocation persist into the next `--tool
    // run_search` invocation across process boundaries.
    if let Some(session) = SessionState::load(&session_path) {
        tracing::info!(
            path = ?session_path,
            concept = %session.concept,
            "session state restored — engine seeded from prior invocation"
        );
        engine.set_state(
            session.concept,
            session.rationale,
            session.initial_cell_focus,
            session.seed_pob_xml,
        );
    } else {
        tracing::debug!(?session_path, "no session state on disk (clean slate)");
    }

    Context {
        archive,
        archive_path,
        session_path,
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
            seed_xml.clone(),
        );
        // Persist session so a subsequent --tool run_search invocation
        // (separate process) restores the engine from this hypothesis.
        let session = SessionState {
            concept: hypothesis.concept.clone(),
            rationale: hypothesis.rationale.clone(),
            initial_cell_focus: hypothesis.initial_cell_focus.clone(),
            seed_pob_xml: seed_xml,
        };
        if let Err(e) = session.save(&self.ctx.session_path) {
            tracing::warn!(error = %e, "session state save failed");
        }
        Ok(serde_json::to_value(hypothesis).unwrap())
    }

    async fn run_search(&self, generations: u32, region: Option<String>) -> Result<Value, McpError> {
        // Region applies for this run (and sticks until the next run_search
        // call replaces or clears it).
        self.ctx.engine.set_region(region.clone());
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
            "region": region,
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
                // The WPF archive pane encodes this into a clipboard-ready
                // import code on click; omitting it made every archive row
                // report "no PoB XML on this entry".
                "pob_xml": entry.pob_xml,
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
                "cell": format!(
                    "{}/{}/{}/{}",
                    coords.damage_type, coords.defense_layer, coords.role, coords.scaling_vector
                ),
                "total_dps": entry.stats.total_dps,
                "effective_hp": entry.stats.effective_hp,
                "life": entry.stats.life,
                "energy_shield": entry.stats.energy_shield,
                "armour": entry.stats.armour,
                "evasion": entry.stats.evasion,
                "resists": {
                    "fire":      entry.stats.fire_res,
                    "cold":      entry.stats.cold_res,
                    "lightning": entry.stats.lightning_res,
                    "chaos":     entry.stats.chaos_res,
                },
                "variant_id": entry.variant_id,
                "origin_hypothesis": entry.origin_hypothesis,
                // The pob_import_code is what the user pastes into PoB2. The
                // dreamer copies this VERBATIM into the Finalist; we never want
                // the LLM to re-encode it (a Claude-generated import code would
                // be nonsense).
                "pob_import_code": mossraven_archive::encode_pob_import_code(&entry.pob_xml),
            })).collect::<Vec<_>>(),
        }))
    }

    /// Tier 5 — turn the current frontier into 5–10 curated finalists with prose.
    /// Routes through the active Tier-1 driver: AnthropicApiDriver in Mode A,
    /// the external MCP driver returns DriverIsExternal so the host (Claude
    /// Code / Cowork) does the synthesis itself.
    async fn synthesize_finalists(&self) -> Result<Value, McpError> {
        let frontier = self.get_frontier().await?;
        match self.ctx.dreamer.synthesize_finalists(&frontier).await {
            Ok(finalists) => {
                // Mode A: persist immediately — the files ARE the deliverable
                // (SPEC §1.1). A persistence failure is logged but doesn't
                // void the synthesis.
                let saved_to = match persist_finalists(&self.ctx, &finalists) {
                    Ok(dir) => Some(dir.display().to_string()),
                    Err(e) => {
                        tracing::warn!(error = %e, "finalist persistence failed");
                        None
                    }
                };
                Ok(json!({
                    "finalists": finalists,
                    "source_frontier_size": frontier.get("frontier").and_then(|f| f.as_array()).map(|a| a.len()).unwrap_or(0),
                    "saved_to": saved_to,
                }))
            }
            Err(mossraven_dreamer::DreamerError::DriverIsExternal) => {
                // Mode B: hand the frontier to the external Claude with the
                // finalist schema; it curates, then writes back via the
                // save_finalists tool.
                Ok(json!({
                    "external": true,
                    "frontier": frontier.get("frontier").cloned().unwrap_or(json!([])),
                    "instructions": "Mode B: synthesize Finalists yourself from this frontier. Schema: {variant_id, title, one_liner, why_it_works, tags[], cell, key_stats[{label,value}], pob_import_code, guide:{leveling, endgame, loadout_swap, playtest_notes}}. The guide is REQUIRED (SPEC 1.1): leveling = act milestones + gem/passive order + respec points; endgame = final tree direction + gear priorities + breakpoints; loadout_swap = clear-vs-boss duality via PoE2 weapon-set swap (which gems/passives per weapon set), or an EXPLICIT statement the build can't dual-loadout cleanly; playtest_notes = what PoB can't model (never claim it's fun). Copy variant_id/cell/pob_import_code VERBATIM. When done, call save_finalists with {\"finalists\":[...]} to persist them to disk.",
                }))
            }
            Err(e) => Err(McpError::ToolFailed(format!("synthesize_finalists: {e}"))),
        }
    }

    /// Mode B write-back: the external Claude curated finalists and hands them
    /// here for persistence. Also reachable via `--tool save_finalists`.
    async fn save_finalists(&self, args: Value) -> Result<Value, McpError> {
        let list = if args.is_array() {
            args.clone()
        } else {
            args.get("finalists")
                .cloned()
                .ok_or_else(|| McpError::Protocol("save_finalists: missing 'finalists' array".into()))?
        };
        let finalists: Vec<mossraven_dreamer::Finalist> = serde_json::from_value(list)
            .map_err(|e| McpError::Protocol(format!("save_finalists: bad finalist shape: {e}")))?;
        if finalists.is_empty() {
            return Err(McpError::Protocol("save_finalists: empty finalists array".into()));
        }
        let dir = persist_finalists(&self.ctx, &finalists)
            .map_err(|e| McpError::ToolFailed(format!("save_finalists: persist failed: {e}")))?;
        Ok(json!({
            "saved": finalists.len(),
            "dir": dir.display().to_string(),
        }))
    }
}

/// Write finalists to `<data-dir>/finalists/<unix-ts>/`:
/// `finalists.json` (the full array) plus, per finalist, a human-readable
/// markdown guide, the raw PoB XML (from the archive by variant_id, falling
/// back to decoding the import code), and the paste-ready import code.
fn persist_finalists(
    ctx: &Context,
    finalists: &[mossraven_dreamer::Finalist],
) -> anyhow::Result<std::path::PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = ctx
        .archive_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("finalists")
        .join(ts.to_string());
    std::fs::create_dir_all(&base)?;

    // Archive XML by variant_id — the authoritative source for each finalist's
    // build XML (import codes are derived FROM these).
    let xml_by_variant: std::collections::HashMap<String, String> = ctx
        .archive
        .snapshot()
        .into_iter()
        .map(|(_, e)| (e.variant_id, e.pob_xml))
        .collect();

    std::fs::write(
        base.join("finalists.json"),
        serde_json::to_string_pretty(finalists)?,
    )?;

    for (i, f) in finalists.iter().enumerate() {
        let stem = format!("{:02}-{}", i + 1, slugify(&f.title, 40));
        std::fs::write(base.join(format!("{stem}.pob-code.txt")), &f.pob_import_code)?;
        let xml = xml_by_variant
            .get(&f.variant_id)
            .cloned()
            .or_else(|| mossraven_archive::decode_pob_import_code(&f.pob_import_code).ok());
        match &xml {
            Some(xml) => std::fs::write(base.join(format!("{stem}.xml")), xml)?,
            None => tracing::warn!(
                variant = %f.variant_id,
                "finalist XML unavailable (not in archive, import code undecodable)"
            ),
        }
        std::fs::write(base.join(format!("{stem}.md")), finalist_markdown(f))?;
    }
    tracing::info!(dir = %base.display(), count = finalists.len(), "finalists persisted");
    Ok(base)
}

/// Lowercase alnum-dash slug used for finalist file stems, capped at `max` chars.
fn slugify(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.len() >= max {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "finalist".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Render one finalist as a standalone markdown build guide (SPEC §1.1).
fn finalist_markdown(f: &mossraven_dreamer::Finalist) -> String {
    let mut md = String::new();
    md.push_str(&format!("# {}\n\n> {}\n\n", f.title, f.one_liner));
    if !f.tags.is_empty() {
        md.push_str(&format!("**Tags:** {}\n\n", f.tags.join(" · ")));
    }
    md.push_str(&format!(
        "**Cell:** `{}`  \n**Variant:** `{}`\n\n",
        f.cell, f.variant_id
    ));
    if !f.key_stats.is_empty() {
        md.push_str("| stat | value |\n|---|---|\n");
        for ks in &f.key_stats {
            md.push_str(&format!("| {} | {} |\n", ks.label, ks.value));
        }
        md.push('\n');
    }
    md.push_str(&format!("## Why it works\n\n{}\n\n", f.why_it_works));
    match &f.guide {
        Some(g) => {
            md.push_str(&format!("## Leveling\n\n{}\n\n", g.leveling));
            md.push_str(&format!("## Endgame\n\n{}\n\n", g.endgame));
            md.push_str(&format!(
                "## Clear / boss loadout swap\n\n{}\n\n",
                g.loadout_swap
            ));
            if let Some(p) = &g.playtest_notes {
                md.push_str(&format!("## Playtest notes\n\n{}\n\n", p));
            }
        }
        None => md.push_str(
            "## Guide\n\n*(No guide attached — synthesized by a pre-§1.1 driver.)*\n\n",
        ),
    }
    md.push_str(
        "## PoB2 import code\n\nPaste into desktop PoB2 → Import/Export Build → Import from code.\n\n```\n",
    );
    md.push_str(&f.pob_import_code);
    md.push_str(
        "\n```\n\n*Theoretical viability only — PoB models damage/defense, not feel. Playtest before judging.*\n",
    );
    md
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
