//! Passive-tree database parsed from the vendored `TreeData/<ver>/tree.json`.
//!
//! Powers the `allocate_notable` mutation op — the first operator that can
//! attack the viability DPS gap (gem ops alone top out ~75k on current seeds;
//! the comfort floor is 500k; trees are where real scaling lives).
//!
//! Correctness rule: allocations are PATHED, never teleported. The op walks
//! the adjacency graph from the build's existing allocation set to the target
//! notable (BFS through unallocated nodes, bounded hops) and appends the
//! whole path — paying realistic travel-point costs and guaranteeing the
//! result is a connected tree PoB would accept from a real player.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct TreeNode {
    pub id: u32,
    pub name: String,
    pub is_notable: bool,
    pub stats: Vec<String>,
    pub neighbors: Vec<u32>,
    /// Ascendancy nodes can't be path-allocated from the main tree.
    pub ascendancy: bool,
}

#[derive(Debug, Default)]
pub struct TreeVersion {
    pub nodes: HashMap<u32, TreeNode>,
    /// lowercase notable name → node id (main-tree notables only).
    pub notable_by_name: HashMap<String, u32>,
}

#[derive(Debug, Default)]
pub struct TreeDb {
    versions: HashMap<String, TreeVersion>,
}

impl TreeDb {
    /// Load every `TreeData/<ver>/tree.json` under the PoB checkout. Missing
    /// or unparsable versions are skipped — an empty db just disables the
    /// allocate op (skip + warn), it never breaks the cascade.
    pub fn load(pob_path: &Path) -> Self {
        let mut db = Self::default();
        let root = pob_path.join("src/TreeData");
        let Ok(rd) = std::fs::read_dir(&root) else {
            tracing::warn!(path = ?root, "TreeData not readable; tree ops disabled");
            return db;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let ver = entry.file_name().to_string_lossy().into_owned();
            let tree_json = entry.path().join("tree.json");
            let Ok(text) = std::fs::read_to_string(&tree_json) else {
                continue;
            };
            match Self::parse_version(&text) {
                Some(tv) => {
                    tracing::info!(
                        version = %ver,
                        nodes = tv.nodes.len(),
                        notables = tv.notable_by_name.len(),
                        "tree version loaded"
                    );
                    db.versions.insert(ver, tv);
                }
                None => tracing::warn!(version = %ver, "tree.json unparsable; skipped"),
            }
        }
        db
    }

    pub fn parse_version(text: &str) -> Option<TreeVersion> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let nodes = v.get("nodes")?.as_object()?;
        let mut tv = TreeVersion::default();

