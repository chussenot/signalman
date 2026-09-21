---
title: Change feed
description: How recent deploys, configuration changes and flag flips reach the triage as alert.recent_changes through a push endpoint any delivery tool can call, the native Argo CD and GitLab adapters, how the window and matching work, and the limits.
status: current
last_reviewed: 2026-09-21
tags: [changes, triage, argocd, flux, github-actions, mttq]
---

# Change feed

"What changed?" is the first question a responder asks, and `caused_by_change` is the judgment signalman has for it. Until the feed existed the question was never asked in production: it is only posed when `alert.recent_changes` is non-empty, and nothing filled that list in the webhook flow.

signalman does not poll delivery tools. It accepts change events on `POST /changes` from whatever performed the change, keeps a bounded in-memory window per replica, and at triage time offers the changes that touched the alerting component, or the whole platform, within the last two hours ([decision 0007](decisions/0007-changes-are-pushed-not-polled.md)).

```mermaid
flowchart LR
    AR[Argo CD notification<br/>Application object] -- POST /changes/argocd<br/>bearer token --> LOG[(ChangeLog<br/>per replica, 1000 newest)]
    GL[GitLab webhook<br/>deployment, flag, release, merge] -- POST /changes/gitlab<br/>X-Gitlab-Token --> LOG
    CD[GitHub Actions step<br/>any script] -- POST /changes<br/>bearer token --> LOG
    IO[incident.io alert] --> F[triage flow]
    F -- component hints<br/>+ window --> LOG
    LOG -- state lines --> S[alert.recent_changes]
    S --> Q[caused_by_change asked]
    Q --> N[note lists the changes<br/>tag ai-suspected-change]
```

## Posting a change

```sh
curl -X POST https://signalman.example.com/changes \
  -H "Authorization: Bearer $SIGNALMAN_CHANGES_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"at": "2026-09-21T11:50:00Z", "kind": "deploy", "component": "checkout-api",
       "summary": "checkout-api v2.31.0 by alice", "source": "argocd",
       "url": "https://argocd.example.com/applications/checkout-api"}'
```

One object or an array. The endpoint answers `202` with `{"recorded": n, "held": total}`, `401` on a missing or wrong token, `400` on a body that does not parse or lacks `kind` or `summary`, and is not routed at all when `SIGNALMAN_CHANGES_TOKEN` is unset. `GET /changes` with the same token lists what the replica holds.

| Field | Required | Meaning |
|---|---|---|
| `kind` | yes | `deploy`, `config`, `flag`, `infra`, `migration`, or any short word |
| `summary` | yes | one line: what, version, who |
| `component` | no | the component as the alert labels name it (`service`, `component`, `app` attributes, or the catalog name). Absent means platform-wide: offered to every alert |
| `at` | no | RFC 3339; receipt time when absent or unparsable |
| `source` | no | the tool that posted it |
| `url` | no | link to the deploy, pull request or commit |

The model reads each change as one line, time first: `2026-09-21T11:50:00Z deploy checkout-api v2.31.0 by alice (checkout-api) https://…`.

## Matching and window

At triage time the hints are the alert's component labels ([`backstage.component_keys`](configuration.md#backstage)) plus the catalog component name when Backstage resolved one. A change is offered when its `component` matches a hint case-insensitively, or when it has no component. Newest first, at most `flow.change_max` (10), within `flow.change_window_minutes` (120, `0` disables). An alert that already carries `recent_changes` from its source keeps them; the feed fills only an empty list.

The window is two hours by default, wider than the related-alert window, because a deploy causes an incident hours later more often than minutes later: caches expire, a batch job runs, traffic peaks.

## Native adapters: Argo CD and GitLab

The generic endpoint takes signalman's own shape and asks every pipeline to write a template. Two adapters take the tools' own payloads instead, authenticated with the same token, and translate them into changes. Both refuse deliveries that are not a change that happened (a failed sync, a running deployment, an updated but unmerged merge request) with a `200` and `{"recorded": 0, "ignored": "…"}`, so the sender does not retry.

### Argo CD, `POST /changes/argocd`

Argo CD Notifications sends the `Application` object; one line of template. The change carries the application name as component, the synced revision, the destination namespace, the images, and who initiated the sync from the last `status.history` entry. Posting `{"app": …, "context": …}` adds a link to the application in the Argo CD UI.

