#!/usr/bin/env sh
# Create a least-privilege incident.io API key for signalman and prove it can
# reach every endpoint the flow uses, before anyone puts it in a deployment.
#
# Why a script: incident.io documents its roles by name only (viewer,
# incident_editor, ...) and never says which role covers alert tags, alert
# notes or incident_alerts. The shared key that was verified on 2026-09-23
# (signalman-b11.2) had global_access, which proves nothing about the minimal
# set. So this creates a key with the roles you name, then probes each
# endpoint with the new key: reads must answer 200, writes are sent an empty
# body and must answer 422 (validation ran, so authorisation passed) rather
# than 403. A key that fails a probe is deleted again unless --keep is given.
#
# Requirements: curl, jq. The calling key needs the api_keys_manage role
# (Settings -> API keys); it may only grant roles it holds itself.
#
# Usage:
#   INCIDENTIO_ADMIN_KEY=inc_... scripts/incidentio-create-key.sh [options]
#     -n NAME     key name                  (default: signalman)
#     -r ROLES    comma-separated roles     (default: viewer,incident_editor)
#     -c TEXT     comment stored on the key
#     --keep      keep the key even when a probe fails
#     --dry-run   print the request, create nothing
#
# The new token is printed once, on stdout, as the last line. Everything else
# goes to stderr, so `... 2>/dev/null` yields the token alone.
set -eu

BASE_URL=${INCIDENTIO_BASE_URL:-https://api.incident.io}
name=signalman
roles=viewer,incident_editor
comment="Created by scripts/incidentio-create-key.sh for signalman: alert tags, alert notes, incident_alerts, incidents and alerts reads."
keep=0
dry_run=0

while [ $# -gt 0 ]; do
  case $1 in
    -n) name=$2; shift 2 ;;
    -r) roles=$2; shift 2 ;;
    -c) comment=$2; shift 2 ;;
    --keep) keep=1; shift ;;
    --dry-run) dry_run=1; shift ;;
    -h|--help) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

for tool in curl jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "$tool is required" >&2; exit 2; }
done

# role_names from the comma list; team scoping is deliberately empty: the
# flow reads and writes alerts across the account.
body=$(jq -cn --arg name "$name" --arg roles "$roles" --arg comment "$comment" \
  '{name: $name, comments: $comment, team_ids: [], team_role_names: [],
    role_names: ($roles | split(",") | map(gsub("^\\s+|\\s+$"; "")) | map(select(. != "")))}')

if [ "$dry_run" = 1 ]; then
  echo "POST $BASE_URL/v1/api_keys" >&2
  echo "$body" | jq . >&2
  exit 0
fi

admin=${INCIDENTIO_ADMIN_KEY:-${INCIDENTIO_API_KEY:-}}
[ -n "$admin" ] || { echo "set INCIDENTIO_ADMIN_KEY (a key with api_keys_manage)" >&2; exit 2; }

# ---------------------------------------------------------------------------
# Create
# ---------------------------------------------------------------------------
resp=$(curl -sS -m 30 -w '\n%{http_code}' -X POST "$BASE_URL/v1/api_keys" \
  -H "Authorization: Bearer $admin" -H 'Content-Type: application/json' -d "$body")
code=${resp##*
}
json=${resp%
*}
if [ "$code" != 201 ]; then
  echo "create failed: HTTP $code" >&2
  echo "$json" | jq -r '.errors[]?.message // .message // .' >&2 2>/dev/null || echo "$json" >&2
  [ "$code" = 403 ] && echo "hint: the calling key needs api_keys_manage and every role it grants" >&2
  exit 1
fi
key_id=$(echo "$json" | jq -r '.api_key.id')
token=$(echo "$json" | jq -r '.token')
echo "created key $name ($key_id) with roles: $roles" >&2

# ---------------------------------------------------------------------------
# Probe with the new key
# ---------------------------------------------------------------------------
failed=0
probe() { # method path expected-code [json-body]
  m=$1; p=$2; want=$3; data=${4:-}
  if [ -n "$data" ]; then
    got=$(curl -sS -m 20 -o /dev/null -w '%{http_code}' -X "$m" "$BASE_URL$p" \
      -H "Authorization: Bearer $token" -H 'Content-Type: application/json' -d "$data")
  else
    got=$(curl -sS -m 20 -o /dev/null -w '%{http_code}' -X "$m" "$BASE_URL$p" \
      -H "Authorization: Bearer $token")
  fi
  if [ "$got" = "$want" ]; then
    printf '  ok   %-4s %-45s %s\n' "$m" "$p" "$got" >&2
  else
    printf '  FAIL %-4s %-45s %s (wanted %s)\n' "$m" "$p" "$got" "$want" >&2
    failed=1
  fi
}

echo "probing with the new key:" >&2
probe GET  /v1/identity                          200
probe GET  '/v2/incidents?page_size=1'           200
probe GET  '/v2/alerts?page_size=1'              200
# One real alert id makes the read probes exact; without one they are skipped.
alert_id=$(curl -sS -m 20 "$BASE_URL/v2/alerts?page_size=1" -H "Authorization: Bearer $token" \
  | jq -r '.alerts[0].id // empty' 2>/dev/null || true)
if [ -n "$alert_id" ]; then
  probe GET  "/v2/alerts/$alert_id"                 200
  probe GET  "/v1/alert_notes?alert_id=$alert_id"   200
  probe POST "/v2/alerts/$alert_id/actions/add_tags" 422 '{}'
else
  echo "  skip GET  /v2/alerts/{id}, /v1/alert_notes: no alert to read" >&2
fi
probe POST /v1/alert_notes                       422 '{}'
probe POST /v2/incident_alerts                   422 '{}'

if [ "$failed" = 1 ]; then
  echo "a probe failed: the roles $roles do not cover every endpoint (403 = missing role)." >&2
  if [ "$keep" = 1 ]; then
    echo "keeping $key_id as asked" >&2
  else
    curl -sS -m 20 -o /dev/null -X DELETE "$BASE_URL/v1/api_keys/$key_id" -H "Authorization: Bearer $admin" \
      && echo "deleted $key_id; try again with -r and a wider role set" >&2
    exit 1
  fi
fi

echo "every endpoint signalman uses answers as expected. Put this in .env as INCIDENTIO_API_KEY:" >&2
echo "$token"
