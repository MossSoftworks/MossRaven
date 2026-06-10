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

/// A curated finalist — Tier 5 output. Takes one ArchiveEntry's worth of stats
/// and wraps it in the prose + tags the UI needs to render the build as a
/// "this is why you'd play this" card. The `pob_import_code` is the
/// URL-safe-base64'd, zlib-compressed XML the user pastes into PoB2 to
/// open the build for real.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finalist {
    pub variant_id: String,
    pub title: String,
    pub one_liner: String,
    pub why_it_works: String,
    /// 2–5 short tags that describe the build's identity, e.g.
    /// "cold DoT", "ES stacker", "boss-killer", "low-budget".
    pub tags: Vec<String>,
    /// The cell coords as a slash-string for grouping the UI.
    pub cell: String,
    /// Headline numbers cherry-picked from BuildStats — what to show in the
    /// card without making the user click through.
    pub key_stats: Vec<KeyStat>,
    /// `~base64(zlib(pob_xml))` — paste into PoB2 Import.
    pub pob_import_code: String,
    /// SPEC §1.1 — leveling + endgame + dual-loadout guide. Optional in the
    /// schema (old payloads parse) but REQUIRED for end-state finalists; the
    /// synthesize prompts demand it.
    #[serde(default)]
    pub guide: Option<BuildGuide>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStat {
    pub label: String,
    pub value: String,
}

/// SPEC §1.1 build guide — what makes a finalist *playable*, not just scored.
/// All fields are prose written by Tier 5. Serde-defaulted so pre-guide
/// finalist JSON (and conservative models) still parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildGuide {
    /// Leveling path: act milestones, which skills to run early, gem/passive
    /// order, and what to respec out of (and when) on the way to the final build.
    #[serde(default)]
    pub leveling: String,
    /// Endgame plan: final tree direction, gear priorities, key breakpoints
    /// (resist caps, attribute gates, spirit budget).
    #[serde(default)]
    pub endgame: String,
    /// Clear-vs-boss dual-loadout design. PoE2 weapon-set swap is the preferred
    /// mechanism (`useSecondWeaponSet` + weapon-set passive points) — minimal
    /// switching friction. If the build can't dual-loadout cleanly, this MUST
    /// say so explicitly (SPEC §1.1 requirement).
    #[serde(default)]
    pub loadout_swap: String,
    /// Honest caveats — PoB models damage/defense, not feel (clunk, animation
    /// lock, on-death effects). Never claim the build is fun; flag what needs
    /// playtesting.
    #[serde(default)]
    pub playtest_notes: Option<String>,
}

#[async_trait]
pub trait TierOneDriver: Send + Sync {
    async fn seed(&self, prompt: &str) -> Result<Hypothesis, DreamerError>;
    async fn curate(
        &self,
        archive_snapshot: &Value,
    ) -> Result<Hypothesis, DreamerError>;
    /// Tier 5 — consume an archive frontier (already pruned to the best ~20
    /// entries by Tier 4) and produce 5–10 curated, narrated finalists for the
    /// UI's "play these" panel. Default impl returns NotImplemented so existing
    /// drivers (ExternalMcpDriver, future test fakes) don't have to provide it.
    async fn synthesize_finalists(
        &self,
        _frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        Err(DreamerError::NotImplemented)
    }
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

/// Tier-1/Tier-5 prompt builders — shared verbatim by every API-backed driver
/// (Anthropic, OpenAI-compat) so swapping the LLM vendor never changes what
/// we ask for.
mod prompts {
    use serde_json::Value;

    pub fn seed(prompt: &str) -> (&'static str, String) {
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
        (system, user)
    }

    pub fn curate(archive_snapshot: &Value) -> (&'static str, String) {
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
        (system, user)
    }

