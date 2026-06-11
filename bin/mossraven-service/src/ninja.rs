//! poe.ninja grounding (SPEC §1.1.2 live prices; docs/nn-direction.md phase 1).
//!
//! Endpoint discovery log (2026-06-11, probed live):
//! - CONFIRMED `GET /poe2/api/data/index-state` → league list
//!   (`economyLeagues[].{name,url,indexed}`).
//! - CONFIRMED `GET /poe2/api/economy/exchange/{version}/overview
//!   ?league=<DISPLAY NAME>&type=Currency` — the league param wants the
//!   display name ("Runes of Aldur"), NOT the url slug; the slug returns an
//!   empty 200 shell. With the name it returns ~25 KB of live divine/exalted
//!   exchange data. Unique types on this route return empty — it is the
//!   currency-exchange feed only.
//! - CONFIRMED `GET /poe2/api/economy/stash/{version}/item/overview
//!   ?league=<DISPLAY NAME>&type=<PluralType>` — the unique-item prices.
//!   Found via the page's island chunk (the page slug is PLURAL:
//!   /poe2/economy/<slug>/unique-weapons; the singular page is a 404 shell,
//!   which is why earlier probing missed the chunk). Type values are plural
//!   (UniqueWeapons, UniqueArmours, ...). `lines[].primaryValue` is ALREADY
//!   in divines (core.primary == "divine"; rates: 1 div ≈ 130 ex ≈ 10.7
//!   chaos at discovery time). 148 weapon lines live on 2026-06-11.
//!   `MOSSRAVEN_NINJA_ITEM_URL` still overrides the template if it moves.
//!
//! BUILDS API (discovered 2026-06-11, for #74 seed import / meta-distance):
//! - `GET /poe2/api/data/build-index-state` → per-league ladder totals +
//!   class distribution (124,230 chars in Runes of Aldur at discovery).
//! - poe2 `/poe2/api/data/index-state` ALSO carries `snapshotVersions[]`
//!   with daily build snapshots: `{snapshotName:"runes-of-aldur",
//!   version:"1901-20260611-56379"}` — the `{version}` for builds routes.
//! - CONFIRMED `GET /poe2/api/builds/{version}/search?overview=<snapshotName>`
//!   → ~52 KB ladder build data (the missing param was `overview`).
//!   tooltip wants type+tooltip+overview.
//!
//! Behavior: gated by `MOSSRAVEN_NINJA=1`. On startup, refresh a price map
//! (unique name → divine value) into `<data-dir>/ninja-prices.json` (24h
//! TTL) and point `MOSSRAVEN_PRICES_PATH` at it — the cost model overlays
//! live prices over its heuristic. Every failure degrades silently to the
//! heuristic; grounding must never block an offline run.

use serde_json::Value;

const UA: &str = "MossRaven/0.1 (build-discovery; github.com/MossSoftworks/MossRaven)";
const DEFAULT_ITEM_URL: &str =
    "https://poe.ninja/poe2/api/economy/stash/1/item/overview?league={league}&type={type}";
/// Plural per the live API (matches the site nav: Equipment + Atlas).
const UNIQUE_TYPES: &[&str] = &[
    "UniqueWeapons",
    "UniqueArmours",
    "UniqueAccessories",
    "UniqueFlasks",
    "UniqueCharms",
    "UniqueJewels",
    "UniqueRelics",
    "UniqueTablets",
    "PrecursorTablets",
];

