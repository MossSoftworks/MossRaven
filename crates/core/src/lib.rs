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
pub mod viability;

/// The closest reachable tree notables for a seed, as (name, cost, stats).
/// Feeds both the surrogate prompt block AND the engine's forced tree
/// exploration. Empty when the tree db or the seed's Spec is unavailable.
fn reachable_notables(
    seed_xml: &str,
    tree_db: &mossraven_pob::TreeDb,
) -> Vec<(String, usize, String)> {
    if tree_db.is_empty() {
        return Vec::new();
    }
    let Some(spec_start) = seed_xml.find("<Spec ") else {
        return Vec::new();
    };
    let Some(end) = seed_xml[spec_start..].find('>') else {
        return Vec::new();
    };
    let tag = &seed_xml[spec_start..spec_start + end];
    let attr = |a: &str| -> Option<&str> {
        let needle = format!("{a}=\"");
        let i = tag.find(&needle)?;
        let st = i + needle.len();
        let e = tag[st..].find('"')?;
        Some(&tag[st..st + e])
    };
    let ver = attr("treeVersion").unwrap_or("0_4");
    let allocated: std::collections::HashSet<u32> = attr("nodes")
        .map(|csv| csv.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_default();
    if allocated.is_empty() {
        return Vec::new();
    }
    // Weapon-set nodes can't anchor normal-mode paths (see mutate.rs) — list
    // only notables the applier could actually deliver.
    let ws = if tag.ends_with('/') {
        std::collections::HashSet::new()
    } else {
        mutate::weapon_set_ids(seed_xml, spec_start + end + 1)
    };
    let anchors: std::collections::HashSet<u32> = allocated.difference(&ws).copied().collect();
    if anchors.is_empty() {
        return Vec::new();
    }
    tree_db.nearby_notables(ver, &anchors, &ws, 6, 48)
}

/// Offense-flavored notable, by stat text. Drives the forced-exploration
/// ordering: the DPS floor is the binding SPEC constraint, so damage wheels
/// get probed before defense/utility. False positives are cheap — a wasted
/// slot still explores SOMETHING and MAP-Elites may keep it for EHP cells.
fn is_offense_notable(stats: &str) -> bool {
    (stats.contains("Damage") && !stats.contains("taken") && !stats.contains("Taken"))
        || stats.contains("Critical")
        || stats.contains("Cast Speed")
        || stats.contains("Attack Speed")
        || stats.contains("Penetrat")
}

/// Prompt block built from [`reachable_notables`] so the LLM proposes
/// allocations that exist AND connect.
fn format_notables_block(near: &[(String, usize, String)]) -> String {
    if near.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n[REACHABLE TREE NOTABLES — allocate with {\"op\":\"allocate_notable\",\"name\":\"<exact name>\"}; cost = travel points spent]\n",
    );
    for (name, cost, stats) in near {
        out.push_str(&format!("  {name} (cost {cost}): {stats}\n"));
    }
    out
}

/// Per-seed equippable-unique candidates: offense uniques for the equipped
/// weapon kind plus jewellery/gloves. Returns (slot, name) picks for the
/// engine's forced item exploration AND the prompt block teaching the LLM
/// the `equip_unique` op. Gear costs ZERO passive points — it's the only
/// DPS lever left once a tree is at budget.
fn equippable_uniques(
    seed_xml: &str,
    unique_db: &mossraven_pob::UniqueDb,
) -> (Vec<(String, String)>, String) {
    if unique_db.is_empty() {
        return (Vec::new(), String::new());
    }
    let mut picks: Vec<(String, String, String)> = Vec::new(); // slot, name, mods

    // Weapon 1: same-kind swaps only (a staff build explores staves).
    if let Some(base) = equipped_base(seed_xml, "Weapon 1") {
        if let Some(kind) = mossraven_pob::UniqueDb::kind_of_base(&base) {
            for u in unique_db.of_kind(kind).iter().filter(|u| u.is_offense()) {
                picks.push(("Weapon 1".into(), u.name.clone(), u.mods_joined.clone()));
            }
        }
    }
    for (slot, kind) in [
        ("Amulet", "amulet"),
        ("Ring 1", "ring"),
        ("Gloves", "gloves"),
    ] {
        for u in unique_db.of_kind(kind).iter().filter(|u| u.is_offense()) {
            picks.push((slot.into(), u.name.clone(), u.mods_joined.clone()));
        }
    }
    picks.truncate(40);
    if picks.is_empty() {
        return (Vec::new(), String::new());
    }

    let mut block = String::from(
        "\n[EQUIPPABLE UNIQUES — equip with {\"op\":\"equip_unique\",\"slot\":\"<slot>\",\"name\":\"<exact name>\"}; gear costs NO passive points]\n",
    );
    for (slot, name, mods) in picks.iter().take(24) {
        let m: String = mods.chars().take(90).collect();
        block.push_str(&format!("  {slot}: {name} — {m}\n"));
    }
    (
        picks.into_iter().map(|(s, n, _)| (s, n)).collect(),
        block,
    )
}

/// Base-type line of the item equipped in `slot` of the ACTIVE item set.
/// (Item text: "Rarity: X" / name / base — base is line 3 for RARE/UNIQUE,
/// line 2 for NORMAL/MAGIC; we probe both through `kind_of_base`.)
fn equipped_base(xml: &str, slot: &str) -> Option<String> {
    let items_start = xml.find("<Items ")?;
    let items_tag_end = items_start + xml[items_start..].find('>')?;
    let items_tag = &xml[items_start..items_tag_end];
    let attr = |tag: &str, a: &str| -> Option<String> {
        let needle = format!("{a}=\"");
        let i = tag.find(&needle)?;
        let st = i + needle.len();
        let e = tag[st..].find('"')?;
        Some(tag[st..st + e].to_string())
    };
    let active_set = attr(items_tag, "activeItemSet").unwrap_or_else(|| "1".into());

    // Find the active ItemSet span, then the slot tag inside it.
    let set_needle = format!("id=\"{active_set}\"");
    let mut search = items_start;
    let items_close = xml.find("</Items>").unwrap_or(xml.len());
    let mut item_id = None;
    while let Some(rel) = xml[search..items_close].find("<ItemSet ") {
        let tag_start = search + rel;
        let tag_end = tag_start + xml[tag_start..].find('>')?;
        if xml[tag_start..tag_end].contains(&set_needle) {
            let set_end = tag_start
                + xml[tag_start..]
                    .find("</ItemSet>")
                    .unwrap_or(items_close - tag_start);
            let slot_needle = format!("name=\"{slot}\"");
            let mut s2 = tag_start;
            while let Some(r2) = xml[s2..set_end].find("<Slot ") {
                let t2 = s2 + r2;
                let e2 = t2 + xml[t2..].find('>')?;
                if xml[t2..e2].contains(&slot_needle) {
                    item_id = attr(&xml[t2..e2], "itemId");
                    break;
                }
                s2 = e2;
            }
            break;
        }
        search = tag_end;
    }
    let item_id = item_id?;
    if item_id == "0" {
        return None;
    }
    let item_open = format!("<Item id=\"{item_id}\">");
    let i = xml.find(&item_open)?;
    let body_start = i + item_open.len();
    let body_end = body_start + xml[body_start..].find("</Item>")?;
    let lines: Vec<&str> = xml[body_start..body_end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Probe lines 2 then 1 (0-indexed) — covers RARE/UNIQUE then NORMAL/MAGIC.
    for idx in [2usize, 1] {
        if let Some(l) = lines.get(idx) {
            if mossraven_pob::UniqueDb::kind_of_base(l).is_some() {
                return Some((*l).to_string());
            }
        }
    }
    lines.get(2).or_else(|| lines.get(1)).map(|l| (*l).to_string())
}

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
    /// Passive-tree database (TreeData/<ver>/tree.json). Powers pathed
    /// allocate_notable ops + the per-seed reachable-notables prompt block.
    pub tree_db: Arc<mossraven_pob::TreeDb>,
    /// Unique-item database (Data/Uniques/*.lua). Powers equip_unique ops +
    /// the per-seed equippable-uniques prompt block.
    pub unique_db: Arc<mossraven_pob::UniqueDb>,
    /// Generation counter — rotates the engine-forced exploration picks
    /// (tree notables + gear) so successive generations probe new ground.
    gen_counter: std::sync::atomic::AtomicUsize,
}

impl SearchEngine {
    pub fn new(
        archive: Arc<Archive>,
        surrogate: Arc<dyn SurrogateProvider>,
        tier3: Arc<dyn Tier3Backend>,
        gem_db: Arc<mossraven_pob::GemDb>,
        tree_db: Arc<mossraven_pob::TreeDb>,
        unique_db: Arc<mossraven_pob::UniqueDb>,
    ) -> Self {
        Self {
            archive,
            surrogate,
            tier3,
            state: Arc::new(Mutex::new(SearchState::default())),
            gem_db,
            tree_db,
            unique_db,
            gen_counter: std::sync::atomic::AtomicUsize::new(0),
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

        // 0. STEADY-STATE parent selection. Mutating the static session seed
        // every generation is (1+λ) hill-climbing: no variant can ever stack
        // two mutations, which caps tree growth at one notable from the
        // original allocation set. Canonical MAP-Elites mutates ELITES — an
        // elite that won its cell with notable A gets drawn again and gains
        // notable B, compounding. Parent rotation: upper half of the archive
        // by DPS (the binding SPEC §1.1.1 gap), with the session seed
        // re-injected every 4th generation so the hypothesis basin keeps
        // getting explored from the root.
        let gen = self.gen_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Spare passive points on the parent (None = unmeasured, allow and
        // let the post-score guard decide). Gates the forced tree exploration:
        // a parent at budget makes every costed allocation a guaranteed drop.
        let mut parent_spare_points: Option<u32> = None;
        let seed_xml = {
            let mut elites = self.archive.snapshot();
            if elites.is_empty() || gen % 4 == 0 {
                seed_xml
            } else {
                elites.sort_by(|a, b| {
                    b.1.stats
                        .total_dps
                        .partial_cmp(&a.1.stats.total_dps)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                let pool = elites.len().div_ceil(2);
                let (coords, parent) = &elites[(gen / 4 * 3 + gen % 4 - 1) % pool];
                if parent.stats.points_budget > 0 {
                    parent_spare_points = Some(
                        parent
                            .stats
                            .points_budget
                            .saturating_sub(parent.stats.points_used),
                    );
                }
                tracing::info!(
                    parent_dps = parent.stats.total_dps,
                    parent_cell = %coords.as_path_segment(),
                    pool,
                    spare_points = ?parent_spare_points,
                    "steady-state parent: mutating archive elite"
                );
                parent.pob_xml.clone()
            }
        };

        // 1. Tier 2 surrogate: propose mutations. Region focus (if any) rides
        // along in the hypothesis text — no trait churn, surrogate-agnostic.
        let nearby = reachable_notables(&seed_xml, &self.tree_db);
        let notables_block = format_notables_block(&nearby);
        let (unique_picks, uniques_block) = equippable_uniques(&seed_xml, &self.unique_db);
        let concept_for_surrogate = match &region {
            Some(r) => format!(
                "{concept}\n[FOCUS REGION: {r} — bias mutations toward MAP-Elites cells matching this pattern]{notables_block}{uniques_block}"
            ),
            None => format!("{concept}{notables_block}{uniques_block}"),
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

        // 3a½. MECHANICAL scored-group guard. Live LLMs kept mutating a
        // group-1 utility gem (Frost Bomb) instead of the scored skill
        // despite the prompt marker — 9/10 variants scored identically.
        // Prompts request; guards enforce:
        //   - gem-targeting ops whose gem is NOT in the scored group are
        //     DROPPED (logged) — they apply fine but can't move the score.
        //   - a variant left with zero ops gets a deterministic fallback
        //     (main-skill level from an explore ladder keyed by its index)
        //     so the slot still explores instead of duplicating the seed.
        // AddSupportGem targets the scored group by construction and
        // SetActiveWeaponSet is loadout-global — both pass through.
        let main_gem = mossraven_surrogate::find_main_skill_gem_name(&seed_xml);
        let survivors: Vec<MutationProposal> = survivors
            .into_iter()
            .enumerate()
            .map(|(idx, mut p)| {
                use mossraven_surrogate::{gem_in_main_group, MutationOp};
                let before = p.ops.len();
                p.ops.retain(|op| {
                    let target = match op {
                        MutationOp::SetGemLevel { gem, .. }
                        | MutationOp::SetGemQuality { gem, .. }
                        | MutationOp::RemoveGem { gem } => Some(gem.as_str()),
                        MutationOp::SwapGem { old, .. } => Some(old.as_str()),
                        MutationOp::AddSupportGem { .. }
                        | MutationOp::AllocateNotable { .. }
                        | MutationOp::EquipUnique { .. }
                        | MutationOp::SetActiveWeaponSet { .. } => None,
                    };
                    match target {
                        Some(g) if g != "*" && !gem_in_main_group(&seed_xml, g) => {
                            tracing::warn!(
                                variant = %p.variant_id,
                                gem = g,
                                op = ?op,
                                "op targets an UNSCORED group; dropped by scored-group guard"
                            );
                            false
                        }
                        _ => true,
                    }
                });
                if p.ops.is_empty() && before > 0 {
                    if let Some(main) = &main_gem {
                        let ladder = [4u32, 7, 10, 13, 16, 19, 20, 14, 8, 18];
                        let level = ladder[idx % ladder.len()];
                        tracing::info!(
                            variant = %p.variant_id,
                            level,
                            "all ops guarded out; retargeted to main skill {main} explore ladder"
                        );
                        p.ops = vec![MutationOp::SetGemLevel {
                            gem: main.clone(),
                            level,
                        }];
                    }
                }
                p
            })
            .collect();

        // 3a¾. ENGINE-FORCED tree exploration. Live LLMs never propose
        // allocate_notable even with the prompt billing it as the strongest
        // DPS lever — they fixate on familiar gem ops (observed across
        // Cerebras/Groq/Gemini, 0/30 variants over 3 generations). Prompts
        // request; engines enforce: the LAST `TREE_EXPLORE_VARIANTS` variants
        // each get one AllocateNotable appended, cycling through the seed's
        // reachable notables across generations so successive elites compound
        // tree growth. MAP-Elites keeps whatever scores.
        const FORCED_EXPLORE_VARIANTS: usize = 3;
        // Tree picks: offense notables first (cost order within each class),
        // budget-gated — only paths whose worst-case cost fits the parent's
        // spare points (path cost ≤ hop count; free-allocate nodes only make
        // it cheaper). An at-budget parent contributes NO tree picks.
        let tree_picks: Vec<&(String, usize, String)> = nearby
            .iter()
            .filter(|n| is_offense_notable(&n.2))
            .chain(nearby.iter().filter(|n| !is_offense_notable(&n.2)))
            .filter(|n| parent_spare_points.is_none_or(|spare| n.1 as u32 <= spare))
            .collect();
        // Combined explorer: gear first (zero passive points — the only
        // lever an at-budget tree has), then affordable notables. The
        // generation counter walks the whole list across runs.
        enum Explore {
            Tree(String),
            Gear(String, String),
        }
        let explorer: Vec<Explore> = unique_picks
            .iter()
            .map(|(slot, name)| Explore::Gear(slot.clone(), name.clone()))
            .chain(tree_picks.iter().map(|n| Explore::Tree(n.0.clone())))
            .collect();
        if explorer.is_empty() && (!nearby.is_empty() || !unique_picks.is_empty()) {
            tracing::info!(
                spare_points = ?parent_spare_points,
                "forced exploration skipped: nothing affordable or equippable"
            );
        }
        let survivors: Vec<MutationProposal> = if explorer.is_empty() {
            survivors
        } else {
            let n = survivors.len();
            survivors
                .into_iter()
                .enumerate()
                .map(|(idx, mut p)| {
                    if idx + FORCED_EXPLORE_VARIANTS >= n {
                        let slot_i = idx + FORCED_EXPLORE_VARIANTS - n;
                        let pick = (gen * FORCED_EXPLORE_VARIANTS + slot_i) % explorer.len();
                        match &explorer[pick] {
                            Explore::Tree(name) => {
                                tracing::info!(
                                    variant = %p.variant_id,
                                    notable = %name,
                                    "engine-forced exploration: allocate_notable appended"
                                );
                                p.ops.push(mossraven_surrogate::MutationOp::AllocateNotable {
                                    name: name.clone(),
                                });
                            }
                            Explore::Gear(slot, name) => {
                                tracing::info!(
                                    variant = %p.variant_id,
                                    slot = %slot,
                                    unique = %name,
                                    "engine-forced exploration: equip_unique appended"
                                );
                                p.ops.push(mossraven_surrogate::MutationOp::EquipUnique {
                                    slot: slot.clone(),
                                    name: name.clone(),
                                });
                            }
                        }
                    }
                    p
                })
                .collect()
        };

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
                    p.pob_xml = mutate::apply_ops_to_xml(
                        &p.pob_xml,
                        &p.ops,
                        &self.gem_db,
                        &self.tree_db,
                        &self.unique_db,
                    );
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
                    // ILLEGALITY GUARD: over-budget trees never enter the
                    // archive. PoB calculates them happily, so without this
                    // the compounding loop inflates trees forever and every
                    // downstream elite/finalist inherits an unbuildable tree.
                    // (budget==0 = unmeasured; skip — see BuildStats docs.)
                    if stats.points_budget > 0 && stats.points_used > stats.points_budget {
                        tracing::warn!(
                            variant_id = %id,
                            points_used = stats.points_used,
                            points_budget = stats.points_budget,
                            level = stats.character_level,
                            "variant exceeds passive point budget; dropped (not archived)"
                        );
                        continue;
                    }
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

    // Canonical labels only: snap hint fuzz ("supports"/"support-swap") and
    // schema echoes ("scaling_vector" as a literal value) to the whitelist
    // so the grid never fragments — every fragment hides an elite check.
    CellCoords {
        damage_type,
        defense_layer,
        role,
        scaling_vector,
    }
    .normalized()
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
