//! XML-side mutation applier.
//!
//! Takes a PoB2 seed XML and a list of [`MutationOp`]s and produces a new XML
//! with the ops applied in order. Until this module landed, every survivor in
//! a generation shared the seed's XML byte-for-byte, so Tier 3 produced
//! identical BuildStats for every variant — the MAP-Elites archive filled
//! with the same build under different cell labels.
//!
//! Ops:
//! - `SetGemLevel { gem, level }`   — rewrites `level="N"` on a `<Gem>`
//! - `SetGemQuality { gem, quality }` — rewrites `quality="N"` on a `<Gem>`
//! - `SwapGem { old, new }`         — resolves `new` through [`GemDb`] and
//!   rewrites `gemId` + `skillId` + `variantId` + `nameSpec` together, so PoB
//!   actually scores the new skill (PoB resolves by gemId, then skillId; the
//!   display name alone was the documented v1 no-op)
//! - `RemoveGem { gem }`            — deletes the gem element from the scored
//!   socket group (PoE2 supports are binary → genuine score change)
//! - `AddSupportGem { gem }`        — synthesizes a full `<Gem>` element from
//!   [`GemDb`] and inserts it into the scored socket group
//! - `SetActiveWeaponSet { use_second }` — rewrites `useSecondWeaponSet` on the
//!   active `<ItemSet>` (PoE2 clear-vs-boss weapon-set swap, SPEC §1.1)
//!
//! `gem` (or `old`) is matched against the `nameSpec` attribute on a `<Gem>`
//! element. The special value `"*"` matches the FIRST `<Gem>` in the document
//! (used by the MockSurrogate to produce diverse variants without knowing the
//! seed's gem names).
//!
//! Ops that resolve through the gem db (`SwapGem`/`AddSupportGem`) are skipped
//! with a WARN when the name isn't in PoB's data — surrogate hallucinations
//! don't break the cascade, the variant just scores identical to the seed and
//! gets out-competed in MAP-Elites.

use mossraven_pob::GemDb;
use mossraven_surrogate::{main_socket_group_span, MutationOp};

/// Apply ops to `seed_xml` in order, returning the mutated XML.
pub fn apply_ops_to_xml(seed_xml: &str, ops: &[MutationOp], gem_db: &GemDb) -> String {
    let mut xml = seed_xml.to_string();
    for op in ops {
        match op {
            MutationOp::SetGemLevel { gem, level } => {
                xml = rewrite_gem_attr(&xml, gem, "level", &level.to_string());
            }
            MutationOp::SetGemQuality { gem, quality } => {
                xml = rewrite_gem_attr(&xml, gem, "quality", &quality.to_string());
            }
            MutationOp::SwapGem { old, new } => {
                xml = swap_gem_real(&xml, old, new, gem_db);
            }
            MutationOp::RemoveGem { gem } => {
                xml = remove_gem_in_main_group(&xml, gem);
            }
            MutationOp::AddSupportGem { gem } => {
                xml = add_support_gem_to_main_group(&xml, gem, gem_db);
            }
            MutationOp::SetActiveWeaponSet { use_second } => {
                xml = set_active_weapon_set(&xml, *use_second);
            }
        }
    }
    // The `<Skills ... defaultGemLevel="normalMaximum" defaultGemQuality="0">`
    // container overrides every individual `<Gem level="X" quality="Y">` we
    // just wrote — PoB scores from the defaults, not the per-gem attrs, so
    // our mutations produce zero score change. Force defaults to `custom` so
    // PoB actually reads each gem's level/quality.
    if !ops.is_empty() {
        xml = set_skills_container_attr(&xml, "defaultGemLevel", "custom");
        xml = set_skills_container_attr(&xml, "defaultGemQuality", "custom");
    }
    xml
}

