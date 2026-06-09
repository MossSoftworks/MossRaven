//! XML-side mutation applier.
//!
//! Takes a PoB2 seed XML and a list of [`MutationOp`]s and produces a new XML
//! with the ops applied in order. Until this module landed, every survivor in
//! a generation shared the seed's XML byte-for-byte, so Tier 3 produced
//! identical BuildStats for every variant — the MAP-Elites archive filled
//! with the same build under different cell labels.
//!
//! v1 ops touch `<Gem ...>` element attributes only:
//! - `SetGemLevel { gem, level }`   — rewrites `level="N"`
//! - `SetGemQuality { gem, quality }` — rewrites `quality="N"`
//! - `SwapGem { old, new }`         — rewrites `nameSpec="OLD"` → `nameSpec="NEW"`
//!
//! `gem` (or `old`) is matched against the `nameSpec` attribute on a `<Gem>`
//! element. The special value `"*"` matches the FIRST `<Gem>` in the document
//! (used by the MockSurrogate to produce diverse variants without knowing the
//! seed's gem names).
//!
//! ## Known v1 limitations
//!
//! `SwapGem` only rewrites the `nameSpec` attribute. PoB's actual gem lookup
//! uses `skillId` / `gemId` / `variantId`, so a swap won't really change the
//! scored skill. The level/quality ops *do* change the score: PoB rescales
//! base damage and support effect by level/quality. v2 will route swaps through
//! the PoB Lua API so they affect the build for real.

use mossraven_surrogate::MutationOp;

/// Apply ops to `seed_xml` in order, returning the mutated XML.
///
/// If an op references a gem that doesn't exist in the XML, that op is silently
/// skipped — surrogate hallucinations don't break the cascade, they just yield
/// a variant that scores identical to the seed (which will fail to displace any
/// elite in MAP-Elites and be naturally pruned).
pub fn apply_ops_to_xml(seed_xml: &str, ops: &[MutationOp]) -> String {
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
                xml = rewrite_gem_attr(&xml, old, "nameSpec", new);
            }
        }
    }
    xml
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
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops);
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
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops);
        // The FIRST gem (Whirling Slash) should have quality 12.
        let line = out.lines().find(|l| l.contains("Whirling Slash")).unwrap();
        assert!(line.contains(r#"quality="12""#), "got: {line}");
        // Inspiration's quality should still be 0.
        let insp = out.lines().find(|l| l.contains("Inspiration")).unwrap();
        assert!(insp.contains(r#"quality="0""#), "got: {insp}");
    }

    #[test]
    fn swap_gem_rewrites_name_spec() {
        let ops = vec![MutationOp::SwapGem {
            old: "Inspiration".to_string(),
            new: "Frigid Bond".to_string(),
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops);
        assert!(out.contains(r#"nameSpec="Frigid Bond""#), "got: {out}");
        assert!(!out.contains(r#"nameSpec="Inspiration""#), "got: {out}");
    }

    #[test]
    fn missing_gem_is_silently_skipped() {
        // Nothing in the snippet is named "Nonexistent Gem". The XML should
        // be returned unchanged rather than panicking.
        let ops = vec![MutationOp::SetGemLevel {
            gem: "Nonexistent Gem".to_string(),
            level: 99,
        }];
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops);
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
        let out = apply_ops_to_xml(REAL_GEM_SNIPPET, &ops);
        let ws = out.lines().find(|l| l.contains("Whirling Slash")).unwrap();
        assert!(ws.contains(r#"level="7""#), "got: {ws}");
        assert!(ws.contains(r#"quality="5""#), "got: {ws}");
        assert!(out.contains(r#"nameSpec="Magnified Effect""#), "got: {out}");
    }

    #[test]
    fn insert_missing_attr() {
        let tag = r#"<Gem skillId="X" nameSpec="Foo"/>"#;
        let out = set_attr_in_tag(tag, "level", "13");
        assert!(out.contains(r#"level="13""#), "got: {out}");
        assert!(out.ends_with(r#""/>"#), "got: {out}");
    }
}
