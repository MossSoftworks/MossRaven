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

## 6. GPU utilization — "can we CUDA the loop?"

Honest map of what is and isn't GPU-able:
- **PoB scoring:** Lua/CPU. *Not* GPU-able without rewriting PoB's calc engine (a
  non-starter; breaks updates). The GNN is the escape hatch — it *is* the GPU path that
  eventually replaces most PoB calls.
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
