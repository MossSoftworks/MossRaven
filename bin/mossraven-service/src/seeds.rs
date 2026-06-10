//! Concept-grounded seed selection (handoff item [1], Phase A).
//!
//! The engine mutates whatever seed XML it's given — it cannot invent a class.
//! Before this module, the only default was `seed.xml` (a Huntress), so a
//! "druid" concept produced Huntress builds with mislabeled cells.
//!
//! `SeedLibrary` scans the fixtures directory at startup (fixtures are
//! user-supplied and gitignored per GGG fan-content policy, so the index is
//! built at runtime, never hardcoded) and `select()` maps a free-text concept
//! to the best-matching seed by class / ascendancy / skill keywords.
//!
//! Selection precedence at the call sites (seed_hypothesis / headless):
//!   1. `MOSSRAVEN_SEED_XML_PATH` env — explicit user override, always wins
//!   2. `hypothesis.seed_pob_xml` — Tier-1 grounding (Phase B, when live)
//!   3. `SeedLibrary::select(concept)` — this module
//!   4. bundled default seed.xml — last resort

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SeedEntry {
    pub path: PathBuf,
    /// `className` parsed from the XML `<Build>` element — ground truth.
    pub class_name: String,
    /// `ascendClassName` from the XML.
    pub ascend_name: String,
    /// Lowercased tokens from the filename (class, ascendancy, skill words).
    pub tokens: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SeedLibrary {
    pub entries: Vec<SeedEntry>,
}

/// Words that imply a class without naming it. Kept deliberately small and
/// high-precision — a fuzzy hit here beats falling back to a wrong-class seed,
/// but a wrong hit poisons the whole run.
fn class_aliases(token: &str) -> Option<&'static str> {
    Some(match token {
        // Druid (new in 0.5 "Return of the Ancients") — shapeshift archetypes.
        "wolf" | "werewolf" | "bear" | "shapeshift" | "shapeshifter" | "plant" | "plants"
        | "vine" | "vines" | "tornado" | "shaman" | "oracle" => "druid",
        // Common loose names for existing classes.
        "witchhunter" | "gemling" | "tactician" => "mercenary",
        "infernalist" | "lich" | "necromancer" | "minion" | "minions" | "zombie" | "skeleton" => "witch",
        "stormweaver" | "chronomancer" => "sorceress",
        "invoker" | "chayula" => "monk",
        "deadeye" | "pathfinder" | "bow" => "ranger",
        "titan" | "warbringer" | "smith" => "warrior",
        "amazon" | "ritualist" | "spear" => "huntress",
        _ => return None,
    })
}

const CLASS_NAMES: &[&str] = &[
    "druid", "witch", "sorceress", "huntress", "monk", "ranger", "warrior", "mercenary",
];

impl SeedLibrary {
    /// Scan for fixture XMLs. Probes the same locations the seed.xml loader
    /// uses, so dist deployments can ship a `fixtures/` dir next to the exe.
    pub fn discover() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(p) = std::env::var("MOSSRAVEN_SEED_LIBRARY_DIR") {
            candidates.push(p.into());
        }
        candidates.push(exe_dir.join("fixtures"));
        candidates.push(exe_dir.join("../crates/pob/tests/fixtures"));
        candidates.push(exe_dir.join("../../crates/pob/tests/fixtures"));
        candidates.push(PathBuf::from("crates/pob/tests/fixtures"));

