---
title: 0002 Calibrated judgments over generated text
description: Use TypeSafe's System One primitives, which return typed answers with probabilities, instead of a generative model with prompt-and-parse output.
status: accepted
date: 2026-09-20
last_reviewed: 2026-09-20
tags: [decisions, typesafe]
---

# 0002 Calibrated judgments over generated text

## Context

Routing decisions need semantic understanding of alert text, which rule engines lack. A generative model can provide it, but its output is free text: it must be parsed, it varies between runs, it carries no probability, and the reasoning that would justify paging someone is buried in prose.

## Decision

Ask TypeSafe's System One model narrow, typed questions (Choice, Noul, Score) and consume the returned probabilities and confidence in code. Do not ask any model to generate text.

## Consequences

- Every decision is traceable to specific probabilities and a threshold in `Policy`, both inspectable after the fact.
- Thresholds are tuned per action from observed distributions; changing one does not require re-running inference.
- The model's uncertainty is a first-class input. Low confidence routes to a person instead of guessing.
- Questions must be designed carefully: one judgment each, complete criteria, a no-match option. This is engineering work that a prompt would have hidden.
- The application cannot summarise or explain. That is out of scope by design.