```yaml
# argocd-notifications-cm
service.webhook.signalman: |
  url: https://signalman.example.com/changes/argocd
  headers:
    - name: Authorization
      value: Bearer $signalman-changes-token      # key in argocd-notifications-secret
    - name: Content-Type
      value: application/json
template.signalman-deployed: |
  webhook:
    signalman:
      method: POST
      body: |
        {"app": {{ toJson .app }}, "context": {{ toJson .context }}}
trigger.on-deployed: |
  - when: app.status.operationState.phase in ['Succeeded'] and app.status.health.status == 'Healthy'
    oncePer: app.status.operationState.syncResult.revision
    send: [signalman-deployed]
```

Subscribe applications with the annotation `notifications.argoproj.io/subscribe.on-deployed.signalman: ""`, or the default subscription in the ConfigMap. Only the fields signalman reads are modelled (`metadata.name`, `spec.destination.namespace`, `spec.project`, `status.history`, `status.operationState`, `status.sync.revision`, `status.summary.images`); the rest of the Application is ignored, so Argo CD upgrades do not break the contract. A body whose `operationState.phase` is not `Succeeded` is ignored. [`examples/changes/argocd-application.json`](https://github.com/chussenot/rustsafe/blob/main/examples/changes/argocd-application.json) is the fixture the tests use.

### GitLab, `POST /changes/gitlab`

A project or group webhook. GitLab authenticates with `X-Gitlab-Token`, set to the same value as `SIGNALMAN_CHANGES_TOKEN`, and names the event in `X-Gitlab-Event`. Settings → Webhooks → Add new webhook: URL `https://signalman.example.com/changes/gitlab`, secret token, and these triggers:

| Trigger | `X-Gitlab-Event` | Becomes | When |
|---|---|---|---|
| Deployment events | `Deployment Hook` | `deploy` | `status` is `success`; summary has the short SHA, environment, commit title and user; link to the job |
| Feature flag events | `Feature Flag Hook` | `flag` | always; `enabled` or `disabled` by user |
| Releases events | `Release Hook` | `release` | `action` is `create`; the tag |
| Merge request events | `Merge Request Hook` | `merge` | `action` is `merge`; `!iid`, title, target branch, merge commit |

Any other event (push, pipeline, job, comment) answers `200` ignored: enable only the four triggers to spare the traffic. The component is the project name, lowercased. GitLab's timestamps (`2026-09-21 11:50:00 +0200`, `… UTC`) are normalised to RFC 3339. Payloads are lenient: only the fields read are modelled, from the [webhook events documentation](https://docs.gitlab.com/user/project/integrations/webhook_events/). Fixtures for all four events live under `examples/changes/`.

### Other sources

Anything else posts the generic shape. **Flux** notification-controller: a `Provider` of type `generic` posts its own event schema, so a small relay is needed; **GitHub Actions**, after the deploy step:

```yaml
- name: Tell signalman
  run: |
    curl -fsS -X POST "$SIGNALMAN_URL/changes" \
      -H "Authorization: Bearer ${{ secrets.SIGNALMAN_CHANGES_TOKEN }}" \
      -H "Content-Type: application/json" \
      -d "$(jq -nc --arg s "${{ github.repository }} ${{ github.sha }} by ${{ github.actor }}" \
                 --arg u "${{ github.server_url }}/${{ github.repository }}/commit/${{ github.sha }}" \
                 '{kind: "deploy", source: "github-actions", component: "checkout-api", summary: $s, url: $u}')"
```

Name `component` the way the alert labels will: the value of the `service` attribute in incident.io, or the catalog component name. For the adapters that is the Argo CD application name and the GitLab project name; when those differ from what the alerts carry, the change is not matched, which the `recent_changes` count in the outcome and the log line make visible.

## What the responder sees

When changes were offered, the [qualification note](incidentio.md#the-qualification-note) lists them under the cause line, and `ai-suspected-change` is tagged when the probability clears `policy.flag_change_above`. The `Outcome` carries `recent_changes`, the count offered.

## Limits

- In memory, per replica. A restart forgets the window; with several replicas each holds what was posted to it. Point the delivery tools at one replica, or accept that a change may be missing from some triages: the consequence is a question not asked, which is where the flow stood before the feed existed.
- Newest 1,000 changes are kept; older ones are evicted regardless of window.
- No deduplication: two identical posts are two lines. Post once per deploy, from the tool that knows it finished.
- Nothing has been wired to a real Argo CD or GitLab yet; the adapters follow the tools' documented payloads (fixtures under `examples/changes/`) and the Application CRD, not observed traffic.
