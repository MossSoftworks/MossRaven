//! MAP-Elites quality-diversity archive.
//!
//! The archive is **the product** of MossRaven. Each cell holds the best build
//! found so far for that behavioral niche. Empty cells with high theoretical
//! potential are the discovery signal — "this kind of build should work but
//! nobody has surfaced it."
//!
//! # Behavioral axes (tunable, not one-shot)
//!
//! v1 axes (placeholder — iterate empirically):
//! - damage_type: physical / cold / fire / lightning / chaos / dot / minion
//! - defense_layer: evasion / armour / ES / hybrid / block-spell / dodge-roll
//! - role: clear / boss / hybrid
//! - scaling_vector: gem-levels / attribute-stack / unique-driven / tree-keystone

use base64::Engine;
use mossraven_pob::BuildStats;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Encode a PoB2 XML build into the import code players paste into
/// "Path of Building > Import > Import from import code".
///
/// The format is `urlsafe_base64( zlib_default( xml_bytes ) )`. This matches
/// PoB2's import expectation; the WPF UI uses the equivalent C# implementation
/// in `MainWindow.xaml.cs::EncodePobImportCode`. The two implementations must
/// stay in sync — both compress at default-compression with the zlib wrapper
/// (not raw deflate, not gzip), and both use the URL-safe base64 alphabet.
pub fn encode_pob_import_code(xml: &str) -> String {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(xml.as_bytes()).expect("write to Vec");
    let compressed = enc.finish().expect("zlib finish");
    base64::engine::general_purpose::URL_SAFE.encode(&compressed)
}

/// Decode a PoB2 import code back to XML — inverse of
/// [`encode_pob_import_code`]. Tolerant of both base64 alphabets (PoB itself
/// uses URL-safe; codes copied off pobb.in / pastebins sometimes carry the
/// standard alphabet).
pub fn decode_pob_import_code(code: &str) -> Result<String, String> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let trimmed = code.trim();
    let compressed = base64::engine::general_purpose::URL_SAFE
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(trimmed))
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    let mut xml = String::new();
    ZlibDecoder::new(compressed.as_slice())
        .read_to_string(&mut xml)
        .map_err(|e| format!("zlib inflate failed: {e}"))?;
    Ok(xml)
}

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellCoords {
    pub damage_type: String,
    pub defense_layer: String,
    pub role: String,
    pub scaling_vector: String,
}

impl CellCoords {
    pub fn as_path_segment(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.damage_type, self.defense_layer, self.role, self.scaling_vector
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub variant_id: String,
    pub pob_xml: String,
    pub stats: BuildStats,
    /// Free-text rationale from Tier 1 (why this build was hypothesized).
    pub origin_hypothesis: Option<String>,
    /// PoB2 version + game-data version for staleness detection.
    pub data_version: String,
}

#[derive(Default)]
pub struct Archive {
    cells: RwLock<HashMap<CellCoords, ArchiveEntry>>,
    /// True once THIS process has placed/improved a cell — gates
    /// `save_if_dirty` so idle daemons never write on shutdown.
    dirty: std::sync::atomic::AtomicBool,
    /// mtime of the on-disk file as of our last load/merge/save. Drives
    /// `refresh_from_disk`'s "is disk newer than me" check.
    loaded_mtime: parking_lot::Mutex<Option<std::time::SystemTime>>,
}

enum LoadOutcome {
    Loaded(Archive),
    Missing,
    Corrupt,
}

fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[derive(Debug, Serialize, Deserialize)]
struct ArchiveSnapshot {
    /// Schema version for forward-compat migrations.
    version: u32,
    cells: Vec<(CellCoords, ArchiveEntry)>,
}

impl Archive {
    pub fn new() -> Self {
        Self::default()
    }

    /// Default on-disk location. Falls back to relative `./archive.json` if
    /// the platform data dir isn't available.
    pub fn default_path() -> PathBuf {
        directories::ProjectDirs::from("", "Moss", "MossRaven")
            .map(|d| d.data_dir().join("archive.json"))
            .unwrap_or_else(|| PathBuf::from("archive.json"))
    }

