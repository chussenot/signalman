---
title: Laya as a model provider
description: What Laya is, how signalman ran against it unchanged through a local System One-compatible shim, what the three example alerts measured on CPU, and why Jev stays the default until an evaluation on real alert history says otherwise.
status: experiment
last_reviewed: 2026-09-25
tags: [typesafe, laya, model, evaluation]
---

# Laya as a model provider

[Laya](https://huggingface.co/convaiinnovations/laya) ([source](https://github.com/NandhaKishorM/laya); Convai Innovations, Apache 2.0) is an open-weights System One decision model: a 421M-parameter ModernBERT-large encoder with a decision head that scores every option at its own mask token in one forward pass. It takes the same request shape as TypeSafe's API, a `state` plus `noul`, `choice` and `score` questions with `instructions` and `criteria`, and returns the same answer shape with probabilities and confidence. It never generates text. Three checkpoints ship in one repository: English (root), multilingual (`mmBERT-base`, 322M) and `typed-decisions`, the English model fine-tuned on a four-workflow typed-decisions benchmark.

This page is an experiment, not a supported configuration. Jev's per-token cost, its latency and the fact that alert text leaves the network are real costs, and an open-weights model that speaks the same wire shape is the obvious way to remove them; the question is whether its judgments are good enough to route on. Nothing in the binary knows about Laya, the only artefact in the repository is a shim under `examples/`, and the measurement below is three synthetic alerts. It becomes `current` when the [evaluation harness](evaluation.md) has compared both models on labelled alert history and Laya, fine-tuned, matches Jev's accuracy and calibration per question; at that point self-hosting becomes a documented `typesafe.model` option rather than a page. Until then the page records what was tried so nobody repeats it.

## It runs, and signalman needs no code change

The [configuration layer](configuration.md) already makes the model endpoint a setting. `examples/laya/serve_laya.py` is a short HTTP shim that loads a checkpoint with the `laya` package and serves `POST /v1/systemone` and `GET /v1/models`:

```sh
python -m venv .venv
.venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu   # or a CUDA build
.venv/bin/pip install laya
USE_TF=0 .venv/bin/python examples/laya/serve_laya.py                          # port 8099

TYPESAFE_BASE_URL=http://127.0.0.1:8099 TYPESAFE_API_KEY=unused \
  signalman triage examples/alerts/crashloop.json
```

On 2026-09-21, in a 4-CPU, 15 GB container with no GPU, the English checkpoint loaded in 42 s (843 MB of weights) and every signalman command worked against it: `models`, `triage` on all three example alerts, `--json`. The extra `rl_agent` field Laya adds to each answer is ignored by the lenient wire types. The API key is still required by the client and ignored by the shim.

## What the example alerts measured

Zero-shot, no fine-tuning, no temperature refit, CPU only. The expected column is what a responder would say.

| Alert | Expected | Laya English | Laya typed-decisions |
|---|---|---|---|
| `crashloop` (OOMKilled after a deploy, checkout-api) | application or platform; caused by change; major | owner platform 0.53, confidence 0.23; change 0.82; impact 1.85 (major); duplicate of INC-4821 0.86 | owner platform 0.36, confidence 0.13; change 0.65; impact 2.21; duplicate 0.81 |
| `dns` (probe failing, blackbox) | network; major | owner none of these 0.45; impact 2.03 | owner observability, confidence 0.05; impact 2.24 |
| `disk-noise` (disk 81 %, no user impact) | suppress: not actionable, none | actionable 0.48; impact 1.53 (major) | actionable 0.53; impact 1.75 (major) |

Every decision was `HumanTriage`: the [policy](triage.md#the-decision) refused to route on owner confidence between 0.04 and 0.45, which is the right behaviour for the wrong reason. Three things stand out:

- **Impact does not discriminate.** Every alert, including the disk-usage noise, scored between 1.5 and 2.2 on the four-level scale. The model card says ordinal `score` is Laya's weakest primitive.
- **The duplicate question is over-attached.** Both checkpoints put over 0.8 on INC-4821 (a payments-gateway 5xx incident) for a checkout-api crash loop. Plausible causally, but it is a separate problem, and with the default `attach_confidence` of 0.75 only the low Choice confidence (0.31 to 0.43) kept the alert from being attached.
- **Latency on CPU is 2 to 6 seconds per request** (five questions in one call). The model card's 33 to 40 ms figures are on a T4 GPU; Jev's API is measured by third parties at 236 to 276 ms.

None of this contradicts the model card, which states that the base checkpoints are near chance on typed decisions zero-shot and that Laya is a base to specialise rather than a zero-shot decision engine. The `typed-decisions` checkpoint's 0.766 accuracy belongs to that benchmark's own training split; on our alerts it was not better than the base.

## Verdict

Laya can stand in for Jev at the wire level today, and it cannot stand in for Jev at the judgment level today. The path to a real answer runs through the [evaluation harness](roadmap.md#triage-quality-signalman-ufg): replay labelled historical alerts through both models, compare accuracy, calibration and decision agreement per question, fine-tune Laya on the training split with the published notebook, refit its temperatures on our data, and then decide with numbers whether self-hosting (a GPU, tens of milliseconds, no per-token cost, alert text never leaving the network) beats the API. Tracked as `signalman-ufg.5`.

Until then Jev remains the default `typesafe.model`, and the only Laya artefact in the repository is the shim.

Since this page was written the typed client became the `judgment` crate and Laya grew its own server, `laya-serve`, so the shim is no longer the only way to run it. [judgment against Laya typed-decisions](judgment-laya-typed-decisions.md) records the crate's compatibility run against that server and the `typed-decisions` checkpoint on the benchmark it was fine-tuned for, with the ignored live tests and the replay example that produced the numbers.
