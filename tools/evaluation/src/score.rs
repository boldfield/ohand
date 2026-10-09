//! Semantic comparison of an evaluated outcome with a fixture's `expected` and `forbidden`.
//!
//! Nothing here produces an accuracy figure. A case yields typed defects, an abstention outcome
//! and forbidden-rule hits, and the report counts each of them separately. The same comparison
//! runs at two stages: the candidate the interpreter proposed, and the authoritative state that
//! survived the application guard.

use chrono::{DateTime, Utc};
use ohand_core::interpretation::contracts::{
    AbstentionReason, Operation, Proposal, TimeResolutionQuality,
};
use ohand_core::store::events::ItemType;
use serde::Serialize;

use crate::corpus::{Expected, FacetBan, Forbidden, ForbiddenOperation, ReminderBan};
use crate::scratch::AuthoritativeState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// What the interpreter or recognizer proposed, before the application guard.
    Candidate,
    /// What the application guard left in durable state.
    Authoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefectKind {
    /// An `action` where the oracle has none.
    FalseAction,
    /// A resolved reminder instant the oracle does not support.
    FalseDeadline,
    /// A request to change, or a change of, an existing item's lifecycle.
    FalseCompletion,
    /// An item type other than the oracle's (and other than `action`, which is `FalseAction`),
    /// or an item type where the oracle has none.
    WrongItemType,
    /// Any other facet, evidence or quality the oracle does not support.
    UnsupportedClaim,
    /// A facet the oracle expects that was not produced (or that the guard dropped).
    MissedIntent,
}