    /// Load archive from disk. Returns empty archive if path doesn't exist or
    /// the file is corrupt — losing the archive shouldn't crash the service.
    ///
    /// Retries once after a short delay on not-found/corrupt: another process
    /// may be mid-save (a writer's replace, or a third-party tool's
    /// delete+recreate). Observed in the field: a daemon spawning during a
    /// save window logged "file not found" while 10 cells sat on disk.
    pub fn load(path: &Path) -> Result<Self, ArchiveError> {
        for attempt in 0..2 {
            match Self::load_once(path) {
                LoadOutcome::Loaded(a) => return Ok(a),
                LoadOutcome::Missing | LoadOutcome::Corrupt if attempt == 0 => {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
                LoadOutcome::Missing => {
                    tracing::info!(?path, "archive file not found; starting fresh");
                    return Ok(Self::default());
                }
                LoadOutcome::Corrupt => {
                    tracing::warn!(?path, "archive file corrupt; starting fresh");
                    return Ok(Self::default());
                }
            }
        }
        Ok(Self::default())
    }

    fn load_once(path: &Path) -> LoadOutcome {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::Missing,
            Err(_) => return LoadOutcome::Corrupt,
        };
        match serde_json::from_str::<ArchiveSnapshot>(&text) {
            Ok(snap) => {
                let map: HashMap<_, _> = snap.cells.into_iter().collect();
                tracing::info!(?path, count = map.len(), "archive loaded");
                let a = Self {
                    cells: RwLock::new(map),
                    ..Self::default()
                };
                *a.loaded_mtime.lock() = file_mtime(path);
                LoadOutcome::Loaded(a)
            }
            Err(_) => LoadOutcome::Corrupt,
        }
    }

    /// If the on-disk archive is newer than what this instance last
    /// loaded/saved, MERGE it in (per-cell elite check, same rule as
    /// `try_place`). This is what lets long-lived daemons (the WPF's child,
    /// MCP-registered services) serve disk truth after a CLI process — or
    /// another daemon — writes: without it, a daemon answers `read_archive`
    /// from memory frozen at its spawn time forever.
    ///
    /// Merging (not replacing) means two concurrent writers can't lose each
    /// other's cells — worst case a cell briefly holds the lower-DPS elite
    /// until the next refresh.
    pub fn refresh_from_disk(&self, path: &Path) {
        let disk_mtime = match file_mtime(path) {
            Some(m) => m,
            None => return, // no file — nothing newer to merge
        };
        {
            let loaded = self.loaded_mtime.lock();
            if let Some(l) = *loaded {
                if disk_mtime <= l {
                    return; // we're current
                }
            }
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return,
        };
        let snap: ArchiveSnapshot = match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(_) => return, // mid-write torn read — next call will retry
        };
        let mut merged = 0usize;
        {
            let mut cells = self.cells.write();
            for (coords, entry) in snap.cells {
                match cells.get(&coords) {
                    Some(cur) if cur.stats.total_dps >= entry.stats.total_dps => {}
                    _ => {
                        cells.insert(coords, entry);
                        merged += 1;
                    }
                }
            }
        }
        *self.loaded_mtime.lock() = Some(disk_mtime);
        if merged > 0 {
            tracing::info!(?path, merged, "archive refreshed from disk (external writer detected)");
        }
    }

