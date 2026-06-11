# MossRaven — POE2 Build-Discovery Engine

**Status:** v1 spec, pre-scaffold.
**Family:** Moss (MossPost, MossNote, MossNiche, MossPlane).
**Date frozen:** 2026-06-08.

---

## 1. Goal

Self-hosted tool that **finds** novel, viable, fun POE2 builds the meta hasn't surfaced — "should theoretically work," "works if we tweak xyz." **Not a Q&A oracle.** It is an LLM-driven **Quality-Diversity evolutionary search** where Path of Building 2's calc engine is the fitness function.

Distribution target: **single Windows app any POE2 player can install** and run on their own gaming rig. Optional power-user mode points the simulation tier at a remote node-pool (e.g., a homelab) for higher throughput.

No OpenAI dependency anywhere.

### 1.1 End state — definition of done (v2, 2026-06-11)

A completed discovery session delivers **exactly 5 curated builds**, chosen by
Tier 6 from a Tier-5 **selection pool of 15–20 candidates** (§3.1a separates
selection from authorship). Each curated build ships:

1. **Current-patch PoB2 XML + import code** — importable into desktop PoB2,
   generated against the vendored PoB2 version and version-stamped. "Current"
   is a living requirement: keep `vendor/PathOfBuilding-PoE2` tracking the
   live game, and run `rescore_archive` after every vendor pull.
2. **Build + leveling guide with 5 checkpoints** — fixed waypoints, each with
   the gems to run, passives to take, and gear to look for at that point:
   - **CP1** Acts 1–2 (≈ lvl 1–25)
   - **CP2** Act 3 + Cruel entry (≈ 25–45)
   - **CP3** Cruel done / maps entry (≈ 45–65)
   - **CP4** Early maps + ascendancy complete (≈ 65–85)
   - **CP5** Pinnacle-ready endgame (≈ 85+, the shipped PoB XML)
3. **Bossing guide** — single-target loadout, defensive swaps, fight-specific
   notes (what kills this build and what to do about it).
4. **Clearing/mapping guide** — clear-speed loadout, pack-handling pattern,
   map-mod warnings (mods this build cannot run).
5. **Dual-loadout design for clear vs. boss** — one character, minimal
   switching friction via PoE2's weapon-set swap (PoB2 XML already encodes
   it); the finalist XML carries both loadouts. If a build can't dual-loadout
   cleanly, its guide must say so explicitly.
6. **All-content viability** — hard floors on PoB-scored stats (§1.1.1) plus
   **passive-point legality** (PoB-sourced budget accounting; over-budget
   trees are unbuildable in-game and never ship). A finalist failing the gate
   is never silently presented as endgame-ready — explicit `viability: FAIL`
   plus the verbatim failure list. Floors are league-currency: revisit each
   patch.
7. **Cost estimate + value notes** — estimated gear cost (band + breakdown,
   §1.1.2) surfaced on the build card and in the guide, with explicit value
   tradeoffs. Rule the curator must apply: **giving up 1M DPS on a 10M build
   to save 90% of the cost is a WIN** — effectiveness-per-divine beats raw
   ceiling for most players. Budget variants get called out where they exist.

**Output formats per build:** PoB import code (`.txt`), PoB XML (`.xml`),
markdown guide (`.md`), and a **copy-paste-ready standalone HTML page**
(`.html`, inline CSS, zero external assets) suitable for pasting into
forums or a site. A session-level `index.html` links all five.

#### 1.1.1 Viability floors (v1 — PoE2 0.5 "Runes of Aldur")

Community DPS bands (0.5): **50–100k** = entry-pinnacle baseline; **500k+** =
comfortably farming Tier-0 pinnacles; **10M+** = min-maxed meta. "Easily and
cleanly clears all content" maps to the COMFORT band — that is the PASS floor —
and the achieved band is always reported alongside pass/fail.

