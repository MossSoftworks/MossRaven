//! Orchestration core — the search loop and Tier-3 dispatch.
//!
//! Owns the cascade evaluator:
//!
//! ```text
//! seed concept (Tier 1)
//!   → mutate variant space        (Tier 2 surrogate proposes)
//!   → Tier 2 surrogate: cheap-filter for plausible + novel   (prune)
//!   → Tier 3 pob-headless: hard numbers on survivors only    (expensive)
//!   → place in MAP-Elites cell IF it beats that niche's elite
//!   → Tier 1 reads filled + empty cells → new hypothesis → repeat
//! ```
//!
//! Tier 3 is pluggable behind [`tier3::Tier3Backend`].

pub mod tier3;

use async_trait::async_trait;
use mossraven_archive::{Archive, ArchiveEntry, CellCoords};
use mossraven_pob::BuildStats;
use mossraven_surrogate::{MutationProposal, SurrogateProvider};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tier3::Tier3Backend;

pub mod mutate;

/// Cheap non-cryptographic 64-bit hash for "did this string change" diffs in
/// trace logs. FNV-1a — no extra deps, distinguishes 1-char changes reliably.
fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("tier-3 backend error: {0}")]
    Tier3(String),
    #[error("surrogate error: {0}")]
    Surrogate(String),
    #[error("archive error: {0}")]
    Archive(String),
    #[error("not implemented (stub)")]
    NotImplemented,
}

/// Tunable knobs for one generation of the cascade.
#[derive(Debug, Clone)]
pub struct StepConfig {
    /// How many candidates to ask the surrogate to propose per step.
    pub mutations_per_step: usize,
    /// Minimum surrogate `interest` score for a candidate to survive pruning.
    pub interest_threshold: f32,
    /// Minimum surrogate `plausibility` score for a candidate to survive pruning.
    pub plausibility_threshold: f32,
    /// Data-version stamp baked into archive entries. Patches change calc math;
    /// without this stamp the archive silently rots across game updates.
    pub data_version: String,
}

impl Default for StepConfig {
    fn default() -> Self {
        Self {
            // 10 = one full pass of the MockSurrogate's deterministic axis
            // set, and a healthier diversity budget for live providers too.
            mutations_per_step: 10,
            // Lowered from 0.4/0.5 — the surrogate's cheap-score is too
            // aggressive for our use case: it kills "boring" mutations like
            // "Whirling Slash quality 0→20" which actually move DPS, while
            // letting through "novel" cosmetic swaps that don't. Until we
            // have a better Tier-2 judge, let almost everything through and
            // let Tier-3 + the MAP-Elites elite check do the filtering.
            interest_threshold: 0.1,
            plausibility_threshold: 0.2,
            data_version: "pob2:unknown".to_string(),
        }
    }
}

/// Live search state — what hypothesis are we exploring, from what XML.
/// Updated by `set_state` when the dreamer seeds a new search.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub concept: String,
    pub rationale: Option<String>,
    pub initial_cell_focus: Option<String>,
    /// Starting PoB XML for mutation. Empty means "no seed yet"; the cascade
    /// will still run but Tier 3 will reject every variant.
    pub seed_pob_xml: String,
    /// Optional cell-region focus (e.g. `"chaos/es/boss/*"`). Folded into the
    /// surrogate prompt so mutations bias toward matching cells.
    pub region: Option<String>,
    pub config: StepConfig,
}

pub struct SearchEngine {
    pub archive: Arc<Archive>,
    pub surrogate: Arc<dyn SurrogateProvider>,
    pub tier3: Arc<dyn Tier3Backend>,
    pub state: Arc<Mutex<SearchState>>,
    /// PoB gem database (Gems.lua). Powers real gem swaps (gemId/skillId
    /// rewrite) and ground-truth damage_type cell labels. An empty db keeps
    /// the cascade running with swap/add ops skipped.
    pub gem_db: Arc<mossraven_pob::GemDb>,
}

impl SearchEngine {
    pub fn new(
        archive: Arc<Archive>,
        surrogate: Arc<dyn SurrogateProvider>,
        tier3: Arc<dyn Tier3Backend>,
        gem_db: Arc<mossraven_pob::GemDb>,
    ) -> Self {
        Self {
            archive,
            surrogate,
            tier3,
            state: Arc::new(Mutex::new(SearchState::default())),
            gem_db,
        }
    }

