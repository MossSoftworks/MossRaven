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

/// Find the first `<Gem nameSpec="X">` inside the `<Skill mainActiveSkill="1">`
/// block — that's the scored active skill whose level actually moves DPS in
/// POE2 (supports are level-agnostic). Returns None if the build XML doesn't
/// have a `mainActiveSkill="1"` annotation or no gem under it.
pub fn find_main_skill_gem_name(xml: &str) -> Option<String> {
    let needle = r#"mainActiveSkill="1""#;
    let flag_idx = xml.find(needle)?;
    // Walk forward from `mainActiveSkill="1"` to find the first nameSpec="...".
    // The Skill opening tag itself doesn't carry nameSpec, only its <Gem>
    // children do.
    let tail = &xml[flag_idx..];
    let ns_start = tail.find(r#"nameSpec=""#)?;
    let after_ns = ns_start + r#"nameSpec=""#.len();
    let end_quote = tail[after_ns..].find('"')?;
    Some(tail[after_ns..after_ns + end_quote].to_string())
}

/// Insert a comment marker before every `<Skill ... mainActiveSkill="1" ...>`
/// block so the surrogate can see which gem group is the scored skill. PoB
/// only reports DPS for this group; mutations to other groups (warcries,
/// herald sources, alt skill links) apply correctly but don't move the score.
fn annotate_main_skill(skills_block: &str) -> String {
    let needle = r#"mainActiveSkill="1""#;
    let mut out = String::with_capacity(skills_block.len() + 128);
    let mut cursor = 0;
    while let Some(rel) = skills_block[cursor..].find(needle) {
        let abs = cursor + rel;
        // Walk back from `mainActiveSkill="1"` to the `<Skill` that owns it.
        let skill_start = skills_block[..abs].rfind("<Skill ").unwrap_or(abs);
        out.push_str(&skills_block[cursor..skill_start]);
        out.push_str("\n<!-- *** MAIN SCORED SKILL — mutations to gems IN THIS BLOCK change DPS *** -->\n");
        // Advance cursor; the rest of the block (including the annotation
        // anchor) gets copied normally.
        cursor = skill_start;
        // Avoid infinite loop if multiple skills are flagged main: jump past
        // the needle so the next iteration looks for the NEXT main flag.
        let after_needle = abs + needle.len();
        out.push_str(&skills_block[cursor..after_needle]);
        cursor = after_needle;
    }
    out.push_str(&skills_block[cursor..]);
    out
}

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
fn default_max_tokens() -> u32 { 8192 }

impl OpenAiCompatConfig {
    pub fn cerebras_default(api_key: String) -> Self {
        Self {
            base_url: "https://api.cerebras.ai/v1".into(),
            model: "gpt-oss-120b".into(),
            api_key: Some(api_key),
            temperature: 0.4,
            // Cerebras's live free-tier models split between non-reasoning
            // (gpt-oss-120b) and reasoning (zai-glm-4.7). Reasoning models
            // burn 1-2K tokens thinking before emitting an answer; 2K was too
            // tight and caused `finish_reason: "length"` with the answer JSON
            // stranded inside `message.reasoning`. 8K gives headroom for
            // either family.
            max_tokens: 8192,
        }
    }
    pub fn local_ollama_default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen2.5:14b".into(),
            api_key: None,
            temperature: 0.4,
            max_tokens: 8192,
        }
    }
    pub fn gemini_default(api_key: String) -> Self {
        // Google AI Studio's OpenAI-compat endpoint. Free tier (no card):
        // gemini-2.5-flash-lite at 1500 req/day, 15 req/min — plenty for
        // our 2-call-per-generation cascade. Better instruction following
        // on negative constraints than the 120B-class Cerebras models.
        Self {
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            model: "gemini-2.5-flash-lite".into(),
            api_key: Some(api_key),
            temperature: 0.4,
            max_tokens: 8192,
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
    /// content. Non-2xx other than 429 surfaces as SurrogateError::BadStatus
    /// with the response body. 429 (rate limit) triggers an exponential
    /// backoff with up to 3 retries — Cerebras's free tier RPM limit makes
    /// this hit constantly on multi-gen runs, so we paper over it transparently.
    async fn chat(&self, system: &str, user: &str) -> Result<String, SurrogateError> {
        let mut delay_ms = 1000;
        let mut last_429: Option<(u16, String)> = None;
        for attempt in 0..4 {
            match self.chat_once(system, user).await {
                Ok(s) => return Ok(s),
                Err(SurrogateError::BadStatus { status: 429, body }) => {
                    last_429 = Some((429, body));
                    if attempt == 3 {
                        break;
                    }
                    tracing::warn!(
                        attempt = attempt + 1,
                        delay_ms,
                        "surrogate rate-limited (429); backing off"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    // 1s -> 4s -> 15s -> bail. The Cerebras free-tier RPM
                    // bucket refills every 60s, but a few short waits often
                    // get through during a refill window.
                    delay_ms = match delay_ms {
                        1000 => 4000,
                        4000 => 15000,
                        _ => delay_ms,
                    };
                    continue;
                }
                Err(other) => return Err(other),
            }
        }
        let (status, body) = last_429
            .unwrap_or((429, "rate limited (no body captured)".to_string()));
        Err(SurrogateError::BadStatus { status, body })
    }

    async fn chat_once(&self, system: &str, user: &str) -> Result<String, SurrogateError> {
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
        // Some providers (Cerebras zai-glm-4.7) split their output between
        // `message.content` (the answer the user asked for) and
        // `message.reasoning` (the chain-of-thought). When a reasoning model
        // hits `finish_reason: "length"` mid-thought, content stays empty.
        // Fall back to reasoning so we at least get something to JSON-extract.
        let msg = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .ok_or_else(|| SurrogateError::Schema(format!("no choices[0].message in {text}")))?;
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.trim().is_empty());
        let reasoning = msg
            .get("reasoning")
            .and_then(|c| c.as_str())
            .filter(|s| !s.trim().is_empty());
        let out = content.or(reasoning).ok_or_else(|| {
            SurrogateError::Schema(format!(
                "no usable message.content or message.reasoning in {text}"
            ))
        })?;
        Ok(out.to_string())
    }

    /// Extract a focused slice of the PoB XML that the surrogate can actually
    /// act on. The full seed is ~30 KB and our truncation-from-byte-0 was
    /// hiding the `<Skills>` block behind 2 KB of `<PlayerStat>` lines.
    ///
    /// Strategy: pull the `<Build>` opening tag (class + ascendancy) PLUS the
    /// `<Skills>...</Skills>` block (every gem the build has), AND annotate
    /// which `<Skill>` block is the scored one with a `<!-- *** MAIN SKILL
    /// (mutations here actually change DPS) *** -->` comment. The LLM was
    /// proposing mutations on decorative supports (e.g. Volcanic Eruption on
    /// the second Whirling Slash) which apply correctly but PoB doesn't score.
    fn extract_gem_slice(xml: &str, char_budget: usize) -> String {
        let class_line = xml
            .lines()
            .find(|l| l.contains("<Build "))
            .unwrap_or("")
            .to_string();
        let skills_start = xml.find("<Skills");
        let skills_end = xml.find("</Skills>");
        let skills_block = match (skills_start, skills_end) {
            (Some(s), Some(e)) if e > s => &xml[s..e + "</Skills>".len()],
            _ => "",
        };
        // Annotate the main scored skill block. PoB scores the `<Skill>` that
        // has `mainActiveSkill="1"` — every gem inside that block is on the
        // critical path. Other blocks are warcries, herald-sources, or alt
        // links the build uses for utility, none of which move the DPS that
        // Tier 3 reports.
        let annotated_skills = annotate_main_skill(skills_block);
        let combined = if class_line.is_empty() {
            annotated_skills.clone()
        } else {
            format!("{class_line}\n{annotated_skills}")
        };
        if combined.len() <= char_budget {
            return combined;
        }
        let half = char_budget / 2;
        format!(
            "{}\n…[trimmed {} chars]…\n{}",
            &combined[..half],
            combined.len() - char_budget,
            &combined[combined.len() - half..]
        )
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
             For each mutation, ALSO return an `ops` array of STRUCTURED ops the engine will\n\
             ACTUALLY APPLY to the PoB XML. Each op is one of:\n\
               {{\"op\":\"set_gem_level\", \"gem\":\"<exact nameSpec>\", \"level\":N}}     // 1..20\n\
               {{\"op\":\"set_gem_quality\", \"gem\":\"<exact nameSpec>\", \"quality\":Q}}  // 0..20\n\
               {{\"op\":\"swap_gem\", \"old\":\"<exact nameSpec>\", \"new\":\"<other POE2 gem>\"}}\n\n\
             ⚠️ POE2 SCORING RULES (read carefully — these are not PoE1):\n\
             - **Only the ACTIVE skill gem's `level` moves DPS.** This is the FIRST gem in the\n\
               `<Skill mainActiveSkill=\"1\">` block (the main skill, NOT its supports).\n\
             - **Support gem level changes are NO-OPS in PoE2.** Supports are binary in POE2;\n\
               their `level=\"N\"` attribute doesn't scale their effect. set_gem_level on a support\n\
               wastes a variant slot.\n\
             - **Quality changes barely move DPS** for most gems. Don't lead with quality.\n\
             - **swap_gem is v1-broken** — it rewrites only the display label, not the scored skill.\n\n\
             To explore distinct DPS values, each variant MUST set the main skill gem to a\n\
             DIFFERENT level. Example: m1→level 4, m2→level 8, m3→level 12, m4→level 16, m5→level 20.\n\
             Variants targeting the SAME gem at the SAME level produce identical scores → wasted.\n\n\
             The `gem` / `old` / `new` strings MUST match a `nameSpec` attribute on a `<Gem>` element\n\
             in the PoB XML excerpt above — copy them VERBATIM, do not invent new gem names.\n\n\
             Return JSON of shape:\n\
             {{\n  \"mutations\": [\n    {{ \"variant_id\": \"m1\", \"mutation\": \"undercut the main skill — Whirling Slash at level 4 for fast-clear, low-mana variant\", \"cell_focus\": \"physical/armour/clear/gem-levels\", \"ops\": [{{\"op\":\"set_gem_level\",\"gem\":\"Whirling Slash\",\"level\":4}}] }},\n    {{ \"variant_id\": \"m2\", \"mutation\": \"mid-level main skill at 12 for resource trade-off\", \"cell_focus\": \"physical/armour/hybrid/gem-levels\", \"ops\": [{{\"op\":\"set_gem_level\",\"gem\":\"Whirling Slash\",\"level\":12}}] }},\n    ...\n  ]\n}}",
            xml = Self::extract_gem_slice(seed_pob_xml, 6000),
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
        // Find the main scored skill's gem name in the seed XML — the first
        // `<Gem nameSpec="...">` inside the `<Skill mainActiveSkill="1">`
        // block. That's the only gem whose level changes move PoB's DPS for
        // this build (POE2 supports are level-agnostic). If we can't find it,
        // fall back to "*" wildcard.
        let main_skill_gem = find_main_skill_gem_name(seed_pob_xml).unwrap_or_else(|| "*".to_string());
        Ok((0..count)
            .map(|i| {
                // Pick a level from the explore set [4, 8, 12, 16, 20] so each
                // variant produces a distinct DPS. The cycle wraps if count > 5.
                let levels = [4u32, 8, 12, 16, 20];
                let level = levels[i % levels.len()];
                MutationProposal {
                    variant_id: format!("mock-{i}"),
                    pob_xml: seed_pob_xml.to_string(),
                    origin_hypothesis: Some(format!(
                        "mock mutation #{i} — set main skill {main_skill_gem} to level {level}"
                    )),
                    cell_focus: Some(cells[i % cells.len()].to_string()),
                    ops: vec![MutationOp::SetGemLevel {
                        gem: main_skill_gem.clone(),
                        level,
                    }],
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
