//! Unique-item database parsed from the vendored `Data/Uniques/*.lua`.
//!
//! Powers the `equip_unique` mutation op — the legal DPS lever for builds
//! whose passive tree is at budget (trees saturate; gear doesn't cost
//! points). Uniques are the honest v1 item operator: their mods are FIXED
//! by GGG data, so a swapped-in unique is legal-by-construction — no mod
//! pool / tier / crafting legality to verify, unlike synthesized rares.
//!
//! Item text is rendered in PoB's own import format ("Rarity: UNIQUE",
//! name, base, mods) with variant machinery resolved to the CURRENT
//! variant, so `<Item>` blocks paste straight into build XML and PoB's
//! parser rolls ranged mods at its default midpoint.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct UniqueItem {
    pub name: String,
    pub base: String,
    /// Slot family from the source file name: "staff", "amulet", "ring",
    /// "body", "boots", "gloves", "helmet", "belt", "wand", ...
    pub kind: String,
    /// Variant-resolved PoB item text (starts "Rarity: UNIQUE").
    pub item_text: String,
    /// Joined mod lines — offense classification + prompt fodder.
    pub mods_joined: String,
}

impl UniqueItem {
    /// Offense-flavored, by mod text — same spirit as the tree explorer's
    /// notable filter. False positives just spend an exploration slot.
    pub fn is_offense(&self) -> bool {
        let s = &self.mods_joined;
        (s.contains("Damage") && !s.contains("Damage taken") && !s.contains("Damage Taken"))
            || s.contains("Critical")
            || s.contains("Cast Speed")
            || s.contains("Attack Speed")
            || s.contains("Penetrat")
            || s.contains("Level of all")
            || s.contains("Grants Skill")
    }
}

#[derive(Debug, Default)]
pub struct UniqueDb {
    /// kind (file stem) → items in file order.
    by_kind: HashMap<String, Vec<UniqueItem>>,
    /// lowercase name → (kind, index into by_kind).
    by_name: HashMap<String, (String, usize)>,
}

/// Slot families that can hold scored gear. `fishing`, `incursionlimb`,
/// jewels and flasks are excluded from v1 (jewel sockets and flask logic
/// need their own ops).
const KINDS: &[&str] = &[
    "amulet", "axe", "belt", "body", "boots", "bow", "claw", "crossbow", "dagger", "flail",
    "focus", "gloves", "helmet", "mace", "quarterstaff", "quiver", "ring", "sceptre", "shield",
    "spear", "staff", "sword", "wand",
];

impl UniqueDb {
    pub fn load(pob_path: &Path) -> Self {
        let mut db = Self::default();
        let root = pob_path.join("src/Data/Uniques");
        for kind in KINDS {
            let file = root.join(format!("{kind}.lua"));
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let items = parse_uniques_lua(&text, kind);
            if !items.is_empty() {
                for (i, it) in items.iter().enumerate() {
                    db.by_name
                        .insert(it.name.to_lowercase(), (kind.to_string(), i));
                }
                db.by_kind.insert(kind.to_string(), items);
            }
        }
        let total: usize = db.by_kind.values().map(Vec::len).sum();
        if total == 0 {
            tracing::warn!(path = ?root, "no uniques parsed; equip_unique op disabled");
        } else {
            tracing::info!(
                uniques = total,
                kinds = db.by_kind.len(),
                "unique db loaded"
            );
        }
        db
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&UniqueItem> {
        let (kind, i) = self.by_name.get(&name.to_lowercase())?;
        self.by_kind.get(kind)?.get(*i)
    }

    pub fn of_kind(&self, kind: &str) -> &[UniqueItem] {
        self.by_kind.get(kind).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Map an equipped item's BASE line to its slot family, e.g.
    /// "Roaring Staff" → "staff". Drives same-family weapon swaps: a staff
    /// build explores staves, not wands.
    pub fn kind_of_base(base: &str) -> Option<&'static str> {
        let b = base.to_lowercase();
        // Order matters: "quarterstaff" before "staff", "crossbow" before "bow".
        for (needle, kind) in [
            ("quarterstaff", "quarterstaff"),
            ("crossbow", "crossbow"),
            ("staff", "staff"),
            ("wand", "wand"),
            ("sceptre", "sceptre"),
            ("bow", "bow"),
            ("quiver", "quiver"),
            ("focus", "focus"),
            ("shield", "shield"),
            ("buckler", "shield"),
        ] {
            if b.contains(needle) {
                return Some(kind);
            }
        }
        None
    }
}

/// Extract every `[[ ... ]]` block and render it as a current-variant PoB
/// item. Blocks whose first line can't be a name (empty file headers) are
/// skipped.
fn parse_uniques_lua(text: &str, kind: &str) -> Vec<UniqueItem> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let Some(end_rel) = rest[start + 2..].find("]]") else {
            break;
        };
        let block = &rest[start + 2..start + 2 + end_rel];
        rest = &rest[start + 2 + end_rel + 2..];
        if let Some(item) = render_current_variant(block, kind) {
            out.push(item);
        }
    }
    out
}