    pub fn synthesize(frontier_snapshot: &Value) -> (&'static str, String) {
        let system = "You are a Path of Exile 2 build CURATOR. The search engine has produced \
                      a frontier of mechanically-scored builds. Your job is to pick the 5–10 \
                      most COMPELLING ones and explain — to a player who hasn't read the data — \
                      WHY each is worth playing, and HOW to actually play it. \
                      \n\nOutput ONLY valid JSON. No prose outside the JSON. No markdown fences. \
                      Be ruthless about distinct identities — don't return two finalists that \
                      play the same. Prefer variety across damage type, defense layer, role. \
                      Borrow the `pob_import_code`, `variant_id`, and `cell` values from the \
                      frontier entry you're describing — DO NOT invent new ones. \
                      \n\nEvery finalist MUST include a `guide` object (SPEC §1.1): \
                      `leveling` (act milestones, early skills, gem/passive order, respec points), \
                      `endgame` (final tree direction, gear priorities, breakpoints), and \
                      `loadout_swap` (clear-vs-boss duality via PoE2 weapon-set swap — which gems/sets \
                      go in weapon set 1 vs 2; if the build can't dual-loadout cleanly, SAY SO explicitly). \
                      In `playtest_notes`, flag what PoB can't model (clunk, animation lock, on-death) — \
                      NEVER claim the build is fun; it is theoretically viable until played.";
        let user = format!(
            "Frontier (Tier 4 pruned, ready for curation):\n{}\n\n\
             Return JSON of shape:\n\
             {{\n  \"finalists\": [\n    {{\n      \"variant_id\": \"<copy from frontier>\",\n      \"title\": \"a short evocative name, like 'Cold DoT Tank Witch'\",\n      \"one_liner\": \"one sentence — what's the build do?\",\n      \"why_it_works\": \"2–4 sentences. Mechanical reasoning. Why this combo of skill/support/defense layer is good.\",\n      \"tags\": [\"cold\", \"DoT\", \"ES-stack\", \"boss-killer\"],\n      \"cell\": \"<copy from frontier>\",\n      \"key_stats\": [\n        {{\"label\": \"DPS\", \"value\": \"4.2M\"}},\n        {{\"label\": \"EHP\", \"value\": \"24k\"}},\n        {{\"label\": \"Resist\", \"value\": \"75/75/75\"}}\n      ],\n      \"pob_import_code\": \"<copy from frontier>\",\n      \"guide\": {{\n        \"leveling\": \"act-by-act milestones: which skills carry acts 1–3, gem order, passive priorities, when to respec into the final form\",\n        \"endgame\": \"final tree direction, gear priorities by slot, breakpoints to hit (resists, attributes, spirit)\",\n        \"loadout_swap\": \"clear vs boss: what lives in weapon set 1 vs weapon set 2, which passives to bind per set — or an explicit statement that this build can't dual-loadout cleanly and why\",\n        \"playtest_notes\": \"what PoB can't verify here — feel, clunk, on-death effects\"\n      }}\n    }}\n  ]\n}}",
            serde_json::to_string_pretty(frontier_snapshot).unwrap_or_default(),
        );
        (system, user)
    }
}

#[async_trait]
impl TierOneDriver for AnthropicApiDriver {
    async fn seed(&self, prompt: &str) -> Result<Hypothesis, DreamerError> {
        let (system, user) = prompts::seed(prompt);
        let raw = self.message(system, &user).await?;
        Self::parse_hypothesis(&raw)
    }

    async fn curate(
        &self,
        archive_snapshot: &Value,
    ) -> Result<Hypothesis, DreamerError> {
        let (system, user) = prompts::curate(archive_snapshot);
        let raw = self.message(system, &user).await?;
        Self::parse_hypothesis(&raw)
    }

    /// Tier 5: turn a frontier of scored entries into curated finalists.
    ///
    /// The frontier is a JSON array (whatever `get_frontier` produces) where
    /// each entry has at least: `variant_id`, `cell`, `score`, `dps`, `ehp`,
    /// `pob_import_code`, and the build's `origin_hypothesis` / `pob_xml`.
    /// The driver returns 5–10 finalists with prose + headline stats; the UI
    /// is already wired to render `Finalist`-shaped objects.
    async fn synthesize_finalists(
        &self,
        frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        // Crank max_tokens for this single call — finalists need real prose
        // (guides are paragraphs, not one-liners), so give generous headroom.
        let me = self.clone_with_max_tokens(8192);
        let (system, user) = prompts::synthesize(frontier_snapshot);
        let raw = me.message(system, &user).await?;
        parse_finalists(&raw)
    }
}

impl AnthropicApiDriver {
    /// Internal: build a copy with a different max_tokens. We can't `clone`
    /// reqwest::Client cheaply enough to justify a generic Clone impl, so this
    /// is the explicit one-call escape hatch the synthesize step uses.
    fn clone_with_max_tokens(&self, max_tokens: u32) -> Self {
        Self {
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            max_tokens,
            http: self.http.clone(),
        }
    }

}

/// Shared response parser: pull `{"finalists": [...]}` out of a raw model
/// reply, tolerating markdown fences and prose preambles.
fn parse_finalists(raw: &str) -> Result<Vec<Finalist>, DreamerError> {
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
            let start = inner.find('{').ok_or_else(|| {
                DreamerError::Schema(format!("no JSON object in response: {raw}"))
            })?;
            serde_json::from_str(&inner[start..]).map_err(|e| {
                DreamerError::Schema(format!("JSON parse failed: {e} — {raw}"))
            })?
        }
    };
    let arr = v
        .get("finalists")
        .and_then(|f| f.as_array())
        .ok_or_else(|| DreamerError::Schema(format!("no `finalists` array: {v}")))?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let f: Finalist = serde_json::from_value(entry.clone())
            .map_err(|e| DreamerError::Schema(format!("finalist parse failed: {e}")))?;
        out.push(f);
    }
    Ok(out)
}

