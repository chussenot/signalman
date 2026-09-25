#!/usr/bin/env python3
"""Export the typed-decisions benchmark to the JSONL the example reads.

The benchmark (https://huggingface.co/datasets/LocalLLaMA/typed-decisions,
Apache 2.0) ships as Parquet; the example reads one JSON object per line so it
needs no Parquet reader in Rust and a case can be inspected with any editor.
Each line keeps the columns the replay needs: id, workflow, state, questions
and gold. The other columns (factors, label_agreement) describe how the gold
was produced and are not needed to score against it.

    pip install pyarrow huggingface_hub
    python export.py --split test --out typed-decisions-test.jsonl
    python export.py --split test --per-workflow 10 --out sample.jsonl
"""
import argparse
import json

from huggingface_hub import hf_hub_download
import pyarrow.parquet as pq

parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--split", default="test", choices=["test", "train"])
parser.add_argument("--per-workflow", type=int, default=0, help="keep the first N cases of each workflow (0 = all)")
parser.add_argument("--out", required=True)
args = parser.parse_args()

path = hf_hub_download("LocalLLaMA/typed-decisions", f"all/{args.split}-00000-of-00001.parquet", repo_type="dataset")
rows = pq.read_table(path).to_pylist()
kept = {}
with open(args.out, "w", encoding="utf-8") as out:
    for row in rows:
        n = kept.get(row["workflow"], 0)
        if args.per_workflow and n >= args.per_workflow:
            continue
        kept[row["workflow"]] = n + 1
        case = {
            "id": row["id"],
            "workflow": row["workflow"],
            "state": json.loads(row["state"]),
            "questions": json.loads(row["questions"]),
            "gold": json.loads(row["gold"]),
        }
        out.write(json.dumps(case, ensure_ascii=False, sort_keys=True) + "\n")
print(f"wrote {sum(kept.values())} cases to {args.out}: " + ", ".join(f"{k} {v}" for k, v in sorted(kept.items())))
