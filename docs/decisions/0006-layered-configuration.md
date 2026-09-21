---
title: 0006 Layered configuration with secrets outside the file
description: One configuration model resolved in a fixed order (default, TOML file, environment, flag), owned by a single module; secrets are refused from the file; question wording, thresholds and the fallback team list become data while the question set stays code.
status: accepted
date: 2026-09-20
decision-makers: [platform engineering]
consulted: [on-call leads]
informed: []
last_reviewed: 2026-09-20
tags: [decisions, configuration, kubernetes]
---

# 0006 Layered configuration with secrets outside the file

## Context and problem statement

Until now signalman was configured by environment variables and a handful of flags, read in six modules. That is enough for one laptop. In Kubernetes the shape of a deployment (URLs, thresholds, rubric wording, the fallback team list) belongs in a `ConfigMap` that goes through review, secrets belong in a `Secret`, an operator needs to know which value is in effect and where it came from, and a typo must fail at start-up rather than silently keep a default. Which configuration model gives that, and how much of what is compiled today should become configuration?

## Decision drivers

- One stated precedence that the code follows literally, so the documentation cannot drift from behaviour
- Secrets must not be persuadable into a file that is committed, mounted or printed
- A `ConfigMap` change should be validated before rollout and should roll the pods
- The policy consumes a fixed set of typed answers (decision 0003); configuration must not be able to break that contract
- Few dependencies, no framework the rest of the code has to fit

## Considered options

1. Environment variables only, as today, documented better
2. A configuration crate with layered providers (`figment`, `config`) merging file, env and flags generically
3. A hand-written layer: a serde schema for the file, one module that reads the environment, clap flags as optional overrides, an explicit resolve function

And, separately, for what becomes configuration:

- a. Nothing: thresholds and text stay compiled
- b. Text, thresholds and the fallback team list as data; the question set and primitive types stay code
- c. A data-driven question set: questions, types and a policy expression language in the file

## Decision outcome

Chosen: option 3 with scope b.

Precedence, lowest to highest: built-in default, configuration file, environment variable, command-line flag. The file sits below the environment because the file is the reviewed, shared shape of a deployment and the environment is how one deployment or one operator deviates from it without editing that shape; a flag is a one-off for the person at the keyboard. The file format is TOML: the crate is mature, the repository already uses it (`Cargo.toml`, `mise.toml`), it allows comments, and it avoids the implicit typing of YAML. A `ConfigMap` holds it as a string.

`src/config.rs` is the only module that reads a non-secret environment variable. The file schema rejects unknown keys, so `api_key = …` in the file is an error, not a silent secret. `signalman config show` prints the effective values and names the file and variables that contributed, and exits non-zero on an invalid file, so a CI job can validate the `ConfigMap`.

Question wording (`[triage.text]`), the impact rubric, the routing thresholds (`[policy]`) and the fallback team list (`[[triage.teams]]`) are file configuration. They are what a team tunes to its own alert sources and vocabulary, and none of them changes what the code does with an answer. The set of questions and their primitives stay compiled: `decide` reads `owner`, `impact`, `actionable`, `duplicate_of` and `caused_by_change` through typed handles, and `page_at` compares against the `Impact` enum, so the level count is fixed at four and validated.

### Consequences

- Good, because the precedence table in the documentation is a description of one function
- Good, because a deployment can retune thresholds or reword a question with a `ConfigMap` change and a rollout, no build
- Good, because secrets cannot be committed by way of the configuration file
- Bad, because every new setting must be added in three places (schema, resolve, docs table); a test enforces that the table of environment variables matches
- Bad, because the file cannot add a question; that remains a code change with a policy change, by design

### Confirmation

- `src/config.rs` tests: layer precedence, unknown key rejection, invalid range messages, TOML round trip of the effective configuration
- `grep -rn "env::var" src` returns only `config.rs` and the four secret reads (API keys, token, webhook secret)
- `tests/config_precedence.rs` exercises the environment layer in a subprocess

## Pros and cons of the options

### Environment variables only

- Good, because nothing to build
- Bad, because rubric text and a team list do not fit in variables, and thresholds across six variables are hard to review as one change
- Bad, because there is no place to validate a deployment's shape before it runs

### A configuration crate with layered providers

- Good, because merging is written once, generically
- Bad, because generic environment mapping (`SIGNALMAN__POLICY__SUPPRESS_BELOW`) would rename every existing variable or need a translation table anyway
- Bad, because it adds a framework and its error vocabulary between the operator and the mistake
- Neutral, because the win over a hand-written resolve is small when the schema is this size

### A hand-written layer

- Good, because the resolve function is the precedence and reads top to bottom
- Good, because existing variable names stay and the error names the variable, the value and the key it maps to
- Bad, because it is code to maintain per setting

### Scope c: a data-driven question set

- Good, because a new judgment would need no release
- Bad, because the policy is code that reads specific typed answers; a configurable set needs a policy language, which is a second product
- Bad, because the typed handles (decision 0003) exist to catch exactly the mismatch a data-driven set would reintroduce
