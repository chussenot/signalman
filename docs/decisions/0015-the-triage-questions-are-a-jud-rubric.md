---
title: 0015 The triage questions are a .jud rubric
description: The words of the five triage questions, when each instruction part is sent, and the routing thresholds move from thirteen [triage.text] fields and the [policy] table to one jud 1.1 rubric, lowered per alert by the judgment crate, while the question set, when each question is asked and the shape of each gate stay where the code can hold them.
status: accepted
date: 2026-10-04
decision-makers: [platform engineering]
consulted: []
informed: []
last_reviewed: 2026-10-04
tags: [decisions, triage, configuration, jud, judgment]
---

# 0015 The triage questions are a .jud rubric

## Context and problem statement

[Decision 0006](0006-layered-configuration.md) made the wording of the triage questions file configuration: thirteen `[triage.text]` fields (`owner_question`, `impact_levels`, `state_guard` and so on), merged onto compiled defaults by `TextsFile` and assembled into instructions by `Texts`, with the six routing thresholds beside them in `[policy]`. The fields were a request in all but name. A team that tuned them could not see the request the model reads without running `triage --print-request`. It could not add an instruction part, or reorder the questions, without a code change. Two copies of signalman, or signalman and a notebook, could not tell whether they asked the same thing except through signalman's own fingerprint. And a threshold lived in one file while the words it was tuned against lived in another.

