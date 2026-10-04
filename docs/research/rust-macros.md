---
title: Rust macros
description: A study of where a declarative or procedural macro would remove repetition in signalman and the judgment crate, with the evidence for each candidate, what the macro would generate and what would stay hand-written, the cost of each, and a ranked recommendation; two small declarative macros are worth adding, one existing macro is worth deleting, and the largest repetition is better solved without a macro.
status: current
last_reviewed: 2026-10-04
tags: [research, rust, maintainability, judgment]
---

# Rust macros

The question was whether parts of this software should use Rust macros. The answer depends on where the code repeats itself structurally rather than incidentally, so the study started with a survey of every repetition in the workspace at commit `724f2ef` (2026-10-02), counted by file and line, before any macro was considered. The survey, the verdict per candidate and the recommendation follow. A candidate is "adopt" only when the generated code is what a reader would have written by hand, the macro's input reads as a table, an error in a use site points at the use site, and no new crate is needed.

## What the code uses today

The workspace leans on other people's macros and defines two of its own.

| Kind | Where | Count |
|---|---|---|
| `#[derive(…)]` from serde, thiserror, schemars, clap | everywhere | 197 derive lines in `src/`, 38 in the crate |
| `#[tracing::instrument]` | client methods and the flow | 15 in `src/`, 2 in the crate |
| rmcp `#[tool]`, `#[tool_router]`, `#[tool_handler]` | `src/mcp.rs` | 6 tools |
| `json!` | tests and fixtures | 56 in `src/`, 231 in `tests/`, 227 in the crate |
| `options!` (declarative, the crate's own) | `src/question.rs` in the judgment crate | defined once; used in six tests and three doc examples; **not used by signalman**, which builds its owner options at run time with `dynamic_choice` |
| `take!` (declarative, local) | `TextsFile::apply` in `src/config.rs` | one use, over 13 fields |

The dependency tree already carries `syn`, `quote` and `proc-macro2` through serde, thiserror, clap, schemars, rmcp and tokio, so a procedural macro of our own would add a crate and a build step, not a new dependency. It would still add the thing proc macros cost most: an expansion the reader cannot see in the source file and that rust-analyzer shows only on request.

## Candidates, ranked

The ranking is by lines of repetition removed per line of macro, weighted by how often the repeated code changes. Evidence is from the survey; file and line numbers are as of the commit above.

### 1. Enum to wire string: eight hand-written tables duplicating `rename_all`

Eight enums carry a serde `rename_all` and, beside it, a hand-written `key()` or `as_str()` that types the same strings again, sometimes with a `FromStr` or an `ALL` list: `Action::key` and `ImpactLevel::key` in `src/outcome.rs`, `Impact` in `src/triage/policy.rs` (which also hand-writes `Serialize` and `Deserialize`, listing its four names three times), `Split` and `Method` in `src/eval/mod.rs`, `StatusCategory::as_str` in `src/incidentio/types.rs`, `McpTransport` in `src/config.rs`. Adding a variant means editing two to four lists, and nothing checks that they agree.

A declarative `wire_enum!` taking `Variant = "string"` pairs would emit the enum, `key()`, `ALL`, `FromStr` and the two serde impls from one list. The input is a table; the expansion is twenty lines anyone could have typed; a wrong string in a use site is an error at that line. The crate's `options!` is the same shape and could be the model. **Adopt.** About 150 lines removed for 40 of macro and one test of the macro. `Action::tag_value` stays by hand: it is a second mapping, not the wire name.

### 2. The `take!` macro and `TextsFile`: a macro to delete

*Done otherwise (2026-10-04): `Texts`, `TextsFile` and `take!` were all deleted when a `.jud` rubric replaced `[triage.text]` ([Triage](../triage.md#the-rubric)). The analysis below is kept as written.*

`Texts` already derives `Deserialize` with `#[serde(default, deny_unknown_fields)]`, so a partial `[triage.text]` table deserializes straight into it with the defaults filled in. `TextsFile` (13 `Option<String>` fields that mirror `Texts`) and the `take!` macro that copies each `Some` onto `Texts::default()` reproduce what serde's container default already does, because the base is always the default. Validation runs on the merged value either way. **Delete both**, deserialize `Texts` in `Settings`, and the 13-field list exists in one place fewer. The `validate` field list stays, since it is the one place that names the fields allowed to be empty. This is the only macro in the workspace whose job a derive already does.

### 3. The judgment crate's three copies

Three repetitions inside the crate are exact enough for a macro and small enough that one would be read in a minute:

- `SystemOne for &T`, `Arc<T>` and `Box<T>` (`backend.rs:90-121`) are byte-identical apart from the wrapper. A `forward_system_one!(&T, Arc<T>, Box<T>)` reduces thirty lines to ten. A blanket `impl<P: Deref> SystemOne for P where P::Target: SystemOne` would do the same without a macro, but it claims every future `Deref` type and would conflict with a concrete impl on one; the macro keeps the three impls explicit.
- `Probability` and `Confidence` (`answer.rs:192-283`) are the same `[0, 1]` newtype twice: `new` with the range check, `value`, `at_least`, `TryFrom<f64>`, `From<_> for f64`, `Display`. signalman's `Unit` (`src/outcome.rs:119-162`) is a third copy with its own error. A `unit_interval!(Name, ErrorVariant)` keeps three distinct types, which decision 0003 wants, from one definition.
- The kind strings `noul`, `choice`, `score` appear in seven places (`Question::kind`, `Answer::kind`, `KNOWN_KINDS`, four `FromAnswer::KIND` constants, three `rename_all` attributes). This is candidate 1 again, inside the crate.

**Adopt the first two** when the crate is next touched; they are internal and change no public API. The kind strings follow candidate 1.

### 4. The configuration table: the biggest repetition, and not a macro's

`src/config.rs` holds 31 environment-layered settings and 20 file-only ones. Each env setting appears in four places in the file: the `ENV_VARS` row, the `*File` field, the effective field and the `resolve_from` expression, which types the variable name a second time (the table is documentation; `resolve_from` does not read it) and, for the 19 parsed settings, the file key a second time. Across the repository one setting lives in eleven places in nine files (`flow.related_window_minutes`: config.rs five times, main.rs twice, three tests, the configuration page, the example file, `.env.example`, the operations page, an MCP doc comment). That is the "seven places" problem `config-reviewer` exists for, and it is real.

A table macro with one row per setting could emit the `ENV_VARS` table, both struct families and the resolve chain, and it is the obvious proposal. **Do not adopt it.** Six settings break the pattern (`addr` with its unreachable default, `app_url` falling back to `base_url`, `otlp_endpoint` trimmed and filtered, three with no default), and three sections carry range checks after resolution. A macro that handles 25 of 31 settings and leaves six outside makes the file harder to read than it is now, and a type error inside a 300-line expansion is the worst diagnostic in this study. The repetition that matters, the variable name and the file key typed twice, has a plain fix: make `resolve_from` read the `ENV_VARS` row, through a small `Setting { var, key, default }` value, so the table becomes the source rather than the documentation. The eleven places outside `config.rs` are docs and tests; a test already checks the env-var table against the configuration page, and the same test could check the example file. The decision-0006 layering stays explicit in code, which is what a reviewer reads.

### 5. The five questions: structural, and deliberately explicit

Each of the five questions is written out in the handle struct, the builder, `read()`, the answers struct, three places in `src/outcome.rs` and three in `src/eval/mod.rs`; `"owner"` appears 15 times in `questions.rs` alone. A `questions!` macro could generate the handle struct, the answers struct and `read()` from `(id, primitive, optional)` rows. **Do not adopt.** The part a macro would generate is about forty lines; the part that varies (each question's instructions, its options, its asked-when condition) is the whole content, and it would sit inside the macro's input or outside it with the ids repeated anyway. The explicit handles are the point of decision 0003 and the first thing a reader of `questions.rs` learns from. The cheap improvement is `const` ids (`OWNER: &str = "owner"`) shared by `questions.rs`, `policy.rs`, `outcome.rs` and `eval`, which removes the string repetition without hiding anything.

### 6. Telemetry instruments

Seven instruments, six built in `Metrics::new` with the same builder chain and one observable gauge. Each name appears in the field doc, the builder literal, the observability page and, for five of them, a test. A `(field, kind, name, unit, description)` table macro would emit the struct and `new()`. **Not worth it**: forty lines for seven rows that change once a quarter. What is missing is a test that the observability page's instrument table matches the names in code, as the configuration page has; that is a test, not a macro.

### 7. MCP tools

The five read-only tools repeat the same `annotations(…)` block, `tool_failed("<name>")` retypes the tool name eight times, and the router merge list is written twice. A `macro_rules!` wrapper emitting `#[tool]` with the shared annotations is possible. **Do not adopt**: rmcp's attribute is the contract readers know, and the duplication that costs something is elsewhere in the same file. `qualify_inline` and `enrich_inline` repeat the CLI's `triage()` steps, `resolve_incident_refs` is a copy of `main.rs:623-646`, and the `ComponentContext`/`Change`/`RelatedAlert` conversions exist in both `outcome.rs` and `mcp.rs`. The repository rule is that every MCP tool wraps a function the CLI already calls; those copies are function extractions owed to that rule, and a macro would not do them.

### 8. Tests and fixtures

`Mock::given` appears 166 times across the two test trees, with 38 path-less chains in the crate's `client.rs` alone, and the five-question answer map is hand-built in nine places. A `mock!(server, POST "/v1/systemone" => 200 json, expect 1)` macro would shorten the chains. **Do not adopt**: a function `mount(&server, method, path, status, body)` does the same with type checking and go-to-definition, and `json!` is already the macro at work in every body. The crate's `tests/` has no `common` module while signalman's has one of 297 lines; adding one (the client builder is defined three times, `one_noul()` twice, the same body four times) is the improvement, and the two hand-written write-mock scenes in `webhook_server.rs` and `mcp_server.rs` belong in signalman's `common`.

### 9. Wire types

`#[serde(default)]` is written per field 92 times across the incident.io, Backstage, GitLab and Argo CD wire types. Where the struct derives `Default`, one container-level `#[serde(default)]` replaces them. No macro; the attribute exists.

### 10. A derive for `options!`

A `#[derive(Options)]` procedural macro would let a consumer write a plain enum with attributes instead of the `options!` syntax. **Do not adopt.** `options!` works, its only users are tests and examples, signalman's options are data at run time, and a proc-macro crate would be a fourth workspace member maintained for a syntax nobody has asked for.

## Recommendation

In order, each its own bead and pull request:

1. `wire_enum!` in signalman for the eight enums, modelled on `options!`, with one test of the macro (`signalman-1zs.1`).
2. Delete `TextsFile` and `take!`; deserialize `Texts` directly (`signalman-1zs.2`).
3. In the judgment crate, `forward_system_one!` and `unit_interval!`, internal only, with the next crate change (`signalman-1zs.3`).
4. `config.rs`: `resolve_from` reads `ENV_VARS`; a `Setting` value per row; extend the configuration test to the example file. No macro (`signalman-1zs.4`).
5. Shared question ids as constants, a `tests/common` for the crate, and the MCP copies extracted into the functions the CLI calls. None of these is a macro; all are cheaper than one.

What this study does not recommend is a general move toward macros. The workspace's repetition is mostly in configuration, fixtures and conversions, where explicit code is what reviewers and the harness's own agents read; the two places where a macro is right are the two where the input is a table and the expansion is obvious.

## Sources

- The survey of commit `724f2ef`, 2026-10-02, by file and line: `src/config.rs`, `src/triage/questions.rs`, `src/telemetry.rs`, `src/mcp.rs`, `src/outcome.rs`, `src/eval/mod.rs`, `src/incidentio/types.rs`, `src/changes/`, `tests/`, and, in the judgment crate (then `crates/judgment/`), `src/{question,answer,backend,error}.rs` and `tests/`.
- [The Rust Reference on macros by example](https://doc.rust-lang.org/reference/macros-by-example.html) and [procedural macros](https://doc.rust-lang.org/reference/procedural-macros.html), for what each kind can and cannot see.
- [Decision 0003](https://github.com/chussenot/judgment/blob/main/docs/decisions/0003-typed-handles-between-questions-and-answers.md) and [decision 0006](../decisions/0006-layered-configuration.md), which fix what must stay explicit.
