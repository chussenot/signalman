//! Svix-style webhook verification and event parsing for incident.io.
//!
//! incident.io delivers webhooks through Svix. Each request carries
//! `webhook-id`, `webhook-timestamp` and `webhook-signature` (Svix's
//! `svix-*` names are accepted too). The signature is
//! `base64(HMAC-SHA256(secret, "{id}.{timestamp}.{raw body}"))`, prefixed
//! `v1,`; several space-separated signatures may be present during secret
//! rotation. The secret is the base64 part after `whsec_`.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::types::{Alert, Incident, IncidentStatus};

/// Default tolerance for the timestamp check (Svix libraries use 5 minutes).
pub const DEFAULT_TOLERANCE: Duration = Duration::from_secs(5 * 60);

/// Environment variable holding the endpoint's signing secret.
pub const SECRET_ENV: &str = "INCIDENTIO_WEBHOOK_SECRET";

/// A decoded signing secret. Never printed.
#[derive(Clone)]
pub struct WebhookSecret(Vec<u8>);

impl fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookSecret(<redacted>)")
    }
}

impl WebhookSecret {
    /// Parse a `whsec_...` secret as shown in Settings → Webhooks. The prefix
    /// is optional; the remainder must be base64.
    pub fn parse(secret: &str) -> Result<Self, VerifyError> {
        let b64 = secret
            .trim()
            .strip_prefix("whsec_")
            .unwrap_or(secret.trim());
        let bytes = STANDARD.decode(b64).map_err(|_| VerifyError::BadSecret)?;
        if bytes.is_empty() {
            return Err(VerifyError::BadSecret);
        }
        Ok(Self(bytes))
    }

    /// Read from `INCIDENTIO_WEBHOOK_SECRET`.
    pub fn from_env() -> Result<Self, VerifyError> {
        let raw = std::env::var(SECRET_ENV).map_err(|_| VerifyError::MissingSecret)?;
        Self::parse(&raw)
    }

    /// Compute the `v1` signature for a message. Exposed so tests and tooling
    /// can sign payloads.
    pub fn sign(&self, id: &str, timestamp: i64, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0)
            .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
        mac.update(id.as_bytes());
        mac.update(b".");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
    }
}

/// The three verification headers, as extracted from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHeaders {
    /// `webhook-id` / `svix-id`.
    pub id: String,
    /// `webhook-timestamp` / `svix-timestamp`, seconds since epoch.
    pub timestamp: String,
    /// `webhook-signature` / `svix-signature`.
    pub signature: String,
}

impl SignatureHeaders {
    /// Pull the headers out of a map, accepting both prefixes.
    pub fn from_headers(headers: &axum::http::HeaderMap) -> Result<Self, VerifyError> {
        let get = |a: &str, b: &str| {
            headers
                .get(a)
                .or_else(|| headers.get(b))
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .ok_or_else(|| VerifyError::MissingHeader(a.to_owned()))
        };
        Ok(Self {
            id: get("webhook-id", "svix-id")?,
            timestamp: get("webhook-timestamp", "svix-timestamp")?,
            signature: get("webhook-signature", "svix-signature")?,
        })
    }
}

/// Why a webhook was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// No secret configured.
    #[error("no webhook secret: set INCIDENTIO_WEBHOOK_SECRET")]
    MissingSecret,
    /// Secret is not `whsec_<base64>`.
    #[error("webhook secret is not base64 (expected whsec_...)")]
    BadSecret,
    /// A required header is absent.
    #[error("missing header {0}")]
    MissingHeader(String),
    /// Timestamp header is not an integer.
    #[error("webhook-timestamp is not a unix timestamp")]
    BadTimestamp,
    /// Timestamp is outside the tolerance window.
    #[error("webhook-timestamp is {skew_secs}s from now, beyond the {tolerance_secs}s tolerance")]
    Stale {
        /// Absolute difference.
        skew_secs: u64,
        /// Allowed difference.
        tolerance_secs: u64,
    },
    /// No `v1` signature matched.
    #[error("webhook signature does not match")]
    Mismatch,
}

