"""Tier-3 value model trainer (SPEC §3.1 Tier 3, docs/nn-direction.md phase 3).

Reads the §3.7 corpus (data/corpus/evals-<pob2-version>.jsonl), builds a
sparse feature matrix, trains gradient-boosted trees to predict log1p(DPS)
and EHP, and reports holdout metrics — most importantly RANK correlation,
because the engine uses Tier 3 as a pre-ranker (pick the top slice for the
judge), not as an oracle.

Usage:
  py -3 scripts/train-value-model.py                     # newest corpus file
  py -3 scripts/train-value-model.py --corpus PATH.jsonl --out models/

Requires: pip install lightgbm scikit-learn numpy scipy
Exports:  <out>/value-model-<version>.txt   (LightGBM text model, dps head)
          <out>/value-model-<version>.ehp.txt
          <out>/feature-map-<version>.json  (column -> feature name)
          <out>/report-<version>.json       (metrics + row counts)

NEVER train across pob2 versions — patches change calc math; the per-version
file naming enforces the boundary.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sys


def newest_corpus(default_dir: str) -> str:
    files = [
        os.path.join(default_dir, f)
        for f in os.listdir(default_dir)
        if f.startswith("evals-") and f.endswith(".jsonl")
    ]
    if not files:
        sys.exit(f"no corpus files in {default_dir} — run scripts/corpus-churn.ps1 first")
    return max(files, key=os.path.getmtime)


def load_rows(path: str):
    rows = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass  # torn tail line from a live writer
    return rows


def build_matrix(rows):
    """Sparse one-hot over categorical sets + a few numerics."""
    import numpy as np
    from scipy import sparse

    vocab: dict[str, int] = {}

    def col(name: str) -> int:
        if name not in vocab:
            vocab[name] = len(vocab)
        return vocab[name]

    data, indices, indptr = [], [], [0]
    y_dps, y_ehp = [], []
    for r in rows:
        f = r["features"]
        feats: dict[int, float] = collections.defaultdict(float)
        feats[col("num:level")] = float(f.get("level", 0)) / 100.0
        feats[col("num:node_count")] = float(f.get("node_count", 0)) / 130.0
        feats[col("num:support_count")] = float(f.get("support_count", 0)) / 5.0
        feats[col(f"class:{f.get('class','')}")] = 1.0
        feats[col(f"asc:{f.get('ascendancy','')}")] = 1.0
        feats[col(f"skill:{f.get('main_skill','')}")] = 1.0
        for s in f.get("supports", []):
            feats[col(f"sup:{s}")] = 1.0
        for n in f.get("nodes", []):
            feats[col(f"node:{n}")] = 1.0
        for u in f.get("uniques", []):
            feats[col(f"uniq:{u}")] = 1.0
        for b in f.get("bases", []):
            feats[col(f"base:{b}")] = 1.0
        for k, v in sorted(feats.items()):
            indices.append(k)
            data.append(v)
        indptr.append(len(indices))
        lab = r["labels"]
        y_dps.append(np.log1p(max(0.0, float(lab.get("dps", 0)))))
        y_ehp.append(float(lab.get("ehp", 0)))
    x = sparse.csr_matrix(
        (data, indices, indptr), shape=(len(rows), len(vocab)), dtype="float32"
    )
    return x, np.array(y_dps), np.array(y_ehp), vocab


def main() -> None:
    ap = argparse.ArgumentParser()
    default_dir = os.path.join(
        os.environ.get("APPDATA", "."), "Moss", "MossRaven", "data", "corpus"
    )
    ap.add_argument("--corpus", default=None)
    ap.add_argument("--out", default="models")
    ap.add_argument("--min-rows", type=int, default=5_000)
    args = ap.parse_args()

    corpus = args.corpus or newest_corpus(default_dir)
    rows = load_rows(corpus)
    version = rows[0]["v"] if rows else "unknown"
    print(f"corpus: {corpus}  rows={len(rows)}  version={version}")
    if len(rows) < args.min_rows:
        sys.exit(
            f"only {len(rows)} rows — the value model needs >= {args.min_rows} "
            f"(SPEC suggests 500k+ for production). Keep corpus-churn.ps1 running."
        )

    import lightgbm as lgb
    import numpy as np
    from scipy import stats as sstats
    from sklearn.model_selection import train_test_split

    x, y_dps, y_ehp, vocab = build_matrix(rows)
    xtr, xte, dtr, dte, etr, ete = train_test_split(
        x, y_dps, y_ehp, test_size=0.15, random_state=7
    )
    params = dict(
        objective="regression", metric="l2", num_leaves=127, learning_rate=0.07,
        feature_fraction=0.8, bagging_fraction=0.8, bagging_freq=1, verbose=-1,
    )
    print("training dps head...")
    m_dps = lgb.train(params, lgb.Dataset(xtr, dtr), num_boost_round=600,
                      valid_sets=[lgb.Dataset(xte, dte)],
                      callbacks=[lgb.early_stopping(40), lgb.log_evaluation(100)])
    print("training ehp head...")
    m_ehp = lgb.train(params, lgb.Dataset(xtr, etr), num_boost_round=600,
                      valid_sets=[lgb.Dataset(xte, ete)],
                      callbacks=[lgb.early_stopping(40), lgb.log_evaluation(100)])

    pred = m_dps.predict(xte)
    spearman = float(sstats.spearmanr(pred, dte).statistic)
    # Pre-ranker KPI: of the TRUE top 5%, how many land in the predicted top 20%?
    k_true = max(1, int(0.05 * len(dte)))
    k_pred = max(1, int(0.20 * len(dte)))
    true_top = set(np.argsort(-dte)[:k_true])
    pred_top = set(np.argsort(-pred)[:k_pred])
    recall_at = len(true_top & pred_top) / len(true_top)
    print(f"holdout: spearman={spearman:.3f}  top5%→top20% recall={recall_at:.3f}")

    os.makedirs(args.out, exist_ok=True)
    tag = "".join(c if c.isalnum() or c in ".-" else "_" for c in version)
    m_dps.save_model(os.path.join(args.out, f"value-model-{tag}.txt"))
    m_ehp.save_model(os.path.join(args.out, f"value-model-{tag}.ehp.txt"))
    with open(os.path.join(args.out, f"feature-map-{tag}.json"), "w", encoding="utf-8") as fh:
        json.dump(vocab, fh)
    with open(os.path.join(args.out, f"report-{tag}.json"), "w", encoding="utf-8") as fh:
        json.dump(
            {"rows": len(rows), "version": version, "spearman": spearman,
             "recall_top5_in_top20": recall_at, "features": len(vocab)},
            fh, indent=2,
        )
    print(f"exported to {args.out}/ — wire into the engine via the Tier-3 inference task")


if __name__ == "__main__":
    main()
