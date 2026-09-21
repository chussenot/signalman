//! The qualification note: everything the tags imply and what they cannot
//! carry, in one markdown body on the alert, where the responder already is.
//!
//! The note is rendered from a fixed template in code (decision 0002: the
//! model judges, it does not write). It starts with [`MARKER`] so a later
//! triage of the same alert finds and rewrites it instead of stacking notes.

use std::fmt::Write;
use std::time::Duration;

use super::sync::AttachedIncident;
use super::types::Alert as IoAlert;
use crate::triage::{
    ComponentContext, Decision, Impact, NO_DUPLICATE, RelatedAlert, TriageAnswers,
};

/// First line of every note signalman writes. Also how it recognises its own.
pub const MARKER: &str = "**Signalman qualification**";

/// Most related alerts listed; the count is always given.
const MAX_RELATED_LISTED: usize = 8;

/// Probability below which an alternative option is not worth a mention.
const MENTION_ABOVE: f64 = 0.05;

/// Links the note points at. All optional: they depend on what is configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    /// Backstage page of the resolved component.
    pub component: Option<String>,
    /// Backstage page of the owner the decision names.
    pub owner: Option<String>,
    /// TechDocs page the runbook excerpt came from.
    pub runbook: Option<String>,
}

/// What the note is rendered from.
#[derive(Debug, Clone)]
pub struct NoteInput<'a> {
    /// The alert as fetched from incident.io.
    pub alert: &'a IoAlert,
    /// Typed answers.
    pub answers: &'a TriageAnswers,
    /// The decision.
    pub decision: &'a Decision,
    /// The incident the alert was attached to, if any.
    pub attached: Option<&'a AttachedIncident>,
    /// Catalog context, when resolved.
    pub component: Option<&'a ComponentContext>,
    /// Links.
    pub links: &'a Links,
    /// Other alerts firing in the window.
    pub related: &'a [RelatedAlert],
    /// The window the related alerts were taken from.
    pub related_window: Duration,
    /// Recent changes offered to the model (state lines).
    pub changes: &'a [String],
    /// Tags written on the alert.
    pub tags: &'a [String],
    /// From the alert's creation to the decision.
    pub time_to_qualify: Option<Duration>,
}

/// True when `content` is a note this module wrote.
pub fn is_signalman_note(content: &str) -> bool {
    content.trim_start().starts_with(MARKER)
}

/// Render the note. Infallible: writing to a `String` cannot fail.
#[allow(clippy::too_many_lines)] // one template, read top to bottom
pub fn render(input: &NoteInput<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{MARKER}");
    let _ = writeln!(out);
    let _ = writeln!(out, "**Decision:** {}", decision_line(input));
    let _ = writeln!(out);

    let a = input.answers;

    // Owner.
    let owner_label = a
        .candidates
        .get(&a.owner.chosen)
        .map_or(a.owner.chosen.as_str(), |c| c.label.as_str());
    let mut owner = format!(
        "- Owner: {} (confidence {:.2}",
        linked(owner_label, input.links.owner.as_deref()),
        a.owner.confidence.value()
    );
    let alternatives = ranked_alternatives(
        a.owner
            .probabilities
            .iter()
            .filter(|(k, _)| **k != a.owner.chosen)
            .map(|(k, p)| {
                let label = a.candidates.get(k).map_or(k.as_str(), |c| c.label.as_str());
                (label.to_owned(), p.value())
            }),
    );
    if !alternatives.is_empty() {
        let _ = write!(owner, "; next: {alternatives}");
    }
    owner.push(')');
    let _ = writeln!(out, "{owner}");

    // Impact.
    let level = a.impact.nearest_level();
    let mut impact = format!(
        "- Impact: {:?} (P {:.2}",
        Impact::from_level(level),
        a.impact.probabilities.get(level).map_or(0.0, |p| p.value())
    );
    let others = ranked_alternatives(
        a.impact
            .probabilities
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != level)
            .map(|(i, p)| (format!("{:?}", Impact::from_level(i)), p.value())),
    );
    if !others.is_empty() {
        let _ = write!(impact, "; {others}");
    }
    impact.push(')');
    let _ = writeln!(out, "{impact}");

    let _ = writeln!(out, "- Actionable: {:.2}", a.actionable.yes.value());

    // Duplicate.
    match &a.duplicate_of {
        Some(dup) if dup.chosen != NO_DUPLICATE => {
            let target = input
                .attached
                .filter(|i| i.reference == dup.chosen)
                .map_or_else(
                    || dup.chosen.clone(),
                    |i| linked(&i.reference, i.permalink.as_deref()),
                );
            let _ = writeln!(
                out,
                "- Duplicate of: {target} (confidence {:.2}){}",
                dup.confidence.value(),
                if input.attached.is_some() {
                    ", attached"
                } else {
                    ", below the attach threshold"
                }
            );
        }
        Some(dup) => {
            let _ = writeln!(
                out,
                "- Duplicate of: none of the {} open incidents offered (confidence {:.2})",
                dup.probabilities.len().saturating_sub(1),
                dup.confidence.value()
            );
        }
        None => {
            let _ = writeln!(out, "- Duplicate of: not asked, no open incidents");
        }
    }

    // Change.
    if let Some(c) = a.caused_by_change {
        let _ = writeln!(out, "- Recent change as cause: {:.2}", c.yes.value());
        for line in input.changes.iter().take(MAX_RELATED_LISTED) {
            let _ = writeln!(out, "  - {line}");
        }
        if input.changes.len() > MAX_RELATED_LISTED {
            let _ = writeln!(
                out,
                "  - and {} more",
                input.changes.len() - MAX_RELATED_LISTED
            );
        }
    }

    // Catalog.
    if let Some(c) = input.component {
        let mut line = format!(
            "- Component: {}",
            linked(&c.name, input.links.component.as_deref())
        );
        let facts: Vec<&str> = [c.component_type.as_deref(), c.lifecycle.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if !facts.is_empty() {
            let _ = write!(line, " ({})", facts.join(", "));
        }
        if let Some(owner) = &c.owner {
            let _ = write!(line, ", registered owner {owner}");
        }
        if !c.dependents.is_empty() {
            let _ = write!(line, "; depended on by {}", c.dependents.join(", "));
        }
        let _ = writeln!(out, "{line}");
    }
    match &input.links.runbook {
        Some(url) => {
            let _ = writeln!(out, "- Runbook: [TechDocs page]({url})");
        }
        None => {
            let _ = writeln!(out, "- Runbook: none found");
        }
    }

    // Blast radius.
    let window_min = input.related_window.as_secs() / 60;
    if input.related.is_empty() {
        let _ = writeln!(out, "- Related firing alerts (last {window_min} min): none");
    } else {
        let _ = writeln!(
            out,
            "- Related firing alerts (last {window_min} min): {}",
            input.related.len()
        );
        for r in input.related.iter().take(MAX_RELATED_LISTED) {
            let mut line = format!("  - {} ({} min ago", r.title, r.age_minutes);
            if let Some(c) = &r.component {
                let _ = write!(line, ", {c}");
            }
            line.push(')');
            let _ = writeln!(out, "{line}");
        }
        if input.related.len() > MAX_RELATED_LISTED {
            let _ = writeln!(
                out,
                "  - and {} more",
                input.related.len() - MAX_RELATED_LISTED
            );
        }
    }

    if let Some(url) = &input.alert.source_url {
        let _ = writeln!(out, "- Upstream: {url}");
    }

    let _ = writeln!(out);
    let mut footer = String::new();
    if let Some(t) = input.time_to_qualify {
        let _ = write!(footer, "Time to qualify: {}. ", human_duration(t));
    }
    let _ = write!(footer, "Model {}.", a.model);
    if !input.tags.is_empty() {
        let _ = write!(footer, " Tags: {}.", input.tags.join(", "));
    }
    let _ = writeln!(out, "_{footer}_");
    out
}