    /// Persist archive to disk atomically (.tmp + rename-replace).
    pub fn save(&self, path: &Path) -> Result<(), ArchiveError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let snap = ArchiveSnapshot {
            version: 1,
            cells: self
                .cells
                .read()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            serde_json::to_writer_pretty(&mut f, &snap)?;
            f.flush()?;
            // Best-effort fsync. Atomic rename below is the real durability gate.
            let _ = f.sync_all();
        }
        // std::fs::rename on Windows uses MOVEFILE_REPLACE_EXISTING — the
        // replace is atomic and the destination never goes missing. (The old
        // remove-then-rename here opened a not-found window every save; a
        // daemon spawning inside it started "fresh" on a populated archive.)
        std::fs::rename(&tmp, path)?;
        self.dirty.store(false, std::sync::atomic::Ordering::SeqCst);
        *self.loaded_mtime.lock() = file_mtime(path);
        tracing::debug!(?path, count = snap.cells.len(), "archive saved");
        Ok(())
    }

    /// Save only if this instance has modified the archive since the last
    /// load/save — after first merging any newer on-disk state. Idle daemons
    /// (a WPF child that never ran a search) become guaranteed-harmless on
    /// shutdown instead of clobbering whatever a CLI run wrote meanwhile.
    pub fn save_if_dirty(&self, path: &Path) -> Result<bool, ArchiveError> {
        if !self.dirty.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::debug!(?path, "archive unchanged in this process; skipping save");
            return Ok(false);
        }
        self.refresh_from_disk(path);
        self.save(path)?;
        Ok(true)
    }

    /// Place a candidate. Returns `true` if it beat the current cell elite
    /// (or filled an empty cell), `false` if rejected.
    pub fn try_place(&self, coords: CellCoords, entry: ArchiveEntry) -> bool {
        let mut cells = self.cells.write();
        match cells.get(&coords) {
            Some(current) if current.stats.total_dps >= entry.stats.total_dps => false,
            _ => {
                cells.insert(coords, entry);
                self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            }
        }
    }

    pub fn read(&self, coords: &CellCoords) -> Option<ArchiveEntry> {
        self.cells.read().get(coords).cloned()
    }

    /// Replace the ENTIRE grid. For maintenance passes (rescore after a PoB
    /// patch, legality purges, label migrations) where entries must be
    /// updated even when the new stats are WORSE — `try_place`'s
    /// improve-only rule would silently keep stale elites.
    pub fn rebuild(&self, cells: Vec<(CellCoords, ArchiveEntry)>) {
        let mut grid = self.cells.write();
        grid.clear();
        for (coords, entry) in cells {
            grid.insert(coords, entry);
        }
        self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn filled_count(&self) -> usize {
        self.cells.read().len()
    }

    /// Snapshot all filled cells. Used by the MCP `read_archive` tool and the
    /// prompt sampler (which feeds Tier 1 a diverse sub-optimal sample).
    pub fn snapshot(&self) -> Vec<(CellCoords, ArchiveEntry)> {
        self.cells
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("mossraven-test-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn entry(dps: f64) -> ArchiveEntry {
        ArchiveEntry {
            variant_id: format!("v{dps}"),
            pob_xml: String::new(),
            stats: BuildStats { total_dps: dps, ..Default::default() },
            origin_hypothesis: None,
            data_version: "test".into(),
        }
    }

    fn coords(d: &str) -> CellCoords {
        CellCoords {
            damage_type: d.into(),
            defense_layer: "es".into(),
            role: "boss".into(),
            scaling_vector: "gem-levels".into(),
        }
    }

    #[test]
    fn idle_instance_skips_save_and_never_clobbers() {
        let path = tmp("idle");
        // Writer A persists one cell.
        let a = Archive::new();
        a.try_place(coords("fire"), entry(100.0));
        a.save(&path).unwrap();
        // Reader B loads, runs NOTHING, then "shuts down".
        let b = Archive::load(&path).unwrap();
        assert!(!b.save_if_dirty(&path).unwrap(), "clean instance must skip the save");
        // A's data survives B's shutdown.
        let check = Archive::load(&path).unwrap();
        assert_eq!(check.snapshot().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn refresh_merges_external_writer_by_cell_elite() {
        let path = tmp("merge");
        // Daemon D loads an empty file state (no file yet).
        let d = Archive::new();
        d.try_place(coords("fire"), entry(50.0)); // D's own work
        // External CLI writes a DIFFERENT cell + a BETTER fire elite.
        let cli = Archive::new();
        cli.try_place(coords("fire"), entry(200.0));
        cli.try_place(coords("cold"), entry(75.0));
        cli.save(&path).unwrap();
        // D refreshes: gains cold, upgrades fire — keeps serving disk truth.
        d.refresh_from_disk(&path);
        let snap = d.snapshot();
        assert_eq!(snap.len(), 2);
        let fire = snap.iter().find(|(c, _)| c.damage_type == "fire").unwrap();
        assert_eq!(fire.1.stats.total_dps, 200.0, "external better elite wins the merge");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn refresh_noop_when_disk_not_newer() {
        let path = tmp("noop");
        let a = Archive::new();
        a.try_place(coords("fire"), entry(100.0));
        a.save(&path).unwrap();
        // Same instance, no external writes: refresh must not duplicate/alter.
        a.refresh_from_disk(&path);
        assert_eq!(a.snapshot().len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
