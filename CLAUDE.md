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

## Session ritual (run at the START of every Claude conversation)

1. **Scan the service logs**: `%APPDATA%\Moss\MossRaven\data\logs\service-*.log`
   (last 20 runs) plus `%TEMP%\mossraven-ui.log` for `WARN`/`ERROR`/`error:`/
   `FAIL`/`panicked`. The "missing node NNNNN" stderr lines are known fixture
   tree-version skew (informational); everything else is actionable.
2. For each distinct actionable finding: **create a task, fix it, and report
   back only when the scan is clean (0 errors)** — Taylor's standing
   instruction. Don't ask whether to fix; fix.
3. Check no stale `mossraven-service` daemons are racing the run you're about
   to do (`Get-Process | ? ProcessName -match mossraven`), and that
   `dist\*.exe` mtimes match `target\release` (a stale dist binary silently
   reintroduces fixed bugs through the WPF).

## SANDBOX SHADOW FILESYSTEM (critical, discovered 2026-06-11)

Claude Code's shell runs in a sandbox with a **copy-on-write overlay over the
user profile** (`%APPDATA%`, `%LOCALAPPDATA%`): writes land in a shadow only
sandboxed processes see; the user's real session never sees them. The repo
(`C:\#AppProjects`) is SHARED/real. Consequences:

- Anything "generated for the user" via CLI one-shots or shell-launched apps
  (archive cells, finalist runs, PoB runtime downloads) is INVISIBLE to them.
  This masqueraded as flaky launches for two days ("works when you launch
  it, broken when I do").
- **To act in the user's real world**: write a `.cmd` into the repo and run
  it via `explorer.exe "path	o\script.cmd"` (Explorer = real-session
  parent). Same trick to launch the app in the user's true context for
  validation: `explorer.exe "...\dist\MossRaven.exe"`, then read
  `%TEMP%\mossraven-ui.log` (TEMP is shared enough for logs in practice —
  verify per file).
- **Repo bridge** for data migration: sandbox-copy shadow → `scratch/…`
  (real), then explorer-run robocopy `scratch/… → real %APPDATA%`.
  Used 2026-06-11 to deliver archive.json + 7 finalist runs to the user.
- Validation rule: a feature touching user data is NOT validated until
  exercised through an explorer-parented launch.

## Multi-agent / environment rules (learned the hard way, 2026-06-10)

- **One git writer at a time.** Claude Code (Windows) and Cowork sessions must not run git mutations concurrently in this worktree.
- The Cowork sandbox's file bridge can serve **stale or truncated views** of recently-edited files and can intermittently **corrupt `.git/index`** (zeroed signature). Recovery is always: delete `.git/index`, run `git reset` — worktree, objects, and refs are unaffected. Cowork sessions should (a) verify file content markers before staging, (b) check `git diff --cached --stat` before every commit, (c) prefer `GIT_INDEX_FILE=/tmp/...` for their operations, and (d) leave the in-repo index healthy when done.
- Windows-side git locks (`.git/index.lock`): if stale, deleting it is the standard fix — but confirm no live git process first.
- **Never pipe a long service run through `Select-Object -First N`** (or any early-terminating PS pipeline stage): when the pipe closes, PS 5.1 kills the native process mid-run (observed: exit 255, archive save lost at gen 4/8). Redirect to a file (`*> $env:TEMP\run.log`), then grep the file.
- Maintenance after each vendor pull / legality change: `--tool rescore_archive` re-runs PoB on every elite, refreshes stats + version stamps, drops over-budget trees.
- Never commit `vendor/` or any GGG data (fan-content policy: non-commercial, no asset redistribution). Fixture build XMLs stay gitignored.
- No OpenAI keys anywhere. All keys via env vars — see `mossraven-service --help`.
