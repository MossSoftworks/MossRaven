//! SPEC §1.1.2 cost layer (v1 — heuristic bands, no live market).
//!
//! A pure function of the build XML: walk the ACTIVE item set's equipped
//! slots, price each item by a deliberately-blunt heuristic, sum, band.
//! The point of v1 is RELATIVE comparison (Tier 6 curates on value and the
//! guides talk about cost reality), not exchange-rate accuracy. Follow-up:
//! poe.ninja PoE2 economy API as an env-gated live source.
//!
//! Heuristic:
//! - rares: mod-line density is the strongest cheap proxy for price —
//!   4 mods ≈ 1 div, 5 ≈ 5, 6+ ≈ 15; desecrated/fractured lines ×2;
//!   corrupted ×1.5 (good corruptions cost, bricked ones aren't equipped).
//! - uniques: flat 5 div baseline (most uniques are cheap), with a small
//!   curated CHASE list at 100 div — heuristic, revisited each league.
//! - normal/magic: 0.1 div.
//! - jewels/flasks/charms: priced like their rarity (cheap; rare jewels
//!   with 4 mods price as 4-mod rares).

use serde::Serialize;

/// Curated chase uniques (PoE2 0.5 era) — names lowercased for lookup.
/// Being on this list ≈ "this is the expensive part of the build".
const CHASE_UNIQUES: &[&str] = &[
    "temporalis",
    "ingenuity",
    "astramentis",
    "morior invictus",
    "hand of wisdom and action",
    "headhunter",
    "mageblood",
    "the adorned",
    "from nothing",
    "sandstorm visage",
];

pub const BAND_BUDGET_MAX: f64 = 5.0;
pub const BAND_MID_MAX: f64 = 30.0;
pub const BAND_EXPENSIVE_MAX: f64 = 150.0;

pub fn cost_band(total_div: f64) -> &'static str {
    if total_div <= BAND_BUDGET_MAX {
        "budget (≤5 div)"
    } else if total_div <= BAND_MID_MAX {
        "mid (≤30 div)"
    } else if total_div <= BAND_EXPENSIVE_MAX {
        "expensive (≤150 div)"
    } else {
        "mirror-tier (150+ div)"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CostEstimate {
    pub total_div: f64,
    pub band: &'static str,
    /// (item display name, estimated div) — sorted descending, capped.
    pub breakdown: Vec<(String, f64)>,
}

/// Estimate the equipped-gear cost of a build. Returns zero-cost when the
/// XML has no parsable Items section (cost unknown ≠ free, but a missing
/// section means a synthetic/test build — banding it "budget" is harmless).
pub fn estimate_cost(xml: &str) -> CostEstimate {
    let mut breakdown: Vec<(String, f64)> = Vec::new();
    for (slot, item_text) in equipped_items(xml) {
        let price = price_item(&item_text);
        let name = item_display_name(&item_text).unwrap_or_else(|| slot.clone());
        breakdown.push((name, price));
    }
    breakdown.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = breakdown.iter().map(|(_, p)| p).sum();
    breakdown.truncate(8);
    CostEstimate {
        total_div: (total * 10.0).round() / 10.0,
        band: cost_band(total),
        breakdown,
    }
}

fn price_item(text: &str) -> f64 {
    let lower = text.to_lowercase();
    let rarity = if lower.contains("rarity: unique") {
        "unique"
    } else if lower.contains("rarity: rare") {
        "rare"
    } else {
        "common"
    };
    match rarity {
        "unique" => {
            let name = item_display_name(text).unwrap_or_default().to_lowercase();
            // Live overlay (poe.ninja via MOSSRAVEN_PRICES_PATH) beats the
            // heuristic; heuristic stays as the offline floor.
            if let Some(div) = live_prices().get(&name) {
                return *div;
            }
            if CHASE_UNIQUES.iter().any(|c| name.contains(c)) {
                100.0
            } else {
                5.0
            }
        }
        "rare" => {
            // Mod lines = lines after the header block that aren't metadata.
            let mods = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .filter(|l| {
                    !(l.starts_with("Rarity:")
                        || l.starts_with("Unique ID:")
                        || l.starts_with("Item Level:")
                        || l.starts_with("Quality:")
                        || l.starts_with("Sockets:")
                        || l.starts_with("Rune:")
                        || l.starts_with("LevelReq:")
                        || l.starts_with("Implicits:")
                        || l.starts_with("Energy Shield:")
                        || l.starts_with("Armour:")
                        || l.starts_with("Evasion:")
                        || l.starts_with("{enchant}")
                        || l.starts_with("{rune}")
                        || *l == "Corrupted")
                })
                .count()
                // first two non-meta lines are name + base
                .saturating_sub(2);
            let base = match mods {
                0..=4 => 1.0,
                5 => 5.0,
                _ => 15.0,
            };
            let desecrated = lower.contains("{desecrated}") || lower.contains("{fractured}");
            let corrupted = lower.contains("\ncorrupted") || lower.ends_with("corrupted");
            base * if desecrated { 2.0 } else { 1.0 } * if corrupted { 1.5 } else { 1.0 }
        }
        _ => 0.1,
    }
}

/// Live unique prices (name lowercase → divines), loaded once from the JSON
/// file `MOSSRAVEN_PRICES_PATH` points at (written by the service's ninja
/// refresh). Missing/invalid file = empty map = pure heuristic.
fn live_prices() -> &'static std::collections::HashMap<String, f64> {
    static PRICES: std::sync::OnceLock<std::collections::HashMap<String, f64>> =
        std::sync::OnceLock::new();
    PRICES.get_or_init(|| {
        std::env::var_os("MOSSRAVEN_PRICES_PATH")
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    })
}

