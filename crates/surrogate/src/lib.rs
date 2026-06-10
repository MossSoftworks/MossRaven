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

/// Parse the sentinel `[retry-after:N]` prefix that `chat_once` embeds in the
/// 429 body when the provider sent a `Retry-After: N` header. N is delta-seconds.
fn parse_retry_after_ms(body: &str) -> Option<u64> {
    let prefix = "[retry-after:";
    let start = body.find(prefix)?;
    let after = start + prefix.len();
    let end = body[after..].find(']')?;
    let secs: u64 = body[after..after + end].trim().parse().ok()?;
    // Cap at 5 minutes — if the provider asks for more, something is wrong
    // and we'd rather fail fast.
    Some((secs * 1000).min(5 * 60_000))
}

/// Byte range `[start, end)` of the scored socket group in a PoB2 build XML.
///
/// PoB picks the scored group via `mainSocketGroup="N"` on `<Build>` (1-based
/// index into the `<Skill>` children of the active `<SkillSet>`). The
/// `mainActiveSkill` attribute on each `<Skill>` is the index of the active
/// gem *within* that group — every group carries one, so it can't identify
/// the scored group (the original heuristic here matched the first group with
/// `mainActiveSkill="1"`, which on real 0.5 exports is a utility group like
/// Frost Bomb — mutations landed there and never moved CombinedDPS).
fn main_socket_group_range(xml: &str) -> Option<(usize, usize)> {
    let group_idx: usize = attr_in(xml, "<Build ", "mainSocketGroup")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let set_id = attr_in(xml, "<Skills ", "activeSkillSet").unwrap_or_else(|| "1".into());

    // Locate the active <SkillSet id="..."> ... </SkillSet> block. Builds
    // exported without skill sets fall back to the whole <Skills> block.
    let needle = format!("id=\"{set_id}\"");
    let mut set_start = None;
    let mut cursor = 0;
    while let Some(rel) = xml[cursor..].find("<SkillSet ") {
        let abs = cursor + rel;
        let tag_end = abs + xml[abs..].find('>')?;
        if xml[abs..tag_end].contains(&needle) {
            set_start = Some(tag_end + 1);
            break;
        }
        cursor = tag_end;
    }
    let (region_start, region_end) = match set_start {
        Some(s) => (s, s + xml[s..].find("</SkillSet>").unwrap_or(xml.len() - s)),
        None => {
            let s = xml.find("<Skills")?;
            (s, s + xml[s..].find("</Skills>").unwrap_or(xml.len() - s))
        }
    };
    let region = &xml[region_start..region_end];

    // Walk <Skill ...> groups (NOT <SkillSet>) counting to group_idx.
    let mut count = 0;
    let mut pos = 0;
    while let Some(rel) = region[pos..].find("<Skill ") {
        let abs = pos + rel;
        count += 1;
        let body_end = abs + region[abs..].find("</Skill>").map(|e| e + "</Skill>".len())
            .unwrap_or(region.len() - abs);
        if count == group_idx {
            return Some((region_start + abs, region_start + body_end));
        }
        pos = body_end;
    }
    None
}

/// Public alias of [`main_socket_group_range`] — byte span `[start, end)` of
/// the scored socket group. The mutation applier in `mossraven-core` uses it
/// to constrain add/remove-gem ops to the group PoB actually scores.
pub fn main_socket_group_span(xml: &str) -> Option<(usize, usize)> {
    main_socket_group_range(xml)
}

/// Name of the scored active skill gem — `mainSocketGroup`'s group, gem
/// number `mainActiveSkill` (1-based, default 1) within it. This is the gem
/// whose `level=` actually moves PoB's reported DPS.
pub fn find_main_skill_gem_name(xml: &str) -> Option<String> {
    let (start, end) = main_socket_group_range(xml)?;
    let group = &xml[start..end];
    let gem_idx: usize = attr_in(group, "<Skill ", "mainActiveSkill")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut seen = 0;
    let mut pos = 0;
    while let Some(rel) = group[pos..].find("nameSpec=\"") {
        let abs = pos + rel + "nameSpec=\"".len();
        let endq = abs + group[abs..].find('"')?;
        seen += 1;
        if seen == gem_idx {
            return Some(group[abs..endq].to_string());
        }
        pos = endq;
    }
    None
}

