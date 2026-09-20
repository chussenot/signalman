---
title: 0002 Calibrated judgments over generated text
description: Use TypeSafe's System One primitives, which return typed answers with probabilities, instead of a generative model with prompt-and-parse output.
status: accepted
date: 2026-09-20
decision-makers: [platform engineering]
consulted: []
informed: [on-call leads]
last_reviewed: 2026-09-20
tags: [decisions, typesafe]
---

# 0002 Calibrated judgments over generated text

## Context and problem statement

Routing an alert needs semantic understanding of its text, which rule engines lack. A model can supply it. Which kind of model output can a paging decision safely rest on?

## Decision drivers

- A decision that pages someone at 03:00 must be traceable to a number and a threshold
- The same alert should produce the same routing on repeated runs
- Operators must be able to tune behaviour without re-running inference
- Uncertainty must be a first-class signal, not hidden in prose

## Considered options

1. TypeSafe System One primitives: Choice, Noul, Score, returning probabilities and confidence
2. A generative LLM with a structured-output prompt, parsed into a decision
3. A generative LLM asked for a free-text recommendation, read by the on-call engineer

## Decision outcome

Chosen option: "TypeSafe System One primitives". Every judgment is a narrow typed question; code combines the probabilities with explicit thresholds. No model generates text anywhere in signalman.

### Consequences

- Good, because every decision cites specific probabilities and the threshold that turned them into an action
- Good, because thresholds are tuned per action from observed distributions with no new inference
- Good, because low confidence routes to a person instead of guessing
- Bad, because questions must be designed with care: one judgment each, complete criteria, a no-match option
- Bad, because the application cannot summarise or explain; that is out of scope by design

### Confirmation

`src/triage/questions.rs` contains only Choice, Noul and Score questions. `Policy` thresholds are the only place probabilities become actions. The `question-designer` agent reviews new questions against the TypeSafe guidance.

## Pros and cons of the options

### TypeSafe System One primitives

- Good, because outputs are calibrated probabilities with a confidence summary
- Good, because a request carries many independent questions at once (speculative fan-out)
- Bad, because a new vendor and a smaller ecosystem than general LLM providers
- Bad, because non-English alert text has lower accuracy today

### Generative LLM with structured output

- Good, because one provider can also summarise and explain
- Bad, because a JSON field named `confidence` is generated text, not a calibrated probability
- Bad, because outputs vary between runs and prompt versions
- Bad, because parsing failures become a routing failure mode

### Free-text recommendation for a human

- Good, because nothing is automated and nothing can mis-page
- Bad, because it adds reading at the worst moment and removes none of the toil
- Bad, because it cannot suppress noise or attach duplicates

## More information

[Triage](../triage.md), [TypeSafe client](../typesafe-client.md), TypeSafe's [confidence guidance](https://docs.typesafe.ai/confidence).
