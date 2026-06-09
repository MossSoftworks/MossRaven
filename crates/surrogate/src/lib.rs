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

pub mod vocab;

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

/// Structured mutation operations. The cascade applies these in-order to the
/// seed PoB XML before Tier 3 scoring, so each variant gets a uniquely-mutated
/// XML → unique BuildStats → real cell-coords diversity in the archive.
///
/// v1: gem-attribute mutations (level/quality/swap). These are XML-rewriteable
/// without touching PoB's Lua API. Future ops (item swap, passive allocation)
/// will route through `PobParser::apply_mutations` for PoB-side validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MutationOp {
    /// Set the `level` attribute on the first `<Gem>` whose `nameSpec` matches.
    SetGemLevel { gem: String, level: u32 },
    /// Set the `quality` attribute on the first `<Gem>` whose `nameSpec` matches.
    SetGemQuality { gem: String, quality: u32 },
    /// Replace the first `nameSpec="OLD"` with `nameSpec="NEW"` — swaps the gem.
    SwapGem { old: String, new: String },
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
    /// Structured mutation operations to apply to the seed XML before Tier 3
    /// scores this variant. Empty = no XML changes, variant scores identically
    /// to seed.
    #[serde(default)]
    pub ops: Vec<MutationOp>,
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
        // Vocab block is datamined (HivemindOverlord/poe2-mcp) and embedded
        // into the binary at compile time — no I/O at request time. See
        // crates/surrogate/src/vocab.rs for the loader.
        let vocab_block = vocab::prompt_block(200, 200);
        let system = format!(
            "You are a Path of Exile 2 (NOT Path of Exile 1) build-mutation generator. \
             \n\nCRITICAL: This is POE 2 (early access v0.3+). Do NOT reference any of these \
             PoE1-only concepts (they do not exist in PoE2): Vaal skills (Vaal Cold Snap, \
             Vaal Fireball, etc.), 'Increased Critical Strikes' / 'Increased Critical Damage' \
             support gems (PoE1 names), 'Elemental Focus' support gem, Watcher's Eye, \
             Headhunter, The Pandemonius, Shaper/Elder influences, Cluster Jewels, \
             Aura Reservation Efficiency support, Spirit Burst Barrage.\n\n\
             Use ONLY skill and support gem names from this datamined list. If you don't \
             see a name in this list, describe the EFFECT abstractly (e.g. \"a cold-conversion \
             support gem\") rather than inventing a PoE1 name.\n\n\
             {vocab_block}\n\
             \n\
             Output ONLY valid JSON. No prose, no markdown fences. \
             Each mutation is a single targeted tweak to the seed build (gem swap, \
             passive reshuffle, gear-mod change, scaling-vector pivot). \
             The PoB XML you receive is the current state; you do not need to return \
             full XML — just describe the mutation in the 'mutation' field. \
             The orchestrator will apply mutations to the seed XML separately.",
        );
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
             For each mutation, ALSO return a `ops` array of STRUCTURED ops that the engine will\n\
             actually apply to the PoB XML. Each op is one of:\n\
               {{\"op\":\"set_gem_level\", \"gem\":\"Whirling Slash\", \"level\":18}}\n\
               {{\"op\":\"set_gem_quality\", \"gem\":\"Whirling Slash\", \"quality\":20}}\n\
               {{\"op\":\"swap_gem\", \"old\":\"Inspiration\", \"new\":\"Frigid Bond\"}}\n\
             The `gem`/`old`/`new` strings must match the `nameSpec` attribute on a `<Gem>` element\n\
             in the PoB XML. If you can't express the mutation as ops, return [] for ops (the engine\n\
             will still record the description but the build won't actually differ from the seed).\n\n\
             Return JSON of shape:\n\
             {{\n  \"mutations\": [\n    {{ \"variant_id\": \"m1\", \"mutation\": \"swap support gem Inspiration for Frigid Bond\", \"cell_focus\": \"cold/es/boss/unique-driven\", \"ops\": [{{\"op\":\"swap_gem\",\"old\":\"Inspiration\",\"new\":\"Frigid Bond\"}}] }},\n    ...\n  ]\n}}",
            xml = &seed_pob_xml[..seed_pob_xml.len().min(2000)],
            count = count,
        );

        let raw = self.chat(&system, &user).await?;
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
                // Parse structured ops if the model provided them. Each op is
                // {"op": "set_gem_level"|"set_gem_quality"|"swap_gem", ...args}.
                let ops: Vec<MutationOp> = m
                    .get("ops")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|op| serde_json::from_value::<MutationOp>(op.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                Some(MutationProposal {
                    variant_id,
                    pob_xml: seed_pob_xml.to_string(),
                    origin_hypothesis: Some(mutation),
                    cell_focus,
                    ops,
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
            .map(|i| {
                // Each mock variant gets a different gem-level mutation on the seed's
                // first gem. The variant_id determines the level deterministically,
                // so successive runs produce repeatable diversity. PoB rescores each
                // mutated XML differently → cells fill with actually-distinct stats.
                let level = (3 + (i as u32 * 2) % 18) + 1; // 4, 6, 8, ..., 20
                MutationProposal {
                    variant_id: format!("mock-{i}"),
                    pob_xml: seed_pob_xml.to_string(),
                    origin_hypothesis: Some(format!(
                        "mock mutation #{i} — set primary gem level to {level}"
                    )),
                    cell_focus: Some(cells[i % cells.len()].to_string()),
                    ops: vec![
                        // Set level on whatever the first gem is. For the Ritualist
                        // seed.xml that's "Whirling Slash"; for other seeds it picks
                        // up whatever PoB has as the first <Gem> element. Using a
                        // wildcard-ish approach: SetGemLevel matches "*" → "first gem".
                        MutationOp::SetGemLevel {
                            gem: "*".to_string(),
                            level,
                        },
                    ],
                }
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