/// Is a gem with this `nameSpec` present inside the SCORED socket group?
/// Case-insensitive. The mechanical guard in the cascade uses this to
/// reject/retarget LLM ops aimed at unscored groups — live runs showed
/// models repeatedly mutating a group-1 utility gem (Frost Bomb) despite
/// the prompt marker, producing 9/10 identical-DPS variants.
pub fn gem_in_main_group(xml: &str, gem_name: &str) -> bool {
    let Some((start, end)) = main_socket_group_range(xml) else {
        return false;
    };
    let group = xml[start..end].to_lowercase();
    let needle = format!("namespec=\"{}\"", gem_name.to_lowercase());
    group.contains(&needle)
}

/// Name of the Nth gem (1-based) in the scored socket group. N=1 is the
/// active skill itself; N>=2 are its supports. Used by the MockSurrogate to
/// pick a real support to remove without knowing the seed's contents.
pub fn nth_gem_in_main_group(xml: &str, n: usize) -> Option<String> {
    let (start, end) = main_socket_group_range(xml)?;
    let group = &xml[start..end];
    let mut seen = 0;
    let mut pos = 0;
    while let Some(rel) = group[pos..].find("nameSpec=\"") {
        let abs = pos + rel + "nameSpec=\"".len();
        let endq = abs + group[abs..].find('"')?;
        seen += 1;
        if seen == n {
            return Some(group[abs..endq].to_string());
        }
        pos = endq;
    }
    None
}

