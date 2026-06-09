//! Tier-1 driver — the "dreamer" that seeds hypotheses and curates the
//! frontier. Two interchangeable implementations:
//!
//! - **Mode A**: thin Anthropic API client. Headless, automated, schedulable.
//!   Holds `ANTHROPIC_API_KEY`. Metered at API rates.
//! - **Mode B**: the search service exposes itself as an MCP server; the user
//!   drives interactively via Claude Code or Cowork. Uses subscription, not
//!   metered (interactive only — `claude -p` / Agent SDK has separate metering
//!   since 2026-06-15).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

#[derive(Debug, Error)]
pub enum DreamerError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("anthropic returned non-OK: {status} — {body}")]
    BadStatus { status: u16, body: String },
    #[error("schema mismatch: {0}")]
    Schema(String),
    #[error("not implemented (stub)")]
    NotImplemented,
    #[error("Mode B is driven externally via the MCP control surface, not via this trait")]
    DriverIsExternal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DreamerMode {
    Api { model: String, api_key_env: String },
    ClaudeCode,
    Cowork { mcp_public_url: String },
}

/// A hypothesis fed into the search. Free-text from the dreamer; the search
/// core turns it into a seed PoB XML before mutation begins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub concept: String,
    pub rationale: Option<String>,
    pub initial_cell_focus: Option<String>,
    /// Optional starting PoB XML the engine will mutate from. v1: usually
    /// empty — the dreamer returns concept + rationale, not a full XML.
    /// Future: ground hypothesis in a real meta build via poe2-mcp / poe.ninja,
    /// then return the XML here. Without a seed XML the cascade still runs
    /// (proposals are generated) but Tier 3 has nothing meaningful to score.
    #[serde(default)]
    pub seed_pob_xml: Option<String>,
}

#[async_trait]
pub trait TierOneDriver: Send + Sync {
    async fn seed(&self, prompt: &str) -> Result<Hypothesis, DreamerError>;
    async fn curate(
        &self,
        archive_snapshot: &Value,
    ) -> Result<Hypothesis, DreamerError>;
}

/// Mode A — Anthropic Messages API driver.
pub struct AnthropicApiDriver {
    pub model: String,
    pub api_key: String,
    pub max_tokens: u32,
    http: reqwest::Client,
}

impl AnthropicApiDriver {
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
            max_tokens: 1024,
            http: reqwest::Client::new(),
        }
    }

    async fn message(&self, system: &str, user: &str) -> Result<String, DreamerError> {
        let body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": [
                { "role": "user", "content": user }
            ]
        });
        let resp = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(DreamerError::BadStatus {
                status: status.as_u16(),
                body: text,
            });
        }
        // Response shape: { content: [ { type: "text", text: "..." }, ... ], ... }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| DreamerError::Schema(format!("body not JSON: {e} — {text}")))?;
        let out = v
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.iter().find(|x| x.get("type").and_then(|t| t.as_str()) == Some("text")))
            .and_then(|x| x.get("text"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| DreamerError::Schema(format!("no text content in response: {text}")))?
            .to_string();
        Ok(out)
    }

    fn parse_hypothesis(raw: &str) -> Result<Hypothesis, DreamerError> {
        // Models sometimes wrap JSON in ```json fences. Strip them.
        let trimmed = raw.trim();
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .map(|s| s.trim_start_matches('\n'))
            .and_then(|s| s.rsplit_once("```").map(|(a, _)| a))
            .unwrap_or(trimmed);
        let v: Value = match serde_json::from_str(inner) {
            Ok(v) => v,
            Err(_) => {
                // Fallback: look for the first balanced {...} and try that.
                let start = inner.find('{').ok_or_else(|| {
                    DreamerError::Schema(format!("no JSON object in response: {raw}"))
                })?;
                serde_json::from_str(&inner[start..])
                    .map_err(|e| DreamerError::Schema(format!("JSON parse failed: {e} — {raw}")))?
            }
        };
        let concept = v
            .get("concept")
            .and_then(|c| c.as_str())
            .ok_or_else(|| DreamerError::Schema(format!("no `concept` field: {v}")))?
            .to_string();
        let rationale = v.get("rationale").and_then(|c| c.as_str()).map(String::from);
        let initial_cell_focus = v.get("initial_cell_focus").and_then(|c| c.as_str()).map(String::from);
        let seed_pob_xml = v.get("seed_pob_xml").and_then(|c| c.as_str()).map(String::from);
        Ok(Hypothesis {
            concept,
            rationale,
            initial_cell_focus,
            seed_pob_xml,
        })
    }
}

#[async_trait]
impl TierOneDriver for AnthropicApiDriver {
    async fn seed(&self, prompt: &str) -> Result<Hypothesis, DreamerError> {
        let system = "You are a Path of Exile 2 build theorycrafter. \
                      Output ONLY valid JSON. No prose, no markdown fences. \
                      Given a free-text build concept, return a structured hypothesis \
                      that names what's interesting about it and where in build-space \
                      the search should begin.";
        let user = format!(
            "User concept: {prompt}\n\n\
             Return JSON of shape:\n\
             {{\n  \"concept\": \"a refined one-line concept statement\",\n  \"rationale\": \"why this might work, mechanically grounded\",\n  \"initial_cell_focus\": \"damage_type/defense_layer/role/scaling_vector — the MAP-Elites cell to anchor at\"\n}}",
        );
        let raw = self.message(system, &user).await?;
        Self::parse_hypothesis(&raw)
    }

    async fn curate(
        &self,
        archive_snapshot: &Value,
    ) -> Result<Hypothesis, DreamerError> {
        let system = "You are a Path of Exile 2 build theorycrafter looking at a MAP-Elites \
                      archive. Output ONLY valid JSON. No prose. \
                      Read what's been filled in. Notice the GAPS — empty high-potential cells, \
                      directions that haven't been tried. Suggest the next hypothesis to explore. \
                      Prefer pivots that explore new regions over refinements of existing cells.";
        let user = format!(
            "Current archive snapshot:\n{}\n\n\
             Return JSON of shape:\n\
             {{\n  \"concept\": \"the next idea to try\",\n  \"rationale\": \"what gap in the archive this fills\",\n  \"initial_cell_focus\": \"damage_type/defense_layer/role/scaling_vector\"\n}}",
            serde_json::to_string_pretty(archive_snapshot).unwrap_or_default(),
        );
        let raw = self.message(system, &user).await?;
        Self::parse_hypothesis(&raw)
    }
}

/// Mode B marker driver. Returns `DriverIsExternal` for every call — Mode B
/// drives the engine from outside (Claude Code or Cowork as MCP client),
/// so the service's own driver does nothing.
pub struct ExternalMcpDriver;

#[async_trait]
impl TierOneDriver for ExternalMcpDriver {
    async fn seed(&self, _prompt: &str) -> Result<Hypothesis, DreamerError> {
        Err(DreamerError::DriverIsExternal)
    }
    async fn curate(
        &self,
        _archive_snapshot: &Value,
    ) -> Result<Hypothesis, DreamerError> {
        Err(DreamerError::DriverIsExternal)
    }
}
