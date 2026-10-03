---
title: Decisions
description: The architecture decision records that govern the judgment crate's design, how their numbers relate to signalman's records, and the records of signalman's that shaped the crate.
status: current
last_reviewed: 2026-10-03
tags: [judgment, decisions, adr, madr]
---

# Decisions

A record captures one architecturally significant decision: the problem, the options weighed, the choice and what it costs, in [MADR](https://adr.github.io/madr/) form. The records here govern the crate's own design.

Record numbers are shared with [signalman's records](https://github.com/chussenot/signalman/blob/main/docs/decisions/README.md): there is one sequence for the repository, and a record lives with the code it governs. A number is an identifier, never reused or renumbered, so `decision 0003` means the same record wherever it is cited.

| Id | Title | Status |
|---|---|---|
| [0003](0003-typed-handles-between-questions-and-answers.md) | Typed handles between questions and answers | accepted |

Signalman's records that shaped the crate, read with these:

- [0002 Calibrated judgments over generated text](https://github.com/chussenot/signalman/blob/main/docs/decisions/0002-calibrated-judgments-over-generated-text.md): why System One primitives at all.
- [0010 Extract the judgment core into a reusable crate](https://github.com/chussenot/signalman/blob/main/docs/decisions/0010-extract-the-judgment-core-into-a-crate.md): what became the crate, what stayed in the application, and why the existing crates were not adopted.
- [0011 Documentation lives with its concern](https://github.com/chussenot/signalman/blob/main/docs/decisions/0011-documentation-lives-with-its-concern.md): why these pages are here and not under signalman's `docs/`.
- [0012 The judgment crate moves to its own repository](https://github.com/chussenot/signalman/blob/main/docs/decisions/0012-the-judgment-crate-moves-to-its-own-repository.md): why this directory carries a repository's files, and how it leaves the workspace with its history.

## Status notes

Records are not edited after acceptance; a fact that has moved since is noted here.

- 0003 was written in signalman's `docs/decisions/` before the crate existed, and moved here with its number on 2026-10-03 (decision 0011). Its "More information" link was the one line changed: the page it pointed to was split, and the link now names both halves.

To add a record, follow [signalman's procedure](https://github.com/chussenot/signalman/blob/main/docs/decisions/README.md#writing-a-record) with the next number of the shared sequence, and put the record here when it governs the crate.