        for dir in candidates {
            let lib = Self::scan_dir(&dir);
            if !lib.entries.is_empty() {
                tracing::info!(
                    dir = ?dir,
                    seeds = lib.entries.len(),
                    classes = ?lib
                        .entries
                        .iter()
                        .map(|e| e.class_name.as_str())
                        .collect::<std::collections::BTreeSet<_>>(),
                    "seed library discovered"
                );
                return lib;
            }
        }
        tracing::warn!("no seed library found — concept-grounded seed selection disabled");
        Self::default()
    }

    fn scan_dir(dir: &Path) -> Self {
        let mut entries = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Self::default();
        };
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("xml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_lowercase();
            // Read just the head — className lives in the <Build> opening tag.
            let head = match read_head(&path, 4096) {
                Some(h) => h,
                None => continue,
            };
            let class_name = attr_value(&head, "className").unwrap_or_default();
            let ascend_name = attr_value(&head, "ascendClassName").unwrap_or_default();
            if class_name.is_empty() {
                continue; // not a PoB build XML
            }
            let tokens: Vec<String> = stem
                .split(['-', '_', ' '])
                .filter(|t| !t.is_empty() && *t != "crit" && t.parse::<u32>().is_err())
                .map(str::to_string)
                .collect();
            entries.push(SeedEntry {
                path,
                class_name,
                ascend_name,
                tokens,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Self { entries }
    }

    /// Map a concept to the best seed. Returns None when nothing in the
    /// concept matches any class/ascendancy/skill token — callers fall back
    /// to the bundled default rather than guessing.
    pub fn select(&self, concept: &str) -> Option<&SeedEntry> {
        if self.entries.is_empty() {
            return None;
        }
        let concept_tokens: Vec<String> = concept
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .map(str::to_string)
            .collect();

        // Resolve which class the concept asks for (direct name or alias).
        let mut wanted_class: Option<&str> = None;
        for t in &concept_tokens {
            if let Some(c) = CLASS_NAMES.iter().find(|c| *c == t) {
                wanted_class = Some(c);
                break;
            }
        }
        if wanted_class.is_none() {
            for t in &concept_tokens {
                if let Some(c) = class_aliases(t) {
                    wanted_class = Some(c);
                    break;
                }
            }
        }

        let mut best: Option<(i32, &SeedEntry)> = None;
        for entry in &self.entries {
            let mut score = 0i32;
            let entry_class = entry.class_name.to_lowercase();
            let entry_ascend = entry.ascend_name.to_lowercase();

            if let Some(wc) = wanted_class {
                if entry_class == wc {
                    score += 100;
                } else {
                    // Concept named a class and this seed isn't it — heavy
                    // penalty so a skill-word coincidence can't override the
                    // user's explicit class choice.
                    score -= 100;
                }
            }
            for t in &concept_tokens {
                if *t == entry_ascend {
                    score += 40;
                }
                if entry.tokens.contains(t) && *t != entry_class && *t != entry_ascend {
                    score += 15;
                }
            }
            match &best {
                Some((s, _)) if *s >= score => {}
                _ => best = Some((score, entry)),
            }
        }
        match best {
            Some((score, entry)) if score > 0 => {
                tracing::info!(
                    seed = ?entry.path.file_name().unwrap_or_default(),
                    class = %entry.class_name,
                    ascendancy = %entry.ascend_name,
                    score,
                    "concept-grounded seed selected"
                );
                Some(entry)
            }
            _ => {
                tracing::info!("no seed matched the concept; using default seed");
                None
            }
        }
    }
}

fn read_head(path: &Path, max: usize) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; max];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn attr_value(haystack: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let i = haystack.find(&needle)?;
    let start = i + needle.len();
    let end = haystack[start..].find('"')?;
    Some(haystack[start..start + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> SeedLibrary {
        // Tests run with CWD = crate dir; fixtures live at ../../crates/... from
        // the bin crate — but `cargo test -p mossraven-service` sets CWD to the
        // bin crate dir. Probe the workspace-relative path directly.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/pob/tests/fixtures");
        SeedLibrary::scan_dir(&dir)
    }

    #[test]
    fn druid_concept_selects_druid_seed() {
        let lib = lib();
        if lib.entries.is_empty() {
            eprintln!("skipping: fixtures not present (gitignored)");
            return;
        }
        let Some(sel) = lib.select("Shieldy lightning wolf druid with boss and clear weapon swaps") else {
            // Library exists but no druid fixture pulled yet — acceptable in CI.
            assert!(
                !lib.entries.iter().any(|e| e.class_name == "Druid"),
                "druid fixture exists but selector missed it"
            );
            return;
        };
        assert_eq!(sel.class_name, "Druid", "selected: {:?}", sel.path);
    }

    #[test]
    fn alias_wolf_maps_to_druid() {
        let lib = lib();
        if !lib.entries.iter().any(|e| e.class_name == "Druid") {
            eprintln!("skipping: no druid fixture");
            return;
        }
        let sel = lib.select("werewolf shapeshift melee").expect("alias should match");
        assert_eq!(sel.class_name, "Druid");
    }

    #[test]
    fn sorceress_spark_prefers_spark_fixture() {
        let lib = lib();
        if lib.entries.is_empty() {
            return;
        }
        if let Some(sel) = lib.select("sorceress spark stormweaver") {
            assert_eq!(sel.class_name, "Sorceress", "selected: {:?}", sel.path);
            // Prefer a spark fixture over other sorceress seeds when present.
            if lib.entries.iter().any(|e| e.tokens.contains(&"spark".to_string())) {
                assert!(
                    sel.tokens.contains(&"spark".to_string()),
                    "expected a spark seed, got {:?}",
                    sel.path
                );
            }
        }
    }

    #[test]
    fn unrelated_concept_returns_none() {
        let lib = lib();
        if lib.entries.is_empty() {
            return;
        }
        assert!(lib.select("the quick brown fox jumps over the lazy dog").is_none());
    }
}