| Stat | Floor | Rationale |
|---|---|---|
| `total_dps` | ≥ 500,000 (PASS) | comfort band — comfortably farming T0 pinnacles; bands reported: <50k below-entry · 50–100k entry · 100–500k capable · 500k+ comfort · 10M+ meta |
| `effective_hp` | ≥ 5,000 | survives endgame white-mob burst (phys max-hit proxy) |
| fire / cold / lightning res | = 75 (capped) | uncapped elemental res is the #1 pool shredder |
| chaos res | ≥ −30 | chaos-heavy 0.5 endgame (Fate of the Vaal systems) |

The gate is **reported, not filtering, in v1** — failing cells stay in the
archive (a failing cell is still discovery signal), but the frontier API and
every guide must surface pass/fail + failures. Exception: **passive-point
legality IS filtering** — the engine refuses to archive over-budget trees
(PoB scores them happily; an evolutionary loop would inflate forever).

#### 1.1.2 Cost layer (v1 — heuristic bands)

Where cost lives in the pipeline (decided 2026-06-11):

1. **Estimated at scoring time** (Tier 3 adjacency) — a pure function of the
   build XML: uniques classed into price bands, rares banded by mod density,
   summed into `estimated_cost_div` + `cost_band` ∈
   {budget ≤ 5 div · mid ≤ 30 · expensive ≤ 150 · mirror-tier > 150}.
   Stamped on every `ArchiveEntry`.
2. **Surfaced everywhere** — frontier JSON, viability report, build cards,
   guides.
3. **Curated on at Tier 6** — the value rule (§1.1 item 7) is an explicit
   curation criterion: the 5 finalists should span cost bands when the pool
   allows, and every guide writes its cost reality plus the cheapest
   acceptable variant.

v1 prices are HEURISTIC (no live market). Follow-up: poe.ninja PoE2 economy
API as an env-gated live source; heuristic stays as offline fallback.

#### 1.1.3 Tier-6 curation (selection ≠ authorship)

Tier 5 SELECTS a pool: 15–20 candidates from the frontier, each a one-line
pitch + stats + cost — cheap tokens, breadth over depth. Tier 6 CURATES and
WRITES: picks exactly 5 balancing power, cost spread, playstyle diversity,
and viability honesty, then writes the full §1.1 guide set per pick. Both
tiers run on the same Tier-1/5 driver chain (Anthropic or OpenAI-compat).
Adversarial critique (§3.6) wraps both stages.

Status (2026-06-11): the loop produces archives + finalists end-to-end (live
runs 1781112410 → 1781127587). Tier-6 split, cost layer, HTML output, and
adversarial critics are the active build-out.

Tracked implications:

- Tier-5/6 synthesis must emit guide prose per §1.1 (checkpoints, bossing,
  mapping), not just stats and an import code.
- Variant representation and mutation operators must become weapon-set aware,
  so clear/boss duality is *searched*, not bolted on afterwards. (DONE:
  SetActiveWeaponSet op + weapon-set walls in tree pathing.)
- The viability gate needs richer inputs over time (sustain, ailment
  immunity, movement) — v1's floors are the deliberately-blunt start.
- Guides are only as honest as their fact-checking — §3.6 adversarial pass
  is mandatory for shipped finalists.

---

## 2. Prior art this implements (don't reinvent — read these first)

- **FunSearch / AlphaEvolve (DeepMind)** — the loop: LLM proposes candidates → deterministic evaluator scores → evolve winners. AlphaEvolve uses a fast-model + smart-model ensemble for exactly the same reason this spec splits Tier 2 + Tier 1.
- **MAP-Elites / Quality-Diversity** — the **objective**. Don't optimize toward one "best" build (that just rediscovers the meta, which is a deceptive local optimum). Maintain an archive of behavioral niches; fill each with its best example. **The archive is the product.** An empty high-potential cell = "a build that should work but nobody found."
- **SAIL (Surrogate-Assisted Illumination)** — cheap surrogate prunes candidates before the expensive evaluator runs. ~20-year track record. Tier 2 = surrogate, PoB = expensive evaluator.
- **OpenEvolve** — open-source AlphaEvolve reimplementation. Steal its four modules: prompt sampler, MAP-Elites DB, cascade evaluator, evolution controller. Read its design before writing our own.

