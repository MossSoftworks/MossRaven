//! mossraven-node — the power-user farm-machine worker.
//!
//! HTTP/JSON. Stateless. Exposes:
//!
//!   GET  /health  → liveness + capacity
//!   POST /score   → score a batch of variants (bearer-auth)
//!
//! Deployed standalone on Linux Proxmox VMs, idle gaming PCs, or anywhere
//! with a CPU. Service-side `RemoteBackend` round-robins across registered
//! node URLs.
//!
//! v1 status: real /health, stub /score (returns zeros). Real scoring lands
//! once the in-process PobParser is validated against desktop PoB2.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use mossraven_node_protocol::{
    HealthResponse, ScoreBatchRequest, ScoreBatchResponse, VariantOutcome, VariantResult,
};
use parking_lot::Mutex;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Bearer token clients must present.
    bearer: String,
    /// `vendor/PathOfBuilding-PoE2` location on this node.
    pob_path: String,
    /// Bind address.
    #[serde(default = "default_addr")]
    bind: SocketAddr,
}

fn default_addr() -> SocketAddr {
    "0.0.0.0:5380".parse().unwrap()
}

struct AppState {
    cfg: Config,
    in_flight: Mutex<usize>,
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
    let cfg = Config {
        bearer: std::env::var("MOSSRAVEN_NODE_BEARER")
            .unwrap_or_else(|_| "dev-bearer-change-me".to_string()),
        pob_path: std::env::var("MOSSRAVEN_POB_PATH")
            .unwrap_or_else(|_| "vendor/PathOfBuilding-PoE2".to_string()),
        bind: std::env::var("MOSSRAVEN_NODE_BIND")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_addr),
    };

    tracing::info!(addr = %cfg.bind, pob = %cfg.pob_path, "mossraven-node starting");

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        in_flight: Mutex::new(0),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/score", post(score))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pob2_version: "unknown".to_string(), // TODO: read from vendor/PathOfBuilding-PoE2/manifest.cfg
        cores: num_cpus(),
        in_flight: *state.in_flight.lock(),
    })
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

    // STUB: real impl fans across a rayon pool, each thread holds its own
    // PobParser, returns stats. For v1 we just acknowledge the batch with
    // errors so the wire format is exercised end-to-end.
    let results: Vec<VariantResult> = req
        .variants
        .into_iter()
        .map(|v| VariantResult {
            variant_id: v.variant_id,
            outcome: VariantOutcome::Error {
                message: "mossraven-node /score is stubbed; PobParser fan-out not yet wired".into(),
            },
        })
        .collect();

    {
        let mut g = state.in_flight.lock();
        *g = g.saturating_sub(batch_size);
    }

    Ok(Json(ScoreBatchResponse {
        batch_id: req.batch_id,
        results,
    }))
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}
