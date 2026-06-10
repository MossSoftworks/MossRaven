# CLAUDE.md — MossRaven

POE2 build-discovery engine: LLM-driven MAP-Elites quality-diversity search with
PoB2's calc engine as the fitness function. **SPEC.md is authoritative** (§1.1 =
definition of done). Keep README's status table truthful when component states change.

## Build & test (Windows is the primary platform)

- `cargo build --workspace --release`
- `cargo test -p mossraven-core -p mossraven-surrogate -p mossraven-archive -p mossraven-node-protocol`
- `cargo test -p mossraven-pob --test init_smoke -- --nocapture` (loads the real Lua VM)
- `cargo test -p mossraven-pob --test parity -- --ignored --nocapture` (fixtures vs desktop PoB2)
- WPF: `dotnet publish ui/MossRaven/MossRaven.csproj -c Release -r win-x64 --self-contained -p:PublishSingleFile=true -o dist\`
- **One-command gate: `scripts/windows-validate.ps1`** (vendor pull → build → tests → smoke drive → dist refresh)

## Run / drive

- `dist/mossraven-service.exe` — no args = MCP stdio daemon; `--headless`; `--tool NAME --tool-args JSON` one-shots. `--help` lists all tools + env vars.
- Claude Code Mode B setup: `docs/claude-code-mcp-setup.md`. Drive flow: `seed_hypothesis` → `run_search` (repeat; read `read_archive` for gaps) → `synthesize_finalists` → curate → `save_finalists`.
- Data: `%APPDATA%\Moss\MossRaven\data\` (archive.json, session.json, `finalists/<ts>/`); logs in `...\logs\` (last 20 service runs).

## League currency (SPEC §1.1 "current-patch" is a living requirement)

- `vendor/PathOfBuilding-PoE2` is a git clone: `git -C vendor/PathOfBuilding-PoE2 pull` after each league/patch, then re-run parity and re-check the `lua-utf8` stub note in `crates/pob/src/lib.rs`.
- Surrogate vocab: refresh `scratch/poe2-mcp`, then `python scripts/extract-poe2-vocab.py`.
- Archive entries are stamped `pob2:<version>` from the vendor `manifest.xml`; mismatched stamps mean re-score before trusting.

## Multi-agent / environment rules (learned the hard way, 2026-06-10)

- **One git writer at a time.** Claude Code (Windows) and Cowork sessions must not run git mutations concurrently in this worktree.
- The Cowork sandbox's file bridge can serve **stale or truncated views** of recently-edited files and can intermittently **corrupt `.git/index`** (zeroed signature). Recovery is always: delete `.git/index`, run `git reset` — worktree, objects, and refs are unaffected. Cowork sessions should (a) verify file content markers before staging, (b) check `git diff --cached --stat` before every commit, (c) prefer `GIT_INDEX_FILE=/tmp/...` for their operations, and (d) leave the in-repo index healthy when done.
- Windows-side git locks (`.git/index.lock`): if stale, deleting it is the standard fix — but confirm no live git process first.
- Never commit `vendor/` or any GGG data (fan-content policy: non-commercial, no asset redistribution). Fixture build XMLs stay gitignored.
- No OpenAI keys anywhere. All keys via env vars — see `mossraven-service --help`.