---

## 3. Architecture

### 3.1 Three compute tiers + archive

```
Tier 1 — THE DREAMER                       [dozens of calls / session]
   Hypothesis seeding + curation. Reads the archive, reasons about
   empty high-potential cells, forms next hypothesis.
   Pluggable backend (see §4.1): API driver  OR  Claude Code / Cowork.
        │
Tier 2 — THE SURROGATE                     [thousands of calls / session]
   Proposes mutations, cheap-scores plausibility/novelty,
   prunes the candidate pool before expensive sim.
   Provider-agnostic OpenAI-compatible interface (see §4.2):
   Cerebras (default) | local Ollama/llama.cpp | Groq | OpenRouter.
   Swap = change base_url + model. Same code.
        │
Tier 3 — THE JUDGE                         [100k+ evals / session]
   pob-headless. Deterministic fitness: DPS, EHP, sustain, resist caps,
   breakpoints. CPU-bound Lua, NOT MCP-wrapped (inner loop hammers it).
   No Claude, no network in this tier (in local mode).
   Backend swappable (see §4.3): `local` (in-process, host cores)
                              OR `remote` (POSTs to mossraven-node URL).
        │
MAP-Elites ARCHIVE  ← THE SEARCH OUTPUT
   Grid keyed on archetype axes (damage type × defense layer ×
   clear/boss × scaling vector). Each cell = best build of that type;
   empty cell = undiscovered niche. Persisted to disk; resumes across
   sessions; readable by Tier 1.
        │
Tier 4 — THE PRUNE                          [post-sim gates]
   Mechanical legality + viability: passive point budget (filtering),
   stat floors (reported), label normalization. No LLM.
        │
Tier 5 — THE SELECTOR                       [a few calls / session]
   Reads the frontier, nominates a POOL of 15–20 candidates
   (one-line pitch + stats + cost each). Breadth, not prose.
        │
Tier 6 — THE CURATOR-AUTHOR                 [a few quality calls / session]
   Picks exactly 5 from the pool (power × cost spread × playstyle
   diversity × viability honesty), then writes the full guide set
   per pick (§1.1: 5 checkpoints, bossing, mapping, cost/value).
   Outputs: pool.json, finalists.json, .md + .html + .xml + import code.
```

Every LLM-bearing tier (1, 2, 5, 6) is wrapped by an **adversarial critic
pass** (§3.6).

### 3.2 Why the tiers split this way

Cerebras (or any LLM-throughput accelerator) only helps where an LLM is in the hot loop — Tier 2. Tier 3 is CPU-bound Lua and parallelizes across cores, not silicon-LLM. Claude stays at Tier 1 because Tier 2's open model is dumber — fine for pruning, not for the final verdict.

### 3.3 The loop (cascade evaluator, FunSearch-style)

```
seed concept (Tier 1)
  → mutate variant space
  → Tier 2 surrogate: cheap-filter for plausible + novel       (prune)
  → Tier 3 pob-headless: hard numbers on survivors only       (expensive)
  → place in MAP-Elites cell IF it beats that niche's current elite
  → Tier 1 reads filled + empty cells → new hypothesis → repeat
```

Prompt sampler feeds Tier 1 **diverse sub-optimal** archive members as inspiration, not just the current best — prevents mode-collapse back to the meta. (OpenEvolve does this; we copy it.)

### 3.6 Adversarial critics (every LLM tier)

Pattern: **generate → critique → revise once**. The critic is a second LLM
call (same failover chain) prompted to REFUTE — find concrete, checkable
errors — and return a structured verdict. The generator gets one revision
against the critique; the revised output ships with the verdict attached.
Never more than one round (free-tier token budget; diminishing returns).