pub async fn refresh_prices(data_dir: &std::path::Path) {
    if std::env::var("MOSSRAVEN_NINJA").map(|v| v != "1").unwrap_or(true) {
        return;
    }
    let out_path = data_dir.join("ninja-prices.json");
    // TTL: skip the network when the cache is fresh (<24h).
    if let Ok(meta) = std::fs::metadata(&out_path) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().map(|e| e.as_secs() < 24 * 3600).unwrap_or(false) {
                std::env::set_var("MOSSRAVEN_PRICES_PATH", &out_path);
                tracing::info!(path = %out_path.display(), "ninja prices: cache fresh (<24h)");
                return;
            }
        }
    }
    let client = match reqwest::Client::builder().user_agent(UA).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "ninja: client build failed; heuristic prices");
            return;
        }
    };

    let Some(league) = current_league(&client).await else {
        tracing::warn!("ninja: league discovery failed; heuristic prices");
        return;
    };
    tracing::info!(league = %league, "ninja: current indexed league");
    // Confirmed-live currency exchange — logs the divine rate as a sanity
    // anchor and proves the wire end-to-end even while the unique-item
    // route is unconfirmed.
    if let Ok(resp) = client
        .get(format!(
            "https://poe.ninja/poe2/api/economy/exchange/1/overview?league={}&type=Currency",
            urlencode(&league)
        ))
        .send()
        .await
    {
        if let Ok(v) = resp.json::<Value>().await {
            let n = v.get("lines").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            tracing::info!(currency_lines = n, "ninja: live currency exchange confirmed");
        }
    }

    let template = std::env::var("MOSSRAVEN_NINJA_ITEM_URL")
        .unwrap_or_else(|_| DEFAULT_ITEM_URL.to_string());
    let mut prices: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for ty in UNIQUE_TYPES {
        let url = template.replace("{league}", &urlencode(&league)).replace("{type}", ty);
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(v) = resp.json::<Value>().await {
                    let n = harvest_lines(&v, &mut prices);
                    tracing::info!(ty, n, "ninja: prices harvested");
                }
            }
            Ok(resp) => {
                tracing::warn!(ty, status = %resp.status(), url = %url,
                    "ninja: item overview rejected — set MOSSRAVEN_NINJA_ITEM_URL \
                     to the route observed in browser devtools");
                break; // same template will fail for every type
            }
            Err(e) => {
                tracing::warn!(ty, error = %e, "ninja: fetch failed");
                break;
            }
        }
    }
    // Ladder meta snapshot (#74): builds search keyed by snapshot version
    // from index-state.snapshotVersions (snapshotName with '-' stripped ==
    // league url). Raw JSON cached for the seed-import / meta-distance
    // consumers; refreshed with the same 24h cadence as prices.
    if let Some((snap_ver, snap_name)) = current_snapshot(&client).await {
        let url = format!(
            "https://poe.ninja/poe2/api/builds/{snap_ver}/search?overview={snap_name}"
        );
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(body) => {
                    let meta_path = data_dir.join("ninja-meta.json");
                    match std::fs::write(&meta_path, &body) {
                        Ok(()) => tracing::info!(
                            bytes = body.len(), snapshot = %snap_name,
                            path = %meta_path.display(), "ninja: ladder meta cached"
                        ),
                        Err(e) => tracing::warn!(error = %e, "ninja: meta write failed"),
                    }
                }
                Err(e) => tracing::warn!(error = %e, "ninja: meta read failed"),
            },
            Ok(resp) => tracing::warn!(status = %resp.status(), "ninja: builds search rejected"),
            Err(e) => tracing::warn!(error = %e, "ninja: builds search failed"),
        }
    }

    if prices.is_empty() {
        tracing::info!("ninja: no live prices; cost model stays heuristic");
        return;
    }
    match serde_json::to_string(&prices)
        .map_err(anyhow::Error::from)
        .and_then(|s| std::fs::write(&out_path, s).map_err(Into::into))
    {
        Ok(()) => {
            std::env::set_var("MOSSRAVEN_PRICES_PATH", &out_path);
            tracing::info!(count = prices.len(), path = %out_path.display(), "ninja prices live");
        }
        Err(e) => tracing::warn!(error = %e, "ninja: price cache write failed"),
    }
}

fn urlencode(s: &str) -> String {
    s.replace(' ', "%20")
}

/// Newest indexed league DISPLAY NAME from the CONFIRMED index-state
/// endpoint (the API's league param wants the name, not the slug).
async fn current_league(client: &reqwest::Client) -> Option<String> {
    let v: Value = client
        .get("https://poe.ninja/poe2/api/data/index-state")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("economyLeagues")?
        .as_array()?
        .iter()
        .find(|l| l.get("indexed").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|l| l.get("name").and_then(Value::as_str))
        .map(String::from)
}

/// Current build snapshot (version id, snapshot name) from
/// index-state.snapshotVersions, matched to the indexed league by
/// "snapshotName minus hyphens == league url slug".
async fn current_snapshot(client: &reqwest::Client) -> Option<(String, String)> {
    let v: Value = client
        .get("https://poe.ninja/poe2/api/data/index-state")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let league_url = v
        .get("economyLeagues")?
        .as_array()?
        .iter()
        .find(|l| l.get("indexed").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|l| l.get("url").and_then(Value::as_str))?
        .to_string();
    let snaps = v.get("snapshotVersions")?.as_array()?;
    let pick = snaps
        .iter()
        .find(|s| {
            s.get("snapshotName")
                .and_then(Value::as_str)
                .map(|n| n.replace('-', "") == league_url)
                .unwrap_or(false)
        })
        .or_else(|| snaps.first())?;
    Some((
        pick.get("version")?.as_str()?.to_string(),
        pick.get("snapshotName")?.as_str()?.to_string(),
    ))
}

/// Pull (name → divine value) pairs out of a poe.ninja overview payload.
/// Tolerant of both PoE1-style (`lines[].{name,divineValue,chaosValue}`)
/// and exchange-style (`lines[]` + `items[]`) shapes.
fn harvest_lines(v: &Value, out: &mut std::collections::HashMap<String, f64>) -> usize {
    let mut n = 0;
    if let Some(lines) = v.get("lines").and_then(Value::as_array) {
        for l in lines {
            let Some(name) = l.get("name").and_then(Value::as_str) else { continue };
            let div = l
                .get("divineValue")
                .and_then(Value::as_f64)
                .or_else(|| l.get("chaosValue").and_then(Value::as_f64).map(|c| c / 100.0))
                .or_else(|| l.get("primaryValue").and_then(Value::as_f64));
            if let Some(d) = div {
                out.insert(name.to_lowercase(), d);
                n += 1;
            }
        }
    }
    n
}
