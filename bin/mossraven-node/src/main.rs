//! mossraven-node — the power-user farm-machine worker.
//!
//! HTTP/JSON. Stateless. Exposes:
//!
//!   GET  /health  → liveness + capacity
//!   POST /score   → score a batch of variants (bearer-auth)
//!
//! Deployment targets, in order: idle Windows gaming PCs (first-class),
//! Linux Proxmox VMs / anything with a CPU. Service-side `RemoteBackend`
//! distributes batches across registered node URLs.
//!
//! v1.1 status: real `/score`. A pool of [`PobParser`] workers (one Lua VM
//! pinned to one OS thread each) is initialized sequentially at startup —
//! the node does not bind its port until every worker is ready, so a
//! reachable node is a ready node. Batches are round-robined across the
//! pool; the rotation offset advances per batch so small batches don't pile
//! onto worker 0. Result order within a batch is unspecified — consumers
//! match on `variant_id` (RemoteBackend already does).
//!
//! `VariantPayload::Spec` is not yet supported (returns a per-variant
//! error); send `PobXml`.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use anyhow::Context;
use mossraven_node_protocol::{
    HealthResponse, ScoreBatchRequest, ScoreBatchResponse, VariantOutcome, VariantPayload,
    VariantRequest, VariantResult,
};
use mossraven_pob::PobParser;
use parking_lot::Mutex;

const DEFAULT_BEARER: &str = "dev-bearer-change-me";

#[derive(Debug, Clone)]
struct Config {
    /// Bearer token clients must present.
    bearer: String,
    /// `vendor/PathOfBuilding-PoE2` location on this node. Canonicalized at
    /// startup — must be absolute by the time workers spin up, because the
    /// engine briefly flips process CWD during init and queries.
    pob_path: PathBuf,
    /// Bind address.
    bind: SocketAddr,
    /// PobParser pool size. Each worker = one OS thread + one Lua VM
    /// (~50–100 MB). Default: half the logical cores, clamped to [1, 8].
    workers: usize,
}

fn default_addr() -> SocketAddr {
    "0.0.0.0:5380".parse().unwrap()
}

fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .div_ceil(2)
        .clamp(1, 8)
}

struct AppState {
    cfg: Config,
    pool: Vec<Arc<PobParser>>,
    /// Per-batch rotation offset so consecutive small batches spread across
    /// the pool instead of always starting at worker 0.
    rotate: AtomicUsize,
    in_flight: Mutex<usize>,
    pob2_version: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Config: env-first, falls back to plausible defaults. Real impl could
    // accept a config.toml path via clap.
    let raw_pob_path = std::env::var("MOSSRAVEN_POB_PATH")
        .unwrap_or_else(|_| "vendor/PathOfBuilding-PoE2".to_string());
    let pob_path = validate_pob_path(Path::new(&raw_pob_path))?;

    let cfg = Config {
        bearer: std::env::var("MOSSRAVEN_NODE_BEARER")
            .unwrap_or_else(|_| DEFAULT_BEARER.to_string()),
        pob_path,
        bind: std::env::var("MOSSRAVEN_NODE_BIND")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_addr),
        workers: std::env::var("MOSSRAVEN_NODE_WORKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(|w: usize| w.max(1))
            .unwrap_or_else(default_workers),
    };

    if cfg.bearer == DEFAULT_BEARER {
        tracing::warn!(
            "MOSSRAVEN_NODE_BEARER is unset — running with the well-known dev bearer. \
             Anyone who can reach {} can submit work. Set a real token before exposing \
             this node beyond localhost.",
            cfg.bind
        );
    }

    let pob2_version = read_pob2_version(&cfg.pob_path);
    tracing::info!(
        addr = %cfg.bind,
        pob = %cfg.pob_path.display(),
        pob2_version = %pob2_version,
        workers = cfg.workers,
        "mossraven-node starting"
    );

