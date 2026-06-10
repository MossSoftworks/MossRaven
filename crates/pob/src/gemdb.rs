//! Gem database parsed from the vendored PoB2 `Data/Gems.lua`.
//!
//! Two consumers (handoff item [2]):
//! - **Real `swap_gem`**: PoB resolves gems by `gemId`/`skillId`, not the
//!   display `nameSpec` — a swap must rewrite all of them with values from
//!   the actual game data or the scored skill never changes.
//! - **Honest cell labels**: each gem entry's `tags` block carries the
//!   damage identity (`fire`/`cold`/`lightning`/`chaos`/`physical`/`minion`)
//!   straight from GGG's data — far more reliable than the surrogate's guess.
//!
//! The file is ~1 MB of mechanically-generated Lua with a rigid shape, so a
//! line-oriented text scan is sufficient — no Lua VM required. Loaded once at
//! service startup; lookups are by lowercased display name.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct GemInfo {
    pub name: String,
    pub game_id: String,
    pub variant_id: String,
    pub granted_effect_id: String,
    /// "Spell" / "Attack" / "Support" / "Buff" / ...
    pub gem_type: String,
    /// Damage-identity tags present on the gem: fire, cold, lightning,
    /// chaos, physical, minion (subset of the full tag set).
    pub damage_tags: Vec<String>,
}

impl GemInfo {
    pub fn is_support(&self) -> bool {
        self.gem_type.eq_ignore_ascii_case("support")
    }

    /// MAP-Elites `damage_type` axis value for this gem, from game data.
    /// Minion beats element (a minion gem's element describes the minion's
    /// damage, but the build identity is "minion"). Multiple elements →
    /// first of the fixed precedence order; none → physical if tagged, else
    /// None (caller falls back to the surrogate hint).
    pub fn damage_type(&self) -> Option<&'static str> {
        let has = |t: &str| self.damage_tags.iter().any(|x| x == t);
        if has("minion") {
            return Some("minion");
        }
        for t in ["chaos", "lightning", "cold", "fire"] {
            if has(t) {
                return Some(match t {
                    "chaos" => "chaos",
                    "lightning" => "lightning",
                    "cold" => "cold",
                    _ => "fire",
                });
            }
        }
        if has("physical") {
            return Some("physical");
        }
        None
    }
}

#[derive(Debug, Default)]
pub struct GemDb {
    by_name: HashMap<String, GemInfo>,
}

impl GemDb {
    /// Parse `<pob_path>/src/Data/Gems.lua`. Returns an empty db (lookups
    /// all-miss, swaps no-op with a warning) when the file is absent — the
    /// cascade still runs, it just can't do real swaps or truth-labels.
    pub fn load(pob_path: &Path) -> Self {
        let path = pob_path.join("src/Data/Gems.lua");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(path = ?path, error = %e, "Gems.lua not readable; gem db empty");
                return Self::default();
            }
        };
        let db = Self::parse(&text);
        tracing::info!(gems = db.by_name.len(), "gem db loaded from Gems.lua");
        db
    }

    pub fn parse(text: &str) -> Self {
        let mut by_name: HashMap<String, GemInfo> = HashMap::new();
        let mut cur = GemInfo::default();
        let mut in_tags = false;

        let field = |line: &str, key: &str| -> Option<String> {
            let rest = line.trim().strip_prefix(key)?.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let rest = rest.strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_string())
        };

        for line in text.lines() {
            let t = line.trim();
            if in_tags {
                if t.starts_with('}') {
                    in_tags = false;
                } else if let Some(tag) = t.split('=').next().map(str::trim) {
                    if matches!(tag, "fire" | "cold" | "lightning" | "chaos" | "physical" | "minion") {
                        cur.damage_tags.push(tag.to_string());
                    }
                }
                continue;
            }
            if let Some(v) = field(t, "name") {
                // `name` opens a new entry; flush the previous one.
                if !cur.name.is_empty() && !cur.game_id.is_empty() {
                    by_name.insert(cur.name.to_lowercase(), std::mem::take(&mut cur));
                } else {
                    cur = GemInfo::default();
                }
                cur.name = v;
            } else if let Some(v) = field(t, "gameId") {
                cur.game_id = v;
            } else if let Some(v) = field(t, "variantId") {
                cur.variant_id = v;
            } else if let Some(v) = field(t, "grantedEffectId") {
                cur.granted_effect_id = v;
            } else if let Some(v) = field(t, "gemType") {
                cur.gem_type = v;
            } else if t.starts_with("tags = {") {
                in_tags = true;
            }
        }
        if !cur.name.is_empty() && !cur.game_id.is_empty() {
            by_name.insert(cur.name.to_lowercase(), cur);
        }
        Self { by_name }
    }

    pub fn get(&self, display_name: &str) -> Option<&GemInfo> {
        self.by_name.get(&display_name.to_lowercase())
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
return {
	["Metadata/Items/Gems/SkillGemTornado"] = {
		name = "Tornado",
		baseTypeName = "Tornado",
		gameId = "Metadata/Items/Gems/SkillGemTornado",
		variantId = "Tornado",
		grantedEffectId = "TornadoPlayer",
		tags = {
			strength = true,
			spell = true,
			area = true,
			physical = true,
		},
		gemType = "Spell",
	},
	["Metadata/Items/Gems/SkillGemSpark"] = {
		name = "Spark",
		gameId = "Metadata/Items/Gems/SkillGemSpark",
		variantId = "Spark",
		grantedEffectId = "SparkPlayer",
		tags = {
			intelligence = true,
			spell = true,
			projectile = true,
			lightning = true,
		},
		gemType = "Spell",
	},
	["Metadata/Items/Gems/SupportGemSwiftAffliction"] = {
		name = "Swift Affliction",
		gameId = "Metadata/Items/Gems/SupportGemSwiftAffliction",
		variantId = "SwiftAffliction",
		grantedEffectId = "SupportSwiftAfflictionPlayer",
		tags = {
			dexterity = true,
		},
		gemType = "Support",
	},
}
"#;

    #[test]
    fn parses_entries_with_ids_and_tags() {
        let db = GemDb::parse(SAMPLE);
        assert_eq!(db.len(), 3);
        let t = db.get("Tornado").expect("tornado");
        assert_eq!(t.game_id, "Metadata/Items/Gems/SkillGemTornado");
        assert_eq!(t.granted_effect_id, "TornadoPlayer");
        assert_eq!(t.damage_type(), Some("physical"));
        let s = db.get("spark").expect("case-insensitive lookup");
        assert_eq!(s.damage_type(), Some("lightning"));
        assert!(db.get("Swift Affliction").unwrap().is_support());
    }

    #[test]
    fn real_vendor_gems_lua_parses_when_present() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/PathOfBuilding-PoE2");
        if !p.join("src/Data/Gems.lua").exists() {
            eprintln!("skipping: vendor not present");
            return;
        }
        let db = GemDb::load(&p);
        assert!(db.len() > 300, "expected hundreds of gems, got {}", db.len());
        let t = db.get("Tornado").expect("Tornado in vendor data");
        assert_eq!(t.granted_effect_id, "TornadoPlayer");
        assert_eq!(t.damage_type(), Some("physical"));
    }
}