/// Line 2 of the item text (after "Rarity: X") — the display name.
fn item_display_name(text: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    if first.starts_with("Rarity:") {
        lines.next().map(String::from)
    } else {
        Some(first.to_string())
    }
}

/// Crate-internal re-export for feature extraction (§3.7) — same walk, no
/// duplicate parser.
pub(crate) fn equipped_items_for_features(xml: &str) -> Vec<(String, String)> {
    equipped_items(xml)
}

/// (slot name, raw item text) for every populated slot of the ACTIVE item
/// set. Flasks/charms included — they're gear with prices too.
fn equipped_items(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(items_start) = xml.find("<Items ") else {
        return out;
    };
    let items_close = xml.find("</Items>").unwrap_or(xml.len());
    let items_tag_end = items_start + xml[items_start..].find('>').unwrap_or(0);
    let items_tag = &xml[items_start..items_tag_end];
    let attr = |tag: &str, a: &str| -> Option<String> {
        let needle = format!("{a}=\"");
        let i = tag.find(&needle)?;
        let st = i + needle.len();
        let e = tag[st..].find('"')?;
        Some(tag[st..st + e].to_string())
    };
    let active_set = attr(items_tag, "activeItemSet").unwrap_or_else(|| "1".into());

    // Collect item bodies by id.
    let mut items: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut at = items_start;
    while let Some(rel) = xml[at..items_close].find("<Item id=\"") {
        let id_start = at + rel + "<Item id=\"".len();
        let Some(idq) = xml[id_start..].find('"') else { break };
        let id = xml[id_start..id_start + idq].to_string();
        let Some(body_rel) = xml[id_start..].find('>') else { break };
        let body_start = id_start + body_rel + 1;
        let Some(end_rel) = xml[body_start..].find("</Item>") else { break };
        items.insert(id, xml[body_start..body_start + end_rel].to_string());
        at = body_start + end_rel;
    }

    // Walk the active set's slots.
    let set_needle = format!("id=\"{active_set}\"");
    let mut search = items_start;
    while let Some(rel) = xml[search..items_close].find("<ItemSet ") {
        let tag_start = search + rel;
        let tag_end = tag_start + xml[tag_start..].find('>').unwrap_or(0);
        if xml[tag_start..tag_end].contains(&set_needle) {
            let set_end = tag_start
                + xml[tag_start..]
                    .find("</ItemSet>")
                    .unwrap_or(items_close - tag_start);
            let mut s2 = tag_start;
            while let Some(r2) = xml[s2..set_end].find("<Slot ") {
                let t2 = s2 + r2;
                let Some(e2_rel) = xml[t2..].find('>') else { break };
                let e2 = t2 + e2_rel;
                let tag = &xml[t2..e2];
                if let (Some(name), Some(id)) = (attr(tag, "name"), attr(tag, "itemId")) {
                    if id != "0" {
                        if let Some(body) = items.get(&id) {
                            out.push((name, body.clone()));
                        }
                    }
                }
                s2 = e2;
            }
            break;
        }
        search = tag_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"<Build><Items activeItemSet="1">
		<Item id="1">
			Rarity: RARE
Doom Spiral
Lunar Amulet
Item Level: 82
+30 to maximum Energy Shield
20% increased Cast Speed
+25% to Cold Resistance
30% increased Spell Damage
+40 to maximum Life
		</Item>
		<Item id="2">
			Rarity: UNIQUE
Temporalis
Silk Robe
Skills have -2 seconds to Cooldown
		</Item>
		<Item id="3">
			Rarity: UNIQUE
Plainstick
Ashen Staff
(60-80)% increased Spell Damage
		</Item>
		<ItemSet useSecondWeaponSet="false" id="1">
			<Slot itemId="1" name="Amulet"/>
			<Slot itemId="2" name="Body Armour"/>
			<Slot itemId="3" name="Weapon 1"/>
			<Slot itemId="0" name="Ring 1"/>
		</ItemSet>
	</Items></Build>"#;

    #[test]
    fn prices_and_bands() {
        let est = estimate_cost(MINI);
        // 5-mod rare = 5, chase unique = 100, plain unique = 5 → 110 div.
        assert_eq!(est.total_div, 110.0, "{:?}", est.breakdown);
        assert!(est.band.starts_with("expensive"), "{}", est.band);
        assert_eq!(est.breakdown[0].0, "Temporalis");
        assert_eq!(est.breakdown[0].1, 100.0);
    }

    #[test]
    fn empty_slots_and_missing_items_dont_count() {
        let est = estimate_cost(MINI);
        assert_eq!(est.breakdown.len(), 3, "{:?}", est.breakdown);
    }

    #[test]
    fn no_items_section_is_zero_budget() {
        let est = estimate_cost("<Build></Build>");
        assert_eq!(est.total_div, 0.0);
        assert!(est.band.starts_with("budget"));
    }

    #[test]
    fn band_thresholds() {
        assert!(cost_band(4.9).starts_with("budget"));
        assert!(cost_band(20.0).starts_with("mid"));
        assert!(cost_band(100.0).starts_with("expensive"));
        assert!(cost_band(500.0).starts_with("mirror"));
    }
}
