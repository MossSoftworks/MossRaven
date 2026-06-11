//! Tier-4 backends — local (in-process pob-headless via Rayon) and remote
//! (HTTP fan-out to a pool of `mossraven-node` URLs).
//!
//! Both implement [`JudgeBackend`]. Selection is by config; v1 ships the
//! `local` impl and a skeletal `remote` impl that's wired but not benchmarked.

use async_trait::async_trait;
use mossraven_node_protocol::{ScoreBatchRequest, ScoreBatchResponse, VariantRequest, VariantPayload};
use mossraven_pob::{BuildStats, PobParser};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("pob engine error: {0}")]
    Pob(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("no healthy nodes available")]
    NoHealthyNodes,
    #[error("not implemented (stub)")]
    NotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Tier3Config {
    /// In-process pob-headless across host cores (default; gaming-rig deployment).
    Local,
    /// HTTP fan-out to a pool of mossraven-node URLs (power-user / homelab).
    Remote { node_urls: Vec<String>, bearer: String },
}

#[async_trait]
pub trait JudgeBackend: Send + Sync {
    async fn score(
        &self,
        variants: Vec<(String, String)>,
    ) -> Result<Vec<(String, Result<BuildStats, String>)>, JudgeError>;
}

// -------- Local --------

/// In-process Tier-4 backend. Holds a pool of PobParsers; each parser owns
/// its own OS thread + Lua VM (mlua::Lua is `!Send`, so we can't share one
/// across worker threads — only across async tasks that all hit the same
/// dedicated thread). With pool_size > 1, variants are round-robined across
/// parsers so multiple cores actually do work concurrently.
///
/// Memory cost per parser: ~50-100 MB (Lua VM + PoB2 game data). Cap pool
/// size at min(num_cpus / 2, 4) for a reasonable RAM ceiling.
pub struct LocalBackend {
    pool: Vec<Arc<PobParser>>,
}

impl LocalBackend {
    /// Single-parser constructor (back-compat with older call sites).
    pub fn new(parser: Arc<PobParser>) -> Self {
        Self { pool: vec![parser] }
    }

    /// Construct with N PobParser workers initialized in advance.
    /// Each one calls `PobParser::new(pob_path)` sequentially so the
    /// process-global `set_current_dir` inside `init()` doesn't race.
    pub async fn with_pool(
        pob_path: &std::path::Path,
        pool_size: usize,
    ) -> anyhow::Result<Self> {
        let pool_size = pool_size.max(1);
        let mut pool = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            tracing::info!(worker = i, total = pool_size, "initializing PobParser worker");
            pool.push(Arc::new(PobParser::new(pob_path).await?));
        }
        Ok(Self { pool })
    }

    /// Construct from a pre-built pool. Used when an external caller has
    /// already instantiated the parsers (e.g. mossraven-service reusing its
    /// startup parser as the first worker).
    pub fn from_pool(pool: Vec<Arc<PobParser>>) -> Self {
        Self { pool }
    }

    pub fn pool_size(&self) -> usize {
        self.pool.len()
    }
}

#[async_trait]
impl JudgeBackend for LocalBackend {
    async fn score(
        &self,
        variants: Vec<(String, String)>,
    ) -> Result<Vec<(String, Result<BuildStats, String>)>, JudgeError> {
        use futures::future::join_all;

        let n = self.pool.len().max(1);

        // Round-robin into one bucket per parser. Each bucket's variants
        // serialize on that parser's worker thread (Lua VM is !Send) but
        // buckets run concurrently across parsers.
        let mut buckets: Vec<Vec<(String, String)>> = (0..n).map(|_| Vec::new()).collect();
        for (i, v) in variants.into_iter().enumerate() {
            buckets[i % n].push(v);
        }

        let futures = buckets.into_iter().enumerate().map(|(i, batch)| {
            let parser = self.pool[i].clone();
            async move {
                let mut out: Vec<(String, Result<BuildStats, String>)> = Vec::with_capacity(batch.len());
                for (id, xml) in batch {
                    let r = parser.parse(xml.as_bytes()).await;
                    let stats: Result<BuildStats, String> = match r {
                        Ok(bytes) => serde_json::from_slice::<BuildStats>(&bytes)
                            .map_err(|e| format!("BuildStats deserialize failed: {e}")),
                        Err(e) => Err(e.to_string()),
                    };
                    out.push((id, stats));
                }
                out
            }
        });

        let bucket_results = join_all(futures).await;
        Ok(bucket_results.into_iter().flatten().collect())
    }
}

// -------- Remote --------

pub struct RemoteBackend {
    pub node_urls: Vec<String>,
    pub bearer: String,
    http: reqwest::Client,
}

impl RemoteBackend {
    pub fn new(node_urls: Vec<String>, bearer: String) -> Self {
        Self {
            node_urls,
            bearer,
            http: reqwest::Client::new(),
        }
    }

    fn pick_node(&self, batch_id: &str) -> Option<&str> {
        if self.node_urls.is_empty() {
            return None;
        }
        // Cheap, predictable distribution — hash batch_id to a node index.
        let idx = batch_id
            .bytes()
            .fold(0usize, |acc, b| acc.wrapping_add(b as usize))
            % self.node_urls.len();
        Some(self.node_urls[idx].as_str())
    }
}

#[async_trait]
impl JudgeBackend for RemoteBackend {
    async fn score(
        &self,
        variants: Vec<(String, String)>,
    ) -> Result<Vec<(String, Result<BuildStats, String>)>, JudgeError> {
        let batch_id = format!("b-{}", variants.len());
        let url = self.pick_node(&batch_id).ok_or(JudgeError::NoHealthyNodes)?;

        let req = ScoreBatchRequest {
            batch_id: batch_id.clone(),
            variants: variants
                .iter()
                .map(|(id, xml)| VariantRequest {
                    variant_id: id.clone(),
                    payload: VariantPayload::PobXml(xml.clone()),
                })
                .collect(),
        };

        let response = self
            .http
            .post(format!("{url}/score"))
            .bearer_auth(&self.bearer)
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json::<ScoreBatchResponse>()
            .await?;

        Ok(response
            .results
            .into_iter()
            .map(|r| {
                let id = r.variant_id;
                match r.outcome {
                    mossraven_node_protocol::VariantOutcome::Ok { stats, .. } => (
                        id,
                        serde_json::from_value::<BuildStats>(stats)
                            .map_err(|e| e.to_string()),
                    ),
                    mossraven_node_protocol::VariantOutcome::Error { message } => (id, Err(message)),
                }
            })
            .collect())
    }
}