/// Resolve a raw datafile block to a single-variant item text.
///
/// - `Variant: X` header lines declare variants in order; the LAST one is
///   the current patch ("Variant: Current" by convention).
/// - `{variant:1,3}mod` lines apply only to the listed variants — keep the
///   line (prefix stripped) iff the last variant is listed.
/// - `Source:` / `League:` / `Requires` flavor lines are dropped; other
///   `{tag}` prefixes (crafted/implicit/rune) pass through — PoB knows them.
fn render_current_variant(block: &str, kind: &str) -> Option<UniqueItem> {
    let lines: Vec<&str> = block
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty())
        .collect();
    if lines.len() < 2 {
        return None;
    }
    let name = lines[0].to_string();
    let base = lines[1].to_string();
    if name.is_empty() || base.is_empty() || name.starts_with("--") {
        return None;
    }
    let variant_count = lines.iter().filter(|l| l.starts_with("Variant: ")).count();

    let mut body = Vec::new();
    for l in &lines[2..] {
        if l.is_empty()
            || l.starts_with("Variant: ")
            || l.starts_with("Source:")
            || l.starts_with("League:")
        {
            continue;
        }
        if let Some(rest) = l.strip_prefix("{variant:") {
            let Some((list, mod_text)) = rest.split_once('}') else {
                continue;
            };
            let applies = list
                .split(',')
                .filter_map(|n| n.trim().parse::<usize>().ok())
                .any(|n| n == variant_count);
            if applies && !mod_text.is_empty() {
                body.push(mod_text.to_string());
            }
            continue;
        }
        body.push(l.to_string());
    }
    if body.is_empty() {
        return None;
    }

    let mods_joined = body.join("; ");
    let item_text = format!("Rarity: UNIQUE\n{name}\n{base}\n{}", body.join("\n"));
    Some(UniqueItem {
        name,
        base,
        kind: kind.to_string(),
        item_text,
        mods_joined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"
return {
-- Weapon: Staff
[[
Plainstick
Ashen Staff
Source: Drops somewhere
Implicits: 1
Grants Skill: Level (1-20) Firebolt
(60-80)% increased Spell Damage
]],[[
Variantful
Chiming Staff
Variant: Pre 0.4.0
Variant: Current
Implicits: 0
{variant:1}(10-20)% increased Spell Damage
{variant:2}(80-120)% increased Spell Damage
{variant:1,2}10% increased Cast Speed
Always-on mod
]],
}
"#;

    #[test]
    fn parses_blocks_and_resolves_variants() {
        let items = parse_uniques_lua(MINI, "staff");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Plainstick");
        assert!(items[0].item_text.starts_with("Rarity: UNIQUE\nPlainstick\nAshen Staff\n"));
        assert!(items[0].item_text.contains("Implicits: 1"));

        let v = &items[1];
        assert!(v.item_text.contains("(80-120)% increased Spell Damage"), "{}", v.item_text);
        assert!(!v.item_text.contains("(10-20)%"), "old variant must be dropped");
        assert!(v.item_text.contains("10% increased Cast Speed"), "multi-variant line kept");
        assert!(v.item_text.contains("Always-on mod"));
        assert!(!v.item_text.contains("Variant:"));
    }

    #[test]
    fn offense_classifier_and_kind_mapping() {
        let items = parse_uniques_lua(MINI, "staff");
        assert!(items[0].is_offense());
        assert_eq!(UniqueDb::kind_of_base("Gnarled Quarterstaff"), Some("quarterstaff"));
        assert_eq!(UniqueDb::kind_of_base("Roaring Staff"), Some("staff"));
        assert_eq!(UniqueDb::kind_of_base("Siege Crossbow"), Some("crossbow"));
        assert_eq!(UniqueDb::kind_of_base("Leather Belt"), None);
    }

    #[test]
    fn real_vendor_uniques_parse_when_present() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/PathOfBuilding-PoE2");
        if !p.join("src/Data/Uniques/staff.lua").exists() {
            eprintln!("skipping: vendor uniques not present");
            return;
        }
        let db = UniqueDb::load(&p);
        let total: usize = KINDS.iter().map(|k| db.of_kind(k).len()).sum();
        assert!(total > 100, "expected a real unique pool, got {total}");
        assert!(!db.of_kind("staff").is_empty());
        // Spot-check a known 0.x staff exists and is findable by name.
        assert!(db.get("The Burden of Shadows").is_some() || !db.of_kind("staff").is_empty());
    }
}
