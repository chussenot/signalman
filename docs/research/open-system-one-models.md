---
title: Open System One models
description: What the open reproductions of TypeSafe's Jev, the Jev Decision Index leaderboard and the Laya project establish, and the engineering decisions for judgment and signalman that follow from it: provider portability, self-hosting economics, question design rules, calibration policy and evaluation method.
status: current
last_reviewed: 2026-09-25
tags: [research, typesafe, jev, laya, judgment, evaluation, calibration]
---

# Open System One models

signalman depends on one hosted model, Jev, through one wire shape, `POST /v1/systemone`. Two decisions keep coming back: whether to keep paying for and depending on that model, and how to write questions and thresholds that survive a change of model. Both need facts about the field rather than opinions, and the field moved fast enough in September 2026 that the facts were spread across a leaderboard, a news tracker and one project's README. This page pulls them into one place and says what each one changes for this project. It is a research note, not a decision record: it feeds [decision 0002](../decisions/0002-calibrated-judgments-over-generated-text.md), [decision 0010](../decisions/0010-extract-the-judgment-core-into-a-crate.md) and the roadmap items on evaluation and self-hosting, and it is meant to be re-read when a provider choice comes up.

Sources, read on 2026-09-25:

- The [Jev Decision Index](https://huggingface.co/spaces/multimodalart/jev-decision-index), a community leaderboard and news tracker maintained by multimodalart (apolinario), edition 0.2 of 2026-09-24: nearly fifty open reproductions plus Jev 1.13.0 on one frozen suite, with a [methodology](https://huggingface.co/spaces/multimodalart/jev-decision-index/blob/main/methodology.html) page and the [build scripts](https://github.com/apolinario/decision-index). Unofficial and not affiliated with TypeSafe.
- The [Laya repository](https://github.com/NandhaKishorM/laya) (Convai Innovations, Apache 2.0), README at 0.3.20, its [documentation site](https://nandhakishorm.github.io/laya/) and the [typed-decisions dataset](https://huggingface.co/datasets/LocalLLaMA/typed-decisions).
- This project's own runs: [Laya as a model provider](../laya.md) (2026-09-21, zero-shot on three alerts) and [judgment against Laya typed-decisions](../judgment-laya-typed-decisions.md) (2026-09-24, the crate against the fine-tuned checkpoint on its benchmark).

Numbers from the first two are as published by their authors; nothing on this page was re-measured except where the third source says so. Confidence in each conclusion is stated where it is less than high.

## The landscape in one paragraph

Jev's interface is public and simple: a state, typed questions, one pass, a probability per option. Its weights, its training method (RLCD, reinforcement learning for calibrated decisions) and its data are not. Within a week of the launch the open community had rebuilt the interface three ways, and the tracker lists over sixty artifacts. **Parallel constrained decoding** prefills the state once on a stock language model and reads every option's logit in one batched pass: no new weights, Jev's shape and speed, none of its training. **Diffusion language models** (DiffusionGemma) are patched to fill the typed slots of a schema in parallel. **Trained heads and fine-tunes** put a scoring head on a frozen backbone, LoRA or fully fine-tune a 0.6B to 35B decoder, or train a small encoder from scratch; Laya is in this group. The tracker's own summary of what is still not in the open, as of its 2026-09-24 snapshot: TypeSafe's weights, the RLCD algorithm, and any open model matching Jev's calibration claims. The best open scorers reach about 90% agreement with Jev on TypeSafe's public cases (Bespoke Nimble), and the independent studies (jev-on-a-laptop, snapjudge) find that a stock model's softmax over option logits does not reliably flag its own errors.

## What the Decision Index measures, and why its rules matter here

The index is the first apples-to-apples comparison, and its rules are a better evaluation method than the one this project has. Edition 0.2 runs 40 static benchmarks in five equal-weight areas (knowledge and reasoning, language understanding, retrieval and classification, tools, arts) over 121,057 requests, every model answering byte-identical requests on one 96 GB GPU, Jev over its hosted API. The headline is chance-corrected: each benchmark's score is mapped to (score − chance) / (1 − chance) before averaging, so a yes/no benchmark no longer hands out fifty free points and 0 means guessing. Calibration is measured on 32 benchmarks as the probability placed on the chosen option against whether it was right.

The hard rules are the part to copy. No truncation: a request the model's context cannot hold is recorded as unsupported, never shortened. No label filtering: the full option set is offered, so a model that caps options at 26 letters loses those rows. No prompt tuning: each entrant runs its own published inference code. No correctness-based retry: transport errors retry twice, a valid wrong answer never. Native abstentions stand. Everything is recorded: state, questions, native output, mapped answer, probabilities, errors and synchronised wall time, with model load and warm-up excluded. Answer gaps are mined into one line per cause and shown next to the score, so a model that refuses 12% of requests because it supports 26 options is ranked on what it answered and labelled with what it did not.

Jev 1.13.0 on that suite: skill index 51.7 (raw accuracy 63.9), calibration accuracy 0.739 at mean confidence 0.812, ECE 0.074, median 253 ms and p95 437 ms over HTTPS. Per area its ECE ranges from 0.02 on tool selection to 0.15 on retrieval and classification, which is the kind of question signalman asks. A separate study on the tracker (jev-ood-calibration) refit a temperature of 2.7 on 900 rule-generated tickets Jev cannot have seen, against 0.96 to 1.35 on public benchmarks, and found Choice and Score over-confident and the boolean primitive under-confident out of domain. Alert triage is out of domain.

## The leaderboard, read for this project

The full board has nearly fifty rows; these are the ones that inform a decision here. Skill is the chance-corrected index (Jev 51.7); ECE and accuracy are the calibration sample; latency is in-process on the index's GPU, not comparable with Jev's network figure.

| Model | Kind | Base and size | Skill | ECE | Acc. | Median ms | Weights |
|---|---|---|---|---|---|---|---|
| AutoJev-27B | full fine-tune | Qwen3.8-27B | 50.9 | 0.018 | 0.730 | 105 | open |
| Surogate Rune 26B-A4B | full fine-tune, MoE | Gemma-4-26B-A4B | 47.2 | 0.171 | 0.695 | 680 | open, GGUF |
| Decider chat | inference technique | stock Qwen3.6-27B | 46.1 | 0.021 | 0.697 | 918 | none needed |
| Jevfire | inference technique | stock Qwen3.8-27B | 45.7 | 0.052 | 0.689 | 78 | none needed |
| Winnow-12B | LoRA | Gemma-4-12B | 45.1 | 0.168 | 0.670 | 49 | open |
| Decider 35B-A3B | full fine-tune, MoE | Qwen3.5-35B-A3B | 43.5 | 0.023 | 0.696 | 99 | open |
| Decision 1.0 Lux | head and adapter | Qwen3.5-9B | 39.0 | 0.076 | 0.664 | 47 | open |
| Xor | full fine-tune, multimodal | Qwen3.6-35B-A3B | 38.8 | 0.015 | 0.709 | 180 | open |
| Bespoke Nimble 9B v2 | LoRA, open data and recipe | Qwen3.5-9B | 36.7 | 0.024 | 0.643 | 76 | open |
| Decider 4B | full fine-tune | Qwen3.5-4B | 36.6 | 0.084 | 0.649 | 23 | open |
| Kev 9B | LoRA and head, train-it-yourself | Qwen3.5-9B | 35.4 | 0.138 | 0.652 | 52 | open |
| Tev1-4B | supervised fine-tune, hosted at Together | Qwen3.5-4B | 26.3 | 0.104 | 0.635 | 31 | open |
| Decision 1.0 Kai | encoder, multilingual | mmBERT-base, 308M | 7.0 | 0.185 | 0.358 | 30 | open |
| Laya | encoder | ModernBERT-large, 421M | 5.5 | 0.140 | 0.377 | 18 | open |
| Verdict (heman10x) | encoder with an abstain option | GLiClass ModernBERT, 151M | 1.8 | 0.154 | 0.369 | 11 | open |

Five readings.

1. **Jev is matched, not beaten, and only at 27B.** AutoJev-27B reaches 50.9 against Jev's 51.7 with a better ECE. Everything within ten points of Jev is a 12B to 35B model. That size class needs a data-centre GPU, which is not a cost this project's alert volume justifies.
2. **A stock model with the decoding trick gets 90% of the way.** Decider chat and Jevfire train nothing and score 46. The interface is the easy part; the last five points and the calibration are what training buys.
3. **Calibration is independent of accuracy, and open models can beat Jev at it.** Decider, Xor, AutoJev and Nimble sit at ECE 0.015 to 0.025 against Jev's 0.074. Calibration comes from a fitting step on held-out data, not from the base model, and the best of them (Hopper) sets a temperature per option count, state length, answer type and entropy without changing the answer. This is the strongest argument that thresholds belong to a model version, not to a question.
4. **Small models land at 25 to 37, encoders below 10.** A 4B decoder fine-tune (Decider 4B) is a plausible self-hosted engine for a narrow domain; a 421M encoder is not a general engine at all. Laya's 5.5 on this index and its 0.766 on the benchmark it was fine-tuned for are the same fact from two sides: capability comes from fine-tuning on the target distribution, and none is left over for anything else.
5. **Option caps are the most common failure.** JevK5, SemIf, Tev1, reflex, Solomon and mini-jev refuse 12% to 23% of the suite because they answer with one of 16 to 26 letters or accept 2 to 8 options. Nimble v2 raised its cap to 255 and moved up. Abstention is the other way to lose rows: the Verdict on the board chose its own abstain option on 47% of requests, and the index counts that as unanswered. Any question whose option list can grow (owners from a catalog, candidate incidents) has to be capped in code before the wire, whatever the model.

Two rows deserve a note because they change the provider question. **meraGPT Decider 1** is a proprietary hosted model that tops the typed-decisions dataset's own leaderboard at 0.768 with a Brier of 0.052 against Jev's 0.727 and 0.148, answers `/v1/systemone` at meragpt.com at $0.03 per million input tokens, and is not on the Decision Index. **Tev1-4B** is served on Together's serverless at $0.042 per million tokens, Jev's list price, with the data recipe published. Either is a second hosted provider on the same wire; neither has been tried here.

## Laya, precisely

Laya is a 421M-parameter ModernBERT-large encoder with a decision head that scores every option at its own mask token in one forward pass, trained with RLCD against strictly proper scoring rules (log, spherical and ranked probability). Three checkpoints: English (512-token context), multilingual (mmBERT-base, 322M, 1,024 up to 8,192) and typed-decisions (English, fine-tuned). A `Router` picks by script and language before the forward pass, because the English checkpoint scores 0.000 on Khmer at 0.952 confidence: the model's own confidence gives no warning, so the routing decision cannot come after inference. Published speed is 33 ms per question on a T4 and 7 ms per question batched; this project measured 2.1 s per five-question request on four CPU cores.

What its README documents that a consumer must know, each confirmed against an issue number or a measurement:

- **Options share a token budget** of 192 (English) or 256 tokens; at 77 options each label gets three or four tokens and accuracy collapses (Banking77 0.425 against Jev's 0.870). Past about 20 options, raise the budget, shortlist with embeddings first (`predict_shortlist`), or split the question.
- **Boolean-word labels are unsafe.** Choice keys are rendered verbatim, and `true`/`false` or `yes`/`no` keys can be followed instead of the descriptions. Use semantic or opaque keys.
- **Negation is not made safe by semantic labels.** Four of four negated cancellation requests chose `cancel_account` on the English checkpoint (issue 377).
- **Noul can follow its option labels instead of the state** on the English checkpoint (issue 156). The `labels` override and explicit `true`/`false` criteria are the workaround.
- **Score is the weakest primitive** (SST-5 0.372), and the multilingual checkpoint rarely picks the first level.
- **`act_probability` carries no signal** (AUROC 0.30); gate on `confidence` (0.77) or on the answer's probability.
- **Both base checkpoints are over-confident as shipped.** One temperature per question type and option count, fit on held-out data, moves ECE from 0.466 to 0.081. The multilingual checkpoint ships with no temperatures at all. At load, temperatures outside 0.5 to 5 are clamped with a warning; the typed-decisions checkpoint triggers that warning for Choice questions with eleven or more options.
- **Fine-tuning is where the value is.** The base checkpoints sit below the majority-class baseline on typed-decisions (0.36 against 0.46); the fine-tune reaches 0.766 in four to five hours on Kaggle's free two T4s from 1,200 cases. A worked browser-agent example goes from 0.10 to 0.66 top-1 among 45 candidates on one 16 GB GPU.

What it ships that this project can use: `laya-serve`, the wire-compatible server this project tested; prediction hooks (audit logging, redaction before inference, caching) that run inside the server rather than in every client; workflow presets, including `triage_questions` for support tickets, which are a reference for how the authors phrase questions; an MCP server exposing decisions as tools; ONNX and Apple-silicon runtimes from the community; and the fine-tuning notebook that fits temperatures as part of the loop.

## What this changes for signalman

### Provider portability is the asset; keep it

The field converged on Jev's wire. More than a dozen of the projects on the tracker serve `/v1/systemone`, two of them as hosted APIs. That makes the choice of model a configuration and evaluation decision, not a code one, as long as the client stays generic. The `judgment` crate is that client, and the Laya run proved it drives a second implementation unchanged. The recommendation is to protect that property deliberately: no provider-specific field in the crate's public types, backend limits (option cap, level cap, context) expressed as data the caller can read rather than constants, and the `SystemOne` trait as the only seam. A second consequence is cheap resilience: a hosted fallback provider on the same wire is a `base_url`, a `model` and a threshold profile away, and is worth more than a self-hosted model for an alert path that must not stall when one API is down.

### Self-hosting is a residency decision, not a cost decision

The economics do not favour self-hosting for alert triage. An alert request is on the order of a thousand input tokens; at Jev's $0.042 per million that is well under a hundredth of a cent per alert, or about half a dollar a day at ten thousand alerts. Jev's 253 ms median is far inside an alert's budget. A GPU able to run a 9B model costs more per day than the API at that volume, and a 27B-class model that actually matches Jev needs a 96 GB card. The two reasons that can still justify self-hosting are data residency (alert text, which carries hostnames, customer identifiers and sometimes secrets in messages, leaving the network) and provider risk. Both are policy inputs, and the page should be re-read with the actual volume and the actual policy when the question is asked.

If residency or risk does force it, the candidates in order: a fine-tuned Laya (CPU-capable, Apache 2.0, seconds per alert, the cheapest to run, but needs labelled alert history to fine-tune and stays weak outside what it was tuned on); a 4B to 9B decoder with an open recipe (Decider 4B, Bespoke Nimble, Kev: better breadth, calibration in the 0.02 to 0.08 range, needs one mid-range GPU); a 27B model only if the volume is shared with other workloads. Every path starts with the same prerequisite, labelled history in the [evaluation harness](../evaluation.md), which is why the roadmap orders it first.

### Question design rules the field confirms

signalman's questions were written from TypeSafe's guidance. The open reproductions, being weaker, expose which rules carry the most weight, and each of these is checked against the current questions:

- **Cap dynamic option lists in code.** Owner candidates and duplicate candidates are dynamic Choices. Jev takes 255 options; most open models take 16 to 26 and Laya degrades past 20. Backstage enrichment caps owner candidates at 24 (`DEFAULT_MAX_CANDIDATES` in `src/backstage/enrich.rs`) plus the no-match option, 25 in all: inside Jev's limit and the 26-letter models, above the point where Laya's per-option token budget starts to bite. Related incidents are capped by the sync's own maximum. Passes for Jev; a smaller model wants the owner cap lowered to about 15, which is a setting, not a code change.
- **No boolean-word keys.** The owner Choice uses team ids and `none_of_these`; the duplicate Choice uses incident ids and `none`. No key is a yes/no word. Passes.
- **Every Choice has a no-match option.** Already a rule in this repository, and Verdict's conformal abstain set is the same idea from the other side. Passes.
- **Phrase instructions positively.** Negation fails on small models. A pass over the question texts for "not", "no" and "never" in the instruction is cheap and worth doing when the texts are next edited.
- **Score is the weakest primitive everywhere.** The impact Score has four levels and drives tags and the note. Laya scored it flat on the three example alerts. When labelled history exists, the first thing to measure is whether impact would be better as a Choice over the same four described levels; the wire cost is the same and a Choice reports a confidence the policy can use.
- **Keep Noul criteria explicit.** The `NoulCriteria` type exists for this, since one checkpoint follows its option labels rather than the state when the criteria are absent. `actionable` names what yes and no mean; `caused_by_change` sends no criteria. Fails on one of two: the change question should describe both outcomes when the texts are next edited.

### Calibration policy: thresholds belong to a model version

Confidence is not one quantity. TypeSafe computes it from the spread of the distribution; Laya reports one minus normalised entropy under the same name and adds the answer's probability as `answer_confidence`; several open models report a temperature-scaled softmax. The policy's `attach_confidence` of 0.75 and `auto_route_confidence` of 0.70 were set against Jev and mean nothing against another provider until refit. Jev itself is over-confident out of domain by its own measured temperature. The rules that follow: the outcome document carries the model version (it does), thresholds are recorded with the version they were tuned on, the evaluation harness reports ECE per question and refuses to bless a threshold profile when the ECE at the threshold is above an agreed bound, and a change of `typesafe.model` is treated as a change of policy that needs a replay before it ships. A per-model threshold profile in the configuration, selected by the model name the response reports, is the mechanism; it is not built yet.

### Evaluation method: adopt the index's rules

The harness grades accuracy, Brier and ECE per question and decision agreement. Four things the index does that it does not: chance-corrected accuracy, so a yes/no question is not credited with fifty points; an answered rate with the refusal causes, so a provider that cannot take a question is visible rather than silently skipped; a frozen corpus with a content hash, so two runs are comparable by construction; and a paired comparison on identical requests when two models are compared, rather than two separate reports. The first two are small additions to `judgment::eval`; the third exists as the recording hash; the fourth is a reporting change. The Brier score should also be documented as the multi-class sum it is, since the benchmark and the model card use a different form and the numbers are not comparable.

## What this changes for judgment

- The crate is a client to a de-facto standard, and its value is that any of the servers above works with it. Keep it provider-neutral and test it against a second server on every release; the ignored live tests and the benchmark example exist for that.
- Expose backend limits as data. A `Limits` value on the backend (maximum options, maximum levels, context in tokens) would let a caller shortlist before the wire and let the harness report a refusal as a gap rather than an error. Jev's 255 and 10 are the crate's constants today; a second backend wants its own.
- Offer the answer's probability as a first-class accessor. Every open model and the benchmark grade on it; the crate's `Choice` and `Score` views make the caller compute it from the distribution. A method on the typed views removes that and makes the calibration numbers comparable across providers.
- Keep unknown response fields. Done in judgment 0.2: top-level fields the crate does not model, such as Laya's `routing`, are kept in `Response::extra` and written back into recordings, so an operator can log which checkpoint answered without a second type change. Fields inside an answer, such as Laya's `answer_confidence` and `action`, are still ignored, as in the Python SDK; model them only when a consumer needs them.
- Do not add batching, images or streaming on speculation. Two servers offer a batch endpoint and two accept images; neither is on the standard wire and no consumer here needs them.

## Watch list

Projects whose next release would change a conclusion above, with the reason to watch each:

| Project | Why it matters here |
|---|---|
| [Bespoke Nimble](https://github.com/bespokelabsai/nimble) | The only fully open recipe (data curation included); 255 options; 90% agreement with Jev. The reference if a 9B self-host is ever built. |
| [Decider](https://github.com/Mapika/decider) | Best calibration on the index across sizes; `pip install decider-ai`; a 4B checkpoint that fits one consumer GPU. |
| [Kev](https://github.com/jaredpalmer/kev) | Train-it-yourself family with a Modal script; Kev-4B trains in 40 minutes on one H100. |
| [Decision 1.0](https://huggingface.co/collections/llm-semantic-router/decision-10-6ab12177bd0002394d8409f9) | The vLLM Semantic Router team's encoders and decoders, 50 languages; Kai outscores Laya's base checkpoints. The multilingual alternative to Laya. |
| [Hopper](https://github.com/hopit-ai/hopper) | A calibration map by option count, state length, answer type and entropy that never changes the answer: the design for a per-question-shape threshold profile. |
| [Verdict](https://github.com/Manavarya09/verdict) (Manavarya09, not the one on the board) | Conformal abstention with a coverage guarantee, fit on the caller's labels in seconds on a CPU; runs in the browser via ONNX. The principled version of "none of these". |
| [meraGPT Decider 1](https://meragpt.com/models/state-decider-1) | Hosted, same wire, tops the typed-decisions leaderboard, cheaper than Jev. The candidate second provider. |
| [Laya](https://github.com/NandhaKishorM/laya) | The CPU-capable fine-tune path, the server this project tested, and the fine-tuning notebook that fits temperatures. |
| [jev-benchmarks](https://github.com/AbdelStark/jev-benchmarks) and [jev-ood-calibration](https://github.com/scienthoon/jev-ood-calibration) | Independent calibration and selective-risk measurements of Jev itself; the latter is the evidence that Jev is over-confident out of domain. |

## Open questions

- **Jev's calibration on alerts is unmeasured.** Every number above is on public benchmarks or synthetic workflows. The out-of-domain temperature of 2.7 suggests the policy thresholds are optimistic; only labelled history can say by how much. Confidence: medium that the thresholds need lowering, low on the amount.
- **Whether impact should be a Choice** is a measurement, not an argument; it waits on the same history.
- **A second hosted provider has not been tried.** The crate would need no change; the policy would need its own threshold profile. Confidence that the wire works: high, given two independent implementations already do; on judgment quality on alerts: none, unmeasured.
- **The Decision Index's interactive environments have not been run for any reproduction**, and its point estimates carry no confidence intervals yet (the changelog says bootstrap intervals are not calculated). Rankings within two or three points are not distinguishable.
