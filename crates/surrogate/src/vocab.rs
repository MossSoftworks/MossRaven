//! POE2 entity vocabulary — embedded datamined names that ground the
//! surrogate's prompts in real game content instead of LLM-hallucinated PoE1
//! names.
//!
//! Source: `crates/surrogate/src/poe2_vocab.json`, extracted from
//! HivemindOverlord/poe2-mcp (MIT) by `scripts/extract-poe2-vocab.py`.
//!
//! ## Why this exists
//!
//! Free-tier Cerebras models (gpt-oss-120b, zai-glm-4.7) consistently leaked
//! PoE1-only entities into mutation proposals: Vaal Cold Snap, Elemental Focus
//! support, Watcher's Eye, Shaper influence. Tier 3 then rejected every
//! proposal because the gem names didn't resolve in PoB2's data tables, so
//! the archive stayed empty.
//!
//! Anchoring the prompt with the real list of POE2 skill + support gem names
//! cuts the hallucination rate dramatically. The vocab is small enough
//! (~16 KB) to ship inside the binary via `include_str!`, so no I/O is needed
//! at runtime and the binary stays self-contained.

use serde::Deserialize;
use std::sync::OnceLock;

/// Raw vocab document as parsed from `poe2_vocab.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Poe2Vocab {
    pub source: String,
    pub license: String,
    pub game_version: Option<String>,
    pub skills: Vec<Poe2Skill>,
    pub supports: Vec<Poe2Support>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Poe2Skill {
    pub name: String,
    /// "fire" / "cold" / "lightning" / "chaos" / "physical" / None (for
    /// minion / meta / buff / curse / herald / elemental-expression buckets).
    #[serde(default)]
    pub element: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Poe2Support {
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// What categories of skill this support can attach to (e.g. ["spell"],
    /// ["bow", "crossbow"]).
    #[serde(default)]
    pub compatible_with: Vec<String>,
}

const VOCAB_JSON: &str = include_str!("poe2_vocab.json");

/// Lazy global — parsed once, used by every surrogate prompt build.
fn vocab() -> &'static Poe2Vocab {
    static CACHED: OnceLock<Poe2Vocab> = OnceLock::new();
    CACHED.get_or_init(|| {
        serde_json::from_str::<Poe2Vocab>(VOCAB_JSON)
            .expect("embedded poe2_vocab.json is malformed — re-run scripts/extract-poe2-vocab.py")
    })
}

/// Build a compact entity-list block to splice into the surrogate system prompt.
///
/// Format:
/// ```text
/// POE2 SKILL GEMS (datamined, v0.3.0):
///   Arc [lightning: spell, projectile, chaining]
///   Arctic Armour [cold: spell, buff, persistent, staged]
///   ...
/// POE2 SUPPORT GEMS (datamined):
///   Controlled Destruction [Critical, Spell; compatible: spell]
///   ...
/// ```
///
/// `max_skills` and `max_supports` cap the included entries so a 4k-context
/// model isn't blown out — defaults are calibrated for the full vocab (under
/// 100 entries each) but adjustable for tighter budgets.
pub fn prompt_block(max_skills: usize, max_supports: usize) -> String {
    let v = vocab();
    let mut out = String::with_capacity(8192);

    out.push_str("POE2 SKILL GEMS (datamined");
    if let Some(ver) = v.game_version.as_deref() {
        out.push_str(", ");
        out.push_str(ver);
    }
    out.push_str("):\n");
    for s in v.skills.iter().take(max_skills) {
        out.push_str("  ");
        out.push_str(&s.name);
        out.push_str(" [");
        if let Some(elem) = s.element.as_deref() {
            out.push_str(elem);
            out.push_str(": ");
        }
        out.push_str(&s.tags.join(", "));
        out.push_str("]\n");
    }
    out.push_str("\nPOE2 SUPPORT GEMS (datamined):\n");
    for s in v.supports.iter().take(max_supports) {
        out.push_str("  ");
        out.push_str(&s.name);
        out.push_str(" [");
        out.push_str(&s.tags.join(", "));
        if !s.compatible_with.is_empty() {
            out.push_str("; compatible: ");
            out.push_str(&s.compatible_with.join(","));
        }
        out.push_str("]\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_parses_and_has_entries() {
        let v = vocab();
        assert!(!v.skills.is_empty(), "skills list empty — extraction broken?");
        assert!(!v.supports.is_empty(), "supports list empty — extraction broken?");
        // Sanity: a few POE2 names we know exist
        assert!(
            v.skills.iter().any(|s| s.name == "Fireball"),
            "Fireball missing from skill vocab"
        );
        assert!(
            v.supports.iter().any(|s| s.name == "Controlled Destruction"),
            "Controlled Destruction missing from support vocab"
        );
    }

    #[test]
    fn prompt_block_includes_skill_and_support_section() {
        let block = prompt_block(200, 200);
        assert!(block.contains("POE2 SKILL GEMS"));
        assert!(block.contains("POE2 SUPPORT GEMS"));
        assert!(block.contains("Fireball"));
        assert!(block.contains("Controlled Destruction"));
    }
}
