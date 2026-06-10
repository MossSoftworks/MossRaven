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
    pub fn load(path: &Path) -> Result<Self, ArchiveError> {
        if !path.exists() {
            tracing::info!(?path, "archive file not found; starting fresh");
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        match serde_json::from_str::<ArchiveSnapshot>(&text) {
            Ok(snap) => {
                let map: HashMap<_, _> = snap.cells.into_iter().collect();
                tracing::info!(?path, count = map.len(), "archive loaded");
                Ok(Self {
                    cells: RwLock::new(map),
                })
            }
            Err(e) => {
                tracing::warn!(?path, error = %e, "archive file corrupt; starting fresh");
                Ok(Self::default())
            }
        }
    }

    /// Persist archive to disk atomically (.tmp + rename).
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
        // Windows rename fails if destination exists; remove first.
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&tmp, path)?;
        tracing::debug!(?path, count = snap.cells.len(), "archive saved");
        Ok(())
    }

    /// Place a candidate. Returns `true` if it beat the current cell elite
    /// (or filled an empty cell), `false` if rejected.
    pub fn try_place(&self, coords: CellCoords, entry: ArchiveEntry) -> bool {
        let mut cells = self.cells.write();
        match cells.get(&coords) {
            Some(current) if current.stats.total_dps >= entry.stats.total_dps => false,
            _ => {
                cells.insert(coords, entry);
                true
            }
        }
    }

    pub fn read(&self, coords: &CellCoords) -> Option<ArchiveEntry> {
        self.cells.read().get(coords).cloned()
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
