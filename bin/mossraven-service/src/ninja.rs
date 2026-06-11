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
//! - NOT YET FOUND: the unique-item overview route (`items/…` guesses 404).
//!   `MOSSRAVEN_NINJA_ITEM_URL` overrides the template (placeholders
//!   `{league}` and `{type}`) — confirm via browser devtools on
//!   poe.ninja/poe2/economy/<league>/unique-weapon and set the env var; no
//!   rebuild needed.
//!
//! Behavior: gated by `MOSSRAVEN_NINJA=1`. On startup, refresh a price map
//! (unique name → divine value) into `<data-dir>/ninja-prices.json` (24h
//! TTL) and point `MOSSRAVEN_PRICES_PATH` at it — the cost model overlays
//! live prices over its heuristic. Every failure degrades silently to the
//! heuristic; grounding must never block an offline run.

use serde_json::Value;

const UA: &str = "MossRaven/0.1 (build-discovery; github.com/MossSoftworks/MossRaven)";
const DEFAULT_ITEM_URL: &str =
    "https://poe.ninja/poe2/api/economy/items/1/overview?league={league}&type={type}";
const UNIQUE_TYPES: &[&str] = &[
    "UniqueWeapon",
    "UniqueArmour",
    "UniqueAccessory",
    "UniqueJewel",
    "UniqueFlask",
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
        let url = template.replace("{league}", &league).replace("{type}", ty);
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
