//! Tier-2 surrogate provider.
//!
//! The surrogate cheap-scores mutation candidates BEFORE the expensive Tier-3
//! sim runs. Provider-agnostic behind an OpenAI-compatible chat-completions
//! interface — swap providers by changing `base_url` + `model` in config,
//! no code change required.
//!
//! ## Built-in providers
//!
//! - **Cerebras** (default): `https://api.cerebras.ai/v1`, free tier 1M tok/day.
//! - **Local Ollama**: `http://localhost:11434/v1`, zero marginal cost.
//! - **Groq / OpenRouter / anyone OpenAI-compatible**: drop in the URL.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SurrogateError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider returned non-OK: {status} — {body}")]
    BadStatus { status: u16, body: String },
    #[error("schema mismatch: {0}")]
    Schema(String),
    #[error("not implemented (stub)")]
    NotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateScore {
    pub variant_id: String,
    /// 0.0 = uninteresting / implausible; 1.0 = high-novelty + plausible.
    pub interest: f32,
    /// 0.0 = certain to fail hard-constraint validation; 1.0 = passes.
    pub plausibility: f32,
    /// Optional one-line rationale.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationProposal {
    pub variant_id: String,
    pub pob_xml: String,
    pub origin_hypothesis: Option<String>,
    /// Cell coordinates the surrogate THINKS this mutation explores, as a
    /// `"damage_type/defense_layer/role/scaling_vector"` string. The engine
    /// uses this to place the resulting build in the MAP-Elites archive
    /// without having to infer all axes from BuildStats alone.
    #[serde(default)]
    pub cell_focus: Option<String>,
}

#[async_trait]
pub trait SurrogateProvider: Send + Sync {
    async fn propose_mutations(
        &self,
        seed_pob_xml: &str,
        seed_hypothesis: &str,
        count: usize,
    ) -> Result<Vec<MutationProposal>, SurrogateError>;

    async fn cheap_score(
        &self,
        candidates: &[MutationProposal],
    ) -> Result<Vec<CandidateScore>, SurrogateError>;
}

/// OpenAI-compatible provider config. Same struct for Cerebras, Ollama, Groq, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default = "default_temp")]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_temp() -> f32 { 0.4 }
fn default_max_tokens() -> u32 { 2048 }

impl OpenAiCompatConfig {
    pub fn cerebras_default(api_key: String) -> Self {
        Self {
            base_url: "https://api.cerebras.ai/v1".into(),
            model: "gpt-oss-120b".into(),
            api_key: Some(api_key),
            temperature: 0.4,
            max_tokens: 2048,
        }
    }
    pub fn local_ollama_default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen2.5:14b".into(),
            api_key: None,
            temperature: 0.4,
            max_tokens: 2048,
        }
    }
}

/// Default surrogate impl that talks to any OpenAI-compatible chat endpoint.
pub struct OpenAiCompatSurrogate {
    cfg: OpenAiCompatConfig,
    http: reqwest::Client,
}

impl OpenAiCompatSurrogate {
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
        }
    }

    /// POST to {base_url}/chat/completions and return the assistant message
    /// content. Any non-2xx surfaces as SurrogateError::BadStatus with the
    /// response body, which is what we want for visibility into provider
    /// errors (rate limits, model-not-found, etc.).
    async fn chat(&self, system: &str, user: &str) -> Result<String, SurrogateError> {
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user",   "content": user },
            ],
            "temperature": self.cfg.temperature,
            "max_tokens": self.cfg.max_tokens,
            "stream": false,
        });

        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.cfg.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SurrogateError::BadStatus {
                status: status.as_u16(),
                body: text,
            });
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| SurrogateError::Schema(format!("body not JSON: {e} — {text}")))?;
        let content = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| SurrogateError::Schema(format!("no choices[0].message.content in {text}")))?
            .to_string();
        Ok(content)
    }

    /// Salvage a JSON value from a model response. Models sometimes wrap JSON
    /// in ```json fences, prose preambles, or both — we pull the first {...}
    /// or [...] balanced span.
    fn extract_json(blob: &str) -> Option<Value> {
        // Strip ```json ... ``` fences if present.
        let trimmed = blob.trim();
        let stripped = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .map(|s| s.trim_start_matches('\n'))
            .and_then(|s| s.rsplit_once("```").map(|(a, _)| a))
            .unwrap_or(trimmed);

        // Try direct parse first.
        if let Ok(v) = serde_json::from_str::<Value>(stripped) {
            return Some(v);
        }
        // Find the first { or [ and try to parse from there.
        for (i, c) in stripped.char_indices() {
            if c == '{' || c == '[' {
                if let Ok(v) = serde_json::from_str::<Value>(&stripped[i..]) {
                    return Some(v);
                }
            }
        }
        None
    }
}

