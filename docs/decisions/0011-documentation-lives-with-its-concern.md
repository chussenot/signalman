---
title: 0011 Documentation lives with its concern
description: The judgment crate's documentation moves from signalman's docs/ into crates/judgment/docs with its own nav and llms.txt, signalman's stays in docs/, and a page's place is decided by whether it would still be true without the application.
status: accepted
date: 2026-10-03
decision-makers: [platform engineering]
consulted: []
informed: []
last_reviewed: 2026-10-03
tags: [decisions, documentation, judgment, architecture]
---

# 0011 Documentation lives with its concern

## Context and problem statement

[Decision 0010](0010-extract-the-judgment-core-into-a-crate.md) made the typed TypeSafe client its own crate and gave it two documents: its README and its rustdoc. Everything else written about the crate after that landed in signalman's `docs/`: two verification records, a survey of the other clients, the design diagrams and the decision behind typed handles. signalman's index then answered questions about a library next to questions about an alert triager. The crate, meant to be published and used without signalman, had no documentation of its own beyond the README. Where should a page about the crate live, and how does a writer, human or agent, decide?

## Decision drivers

- The crate is published apart from the application (decision 0010), so its documentation must make sense, and resolve its links, without signalman.
- A reader of either set should find only that set's concern, and be pointed at the other.
- Both sets must stay under the same gates: frontmatter, a nav that lists every page, and a generated `llms.txt` that cannot go stale.
- The rule must be one an agent applies without judgment calls that drift.

## Considered options

1. Keep one documentation set under `docs/`, with a "judgment" section in its nav.
2. Put the crate's documentation in `crates/judgment/docs/`, with its own `mkdocs.yml` and `llms.txt`; signalman's stays in `docs/`.
3. Move the crate's pages into its rustdoc as module documentation.

## Decision outcome

Chosen option: 2, a documentation set per concern, because it is the only one in which the crate's pages travel with the crate and still sit under the same gates.

The rule a writer applies: **a page belongs to the crate when it would still be true, and still be needed, if signalman did not exist.** How the crate works, what it guarantees, what real servers did with it, why its API is shaped as it is: the crate's. How signalman configures, observes and builds on the crate, its triage questions, its evaluation harness, its choice of model provider: signalman's. A page that informs both (the open-model landscape, the workspace's use of macros) stays with signalman, which acts on it, and names what it means for the crate in a section of its own.

Decision records keep one numbering sequence for the repository and live with the code they govern. 0003 governs the crate's API and moved there with its number; signalman's index keeps the row and points to it.

Links inside a set are relative. Links across sets are absolute GitHub URLs, the only form that resolves on GitHub, in a TechDocs build and in a packaged crate alike. Paths written in the crate's sources and pages are relative to the crate.

### Consequences

- Good, because the crate's documentation is complete without signalman: its README, an index, how it works, its verification records, its research and its decisions, in the directory that is packaged with it.
- Good, because each set has one subject, and each index points at the other once.
- Good, because the same generator and checks cover both sets: `scripts/gen-llms-txt.sh` writes an `llms.txt` per set, and `mise run docs:check` checks both.
- Bad, because a cross-set link is an absolute URL to the default branch, so it points at what is merged, not at the branch being reviewed, and a renamed page breaks it silently until the docs auditor's link check finds it.
- Bad, because a writer must decide where a page goes. The rule above is meant to make that a question with an answer, and the docs agents carry it.

### Confirmation

`scripts/gen-llms-txt.sh --check` fails when a page under either documentation directory is missing from its nav. The `docs-writer` and `docs-auditor` agents state the placement rule, and the auditor reports a page in the wrong set as a finding. Both indexes, `docs/index.md` and `crates/judgment/docs/index.md`, say what the other set holds.

## Pros and cons of the options

### One set with a judgment section

- Good, because there is one nav, one TechDocs site and one `llms.txt`.
- Bad, because the crate's pages would not travel with the crate, and their links would point into an application a crate user does not have.
- Bad, because each index mixes two subjects, and nothing in the structure tells a writer which one a page is about.

### A set per concern

- Good, for the reasons above.
- Bad, because cross-set links are absolute and checked by audit rather than by the build.

### The crate's pages as rustdoc

- Good, because rustdoc is versioned with the code and published on docs.rs.
- Bad, because rustdoc renders no Mermaid diagram, and verification records with benchmark tables are not API reference; they would bury the reference they sit in.
- Bad, because rustdoc is outside the frontmatter and `llms.txt` gates.

## More information

[Decision 0010](0010-extract-the-judgment-core-into-a-crate.md), which this extends; the crate's documentation index, [crates/judgment/docs/index.md](https://github.com/chussenot/signalman/blob/main/crates/judgment/docs/index.md); [Development](../development.md#documentation), which says how both sets are built and checked.