/// Swap a gem for real: resolve the replacement through PoB's gem data and
/// rewrite every identity attribute PoB resolves by. `ProcessSocketGroup`
/// prefers `gemId`, falls back to `skillId`, and normalizes `nameSpec` from
/// whichever hits — stale ids would silently keep scoring the OLD skill.
fn swap_gem_real(xml: &str, old: &str, new: &str, gem_db: &GemDb) -> String {
    let Some(info) = gem_db.get(new) else {
        tracing::warn!(old, new, "swap_gem: replacement not in PoB gem data; op skipped");
        return xml.to_string();
    };
    let Some((start, end)) = find_gem_tag(xml, old) else {
        tracing::warn!(old, "swap_gem: source gem not in build XML; op skipped");
        return xml.to_string();
    };
    let mut tag = xml[start..end].to_string();
    tag = set_attr_in_tag(&tag, "nameSpec", &info.name);
    tag = set_attr_in_tag(&tag, "gemId", &info.game_id);
    tag = set_attr_in_tag(&tag, "skillId", &info.granted_effect_id);
    tag = set_attr_in_tag(&tag, "variantId", &info.variant_id);
    let mut out = String::with_capacity(xml.len() + 64);
    out.push_str(&xml[..start]);
    out.push_str(&tag);
    out.push_str(&xml[end..]);
    out
}

/// Delete a `<Gem>` element from the scored socket group. Constrained to the
/// main group so "remove Swift Affliction" doesn't nuke a same-named gem in a
/// utility group PoB doesn't score.
fn remove_gem_in_main_group(xml: &str, gem: &str) -> String {
    let (g_start, g_end) = match main_socket_group_span(xml) {
        Some(r) => r,
        None => (0, xml.len()),
    };
    let group = &xml[g_start..g_end];
    let Some((rel_start, rel_end)) = find_gem_tag(group, gem) else {
        tracing::warn!(gem, "remove_gem: not present in scored group; op skipped");
        return xml.to_string();
    };
    // Expand to consume the line's leading whitespace + trailing newline so we
    // don't leave a blank line behind.
    let abs_start = g_start + rel_start;
    let abs_end = g_start + rel_end;
    let line_start = xml[..abs_start].rfind('\n').map(|i| i + 1).unwrap_or(abs_start);
    let cut_start = if xml[line_start..abs_start].trim().is_empty() {
        line_start
    } else {
        abs_start
    };
    let cut_end = if xml[abs_end..].starts_with('\n') {
        abs_end + 1
    } else if xml[abs_end..].starts_with("\r\n") {
        abs_end + 2
    } else {
        abs_end
    };
    let mut out = String::with_capacity(xml.len());
    out.push_str(&xml[..cut_start]);
    out.push_str(&xml[cut_end..]);
    out
}

/// Insert a support gem (full element synthesized from PoB gem data) at the
/// end of the scored socket group.
fn add_support_gem_to_main_group(xml: &str, gem: &str, gem_db: &GemDb) -> String {
    let Some(info) = gem_db.get(gem) else {
        tracing::warn!(gem, "add_support_gem: not in PoB gem data; op skipped");
        return xml.to_string();
    };
    let Some((g_start, g_end)) = main_socket_group_span(xml) else {
        tracing::warn!("add_support_gem: no scored group found; op skipped");
        return xml.to_string();
    };
    let group = &xml[g_start..g_end];
    let Some(close_rel) = group.rfind("</Skill>") else {
        return xml.to_string();
    };
    let insert_at = g_start + close_rel;
    let element = format!(
        "\t<Gem enableGlobal2=\"true\" level=\"1\" enableGlobal1=\"true\" variantId=\"{}\" skillId=\"{}\" quality=\"0\" gemId=\"{}\" nameSpec=\"{}\" enabled=\"true\" count=\"1\"/>\n\t\t\t\t",
        info.variant_id, info.granted_effect_id, info.game_id, info.name
    );
    let mut out = String::with_capacity(xml.len() + element.len());
    out.push_str(&xml[..insert_at]);
    out.push_str(&element);
    out.push_str(&xml[insert_at..]);
    out
}

