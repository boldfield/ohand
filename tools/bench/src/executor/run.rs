use std::collections::HashSet;
use std::path::Path;

use ohand_core::interpretation::instructions::content_version;
use ohand_core::privacy::routing::{Authorization, AuthorizationDecision, ProcessingCapability};
use ohand_core::providers::contracts::{
    dispatch_diagnostic, DiagnosticFailure, DiagnosticFailureKind, DiagnosticOutput, FailureKind,
    ProviderProfile, RejectionDetail, UsageAvailability,
};
use ohand_core::review::compare::{derive_pair, ArmInput, ArmLabel, PairedRequests};

use super::envelope::{class, Envelope, Phase, RejectionRecord};
use super::error::{ConfigurationChange, ExecutorError};
use super::plan::{
    ArmBinding, AuthorizationQuery, ExecutorLimits, PlannedCase, RunControls, RunRequest,
};
use super::report::{CaseReport, ConfigurationStatus, RunReport, SideKind, SideReport};
use super::state::{arm_key, AttemptEntry, JournalState};
use crate::journal::{JournalError, JournalReader, JournalWriter};
use crate::records::{Attempt, AttemptState, Case, Experiment, UsageMetadata};

const UNKNOWN_REASON: &str =
    "interrupted after the request was reserved; remote completion unknown";

/// Runs paired cases through the two arms and records everything in one journal.
pub struct Executor {
    writer: JournalWriter,
    experiment: Experiment,
    state: JournalState,
}

struct Prepared<'p> {
    planned: &'p PlannedCase,
    position: usize,
    record_id: String,
    new_case: Option<Case>,
    pair: Result<PairedRequests, String>,
    fingerprint: Option<String>,
}

#[derive(Default)]
struct Meter {
    reserved: u32,
    exhausted: bool,
    cancelled: bool,
}

/// The first arm of every even-numbered case is A and of every odd-numbered case is B, so each
/// arm leads about half of the cases and a shared endpoint is never loaded by both at once.
fn counterbalanced_order(position: usize) -> [ArmLabel; 2] {
    if position.is_multiple_of(2) {
        [ArmLabel::A, ArmLabel::B]
    } else {
        [ArmLabel::B, ArmLabel::A]
    }
}

fn experiment_differences(stored: &Experiment, requested: &Experiment) -> Vec<&'static str> {
    let checks = [
        (
            "schema_version",
            stored.schema_version == requested.schema_version,
        ),
        (
            "corpus_version",
            stored.corpus_version == requested.corpus_version,
        ),
        (
            "source_context",
            stored.source_context == requested.source_context,
        ),
        (
            "instruction_version",
            stored.instruction_version == requested.instruction_version,
        ),
        (
            "profile_a_id",
            stored.profile_a_id == requested.profile_a_id,
        ),
        (
            "profile_a_version",
            stored.profile_a_version == requested.profile_a_version,
        ),
        (
            "profile_b_id",
            stored.profile_b_id == requested.profile_b_id,
        ),
        (
            "profile_b_version",
            stored.profile_b_version == requested.profile_b_version,
        ),
        (
            "build_revision",
            stored.build_revision == requested.build_revision,
        ),
        (
            "unknown_metadata",
            stored.unknown_metadata == requested.unknown_metadata,
        ),
    ];
    checks
        .into_iter()
        .filter(|(_, same)| !same)
        .map(|(field, _)| field)
        .collect()
}

fn pair_fingerprint(pair: &PairedRequests) -> String {
    let value = serde_json::json!({
        "a": pair.first.request(),
        "b": pair.second.request(),
    });
    content_version(&value.to_string())
}

fn sorted(origins: &[String]) -> Vec<&String> {
    let mut origins: Vec<&String> = origins.iter().collect();
    origins.sort();
    origins
}

