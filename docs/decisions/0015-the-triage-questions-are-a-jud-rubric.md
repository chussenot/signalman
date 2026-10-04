---
title: 0015 The triage questions are a .jud rubric
description: The words of the five triage questions move from thirteen [triage.text] fields to one rubric file in the judgment crate's .jud format, read as the request for an alert that carries everything and lowered per alert in code, while the question set, the dynamic options and the thresholds stay where the code can hold them.
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

[Decision 0006](0006-layered-configuration.md) made the wording of the triage questions file configuration: thirteen `[triage.text]` fields (`owner_question`, `impact_levels`, `state_guard` and so on), merged onto compiled defaults by `TextsFile` and assembled into instructions by `Texts`. The fields were a request in all but name. A team that tuned them could not see the request the model reads without running `triage --print-request`. It could not add an instruction part, or reorder the questions, without a code change. Two copies of signalman, or signalman and a notebook, could not tell whether they asked the same thing except through signalman's own fingerprint. Judgment 0.4 published the `.jud` format, a YAML document whose questions are the wire's own objects in wire order, with a fingerprint any tool computes the same way, and started sending questions and options in the order they were given rather than alphabetically. Should signalman's questions become such a document, and how far should the document reach?

## Decision drivers

- The words are what a team tunes; they should be reviewable as the request the model reads, with nothing to translate
- The five questions and their primitives are what the policy reads through typed handles ([decision 0003](https://github.com/chussenot/judgment/blob/main/docs/decisions/0003-typed-handles-between-questions-and-answers.md)); configuration must not be able to change them
- Two of the five questions are asked over options known only per alert, and two instruction parts and two questions depend on what the alert carries
- The committed evaluation runs must keep replaying: their answers were given to specific requests
- Thresholds have one home, `[policy]`, with their own validation, fingerprint and frozen-policy discipline
- Configuration stays layered and file-only for wording ([decision 0006](0006-layered-configuration.md))

## Considered options

1. A `.jud` rubric for the words, read as the request for an alert that carries everything and lowered per alert in code; thresholds stay in `[policy]`
2. A `.jud` rubric for the words and the thresholds, the format's `policy` block replacing `[policy]`
3. Keep `[triage.text]`, and add an export of the effective request as `.jud` for review and comparison
4. Keep `[triage.text]` unchanged; take only the judgment 0.4 bump

## Decision outcome

Chosen option: 1. `[triage] rubric` names a `.jud` file, resolved relative to the configuration file; absent, the built-in rubric (`src/triage/triage.jud`, compiled in) is used. `src/triage/rubric.rs` checks a rubric against what the code relies on before the configuration loads: exactly `owner`, `impact`, `actionable`, `duplicate_of` and `caused_by_change`, each of the primitive the policy reads; instructions as objects of named non-empty parts with a `question`; the no-match options `none_of_these` and `none`; four impact levels as text; no `policy` and no `tuning`. `TriageQuestions::for_alert_with_rubric` lowers it per alert. The questions go in the rubric's order. `owner` and `duplicate_of` take their options from the candidates and the open incidents, with only the no-match option read from the rubric and sent last. The `catalog` and `context` parts, and the `duplicate_of` and `caused_by_change` questions, are sent only when the alert carries what they are about. `[triage.text]` is refused with a message naming its replacement; an empty table is accepted. `Texts`, `TextsFile` and the `take!` macro are deleted.

### Consequences

- Good, because the file a team edits is the request the model reads, in its order, with its anchors and comments; an instruction part the code never named (a `focus`, an example) is a line in the file
- Good, because the rubric has a `.jud` fingerprint, which `questions_fingerprint` now covers, so the identity of the questions is the format's, not only signalman's
- Good, because the built-in rubric asks exactly what the texts asked, which `tests/triage_rubric.rs` checks against requests captured before the change, so the committed runs keep replaying after a mechanical migration of their manifests' fingerprints
- Bad, because the rubric does not say everything that is sent: `owner`'s and `duplicate_of`'s other options are examples replaced per alert, and two parts and two questions are conditional, a contract stated in `docs/triage.md` and enforced in code rather than in the file ([judgment#8](https://github.com/chussenot/judgment/issues/8))
- Bad, because thresholds and words now live in two files of two formats, and the format's `tuning` provenance cannot be used for the thresholds until its gates can express a two-bar owner policy and a level threshold on a Score ([judgment#9](https://github.com/chussenot/judgment/issues/9))
- Bad, because the shared state rule is a YAML anchor on the first question, so reordering above `owner` means moving the anchor ([judgment#10](https://github.com/chussenot/judgment/issues/10))
- Bad, because a deployment with a non-empty `[triage.text]` fails to start after the upgrade until it is migrated; accepted over silently ignoring the table, which would change the questions without saying so

### Confirmation

`src/triage/rubric.rs` and its tests (the built-in rubric parses, its example owners equal `triage::default_teams`, each broken contract is refused by name); `src/triage/questions.rs` tests (order on the wire, no-match last, parts and questions per alert); `tests/triage_rubric.rs` (the built-in rubric against the captured requests; a rubric named in the configuration is what is asked; the effective configuration reads back); `src/config.rs` tests (`[triage.text]` refused, an empty one accepted); `tests/eval.rs` (the committed manifests match the rubric's fingerprint).

## Pros and cons of the options

### 1. A rubric for the words, lowered per alert

- Good, because the question set stays code and the words become one reviewable request
- Good, because the dynamic parts stay deterministic code, where the routing logic already is
- Bad, because the file is a template in practice and the format cannot yet say so

### 2. A rubric for the words and the thresholds

- Good, because one file would carry the questions and the bars tuned on them, with their provenance
- Bad, because the format's gates cannot express the owner's two bars or `page_at`, so half the policy would stay in `[policy]` anyway, and two homes for thresholds is the drift `[policy]` exists to prevent
- Bad, because `[policy]` has a frozen-policy and fingerprint discipline in the evaluation harness that a second source would have to reproduce

### 3. Keep `[triage.text]`, export the request as `.jud`

- Good, because nothing breaks for an existing deployment
- Bad, because the export is a derived view: the thing reviewed is not the thing edited, and the thirteen-field model still cannot add a part or reorder

### 4. Bump only

- Good, because it is the smallest change
- Bad, because it leaves the format unused where it fits best, and the wording keeps its 1:1 mirror of code fields

## More information

[Triage: the rubric](../triage.md#the-rubric) states the lowering contract; [Configuration](../configuration.md#triage-file-only) the setting and the migration from `[triage.text]`; [Evaluation](../evaluation.md#what-a-recording-answers) the fingerprint migration. The format is the judgment crate's [`.jud` specification](https://github.com/chussenot/judgment/blob/main/docs/jud.md) and its [decision 0014](https://github.com/chussenot/judgment/blob/main/docs/decisions/0014-a-file-format-for-rubrics-cases-and-recordings.md). The beads issue to create is "triage: questions as a .jud rubric (judgment 0.4)"; it closes `signalman-1zs.2`.