/// Rewrite a single attribute on the `<Skills ...>` opening tag (not
/// `<SkillSet>`, not the `</Skills>` closer). Used to flip the build's
/// defaultGemLevel / defaultGemQuality from "normalMaximum" / "0" to
/// "custom" so PoB respects per-gem level/quality attributes.
fn set_skills_container_attr(xml: &str, attr: &str, new_value: &str) -> String {
    // Find "<Skills " specifically (not "<SkillSet" which also starts with <Skills).
    // Match "<Skills " (with trailing space) or "<Skills>".
    let Some(start) = xml.find("<Skills ").or_else(|| xml.find("<Skills>")) else {
        return xml.to_string();
    };
    let after_open = start + "<Skills".len();
    let Some(close_offset) = xml[after_open..].find('>') else {
        return xml.to_string();
    };
    let close = after_open + close_offset + 1;
    let tag = &xml[start..close];
    let rewritten = set_attr_in_tag(tag, attr, new_value);
    let mut out = String::with_capacity(xml.len() + new_value.len() + 8);
    out.push_str(&xml[..start]);
    out.push_str(&rewritten);
    out.push_str(&xml[close..]);
    out
}

/// Find the first `<Gem ...>` element matching `gem_name_spec` (or any gem if
/// `gem_name_spec == "*"`), then set or add `attr="new_value"` on it.
fn rewrite_gem_attr(xml: &str, gem_name_spec: &str, attr: &str, new_value: &str) -> String {
    // Find the target <Gem ...> tag range [start, end) where start = '<Gem'
    // and end is the index of the closing '>'.
    let Some(tag_range) = find_gem_tag(xml, gem_name_spec) else {
        return xml.to_string();
    };

    let head = &xml[..tag_range.0];
    let tag = &xml[tag_range.0..tag_range.1];
    let tail = &xml[tag_range.1..];

    let rewritten_tag = set_attr_in_tag(tag, attr, new_value);

    let mut out = String::with_capacity(xml.len() + new_value.len() + 8);
    out.push_str(head);
    out.push_str(&rewritten_tag);
    out.push_str(tail);
    out
}

/// Locate `<Gem ...>` such that the `nameSpec` attribute equals `name_spec`
/// (case-sensitive, exact). Returns the byte range `[tag_start, tag_close_inclusive)`
/// — `tag_close_inclusive` points just *past* the closing `>`.
///
/// `name_spec == "*"` matches the very first `<Gem` tag in the document.
fn find_gem_tag(xml: &str, name_spec: &str) -> Option<(usize, usize)> {
    let mut cursor = 0;
    while let Some(rel) = xml[cursor..].find("<Gem") {
        let tag_start = cursor + rel;
        let after_open = tag_start + 4; // past "<Gem"

        // Find the end of this opening tag. We don't need full XML correctness:
        // PoB's gem tags are always self-closing on one line, and `>` doesn't
        // appear inside attribute values for any gem we've ever seen.
        let close = match xml[after_open..].find('>') {
            Some(off) => after_open + off + 1,
            None => return None,
        };

        let tag = &xml[tag_start..close];

        if name_spec == "*" {
            return Some((tag_start, close));
        }

        if let Some(found) = get_attr_value(tag, "nameSpec") {
            if found == name_spec {
                return Some((tag_start, close));
            }
        }

        cursor = close;
    }
    None
}