/// Value of `attr="..."` on the first occurrence of `tag_prefix` (e.g.
/// `"<Build "`). Searches only within that tag's opening `<...>`.
fn attr_in(xml: &str, tag_prefix: &str, attr: &str) -> Option<String> {
    let tag_start = xml.find(tag_prefix)?;
    let tag_end = tag_start + xml[tag_start..].find('>')?;
    let tag = &xml[tag_start..tag_end];
    let needle = format!("{attr}=\"");
    let i = tag.find(&needle)?;
    let start = i + needle.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
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
    /// Swap a gem for another. The applier resolves `new` through the PoB
    /// gem database and rewrites `gemId`/`skillId`/`variantId`/`nameSpec`
    /// together, so PoB scores the NEW skill (rewriting only the display
    /// name was the documented v1 no-op).
    SwapGem { old: String, new: String },
    /// Remove a gem (usually a support) from the scored socket group.
    /// PoE2 supports are binary, so dropping one genuinely changes the score.
    RemoveGem { gem: String },
    /// Add a support gem to the scored socket group. The applier synthesizes
    /// the full `<Gem>` element from the PoB gem database.
    AddSupportGem { gem: String },
    /// Allocate a passive-tree NOTABLE by name. The applier paths to it from
    /// the build's current allocation through the real tree graph (BFS,
    /// bounded hops, travel nodes paid for) and appends the whole connected
    /// path to `<Spec nodes>` — never a teleported/disconnected node. This is
    /// the operator class that attacks the viability DPS gap: gem ops top
    /// out ~75k on current seeds; the comfort floor is 500k.
    AllocateNotable { name: String },
    /// Flip which weapon loadout the build's active item set scores under —
    /// PoE2's clear-vs-boss weapon-set swap (SPEC §1.1). Rewrites
    /// `useSecondWeaponSet` on the active `<ItemSet>` so Tier 3 evaluates the
    /// other loadout. Only moves the score when the seed actually has gear in
    /// weapon set 2.
    SetActiveWeaponSet { use_second: bool },
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
    pub fn groq_default(api_key: String) -> Self {
        // Groq free tier (no card): llama-3.3-70b-versatile at 30 req/min,
        // 1K req/day, ~700 tok/s. The fastest of the free options and the
        // best instruction-follower of the three for negative constraints.
        Self {
            base_url: "https://api.groq.com/openai/v1".into(),
            model: "llama-3.3-70b-versatile".into(),
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
    /// Entity-vocabulary block spliced into the proposal prompt. Defaults to
    /// the embedded (0.2/0.3-era) datamined list; the service overrides it
    /// with one generated from the live vendor Gems.lua (960 gems @ 0.5) so
    /// the model can only name gems the applier's GemDb will accept —
    /// without the override, "Added Cold Damage"-style stale names get
    /// proposed and skipped (wasted variants).
    vocab_block: Option<String>,
}

impl OpenAiCompatSurrogate {
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
            vocab_block: None,
        }
    }

    pub fn with_vocab_block(mut self, block: String) -> Self {
        self.vocab_block = Some(block);
        self
    }

    /// POST to {base_url}/chat/completions and return the assistant message
    /// content. Non-2xx other than 429 surfaces as SurrogateError::BadStatus
    /// with the response body.
    ///
    /// 429 handling — Cerebras free tier is ~30 req/min PER MODEL. A burst of
    /// 2 calls/gen × ~2 gens within 5s saturates the bucket and triggers a
    /// 429 that doesn't clear until the next minute-aligned refill. We:
    ///   1. Parse the `Retry-After` header if present (seconds) and honor it.
    ///   2. Otherwise back off 10s → 30s → 60s → 60s (up to 4 retries, ~2.5
    ///      minutes max wait). One refill cycle is 60s on Cerebras, so 60s
    ///      is the right floor; doubling beyond doesn't help.
    /// Logs every retry so the user can see why a long gen is taking a while.
    async fn chat(&self, system: &str, user: &str) -> Result<String, SurrogateError> {
        let schedule_ms = [10_000u64, 30_000, 60_000, 60_000];
        let mut last_err: Option<(u16, String)> = None;
        for (attempt, &default_delay) in schedule_ms.iter().enumerate() {
            match self.chat_once(system, user).await {
                Ok(s) => {
                    if attempt > 0 {
                        tracing::info!(attempt, "surrogate recovered from 429");
                    }
                    return Ok(s);
                }
                Err(SurrogateError::BadStatus { status: 429, body }) => {
                    // Retry-After is captured inside chat_once and surfaced
                    // via a sentinel prefix in body so we don't have to
                    // restructure the error variant. Fall back to schedule.
                    let retry_after_ms = parse_retry_after_ms(&body).unwrap_or(default_delay);
                    last_err = Some((429, body));
                    if attempt + 1 == schedule_ms.len() {
                        break;
                    }
                    tracing::warn!(
                        attempt = attempt + 1,
                        wait_ms = retry_after_ms,
                        "surrogate rate-limited (429); backing off — Cerebras free tier RPM bucket refills every 60s"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(retry_after_ms)).await;
                    continue;
                }
                Err(other) => return Err(other),
            }
        }
        let (status, body) = last_err
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
        // Capture Retry-After before consuming the body. Cerebras returns it
        // as a delta-seconds value; some providers use HTTP-date. We only
        // care about the delta form for now.
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let text = resp.text().await?;
        if !status.is_success() {
            // Embed Retry-After at the head of the body using a sentinel that
            // the chat() retry loop knows to peel off. Keeps the public error
            // shape (SurrogateError::BadStatus { status, body }) unchanged.
            let body = match retry_after {
                Some(s) => format!("[retry-after:{s}]\n{text}"),
                None => text,
            };
            return Err(SurrogateError::BadStatus {
                status: status.as_u16(),
                body,
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
        let (skills_block, skills_offset) = match (skills_start, skills_end) {
            (Some(s), Some(e)) if e > s => (&xml[s..e + "</Skills>".len()], s),
            _ => ("", 0),
        };
        // Annotate the main scored group. The group index comes from
        // `mainSocketGroup` on `<Build>` — OUTSIDE the skills block — so the
        // range must be computed against the FULL xml, then rebased onto the
        // slice we're sending the model.
        let annotated_skills = match main_socket_group_range(xml) {
            Some((abs_start, _)) if abs_start >= skills_offset && !skills_block.is_empty() => {
                let rel = abs_start - skills_offset;
                let mut s = String::with_capacity(skills_block.len() + 96);
                s.push_str(&skills_block[..rel]);
                s.push_str("\n<!-- *** MAIN SCORED SKILL — mutations to gems IN THIS BLOCK change DPS *** -->\n");
                s.push_str(&skills_block[rel..]);
                s
            }
            _ => skills_block.to_string(),
        };
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
        // Vocab block: live Gems.lua-derived when the service provided one
        // (preferred — every name is applier-valid), else the embedded
        // datamined fallback. No I/O at request time either way.
        let vocab_block = self
            .vocab_block
            .clone()
            .unwrap_or_else(|| vocab::prompt_block(200, 200));
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
               {{\"op\":\"swap_gem\", \"old\":\"<exact nameSpec>\", \"new\":\"<POE2 gem name>\"}}  // REAL swap: engine rewrites gemId/skillId from PoB data; the new skill IS scored\n\
               {{\"op\":\"remove_gem\", \"gem\":\"<support nameSpec in the MAIN group>\"}}  // drop a support (PoE2 supports are binary — real score change)\n\
               {{\"op\":\"add_support_gem\", \"gem\":\"<POE2 support name>\"}}  // add a support to the MAIN group (engine synthesizes the element)\n\
               {{\"op\":\"set_active_weapon_set\", \"use_second\":true}}  // score the OTHER weapon loadout (PoE2 clear-vs-boss swap)\n\
             swap_gem / add_support_gem names are validated against PoB's own gem database —\n\
             an unknown name skips the op, so prefer names from the datamined list above.\n\
             Use set_active_weapon_set (optionally combined with a gem-level op) to probe the\n\
             build's second weapon-set loadout — the clear-vs-boss duality we ultimately ship.\n\
             It only changes the score when the seed has gear in weapon set 2.\n\n\
             ⚠️ POE2 SCORING RULES (read carefully — these are not PoE1):\n\
             - **Only the MAIN SCORED SKILL group moves DPS** — the group under the\n\
               `<!-- *** MAIN SCORED SKILL *** -->` marker in the XML above. Mutations to other\n\
               groups apply but don't change the score.\n\
             - **Support gem LEVEL changes are NO-OPS in PoE2** (supports are binary). To vary a\n\
               support's contribution use remove_gem / add_support_gem / swap_gem instead.\n\
             - **Quality changes barely move DPS** for most gems. Don't lead with quality.\n\n\
             To explore distinct builds, VARY THE AXIS per variant: some main-skill levels\n\
             (4/8/12/16/20), some support adds/removes/swaps in the main group, at most one\n\
             main-skill swap_gem to a thematically-adjacent skill. Variants with identical ops\n\
             produce identical scores → wasted.\n\n\
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

/// Chains multiple surrogate providers with rate-limit-aware failover.
///
/// Free-tier LLM endpoints saturate independently (Cerebras spent a whole
/// morning returning `queue_exceeded` platform-wide). One provider being
/// down shouldn't stall the cascade when two other free tiers are idle.
///
/// Per call: providers are tried in order; one that fails with a
/// rate/availability status (429 or any 5xx) is put on cooldown and the next
/// is tried immediately. Cooldown lasts [`FailoverSurrogate::COOLDOWN`] so a
/// saturated provider isn't hammered while its bucket refills, but rejoins
/// the rotation automatically. Non-availability errors (schema mismatch,
/// auth) also advance to the next provider — a provider that can't produce
/// usable output is no better than one that's down — but don't trigger
/// cooldown (the next call may be fine).
pub struct FailoverSurrogate {
    providers: Vec<FailoverEntry>,
}

struct FailoverEntry {
    name: String,
    inner: std::sync::Arc<dyn SurrogateProvider>,
    cooldown_until: std::sync::Mutex<Option<std::time::Instant>>,
}

impl FailoverSurrogate {
    pub const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(120);

    pub fn new(providers: Vec<(String, std::sync::Arc<dyn SurrogateProvider>)>) -> Self {
        Self {
            providers: providers
                .into_iter()
                .map(|(name, inner)| FailoverEntry {
                    name,
                    inner,
                    cooldown_until: std::sync::Mutex::new(None),
                })
                .collect(),
        }
    }

    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name.as_str()).collect()
    }

    fn is_cooling(entry: &FailoverEntry) -> bool {
        entry
            .cooldown_until
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map(|t| t > std::time::Instant::now())
            .unwrap_or(false)
    }

    fn start_cooldown(entry: &FailoverEntry) {
        *entry
            .cooldown_until
            .lock()
            .unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now() + Self::COOLDOWN);
    }

    fn is_availability_error(err: &SurrogateError) -> bool {
        matches!(
            err,
            SurrogateError::BadStatus { status, .. } if *status == 429 || *status >= 500
        ) || matches!(err, SurrogateError::Http(_))
    }

    /// Try each provider in order, skipping ones on cooldown. If every
    /// provider is cooling, ignore cooldowns and try them all anyway —
    /// a stale cooldown must never make the cascade fail when a provider
    /// has recovered early.
    async fn try_each<'a, T, F, Fut>(&'a self, mut call: F) -> Result<T, SurrogateError>
    where
        F: FnMut(&'a dyn SurrogateProvider) -> Fut,
        Fut: std::future::Future<Output = Result<T, SurrogateError>> + 'a,
    {
        let all_cooling = self.providers.iter().all(Self::is_cooling);
        let mut last_err: Option<SurrogateError> = None;
        for entry in &self.providers {
            if !all_cooling && Self::is_cooling(entry) {
                tracing::debug!(provider = %entry.name, "skipping provider on cooldown");
                continue;
            }
            match call(entry.inner.as_ref()).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if Self::is_availability_error(&e) {
                        tracing::warn!(
                            provider = %entry.name,
                            error = %e,
                            cooldown_s = Self::COOLDOWN.as_secs(),
                            "surrogate provider unavailable; cooling down and failing over"
                        );
                        Self::start_cooldown(entry);
                    } else {
                        tracing::warn!(
                            provider = %entry.name,
                            error = %e,
                            "surrogate provider errored; trying next provider"
                        );
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(SurrogateError::NotImplemented))
    }
}

#[async_trait]
impl SurrogateProvider for FailoverSurrogate {
    async fn propose_mutations(
        &self,
        seed_pob_xml: &str,
        seed_hypothesis: &str,
        count: usize,
    ) -> Result<Vec<MutationProposal>, SurrogateError> {
        self.try_each(|p| p.propose_mutations(seed_pob_xml, seed_hypothesis, count))
            .await
    }

    async fn cheap_score(
        &self,
        candidates: &[MutationProposal],
    ) -> Result<Vec<CandidateScore>, SurrogateError> {
        self.try_each(|p| p.cheap_score(candidates)).await
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
        // One hint per axis, each with a DISTINCT role/scaling pair. The
        // engine takes damage_type from gem-data truth and defense from
        // stats, so role+scaling are the only hint-controlled coords — if
        // two axes share a pair they collide into one cell and best-of-cell
        // silently drops the other (observed: 10 variants → 7 cells).
        let cells = [
            "physical/es/clear/gem-levels",      // 0: L4
            "physical/es/hybrid/gem-levels",     // 1: L8
            "physical/es/boss/tree-keystone",    // 2: L12
            "physical/es/clear/attribute-stack", // 3: L16
            "physical/es/boss/gem-levels",       // 4: L20
            "lightning/es/boss/unique-driven",   // 5: Spark swap
            "physical/es/hybrid/attribute-stack",// 6: remove support
            "chaos/es/boss/tree-keystone",       // 7: Contagion swap
            "chaos/es/clear/unique-driven",      // 8: Essence Drain swap
            "physical/es/boss/unique-driven",    // 9: add support
        ];
        // Find the main scored skill's gem name in the seed XML — the first
        // `<Gem nameSpec="...">` inside the `<Skill mainActiveSkill="1">`
        // block. That's the only gem whose level changes move PoB's DPS for
        // this build (POE2 supports are level-agnostic). If we can't find it,
        // fall back to "*" wildcard.
        let main_skill_gem = find_main_skill_gem_name(seed_pob_xml).unwrap_or_else(|| "*".to_string());
        Ok((0..count)
            .map(|i| {
                // Ten deterministic variant axes. Five main-skill level rungs
                // for the DPS ladder, then composition mutations that exercise
                // every real op AND populate multiple damage_type cells (the
                // chaos swaps exist because concepts ask for chaos and the
                // ground-truth labeler should have real chaos cells to label):
                //   5 → swap main → Spark        (lightning)
                //   6 → remove a support from the scored group
                //   7 → swap main → Contagion    (chaos AoE DoT)
                //   8 → swap main → Essence Drain (chaos projectile DoT)
                //   9 → add Controlled Destruction (spell support) to the group
                let levels = [4u32, 8, 12, 16, 20];
                let (ops, desc): (Vec<MutationOp>, String) = match i % 10 {
                    5 => (
                        vec![MutationOp::SwapGem {
                            old: main_skill_gem.clone(),
                            new: "Spark".to_string(),
                        }],
                        format!("mock swap — replace {main_skill_gem} with Spark (lightning)"),
                    ),
                    6 => match nth_gem_in_main_group(seed_pob_xml, 2) {
                        Some(support) => (
                            vec![MutationOp::RemoveGem {
                                gem: support.clone(),
                            }],
                            format!("mock remove — drop support {support} from the scored group"),
                        ),
                        None => (
                            vec![MutationOp::SetGemQuality {
                                gem: main_skill_gem.clone(),
                                quality: 20,
                            }],
                            "mock fallback — no support to remove; quality the main skill".to_string(),
                        ),
                    },
                    7 => (
                        vec![MutationOp::SwapGem {
                            old: main_skill_gem.clone(),
                            new: "Contagion".to_string(),
                        }],
                        format!("mock swap — replace {main_skill_gem} with Contagion (chaos AoE DoT)"),
                    ),
                    8 => (
                        vec![MutationOp::SwapGem {
                            old: main_skill_gem.clone(),
                            new: "Essence Drain".to_string(),
                        }],
                        format!("mock swap — replace {main_skill_gem} with Essence Drain (chaos DoT)"),
                    ),
                    9 => (
                        vec![MutationOp::AddSupportGem {
                            gem: "Controlled Destruction".to_string(),
                        }],
                        "mock add — socket Controlled Destruction into the scored group".to_string(),
                    ),
                    _ => {
                        let level = levels[i % levels.len()];
                        (
                            vec![MutationOp::SetGemLevel {
                                gem: main_skill_gem.clone(),
                                level,
                            }],
                            format!("mock mutation #{i} — set main skill {main_skill_gem} to level {level}"),
                        )
                    }
                };
                MutationProposal {
                    variant_id: format!("mock-{i}"),
                    pob_xml: seed_pob_xml.to_string(),
                    origin_hypothesis: Some(desc),
                    cell_focus: Some(cells[i % cells.len()].to_string()),
                    ops,
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

#[cfg(test)]
mod main_skill_tests {
    use super::find_main_skill_gem_name;

    /// Mirrors real 0.5 exports: every group carries mainActiveSkill="1";
    /// the scored group is picked by mainSocketGroup on <Build>.
    const DRUID_SHAPED: &str = r#"<PathOfBuilding2>
<Build level="92" className="Druid" ascendClassName="Oracle" mainSocketGroup="2" viewMode="IMPORT">
</Build>
<Skills activeSkillSet="1" defaultGemLevel="normalMaximum">
<SkillSet id="1">
<Skill mainActiveSkill="1" enabled="true">
<Gem nameSpec="Frost Bomb" level="20"/>
<Gem nameSpec="Spell Cascade" level="1"/>
</Skill>
<Skill mainActiveSkill="1" enabled="true">
<Gem nameSpec="Tornado" level="20"/>
<Gem nameSpec="Swift Affliction II" level="1"/>
</Skill>
</SkillSet>
</Skills>
</PathOfBuilding2>"#;

    #[test]
    fn scored_gem_comes_from_main_socket_group() {
        assert_eq!(
            find_main_skill_gem_name(DRUID_SHAPED).as_deref(),
            Some("Tornado"),
            "mainSocketGroup=2 must select group 2 (Tornado), not group 1 (Frost Bomb)"
        );
    }

    #[test]
    fn main_active_skill_indexes_gem_within_group() {
        let xml = DRUID_SHAPED.replace(
            r#"<Skill mainActiveSkill="1" enabled="true">
<Gem nameSpec="Tornado" level="20"/>"#,
            r#"<Skill mainActiveSkill="2" enabled="true">
<Gem nameSpec="Cast on Critical" level="20"/>
<Gem nameSpec="Tornado" level="20"/>"#,
        );
        assert_eq!(
            find_main_skill_gem_name(&xml).as_deref(),
            Some("Tornado"),
            "mainActiveSkill=2 must select the second gem in the group"
        );
    }
}

#[cfg(test)]
mod failover_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Scripted fake: fails its first `fail_n` calls with the given error
    /// factory, then succeeds. Counts total calls.
    struct Fake {
        fail_n: usize,
        calls: AtomicUsize,
        make_err: fn() -> SurrogateError,
        tag: &'static str,
    }

    #[async_trait]
    impl SurrogateProvider for Fake {
        async fn propose_mutations(
            &self,
            _x: &str,
            _h: &str,
            _c: usize,
        ) -> Result<Vec<MutationProposal>, SurrogateError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                return Err((self.make_err)());
            }
            Ok(vec![MutationProposal {
                variant_id: self.tag.to_string(),
                pob_xml: String::new(),
                origin_hypothesis: None,
                cell_focus: None,
                ops: vec![],
            }])
        }
        async fn cheap_score(
            &self,
            c: &[MutationProposal],
        ) -> Result<Vec<CandidateScore>, SurrogateError> {
            Ok(c.iter()
                .map(|p| CandidateScore {
                    variant_id: p.variant_id.clone(),
                    interest: 1.0,
                    plausibility: 1.0,
                    note: None,
                })
                .collect())
        }
    }

    fn rate_limited() -> SurrogateError {
        SurrogateError::BadStatus { status: 429, body: "queue_exceeded".into() }
    }
    fn schema_err() -> SurrogateError {
        SurrogateError::Schema("no JSON".into())
    }

    fn fake(tag: &'static str, fail_n: usize, make_err: fn() -> SurrogateError) -> Arc<Fake> {
        Arc::new(Fake { fail_n, calls: AtomicUsize::new(0), make_err, tag })
    }

    #[tokio::test]
    async fn rate_limited_provider_fails_over_and_cools_down() {
        let a = fake("a", 99, rate_limited); // always 429
        let b = fake("b", 0, rate_limited);  // always succeeds
        let f = FailoverSurrogate::new(vec![
            ("a".into(), a.clone() as Arc<dyn SurrogateProvider>),
            ("b".into(), b.clone() as Arc<dyn SurrogateProvider>),
        ]);

        let r = f.propose_mutations("x", "h", 1).await.unwrap();
        assert_eq!(r[0].variant_id, "b", "second provider served the call");
        assert_eq!(a.calls.load(Ordering::SeqCst), 1);

        // Second call: a is on cooldown and must be SKIPPED (no second hit).
        let r = f.propose_mutations("x", "h", 1).await.unwrap();
        assert_eq!(r[0].variant_id, "b");
        assert_eq!(a.calls.load(Ordering::SeqCst), 1, "cooling provider was not re-hit");
    }

    #[tokio::test]
    async fn schema_error_advances_without_cooldown() {
        let a = fake("a", 1, schema_err); // fails once (schema), then fine
        let b = fake("b", 0, rate_limited);
        let f = FailoverSurrogate::new(vec![
            ("a".into(), a.clone() as Arc<dyn SurrogateProvider>),
            ("b".into(), b.clone() as Arc<dyn SurrogateProvider>),
        ]);

        let r = f.propose_mutations("x", "h", 1).await.unwrap();
        assert_eq!(r[0].variant_id, "b", "schema failure advanced to next provider");

        // No cooldown for schema errors: a is tried again and now succeeds.
        let r = f.propose_mutations("x", "h", 1).await.unwrap();
        assert_eq!(r[0].variant_id, "a", "provider rejoined immediately after non-availability error");
    }

    #[tokio::test]
    async fn all_cooling_still_tries_everyone() {
        let a = fake("a", 1, rate_limited); // 429 once, then succeeds
        let f = FailoverSurrogate::new(vec![
            ("a".into(), a.clone() as Arc<dyn SurrogateProvider>),
        ]);

        assert!(f.propose_mutations("x", "h", 1).await.is_err(), "first call fails (only provider 429s)");
        // a is now cooling — but it's ALL the providers, so the next call must
        // ignore cooldowns rather than failing without trying.
        let r = f.propose_mutations("x", "h", 1).await.unwrap();
        assert_eq!(r[0].variant_id, "a");
    }
}