fn decision_line(input: &NoteInput<'_>) -> String {
    match input.decision {
        Decision::Suppress { actionable } => {
            format!("Suppress. Nobody needs to act (actionable {actionable:.2}).")
        }
        Decision::AttachToIncident {
            incident_id,
            confidence,
        } => {
            let target = input.attached.map_or_else(
                || incident_id.clone(),
                |i| linked(&i.reference, i.permalink.as_deref()),
            );
            format!(
                "Attach to {target}: same problem as an open incident (confidence {confidence:.2})."
            )
        }
        Decision::Page {
            owner,
            impact,
            confirm_owner,
            suspected_change,
        } => format!(
            "Page **{}** ({impact:?} impact).{}{}",
            owner.label,
            confirm_note(*confirm_owner),
            change_note(*suspected_change)
        ),
        Decision::Ticket {
            owner,
            impact,
            confirm_owner,
            suspected_change,
        } => format!(
            "Ticket for **{}** ({impact:?} impact), no page.{}{}",
            owner.label,
            confirm_note(*confirm_owner),
            change_note(*suspected_change)
        ),
        Decision::HumanTriage {
            best_guess,
            confidence,
            impact,
        } => format!(
            "Human triage. Owner unclear (best guess {}, confidence {confidence:.2}); {impact:?} impact.",
            best_guess.label
        ),
    }
}

fn confirm_note(confirm: bool) -> &'static str {
    if confirm {
        " Owner confidence is moderate: confirm ownership."
    } else {
        ""
    }
}

fn change_note(suspected: bool) -> &'static str {
    if suspected {
        " A listed recent change is a suspected cause."
    } else {
        ""
    }
}

/// `[text](url)` when a URL is known, plain text otherwise.
fn linked(text: &str, url: Option<&str>) -> String {
    match url {
        Some(u) => format!("[{text}]({u})"),
        None => text.to_owned(),
    }
}

/// `Label 0.20, Other 0.08` for options above the mention threshold, best first.
fn ranked_alternatives(items: impl Iterator<Item = (String, f64)>) -> String {
    let mut v: Vec<(String, f64)> = items.filter(|(_, p)| *p >= MENTION_ABOVE).collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v.iter()
        .take(2)
        .map(|(l, p)| format!("{l} {p:.2}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `42 s` below two minutes, `3.5 min` below two hours, `2.1 h` above.
fn human_duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 120.0 {
        format!("{s:.0} s")
    } else if s < 7200.0 {
        format!("{:.1} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn recognises_only_its_own_notes() {
        assert!(is_signalman_note("**Signalman qualification**\n\nx"));
        assert!(is_signalman_note("\n  **Signalman qualification** more"));
        assert!(!is_signalman_note("Customer reports 500s"));
        assert!(!is_signalman_note("Signalman qualification"));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(Duration::from_secs(42)), "42 s");
        assert_eq!(human_duration(Duration::from_secs(210)), "3.5 min");
        assert_eq!(human_duration(Duration::from_secs(9000)), "2.5 h");
    }

    #[test]
    fn alternatives_are_ranked_and_thresholded() {
        let s = ranked_alternatives(
            [
                ("a".to_owned(), 0.02),
                ("b".to_owned(), 0.3),
                ("c".to_owned(), 0.1),
            ]
            .into_iter(),
        );
        assert_eq!(s, "b 0.30, c 0.10");
    }
}