/// Set `useSecondWeaponSet` on the ACTIVE `<ItemSet>` — the one whose `id`
/// matches the `activeItemSet` attribute on the `<Items>` container — falling
/// back to the first `<ItemSet>` when the active id can't be resolved. PoB2
/// stores the flag as the strings `"true"` / `"nil"`.
fn set_active_weapon_set(xml: &str, use_second: bool) -> String {
    let value = if use_second { "true" } else { "nil" };

    // Resolve the active item-set id from `<Items activeItemSet="N" ...>`.
    let active_id: Option<String> = xml.find("<Items").and_then(|start| {
        let close = xml[start..].find('>')? + start + 1;
        get_attr_value(&xml[start..close], "activeItemSet").map(String::from)
    });

    // Walk every `<ItemSet ...>` opening tag; prefer the id match, fall back
    // to the first one seen.
    let mut cursor = 0;
    let mut target: Option<(usize, usize)> = None;
    while let Some(rel) = xml[cursor..].find("<ItemSet") {
        let tag_start = cursor + rel;
        let Some(close_rel) = xml[tag_start..].find('>') else {
            break;
        };
        let close = tag_start + close_rel + 1;
        let tag = &xml[tag_start..close];
        if target.is_none() {
            target = Some((tag_start, close));
        }
        if let (Some(want), Some(have)) = (&active_id, get_attr_value(tag, "id")) {
            if want == have {
                target = Some((tag_start, close));
                break;
            }
        }
        cursor = close;
    }

    let Some((start, close)) = target else {
        return xml.to_string();
    };
    let rewritten = set_attr_in_tag(&xml[start..close], "useSecondWeaponSet", value);
    let mut out = String::with_capacity(xml.len() + 8);
    out.push_str(&xml[..start]);
    out.push_str(&rewritten);
    out.push_str(&xml[close..]);
    out
}

/// Return the value of `attr="..."` inside a single tag, or None if missing.
fn get_attr_value<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("{attr}=\"");
    let idx = tag.find(&needle)?;
    let start = idx + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Set or insert `attr="new_value"` in a tag string.
