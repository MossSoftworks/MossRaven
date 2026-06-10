#!/usr/bin/env python3
"""Bulk-fetch + decode PoB2 build exports from pobb.in into local fixtures.

For each (slug, descriptive_name) in CANDIDATES:
  1. GET https://pobb.in/{slug}/raw with a real browser User-Agent.
  2. URL-safe-base64-decode + zlib.decompress -> raw XML.
  3. Save to crates/pob/tests/fixtures/{descriptive_name}.xml.

Skips silently on HTTP error or decode failure (logs the issue, keeps going).
Fixtures dir is gitignored per GGG fan-content policy.

Usage:
    python scripts/pull-pob-fixtures.py
"""

import base64
import os
import sys
import urllib.error
import urllib.request
import zlib

# (pobb.in_slug, descriptive_filename_without_extension, class_ascendancy_skill_label)
CANDIDATES = [
    # Druid (new class in 0.5 "Return of the Ancients") — sourced from the
    # official-forum Build of the Week thread 3891806 (poe2db.tw pastes serve
    # /raw with the same encoding as pobb.in).
    ("https://poe2db.tw/pob/NQJdeLUuy8", "druid-shaman-tornado", "Druid / Shaman / Tornado (mapper)"),
    ("https://poe2db.tw/pob/YTpuJs9EJI", "druid-oracle-tornado", "Druid / Oracle / Tornado (boss nuker)"),
    ("JRM_JzYr625Y", "huntress-ritualist-whirling-slash",      "Huntress / Ritualist / Whirling Slash"),
    ("1ff_AZtMMrQY", "mercenary-tactician-galvanic-shards",   "Mercenary / Tactician / Galvanic Shards"),
    ("KCevYBaIJo8e", "warrior-warbringer-seismic-cry",         "Warrior / Warbringer / Seismic Cry"),
    ("2IpzgmW1Oyuo", "ranger-deadeye",                          "Ranger / Deadeye"),
    ("_gMYZPiRJZcY", "ranger-pathfinder-poisonburst-arrow",    "Ranger / Pathfinder / Poisonburst Arrow"),
    ("I2b9eMPVuO6f", "warrior-smith-crit-whirling-assault",    "Warrior / Smith of Kitava / Crit Whirling Assault"),
    ("mT21QJHQrRN6", "warrior-titan",                           "Warrior / Titan"),
    ("jZeGcZsl8GYm", "monk-invoker-crit-wind-blast",            "Monk / Invoker / Crit Wind Blast"),
    ("p_Rcv7GmDxJd", "huntress-amazon-crit-whirling-slash",    "Huntress / Amazon / Crit Whirling Slash"),
    ("hizt6dz0lWrI", "mercenary-witchhunter-explosive-grenade","Mercenary / Witchhunter / Explosive Grenade"),
    ("KstjsDZoeVSp", "witch-lich-skeletal-frost-mage",          "Witch / Lich / Skeletal Frost Mage"),
    ("ktd4PB9ILt7t", "witch-lich-skeletal-storm-mage",          "Witch / Lich / Skeletal Storm Mage"),
    ("85mnIrsvDAle", "witch-infernalist-crit-rend",             "Witch / Infernalist / Crit Rend"),
    ("nbT07lyVk6mw", "witch-lich-raging-spirits",               "Witch / Lich / Raging Spirits"),
    ("xzd1F3VrPYCV", "witch-blood-mage",                        "Witch / Blood Mage"),
    ("Q5SePBUewa9P", "witch-infernalist-crit-ball-lightning",   "Witch / Infernalist / Crit Ball Lightning"),
    ("hxvdiWlzFizu", "witch-blood-mage-life-remnants",          "Witch / Blood Mage / Life Remnants"),
    ("RE-66Uwczoyt", "witch-blood-mage-crit-lightning-bolt",    "Witch / Blood Mage / Crit Lightning Bolt"),
    ("X1bbd9qU_kpE", "witch-blood-mage-hybrid-life-remnants",   "Witch / Blood Mage / Hybrid Life Remnants"),
    ("_690Qaa3D1um", "witch-crit-eye-of-winter",                "Witch / Crit Eye of Winter"),
    ("3qVMEwZkvTTK", "mercenary-gemling-legionnaire",           "Mercenary / Gemling Legionnaire"),
    ("oZ4yKJ7s8z5C", "mercenary-gemling-ice-strike",            "Mercenary / Gemling Legionnaire / Ice Strike"),
    ("rMzIewN-Pil5", "mercenary-gemling-rolling-magma",         "Mercenary / Gemling Legionnaire / Rolling Magma"),
    ("TDbPbA8Vkayu", "monk-chayula-fireball",                   "Monk / Acolyte of Chayula / Fireball"),
    ("qamr2p0p1gLT", "monk-chayula-shred",                      "Monk / Acolyte of Chayula / Shred"),
    ("k9P_-5OzYcco", "mercenary-gemling-tempest-bell",          "Mercenary / Gemling Legionnaire / Tempest Bell"),
    ("HoFq_ougCFld", "huntress-ritualist-flicker-strike",       "Huntress / Ritualist / Flicker Strike"),
    # ---- second batch (added after initial 27) ----
    # Warrior
    ("zOGz21pIEhZq", "warrior-warbringer-ancestral-spirits-94",  "Warrior / Warbringer / Ancestral Spirits (lvl 94)"),
    ("xnrTMT9btqzc", "warrior-warbringer-ancestral-spirits-91",  "Warrior / Warbringer / Ancestral Spirits (lvl 91)"),
    ("UXlhVslEO-Kx", "warrior-warbringer-92",                    "Warrior / Warbringer (lvl 92)"),
    ("rUBLenkVvdNr", "warrior-warbringer-ancestral-spirits-92",  "Warrior / Warbringer / Ancestral Spirits (lvl 92)"),
    ("Ik8dP8eKTChe", "warrior-smith-companion-chimeral",         "Warrior / Smith of Kitava / Companion Prowling Chimeral"),
    ("xSGc49orrdLP", "warrior-smith-crit-sunder",                "Warrior / Smith of Kitava / Crit Sunder"),
    # Ranger
    ("hPiZBUlSG_H_", "ranger-deadeye-crit-lightning-rod",        "Ranger / Deadeye / Crit Lightning Rod"),
    ("bVUTuVSSkCXy", "ranger-deadeye-crit-bow-shot",             "Ranger / Deadeye / Crit Bow Shot"),
    # Huntress
    ("D3pHgWesf6Rw", "huntress-ritualist-whirling-slash-2",      "Huntress / Ritualist / Whirling Slash (alt)"),
    ("YfIEw9xftUn9", "huntress-ritualist-blood-boil",            "Huntress / Ritualist / Blood Boil"),
    ("T_Ux-GKUYfPE", "huntress-ritualist-ritual-sacrifice",      "Huntress / Ritualist / Ritual Sacrifice"),
    ("DbeQNkDAEd_7", "huntress-spirit-walker-wild-protector",    "Huntress / Spirit Walker / Wild Protector"),
    ("Z7hGG0tfYnM_", "huntress-spirit-walker-companion-zekoa",   "Huntress / Spirit Walker / Companion Zekoa"),
    # Sorceress / Stormweaver
    ("htLxuXrVMVlX", "sorceress-stormweaver-spark-95",           "Sorceress / Stormweaver / Spark (lvl 95)"),
    ("74Yg9hmD1DSP", "sorceress-stormweaver-pinnacle-of-power",  "Sorceress / Stormweaver / Pinnacle of Power"),
    ("KuET8Do31cQm", "sorceress-stormweaver-crit-spark-96",      "Sorceress / Stormweaver / Crit Spark (lvl 96)"),
    ("dQm2aha1_M1p", "sorceress-stormweaver-crit-frostbolt",     "Sorceress / Stormweaver / Crit Frostbolt"),
    ("K-Il3ZqRcZ73", "sorceress-stormweaver-crit-arc",           "Sorceress / Stormweaver / Crit Arc (lvl 100)"),
    ("1bc_19mHOyE2", "sorceress-stormweaver-crit-spark-96b",     "Sorceress / Stormweaver / Crit Spark (alt)"),
    ("oGW0Uw0jLofl", "sorceress-stormweaver-bone-blast",         "Sorceress / Stormweaver / Bone Blast"),
    # Sorceress / Chronomancer
    ("Ek39qkH9UJAZ", "sorceress-chronomancer-ember-fusillade",   "Sorceress / Chronomancer / Ember Fusillade (lvl 100)"),
    ("t2I21MvvBCx1", "sorceress-chronomancer-incinerate-88",     "Sorceress / Chronomancer / Incinerate (lvl 88)"),
    ("ed3ElGNEmfLp", "sorceress-chronomancer-crit-nimble-reload","Sorceress / Chronomancer / Crit Nimble Reload"),
    ("IGdyU6UD_zl9", "sorceress-chronomancer-crit-incinerate",   "Sorceress / Chronomancer / Crit Incinerate"),
    ("0lePAwdBEde8", "sorceress-chronomancer-temporal-rift",     "Sorceress / Chronomancer / Temporal Rift"),
    ("CUlquyyrnpmT", "sorceress-chronomancer-inevitable-agony",  "Sorceress / Chronomancer / Inevitable Agony"),
    ("wvO0qql0N3dl", "sorceress-chronomancer-crit-comet",        "Sorceress / Chronomancer / Crit Comet"),
    ("va1v0jYb_EcY", "sorceress-chronomancer-flameblast",        "Sorceress / Chronomancer / Flameblast"),
    ("8qqhTovVRvsT", "sorceress-chronomancer-incinerate-92",     "Sorceress / Chronomancer / Incinerate (lvl 92)"),
]

UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/120.0.0.0 Safari/537.36"
)

FIXTURES_DIR = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "crates",
    "pob",
    "tests",
    "fixtures",
)


def fetch_raw(slug: str) -> bytes:
    # `slug` may be a bare pobb.in slug OR a full URL (poe2db.tw/pob/<id> also
    # serves the raw code at <url>/raw with the same encoding).
    url = slug if slug.startswith("http") else f"https://pobb.in/{slug}/raw"
    if slug.startswith("http") and not url.endswith("/raw"):
        url += "/raw"
    req = urllib.request.Request(
        url,
        headers={"User-Agent": UA, "Accept": "text/plain"},
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        return resp.read()


def decode_pob_export(code: bytes) -> bytes:
    s = code.strip().decode("ascii")
    s += "=" * (-len(s) % 4)
    return zlib.decompress(base64.urlsafe_b64decode(s))


def main() -> int:
    os.makedirs(FIXTURES_DIR, exist_ok=True)
    saved = 0
    skipped = 0
    for slug, fname, label in CANDIDATES:
        out_path = os.path.join(FIXTURES_DIR, fname + ".xml")
        try:
            raw = fetch_raw(slug)
            xml = decode_pob_export(raw)
        except urllib.error.HTTPError as e:
            print(f"[skip] {slug:14s} HTTP {e.code} -- {label}")
            skipped += 1
            continue
        except Exception as e:
            print(f"[skip] {slug:14s} {type(e).__name__}: {e} -- {label}")
            skipped += 1
            continue

        with open(out_path, "wb") as f:
            f.write(xml)
        print(f"[ok]   {slug:14s} {len(xml):>6d} bytes -> {fname}.xml")
        saved += 1

    print()
    print(f"Saved {saved}/{len(CANDIDATES)} fixtures, skipped {skipped}.")
    print(f"Run: cargo test -p mossraven-pob --test parity -- --ignored --nocapture")
    print("to see which actually score against the vendored PoB2.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
