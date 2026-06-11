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
    #[error("tier-1 endpoint returned non-OK: {status} — {body}")]
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
    /// Serde-defaulted: prose-adjacent fields are whatever the model felt
    /// like emitting that day — never let one omission void a synthesis.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The cell coords as a slash-string for grouping the UI. Defaulted —
    /// observed omitted live (gemini-2.5-flash-lite); the engine backfills
    /// it from the frontier by `variant_id`, same as `pob_import_code`.
    #[serde(default)]
    pub cell: String,
    /// Headline numbers cherry-picked from BuildStats — what to show in the
    /// card without making the user click through. Defaulted; engine
    /// backfills DPS/EHP/ES from frontier stats when empty.
    #[serde(default)]
    pub key_stats: Vec<KeyStat>,
    /// `~base64(zlib(pob_xml))` — paste into PoB2 Import. Tier-5 models DO
    /// NOT echo this (ten codes ≈ 90K input + 40K output tokens — truncation
    /// guaranteed); the engine re-attaches it by `variant_id` after parsing.
    #[serde(default)]
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

/// One leveling checkpoint (SPEC §1.1 v2 — 5 per finalist).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckpointGuide {
    /// e.g. "CP1 — Acts 1–2"
    #[serde(default)]
    pub name: String,
    /// e.g. "1–25"
    #[serde(default)]
    pub levels: String,
    /// Which skills/supports to run at this point.
    #[serde(default)]
    pub gems: String,
    /// Passive priorities at this point.
    #[serde(default)]
    pub passives: String,
    /// What gear to look for at this point.
    #[serde(default)]
    pub gear: String,
}

/// SPEC §1.1 build guide — what makes a finalist *playable*, not just scored.
/// All fields are prose written by Tier 7. Serde-defaulted so pre-guide
/// finalist JSON (and conservative models) still parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildGuide {
    /// Leveling path SUMMARY (the 5 checkpoints carry the detail).
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
    /// SPEC §1.1 v2: exactly 5 leveling checkpoints (CP1 Acts 1–2 … CP5
    /// pinnacle-ready).
    #[serde(default)]
    pub checkpoints: Vec<CheckpointGuide>,
    /// Bossing guide: single-target loadout, defensive swaps, what kills this
    /// build and what to do about it.
    #[serde(default)]
    pub bossing: String,
    /// Clearing/mapping guide: clear loadout, pack handling, map mods this
    /// build cannot run.
    #[serde(default)]
    pub mapping: String,
    /// SPEC §1.1.2 value notes: cost reality, cheapest acceptable variant,
    /// what the expensive pieces buy you.
    #[serde(default)]
    pub cost_notes: String,
}