/// Verify a delivery. `now` is injected so tests are deterministic.
pub fn verify(
    secret: &WebhookSecret,
    headers: &SignatureHeaders,
    body: &[u8],
    now: SystemTime,
    tolerance: Duration,
) -> Result<(), VerifyError> {
    let ts: i64 = headers
        .timestamp
        .trim()
        .parse()
        .map_err(|_| VerifyError::BadTimestamp)?;
    let now_secs = i64::try_from(now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
        .unwrap_or(i64::MAX);
    let skew = now_secs.abs_diff(ts);
    if skew > tolerance.as_secs() {
        return Err(VerifyError::Stale {
            skew_secs: skew,
            tolerance_secs: tolerance.as_secs(),
        });
    }

    let expected = secret.sign(&headers.id, ts, body);
    let expected_b64 = expected.trim_start_matches("v1,").as_bytes();
    let matched = headers
        .signature
        .split_whitespace()
        .filter_map(|s| s.strip_prefix("v1,"))
        .any(|candidate| candidate.as_bytes().ct_eq(expected_b64).into());
    if matched {
        Ok(())
    } else {
        Err(VerifyError::Mismatch)
    }
}

/// Convenience: verify against the current time with the default tolerance.
pub fn verify_now(
    secret: &WebhookSecret,
    headers: &SignatureHeaders,
    body: &[u8],
) -> Result<(), VerifyError> {
    verify(secret, headers, body, SystemTime::now(), DEFAULT_TOLERANCE)
}

/// A parsed webhook. The envelope is `{"event_type": T, T: payload}`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `public_alert.alert_created_v1`: the trigger for triage.
    AlertCreated(Alert),
    /// `public_alert.alert_resolved_v1`.
    AlertResolved(Alert),
    /// `public_incident.incident_created_v2`.
    IncidentCreated(Incident),
    /// `public_incident.incident_status_updated_v2`.
    IncidentStatusUpdated {
        /// The incident after the change.
        incident: Incident,
        /// Status before.
        previous_status: IncidentStatus,
        /// Status after.
        new_status: IncidentStatus,
    },
    /// A private resource: the payload carries only its id, per the docs.
    Private {
        /// Event type.
        event_type: String,
        /// Resource id.
        id: String,
    },
    /// Anything this build does not model.
    Other {
        /// Event type.
        event_type: String,
        /// Raw payload.
        payload: Value,
    },
}

/// Envelope decoding failure.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// Body is not JSON.
    #[error("webhook body is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Missing `event_type`.
    #[error("webhook body has no event_type")]
    NoEventType,
    /// `event_type` names a key that is not in the body.
    #[error("webhook body has no payload under {0:?}")]
    NoPayload(String),
}

impl Event {
    /// Decode a raw body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let mut root: Value = serde_json::from_slice(body)?;
        let event_type = root
            .get("event_type")
            .and_then(Value::as_str)
            .ok_or(ParseError::NoEventType)?
            .to_owned();
        let payload = root
            .get_mut(&event_type)
            .map(Value::take)
            .ok_or_else(|| ParseError::NoPayload(event_type.clone()))?;

