# Are we going about this wrong? — PoB-specific NN assessment

*Written 2026-06-11, in answer to: "Training a PoB-specific NN may be the way.
Something with access to current PoB's, market, and build sites can (A) gain
memory/knowledge from build sites, (B) be rewarded/punished to optimize tree
(think lightning — if lightning needed the best path it would never strike, we
want a good-enough path), (C) be scalable and implementable to PoE1 and 2."*

## Short answer

**No — the architecture is right; the bottleneck is real but it's narrower
than "replace the cascade with an NN."** The current loop (LLM proposes →
PoB ground-truths → MAP-Elites keeps) is already the lightning model: we
never ask for the optimal build, we keep every good-enough path per niche.
What an NN buys us is **throughput between proposals and ground truth** — and
there is one specific NN that's clearly worth building, one grounding feed
that's worth wiring, and one RL idea that isn't worth it yet.

The strongest evidence the cascade works: this week it caught its own
founder's lie (the 100k DPS elite that wasn't buildable). A pure NN pipeline
would have shipped that build. PoB-as-judge is non-negotiable; everything
else is replaceable plumbing around it.

## What's actually limiting us today

| Limit | Measured reality | NN-relevant? |
|---|---|---|
| Evaluation throughput | PoB ≈ 50–200 ms/build/core; ~10 variants/generation | **YES — the case for a surrogate NN** |
| Proposal quality | LLMs fixate (Frost Bomb incident; 0/30 allocate_notable); fixed by mechanical forcing | partially — a learned proposer could help, LLM + forcing already works |
| Seed quality | One druid fixture at a local optimum caps the whole session | **YES — the case for build-site grounding** |
| Legality/honesty | Solved mechanically (budget guard, rescore) — zero NN value | no |
| Guide quality | Solved by T6 + critics; LLM is the right tool for prose | no |

## (A) "Gain memory/knowledge from build sites" — YES, wire it, no training needed

poe.ninja exposes character snapshots (PoE1: full API; PoE2: economy + builds
endpoints growing each league). That data answers exactly the questions our
search wastes cycles rediscovering:

- **Seeds**: real ladder characters = thousands of known-viable starting
  points across all classes, instead of 34 hand-collected fixtures. Import →
  rescore through OUR PoB → archive as seed library. This directly attacks
  the "different seeds" path to 500k.
- **Costs**: live unique/currency prices replace the §1.1.2 heuristic
  (already planned as the env-gated follow-up).
- **Meta manifold**: an *embedding* of ladder builds (even just TF-IDF over
  gem+keystone+unique sets — no deep model required) gives a novelty
  distance: "this archive cell is far from anything on the ladder" is the
  discovery signal MAP-Elites wants. Memory without training a network.

Effort: days, not weeks. Highest value-per-effort item in this document.
This is "memory/knowledge from build sites" delivered as data engineering,
not model training — same capability, 1% of the cost.

## (B) "Rewarded/punished to optimize tree" — the right version is a surrogate, not RL

The lightning metaphor cuts the other way: lightning doesn't *learn* the
path, it follows the local field greedily — and that's what BFS-to-notable +
MAP-Elites already do. Good-enough paths are cheap; we have them.

What's expensive is knowing **which** of a million possible mutations are
worth PoB's time. That's a regression problem, not RL:

**Tier 2.5 — a learned value model (the NN worth building).**
- Input: a build feature vector (allocated notables/keystones one-hot ≈ 1k
  dims, gem ids + levels, item base/unique ids, class/ascendancy).
- Output: predicted (DPS, EHP, ES) — or just predicted *rank* within a batch.
- Training data: **we manufacture it ourselves.** Every Tier-3 call this
  engine ever makes is a labeled example. A weekend of unattended PoB churn
  (random + guided perturbations of ladder seeds, ~50–200ms each, 8 cores) =
  1–5M labeled builds. No scraping, no licensing questions, perfectly
  on-distribution for OUR mutation operators.
- Architecture: gradient-boosted trees first (XGBoost-class, CPU, trains in
  minutes, runs in microseconds) — embarrassing if a transformer is proposed
  before this baseline exists. If GBT plateaus, a small MLP/set-transformer
  over (node-set, gem-set, item-set) embeddings.
- Deployment: replaces/augments the LLM cheap-score in Tier 2. Propose
  10,000 mutations mechanically (we already have the operator library),
  value-model ranks them, PoB verifies the top 50. **That's a 100–1000×
  effective search-width increase with zero honesty risk** — PoB still
  gatekeeps everything that enters the archive.
- This is SAIL (surrogate-assisted illumination) done properly, which the
  SPEC already names as prior art. The reward/punish intuition is exactly
  the training loss; it just doesn't need an agent or a policy network.

**Why not RL on tree pathing**: pathing is already optimal-enough via BFS
(cost-bounded, legality-gated). An RL policy would learn to approximate what
we compute exactly, with weeks of reward-shaping pain and a result that
breaks every patch when GGG moves nodes. RL becomes interesting only at the
"whole-build trajectory" level (leveling curves, §1.1 checkpoints), and the
data for that doesn't exist outside gameplay telemetry we don't have.

## (C) "Scalable to PoE1 and 2" — yes, by construction

Everything above is engine-agnostic: PoB1 has the same Lua architecture
(HeadlessWrapper exists; the community PoB1 is two decades more stable than
PoB2), poe.ninja's PoE1 API is richer than PoE2's, and the value model
retrains per-game from self-generated data in a day. The Rust side needs a
`pob1` feature flag in crates/pob and a second vendor checkout. The
MAP-Elites axes, cost layer, viability gates, and T5/6 pipeline carry over
unchanged. PoE1 support is a porting exercise (1–2 weeks), not a research
project — and it doubles the audience.

## Recommended phasing

| Phase | What | Effort | Unblocks |
|---|---|---|---|
| 1 | poe.ninja grounding: seed import + live costs + meta-distance | ~3–5 days | better seeds (the real 500k path), honest §1.1.2 prices, novelty signal |
| 2 | Data factory: headless PoB churn logging (features, stats) per eval — start logging NOW, it's free | ~1 day | training corpus accumulates while we do everything else |
| 3 | Tier 2.5 value model: GBT on the corpus, wire as pre-rank before Tier 3 | ~1 week once corpus ≥ 500k rows | 100–1000× search width |
| 4 | PoE1 port behind a feature flag | 1–2 weeks | second game, bigger data, bigger audience |
| 5 | (only if 3 plateaus) neural set-encoder surrogate; (only with telemetry) trajectory RL | months | not yet justified |

## What we keep, verbatim

- **PoB as the only source of truth.** Nothing enters the archive un-simmed.
- **MAP-Elites as the product.** Diversity-first beats best-first for
  discovery; this is the lightning philosophy formalized.
- **Mechanical guards over learned guards** for legality (budget, scored
  group, label hygiene) — a guard that can't hallucinate beats one that can.
- **LLMs where language is the job**: hypotheses, curation, guides, critics.
  They were never the right tool for throughput and we've stopped using them
  for it.
