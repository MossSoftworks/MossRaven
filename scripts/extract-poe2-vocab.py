"""Extract POE2 skill + support gem vocabulary from HivemindOverlord/poe2-mcp.

Produces `crates/surrogate/src/poe2_vocab.json` — a compact name+tags list the
Cerebras surrogate uses to ground its mutation proposals in real datamined
gem names instead of LLM-hallucinated PoE1 vocabulary.

Source:  scratch/poe2-mcp/data/poe2_spell_gems_database.json
         scratch/poe2-mcp/data/poe2_support_gems_database.json
Upstream: https://github.com/HivemindOverlord/poe2-mcp (MIT)

Re-run after pulling a fresh poe2-mcp data bundle. The script is idempotent and
safe to commit the output of — the vocab JSON is small enough (~10–30 KB) to
embed in the Rust binary via include_str!.
"""

from __future__ import annotations

import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRATCH = REPO_ROOT / "scratch" / "poe2-mcp" / "data"
OUT = REPO_ROOT / "crates" / "surrogate" / "src" / "poe2_vocab.json"


def load(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def extract_skills(spells_db: dict) -> list[dict]:
    """Walk the by-element groupings and produce a flat skill list."""
    skills: list[dict] = []
    seen: set[str] = set()
    for group_key, group in spells_db.items():
        if group_key == "metadata" or not isinstance(group, dict):
            continue
        for _spell_id, entry in group.items():
            if not isinstance(entry, dict):
                continue
            name = entry.get("name")
            if not name or name in seen:
                continue
            seen.add(name)
            skills.append({
                "name": name,
                "element": entry.get("element"),
                "tags": entry.get("tags", []),
            })
    skills.sort(key=lambda s: s["name"])
    return skills


def extract_supports(supports_db: dict) -> list[dict]:
    """Walk all support-gem buckets and produce a flat support list."""
    supports: list[dict] = []
    seen: set[str] = set()

    def absorb(bucket: dict):
        for _id, entry in bucket.items():
            if not isinstance(entry, dict):
                continue
            name = entry.get("name")
            if not name or name in seen:
                continue
            seen.add(name)
            supports.append({
                "name": name,
                "tags": entry.get("tags", []),
                "compatible_with": entry.get("compatible_with", []),
            })

    for key in ("support_gems", "lineage_support_gems"):
        bucket = supports_db.get(key)
        if isinstance(bucket, dict):
            absorb(bucket)

    extra = supports_db.get("additional_support_gems_by_category")
    if isinstance(extra, dict):
        for _cat, sub in extra.items():
            if isinstance(sub, dict):
                absorb(sub)
            elif isinstance(sub, list):
                # list-of-objects form
                for item in sub:
                    if isinstance(item, dict) and item.get("name") and item["name"] not in seen:
                        seen.add(item["name"])
                        supports.append({
                            "name": item["name"],
                            "tags": item.get("tags", []),
                            "compatible_with": item.get("compatible_with", []),
                        })

    supports.sort(key=lambda s: s["name"])
    return supports


def main() -> int:
    spells_path = SCRATCH / "poe2_spell_gems_database.json"
    supports_path = SCRATCH / "poe2_support_gems_database.json"
    if not spells_path.exists() or not supports_path.exists():
        print(
            f"missing source files under {SCRATCH}. "
            "Clone https://github.com/HivemindOverlord/poe2-mcp into scratch/poe2-mcp first."
        )
        return 2

    spells_db = load(spells_path)
    supports_db = load(supports_path)

    skills = extract_skills(spells_db)
    supports = extract_supports(supports_db)

    out = {
        "source": "HivemindOverlord/poe2-mcp",
        "license": "MIT",
        "game_version": spells_db.get("metadata", {}).get("game_version")
        or supports_db.get("metadata", {}).get("version"),
        "skills": skills,
        "supports": supports,
    }

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {OUT} — {len(skills)} skills, {len(supports)} supports")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