///
/// - If `attr` already exists, replaces its value.
/// - If `attr` is missing, inserts ` attr="new_value"` just before the closing `>` (or `/>`).
fn set_attr_in_tag(tag: &str, attr: &str, new_value: &str) -> String {
    let needle = format!("{attr}=\"");
    if let Some(idx) = tag.find(&needle) {
        let val_start = idx + needle.len();
        let rest = &tag[val_start..];
        let Some(val_end_rel) = rest.find('"') else {
            return tag.to_string();
        };
        let val_end = val_start + val_end_rel;
        let mut out = String::with_capacity(tag.len() + new_value.len());
        out.push_str(&tag[..val_start]);
        out.push_str(new_value);
        out.push_str(&tag[val_end..]);
        return out;
    }

    // Insert before the closing `>` or `/>`.
    let close_idx = if let Some(idx) = tag.rfind("/>") {
        idx
    } else {
        match tag.rfind('>') {
            Some(i) => i,
            None => return tag.to_string(),
        }
    };
    let mut out = String::with_capacity(tag.len() + attr.len() + new_value.len() + 4);
    out.push_str(&tag[..close_idx]);
    out.push(' ');
    out.push_str(attr);
    out.push_str("=\"");
    out.push_str(new_value);
    out.push('"');
    out.push_str(&tag[close_idx..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal gem db for swap/add tests — parsed from the same Lua shape as
    /// the vendored Gems.lua.
    fn db() -> GemDb {
        GemDb::parse(
            r#"
return {
	["Metadata/Items/Gems/SkillGemSpark"] = {
		name = "Spark",
		gameId = "Metadata/Items/Gems/SkillGemSpark",
		variantId = "Spark",
		grantedEffectId = "SparkPlayer",
		tags = {
			spell = true,
			lightning = true,
		},
		gemType = "Spell",
	},
	["Metadata/Items/Gems/SupportGemMagnifiedEffect"] = {
		name = "Magnified Effect",
		gameId = "Metadata/Items/Gems/SupportGemMagnifiedEffect",
		variantId = "MagnifiedEffect",
		grantedEffectId = "SupportMagnifiedEffectPlayer",
		tags = {
		},
		gemType = "Support",
	},
}
"#,
        )
    }

    const REAL_GEM_SNIPPET: &str = r#"<Skill>
<Gem enableGlobal2="true" level="20" enableGlobal1="true" variantId="WhirlingSlash" skillId="WhirlingSlashPlayer" quality="0" gemId="Metadata/Items/Gems/SkillGemWhirlingSlash" nameSpec="Whirling Slash" enabled="true" count="1"/>
<Gem enableGlobal2="true" level="1" enableGlobal1="true" variantId="InspirationSupport" skillId="SupportInspirationPlayer" quality="0" gemId="Metadata/Items/Gems/SupportGemInspiration" nameSpec="Inspiration" enabled="true" count="1"/>
</Skill>"#;

    #[test]
    fn set_gem_level_by_name_spec() {
        let ops = vec![MutationOp::SetGemLevel {
            gem: "Whirling Slash".to_string(),
            level: 15,
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops, &db());
        // The Whirling Slash gem's level was 20; should now be 15.
        assert!(out.contains(r#"nameSpec="Whirling Slash""#));
        // The substring containing this gem's level should be 15.
        let line = out.lines().find(|l| l.contains("Whirling Slash")).unwrap();
        assert!(line.contains(r#"level="15""#), "got: {line}");
        // Inspiration's level should still be 1.
        let insp = out.lines().find(|l| l.contains("Inspiration")).unwrap();
        assert!(insp.contains(r#"level="1""#), "got: {insp}");
    }

    #[test]
    fn set_gem_quality_by_wildcard() {
        let ops = vec![MutationOp::SetGemQuality {
            gem: "*".to_string(),
            quality: 12,
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops, &db());
        // The FIRST gem (Whirling Slash) should have quality 12.
        let line = out.lines().find(|l| l.contains("Whirling Slash")).unwrap();
        assert!(line.contains(r#"quality="12""#), "got: {line}");
        // Inspiration's quality should still be 0.
        let insp = out.lines().find(|l| l.contains("Inspiration")).unwrap();
        assert!(insp.contains(r#"quality="0""#), "got: {insp}");
    }

    #[test]
    fn swap_gem_rewrites_all_pob_identity_attrs() {
        // PoB resolves by gemId → skillId → nameSpec; a real swap must move
        // all of them or the OLD skill keeps being scored.
        let ops = vec![MutationOp::SwapGem {
            old: "Whirling Slash".to_string(),
            new: "Spark".to_string(),
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops, &db());
        let line = out.lines().find(|l| l.contains(r#"nameSpec="Spark""#)).expect("swapped gem");
        assert!(line.contains(r#"gemId="Metadata/Items/Gems/SkillGemSpark""#), "got: {line}");
        assert!(line.contains(r#"skillId="SparkPlayer""#), "got: {line}");
        assert!(line.contains(r#"variantId="Spark""#), "got: {line}");
        assert!(!out.contains(r#"nameSpec="Whirling Slash""#), "got: {out}");
    }

    #[test]
    fn swap_gem_to_unknown_gem_is_skipped() {
        // "Frigid Bond" is not in the test db — the op must no-op rather than
        // write a half-identity gem PoB can't resolve.
        let ops = vec![MutationOp::SwapGem {
            old: "Inspiration".to_string(),
            new: "Frigid Bond".to_string(),
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops, &db());
        assert!(out.contains(r#"nameSpec="Inspiration""#), "swap must be skipped: {out}");
        assert!(!out.contains("Frigid Bond"), "got: {out}");
    }

    #[test]
    fn missing_gem_is_silently_skipped() {
        // Nothing in the snippet is named "Nonexistent Gem". The XML should
        // be returned unchanged rather than panicking.
        let ops = vec![MutationOp::SetGemLevel {
            gem: "Nonexistent Gem".to_string(),
            level: 99,
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops, &db());
        assert_eq!(out, REAL_GEM_SNIPPET);
    }

    #[test]
    fn multiple_ops_applied_in_order() {
        let ops = vec![
            MutationOp::SetGemLevel {
                gem: "Whirling Slash".to_string(),
                level: 7,
            },
            MutationOp::SetGemQuality {
                gem: "Whirling Slash".to_string(),
                quality: 5,
            },
            MutationOp::SwapGem {
                old: "Inspiration".to_string(),
                new: "Magnified Effect".to_string(),
            },
        ];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops, &db());
        let ws = out.lines().find(|l| l.contains("Whirling Slash")).unwrap();
        assert!(ws.contains(r#"level="7""#), "got: {ws}");
        assert!(ws.contains(r#"quality="5""#), "got: {ws}");
        assert!(out.contains(r#"nameSpec="Magnified Effect""#), "got: {out}");
    }

    #[test]
    fn skills_container_defaults_flipped_to_custom() {
        let xml = r#"<PathOfBuilding2>
<Skills sortGemsByDPSField="CombinedDPS" activeSkillSet="1" sortGemsByDPS="true" defaultGemQuality="0" defaultGemLevel="normalMaximum" showSupportGemTypes="ALL">
<SkillSet id="1">
<Skill>
<Gem level="20" nameSpec="Whirling Slash" quality="0"/>
</Skill>
</SkillSet>
</Skills>
</PathOfBuilding2>"#;
        let ops = vec![MutationOp::SetGemLevel {
            gem: "Whirling Slash".to_string(),
            level: 5,
        }];
        let out = apply_ops_to_xml(xml, &ops, &db());
        assert!(out.contains(r#"defaultGemLevel="custom""#), "got: {out}");
        assert!(out.contains(r#"defaultGemQuality="custom""#), "got: {out}");
        assert!(out.contains(r#"level="5""#), "got: {out}");
        // Sanity: SkillSet is left alone.
        assert!(out.contains(r#"<SkillSet id="1">"#), "got: {out}");
    }

    #[test]
    fn no_ops_leaves_skills_container_intact() {
        let xml = r#"<Skills defaultGemLevel="normalMaximum" defaultGemQuality="0"><Gem nameSpec="X" level="1"/></Skills>"#;
        let out = apply_ops_to_xml(xml, &[], &db());
        assert_eq!(out, xml);
    }

    #[test]
    fn insert_missing_attr() {
        let tag = r#"<Gem skillId="X" nameSpec="Foo"/>"#;
        let out = set_attr_in_tag(tag, "level", "13");
        assert!(out.contains(r#"level="13""#), "got: {out}");
        assert!(out.ends_with(r#""/>"#), "got: {out}");
    }

    /// Build-shaped snippet with a scored group (mainSocketGroup=2) so the
    /// group-constrained ops (remove/add) have something to aim at.
    const GROUPED_SNIPPET: &str = r#"<PathOfBuilding2>
<Build level="90" className="Druid" mainSocketGroup="2" viewMode="IMPORT">
</Build>
<Skills activeSkillSet="1" defaultGemQuality="0" defaultGemLevel="normalMaximum">
<SkillSet id="1">
<Skill mainActiveSkill="1" enabled="true">
<Gem level="20" nameSpec="Frost Bomb" gemId="Metadata/X" skillId="FrostBombPlayer"/>
</Skill>
<Skill mainActiveSkill="1" enabled="true">
<Gem level="20" nameSpec="Tornado" gemId="Metadata/Items/Gems/SkillGemTornado" skillId="TornadoPlayer"/>
<Gem level="1" nameSpec="Swift Affliction II" gemId="Metadata/Y" skillId="SupportSwiftAfflictionPlayer"/>
</Skill>
</SkillSet>
</Skills>
</PathOfBuilding2>"#;

    #[test]
    fn remove_gem_only_touches_scored_group() {
        let ops = vec![MutationOp::RemoveGem {
            gem: "Swift Affliction II".to_string(),
        }];
        let out = apply_ops_to_xml(GROUPED_SNIPPET, &ops, &db());
        assert!(!out.contains("Swift Affliction II"), "support removed: {out}");
        assert!(out.contains(r#"nameSpec="Tornado""#), "main skill intact");
        assert!(out.contains(r#"nameSpec="Frost Bomb""#), "other group intact");
    }

    #[test]
    fn remove_gem_not_in_scored_group_is_skipped() {
        // Frost Bomb lives in group 1; the scored group is 2 — must not be
        // removed even though it exists in the document.
        let ops = vec![MutationOp::RemoveGem {
            gem: "Frost Bomb".to_string(),
        }];
        let out = apply_ops_to_xml(GROUPED_SNIPPET, &ops, &db());
        assert!(out.contains(r#"nameSpec="Frost Bomb""#), "got: {out}");
    }

    #[test]
    fn add_support_gem_lands_in_scored_group() {
        let ops = vec![MutationOp::AddSupportGem {
            gem: "Magnified Effect".to_string(),
        }];
        let out = apply_ops_to_xml(GROUPED_SNIPPET, &ops, &db());
        let added = out
            .lines()
            .find(|l| l.contains(r#"nameSpec="Magnified Effect""#))
            .expect("support added");
        assert!(
            added.contains(r#"gemId="Metadata/Items/Gems/SupportGemMagnifiedEffect""#),
            "full identity synthesized: {added}"
        );
        // Inserted into group 2 (after Tornado), not group 1: it must appear
        // AFTER the Tornado line in document order.
        let tornado_pos = out.find(r#"nameSpec="Tornado""#).unwrap();
        let added_pos = out.find(r#"nameSpec="Magnified Effect""#).unwrap();
        assert!(added_pos > tornado_pos, "added into scored group: {out}");
    }

    const ITEM_SETS_SNIPPET: &str = r#"<Items activeItemSet="2" useSecondWeaponSet="nil">
<ItemSet useSecondWeaponSet="nil" id="1">
<Slot name="Weapon 1" itemId="1"/>
</ItemSet>
<ItemSet useSecondWeaponSet="nil" id="2">
<Slot name="Weapon 1" itemId="2"/>
</ItemSet>
</Items>"#;

    #[test]
    fn weapon_set_flips_on_active_item_set_only() {
        let ops = vec![MutationOp::SetActiveWeaponSet { use_second: true }];
        let out = apply_ops_to_xml(ITEM_SETS_SNIPPET, &ops, &db());
        // ItemSet id=2 is active — it gets the flag.
        let set2 = out
            .lines()
            .find(|l| l.contains(r#"id="2""#))
            .expect("ItemSet 2 present");
        assert!(set2.contains(r#"useSecondWeaponSet="true""#), "got: {set2}");
        // ItemSet id=1 is untouched.
        let set1 = out
            .lines()
            .find(|l| l.contains(r#"id="1""#))
            .expect("ItemSet 1 present");
        assert!(set1.contains(r#"useSecondWeaponSet="nil""#), "got: {set1}");
    }

    #[test]
    fn weapon_set_falls_back_to_first_item_set() {
        let xml = r#"<ItemSet useSecondWeaponSet="nil" id="7"><Slot/></ItemSet>"#;
        let ops = vec![MutationOp::SetActiveWeaponSet { use_second: true }];
        let out = apply_ops_to_xml(xml, &ops, &db());
        assert!(out.contains(r#"useSecondWeaponSet="true""#), "got: {out}");
    }

    #[test]
    fn weapon_set_false_writes_nil() {
        let xml = r#"<Items activeItemSet="1"><ItemSet useSecondWeaponSet="true" id="1"/></Items>"#;
        let ops = vec![MutationOp::SetActiveWeaponSet { use_second: false }];
        let out = apply_ops_to_xml(xml, &ops, &db());
        assert!(out.contains(r#"useSecondWeaponSet="nil""#), "got: {out}");
    }

    #[test]
    fn weapon_set_no_item_set_is_noop() {
        let xml = r#"<Skills><Gem nameSpec="X" level="1"/></Skills>"#;
        let ops = vec![MutationOp::SetActiveWeaponSet { use_second: true }];
        let out = apply_ops_to_xml(xml, &ops, &db());
        // No <ItemSet> anywhere — XML unchanged except the Skills-defaults
        // flip that any non-empty ops list triggers.
        assert!(!out.contains("useSecondWeaponSet"), "got: {out}");
    }
}