        Ok(match event_type.as_str() {
            "public_alert.alert_created_v1" => Self::AlertCreated(serde_json::from_value(payload)?),
            "public_alert.alert_resolved_v1" => {
                Self::AlertResolved(serde_json::from_value(payload)?)
            }
            "public_incident.incident_created_v2" => {
                Self::IncidentCreated(serde_json::from_value(payload)?)
            }
            "public_incident.incident_status_updated_v2" => {
                #[derive(serde::Deserialize)]
                struct Body {
                    incident: Incident,
                    previous_status: IncidentStatus,
                    new_status: IncidentStatus,
                }
                let b: Body = serde_json::from_value(payload)?;
                Self::IncidentStatusUpdated {
                    incident: b.incident,
                    previous_status: b.previous_status,
                    new_status: b.new_status,
                }
            }
            t if t.starts_with("private_") => {
                let id = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Self::Private { event_type, id }
            }
            _ => Self::Other {
                event_type,
                payload,
            },
        })
    }

    /// The `event_type` string.
    pub fn event_type(&self) -> &str {
        match self {
            Self::AlertCreated(_) => "public_alert.alert_created_v1",
            Self::AlertResolved(_) => "public_alert.alert_resolved_v1",
            Self::IncidentCreated(_) => "public_incident.incident_created_v2",
            Self::IncidentStatusUpdated { .. } => "public_incident.incident_status_updated_v2",
            Self::Private { event_type, .. } | Self::Other { event_type, .. } => event_type,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // Published on docs.svix.com/receiving/verifying-payloads/how-manual.
    const SECRET: &str = "whsec_plJ3nmyCDGBKInavdOK15jsl";
    const PAYLOAD: &str = r#"{"event_type":"ping","data":{"success":true}}"#;
    const MSG_ID: &str = "msg_loFOjxBNrRLzqYUf";
    const TIMESTAMP: i64 = 1_731_705_121;
    const SIGNATURE: &str = "v1,rAvfW3dJ/X/qxhsaXPOyyCGmRKsaKWcsNccKXlIktD0=";

    fn at(ts: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(ts.unsigned_abs())
    }

    fn headers(sig: &str) -> SignatureHeaders {
        SignatureHeaders {
            id: MSG_ID.into(),
            timestamp: TIMESTAMP.to_string(),
            signature: sig.into(),
        }
    }

    #[test]
    fn reproduces_the_svix_reference_signature() {
        let secret = WebhookSecret::parse(SECRET).unwrap();
        assert_eq!(
            secret.sign(MSG_ID, TIMESTAMP, PAYLOAD.as_bytes()),
            SIGNATURE
        );
    }

    #[test]
    fn accepts_a_valid_delivery_within_tolerance() {
        let secret = WebhookSecret::parse(SECRET).unwrap();
        verify(
            &secret,
            &headers(SIGNATURE),
            PAYLOAD.as_bytes(),
            at(TIMESTAMP + 60),
            DEFAULT_TOLERANCE,
        )
        .unwrap();
    }

    #[test]
    fn accepts_when_any_of_several_signatures_matches() {
        let secret = WebhookSecret::parse(SECRET).unwrap();
        let multi = format!("v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= {SIGNATURE} v2,zzz");
        verify(
            &secret,
            &headers(&multi),
            PAYLOAD.as_bytes(),
            at(TIMESTAMP),
            DEFAULT_TOLERANCE,
        )
        .unwrap();
    }

    #[test]
    fn rejects_tampered_body_wrong_secret_and_stale_timestamp() {
        let secret = WebhookSecret::parse(SECRET).unwrap();
        assert_eq!(
            verify(
                &secret,
                &headers(SIGNATURE),
                br#"{"event_type":"ping","data":{"success":false}}"#,
                at(TIMESTAMP),
                DEFAULT_TOLERANCE
            ),
            Err(VerifyError::Mismatch)
        );
        let other = WebhookSecret::parse("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        assert_eq!(
            verify(
                &other,
                &headers(SIGNATURE),
                PAYLOAD.as_bytes(),
                at(TIMESTAMP),
                DEFAULT_TOLERANCE
            ),
            Err(VerifyError::Mismatch)
        );
        assert!(matches!(
            verify(
                &secret,
                &headers(SIGNATURE),
                PAYLOAD.as_bytes(),
                at(TIMESTAMP + 3600),
                DEFAULT_TOLERANCE
            ),
            Err(VerifyError::Stale { .. })
        ));
    }

    #[test]
    fn secret_prefix_is_optional_and_garbage_is_rejected() {
        assert!(WebhookSecret::parse("plJ3nmyCDGBKInavdOK15jsl").is_ok());
        assert!(matches!(
            WebhookSecret::parse("whsec_!!!"),
            Err(VerifyError::BadSecret)
        ));
        assert!(!format!("{:?}", WebhookSecret::parse(SECRET).unwrap()).contains("plJ3"));
    }

    #[test]
    fn parses_alert_created_envelope() {
        let body = serde_json::json!({
            "event_type": "public_alert.alert_created_v1",
            "public_alert.alert_created_v1": {
                "id": "01GW", "alert_source_id": "src", "title": "CPU high", "status": "firing",
                "deduplication_key": "4293868629", "attributes": [], "tags": []
            }
        });
        match Event::parse(body.to_string().as_bytes()).unwrap() {
            Event::AlertCreated(a) => assert_eq!(a.id, "01GW"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn private_and_unknown_events_do_not_fail() {
        let private = serde_json::json!({
            "event_type": "private_incident.incident_created_v2",
            "private_incident.incident_created_v2": { "id": "abc123" }
        });
        assert_eq!(
            Event::parse(private.to_string().as_bytes()).unwrap(),
            Event::Private {
                event_type: "private_incident.incident_created_v2".into(),
                id: "abc123".into()
            }
        );
        let other = serde_json::json!({ "event_type": "schedule.shift_change_v1", "schedule.shift_change_v1": { "x": 1 } });
        assert!(matches!(
            Event::parse(other.to_string().as_bytes()).unwrap(),
            Event::Other { .. }
        ));
        assert!(matches!(Event::parse(b"{}"), Err(ParseError::NoEventType)));
    }
}
