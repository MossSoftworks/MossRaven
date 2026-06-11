//! SPEC §3.7 — the Tier-4 eval corpus (the value model's training data).
//!
//! Every successful judge evaluation appends one JSONL row: build features
//! (§ features.rs) + PoB-scored labels + version stamp. Files are split per
//! `pob2_version` because patches change calc math — the SPEC forbids
//! training across patch boundaries, so the file layout enforces it.
//!
//! On by default; `MOSSRAVEN_CORPUS=0` disables. The directory comes from
//! `MOSSRAVEN_CORPUS_DIR` (the service sets it to `<data-dir>/corpus` at
//! startup); without it, logging is skipped — a bare library consumer
//! shouldn't write files to surprise locations.

use mossraven_pob::BuildStats;
use std::io::Write;

fn corpus_dir() -> Option<std::path::PathBuf> {
    if std::env::var("MOSSRAVEN_CORPUS").map(|v| v == "0").unwrap_or(false) {
        return None;
    }
    std::env::var_os("MOSSRAVEN_CORPUS_DIR").map(std::path::PathBuf::from)
}

fn sanitize(v: &str) -> String {
    v.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect()
}

/// Append one row per (xml, stats) pair. Failures are logged and swallowed —
/// the corpus is a byproduct; it must never break a search.
pub fn log_evals<'a>(
    items: impl Iterator<Item = (&'a str, &'a BuildStats)>,
    data_version: &str,
) {
    let Some(dir) = corpus_dir() else { return };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, "corpus dir create failed; eval not logged");
        return;
    }
    let path = dir.join(format!("evals-{}.jsonl", sanitize(data_version)));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut rows = String::new();
    let mut n = 0usize;
    for (xml, stats) in items {
        let row = serde_json::json!({
            "ts": ts,
            "v": data_version,
            "features": crate::features::extract(xml),
            "labels": {
                "dps": stats.total_dps,
                "ehp": stats.effective_hp,
                "es": stats.energy_shield,
                "life": stats.life,
                "fire_res": stats.fire_res,
                "cold_res": stats.cold_res,
                "lightning_res": stats.lightning_res,
                "chaos_res": stats.chaos_res,
                "points_used": stats.points_used,
                "points_budget": stats.points_budget,
            },
        });
        rows.push_str(&row.to_string());
        rows.push('\n');
        n += 1;
    }
    if n == 0 {
        return;
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(rows.as_bytes()) {
                tracing::warn!(error = %e, "corpus append failed");
            } else {
                tracing::debug!(rows = n, path = %path.display(), "corpus rows appended");
            }
        }
        Err(e) => tracing::warn!(error = %e, path = %path.display(), "corpus open failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_append_with_features_and_labels() {
        let dir = std::env::temp_dir().join(format!("mossraven-corpus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("MOSSRAVEN_CORPUS_DIR", &dir);
        std::env::remove_var("MOSSRAVEN_CORPUS");

        let stats = BuildStats { total_dps: 1234.0, points_budget: 121, ..Default::default() };
        let xml = r#"<Build className="Witch" ascendClassName="Lich" level="90"></Build>"#;
        log_evals([(xml, &stats)].into_iter(), "pob2:0.19.0");
        log_evals([(xml, &stats)].into_iter(), "pob2:0.19.0");

        let f = dir.join("evals-pob2_0.19.0.jsonl");
        let text = std::fs::read_to_string(&f).expect("corpus file written");
        assert_eq!(text.lines().count(), 2);
        let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(row["features"]["class"], "Witch");
        assert_eq!(row["labels"]["dps"], 1234.0);
        assert_eq!(row["labels"]["points_budget"], 121);

        std::env::set_var("MOSSRAVEN_CORPUS", "0");
        log_evals([(xml, &stats)].into_iter(), "pob2:0.19.0");
        let text2 = std::fs::read_to_string(&f).unwrap();
        assert_eq!(text2.lines().count(), 2, "disabled flag must skip logging");
        std::env::remove_var("MOSSRAVEN_CORPUS");
        std::env::remove_var("MOSSRAVEN_CORPUS_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