impl DefectKind {
    pub const ALL: [DefectKind; 6] = [
        DefectKind::FalseAction,
        DefectKind::FalseDeadline,
        DefectKind::FalseCompletion,
        DefectKind::WrongItemType,
        DefectKind::UnsupportedClaim,
        DefectKind::MissedIntent,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Defect {
    pub kind: DefectKind,
    pub stage: Stage,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionOutcome {
    /// Oracle abstains with this reason variant and so did the interpreter.
    Correct,
    /// Both abstain, with different reason variants.
    WrongReason,
    /// Oracle abstains; the interpreter proposed facets instead.
    Missed,
    /// The interpreter abstained where the oracle has facets.
    Unexpected,
}

impl AbstentionOutcome {
    pub const ALL: [AbstentionOutcome; 4] = [
        AbstentionOutcome::Correct,
        AbstentionOutcome::WrongReason,
        AbstentionOutcome::Missed,
        AbstentionOutcome::Unexpected,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationClass {
    /// The item type (model-supplied label).
    Classification,
    /// A reminder or deadline.
    Reminder,
    /// The session-topic facet (model-supplied label).
    SessionTopic,
    /// A create or update operation.
    Operation,
    /// The item left the active state (completion is an explicit UI control, never derived).
    Lifecycle,
    /// The raw capture or the user's correction no longer matches what was saved.
    SourceText,
}

impl MutationClass {
    pub const ALL: [MutationClass; 6] = [
        MutationClass::Classification,
        MutationClass::Reminder,
        MutationClass::SessionTopic,
        MutationClass::Operation,
        MutationClass::Lifecycle,
        MutationClass::SourceText,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForbiddenHit {
    pub stage: Stage,
    pub class: MutationClass,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReminderView {
    /// Known only for a candidate; the store keeps no resolution quality.
    pub quality: Option<TimeResolutionQuality>,
    pub instant: Option<DateTime<Utc>>,
    pub timezone_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedOperation {
    Create,
    Update,
}

/// The facets an outcome carries, in one shape for candidates and for durable state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FacetView {
    pub item_type: Option<ItemType>,
    pub spans: Vec<(usize, usize)>,
    pub reminder: Option<ReminderView>,
    pub session_topic: Option<String>,
    pub requested_operation: Option<RequestedOperation>,
}

impl FacetView {
    pub fn is_empty(&self) -> bool {
        self.item_type.is_none() && self.reminder.is_none() && self.session_topic.is_none()
    }

    /// The facets of a proposal. An abstention carries none, and the contract's one legal
    /// non-annotate form (an `UnsupportedOperation` abstention without facets) requests nothing.
    pub fn of_proposal(proposal: &Proposal) -> FacetView {
        let requested_operation = match (&proposal.operation, &proposal.abstention) {
            (Operation::Annotate {}, _) => None,
            (_, Some(AbstentionReason::UnsupportedOperation)) => None,
            (Operation::Create {}, _) => Some(RequestedOperation::Create),
            (Operation::Update { .. }, _) => Some(RequestedOperation::Update),
        };
        FacetView {
            item_type: proposal.item_type,
            spans: proposal
                .source_spans
                .iter()
                .flatten()
                .map(|span| (span.start, span.end))
                .collect(),
            reminder: proposal
                .reminder_proposal
                .as_ref()
                .map(|reminder| ReminderView {
                    quality: Some(reminder.quality),
                    instant: reminder
                        .instant
                        .as_deref()
                        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                        .map(|instant| instant.with_timezone(&Utc)),
                    timezone_id: reminder.timezone_id.clone(),
                }),
            session_topic: proposal
                .session_topic_proposal
                .as_ref()
                .map(|topic| topic.topic.clone()),
            requested_operation,
        }
    }

    /// The facets durable state holds. A stored reminder has no resolution quality to compare;
    /// what is comparable is whether it carries an instant, and in which zone.
    pub fn of_state(state: &AuthoritativeState) -> FacetView {
        FacetView {
            item_type: state.item_type,
            spans: Vec::new(),
            reminder: state.reminder.as_ref().map(|reminder| ReminderView {
                quality: None,
                instant: reminder.resolved_instant,
                timezone_id: reminder.timezone_id.clone(),
            }),
            session_topic: state.session_topic.clone(),
            requested_operation: None,
        }
    }
}

fn expected_instant(expected: &Expected) -> Option<DateTime<Utc>> {
    expected
        .reminder_proposal
        .as_ref()
        .and_then(|reminder| reminder.instant.as_deref())
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|instant| instant.with_timezone(&Utc))
}

fn normalized_topic(topic: &str) -> String {
    topic.trim().to_lowercase()
}

fn overlaps(left: (usize, usize), right: (usize, usize)) -> bool {
    left.0 < right.1 && right.0 < left.1
}

/// Defects of `view` against the oracle. `proposed` is the candidate view when `stage` is
/// `Authoritative`: a missing facet is only the guard's doing if the candidate carried it, and
/// an abstaining candidate's missing facets are reported once, as an unexpected abstention.
pub fn defects_against(
    expected: &Expected,
    view: &FacetView,
    stage: Stage,
    proposed: Option<&FacetView>,
    abstained: bool,
) -> Vec<Defect> {
    let mut defects = Vec::new();
    let mut push = |kind: DefectKind, detail: String| {
        defects.push(Defect {
            kind,
            stage,
            detail,
        })
    };
    let may_miss = |candidate_had: &dyn Fn(&FacetView) -> bool| match stage {
        Stage::Candidate => !abstained,
        Stage::Authoritative => proposed.is_some_and(candidate_had),
    };

    match (view.item_type, expected.item_type) {
        (Some(ItemType::Action), expected_type) if expected_type != Some(ItemType::Action) => push(
            DefectKind::FalseAction,
            format!(
                "action where the oracle has {}",
                expected_type.map_or("no item type", |item_type| item_type.as_str())
            ),
        ),
        (Some(found), Some(wanted)) if found != wanted => push(
            DefectKind::WrongItemType,
            format!(
                "item type {} where the oracle has {}",
                found.as_str(),
                wanted.as_str()
            ),
        ),
        (Some(found), None) => push(
            DefectKind::WrongItemType,
            format!("item type {} where the oracle has none", found.as_str()),
        ),
        (None, Some(wanted)) if may_miss(&|candidate| candidate.item_type.is_some()) => push(
            DefectKind::MissedIntent,
            format!("no item type; the oracle has {}", wanted.as_str()),
        ),
        _ => {}
    }
    if view.item_type.is_some() {
        if let Some(oracle_spans) = &expected.source_spans {
            for span in &view.spans {
                let supported = oracle_spans
                    .iter()
                    .any(|oracle| overlaps(*span, (oracle.start, oracle.end)));
                if !supported {
                    push(
                        DefectKind::UnsupportedClaim,
                        format!(
                            "item-type evidence {}..{} overlaps none of the oracle's spans",
                            span.0, span.1
                        ),
                    );
                }
            }
        }
    }

    let wanted_reminder = expected.reminder_proposal.as_ref();
    match (&view.reminder, wanted_reminder) {
        (Some(found), wanted) => {
            let wanted_instant = expected_instant(expected);
            if found.instant.is_some() && found.instant != wanted_instant {
                push(
                    DefectKind::FalseDeadline,
                    match wanted_instant {
                        None => "a resolved reminder instant where the oracle has none".to_string(),
                        Some(_) => "a reminder instant that is not the oracle's".to_string(),
                    },
                );
            } else if wanted.is_none() {
                push(
                    DefectKind::UnsupportedClaim,
                    "a reminder where the oracle has none".to_string(),
                );
            } else if let Some(wanted) = wanted {
                if found.instant.is_none() && wanted_instant.is_some() {
                    push(
                        DefectKind::MissedIntent,
                        "a reminder without the instant the oracle has".to_string(),
                    );
                } else if found
                    .quality
                    .is_some_and(|quality| quality != wanted.quality)
                {
                    push(
                        DefectKind::UnsupportedClaim,
                        format!(
                            "reminder quality {} where the oracle has {}",
                            found.quality.map_or("unknown", |quality| quality.as_str()),
                            wanted.quality.as_str()
                        ),
                    );
                } else if wanted.timezone_id.is_some() && found.timezone_id != wanted.timezone_id {
                    push(
                        DefectKind::UnsupportedClaim,
                        "reminder timezone differs from the oracle's".to_string(),
                    );
                }
            }
        }
        (None, Some(_)) if may_miss(&|candidate| candidate.reminder.is_some()) => push(
            DefectKind::MissedIntent,
            "no reminder; the oracle has one".to_string(),
        ),
        _ => {}
    }

    let wanted_topic = expected
        .session_topic_proposal
        .as_ref()
        .map(|topic| normalized_topic(&topic.topic));
    match (&view.session_topic, wanted_topic) {
        (Some(found), None) => push(
            DefectKind::UnsupportedClaim,
            format!("session topic {found:?} where the oracle has none"),
        ),
        (Some(found), Some(wanted)) if normalized_topic(found) != wanted => push(
            DefectKind::UnsupportedClaim,
            format!("session topic {found:?} where the oracle has {wanted:?}"),
        ),
        (None, Some(_)) if may_miss(&|candidate| candidate.session_topic.is_some()) => push(
            DefectKind::MissedIntent,
            "no session topic; the oracle has one".to_string(),
        ),
        _ => {}
    }

    match view.requested_operation {
        Some(RequestedOperation::Update) => push(
            DefectKind::FalseCompletion,
            "asked to change an existing item".to_string(),
        ),
        Some(RequestedOperation::Create) => push(
            DefectKind::UnsupportedClaim,
            "asked to create another item".to_string(),
        ),
        None => {}
    }
    defects
}

/// The one abstention outcome of a candidate, if either side abstains.
pub fn abstention_outcome(
    expected: &Expected,
    candidate: Option<&AbstentionReason>,
) -> Option<AbstentionOutcome> {
    match (&expected.abstention, candidate) {
        (None, None) => None,
        (Some(wanted), Some(found)) => Some(
            if std::mem::discriminant(wanted) == std::mem::discriminant(found) {
                AbstentionOutcome::Correct
            } else {
                AbstentionOutcome::WrongReason
            },
        ),
        (Some(_), None) => Some(AbstentionOutcome::Missed),
        (None, Some(_)) => Some(AbstentionOutcome::Unexpected),
    }
}

/// Every forbidden rule `view` breaks.
pub fn forbidden_hits(forbidden: &Forbidden, view: &FacetView, stage: Stage) -> Vec<ForbiddenHit> {
    let mut hits = Vec::new();
    let mut hit = |class: MutationClass, rule: String| {
        let entry = ForbiddenHit { stage, class, rule };
        if !hits.contains(&entry) {
            hits.push(entry);
        }
    };
    if let Some(item_type) = view.item_type {
        if forbidden.item_types.contains(&item_type) {
            hit(
                MutationClass::Classification,
                format!("item type {}", item_type.as_str()),
            );
        }
        if forbidden.facets == Some(FacetBan::Any) {
            hit(MutationClass::Classification, "any facet".to_string());
        }
    }
    if let Some(reminder) = &view.reminder {
        match forbidden.reminder {
            Some(ReminderBan::Any) => hit(MutationClass::Reminder, "any reminder".to_string()),
            Some(ReminderBan::AnyInstant) if reminder.instant.is_some() => {
                hit(MutationClass::Reminder, "any reminder instant".to_string())
            }
            _ => {}
        }
        if let Some(quality) = reminder.quality {
            if forbidden.reminder_qualities.contains(&quality) {
                hit(
                    MutationClass::Reminder,
                    format!("reminder quality {}", quality.as_str()),
                );
            }
        }
        if let Some(zone) = &reminder.timezone_id {
            // A stored record without an instant schedules nothing; its zone is only the display
            // zone of a pending correction.
            let can_fire = stage == Stage::Candidate || reminder.instant.is_some();
            if can_fire && forbidden.reminder_timezones.contains(zone) {
                hit(MutationClass::Reminder, format!("reminder timezone {zone}"));
            }
        }
        if forbidden.facets == Some(FacetBan::Any) {
            hit(MutationClass::Reminder, "any facet".to_string());
        }
    }
    if view.session_topic.is_some() {
        if forbidden.session_topic {
            hit(MutationClass::SessionTopic, "session topic".to_string());
        }
        if forbidden.facets == Some(FacetBan::Any) {
            hit(MutationClass::SessionTopic, "any facet".to_string());
        }
    }
    match view.requested_operation {
        Some(RequestedOperation::Update)
            if forbidden.operations.contains(&ForbiddenOperation::Update) =>
        {
            hit(MutationClass::Operation, "update operation".to_string())
        }
        Some(RequestedOperation::Create)
            if forbidden.operations.contains(&ForbiddenOperation::Create) =>
        {
            hit(MutationClass::Operation, "create operation".to_string())
        }
        _ => {}
    }
    hits
}

/// Hits that hold for every fixture: no pipeline path may complete an item, alter what the user
/// saved, or create or touch any item other than the one evaluated.
pub fn invariant_hits(state: &AuthoritativeState) -> Vec<ForbiddenHit> {
    let mut hits = Vec::new();
    let stage = Stage::Authoritative;
    if !state.is_active() {
        hits.push(ForbiddenHit {
            stage,
            class: MutationClass::Lifecycle,
            rule: format!("item left the active state ({})", state.lifecycle),
        });
    }
    if !state.foreign_records.is_empty() {
        let found = state
            .foreign_records
            .iter()
            .map(|(table, rows)| format!("{rows} in {table}"))
            .collect::<Vec<_>>()
            .join(", ");
        hits.push(ForbiddenHit {
            stage,
            class: MutationClass::Operation,
            rule: format!("records beyond the evaluated item's own appeared ({found})"),
        });
    }
    if !state.raw_capture_intact {
        hits.push(ForbiddenHit {
            stage,
            class: MutationClass::SourceText,
            rule: "the raw capture changed".to_string(),
        });
    }
    if !state.correction_preserved {
        hits.push(ForbiddenHit {
            stage,
            class: MutationClass::SourceText,
            rule: "the user's correction changed".to_string(),
        });
    }
    hits
}