#[async_trait]
impl SurrogateProvider for OpenAiCompatSurrogate {
    async fn propose_mutations(
        &self,
        seed_pob_xml: &str,
        seed_hypothesis: &str,
        count: usize,
    ) -> Result<Vec<MutationProposal>, SurrogateError> {
        let system = "You are a Path of Exile 2 build-mutation generator. \
                      Output ONLY valid JSON. No prose, no markdown fences. \
                      Each mutation is a single targeted tweak to the seed build (gem swap, \
                      passive reshuffle, gear-mod change, scaling-vector pivot). \
                      The PoB XML you receive is the current state; you do not need to return \
                      full XML — just describe the mutation in the 'mutation' field. \
                      The orchestrator will apply mutations to the seed XML separately.";
        let user = format!(
            "Seed hypothesis: {seed_hypothesis}\n\n\
             Current PoB XML (truncated to first 2000 chars):\n{xml}\n\n\
             Propose {count} distinct mutations. Each should explore a different \
             axis (damage scaling, defense layer, gem support, jewel slot, unique swap). \
             Avoid the meta — prefer obscure-but-plausible interactions.\n\n\
             Also TAG each mutation with the MAP-Elites cell it explores, as \
             \"damage_type/defense_layer/role/scaling_vector\" where:\n\
               damage_type    ∈ physical | cold | fire | lightning | chaos | dot | minion\n\
               defense_layer  ∈ evasion | armour | es | hybrid | block-spell | dodge-roll\n\
               role           ∈ clear | boss | hybrid\n\
               scaling_vector ∈ gem-levels | attribute-stack | unique-driven | tree-keystone\n\n\
             Return JSON of shape:\n\
             {{\n  \"mutations\": [\n    {{ \"variant_id\": \"m1\", \"mutation\": \"swap support gem X for Y\", \"cell_focus\": \"cold/es/boss/unique-driven\" }},\n    ...\n  ]\n}}",
            xml = &seed_pob_xml[..seed_pob_xml.len().min(2000)],
            count = count,
        );

        let raw = self.chat(system, &user).await?;
        let parsed = Self::extract_json(&raw)
            .ok_or_else(|| SurrogateError::Schema(format!("no JSON in response: {raw}")))?;
        let arr = parsed
            .get("mutations")
            .and_then(|m| m.as_array())
            .ok_or_else(|| SurrogateError::Schema(format!("expected `mutations` array: {parsed}")))?;

        Ok(arr
            .iter()
            .filter_map(|m| {
                let variant_id = m.get("variant_id").and_then(|v| v.as_str())?.to_string();
                let mutation = m.get("mutation").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let cell_focus = m.get("cell_focus").and_then(|v| v.as_str()).map(String::from);
                Some(MutationProposal {
                    variant_id,
                    // v1: we don't yet rewrite the XML on the surrogate side; pass the seed
                    // along with the mutation note in origin_hypothesis. Tier 3 will eventually
                    // get a small mutation-applier that operates on the spec, not raw XML.
                    pob_xml: seed_pob_xml.to_string(),
                    origin_hypothesis: Some(mutation),
                    cell_focus,
                })
            })
            .collect())
    }

    async fn cheap_score(
        &self,
        candidates: &[MutationProposal],
    ) -> Result<Vec<CandidateScore>, SurrogateError> {
        if candidates.is_empty() {
            return Ok(vec![]);
        }
        let system = "You are a Path of Exile 2 build novelty + plausibility scorer. \
                      Output ONLY valid JSON. No prose. \
                      For each variant, score 'interest' (0.0 = boring/meta, 1.0 = novel and worth simulating) \
                      and 'plausibility' (0.0 = violates hard game rules, 1.0 = mechanically sound). \
                      Be ruthless about meta — penalize anything that looks like a known top-ladder template. \
                      Reward genuinely off-axis ideas.";

        let list: String = candidates
            .iter()
            .map(|c| {
                format!(
                    "- id={} mutation={}",
                    c.variant_id,
                    c.origin_hypothesis.as_deref().unwrap_or("(no description)"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let user = format!(
            "Score these mutation candidates:\n{list}\n\n\
             Return JSON of shape:\n\
             {{\n  \"scores\": [\n    {{ \"variant_id\": \"m1\", \"interest\": 0.7, \"plausibility\": 0.9, \"note\": \"off-meta cold DoT route\" }},\n    ...\n  ]\n}}"
        );

        let raw = self.chat(system, &user).await?;
        let parsed = Self::extract_json(&raw)
            .ok_or_else(|| SurrogateError::Schema(format!("no JSON in response: {raw}")))?;
        let arr = parsed
            .get("scores")
            .and_then(|s| s.as_array())
            .ok_or_else(|| SurrogateError::Schema(format!("expected `scores` array: {parsed}")))?;

        Ok(arr
            .iter()
            .filter_map(|s| {
                let variant_id = s.get("variant_id").and_then(|v| v.as_str())?.to_string();
                let interest = s.get("interest").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
                let plausibility = s.get("plausibility").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
                let note = s.get("note").and_then(|v| v.as_str()).map(String::from);
                Some(CandidateScore {
                    variant_id,
                    interest: interest.clamp(0.0, 1.0),
                    plausibility: plausibility.clamp(0.0, 1.0),
                    note,
                })
            })
            .collect())
    }
}

/// Mock surrogate for tests and bench harness. Returns deterministic scores.
pub struct MockSurrogate;

#[async_trait]
impl SurrogateProvider for MockSurrogate {
    async fn propose_mutations(
        &self,
        seed_pob_xml: &str,
        _seed_hypothesis: &str,
        count: usize,
    ) -> Result<Vec<MutationProposal>, SurrogateError> {
        // Spread mock mutations across a handful of cells so the cascade
        // exercises archive.try_place across multiple coords.
        let cells = [
            "cold/es/boss/unique-driven",
            "lightning/evasion/clear/gem-levels",
            "fire/armour/boss/tree-keystone",
            "chaos/hybrid/clear/attribute-stack",
            "physical/armour/boss/unique-driven",
        ];
        Ok((0..count)
            .map(|i| MutationProposal {
                variant_id: format!("mock-{i}"),
                pob_xml: seed_pob_xml.to_string(),
                origin_hypothesis: Some(format!("mock mutation #{i}")),
                cell_focus: Some(cells[i % cells.len()].to_string()),
            })
            .collect())
    }

    async fn cheap_score(
        &self,
        candidates: &[MutationProposal],
    ) -> Result<Vec<CandidateScore>, SurrogateError> {
        Ok(candidates
            .iter()
            .map(|c| CandidateScore {
                variant_id: c.variant_id.clone(),
                interest: 0.5,
                plausibility: 1.0,
                note: None,
            })
            .collect())
    }
}