| Tier | Critic checks | Gate |
|---|---|---|
| T1 hypothesis | mechanics coherence (does the concept's scaling exist in PoE2? are named skills/items real per the datamined vocab?) | always |
| T2 mutations | known no-ops (support gem levels), illegal ops, duplicate variants, out-of-budget allocations | env-gated `MOSSRAVEN_T2_CRITIC=1` (token cost vs. Tier-3 already being ground truth) |
| T5 pool | selection diversity (cost bands, playstyles, classes), pitch claims vs. attached stats | always |
| T6 guides | fact-check prose against the actual build: every gem named must exist in the XML; viability band quoted correctly; cost claims match the estimate; checkpoint passives must be real notables | always |

The T6 critic is the highest-value adversary: guide hallucination is the
visible product flaw. Mechanical pre-checks (gem-name-in-XML, band-string
match) run in Rust BEFORE the LLM critic — never spend tokens on what a
string search can catch.

---

## 4. Execution model — what's swappable, at what seam

One core. Three swap points, each behind a Rust trait. Mode is selected by config file, not by code.

### 4.1 Tier 1 driver — dual mode (`TierOneDriver` trait)

The search service's **outer control surface** is itself an MCP server. Tools (low frequency):

- `seed_hypothesis(concept)`
- `run_search(region, generations)`
- `read_archive()`
- `inspect_cell(coords)`
- `get_frontier()`

The autonomous inner loop (mutate → surrogate-prune → PoB-sim → archive-place) runs **inside the service** and is Claude-free. It can grind unattended without consuming any Claude calls.

**Mode A — API driver (headless / automated):**
A thin Rust driver process holds `ANTHROPIC_API_KEY`, calls Claude directly, calls the core. Fully automated, schedulable. Metered at API rates. Use for unattended long runs.

**Mode B — Subscription / interactive (uses your plan):**
Claude Code **or** Cowork connects to the service's MCP server and **is** Tier 1. User drives interactively; draws from Pro/Max usage pool.

| Transport | Reachability | When to use |
|---|---|---|
| **Local stdio MCP** (Claude Code config file) | Runs on host, no exposure | **Preferred** for security |
| **Custom connector** (Cowork / Settings → Connectors) | Connects from Anthropic's cloud — MCP server must be publicly reachable | Only when remote driving is required |

Mode B caveats:
- Must stay **genuinely interactive**. As of 2026-06-15, headless `claude -p` / Agent SDK driving falls into a separately-metered credit pool at API rates — that defeats Mode B.
- `ANTHROPIC_API_KEY` **must not** be set in the Claude Code shell environment in Mode B, or Claude Code silently switches off the subscription onto API billing.
- Pro/Max usage limits shared across Claude + Claude Code, with 5-hour + weekly caps. Fine for low Tier-1 cadence.

Mode A and Mode B are two implementations of one `TierOneDriver` trait. Same core underneath.

### 4.2 Tier 2 surrogate — provider swap (`SurrogateProvider` trait)

OpenAI-compatible chat completions. Swap = change `base_url` + `model` in config; zero code change.

| Provider | base_url | Use when |
|---|---|---|
| **Cerebras** (default) | `https://api.cerebras.ai/v1` | Production. Free tier: 1M tok/day, 30 RPM. Models: `gpt-oss-120b`, `zai-glm-4.7`, `llama3.1-8b`. |
| **Local Ollama** | `http://localhost:11434/v1` | Cost-zero, no rate cap. CPU-only inference is slower; only useful if homelab GPU exists. |
| **Groq / OpenRouter / etc.** | provider URL | Fallback if Cerebras throttles. |

### 4.3 Tier 3 backend — local or remote (`Tier3Backend` trait)

**Default (v1): `local`.** In-process `pob-headless` fans across host machine's cores via Rayon. Zero network. Good for single-user gaming-rig deployment.

**Optional: `remote`.** Service POSTs batched eval requests to one or more `mossraven-node` URLs. Use when a homelab cluster is available, or for power users with idle gaming rigs.

**v1 ships the trait + the `local` impl. The `remote` impl + `mossraven-node` binary are scaffolded but minimal — fleshed out post-v1.** The trait boundary must exist from day one so this is a half-day add, not a refactor.

---

## 5. WPF shell — the v1 frontend

`MossRaven.exe` (WPF, .NET) is an **MCP client** that drives `mossraven-service.exe` over stdio JSON-RPC. The shell launches the service as a subprocess on startup and manages its lifecycle.

```
MossRaven.exe (WPF — UI shell)
    │ stdio JSON-RPC (MCP control surface)
    ▼
mossraven-service.exe (Rust — orchestration core)
    ├── pob-headless (in-process Lua)
    ├── MAP-Elites archive (disk-persisted)
    ├── Tier 2 surrogate client
    ├── Tier 1 driver (Mode A: API call out)
    └── MCP server (control surface)
```

**Concurrent connections:** the MCP server accepts multiple clients. Both `MossRaven.exe` AND Claude Code can connect to the same `mossraven-service.exe` at once — watch the heatmap fill in WPF while Claude Code drives Tier 1 from your subscription. First-class workflow.

**Shell pattern:** Match MossPost. `WindowStyle=SingleBorderWindow`, `WindowChrome.CaptionHeight=32`. Single-file publish (`IncludeNativeLibrariesForSelfExtract` + compression + embedded pdb). `dist/Windows/` contains:

- `MossRaven.exe` (single-file WPF shell, ~80–150 MB)
- `mossraven-service.exe` (Rust service, ~20 MB)
- `vendor/PathOfBuilding-PoE2/` (downloaded from upstream on first run, **never bundled** — see §9 licensing)

Two files in install dir, one Start Menu shortcut.

**UI panels:**
- Concept input (free-text "what kind of build are we exploring")
- Live MAP-Elites heatmap (cells colored by power, empties highlighted)
- Drill-into-cell view (PoB code, key stats, mutation lineage)
- Run controls (start/pause/resume; switch surrogate provider; switch Tier 3 to remote)
- Config: API keys, Cerebras endpoint, remote node-pool URLs

---

## 6. mossraven-node — the power-user farm server

Separate binary in the same Rust workspace. Deployed on farm machines (Linux Proxmox VMs, idle gaming PCs, anything with a CPU).

**Protocol:** HTTP/JSON. Stateless. Self-contained requests.

```
POST /score
  Authorization: Bearer <shared-secret>
  Content-Type: application/json
  Body: { "batch_id": "...", "variants": [ { pob_xml | spec }, ... ] }

→ 200 OK
  { "batch_id": "...", "results": [ { variant_id, stats | error }, ... ] }

GET /health
→ 200 OK { "version": "...", "pob2_version": "...", "cores": N, "in_flight": M }
```

**Concurrency:** node fans within itself across its own cores (Rayon pool), then returns the whole batch. Service-side `remote` backend round-robins across registered nodes.

**Security:** shared-secret bearer token in `Authorization` header. Fine for LAN / Tailscale-meshed homelab. Internet-exposed nodes need real isolation (out of v1 scope; documented but not built).

**Distribution:** single static binary per platform (`mossraven-node-linux-x64`, `mossraven-node-windows-x64`). Operator clones `PathOfBuilding-PoE2` into a sibling `vendor/` dir and runs the binary with a config file pointing at it. No Lua install required (pob-headless embeds it via mlua).

**v1 status:** scaffolded with a working `/health` endpoint and a stub `/score` that echoes back zeros. Real scoring lands in a follow-up once the in-process Tier 3 is validated against desktop PoB2.

---

## 7. Components & sourcing

### 7.1 Net-new (this repo)

| Crate / project | Purpose |
|---|---|
| `pob` | Salvaged/wrapped PoB2 Lua engine. Decision pending deep-dive (§11). |
| `archive` | MAP-Elites grid + behavioral-descriptor axes + disk persistence + resume. |
| `surrogate` | `SurrogateProvider` trait + OpenAI-compat HTTP client. |
| `dreamer` | `TierOneDriver` trait + Mode A (API) impl + Mode B (MCP server) impl. |
| `core` | Orchestration loop. Ties tiers together. Cribbed from OpenEvolve. |
| `mcp-server` | Control-surface MCP server. Both stdio + HTTP transports. |
| `node-protocol` | Shared request/response types between service and node. |
| `mossraven-service` (bin) | The main service. Embeds core + mcp-server. |
| `mossraven-node` (bin) | The power-user farm server. |
| `ui/MossRaven` | WPF shell (.NET solution). |

### 7.2 Vendored / external

| Source | License | How it's used |
|---|---|---|
| [PathOfBuildingCommunity/PathOfBuilding-PoE2](https://github.com/PathOfBuildingCommunity/PathOfBuilding-PoE2) | MIT (own license; bundled GGG data is **not** redistributable) | git-cloned to `vendor/PathOfBuilding-PoE2`. Loaded at runtime by `pob` crate. **Never committed; never bundled in distributed binaries.** |
| [SFerenczy/poe2-agent](https://github.com/SFerenczy/poe2-agent) | MIT | Source of inspiration / possible fork-and-trim for `pob` crate. Deep-dive pending (§11). |

### 7.3 External MCP tools (run alongside, attached to Tier 1)

| Source | License | Purpose |
|---|---|---|
| [HivemindOverlord/poe2-mcp](https://github.com/HivemindOverlord/poe2-mcp) | Confirm before redistribute | Datamined mechanics + ladder/poe.ninja data. Grounds hypotheses in real game rules. |
| [mcpmarket/poe2](https://mcpmarket.com/) | Confirm before redistribute | Live economy: currency rates, item prices, wiki, local log parsing. Costs found builds against current market. |

**[PoAI](https://pathofai.app/) — dropped.** Hosted web app, no self-hostable interface. Browser reference only.

---

## 8. Build order

1. **`pob` crate end-to-end.** Net-new, highest risk. `cargo build --release` clean against vendored PoB2; expose `score_build(pob_code) -> stats`. **Validate numbers match desktop PoB2 on a known build before anything else.** This is the gate everything else stands on.
2. **Tier-3 parallelism.** Batch-score N variants across host cores; benchmark evals/sec/core/node.
3. **MAP-Elites archive.** Define archetype axes + cell-placement logic + disk persistence + resume. Core data structure; get it right early. **Axes are tunable**, not a one-shot guess — treat as empirical.
4. **Tier-2 surrogate.** `SurrogateProvider` trait + Cerebras impl first (free tier prototype). Then confirm a local Ollama swap works by config alone.
5. **Tier-1 + dual drive.** `TierOneDriver` trait with both Mode A (API) and Mode B (MCP server for Claude Code / Cowork). Wire poe2-mcp / poe2 in here for grounding.
6. **`mossraven-node` skeleton.** Health endpoint + stub score endpoint + shared-secret auth + Rayon pool. Real `/score` impl deferred to post-v1.
7. **WPF shell.** Single-file publish; MCP client over stdio; subprocess lifecycle for `mossraven-service.exe`; concept input + heatmap + drill view.

Steps 1, 3, 6 are independently testable in isolation. 2 depends on 1. 4 + 5 depend on 3. 7 depends on 5 (uses the MCP server).

---

## 9. Constraints / guardrails

**Scoring quality:**
- PoB models damage / defense well; it models "feel" poorly (clunk, animation lock, on-death effects). Output is **theoretical viability** — exactly the "would work if tweaked" target. Finalists still need playtesting. **Surface this in the UI; never claim a build is fun.**
- Hypotheses must be grounded in real mechanics (poe2-mcp datamined + wiki), never LLM-invented synergies. Validate a proposed interaction against game rules before it enters search.

**Versioning:**
- **Version-stamp every archive entry** with PoB2 version + game-data version. Patches change calc math; without stamps the archive silently rots after each league/patch — and poe2-mcp auto-pulls patched data, so this WILL bite.

**Keys & billing:**
- **No OpenAI keys anywhere, ever.**
- Cerebras + Anthropic keys via env vars or config file, never hardcoded.
- Mode A: `ANTHROPIC_API_KEY` lives **only** in the API driver process env.
- Mode B: `ANTHROPIC_API_KEY` **must not** be set in the Claude Code / Cowork shell env — Claude Code silently switches to API billing if it's present. The service itself only ever needs a surrogate-provider key (e.g., `CEREBRAS_API_KEY`).

**Licensing:**
- All MIT/permissive code in this repo. Local / personal use fine.
- **Do NOT redistribute PoB2/GGG game data, asset bundles, or extracted skill/passive data** — GGG fan-content policy is non-commercial and prohibits asset redistribution.
- `vendor/` directory and any data bundles **must** be in `.gitignore` and **must not** ship inside distributed binaries. `vendor/PathOfBuilding-PoE2` is cloned from upstream on first run.

**Modularity:**
- Each tier + each driver mode + each backend provider independently runnable / mockable behind its trait. Required for sanity in testing.

**Platform:**
- **Primary:** Windows 10/11 (gaming-rig target audience).
- **Secondary:** Linux (homelab, mossraven-node).
- Rust workspace cross-compiles cleanly; WPF shell is Windows-only.

---

## 10. First task (Claude Code, this turn)

1. **Verify-on-entry:** deep-dive [SFerenczy/poe2-agent](https://github.com/SFerenczy/poe2-agent)'s `pob` module. Report findings as `docs/pob-deepdive.md`. Decide fork-and-trim vs cleanroom mlua wrapper and explain why.
2. **Scaffold the Rust workspace** per §7.1 with all crates, both binaries (`mossraven-service`, `mossraven-node`), trait definitions for `TierOneDriver` / `SurrogateProvider` / `Tier3Backend`, and stubs that `cargo build --workspace --release` cleanly.
3. **Scaffold the WPF shell** per §5 (MossPost pattern, single-file publish settings) with a minimal MCP-over-stdio client stub that launches `mossraven-service.exe` and reads its `read_archive()` response.
4. **Vendor PoB2:** `git clone` `PathOfBuildingCommunity/PathOfBuilding-PoE2` into `vendor/`. Add `vendor/` to `.gitignore`.
5. **Write `README.md`** documenting how to build, run, and benchmark each tier and each drive mode in isolation.

**Do not** implement real scoring, real surrogate calls, or real Tier-1 hypothesis generation in this scaffold. Those land in follow-up steps once §10.1's deep-dive locks the `pob` strategy.

---

## 11. The one open question (decision pending §10.1 deep-dive)

`pob-headless` in [poe2-agent](https://github.com/SFerenczy/poe2-agent) is **not currently a separate crate** — it's a `pob` module inside a single MIT crate. Logical dependency flow is clean (`pob` → `pob_parser` → `agent`), so extraction is fork-and-trim, not a rethink.

**Decision after deep-dive:**

- **Option A: fork-and-trim.** Copy `poe2-agent`'s `pob` module + minimal deps into our `pob` crate. Inherit their (presumably mlua-based) Lua wrapper. Fastest to first parity check. Inherits their style/version-pin.
- **Option B: cleanroom mlua wrapper.** Write our own `pob` crate from scratch against vendored PoB2. More work; we own every line; zero coupling to poe2-agent's choices.

Deep-dive answers: Lua FFI choice (mlua vs IPC vs rlua), thread-safety/async model, vendored PoB2 load surface, build-state I/O shape, error model, transitive deps, abstraction-reusability. **The deep-dive output decides A vs B before scaffolding step 7.1's `pob` crate beyond an empty stub.**