    // Initialize the pool *sequentially*: PobHeadless::init() temporarily
    // flips process-global CWD (and restores it), so parallel inits would
    // race. Same pattern as core::judge::LocalBackend::with_pool.
    let started = Instant::now();
    let mut pool = Vec::with_capacity(cfg.workers);
    for i in 0..cfg.workers {
        let t = Instant::now();
        let parser = PobParser::new(&cfg.pob_path)
            .await
            .with_context(|| format!("initializing PobParser worker {i}"))?;
        tracing::info!(
            worker = i,
            total = cfg.workers,
            elapsed_ms = t.elapsed().as_millis() as u64,
            "PobParser worker ready"
        );
        pool.push(Arc::new(parser));
    }
    tracing::info!(
        workers = pool.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "pool initialized — binding listener"
    );

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        pool,
        rotate: AtomicUsize::new(0),
        in_flight: Mutex::new(0),
        pob2_version,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/score", post(score))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, "mossraven-node ready");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Canonicalize and sanity-check the PoB2 vendor path before paying the
/// cost of Lua VM startup. Fail fast with an actionable message.
fn validate_pob_path(raw: &Path) -> anyhow::Result<PathBuf> {
    let canonical = raw.canonicalize().with_context(|| {
        format!(
            "PoB2 path {} does not exist — set MOSSRAVEN_POB_PATH to your \
             vendor/PathOfBuilding-PoE2 checkout",
            raw.display()
        )
    })?;
    let wrapper = canonical.join("src").join("HeadlessWrapper.lua");
    anyhow::ensure!(
        wrapper.exists(),
        "{} exists but doesn't look like a PoB2 checkout (missing src/HeadlessWrapper.lua)",
        canonical.display()
    );
    Ok(canonical)
}

/// Best-effort PoB2 version from `manifest.xml` (`<Version number="0.19.0" />`).
fn read_pob2_version(pob_path: &Path) -> String {
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

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pob2_version: state.pob2_version.clone(),
        cores: num_cpus(),
        workers: state.pool.len(),
        in_flight: *state.in_flight.lock(),
    })
}

/// Decrements `in_flight` even if the handler errors or a task panics.
struct InFlightGuard {
    state: Arc<AppState>,
    n: usize,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut g = self.state.in_flight.lock();
        *g = g.saturating_sub(self.n);
    }
}

async fn score(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ScoreBatchRequest>,
) -> Result<Json<ScoreBatchResponse>, StatusCode> {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    if presented != Some(state.cfg.bearer.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let batch_size = req.variants.len();
    {
        let mut g = state.in_flight.lock();
        *g = g.saturating_add(batch_size);
    }
    let _guard = InFlightGuard {
        state: state.clone(),
        n: batch_size,
    };

    let started = Instant::now();
    let k = state.pool.len();
    let offset = state.rotate.fetch_add(1, Ordering::Relaxed);

    // Round-robin variants into one bucket per parser. Each bucket's
    // variants serialize on that parser's worker thread (the Lua VM is
    // !Send); buckets run concurrently across the pool.
    let mut buckets: Vec<Vec<VariantRequest>> = (0..k).map(|_| Vec::new()).collect();
    for (i, v) in req.variants.into_iter().enumerate() {
        buckets[(i + offset) % k].push(v);
    }

    let mut handles = Vec::with_capacity(k);
    for (i, bucket) in buckets.into_iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let parser = state.pool[i].clone();
        let pob2_version = state.pob2_version.clone();
        handles.push(tokio::spawn(async move {
            let mut out = Vec::with_capacity(bucket.len());
            for variant in bucket {
                out.push(score_variant(&parser, variant, &pob2_version).await);
            }
            out
        }));
    }

    let mut results: Vec<VariantResult> = Vec::with_capacity(batch_size);
    for handle in handles {
        match handle.await {
            Ok(mut bucket_results) => results.append(&mut bucket_results),
            Err(join_err) => {
                tracing::error!(error = %join_err, "score worker task panicked");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    let errors = results
        .iter()
        .filter(|r| matches!(r.outcome, VariantOutcome::Error { .. }))
        .count();
    tracing::info!(
        batch_id = %req.batch_id,
        variants = batch_size,
        errors,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "batch scored"
    );

    Ok(Json(ScoreBatchResponse {
        batch_id: req.batch_id,
        results,
    }))
}

async fn score_variant(
    parser: &PobParser,
    variant: VariantRequest,
    pob2_version: &str,
) -> VariantResult {
    let outcome = match variant.payload {
        VariantPayload::PobXml(xml) => match parser.parse(xml.as_bytes()).await {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(stats) => VariantOutcome::Ok {
                    stats,
                    pob2_version: pob2_version.to_string(),
                },
                Err(e) => VariantOutcome::Error {
                    message: format!("stats deserialize failed: {e}"),
                },
            },
            Err(e) => VariantOutcome::Error {
                message: format!("pob calc failed: {e}"),
            },
        },
        VariantPayload::Spec(_) => VariantOutcome::Error {
            message: "Spec payload not yet supported by this node; send PobXml".to_string(),
        },
    };
    VariantResult {
        variant_id: variant.variant_id,
        outcome,
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}
