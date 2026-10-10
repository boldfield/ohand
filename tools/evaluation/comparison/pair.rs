//! Pair-level views that stay separate from each other: whether two valid candidates differ,
//! and whether each arm is objectively clean against the oracle. Agreement says nothing about
//! correctness, and neither feeds the other.

use serde::Serialize;

use super::arm::Candidate;
use crate::report::{CaseReport, ProducerOutcome};
use crate::score::AbstentionOutcome;
use crate::scratch::GuardOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisagreementFacet {
    ItemType,
    Reminder,
    SessionTopic,
    Abstention,
    RequestedOperation,
    /// The source spans an arm cites for its item, reminder or session topic.
    GroundingEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Disagreement {
    Agree,
    Differ {
        facets: Vec<DisagreementFacet>,
    },
    /// At least one arm has no valid proposal, so there is nothing to compare.
    NotComparable,
}

pub(super) fn disagreement(a: Option<&Candidate>, b: Option<&Candidate>) -> Disagreement {
    let (Some(a), Some(b)) = (a, b) else {
        return Disagreement::NotComparable;
    };
    let mut facets = Vec::new();
    if a.view.item_type != b.view.item_type {
        facets.push(DisagreementFacet::ItemType);
    }
    let reminder_key = |candidate: &Candidate| {
        candidate.view.reminder.as_ref().map(|reminder| {
            (
                reminder.quality,
                reminder.instant,
                reminder.timezone_id.clone(),
            )
        })
    };
    if reminder_key(a) != reminder_key(b) {
        facets.push(DisagreementFacet::Reminder);
    }
    let topic_key = |candidate: &Candidate| {
        candidate
            .view
            .session_topic
            .as_ref()
            .map(|topic| topic.trim().to_lowercase())
    };
    if topic_key(a) != topic_key(b) {
        facets.push(DisagreementFacet::SessionTopic);
    }
    if a.abstention != b.abstention {
        facets.push(DisagreementFacet::Abstention);
    }
    if a.view.requested_operation != b.view.requested_operation {
        facets.push(DisagreementFacet::RequestedOperation);
    }
    let evidence_key = |candidate: &Candidate| {
        let mut spans = candidate.view.spans.clone();
        spans.sort_unstable();
        (
            spans,
            candidate
                .view
                .reminder
                .as_ref()
                .and_then(|reminder| reminder.evidence),
            candidate.view.session_topic_span,
        )
    };
    if evidence_key(a) != evidence_key(b) {
        facets.push(DisagreementFacet::GroundingEvidence);
    }
    if facets.is_empty() {
        Disagreement::Agree
    } else {
        Disagreement::Differ { facets }
    }
}

/// No defect, no forbidden hit, a correct or absent abstention outcome, and a reply the
/// pipeline accepted. A reply the pipeline rejected is never clean.
pub(super) fn objectively_clean(case: &CaseReport) -> bool {
    matches!(
        case.producer,
        ProducerOutcome::Proposed | ProducerOutcome::Abstained
    ) && !matches!(case.guard, Some(GuardOutcome::Rejected { .. }))
        && case.candidate_defects.is_empty()
        && case.authoritative_defects.is_empty()
        && case.candidate_forbidden.is_empty()
        && case.authoritative_forbidden.is_empty()
        && matches!(case.abstention, None | Some(AbstentionOutcome::Correct))
}