/// Tier-6 selection-pool candidate (SPEC §1.1.3): a nomination, not a guide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolCandidate {
    pub variant_id: String,
    pub title: String,
    /// One-sentence pitch.
    pub pitch: String,
    /// Why this deserves a curation slot (2–3 sentences max).
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub cell: String,
    #[serde(default)]
    pub cost_band: String,
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
    /// Legacy single-stage path; the v2 pipeline is select_pool → write_finalists.
    async fn synthesize_finalists(
        &self,
        _frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        Err(DreamerError::NotImplemented)
    }
    /// Tier 6 (SPEC §1.1.3) — SELECT a pool of 15–20 candidates from the
    /// frontier. Breadth, not prose: one-line pitches only.
    async fn select_pool(
        &self,
        _frontier_snapshot: &Value,
    ) -> Result<Vec<PoolCandidate>, DreamerError> {
        Err(DreamerError::NotImplemented)
    }
    /// Tier 7 (SPEC §1.1.3) — CURATE exactly 5 from the pool and WRITE the
    /// full guide set per pick (5 checkpoints, bossing, mapping, cost notes).
    async fn write_finalists(
        &self,
        _pool: &[PoolCandidate],
        _frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        Err(DreamerError::NotImplemented)
    }
    /// §3.6 adversarial critic — review a stage's draft against ground truth.
    /// Returns (ok, issues). Default: always-ok (drivers without a critic
    /// never block the pipeline).
    async fn review(
        &self,
        _stage: &str,
        _draft: &Value,
        _ground_truth: &Value,
    ) -> Result<(bool, Vec<String>), DreamerError> {
        Ok((true, Vec::new()))
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
                      Borrow the `variant_id` and `cell` values from the frontier entry \
                      you're describing — DO NOT invent new ones. NEVER echo the \
                      `pob_import_code` field: the engine re-attaches it by variant_id, \
                      and echoing ten codes truncates your reply mid-JSON. \
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
             {{\n  \"finalists\": [\n    {{\n      \"variant_id\": \"<copy from frontier>\",\n      \"title\": \"a short evocative name, like 'Cold DoT Tank Witch'\",\n      \"one_liner\": \"one sentence — what's the build do?\",\n      \"why_it_works\": \"2–4 sentences. Mechanical reasoning. Why this combo of skill/support/defense layer is good.\",\n      \"tags\": [\"cold\", \"DoT\", \"ES-stack\", \"boss-killer\"],\n      \"cell\": \"<copy from frontier>\",\n      \"key_stats\": [\n        {{\"label\": \"DPS\", \"value\": \"4.2M\"}},\n        {{\"label\": \"EHP\", \"value\": \"24k\"}},\n        {{\"label\": \"Resist\", \"value\": \"75/75/75\"}}\n      ],\n      \"guide\": {{\n        \"leveling\": \"act-by-act milestones: which skills carry acts 1–3, gem order, passive priorities, when to respec into the final form\",\n        \"endgame\": \"final tree direction, gear priorities by slot, breakpoints to hit (resists, attributes, spirit)\",\n        \"loadout_swap\": \"clear vs boss: what lives in weapon set 1 vs weapon set 2, which passives to bind per set — or an explicit statement that this build can't dual-loadout cleanly and why\",\n        \"playtest_notes\": \"what PoB can't verify here — feel, clunk, on-death effects\"\n      }}\n    }}\n  ]\n}}",
            serde_json::to_string_pretty(frontier_snapshot).unwrap_or_default(),
        );
        (system, user)
    }

    /// Tier 6 v2 — SELECT a pool, don't write guides.
    pub fn select_pool(frontier_snapshot: &Value) -> (&'static str, String) {
        let system = "You are a Path of Exile 2 build SELECTOR (Tier 6 of a discovery \
                      pipeline). The engine produced a frontier of mechanically-scored \
                      builds. NOMINATE a pool of 15–20 candidates worth a curator's \
                      attention. You are NOT writing guides — one-line pitches only. \
                      \n\nOutput ONLY valid JSON. No prose, no markdown fences. \
                      Rules: variant_id and cell are COPIED from frontier entries, never \
                      invented. Span COST BANDS (budget picks matter as much as ceiling \
                      picks), playstyles, and damage types where the frontier allows. \
                      If the frontier has fewer distinct builds than 15, nominate every \
                      genuinely distinct one ONCE — do not pad with duplicates. NEVER \
                      echo pob_import_code.";
        let user = format!(
            "Frontier:\n{}\n\n\
             Return JSON of shape:\n\
             {{\n  \"pool\": [\n    {{\n      \"variant_id\": \"<copy>\",\n      \"title\": \"short name\",\n      \"pitch\": \"one sentence — why a player would care\",\n      \"rationale\": \"2–3 sentences max — why this earns a curation slot (power, value, novelty)\",\n      \"cell\": \"<copy>\",\n      \"cost_band\": \"<copy estimated cost band from the entry>\"\n    }}\n  ]\n}}",
            serde_json::to_string_pretty(frontier_snapshot).unwrap_or_default(),
        );
        (system, user)
    }

    /// Tier 7 v2 — CURATE 5 from the pool and WRITE the full SPEC §1.1 guides.
    pub fn write_finalists(pool: &[super::PoolCandidate], frontier_snapshot: &Value) -> (&'static str, String) {
        let system = "You are a Path of Exile 2 build CURATOR-AUTHOR (Tier 7). From the \
                      selection pool, pick EXACTLY 5 builds and write their complete \
                      guides. \
                      \n\nCuration criteria, in order: (1) all-content viability honesty — \
                      quote the viability verdict, never oversell a FAIL; (2) VALUE — \
                      effectiveness per divine. Giving up 1M DPS on a 10M-DPS build to \
                      save 90% of the cost is a WIN; say so when it applies, and prefer a \
                      cost SPREAD across the five (at least one budget pick when the pool \
                      has one); (3) playstyle + damage-type diversity — no two finalists \
                      that play the same. \
                      \n\nOutput ONLY valid JSON. No markdown fences. variant_id/cell are \
                      COPIED from the pool/frontier. NEVER echo pob_import_code. \
                      \n\nEvery finalist carries a guide with: `checkpoints` — EXACTLY 5 \
                      leveling waypoints (CP1 Acts 1–2 lvl 1–25, CP2 Act 3 + Cruel entry \
                      25–45, CP3 Cruel done / maps entry 45–65, CP4 early maps + \
                      ascendancy 65–85, CP5 pinnacle-ready 85+), each naming the gems to \
                      run, passives to prioritize, and gear to look for AT THAT POINT; \
                      `bossing` — single-target loadout, defensive swaps, what kills this \
                      build and the counterplay; `mapping` — clear loadout, pack handling, \
                      map mods this build cannot run; `cost_notes` — cost reality, the \
                      cheapest acceptable variant, what the expensive pieces buy; plus the \
                      existing `leveling` (summary), `endgame`, `loadout_swap` (weapon-set \
                      duality or an explicit can't-dual-loadout statement), and \
                      `playtest_notes` (what PoB can't model — never claim it's fun).";
        let pool_json = serde_json::to_string_pretty(pool).unwrap_or_default();
        let user = format!(
            "Selection pool (Tier 6 output):\n{pool_json}\n\n\
             Frontier ground truth (stats / viability / cost per variant_id):\n{}\n\n\
             Return JSON of shape:\n\
             {{\n  \"finalists\": [\n    {{\n      \"variant_id\": \"<from pool>\",\n      \"title\": \"...\",\n      \"one_liner\": \"...\",\n      \"why_it_works\": \"...\",\n      \"tags\": [\"...\"],\n      \"cell\": \"<copy>\",\n      \"key_stats\": [{{\"label\": \"DPS\", \"value\": \"...\"}}, {{\"label\": \"EHP\", \"value\": \"...\"}}, {{\"label\": \"Cost\", \"value\": \"<band>\"}}],\n      \"guide\": {{\n        \"leveling\": \"summary\",\n        \"endgame\": \"...\",\n        \"loadout_swap\": \"...\",\n        \"playtest_notes\": \"...\",\n        \"checkpoints\": [\n          {{\"name\": \"CP1 — Acts 1–2\", \"levels\": \"1–25\", \"gems\": \"...\", \"passives\": \"...\", \"gear\": \"...\"}},\n          {{\"name\": \"CP2 — Act 3 + Cruel entry\", \"levels\": \"25–45\", \"gems\": \"...\", \"passives\": \"...\", \"gear\": \"...\"}},\n          {{\"name\": \"CP3 — Maps entry\", \"levels\": \"45–65\", \"gems\": \"...\", \"passives\": \"...\", \"gear\": \"...\"}},\n          {{\"name\": \"CP4 — Early maps + ascendancy\", \"levels\": \"65–85\", \"gems\": \"...\", \"passives\": \"...\", \"gear\": \"...\"}},\n          {{\"name\": \"CP5 — Pinnacle-ready\", \"levels\": \"85+\", \"gems\": \"...\", \"passives\": \"...\", \"gear\": \"...\"}}\n        ],\n        \"bossing\": \"...\",\n        \"mapping\": \"...\",\n        \"cost_notes\": \"...\"\n      }}\n    }}\n  ]\n}}",
            serde_json::to_string_pretty(frontier_snapshot).unwrap_or_default(),
        );
        (system, user)
    }

    /// §3.6 adversarial critic — stage-generic refutation prompt.
    pub fn critique(stage: &str, draft: &Value, ground_truth: &Value) -> (String, String) {
        let system = format!(
            "You are an adversarial REVIEWER for stage '{stage}' of a PoE2 build pipeline. \
             Your job is to REFUTE: find CONCRETE, CHECKABLE errors in the draft against \
             the ground truth — wrong variant_ids, stats/cost/viability claims that \
             contradict the data, missing required fields, duplicate picks, named \
             skills/items that don't appear anywhere in the ground truth. \
             Do NOT raise style opinions or hypothetical concerns. \
             If you find no concrete error, say ok=true. \
             Output ONLY JSON: {{\"ok\": true|false, \"issues\": [\"specific error 1\", ...]}}"
        );
        let user = format!(
            "GROUND TRUTH:\n{}\n\nDRAFT ({stage}):\n{}\n\nReturn the verdict JSON.",
            serde_json::to_string_pretty(ground_truth).unwrap_or_default(),
            serde_json::to_string_pretty(draft).unwrap_or_default(),
        );
        (system, user)
    }

    /// One revision pass: original task + draft + the critic's issues.
    pub fn revise(original_system: &str, original_user: &str, draft: &Value, issues: &[String]) -> (String, String) {
        let system = format!(
            "{original_system}\n\nThis is a REVISION pass: your previous draft was \
             reviewed and concrete issues were found. Fix EXACTLY the listed issues, \
             change nothing else, and return the SAME JSON shape."
        );
        let user = format!(
            "{original_user}\n\nYOUR PREVIOUS DRAFT:\n{}\n\nISSUES TO FIX:\n{}",
            serde_json::to_string_pretty(draft).unwrap_or_default(),
            issues
                .iter()
                .map(|i| format!("- {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
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

    async fn select_pool(
        &self,
        frontier_snapshot: &Value,
    ) -> Result<Vec<PoolCandidate>, DreamerError> {
        let me = self.clone_with_max_tokens(8192);
        let (system, user) = prompts::select_pool(frontier_snapshot);
        let raw = me.message(system, &user).await?;
        parse_pool(&raw)
    }

    async fn write_finalists(
        &self,
        pool: &[PoolCandidate],
        frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        let me = self.clone_with_max_tokens(16_384);
        let (system, user) = prompts::write_finalists(pool, frontier_snapshot);
        let raw = me.message(system, &user).await?;
        parse_finalists(&raw)
    }

    async fn review(
        &self,
        stage: &str,
        draft: &Value,
        ground_truth: &Value,
    ) -> Result<(bool, Vec<String>), DreamerError> {
        let me = self.clone_with_max_tokens(2_048);
        let (system, user) = prompts::critique(stage, draft, ground_truth);
        let raw = me.message(&system, &user).await?;
        Ok(parse_verdict(&raw))
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

/// Strip markdown fences / prose preamble and parse the first JSON object.
fn parse_json_object(raw: &str) -> Result<Value, DreamerError> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start_matches('\n'))
        .and_then(|s| s.rsplit_once("```").map(|(a, _)| a))
        .unwrap_or(trimmed);
    match serde_json::from_str(inner) {
        Ok(v) => Ok(v),
        Err(_) => {
            let start = inner.find('{').ok_or_else(|| {
                DreamerError::Schema(format!("no JSON object in response: {raw}"))
            })?;
            serde_json::from_str(&inner[start..])
                .map_err(|e| DreamerError::Schema(format!("JSON parse failed: {e} — {raw}")))
        }
    }
}

/// Shared response parser: pull `{"finalists": [...]}` out of a raw model
/// reply, tolerating markdown fences and prose preambles.
fn parse_finalists(raw: &str) -> Result<Vec<Finalist>, DreamerError> {
    let v = parse_json_object(raw)?;
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

/// Pull `{"pool": [...]}` out of a Tier-5 selection reply.
fn parse_pool(raw: &str) -> Result<Vec<PoolCandidate>, DreamerError> {
    let v = parse_json_object(raw)?;
    let arr = v
        .get("pool")
        .and_then(|f| f.as_array())
        .ok_or_else(|| DreamerError::Schema(format!("no `pool` array: {v}")))?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let c: PoolCandidate = serde_json::from_value(entry.clone())
            .map_err(|e| DreamerError::Schema(format!("pool candidate parse failed: {e}")))?;
        out.push(c);
    }
    Ok(out)
}

/// Pull `{"ok": bool, "issues": [...]}` out of a critic reply. An unparsable
/// critic verdict reads as OK — the adversary must never break the pipeline.
fn parse_verdict(raw: &str) -> (bool, Vec<String>) {
    match parse_json_object(raw) {
        Ok(v) => {
            let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(true);
            let issues = v
                .get("issues")
                .and_then(|i| i.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (ok, issues)
        }
        Err(_) => (true, Vec::new()),
    }
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
    /// card). Default is flash-LITE: full flash 503-gated twice in one
    /// afternoon ("high demand") while lite answered every call — for an
    /// unattended pipeline, reliability beats the quality step. Override
    /// with MOSSRAVEN_T1_MODEL=gemini-2.5-flash when it's healthy.
    pub fn gemini_default(api_key: String) -> Self {
        Self::new(
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "gemini-2.5-flash-lite",
            Some(api_key),
        )
    }

    /// Groq llama-3.3-70b. Free tier (no card).
    pub fn groq_default(api_key: String) -> Self {
        Self::new("https://api.groq.com/openai/v1", "llama-3.3-70b-versatile", Some(api_key))
    }

    async fn message(&self, system: &str, user: &str) -> Result<String, DreamerError> {
        self.message_with_max(system, user, self.max_tokens).await
    }

    async fn message_with_max(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<String, DreamerError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user",   "content": user },
            ],
            "temperature": 0.4,
            "max_tokens": max_tokens,
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
        // Ten finalists × four-section guides ≈ 12–14K output tokens — the
        // default 8K budget truncated Gemini mid-JSON. flash supports 65K
        // out, llama-3.3 32K; 24K covers the worst case with headroom.
        let raw = self.message_with_max(system, &user, 24_576).await?;
        parse_finalists(&raw)
    }

    async fn select_pool(
        &self,
        frontier_snapshot: &Value,
    ) -> Result<Vec<PoolCandidate>, DreamerError> {
        let (system, user) = prompts::select_pool(frontier_snapshot);
        let raw = self.message_with_max(system, &user, 8_192).await?;
        parse_pool(&raw)
    }

    async fn write_finalists(
        &self,
        pool: &[PoolCandidate],
        frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        let (system, user) = prompts::write_finalists(pool, frontier_snapshot);
        // 5 finalists × (5 checkpoints + bossing + mapping + cost) ≈ 8–12K
        // output tokens; 24K gives headroom.
        let raw = self.message_with_max(system, &user, 24_576).await?;
        parse_finalists(&raw)
    }

    async fn review(
        &self,
        stage: &str,
        draft: &Value,
        ground_truth: &Value,
    ) -> Result<(bool, Vec<String>), DreamerError> {
        let (system, user) = prompts::critique(stage, draft, ground_truth);
        let raw = self.message_with_max(&system, &user, 2_048).await?;
        Ok(parse_verdict(&raw))
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
    async fn select_pool(
        &self,
        _frontier_snapshot: &Value,
    ) -> Result<Vec<PoolCandidate>, DreamerError> {
        Err(DreamerError::DriverIsExternal)
    }
    async fn write_finalists(
        &self,
        _pool: &[PoolCandidate],
        _frontier_snapshot: &Value,
    ) -> Result<Vec<Finalist>, DreamerError> {
        Err(DreamerError::DriverIsExternal)
    }
}