/// A fresh decision must still be a remote text-interpretation grant for this arm's own profile
/// and route, to the same destinations the pair was derived under.
fn authorization_still_valid(
    decision: &AuthorizationDecision,
    pinned: &Authorization,
    profile: &ProviderProfile,
    route_id: &str,
) -> Result<(), String> {
    let current = match decision {
        AuthorizationDecision::Denied(reason) => {
            return Err(format!("authorization denied: {reason}"))
        }
        AuthorizationDecision::Authorized(current) => current,
    };
    if current.is_local() || current.capability() != ProcessingCapability::TextInterpretation {
        return Err("authorization is not a remote text-interpretation grant".to_string());
    }
    if current.profile_version() != Some(profile.profile_version()) {
        return Err("authorization is for a different profile version".to_string());
    }
    if current.route_id() != route_id {
        return Err("authorization is for a different route".to_string());
    }
    if current.destination_class() != pinned.destination_class()
        || sorted(current.destinations()) != sorted(pinned.destinations())
    {
        return Err("authorized destinations changed since the pair was derived".to_string());
    }
    Ok(())
}

/// The provider's own words about a rejection follow the status and error type, so a bound on
/// displayed text cuts the message and not the facts.
fn with_rejection_detail(
    base: &str,
    rejection: Option<&RejectionDetail>,
    include_message: bool,
) -> String {
    let Some(rejection) = rejection else {
        return base.to_string();
    };
    let mut text = format!("{base}: HTTP {}", rejection.status);
    if let Some(error_type) = &rejection.error_type {
        text.push(' ');
        text.push_str(error_type);
    }
    if include_message {
        if let Some(message) = &rejection.message {
            text.push_str(": ");
            text.push_str(message);
        }
    }
    text
}

fn failure_kind_key(kind: FailureKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "provider_error".to_string())
}

impl Executor {
    /// Open or create the journal for `experiment`. Reopening a journal whose experiment
    /// configuration differs fails and leaves it untouched, so results of different
    /// configurations are never merged. The journal's own experiment id is kept.
    pub fn open(path: impl AsRef<Path>, experiment: Experiment) -> Result<Self, ExecutorError> {
        experiment.validate().map_err(JournalError::Validation)?;
        let path = path.as_ref();
        let mut writer = JournalWriter::open(path)?;
        let reader = JournalReader::open(path)?;
        let stored = reader.experiments();
        match stored.as_slice() {
            [] => {
                if !reader.cases().is_empty() || !reader.attempts().is_empty() {
                    return Err(ExecutorError::InconsistentJournal);
                }
                writer.write_experiment(&experiment)?;
                Ok(Executor {
                    writer,
                    experiment,
                    state: JournalState::empty(),
                })
            }
            [stored] => {
                let differing = experiment_differences(stored, &experiment);
                if !differing.is_empty() {
                    return Err(ExecutorError::ConfigurationChanged(
                        ConfigurationChange::Experiment { fields: differing },
                    ));
                }
                let state = JournalState::load(&reader, stored)?;
                Ok(Executor {
                    writer,
                    experiment: (*stored).clone(),
                    state,
                })
            }
            _ => Err(ExecutorError::InconsistentJournal),
        }
    }

    pub fn experiment(&self) -> &Experiment {
        &self.experiment
    }

    /// Run every planned case through both arms, sequentially. Sides that already have a
    /// dispatched attempt are preserved untouched unless named in `reruns`; sides that are
    /// missing or were skipped are sent. Nothing is written until every case has been checked
    /// against the journal's configuration.
    pub fn run(&mut self, request: RunRequest<'_>) -> Result<RunReport, ExecutorError> {
        self.check_bindings(&request)?;
        let prepared = self.prepare(&request)?;
        self.check_reruns(&request, &prepared)?;
        for entry in &prepared {
            if let Some(case) = &entry.new_case {
                self.writer.write_case(case)?;
                self.state.insert_case(case);
            }
        }

        let run = self.state.runs + 1;
        let mut meter = Meter::default();
        for entry in &prepared {
            let order = self
                .state
                .recorded_order(&entry.record_id)
                .unwrap_or_else(|| counterbalanced_order(entry.position));
            for arm in order {
                let rerun = request
                    .reruns
                    .iter()
                    .any(|key| key.case_id == entry.planned.case_id && key.arm == arm);
                if !rerun && self.state.dispatched_count(&entry.record_id, arm) > 0 {
                    continue;
                }
                let binding = match arm {
                    ArmLabel::A => &request.arm_a,
                    ArmLabel::B => &request.arm_b,
                };
                self.execute_side(
                    &SideTask {
                        prepared: entry,
                        arm,
                        order,
                        rerun,
                        run,
                    },
                    binding,
                    &request.controls,
                    &request.limits,
                    &mut meter,
                )?;
            }
        }
        Ok(self.build_report(&prepared, run, &request.limits, &meter))
    }

