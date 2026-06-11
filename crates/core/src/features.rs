//! Build feature extraction for the Tier-3 value model (SPEC §3.7).
//!
//! A feature vector is everything the GBT needs to predict PoB's verdict
//! WITHOUT running PoB: identity (class/ascendancy/level), the scored skill
//! setup, the allocated tree, and the equipped gear. Pure XML parsing — no
//! Lua, microseconds per build, safe to run on every Tier-4 eval.

use serde_json::{json, Value};

/// Extract the model-facing feature object from a build XML.
pub fn extract(xml: &str) -> Value {
    let build_tag = tag_slice(xml, "<Build ");
    let attr = |tag: &str, a: &str| -> Option<String> {
        let needle = format!("{a}=\"");
        let i = tag.find(&needle)?;
        let st = i + needle.len();
        let e = tag[st..].find('"')?;
        Some(tag[st..st + e].to_string())
    };
    let (class_name, ascend_name, level) = build_tag
        .map(|t| {
            (
                attr(t, "className").unwrap_or_default(),
                attr(t, "ascendClassName").unwrap_or_default(),
                attr(t, "level").and_then(|l| l.parse::<u32>().ok()).unwrap_or(0),
            )
        })
        .unwrap_or_default();

    // Scored skill group: active gem + supports.
    let main_skill = mossraven_surrogate::find_main_skill_gem_name(xml).unwrap_or_default();
    let main_group_gems: Vec<String> = mossraven_surrogate::main_socket_group_span(xml)
        .map(|(s, e)| {
            let span = &xml[s..e];
            let mut out = Vec::new();
            let mut at = 0;
            while let Some(rel) = span[at..].find("nameSpec=\"") {
                let st = at + rel + "nameSpec=\"".len();
                if let Some(q) = span[st..].find('"') {
                    out.push(span[st..st + q].to_string());
                    at = st + q;
                } else {
                    break;
                }
            }
            out
        })
        .unwrap_or_default();
    let supports: Vec<&String> = main_group_gems.iter().filter(|g| **g != main_skill).collect();

    // Tree: allocated node ids + weapon-set subset.
    let nodes: Vec<u32> = spec_attr(xml, "nodes")
        .map(|csv| csv.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_default();
    let ws_nodes = spec_body_start(xml)
        .map(|b| crate::mutate::weapon_set_ids(xml, b))
        .unwrap_or_default();
    let tree_version = spec_attr(xml, "treeVersion").unwrap_or_else(|| "0_4".into());

    // Gear: unique names + base lines of equipped items.
    let (uniques, bases) = equipped_gear(xml);

    json!({
        "class": class_name,
        "ascendancy": ascend_name,
        "level": level,
        "main_skill": main_skill,
        "support_count": supports.len(),
        "supports": supports,
        "tree_version": tree_version,
        "node_count": nodes.len(),
        "nodes": nodes,
        "ws_node_count": ws_nodes.len(),
        "uniques": uniques,
        "bases": bases,
    })
}

fn tag_slice<'a>(xml: &'a str, open: &str) -> Option<&'a str> {
    let s = xml.find(open)?;
    let e = s + xml[s..].find('>')?;
    Some(&xml[s..e])
}

fn spec_attr(xml: &str, a: &str) -> Option<String> {
    let tag = tag_slice(xml, "<Spec ")?;
    let needle = format!("{a}=\"");
    let i = tag.find(&needle)?;
    let st = i + needle.len();
    let e = tag[st..].find('"')?;
    Some(tag[st..st + e].to_string())
}

fn spec_body_start(xml: &str) -> Option<usize> {
    let s = xml.find("<Spec ")?;
    let e = s + xml[s..].find('>')?;
    if xml[s..e].ends_with('/') {
        None
    } else {
        Some(e + 1)
    }
}

/// (unique item names, base lines) for the ACTIVE item set, reusing the cost
/// module's equipped-items walk.
fn equipped_gear(xml: &str) -> (Vec<String>, Vec<String>) {
    let mut uniques = Vec::new();
    let mut bases = Vec::new();
    for (_, text) in crate::cost::equipped_items_for_features(xml) {
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let is_unique = lines
            .first()
            .map(|l| l.eq_ignore_ascii_case("rarity: unique"))
            .unwrap_or(false);
        if is_unique {
            if let Some(name) = lines.get(1) {
                uniques.push((*name).to_string());
            }
        }
        if let Some(base) = lines.get(2).or_else(|| lines.get(1)) {
            bases.push((*base).to_string());
        }
    }
    (uniques, bases)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_identity_tree_and_gear_from_fixture() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/pob/tests/fixtures/druid-oracle-tornado.xml");
        let Ok(xml) = std::fs::read_to_string(p) else {
            eprintln!("skipping: fixture not pulled");
            return;
        };
        let f = extract(&xml);
        assert_eq!(f["class"], "Druid");
        assert_eq!(f["ascendancy"], "Oracle");
        assert_eq!(f["level"], 98);
        assert!(f["node_count"].as_u64().unwrap() > 100);
        assert_eq!(f["ws_node_count"], 48);
        assert!(!f["main_skill"].as_str().unwrap().is_empty());
        assert!(f["support_count"].as_u64().unwrap() >= 1);
        assert!(!f["bases"].as_array().unwrap().is_empty());
    }
}