/// Tier-1/Tier-5 driver for any OpenAI-compatible chat endpoint (Google AI
/// Studio's Gemini compat layer, Groq, local Ollama, OpenRouter...). Same
/// prompts as the Anthropic driver — only the wire format differs. This is
/// what makes fully-solo Mode A possible on free tiers: no Anthropic key
/// required for hypothesis seeding or finalist synthesis.
pub struct OpenAiCompatDriver {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_tokens: u32,
    http: reqwest::Client,
}

impl OpenAiCompatDriver {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            max_tokens: 8192,
            http: reqwest::Client::new(),
        }
    }

    /// Gemini via Google AI Studio's OpenAI-compat endpoint. Free tier (no
    /// card). Default model is full flash (not -lite): Tier-5 guides are
    /// long structured prose and the quality step up matters there.
    pub fn gemini_default(api_key: String) -> Self {
        Self::new(
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "gemini-2.5-flash",
            Some(api_key),
        )
    }

    /// Groq llama-3.3-70b. Free tier (no card).
    pub fn groq_default(api_key: String) -> Self {
        Self::new("https://api.groq.com/openai/v1", "llama-3.3-70b-versatile", Some(api_key))
    }

    async fn message(&self, system: &str, user: &str) -> Result<String, DreamerError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user",   "content": user },
            ],
            "temperature": 0.4,
            "max_tokens": self.max_tokens,
            "stream": false,
        });
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(DreamerError::BadStatus {
                status: status.as_u16(),
                body: text,
            });
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| DreamerError::Schema(format!("body not JSON: {e} — {text}")))?;
        let msg = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .ok_or_else(|| DreamerError::Schema(format!("no choices[0].message in {text}")))?;
        // Reasoning models park output in `reasoning` when content is empty
        // (same quirk the Tier-2 surrogate handles).
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.trim().is_empty());
        let reasoning = msg
            .get("reasoning")
            .and_then(|c| c.as_str())
            .filter(|s| !s.trim().is_empty());
        content
            .or(reasoning)
            .map(str::to_string)
            .ok_or_else(|| DreamerError::Schema(format!("no usable message content in {text}")))
    }
}

#[async_trait]
impl TierOneDriver for OpenAiCompatDriver {
    async fn seed(&self, prompt: &str) -> Result<Hypothesis, DreamerError> {
        let (system, user) = prompts::seed(prompt);
        let raw = self.message(system, &user).await?;
        AnthropicApiDriver::parse_hypothesis(&raw)
    }

    async fn curate(&self, archive_snapshot: &Value) -> Result<Hypothesis, DreamerError> {
        let (system, user) = prompts::curate(archive_snapshot);
        let raw = self.message(system, &user).await?;
        AnthropicApiDriver::parse_hypothesis(&raw)
    }

    async fn synthesize_finalists(
        &self,
        frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        let (system, user) = prompts::synthesize(frontier_snapshot);
        let raw = self.message(system, &user).await?;
        parse_finalists(&raw)
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
    /// Without this override the trait default returns `NotImplemented`, which
    /// the service maps to a hard ToolFailed error — and the Mode B handoff
    /// branch (hand the frontier to the external Claude with instructions)
    /// becomes unreachable. DriverIsExternal is the signal that branch keys on
    /// (see ServiceControlSurface::synthesize_finalists in mossraven-service).
    async fn synthesize_finalists(
        &self,
        _frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        Err(DreamerError::DriverIsExternal)
    }
}
