---
title: Backstage bridge
description: How signalman uses the Backstage software catalog for ownership, TechDocs for runbooks and the Notifications plugin for handoff, and how to register signalman itself.
status: current
last_reviewed: 2026-09-20
tags: [backstage, catalog, techdocs, notifications]
---

# Backstage bridge

Without a catalog, signalman routes alerts to a team list compiled into the binary. That list is one organisation's snapshot of who owned what. The Backstage software catalog already holds the current answer, and it holds more than ownership: what a component depends on, what depends on it, its lifecycle, its runbook. The bridge turns that referential into triage inputs and turns triage outputs into a message the owning group sees in the portal.

Contract sources: the [catalog REST API](https://backstage.io/docs/features/software-catalog/software-catalog-api), the [descriptor format](https://backstage.io/docs/features/software-catalog/descriptor-format), [service-to-service auth](https://backstage.io/docs/auth/service-to-service-auth) and the [Notifications plugin](https://backstage.io/docs/notifications/).

## What the catalog contributes

| Catalog fact | Where it goes | Why |
|---|---|---|
| The component matched by the alert's service name | `alert.component` in the state | The model reasons about the real service, not a label string |
| `spec.owner`, via the `ownedBy` relation | `alert.component.owner`, and the first owner candidate | The registered owner is a fact; the model only decides whether this alert is an exception |
| `dependsOn`, `dependencyOf`, `consumesApi`, `providesApi` relations | `alert.component.depends_on` and `dependents`; the owners of those entities become further candidates | Alerts often fire on the symptom, not the cause; the neighbours are where the cause and the blast radius live |
| Groups owning components named in the alert text | further owner candidates | An alert on `checkout-api` that mentions `payments-gateway` may belong to the gateway's team |
| `spec.lifecycle`, `spec.type`, `spec.system`, tags, link titles | `alert.component` | Impact and actionability differ for an experimental service and a production one |
| TechDocs search index | `alert.runbook` | The runbook page that best matches the alert, as plain text, bounded |

If no component matches, every group of `spec.type: team` becomes a candidate, capped, so the owner question still has a real option set instead of the static list.

## Flow

1. Hints come from the alert. For incident.io alerts these are attribute values whose attribute name is in `SIGNALMAN_COMPONENT_KEYS` (default `component, service, app, application`, case-insensitive); for CLI alerts, the labels with those keys. A hint may be a bare name, `kind:namespace/name`, or a title.
2. `GET /api/catalog/entities/by-name/component/{namespace}/{name}` resolves a bare name (the configured namespace first, then `default`); a title falls back to `by-query` with `metadata.title=`.
3. `POST /api/catalog/entities/by-refs` fetches the neighbours from the component's relations, then the owner groups of the component, its neighbours and any components mentioned in the alert text. Group descriptions become the option rubric: display name, description, and up to six owned components.
4. If the component declares TechDocs (`backstage.io/techdocs-ref`, or `backstage.io/techdocs-entity` pointing elsewhere), `GET /api/techdocs/static/docs/{ns}/component/{name}/search/search_index.json` is read and the best-matching page is excerpted. Pages under a `runbook` path win; then title overlap with the alert. Nothing is done if the site is not built.
5. The owner question is a dynamic Choice over the candidate keys plus `none_of_these`; its instructions tell the model that `alert.component.owner` is the registered owner and when to deviate.
6. After a Page, Ticket or HumanTriage decision, and only when `--notify-owners` (or `BACKSTAGE_NOTIFY=true`) is set, `POST /api/notifications/notifications` addresses the chosen group with the decision, severity mapped from impact, and a link to the alert. Suppressions and attachments send nothing. A failed notification is logged and does not undo the incident.io tags.

The catalog supplies candidates and context. Deciding still happens in the model for the semantic part and in `Policy` for the thresholds. See [decision 0004](decisions/0004-catalog-is-the-ownership-source-of-truth.md).

## Setup

1. Create a static external-access token in `app-config.yaml`:

   ```yaml
   backend:
     auth:
       externalAccess:
         - type: static
           options:
             token: ${SIGNALMAN_BACKSTAGE_TOKEN}
             subject: signalman
           accessRestrictions:
             - plugin: catalog
             - plugin: techdocs
             - plugin: notifications
   ```

2. Set `BACKSTAGE_BASE_URL` to the backend URL without `/api` and `BACKSTAGE_TOKEN` to the token. `serve` enables enrichment when the URL is set; the CLI needs `--enrich-from-backstage`.
3. Check with `signalman backstage lookup checkout-api --text "HighErrorRate 5xx"`: it prints the resolved component, the owner candidates as the model will see them, and the runbook excerpt.
4. Make sure alerts carry the component identity. In incident.io, an alert attribute named `Service` or `Component` whose value is the catalog name is enough; the [incident.io catalog importer](https://github.com/incident-io/catalog-importer) can sync Backstage components into incident.io so that attribute is a real catalog entry.

## Registering signalman

`catalog-info.yaml` at the repository root declares signalman as a `Component` of type `service` with TechDocs built from `docs/` (`mkdocs.yml`), plus two `Resource` entities for incident.io and the TypeSafe API that it `dependsOn`. Register the file's URL in the catalog, or let the GitHub discovery provider find it. Replace the placeholder owner `group:default/platform-engineering` with the group that runs it.

Every page under `docs/` starts with YAML frontmatter; mkdocs treats it as page metadata and renders the body.

## Community plugins worth knowing

The community plugin workspaces that touch the same problem space at the time of writing: `firehydrant`, `ilert` and `healert` for incident tooling, `grafana`, `sentry`, `newrelic`, `dynatrace` and `splunk` for signals, `github`, `argocd`, `flux` and `octopus-deploy` for deployments, and `announcements`. None is required by the bridge. The deployment plugins are the natural source for `recent_changes` (roadmap), and the incident.io Backstage plugin (`@incident-io/backstage`) shows incident.io incidents on a component page through a custom field mapped to the Backstage component catalog type, which pairs well with the attributes signalman tags.

## Limits and unverified points

- Not yet run against a real Backstage instance. Request and response shapes follow the published OpenAPI specification and TypeScript types; drift shows up as a `Decode` error.
- Owner candidates are capped at 24 per triage. Each is a Choice option and costs input tokens.
- Components mentioned in the alert text are looked up by exact word match against `metadata.name` in the configured namespace only.
- The TechDocs excerpt is at most 1,800 characters of the single best page. Multi-page runbooks are not stitched.
- Notifications need the Notifications backend installed and the token allowed on the `notifications` plugin.
