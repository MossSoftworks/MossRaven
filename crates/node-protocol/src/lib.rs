//! Wire types shared between `mossraven-service` (Tier-3 remote backend) and
//! `mossraven-node` (the power-user farm worker).
//!
//! Protocol: HTTP/JSON, stateless. Each request is self-contained so nodes can
//! be load-balanced without affinity.

use serde::{Deserialize, Serialize};

/// One variant to be scored. The payload format is intentionally open so we can
/// evolve it (raw PoB XML now, structured spec later) without changing the
/// envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantRequest {
    pub variant_id: String,
    pub payload: VariantPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum VariantPayload {
    /// Raw PoB2 XML export.
    PobXml(String),
    /// Future: structured spec (gem/skill/passive/gear deltas) — let mossraven-node
    /// assemble the XML on its side to reduce wire size.
    Spec(serde_json::Value),
}

/// A batch of variants scored together. Nodes fan within themselves across
/// their own cores before returning the whole batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBatchRequest {
    pub batch_id: String,
    pub variants: Vec<VariantRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBatchResponse {
    pub batch_id: String,
    pub results: Vec<VariantResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantResult {
    pub variant_id: String,
    #[serde(flatten)]
    pub outcome: VariantOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum VariantOutcome {
    Ok {
        stats: serde_json::Value,
        pob2_version: String,
    },
    Error {
        message: String,
    },
}

/// Health response for `GET /health`. Used by mossraven-service to track liveness
/// and capacity across a pool of nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub version: String,
    pub pob2_version: String,
    pub cores: usize,
    pub in_flight: usize,
}