    fn check_bindings(&self, request: &RunRequest<'_>) -> Result<(), ExecutorError> {
        let experiment = &self.experiment;
        let (a, b) = (request.arm_a.profile, request.arm_b.profile);
        if a.profile_id() != experiment.profile_a_id
            || a.profile_version() != experiment.profile_a_version
        {
            return Err(ExecutorError::BindingMismatch("arm a"));
        }
        if b.profile_id() != experiment.profile_b_id
            || b.profile_version() != experiment.profile_b_version
        {
            return Err(ExecutorError::BindingMismatch("arm b"));
        }
        Ok(())
    }

    fn prepare<'p>(&self, request: &RunRequest<'p>) -> Result<Vec<Prepared<'p>>, ExecutorError> {
        let mut seen = HashSet::new();
        let mut prepared = Vec::with_capacity(request.cases.len());
        for (position, planned) in request.cases.iter().enumerate() {
            if !seen.insert(planned.case_id.as_str()) {
                return Err(ExecutorError::DuplicateCase(planned.case_id.clone()));
            }
            if planned.source.instruction_version() != self.experiment.instruction_version {
                return Err(ExecutorError::InstructionVersionMismatch);
            }
            let (record_id, new_case) = match self.state.case(&planned.case_id) {
                Some(journaled) => {
                    if journaled.content != planned.content {
                        return Err(ExecutorError::ConfigurationChanged(
                            ConfigurationChange::CaseContent {
                                case_id: planned.case_id.clone(),
                            },
                        ));
                    }
                    (journaled.record_id.clone(), None)
                }
                None => {
                    let case = Case::new(
                        self.experiment.id.clone(),
                        planned.case_id.clone(),
                        planned.content.clone(),
                    )
                    .map_err(ExecutorError::InvalidCase)?;
                    (case.id.clone(), Some(case))
                }
            };

            let pair = self.derive(request, planned);
            let fingerprint = pair.as_ref().ok().map(pair_fingerprint);
            if let Some(current) = &fingerprint {
                if self.state.conflicting_fingerprint(&record_id, current) {
                    return Err(ExecutorError::ConfigurationChanged(
                        ConfigurationChange::Request {
                            case_id: planned.case_id.clone(),
                        },
                    ));
                }
            }
            prepared.push(Prepared {
                planned,
                position,
                record_id,
                new_case,
                pair,
                fingerprint,
            });
        }
        Ok(prepared)
    }

    /// Derive both arms from the frozen case, each under its own fresh authorization.
    fn derive(
        &self,
        request: &RunRequest<'_>,
        planned: &PlannedCase,
    ) -> Result<PairedRequests, String> {
        let route_id = planned.source.route_id();
        let decide = |arm: ArmLabel, binding: &ArmBinding<'_>| {
            binding.authorizer.authorize(&AuthorizationQuery {
                arm,
                case_id: &planned.case_id,
                route_id,
                profile_version: binding.profile.profile_version(),
            })
        };
        let decision_a = decide(ArmLabel::A, &request.arm_a);
        let decision_b = decide(ArmLabel::B, &request.arm_b);
        let support_a = request.arm_a.adapter.diagnostic_support();
        let support_b = request.arm_b.adapter.diagnostic_support();
        derive_pair(
            &planned.source,
            ArmInput {
                profile: request.arm_a.profile,
                authorization: &decision_a,
                adapter_support: &support_a,
            },
            ArmInput {
                profile: request.arm_b.profile,
                authorization: &decision_b,
                adapter_support: &support_b,
            },
        )
        .map_err(|error| error.to_string())
    }

    fn check_reruns(
        &self,
        request: &RunRequest<'_>,
        prepared: &[Prepared<'_>],
    ) -> Result<(), ExecutorError> {
        for key in request.reruns {
            let Some(entry) = prepared
                .iter()
                .find(|entry| entry.planned.case_id == key.case_id)
            else {
                return Err(ExecutorError::UnknownRerunTarget {
                    case_id: key.case_id.clone(),
                });
            };
            if self.state.dispatched_count(&entry.record_id, key.arm) == 0 {
                return Err(ExecutorError::NothingToRerun {
                    case_id: key.case_id.clone(),
                });
            }
        }
        Ok(())
    }

    fn execute_side(
        &mut self,
        task: &SideTask<'_, '_>,
        binding: &ArmBinding<'_>,
        controls: &RunControls<'_>,
        limits: &ExecutorLimits,
        meter: &mut Meter,
    ) -> Result<(), ExecutorError> {
        let prepared = task.prepared;
        let paired = match &prepared.pair {
            Ok(paired) => paired,
            Err(reason) => {
                return self.record_skip(task, class::NOT_ELIGIBLE, reason.clone());
            }
        };
        let paired_arm = match task.arm {
            ArmLabel::A => &paired.first,
            ArmLabel::B => &paired.second,
        };
        let route_id = prepared.planned.source.route_id();

        let mut follows: Link = match self
            .state
            .latest_dispatched(&prepared.record_id, task.arm)
            .map(|entry| entry.id.clone())
        {
            Some(previous) if task.rerun => Link::Rerun(previous),
            _ => Link::None,
        };
        for side_attempt in 1..=limits.attempts_per_side() {
            if controls.cancel.is_cancelled() {
                meter.cancelled = true;
                return self.record_skip(
                    task,
                    class::CANCELLED,
                    "run cancelled before the request was reserved".to_string(),
                );
            }
            if meter.reserved >= limits.request_cap() {
                meter.exhausted = true;
                return self.record_skip(
                    task,
                    class::BUDGET_EXHAUSTED,
                    "the run's request allowance is spent".to_string(),
                );
            }
            let decision = binding.authorizer.authorize(&AuthorizationQuery {
                arm: task.arm,
                case_id: &prepared.planned.case_id,
                route_id,
                profile_version: binding.profile.profile_version(),
            });
            if let Err(reason) = authorization_still_valid(
                &decision,
                paired_arm.authorization(),
                binding.profile,
                route_id,
            ) {
                return self.record_skip(task, class::NOT_AUTHORIZED, reason);
            }

            meter.reserved += 1;
            let mut attempt = Attempt::new(prepared.record_id.clone(), arm_key(task.arm))
                .map_err(JournalError::Validation)?;
            let mut envelope = self.envelope(Phase::Reserved, true, task);
            envelope.reservation = Some(self.state.reservations + 1);
            envelope.side_attempt = Some(side_attempt);
            follows.apply(&mut envelope);
            attempt.provider_output = Some(envelope.to_value());
            self.write_attempt(&attempt, &envelope)?;

            let result = dispatch_diagnostic(
                binding.adapter,
                binding.profile,
                paired_arm.request(),
                controls.clock,
                controls.cancel,
                &controls.dispatch_limits,
            );
            let retry = self.finish_attempt(attempt.clone(), envelope, result)?;
            if !retry {
                return Ok(());
            }
            follows = Link::Retry(attempt.id);
        }
        Ok(())
    }

    fn envelope(&self, phase: Phase, dispatched: bool, task: &SideTask<'_, '_>) -> Envelope {
        let mut envelope = Envelope::new(
            phase,
            dispatched,
            task.run,
            task.order
                .iter()
                .map(|arm| arm_key(*arm).to_string())
                .collect(),
        );
        envelope.pair_fingerprint = task.prepared.fingerprint.clone();
        envelope
    }

    /// Journal that nothing was sent for this side and why.
    fn record_skip(
        &mut self,
        task: &SideTask<'_, '_>,
        skip_class: &'static str,
        detail: String,
    ) -> Result<(), ExecutorError> {
        let mut attempt = Attempt::new(task.prepared.record_id.clone(), arm_key(task.arm))
            .map_err(JournalError::Validation)?;
        let mut envelope = self.envelope(Phase::Skipped, false, task);
        envelope.class = Some(skip_class.to_string());
        envelope.detail = Some(detail.clone());
        attempt.state = AttemptState::Failed;
        attempt.failure_reason = Some(detail);
        attempt.provider_output = Some(envelope.to_value());
        self.write_attempt(&attempt, &envelope)
    }

    /// Append the record and, once it is durable, update the in-memory view.
    fn write_attempt(
        &mut self,
        attempt: &Attempt,
        envelope: &Envelope,
    ) -> Result<(), ExecutorError> {
        self.writer.write_attempt(attempt)?;
        self.state.upsert_attempt(AttemptEntry {
            id: attempt.id.clone(),
            case_record_id: attempt.case_id.clone(),
            arm: if attempt.arm == "a" {
                ArmLabel::A
            } else {
                ArmLabel::B
            },
            state: attempt.state,
            envelope: envelope.clone(),
        });
        Ok(())
    }

    /// Record the terminal outcome of a reserved attempt. Returns whether the failure may be
    /// retried within this side's bound.
    fn finish_attempt(
        &mut self,
        mut attempt: Attempt,
        mut envelope: Envelope,
        result: Result<DiagnosticOutput, DiagnosticFailure>,
    ) -> Result<bool, ExecutorError> {
        match result {
            Ok(output) => {
                let metadata = output.metadata;
                attempt.state = AttemptState::Completed;
                attempt.elapsed_ms = Some(metadata.elapsed_ms);
                attempt.usage_tokens = usage_of(&metadata.usage);
                envelope.phase = Phase::Completed;
                envelope.output = Some(output.output.proposal);
                envelope.metadata = Some(metadata.clone());
                attempt.provider_output = Some(envelope.to_value());
                match self.write_attempt(&attempt, &envelope) {
                    Ok(()) => Ok(false),
                    Err(ExecutorError::Journal(JournalError::Validation(_))) => {
                        // The journal refuses output that looks like a credential or private
                        // endpoint; keep the safe metadata and say why there is no output.
                        envelope.output = None;
                        self.fail_attempt(
                            attempt,
                            envelope,
                            class::OUTPUT_REJECTED,
                            "provider output was rejected by the journal guard".to_string(),
                            Some(metadata.usage),
                        )?;
                        Ok(false)
                    }
                    Err(other) => Err(other),
                }
            }
            Err(failure) => {
                let usage = failure.usage;
                let (failure_class, detail, retriable) = match &failure.kind {
                    DiagnosticFailureKind::DiagnosticsUnsupported => (
                        class::UNSUPPORTED.to_string(),
                        "adapter does not support diagnostic calls".to_string(),
                        false,
                    ),
                    DiagnosticFailureKind::UnsupportedSettings { .. } => (
                        class::UNSUPPORTED.to_string(),
                        "adapter cannot honor the requested settings".to_string(),
                        false,
                    ),
                    DiagnosticFailureKind::Provider { failure: provider } => match provider.kind {
                        FailureKind::Timeout => (
                            class::TIMED_OUT.to_string(),
                            "timed out; whether the provider completed the call is unknown"
                                .to_string(),
                            provider.retriable,
                        ),
                        FailureKind::Cancelled => (
                            class::CANCELLED.to_string(),
                            "cancelled while the request was in flight".to_string(),
                            false,
                        ),
                        kind => (
                            failure_kind_key(kind),
                            with_rejection_detail(&provider.message, failure.detail.as_ref(), true),
                            provider.retriable,
                        ),
                    },
                };
                if let Some(rejection) = &failure.detail {
                    envelope.rejection = Some(RejectionRecord {
                        status: rejection.status,
                        error_type: rejection.error_type.clone(),
                    });
                }
                let retained = (attempt.clone(), envelope.clone());
                match self.fail_attempt(attempt, envelope, &failure_class, detail, Some(usage)) {
                    // The journal refuses text that looks like a credential or private endpoint.
                    // The provider's wording is the only part it can object to, so keep the
                    // status and error type and drop the message.
                    Err(ExecutorError::Journal(JournalError::Validation(_)))
                        if failure.detail.is_some() =>
                    {
                        let base = match &failure.kind {
                            DiagnosticFailureKind::Provider { failure: provider } => {
                                provider.message.clone()
                            }
                            _ => String::new(),
                        };
                        let detail = with_rejection_detail(&base, failure.detail.as_ref(), false);
                        let (attempt, envelope) = retained;
                        self.fail_attempt(attempt, envelope, &failure_class, detail, Some(usage))?;
                    }
                    other => other?,
                }
                Ok(retriable)
            }
        }
    }

    fn fail_attempt(
        &mut self,
        mut attempt: Attempt,
        mut envelope: Envelope,
        failure_class: &str,
        detail: String,
        usage: Option<UsageAvailability>,
    ) -> Result<(), ExecutorError> {
        attempt.state = AttemptState::Failed;
        attempt.failure_reason = Some(detail.clone());
        if let Some(usage) = &usage {
            attempt.usage_tokens = usage_of(usage);
        }
        envelope.phase = Phase::Failed;
        envelope.class = Some(failure_class.to_string());
        envelope.detail = Some(detail);
        envelope.usage = usage;
        attempt.provider_output = Some(envelope.to_value());
        self.write_attempt(&attempt, &envelope)
    }

    fn build_report(
        &self,
        prepared: &[Prepared<'_>],
        run: u32,
        limits: &ExecutorLimits,
        meter: &Meter,
    ) -> RunReport {
        let cases = prepared
            .iter()
            .map(|entry| {
                let order = self
                    .state
                    .recorded_order(&entry.record_id)
                    .unwrap_or_else(|| counterbalanced_order(entry.position));
                let side = |arm: ArmLabel| self.side_report(&entry.record_id, arm, run);
                CaseReport {
                    case_id: entry.planned.case_id.clone(),
                    arm_order: order,
                    sides: [side(order[0]), side(order[1])],
                    configuration: match &entry.pair {
                        Ok(_) => ConfigurationStatus::Verified,
                        Err(reason) => ConfigurationStatus::Unverified {
                            reason: reason.clone(),
                        },
                    },
                    comparability: entry
                        .pair
                        .as_ref()
                        .ok()
                        .map(|pair| pair.comparability.clone()),
                }
            })
            .collect();
        RunReport {
            experiment_id: self.experiment.id.clone(),
            run,
            request_cap: limits.request_cap(),
            reserved_requests: meter.reserved,
            budget_exhausted: meter.exhausted,
            cancelled: meter.cancelled,
            cases,
        }
    }

    fn side_report(&self, record_id: &str, arm: ArmLabel, run: u32) -> SideReport {
        let dispatched_attempts = self.state.dispatched_count(record_id, arm);
        let dispatched_this_run = self.state.dispatched_in_run(record_id, arm, run);
        let mut report = SideReport {
            arm,
            kind: SideKind::Missing,
            class: None,
            reason: Some("no attempt recorded".to_string()),
            attempt_id: None,
            dispatched_attempts,
            dispatched_this_run,
            rerun_of: None,
            output: None,
            metadata: None,
        };
        if let Some(latest) = self.state.latest_dispatched(record_id, arm) {
            let envelope = &latest.envelope;
            report.attempt_id = Some(latest.id.clone());
            report.rerun_of = envelope.rerun_of.clone();
            report.class = envelope.class.clone();
            report.reason = envelope.detail.clone();
            report.metadata = envelope.metadata.clone();
            match latest.state {
                AttemptState::Completed => {
                    report.kind = SideKind::Completed;
                    report.reason = None;
                    report.output = envelope.output.clone();
                }
                AttemptState::Failed => {
                    report.kind = match envelope.class.as_deref() {
                        Some(class::TIMED_OUT) => SideKind::TimedOut,
                        Some(class::CANCELLED) => SideKind::Cancelled,
                        _ => SideKind::Failed,
                    };
                }
                AttemptState::Started | AttemptState::Unknown => {
                    report.kind = SideKind::Unknown;
                    report.reason = Some(UNKNOWN_REASON.to_string());
                }
            }
        } else if let Some(skip) = self.state.latest_skip(record_id, arm) {
            report.class = skip.envelope.class.clone();
            report.reason = skip.envelope.detail.clone();
            report.kind = match skip.envelope.class.as_deref() {
                Some(class::CANCELLED) => SideKind::Cancelled,
                _ => SideKind::Skipped,
            };
        }
        report
    }
}

struct SideTask<'a, 'p> {
    prepared: &'a Prepared<'p>,
    arm: ArmLabel,
    order: [ArmLabel; 2],
    rerun: bool,
    run: u32,
}

/// How an attempt relates to the one before it on the same side.
enum Link {
    None,
    Retry(String),
    Rerun(String),
}

impl Link {
    fn apply(&self, envelope: &mut Envelope) {
        match self {
            Link::None => {}
            Link::Retry(previous) => envelope.retry_of = Some(previous.clone()),
            Link::Rerun(previous) => envelope.rerun_of = Some(previous.clone()),
        }
    }
}

fn usage_of(usage: &UsageAvailability) -> Option<UsageMetadata> {
    match usage {
        UsageAvailability::Reported { usage } => Some(UsageMetadata {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        }),
        UsageAvailability::Unavailable => None,
    }
}