Judgment 0.4 published the `.jud` format, a YAML document whose questions are the wire's own objects in wire order, with a fingerprint any tool computes the same way. Version 1 described one fixed request, so it could not say that `owner` and `duplicate_of` are asked over options known only per alert, that two instruction parts and two questions depend on what the alert carries, or hold the owner's two bars and a level threshold on a Score. Those gaps were filed as [judgment#8](https://github.com/chussenot/judgment/issues/8), [#9](https://github.com/chussenot/judgment/issues/9) and [#10](https://github.com/chussenot/judgment/issues/10), and judgment 0.5 closed them with `jud: 1.1` ([judgment decision 0016](https://github.com/chussenot/judgment/blob/main/docs/decisions/0016-jud-takes-minor-versions.md)): `when` and `part_when` over state paths, `options_from: request`, named confidence `bands`, `level_at_least`, `strict` bars, and top-level `x-` keys for shared anchors. Should signalman's questions become such a document, and how far should the document reach?

## Decision drivers

- The words are what a team tunes; they should be reviewable as the request the model reads, with nothing to translate
- The five questions and their primitives are what the policy reads through typed handles ([decision 0003](https://github.com/chussenot/judgment/blob/main/docs/decisions/0003-typed-handles-between-questions-and-answers.md)); configuration must not be able to change them, nor whether a question the decision reads is asked
- A threshold is calibrated against the words it was tuned on; it should have one home, with validation, and the evaluation harness's fingerprint and frozen-policy discipline must keep working
- The committed evaluation runs must keep replaying: their answers were given to specific requests
- Configuration stays layered and file-only for wording and thresholds ([decision 0006](0006-layered-configuration.md))

## Considered options

1. A `jud: 1.1` rubric for the words, the per-alert conditions and the thresholds, lowered by the crate; `[policy]` removed
2. A `jud: 1` rubric for the words only, read as the request for an alert that carries everything and lowered per alert in signalman's code; thresholds stay in `[policy]`
3. Keep `[triage.text]`, and add an export of the effective request as `.jud` for review and comparison
4. Keep `[triage.text]` unchanged; take only the judgment bump

## Decision outcome

Chosen option: 1. `[triage] rubric` names a `.jud` file, resolved relative to the configuration file; absent, the built-in rubric (`src/triage/triage.jud`, compiled in) is used. `TriageQuestions::for_alert_with_rubric` calls the crate's `Rubric::lower` with the state `{alert: …}` and the options signalman supplies: the owner candidates for `owner`, the open incidents by id for `duplicate_of`. The rubric decides, by state path, which instruction parts are sent; the built-in one sends `catalog` under `alert.component` and `context` under `alert.related_alerts`, and asks `duplicate_of` under `alert.open_incidents` and `caused_by_change` under `alert.recent_changes`.

`src/triage/rubric.rs` checks a rubric against what the code relies on when the configuration loads:

- exactly `owner`, `impact`, `actionable`, `duplicate_of` and `caused_by_change`, each of the primitive the policy reads;
- instructions as objects of named non-empty parts with a `question`;
- `owner` and `duplicate_of` asked over supplied options, with `none_of_these` and `none` as their one static option;
- each question's `when` as above and no other;
- four impact levels as text;
- each `policy` gate in the one shape `decide` reads, mapped onto `Policy`: `actionable.threshold` is `suppress_below`; `owner`'s `route` and `confirm` bands are `auto_route_confidence` and `human_below_confidence`; `impact.level_at_least` is `page_at`; `duplicate_of.confidence` is `attach_confidence`; `caused_by_change.threshold`, `strict`, is `flag_change_above`.

`[triage.text]` and `[policy]` are refused with a message naming their replacement; an empty table is accepted. `Texts`, `TextsFile` and the `take!` macro are deleted. `Policy` stays the in-memory type, so `decide`, the outcome contract and the evaluation harness are unchanged.

### Consequences

- Good, because the file a team edits is the request the model reads, in its order, with its conditions, its anchors and comments; an instruction part the code never named (a `focus`, an example) is a line in the file, and so is the condition it is sent under
- Good, because the thresholds sit beside the words they were tuned on, in one reviewed file, and the format's `tuning` block is there for their provenance
- Good, because the rubric has a `.jud` fingerprint that covers its declarations, which `questions_fingerprint` now uses, so the identity of the questions is the format's, not only signalman's; the fingerprint excludes the policy, so tuning a threshold leaves a recording current, and the harness fingerprints the policy on its own as before
- Good, because the built-in rubric asks exactly what the texts asked, which `tests/triage_rubric.rs` checks against requests captured before the change, so the committed runs keep replaying after a mechanical migration of their manifests' fingerprints
- Good, because the per-alert lowering that signalman had written by hand is the crate's, tested there for every implementation of the format
- Bad, because a gate can say more than `decide` reads (bands with other names, a strict confidence, a fallback other than the no-match option), so signalman refuses those shapes by name rather than honouring the full format; a team that reads the format's documentation may write a gate signalman will not load
- Bad, because a deployment with a non-empty `[triage.text]` or `[policy]` fails to start after the upgrade until it is migrated; accepted over silently ignoring the table, which would change the questions or the routing without saying so
- Bad, because signalman needs judgment 0.5 or later, and a rubric file written for it says `jud: 1.1`, which a `jud: 1` reader refuses

### Confirmation

`src/triage/rubric.rs` and its tests (the built-in rubric parses and its policy is `Policy::default()`; each broken contract and each gate `decide` cannot honour is refused by name; the policy is read from the gates); `src/triage/questions.rs` tests (order on the wire, no-match last, parts and questions per alert); `tests/triage_rubric.rs` (the built-in rubric against the captured requests; a rubric named in the configuration is what is asked; the effective configuration reads back); `src/config.rs` and `tests/config_precedence.rs` (`[triage.text]` and `[policy]` refused, empty ones accepted; the thresholds come from the rubric the file names); `tests/eval.rs` (the committed manifests match the rubric's fingerprint).

## Pros and cons of the options

### 1. A 1.1 rubric for the words, the conditions and the thresholds

- Good, because one file carries the questions, when their parts are sent, and the bars tuned on them
- Good, because the lowering is the format's, so another tool asks the same request for the same alert
- Bad, because signalman accepts only the subset of the gates its decision reads, and must say so

### 2. A 1.0 rubric for the words, lowered in code

- Good, because it needed nothing beyond judgment 0.4, and was this record's first draft
- Bad, because the file was a template in practice: `owner`'s and `duplicate_of`'s options were examples replaced per alert, and the conditions a contract stated in a page and enforced in code
- Bad, because thresholds and words lived in two files of two formats, and the shared rule had to be an anchor on the first question

### 3. Keep `[triage.text]`, export the request as `.jud`

- Good, because nothing breaks for an existing deployment
- Bad, because the export is a derived view: the thing reviewed is not the thing edited, and the thirteen-field model still cannot add a part or reorder

### 4. Bump only

- Good, because it is the smallest change
- Bad, because it leaves the format unused where it fits best, and the wording keeps its 1:1 mirror of code fields

## More information

[Triage: the rubric](../triage.md#the-rubric) states the contract and [Triage: the decision](../triage.md#the-decision) the gates; [Configuration](../configuration.md#triage-file-only) the setting and the migrations from `[triage.text]` and `[policy]`; [Evaluation](../evaluation.md#what-a-recording-answers) the fingerprint migrations. The format is the judgment crate's [`.jud` specification](https://github.com/chussenot/judgment/blob/main/docs/jud.md) and its decisions [0014](https://github.com/chussenot/judgment/blob/main/docs/decisions/0014-a-file-format-for-rubrics-cases-and-recordings.md) and [0016](https://github.com/chussenot/judgment/blob/main/docs/decisions/0016-jud-takes-minor-versions.md). The beads issue to create is "triage: questions and thresholds as a .jud rubric (judgment 0.5)"; it closes `signalman-1zs.2`.
