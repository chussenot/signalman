---
title: 0012 The judgment crate moves to its own repository
description: crates/judgment leaves the signalman workspace for a repository of its own, chussenot/judgment, through a history-preserving split; signalman then depends on it like any other consumer, and the directory carries its own repository scaffolding in advance so the split builds on its first commit.
status: accepted
date: 2026-10-03
decision-makers: [platform engineering]
consulted: []
informed: []
last_reviewed: 2026-10-03
tags: [decisions, judgment, architecture, repository]
---

# 0012 The judgment crate moves to its own repository

## Context and problem statement

[Decision 0010](0010-extract-the-judgment-core-into-a-crate.md) made the typed TypeSafe client its own crate, `crates/judgment`, "a workspace member of this repository first and a published crate when a second consumer exists". [Decision 0011](0011-documentation-lives-with-its-concern.md) gave it its own documentation set. The crate is now the half of the repository that other projects could use, with its own verification records, examples and changelog, while every commit to it still lands in an alert triager's history and runs the triager's CI. The owner wants it in a repository of its own, `chussenot/judgment`, and wants the history of `crates/judgment` to come with it. How does the crate leave, and what has to be true before it does so that the new repository works on its first commit?

## Decision drivers

- A release of the crate should not wait for, or be entangled with, a change to signalman; its CI should run its own gates and nothing else.
- The history matters: the verification records, the changelog and `git blame` refer to commits that must stay reachable, and the crate's files existed under `src/` for four days before the directory did.
- signalman must keep building, with no gap, and keep its contract test against the document the crate vendors.
- Decision 0011 already made the two documentation sets independent; the split should not reopen that work.
- The split will be done by another session with no memory of this one, so the preparation must be in the tree and in a runbook, not in a conversation.

## Considered options

1. Stay a workspace member and publish to crates.io from here, as 0010 planned.
2. A repository of its own, created by a history-preserving split of `crates/judgment`, with the crate directory carrying its repository files in advance; signalman depends on it by git, then by version.
3. Keep the code here and expose it as a git subtree or submodule from the new repository.

## Decision outcome

Chosen option: 2, a repository of its own by a history-preserving split, because it is the only option that gives the crate independent releases and CI without a second copy of the code to keep in step.

Three things are done in this repository before the split, so the new repository passes its gates on its first commit and the split needs no judgment calls:

- The crate's manifest declares its own edition, Rust version, licence, dependency versions and lints instead of inheriting the workspace's. It still builds here as a member; it also builds from a checkout of its directory alone.
- The directory carries the files a repository root needs (`.github/workflows/ci.yml`, `mise.toml`, `.pre-commit-config.yaml`, `scripts/`, `.claude/`, `CLAUDE.md`, `catalog-info.yaml`, `LICENSE`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `.gitignore`), each inert or harmless while the directory sits inside signalman (prek runs the nested hook configuration as a workspace of its own, which exercises it early) and each the new repository's on the split, with history. Where a file is a copy of signalman's (the two docs scripts, the hooks), the copy is the price of the files travelling with the crate; the docs generator derives its links from the site's place in the git repository, so the copies stay byte-identical.
- The one place signalman reached into the crate's tree, its contract test reading the vendored OpenAPI document by path, now reads it through the crate (`judgment::contract::OPENAPI_DOCUMENT`, feature `openapi`), so the dependency can become a git or registry dependency without a copy of the document.

The split itself is `git filter-repo` keeping `crates/judgment/` and the six files the crate's sources were renamed from (`src/answer.rs`, `src/client.rs`, `src/error.rs`, `src/http.rs`, `src/question.rs`, `tests/client.rs`), with the directory prefix stripped: the pre-move paths are the post-move paths relative to the crate, so one pass yields a history that `git log --follow` reads back to the first commit of 2026-09-20. [Extracting the judgment crate](../judgment-extraction.md) is the runbook, with the command as rehearsed and the edits each repository makes afterwards.

Decision records keep one number sequence across the two repositories: a number is an identifier, 0003 is cited from both sets, and renumbering would break every citation. A new record takes the next free number in either repository, at the cost of one check in the other.

### Consequences

- Good, because a crate change is one pull request in one repository, with the crate's gates and nothing else, and a release is a tag there.
- Good, because the history, the pre-move days included, travels with the code; nothing in the verification records or the changelog points at a commit the new repository cannot show.
- Good, because signalman becomes an ordinary consumer, which is the test 0010 wanted a second consumer for: anything the crate still needs from signalman's tree shows up as a build failure there.
- Bad, because the lint set and the dependency versions are no longer shared by inheritance; the two manifests are kept in step by hand until the split, and may drift after it, which is the point.
- Bad, because signalman pins a tag or revision of the crate and bumps it deliberately; a change that needs both sides lands in two pull requests, the crate's first.
- Bad, because signalman's absolute links into `crates/judgment/` (its index, its decisions index, the development guide) must be repointed at the new repository after the split, and nothing but a link check finds a stale one.
- Bad, because the records about the crate's design are split across two repositories by number, and a reader of either index must follow a link to see the whole sequence.

### Confirmation

Before the split: `crates/judgment` copied alone into an empty directory passes its own `mise run check` (fmt, clippy pedantic, the no-`http` build, tests, rustdoc, docs), and the workspace's `mise run check` still passes with the crate as a member. After it: the new repository's CI is green on its first commit, and signalman's CI is green with the crate as a git dependency and `crates/judgment` removed. The runbook lists both checks.

## Pros and cons of the options

### Stay a workspace member and publish from here

- Good, because nothing moves and the inherited manifest keeps the two crates on one bar.
- Bad, because every crate release rides signalman's history and CI, and a contributor to the crate gets an alert triager's repository.
- Bad, because the crate's own files (CI, hooks, agents) would have to coexist with signalman's at one root or not exist.

### A repository of its own by a history-preserving split

- Good, for the reasons above.
- Bad, because the preparation is a one-time cost paid here, and the cross-repository links and the dependency pin are a running cost.

### A subtree or submodule from the new repository

- Good, because one copy of the code stays here.
- Bad, because the new repository would be a view, not a home: its CI would still run against signalman's tree, and a submodule pins signalman's consumers to a commit of a repository they do not own.

## More information

[Decision 0010](0010-extract-the-judgment-core-into-a-crate.md), which this completes; [Decision 0011](0011-documentation-lives-with-its-concern.md), whose two sets make the split clean; [Extracting the judgment crate](../judgment-extraction.md), the runbook; the crate's own instructions, [crates/judgment/CLAUDE.md](https://github.com/chussenot/signalman/blob/main/crates/judgment/CLAUDE.md), which say which of its files are inert until the split.