        for (k, n) in nodes {
            let Ok(id) = k.parse::<u32>() else { continue };
            let name = n.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let is_notable = n.get("isNotable").and_then(|x| x.as_bool()).unwrap_or(false);
            let ascendancy = n
                .get("ascendancyName")
                .and_then(|x| x.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let stats = n
                .get("stats")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let neighbors = n
                .get("connections")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.get("id").and_then(|i| i.as_u64()).map(|i| i as u32))
                        .collect()
                })
                .unwrap_or_default();
            tv.nodes.insert(
                id,
                TreeNode {
                    id,
                    name,
                    is_notable,
                    stats,
                    neighbors,
                    ascendancy,
                },
            );
        }

        // connections are stored one-directionally — union both ways so BFS
        // can walk the graph regardless of which side recorded the edge.
        let edges: Vec<(u32, u32)> = tv
            .nodes
            .values()
            .flat_map(|n| n.neighbors.iter().map(move |&m| (n.id, m)))
            .collect();
        for (a, b) in edges {
            if let Some(nb) = tv.nodes.get_mut(&b) {
                if !nb.neighbors.contains(&a) {
                    nb.neighbors.push(a);
                }
            }
        }

        for n in tv.nodes.values() {
            if n.is_notable && !n.ascendancy && !n.name.is_empty() {
                tv.notable_by_name.insert(n.name.to_lowercase(), n.id);
            }
        }
        Some(tv)
    }

    pub fn version(&self, ver: &str) -> Option<&TreeVersion> {
        self.versions.get(ver)
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// BFS from the allocated set to the named notable, through unallocated
    /// non-ascendancy nodes, at most `max_hops` NEW nodes (path incl. the
    /// target). Returns the new node ids to allocate, in walk order.
    /// None = unknown notable / already allocated / out of reach.
    ///
    /// `blocked` = nodes the path may neither anchor on nor pass through.
    /// In practice: weapon-set allocations (`<WeaponSet1/2 nodes>`). PoB's
    /// `CanPathThroughAllocMode` forbids a normal-mode (0) node from pathing
    /// to start through a set-mode node, and `BuildAllDependsAndPaths`
    /// orphan-prunes (`DeallocSingleNode`) anything that fails that walk —
    /// so a path anchored on a WS node would be silently de-allocated.
    pub fn path_to_notable(
        &self,
        ver: &str,
        allocated: &HashSet<u32>,
        blocked: &HashSet<u32>,
        notable_name: &str,
        max_hops: usize,
    ) -> Option<Vec<u32>> {
        let tv = self.versions.get(ver)?;
        let &target = tv.notable_by_name.get(&notable_name.to_lowercase())?;
        if allocated.contains(&target) || blocked.contains(&target) {
            return None;
        }
        // BFS outward from every (non-blocked) allocated node simultaneously.
        let mut prev: HashMap<u32, u32> = HashMap::new();
        let mut depth: HashMap<u32, usize> = HashMap::new();
        let mut q: VecDeque<u32> = VecDeque::new();
        for &a in allocated {
            if blocked.contains(&a) {
                continue;
            }
            depth.insert(a, 0);
            q.push_back(a);
        }
        while let Some(cur) = q.pop_front() {
            let d = depth[&cur];
            if d >= max_hops {
                continue;
            }
            let Some(node) = tv.nodes.get(&cur) else { continue };
            for &nb in &node.neighbors {
                if depth.contains_key(&nb) || blocked.contains(&nb) {
                    continue;
                }
                let Some(nbn) = tv.nodes.get(&nb) else { continue };
                if nbn.ascendancy {
                    continue;
                }
                depth.insert(nb, d + 1);
                prev.insert(nb, cur);
                if nb == target {
                    // Reconstruct path back to (but excluding) the allocated set.
                    let mut path = vec![nb];
                    let mut at = nb;
                    while let Some(&p) = prev.get(&at) {
                        if allocated.contains(&p) {
                            break;
                        }
                        path.push(p);
                        at = p;
                    }
                    path.reverse();
                    return Some(path);
                }
                q.push_back(nb);
            }
        }
        None
    }

    /// The closest unallocated main-tree notables reachable within
    /// `max_hops`, as (name, hop-cost, joined-stats) — prompt fodder so the
    /// LLM proposes allocations that actually exist and actually connect.
    pub fn nearby_notables(
        &self,
        ver: &str,
        allocated: &HashSet<u32>,
        blocked: &HashSet<u32>,
        max_hops: usize,
        limit: usize,
    ) -> Vec<(String, usize, String)> {
        let Some(tv) = self.versions.get(ver) else {
            return Vec::new();
        };
        let mut depth: HashMap<u32, usize> = HashMap::new();
        let mut q: VecDeque<u32> = VecDeque::new();
        for &a in allocated {
            if blocked.contains(&a) {
                continue;
            }
            depth.insert(a, 0);
            q.push_back(a);
        }
        let mut found: Vec<(String, usize, String)> = Vec::new();
        while let Some(cur) = q.pop_front() {
            let d = depth[&cur];
            if d >= max_hops {
                continue;
            }
            let Some(node) = tv.nodes.get(&cur) else { continue };
            for &nb in &node.neighbors {
                if depth.contains_key(&nb) || blocked.contains(&nb) {
                    continue;
                }
                let Some(nbn) = tv.nodes.get(&nb) else { continue };
                if nbn.ascendancy {
                    continue;
                }
                depth.insert(nb, d + 1);
                if nbn.is_notable && !allocated.contains(&nb) {
                    found.push((nbn.name.clone(), d + 1, nbn.stats.join("; ")));
                }
                q.push_back(nb);
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        found.truncate(limit);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mini-tree: 1(alloc) — 2 — 3(Notable "Power") ; 4(Notable, isolated);
    /// 5(ascendancy notable, adjacent to 1 — must be unreachable).
    const MINI: &str = r#"{
      "nodes": {
        "1": {"name": "Start", "connections": [{"id": 2}]},
        "2": {"name": "Travel", "connections": [{"id": 3}]},
        "3": {"name": "Power", "isNotable": true, "stats": ["20% more damage"], "connections": []},
        "4": {"name": "Island", "isNotable": true, "stats": ["unreachable"], "connections": []},
        "5": {"name": "AscNode", "isNotable": true, "ascendancyName": "Oracle", "connections": [{"id": 1}]}
      }
    }"#;

    fn db() -> TreeDb {
        let mut d = TreeDb::default();
        d.versions
            .insert("0_4".into(), TreeDb::parse_version(MINI).unwrap());
        d
    }

    fn no_block() -> HashSet<u32> {
        HashSet::new()
    }

    #[test]
    fn paths_through_travel_nodes_to_notable() {
        let alloc: HashSet<u32> = [1].into();
        let path = db().path_to_notable("0_4", &alloc, &no_block(), "Power", 4).unwrap();
        assert_eq!(path, vec![2, 3], "walks 2 then lands on the notable");
    }

    #[test]
    fn unreachable_and_ascendancy_notables_return_none() {
        let alloc: HashSet<u32> = [1].into();
        assert!(db().path_to_notable("0_4", &alloc, &no_block(), "Island", 6).is_none());
        assert!(db().path_to_notable("0_4", &alloc, &no_block(), "AscNode", 6).is_none());
    }

    #[test]
    fn hop_budget_is_enforced() {
        let alloc: HashSet<u32> = [1].into();
        assert!(
            db().path_to_notable("0_4", &alloc, &no_block(), "Power", 1).is_none(),
            "needs 2 hops"
        );
    }

    #[test]
    fn blocked_nodes_are_walls_not_anchors() {
        // 1 is allocated but ALSO blocked (weapon-set node): no usable anchor.
        let alloc: HashSet<u32> = [1].into();
        let blocked: HashSet<u32> = [1].into();
        assert!(
            db().path_to_notable("0_4", &alloc, &blocked, "Power", 6).is_none(),
            "a weapon-set anchor must not seed the path"
        );
        // 2 blocked: the only corridor to Power is walled off.
        let blocked2: HashSet<u32> = [2].into();
        assert!(
            db().path_to_notable("0_4", &alloc, &blocked2, "Power", 6).is_none(),
            "paths must not tunnel through weapon-set nodes"
        );
        assert!(db().nearby_notables("0_4", &alloc, &blocked2, 6, 10).is_empty());
    }

    #[test]
    fn nearby_lists_reachable_notables_with_cost() {
        let alloc: HashSet<u32> = [1].into();
        let near = db().nearby_notables("0_4", &alloc, &no_block(), 5, 10);
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].0, "Power");
        assert_eq!(near[0].1, 2);
    }

    #[test]
    fn real_vendor_tree_parses_when_present() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/PathOfBuilding-PoE2");
        if !p.join("src/TreeData/0_4/tree.json").exists() {
            eprintln!("skipping: vendor tree not present");
            return;
        }
        let db = TreeDb::load(&p);
        let tv = db.version("0_4").expect("0_4 loaded");
        assert!(tv.nodes.len() > 4000, "{}", tv.nodes.len());
        assert!(tv.notable_by_name.len() > 500, "{}", tv.notable_by_name.len());
    }
}
