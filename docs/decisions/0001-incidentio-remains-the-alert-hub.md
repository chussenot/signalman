---
title: 0001 incident.io remains the alert hub
description: rustsafe enriches alerts with tags and attachments and never creates incidents, so incident.io alert routes keep ownership of escalation.
status: accepted
date: 2026-09-20
beads: rustsafe-p2w
last_reviewed: 2026-09-20
tags: [decisions, incidentio]
---

# 0001 incident.io remains the alert hub

## Context

incident.io already receives alerts from every monitoring source, groups them, and decides through alert routes whether to open an incident and whom to escalate to. The API also allows creating incidents directly, with a limit of 10 per hour per key when a chat channel is created.

## Decision

rustsafe writes alert tags and alert-to-incident attachments, or forwards enriched alerts to an HTTP alert source. It does not create, edit or resolve incidents.

## Consequences

- One routing model. Operators change escalation in incident.io, not in rustsafe's code.
- The creation limit is never approached.
- A wrong judgment is a wrong tag, visible and reversible in the incident.io UI, not a spurious incident with a paged responder.
- Anything rustsafe wants incident.io to do must be expressible as an alert route condition. If that proves insufficient, this decision is revisited rather than worked around.