    /// Set the active hypothesis + seed XML. Call this from `seed_hypothesis`.
    pub fn set_state(
        &self,
        concept: impl Into<String>,
        rationale: Option<String>,
        initial_cell_focus: Option<String>,
        seed_pob_xml: impl Into<String>,
    ) {
        let mut s = self.state.lock();
        s.concept = concept.into();
        s.rationale = rationale;
        s.initial_cell_focus = initial_cell_focus;
        s.seed_pob_xml = seed_pob_xml.into();
    }

    /// Set or clear the cell-region focus for subsequent steps. Comes from
    /// `run_search(region)` on the MCP surface.
    pub fn set_region(&self, region: Option<String>) {
        self.state.lock().region = region;
    }

    /// One generation of the cascade. Returns counts for the report.
    pub async fn step(&self) -> Result<GenerationReport, CoreError> {
        // Snapshot the state so we don't hold the lock across awaits.
        let (concept, seed_xml, cfg, fallback_focus, region) = {
            let s = self.state.lock();
            (
                s.concept.clone(),
                s.seed_pob_xml.clone(),
                s.config.clone(),
                s.initial_cell_focus.clone(),
                s.region.clone(),
            )
        };

        if concept.is_empty() {
            tracing::warn!(
                "SearchEngine::step called without a seeded hypothesis; \
                 call seed_hypothesis first"
            );
            return Ok(GenerationReport::default());
        }

        // 1. Tier 2 surrogate: propose mutations. Region focus (if any) rides
        // along in the hypothesis text — no trait churn, surrogate-agnostic.
        let concept_for_surrogate = match &region {
            Some(r) => format!(
                "{concept}\n[FOCUS REGION: {r} — bias mutations toward MAP-Elites cells matching this pattern]"
            ),
            None => concept.clone(),
        };
        let proposals = self
            .surrogate
            .propose_mutations(&seed_xml, &concept_for_surrogate, cfg.mutations_per_step)
            .await
            .map_err(|e| CoreError::Surrogate(e.to_string()))?;
        let proposed = proposals.len();
        if proposed == 0 {
            return Ok(GenerationReport::default());
        }

        // 2. Tier 2 surrogate: cheap-score
        let scores = self
            .surrogate
            .cheap_score(&proposals)
            .await
            .map_err(|e| CoreError::Surrogate(e.to_string()))?;
        let score_map: HashMap<String, _> =
            scores.into_iter().map(|s| (s.variant_id.clone(), s)).collect();

        // 3. Prune by interest + plausibility
        let survivors: Vec<MutationProposal> = proposals
            .into_iter()
            .filter(|p| match score_map.get(&p.variant_id) {
                Some(s) => {
                    s.interest >= cfg.interest_threshold
                        && s.plausibility >= cfg.plausibility_threshold
                }
                None => false,
            })
            .collect();
        let pruned = proposed - survivors.len();

        // 3b. Apply structured mutation ops to each survivor's seed XML.
        // Until this step landed, every variant scored identically to the
        // seed — same DPS, same EHP, same cell. apply_ops_to_xml mutates
        // gem level/quality/swap on the survivor's pob_xml so Tier 3 sees
        // a genuinely different build per variant.
        let survivors: Vec<MutationProposal> = survivors
            .into_iter()
            .map(|mut p| {
                let before_len = p.pob_xml.len();
                let before_hash = simple_hash(&p.pob_xml);
                if !p.ops.is_empty() {
                    p.pob_xml = mutate::apply_ops_to_xml(&p.pob_xml, &p.ops, &self.gem_db);
                }
                let after_hash = simple_hash(&p.pob_xml);
                tracing::info!(
                    variant = %p.variant_id,
                    ops_count = p.ops.len(),
                    xml_changed = before_hash != after_hash,
                    xml_len_before = before_len,
                    xml_len_after = p.pob_xml.len(),
                    ops = ?p.ops,
                    "mutation applied"
                );
                p
            })
            .collect();

        // 4. Tier 3 score the survivors
        let batch: Vec<(String, String)> = survivors
            .iter()
            .map(|p| (p.variant_id.clone(), p.pob_xml.clone()))
            .collect();
        let scored = self
            .tier3
            .score(batch)
            .await
            .map_err(|e| CoreError::Tier3(e.to_string()))?;

        // 5. Place survivors that scored OK into the archive
        let proposal_by_id: HashMap<String, MutationProposal> = survivors
            .into_iter()
            .map(|p| (p.variant_id.clone(), p))
            .collect();

        let mut variants_scored = 0;
        let mut cells_placed = 0;
        for (id, result) in scored {
            match result {
                Ok(stats) => {
                    variants_scored += 1;
                    let proposal = match proposal_by_id.get(&id) {
                        Some(p) => p,
                        None => continue,
                    };
                    let hint = proposal
                        .cell_focus
                        .as_deref()
                        .or(fallback_focus.as_deref());
                    // Ground-truth damage_type: the scored gem of THIS
                    // variant's (post-mutation) XML, looked up in PoB's own
                    // gem data. The surrogate's guess is fallback only —
                    // handoff [2] found it labeling a physical Whirling
                    // Slash build "lightning/evasion/...".
                    let damage_truth = mossraven_surrogate::find_main_skill_gem_name(&proposal.pob_xml)
                        .and_then(|g| self.gem_db.get(&g).and_then(|i| i.damage_type()));
                    let coords = coords_from_stats(&stats, hint, damage_truth);
                    let entry = ArchiveEntry {
                        variant_id: id.clone(),
                        pob_xml: proposal.pob_xml.clone(),
                        stats,
                        origin_hypothesis: proposal.origin_hypothesis.clone(),
                        data_version: cfg.data_version.clone(),
                    };
                    if self.archive.try_place(coords, entry) {
                        cells_placed += 1;
                    }
                }
                Err(e) => {
                    tracing::debug!(variant_id = %id, error = %e, "tier-3 rejected variant");
                }
            }
        }

        Ok(GenerationReport {
            variants_proposed: proposed,
            variants_pruned: pruned,
            variants_scored,
            cells_filled_or_improved: cells_placed,
        })
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct GenerationReport {
    pub variants_proposed: usize,
    pub variants_pruned: usize,
    pub variants_scored: usize,
    pub cells_filled_or_improved: usize,
}

/// Map a `BuildStats` (+ ground truth + optional hint) to a [`CellCoords`].
///
/// Axis sources, most-trusted first (handoff [2] — "derive CellCoords from
/// actual BuildStats + build composition, not the surrogate's guess"):
/// - `damage_type`: `damage_truth` (scored gem's tags from PoB's own data) →
///   surrogate hint → "unknown". The hint mislabeled a physical Whirling
///   Slash build as "lightning/evasion" in the validation run.
/// - `defense_layer`: ALWAYS stats-derived (dominant pool). The hint is not
///   consulted — stats are authoritative and always present.
/// - `role` / `scaling_vector`: hint → defaults. These describe intent
///   (clear vs boss, what the mutation scales) that stats can't reveal.
pub fn coords_from_stats(
    stats: &BuildStats,
    hint: Option<&str>,
    damage_truth: Option<&str>,
) -> CellCoords {
    let parts: Vec<&str> = hint.unwrap_or("").split('/').collect();
    let get = |i: usize| {
        parts
            .get(i)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(String::from)
    };

    let damage_type = damage_truth
        .map(String::from)
        .or_else(|| get(0))
        .unwrap_or_else(|| "unknown".to_string());
    let defense_layer = {
        // Dominant defense pool. Life > 0 fallback so the ratio is finite.
        let life = stats.life.max(1.0);
        if stats.energy_shield > life * 1.0 {
            "es"
        } else if stats.armour > stats.evasion && stats.armour > stats.energy_shield {
            "armour"
        } else if stats.evasion > stats.armour && stats.evasion > stats.energy_shield {
            "evasion"
        } else {
            "hybrid"
        }
        .to_string()
    };
    let role = get(2).unwrap_or_else(|| "boss".to_string());
    let scaling_vector = get(3).unwrap_or_else(|| "unknown".to_string());

    CellCoords {
        damage_type,
        defense_layer,
        role,
        scaling_vector,
    }
}

/// Trait re-exported for symmetry with the workspace surface. (The actual
/// trait lives in `tier3::Tier3Backend`; this alias is for external callers.)
#[async_trait]
pub trait Tier3BackendExt: Send + Sync {
    async fn score_batch(
        &self,
        variants: Vec<(String, String)>,
    ) -> Result<Vec<(String, Result<BuildStats, String>)>, CoreError>;
}
