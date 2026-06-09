# PoB2 build fixtures

**31 working level-90+ builds** pulled from [pobb.in](https://pobb.in), decoded
into raw XML, validated against our vendored PoB2 (`v0.5`) via the parity test
in `../parity.rs`. Plus `seed.xml` (a copy of the canonical default —
currently `huntress-ritualist-whirling-slash.xml`).

## What's here, sorted by DPS

| File | Class / Ascendancy / Skill | DPS | Life | ES |
|---|---|---:|---:|---:|
| `sorceress-stormweaver-crit-arc.xml` | Sorceress / Stormweaver / Crit Arc (lvl 100) | **7,540,725** | — | — |
| `huntress-ritualist-flicker-strike.xml` | Huntress / Ritualist / Flicker Strike | **1,200,918** | 1 | 7,176 |
| `witch-infernalist-crit-rend.xml` | Witch / Infernalist / Crit Rend | **855,209** | 1,745 | 4,276 |
| `mercenary-gemling-legionnaire.xml` | Mercenary / Gemling Legionnaire | **577,062** | 2,010 | 0 |
| `mercenary-gemling-ice-strike.xml` | Mercenary / Gemling / Ice Strike | **303,553** | 3,840 | 402 |
| `mercenary-tactician-galvanic-shards.xml` | Mercenary / Tactician / Galvanic Shards | 162,862 | 3,035 | 281 |
| `ranger-deadeye-crit-bow-shot.xml` | Ranger / Deadeye / Crit Bow Shot | 151,532 | — | — |
| `sorceress-stormweaver-spark-95.xml` | Sorceress / Stormweaver / Spark (lvl 95) | 118,843 | — | — |
| `mercenary-gemling-tempest-bell.xml` | Mercenary / Gemling / Tempest Bell | 115,880 | 3,503 | 450 |
| `sorceress-chronomancer-crit-incinerate.xml` | Sorceress / Chronomancer / Crit Incinerate | 102,109 | — | — |
| `sorceress-chronomancer-incinerate-88.xml` | Sorceress / Chronomancer / Incinerate (lvl 88) | 94,679 | — | — |
| `monk-invoker-crit-wind-blast.xml` | Monk / Invoker / Crit Wind Blast | 87,039 | 2,253 | 0 |
| `monk-chayula-shred.xml` | Monk / Acolyte of Chayula / Shred | 65,659 | 2,327 | 681 |
| `sorceress-chronomancer-ember-fusillade.xml` | Sorceress / Chronomancer / Ember Fusillade (lvl 100) | 59,095 | — | — |
| `warrior-smith-crit-whirling-assault.xml` | Warrior / Smith of Kitava / Crit Whirling Assault | 51,828 | 2,683 | 0 |
| `huntress-ritualist-whirling-slash.xml` ← **seed.xml** | Huntress / Ritualist / Whirling Slash | 49,200 | 2,251 | 17 |
| `mercenary-gemling-rolling-magma.xml` | Mercenary / Gemling / Rolling Magma | 44,591 | 1,560 | 5,259 |
| `huntress-ritualist-whirling-slash-2.xml` | Huntress / Ritualist / Whirling Slash (alt) | 43,691 | — | — |
| `sorceress-chronomancer-crit-comet.xml` | Sorceress / Chronomancer / Crit Comet | 37,267 | — | — |
| `witch-blood-mage-crit-lightning-bolt.xml` | Witch / Blood Mage / Crit Lightning Bolt | 36,818 | 1,717 | 3,224 |
| `sorceress-stormweaver-crit-frostbolt.xml` | Sorceress / Stormweaver / Crit Frostbolt | 35,355 | — | — |
| `sorceress-stormweaver-crit-spark-96b.xml` | Sorceress / Stormweaver / Crit Spark (alt) | 34,720 | — | — |
| `sorceress-chronomancer-flameblast.xml` | Sorceress / Chronomancer / Flameblast | 34,340 | — | — |
| `ranger-deadeye-crit-lightning-rod.xml` | Ranger / Deadeye / Crit Lightning Rod | 32,140 | — | — |
| `huntress-amazon-crit-whirling-slash.xml` | Huntress / Amazon / Crit Whirling Slash | 29,698 | 1,781 | 93 |
| `sorceress-chronomancer-incinerate-92.xml` | Sorceress / Chronomancer / Incinerate (lvl 92) | 29,283 | — | — |
| `monk-chayula-fireball.xml` | Monk / Acolyte of Chayula / Fireball | 22,804 | 1 | 8,248 |
| `sorceress-stormweaver-crit-spark-96.xml` | Sorceress / Stormweaver / Crit Spark (lvl 96) | 15,697 | — | — |
| `ranger-pathfinder-poisonburst-arrow.xml` | Ranger / Pathfinder / Poisonburst Arrow | 3,981 | 2,137 | 0 |
| `sorceress-stormweaver-bone-blast.xml` | Sorceress / Stormweaver / Bone Blast | 2,782 | — | — |
| `witch-crit-eye-of-winter.xml` | Witch / Crit Eye of Winter | 2,556 | 1,354 | 1,547 |

DPS uses the headline `total_dps` from PobParser; many builds (DoT, totem,
multi-projectile, buffed) understate true effective DPS via this single
number. Treat as a relative ranking, not an absolute claim.

`seed.xml` is the default the service auto-loads. Override via the
`MOSSRAVEN_SEED_XML_PATH` env var.

## Coverage

| Class | Ascendancy | Working fixtures |
|---|---|---|
| Warrior | Smith of Kitava | 1 (Crit Whirling Assault) |
| Mercenary | Gemling Legionnaire | 4 (Ice Strike, Rolling Magma, Tempest Bell, generic) |
| Mercenary | Tactician | 1 (Galvanic Shards) |
| Monk | Invoker | 1 (Crit Wind Blast) |
| Monk | Acolyte of Chayula | 2 (Fireball, Shred) |
| Witch | Infernalist | 1 (Crit Rend) |
| Witch | Blood Mage | 1 (Crit Lightning Bolt) |
| Witch | generic | 1 (Crit Eye of Winter) |
| Sorceress | Stormweaver | 6 (Crit Arc, Spark x3, Frostbolt, Bone Blast) |
| Sorceress | Chronomancer | 6 (Crit Incinerate, Incinerate x2, Ember Fusillade, Crit Comet, Flameblast) |
| Huntress | Ritualist | 3 (Flicker Strike, Whirling Slash x2) |
| Huntress | Amazon | 1 (Crit Whirling Slash) |
| Ranger | Deadeye | 2 (Crit Bow Shot, Crit Lightning Rod) |
| Ranger | Pathfinder | 1 (Poisonburst Arrow) |

Damage spans melee strike, spell crit, DoT, totem, projectile, AoE.
Defense layers span pure-life, pure-ES, hybrid, evasion-stack.

## Known coverage gap

**Warbringer, Titan, and most Witch/Lich** ascendancies pulled but scored 0 DPS
against our vendored PoB2 — fixtures were authored against PoB2 0.1 and
reference passive nodes that no longer exist in 0.5. Pruned because they'd
be misleading as seeds. To fill these gaps: open desktop PoB2 0.5+, build a
modern Warrior or Lich, export, and add the slug to `CANDIDATES` in the
puller script.

## How fixtures were obtained

Bulk-pulled via [`scripts/pull-pob-fixtures.py`](../../../../scripts/pull-pob-fixtures.py).
Each pobb.in slug is fetched from `pobb.in/<slug>/raw` (URL-safe base64 of
zlib-deflated XML), decoded, and saved here.

## Adding more

1. Find a build on [pobb.in](https://pobb.in/).
2. Grab the slug from the URL.
3. Add `(slug, "descriptive-name", "Class / Ascendancy / Skill")` to
   `CANDIDATES` in [`scripts/pull-pob-fixtures.py`](../../../../scripts/pull-pob-fixtures.py).
4. Re-run the script.
5. `cargo test -p mossraven-pob --test parity -- --ignored --nocapture` to
   verify it scores. Delete if `total_dps == 0` (tree-version mismatch).

## Why these aren't in git

GGG fan-content policy is non-commercial; PoB exports embed GGG-owned skill
and passive IDs. All `.xml` files in this directory are **gitignored**.
README + parity test + puller script live in git; the data does not.
Anyone with the repo regenerates the fixtures in one command.

## Per-fixture strict bounds

Drop a `<name>.expected.json` next to any fixture for tight bounds (otherwise
loose `DPS > 0 ∧ life-or-ES > 0 ∧ resists in [-100,95]` only):

```json
{
  "total_dps_min": 40000,
  "total_dps_max": 60000,
  "life_min": 2000,
  "ehp_min": 4000,
  "notes": "huntress whirling slash, league start, version 0.5.0"
}
```
