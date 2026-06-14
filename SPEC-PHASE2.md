# MossRaven — Phase 2 Spec

**Status:** living document. Supersedes nothing in `SPEC.md` (Phase 1 §1.1 "definition
of done" still holds) — this *extends* it. Phase 1 built a working single-machine
discovery engine; Phase 2 turns it into a **self-improving, GPU-accelerated, swarm-
coordinated** one. Authored 2026-06-13 from the architecture discussion with Taylor.

---

## 0. The one-paragraph thesis

PoB is a **slow ground-truth oracle** (~1 s/build, CPU, single-threaded Lua). The
corpus is **(build → PoB-measured stats)** training data. The Tier-3 value model is a
**fast learned clone of PoB** (a GNN, ~1 ms, GPU). Phase 2 is the flywheel that turns
PoB's slow truth into a fast neural surrogate, then **flips the loop**: the GNN explores
build-space at lightning speed on every user's GPU ("the ant brain"), and PoB drops to a
sparse fact-checker. A central coordinator aggregates every user's labels into one
corpus, retrains the shared GNN, and redistributes it — **group discovery as a swarm.**

```
BOOTSTRAP (today):  proposer → PoB scores (CPU, slow, TRUTH) → corpus → train GNN
GNN-GUIDED (added): GNN explores millions on GPU (fast) → PoB verifies top-K (sparse)
                    → new labels → retrain GNN → smarter exploration → repeat
SWARM (scale):      every client labels + explores; coordinator aggregates + retrains
                    + redistributes the model + assigns unexplored regions + polices trust
```

**These run CONCURRENTLY, always — not bootstrap-THEN-flip (Taylor, 2026-06-13).**
A GNN that only ever sees what the early CPU churn happened to explore would *ingrain
that bias* — and once it guides exploration, it reinforces the regions it's already
confident in, compounding the skew. So the slow PoB-truth generator and the fast GNN
explorer run **at the same time, for the life of the project**, with an **Overseer
(§5)** holding the wheel: it forces the truth-generator into the regions the GNN is
*weak or untested* on — every class, every skill, every gear archetype, at the **current
patch** — so the corpus stays representative and the GNN keeps generalizing instead of
narrowing. Truth never stops; the GNN never stops; the overseer keeps them honest.

---

## 0.5 The Atlas, the Librarian, and the Cartographer (Taylor, 2026-06-13)

A sharpening of what we're actually building, and what the GNN is *for*:

- **The Atlas = the product.** A map of build-space, **PoB-scored**, multi-objective.
  Not one number per build — a vector: **bossing** power, **mapping** power, defense,
  and **cost** (where *cost ≈ speed*: how fast you can acquire it). Each entry is "this
  build, at this cost, does this much bossing / this much mapping." This is MAP-Elites
  taken to its limit: best-per-niche where niche = (class × skill × gear × content-type ×
  cost-band).
- **The query is a Librarian, not an oracle.** Given a player's description + budget,
  finding the answer is a **search/rank over the Atlas** ("best bossing build under 10
  div for a Druid"), not a neural prediction. We have a library; we need a librarian.
- **So why the GNN ("Lightning Ant")? It's the Cartographer, not the oracle.** Two jobs:
  1. **Afford the Atlas.** The discretized build-space is *still* far too big to PoB-score
     exhaustively (millions of archetypes × ~1 s each = months of CPU; and the *full*
     space — rare-mod gear, every tree subset — is effectively infinite, so the Atlas is a
     quantized map of *meaningful archetypes*, never literally "every atom"). The GNN
     **predicts which regions are worth PoB-scoring**, fills gaps with predictions, and we
     **verify samples** with PoB. It's how we build a near-complete Atlas without infinite
     compute.
  2. **Keep it fresh.** On a patch, the GNN predicts which builds changed most so we
     re-score *deltas*, not the whole Atlas.
- **The scoring problem is the hard part (Taylor).** "Accurately score each variation"
  means multi-category, cost-relative scores: bossing-with-an-eye-to-cost, mapping-with-
  an-eye-to-cost. The value function the librarian ranks by is **score-per-cost per
  content type**, because a cheap build at 80% power beats an expensive one for most
  players. (Defense/EHP is a gate, not a maximand: enough to survive the content, then
  maximize offense-per-cost.)
- **Churn runs over a SCOPE, not a timeframe (Taylor).** A node isn't told "run 400
  generations"; the **Overseer (§5) hands it a SCOPE** — a region of the Atlas to fill
  (e.g. "Warrior × slam skills × budget tier"). The node fills that scope, reports
  coverage, gets the next scope. Generations-per-cycle was a stopgap; scopes are the real
  unit of work, and they're what the swarm coordinator distributes.

So the end-state isn't "GNN replaces PoB." It's: **PoB grounds the Atlas, the Cartographer
GNN lets us map it affordably and keep it current, and the Librarian answers players from
the map.** The GNN earns its place by making a near-complete, always-fresh Atlas
*possible* — not by being the thing players query.

## 0.6 How big is the build space? — adversarially verified (2026-06-13)

Five independent methods, each torn into by an adversary agent, then reconciled (11-agent
run, ~750K tokens, all reading the real `TreeData/0_5/tree.json`). The numbers:

- **Definition of "a build"** (the one all five reviews converged on): its **set of allocated
  notables + keystones**, counted *iff* connected-and-affordable — some connected subgraph
  from a class start grabs exactly those within the ~110-point budget (100–123). The **57% of
  nodes that are degree-2 travel are fungible** — they gate connectivity, they are not identity.
- **Distinct meaningful builds: ~10³⁰** (defensible band **10²⁶–10³⁸**). This is the
  load-bearing number; it was reproduced by a second, different method (connected-k-subgraph
  count on the notable-adjacency graph).
- **Raw connected node-allocations: ~10⁶⁹–10⁷⁶** — quoted only as context; those differ
  mostly by interchangeable travel paths, so they are *not* "different builds."
- Free-choice ceiling C(984,~40) ≈ 10⁴⁵⁺ — rejected (ignores the binding connectivity
  constraint).
- **Real tree facts (0.5):** 4,483 skill nodes, 984 notables, 33 keystones; **6 start nodes,
  each shared by 2 classes** (44683/47175/50459/50986/54447/61525); avg degree 2.41, max 9; a
  greedy build grabs ~38 / 42 / 46 notables at 100 / 110 / 123 points.
- **Killed methods (kept as negative results):** empirical capture-recapture sampling (every
  unseen-species estimator only recovers the *sampler's own* ~10⁵ support — 20+ OOM below
  truth); homogeneous analytic growth-constant bounds (broken derivation; band too wide to
  constrain).

**The verdict — and it is unanimous:** exhaustive enumeration is impossible by **20+ orders of
magnitude**, and no compute pool closes the gap — a 512-core full-calc farm scores ~10⁹·⁴
builds/week vs. a 10³⁰ space. **So the Atlas does NOT catalog builds.** It:

1. **Enumerates only the low-cardinality DESCRIPTOR grid** — ascendancy (22) × main-skill
   (~120) × damage-type (7) × defence-archetype (6) × life-band (5) × dps-band (6) ≈
   **3.3M behavior cells** (the part that *is* enumerable).
2. **Guided-searches** (MAP-Elites / quality-diversity) the 10³⁰ build space to find the best
   occupant of each cell.

- **Tractable target = a de-duplicated ELITE ARCHIVE of ~10⁵–10⁶ occupied cells** (~5×10⁵ at
  the fine descriptor with ~15% feasible occupancy; ~1.1×10⁵ for a coarse browsable grid).
  *Not* a catalog of distinct trees.
- **Fill cost is modest:** ~100–1000 full-calc evals per filled cell ⇒ ~5×10⁷–5×10⁸ evals ⇒
  a fraction of one node-farm-week, gated behind the surrogate funnel (surrogate ranks
  ~10⁹–10¹¹ candidates; full PoB-equivalent calc scores only the ~10⁷–10⁸ that look elite).
- **The binding real-world constraint is full-calc throughput** (the still-unbenchmarked
  ~50–150 ms/build PoB cost), **NOT the size of the space.** This is exactly why "how many
  builds are there" is the wrong question and "how big is the archive" is the right one.

**This vindicates MossRaven's existing MAP-Elites architecture.** The first-principles math
returns the engine we already have: an enumerable descriptor grid, a searched 10³⁰ build space,
a surrogate funnel, an elite archive as the product. The open refinements it implies:
(a) make the archive's cell axes the **measured descriptor grid** above (asc × skill × damage ×
defence × life-band × dps-band) rather than ad-hoc coords; (b) **benchmark PoB's real
per-build calc time** — it is now the one number that bounds the whole project.

## 1. Carry-forward from Phase 1 (do not lose)

The 7-tier pipeline stays the spine:

| Tier | Role | Phase-2 change |
|------|------|----------------|
| T1 | Dreamer (Claude) — concepts, seeds, guides | unchanged; fed cold regions by novelty supervisor (§5) |
| T2 | Surrogate proposer (cloud LLM / **Ollama** / **mechanical**) | proposer split formalized (§2); GNN-policy added later |
| T3 | **Value model** (was LightGBM stub) → **GNN** | the headline of Phase 2 (§3) |
| T4 | Judge (PoB headless, parallel pool) | the ground-truth labeler; distributed via node farm + swarm |
| T5 | Prune | GNN pre-rank replaces cheap-score |
| T6 | Selector (pool 15–20) | unchanged |
| T7 | Curator-author (5 builds + guides) | unchanged — still the product |

Also carried forward, all still required:
- **MAP-Elites archive** = the product; reload/merge/atomic-save daemon discipline.
- **Embedded PoB GUI** (SetParent host, live-link, tree-view stamp, off-screen birth).
- **Gem/socket legality** enforcement (cap 5 supports, dedupe, rescore purge).
- **poe.ninja grounding** (prices, ladder meta, seed import).
- **Cost model + viability bands**; HTML + PoB-code + markdown outputs.
- **Definition of done (Phase 1 §1.1 v2):** 5 curated builds × 5 leveling checkpoints +
  bossing/mapping/cost guides, PoB XML + import code + md + HTML.
- **Sandbox/ops discipline** (CLAUDE.md): explorer-parented real-world ops, pixel-probe
  (screen-BitBlt only for the GL pane — PrintWindow lies), one git writer.

---

## 2. The corpus loop, precisely

Two separable steps. **Only scoring is expensive.**

- **Proposer** — emits candidate mutations (diffs: add support, allocate notable, swap
  unique, set gem level, weapon-set toggle). Pluggable:
  - **mechanical** (`MOSSRAVEN_MECHANICAL=1`): deterministic menu, zero network. The
    data-factory default — max scoring throughput, no rate limits.
  - **local Ollama** (`OLLAMA_MODEL=qwen2.5:14b`): concept-aware proposals on the user's
    **GPU (CUDA)**, no rate limits. Smarter corpus; uses idle hardware.
  - **cloud LLM** (Cerebras/Groq/Gemini failover): interactive search only — free tiers
    429 under churn load (the "no CPU" bug: 5-min backoffs idle the loop).
  - **GNN-policy** (Phase 2 late): the value model itself proposes where to mutate.
- **Scorer** — **always PoB** (the ground truth), parallel pool of Lua VMs. CPU-bound.
  Corpus grows by one labeled row per eval.

**Indicator the loop is healthy:** `mossraven-service.exe` pegging cores (not idle RAM),
`evals-<ver>.jsonl` row count climbing, churn log showing `mechanical=1` / Ollama and no
`429` backoffs. Measured 2026-06-13: mechanical = 52 s CPU / 45 s wall across the pool
vs. ~2 s CPU / 6 min while cloud-rate-limited.

**Throughput levers (in order of impact):**
1. Multi-threaded pool — `MOSSRAVEN_POOL_SIZE = cores−1` (one PoB VM/worker, ~150 MB).
   **N−1, not N:** leave a core for OS + UI + PoB render so the box stays usable. A
   dedicated/headless node may use N.
2. Mechanical (or Ollama) proposer — removes the network bottleneck.
3. **Local node farm** — `mossraven-node` HTTP judges on every LAN machine; engine's
   `RemoteBackend` fans scoring across them (deploy templates exist).
4. **Swarm** — every user's machine (§4).

---

## 3. Tier 3 — the GNN value model (the brain)

**Decision (Taylor): straight to a GNN. No simpler net** — a feature-MLP/LightGBM can't
see the tree's graph structure or compositional gem synergies, so it "gives no value."

- **Input = a build as structured data:**
  - allocated passive-tree subgraph (nodes + adjacency + node stats) → **GNN**;
  - gems/supports per socket group → **set-transformer**;
  - items/uniques/bases, level, ascendancy, masteries → embeddings.
- **Fusion → MLP head → PoB outputs:** total DPS, EHP/ES/life, resistances,
  points used/budget, cost band. Dual-head (offense + defense) as in the Phase-1 plan.
- **Size:** ~10–50 M params. **Fits ≤8 GB VRAM** with headroom (Taylor's target — the
  floor for "most gaming GPUs", incl. the 3070/5070s here) so it runs on the whole
  install base for distributed inference. Trains on one consumer GPU.
- **It is a learned PoB-calc surrogate:** ~1 ms vs ~1 s = **~1000×**.
- **Use:** pre-rank ~10 k mechanical mutations → send only the top ~50 to PoB. ~100×
  wider search per unit of PoB compute.
- **Training:** PyTorch; corpus → feature graphs; KPIs **Spearman ≥ 0.8** and
  **top-5%-in-top-20% recall ≥ 0.9** on held-out builds before it gates anything live.
  Versioned per PoB data version (retrain on patch).
- **Active learning (the flip):** once accurate, the GNN proposes *where to spend the
  expensive PoB labels* — explore millions on GPU, label the most informative ~50 with
  PoB, retrain. This is what makes discovery accelerate instead of plateau.
- **Data requirement:** a GNN wants ~100 k–1 M+ diverse rows (have ~800). → **the swarm
  (§4) is the data engine; the novelty supervisor (§5) keeps coverage broad** so it
  generalizes instead of memorizing.
- Builds on `docs/nn-direction.md` (Phase-1 direction assessment).

---

## 3.5 The GPU calc engine — "fast exact PoB" (Taylor's moonshot; the strongest moat)

A real, buildable thing — and if built, something **no competitor has: an exact, GPU-fast,
auto-data-updating PoB engine.** Distinct from the GNN (§3): the GNN *approximates* PoB; this
*is* PoB's math, on the GPU.

**The honest split** (because the fully-automatic version is a fantasy and we won't bet on it):

| Half | Auto / "in moments"? | Reality |
|---|---|---|
| **Auto-transpile PoB's Lua → CUDA** | **NO** | Lua is dynamically typed, hash-table-heavy, branch-divergent — the opposite of GPU-friendly. A mechanical transpile won't compile or won't be fast. Drop this framing. |
| **Auto-extract the DATA** (mods, tree, items, gems — what changes each patch) | **YES** | structured, scriptable; refresh on every PoB release "in moments." |
| **Hand-port the CALC ENGINE** to CUDA kernels | **NO (effort, not auto)** | the real work, and it's worth it. |

**The buildable engine:**
- One warp per build; compile each build (on CPU, fast) into a **flat modifier array** (mod-type
  enum, value, condition bitflags, tag bitmasks), then a CUDA kernel does the aggregation +
  damage/defense pipeline as branchless arithmetic.
- **Staged, and eventually 100% complete:** common damage/defense path first (~90% of builds),
  then the special-mechanic long tail **incrementally**. *Everything PoB computes is portable* —
  it's volume of work, not a wall (correction to an earlier mis-framing). CPU PoB is a *temporary*
  fallback for not-yet-ported mechanics and a *permanent* parity verifier.
- **Exact, 0-loss — guaranteed by the existing parity-test harness** (#35/#45): every kernel
  diffed against PoB rule-for-rule. This is how "0 loss on the maths" is actually achieved — by
  verification, not by magic.
- **~100–1000× single-build.** Turns a 10⁷–10⁸-eval archive fill from **weeks/months → ~a day**,
  and per-patch re-scoring of the whole archive from **days → minutes**.

**VERDICT (adversarial #94 benchmark, 9-agent run): NO-GO at current scale.** The CPU-swarm +
NN-surrogate funnel clears the realistic ~10⁷-eval budget in **~38 box-days on one box, ~4 days on
the already-built 10-box node farm** — cheap, linear, shipped. Three reasons GO stays off:
1. **PoB is always the exact gatekeeper.** Archive *placement* is exact no matter how the
   surrogate ranks, so "GPU-exact vs NN-approximate" is a **false tradeoff** — the GPU buys no
   extra correctness, only speed on a budget the swarm already covers.
2. **Amdahl trap:** each score re-runs the **CPU-side XML parse + Build:Init**, not just the calc.
   A calc-only CUDA kernel is capped at **~1.4–1.7× end-to-end** unless the *parser* is co-ported
   too — a much bigger job.
3. **Per-league parity tax:** GGG ships tree/mod/base changes every league → recurring
   verification cost forever.
- **It flips to GO only in a narrow corner:** ≥10⁸ exact evals that must finish on a *single
  box* (no horizontal scaling) within ~30 days → needs **<26 ms/build (~12× today's pool)**, a
  rate CPU provably can't hit, *and* the parser co-ported. Below that, or anywhere boxes can be
  added, CPU-swarm wins. **Shelved as a named moonshot, not the path.**
- It remains the only **fast-exact** quadrant (GNN = fast-approximate, CPU swarm = slow-exact) —
  a real moat *if* the corner ever binds. It just doesn't today.

**What #94 proposed as cheap wins, and what survived verification** (a cautionary tale — both
headline "wins" were phantoms, caught by measuring before acting): LuaJIT-restore (claimed 2–5×)
→ **already on** (`JIT=ON LuaJIT 2.1`); more-VM-RAM (claimed RAM-starved) → **7.4 GB free under the
pool**, fine. Neither exists. The single-box ceiling (~2.3×) is the *hard* kind — allocator
contention and/or memory bandwidth — leaving exactly one unverified single-box lever (**mimalloc**)
and one real lever: **horizontal scaling** (the node farm + swarm). Per-box is ~3 builds/sec and
won't climb much; you beat the GPU port not by speeding one box but by **adding boxes** — which
the RemoteBackend node farm already supports.

---

## 4. The swarm — distributed discovery, compute, and a shared brain

**Goal (Taylor):** a central server coordinating all MossRaven clients across all users
as a swarm for group discovery, corpus compilation, and shared compute.

**Topology:** many **clients** ↔ one **coordinator** (new Moss backend service; ties to
the existing Supabase identity + Server-C telemetry).

- **Client** (each user's MossRaven):
  - labels builds locally (PoB pool) and explores locally (its GNN);
  - reports: filled archive cells, best-of-cell builds, and **PoB-labeled rows**;
  - pulls: the latest shared GNN + a **work assignment** (which unexplored region to
    focus) so two users don't grind the same cells.
- **Coordinator:**
  - **aggregates** all clients' labels into one global corpus; **dedups** globally;
  - **retrains** the shared GNN on the aggregate; **redistributes** it;
  - **assigns** unexplored regions (global novelty, §5);
  - maintains the **global MAP-Elites** (best build per niche across all users) — the
    group-discovery artifact.

**Enterprise-grade trust model (Taylor — required, design in from day one):**
- **Identity & auth:** signed client identity (Supabase), per-client API tokens, rate
  limits, **Sybil resistance** (proof-of-work or account-age/reputation gating to make
  fake-client farming costly).
- **Contribution integrity:** every submitted row is **server-side re-validated** — the
  coordinator re-runs PoB on a random sample of each client's claims; mismatches →
  reject + reputation hit. Builds must pass **legality** (§ gem caps) before acceptance.
- **Adversarial defense:** statistical outlier detection on labels (a client reporting
  impossibly high DPS for a region is quarantined), poisoned-build detection, ban list,
  and a **canary set** (known builds with known scores seeded to clients to detect
  tampering). Reputation-weighted aggregation so low-trust contributions count less.
- **Privacy:** corpus rows are build structures + numbers (no PII); telemetry minimal.

---

## 5. The Overseer — coverage, anti-bias, current-patch completeness

**This is the missing piece Taylor flagged: the thing that guarantees we explore ALL
builds, for ALL classes, with ALL gear, at the CURRENT update — so neither the corpus
nor the GNN narrows into a biased rut.** It is the novelty supervisor promoted to a
first-class, always-on controller that steers BOTH the truth-generator (PoB churn) and
the GNN explorer.

Mandates:
- **Full-space coverage as an explicit objective**, not a side effect:
  - **every class + ascendancy**, **every active skill**, **every gear archetype**
    (uniques, base types, weapon classes, defense layers), at the **live patch's** tree
    and data versions. Maintain a coverage grid over (class × skill-family × defense ×
    cost-band × …) and measure fill + density, not just "did we find something good."
- **Anti-bias steering:** direct the PoB churn into the cells where the **GNN is weak or
  untested** (high predicted-vs-actual error, or never-labeled) — *active learning at the
  exploration level*. The truth-generator's job is to keep correcting the GNN's blind
  spots, not to pile more data where it's already accurate.
- **Concurrency contract:** truth-generation and GNN-guided exploration run at the same
  time, indefinitely; the overseer balances their effort (how much PoB compute goes to
  GNN-proposed promising builds vs. to coverage of cold/uncertain regions).
- **Patch-freshness:** on a PoB/game update, flag stale regions (old tree/data version),
  prioritize re-coverage, and never let the GNN train on mixed-version truth without a
  version tag.
- **Diversity mechanics:** bias seed re-injection / mutation toward under-explored cells
  and novel skill/keystone/unique combos; hand genuinely cold regions to the **T1 dreamer
  (Claude)** for fresh concepts.
- **Global scope via the swarm:** the coordinator's work-assignments are the overseer at
  fleet scale — clients are steered away from globally-covered regions and toward the
  collective blind spots; no two users grind the same cells, and the *union* of all
  clients' coverage is what the overseer optimizes.
- **Visibility:** surface the coverage map in the UI (what % of class×skill×gear space is
  mapped at the current patch, where the GNN is still uncertain).

---

## 5.5 MossRaven-Explorer — the discoverer (product mode)

The engine has **two complementary faces**, same infra, different intent:

- **Census** (the Overseer, §5): *systematic* — fill the descriptor grid for coverage and
  completeness. This is the **library**: reliable, browsable, "best build for every niche."
- **Explorer:** *novelty-hunting* — a **full-time loop where the Tier-1 dreamer (Claude)
  hypothesizes weird, off-grid builds** ("lightning werewolf that stacks block", "minion build
  that scales off flask charges"), seeds them into guided search, and probes the **outlier corners
  of the 10³⁰ space for moonshots** — builds no guide-writer would conceive. Raw volume has
  diminishing returns out there, but the *hits* are exactly the novel/surprising builds that
  differentiate MossRaven from a stats site.

Product framing: **the library (Census) + the discoverer (Explorer)** are MossRaven's two faces —
the Census makes it *complete*, the Explorer makes it *surprising*. The Explorer is a **mode, not
new tech**: it reuses the dreamer + guided search + archive + swarm, just pointed at novelty and
run continuously (an "enterprise that hypothesizes builds all day"). It pairs naturally with the
swarm — idle client compute runs Explorer probes, the coordinator dedups discoveries globally.

---

## 6. GPU utilization — "can we CUDA the loop?"

Measured throughput (adversarial #94 benchmark, on the reference 12-vCPU box):
- **Single-build calc: ~770 ms** for the real mutated workload (fixtures floor ~390–420 ms p50;
  heavy crit/projectile builds ~1000–1300 ms; pathological 96-skill ~2000 ms). **Calc depth per
  main skill dominates, not tree size.**
- **Pooled (11 workers): ~3 builds/sec/box (330 ms/build), only ~2.3× over single-thread (~21%
  efficiency).** Fixable ceiling maybe ~5 builds/sec/box; 11× is unreachable.
- **TWO swarm "cheap win" claims were FALSIFIED by direct measurement (verify before acting):**
  - *"Binary runs interpreted Lua → 2–5× from LuaJIT"* — **FALSE.** Runtime probe reports
    `LuaJIT JIT=ON LuaJIT 2.1.ROLLING`. ~770 ms IS the LuaJIT number. The bit polyfill is an mlua
    *safe-mode* artifact (C modules can't load sandboxed), not a JIT-off signal. No win here.
  - *"RAM-starved QEMU guest (~0.6–1 GB free)"* — **FALSE.** Box has 15.9 GB total, **7.4 GB free
    under the full 11-VM pool** (~4 GB used). Capacity is fine. No win from more RAM.
- **So the ~2.3× ceiling is the HARD kind:** allocator contention (11 threads on one process
  malloc → **mimalloc** is the one testable single-box lever, unverified) and/or memory bandwidth.
  Per-box throughput is genuinely capped at single-digit builds/sec → **horizontal scaling
  (the node farm / swarm) is the real lever**, not single-box tuning.
- **GPU escape hatches:** the **GNN (§3, fast-approximate)** and the **GPU calc engine (§3.5,
  fast-exact)** — the latter is NO-GO at current scale (§3.5). The old "non-starter that breaks
  updates" line is retracted: it's buildable, just not economical yet.
- **Proposer:** **yes** — local Ollama runs on CUDA today (`OLLAMA_MODEL`).
- **Value model:** **yes** — GNN training + inference on CUDA (the main GPU workload).
- The user has multi-GPU (2×5070 + 3070): natural split — Ollama on one, GNN
  training/inference on another, PoB pool on CPU. The node farm + swarm let idle GPUs on
  other LAN machines join.

---

## 7. Embedded PoB — responsiveness & lifecycle

- **Done:** SetParent host, off-screen birth (no flash), live-link (~32 ms) with
  `viewMode="TREE"` stamp (both click and AI-explore paths), app-sized capture, console
  hide, gem-legal builds.
- **Click-twice / stale render (open):** `AttachThreadInput` helped the first click then
  re-detaches. **Fix:** re-attach on WPF focus/activation events (handle WM_ACTIVATE on
  the host, re-`AttachThreadInput` + `SetFocus` each time the pane regains focus), so the
  input stays bound across focus changes. PoB's frame-throttle is gated on focus, so
  keeping it "focused" while the pane is active also cures the live-update lag.
- **"Feels like 1995":** SimpleGraphic's render ceiling. We do **not** fork PoB's
  renderer. The real fix is §3 — the app reads builds from the GNN instantly; PoB becomes
  the final verify-view, not the thing you wait on.
- **PoB updates (#80):** track PoB **tagged releases**, swap the whole matched bundle
  (scripts + runtime together) — never let it self-update scripts past its runtime (the
  GetVirtualScreenSize crash). The "older tree version" banner is the trigger.

---

## 8. UI / UX

- **Inline description:** replace the finalist popout window with **click-to-expand under
  the build** (the row grows down to reveal the guide). Applies to both Builds list and
  History.
- **Rounded everything:** buttons + textboxes done; **ComboBoxes** still square → full
  retemplate (folded into the audit #86).
- Naming: collapsed = **History**, list view = **Builds** (done).
- Ops box: churn / rescore / train with auto-window scheduling (done) + a clear "churn is
  working" indicator (cores busy + rows/min).
- Settings: per-tier model toggles, keys, paths, **proposer mode** (mechanical / Ollama /
  cloud), pool size, node URLs.

---

## 9. Service lifecycle & robustness

- **Single-instance churn lock** (PID file) — done; stops pile-ups.
- **Force-close-proof kill:** assign the daemon + churn tree to a Windows **Job Object**
  with `KILL_ON_JOB_CLOSE` so the OS reaps every `mossraven-service.exe` when MossRaven
  dies for any reason (incl. force-close). Plus: service self-exits if its parent PID
  vanishes (belt).
- **Full audit (#86):** cold-start speed (defer/parallelize init), error-path robustness,
  dead code, naming, PS-script hygiene; `/code-review` + simplify pass.

---

## 10. Packaging & distribution (#77)

- Per-user Inno installer (icon done), **code-signing cert** (makes MossRaven a grantable
  app → unlocks the computer-use MCP's pixel-perfect screenshots as a probe backup),
  winget manifest, PoB2 bootstrap (download matched release on first run).

---

## 11. Dev backlog still open (#74)

- **Seed import:** poe.ninja per-character endpoint + character→PoB-XML synthesizer
  (technique proven; meta-distance prompt block done).

---

## 12. Phase 2 definition of done

1. **Corpus at scale:** ≥100 k diverse, legality-clean rows (mechanical + Ollama +
   multithread + node farm + swarm).
2. **GNN trained & gating:** Spearman ≥ 0.8, top-5%-in-top-20% recall ≥ 0.9; wired as the
   T5 pre-ranker; measured search speedup (cells filled per PoB-second) ≥ 10× vs. no
   surrogate.
3. **Active-learning loop closed:** GNN proposes labels → PoB verifies → GNN improves
   across retrains (monotone KPI gain).
4. **Swarm live:** ≥N clients contributing, coordinator aggregating + redistributing the
   GNN, **trust model enforced** (server-side re-validation + reputation + canaries).
5. **Embed:** first-press clicks, live updates, zero flashes (user-confirmed).
6. **Product unchanged in spirit:** still outputs 5 curated builds × 5 checkpoints +
   guides — just discovered far faster and from a far larger, swarm-mapped space.

---

## 13. Sequencing (milestones)

- **M1 — Data factory at volume.** Mechanical + Ollama churn, multithread (done), node
  farm, novelty supervisor v0. Target: 100 k+ rows. *(Unlocks everything.)*
- **M2 — GNN v0.** Architecture + training pipeline + inference wired as T5 pre-ranker.
  Measure speedup. *(The brain comes online.)*
- **M3 — Swarm.** Coordinator service, client check-in protocol, global corpus + archive,
  **trust/adversarial layer**. *(Group discovery.)*
- **M4 — The flip.** Active-learning loop; GNN-policy proposer. *(Lightning exploration.)*
- **M5 — Polish & ship.** Embed click/render, inline description, rounded combos, Job-
  Object lifecycle, installer + signing, audit. *(Phase 2 DoD.)*

Embed/UI/lifecycle polish (M5 items) land opportunistically alongside M1–M4, not strictly
last — they're independent and user-facing.
