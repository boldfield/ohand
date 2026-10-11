//! What the two arms do and do not have in common, from the journal's own evidence.
//!
//! A run pins the requested settings, the instruction and the synthetic corpus for both arms, but
//! it cannot make the arms differ in model identity alone: protocols shape the wire request,
//! providers may ignore or default a setting, and a provider may answer with another model than
//! the pinned one. The report therefore never claims that model identity was the only variable;
//! it lists the evidence that bears on the question and flags what weakens it.

use std::collections::{BTreeMap, BTreeSet};

use evaluation::comparison::ComparisonReport;
use evaluation::report::ExecutionKind;
use ohand_core::providers::contracts::{EffectiveSetting, SettingValue};
use serde::Serialize;

use super::load::Journal;
use super::name_of;

/// Fewer matched pairs than this is a small sample for any reading beyond the individual cases.
pub const SMALL_SAMPLE_PAIRS: u64 = 30;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ArmSettings {
    pub arm: &'static str,
    /// From the provenance of the arm's attempts that reported metadata.
    pub protocol: Option<String>,
    /// Attempts that reported diagnostic metadata (completed attempts only).
    pub attempts_with_metadata: u64,
    /// Dispatched attempts with no diagnostic metadata (failed, timed out or not reported):
    /// their protocol, requested and effective settings are unverified.
    pub attempts_without_metadata: u64,
    /// The settings the attempts requested, as `name=value`.
    pub requested: Vec<String>,
    pub effective_confirmed: u64,
    pub effective_unknown: u64,
    pub effective_differs: u64,
    /// Attempts with a setting that was not requested and so ran on the provider's default.
    pub attempts_using_provider_defaults: u64,
    /// Attempts by how the reported model relates to the pinned one.
    pub reported_model: BTreeMap<String, u64>,
    /// Attempts whose provenance names another profile version than the experiment pinned.
    pub profile_version_mismatches: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Flag {
    pub code: &'static str,
    pub count: u64,
    pub detail: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparability {
    /// Always `not_established`: no journal can show that model identity was the only variable.
    pub model_identity_only_variable: &'static str,
    pub arms: [ArmSettings; 2],
    pub flags: Vec<Flag>,
}

fn setting_label(value: &SettingValue) -> String {
    match value {
        SettingValue::TemperatureMilli(milli) => format!("temperature_milli={milli}"),
        SettingValue::MaxOutputTokens(tokens) => format!("max_output_tokens={tokens}"),
        SettingValue::Seed(seed) => format!("seed={seed}"),
    }
}

fn arm_settings(journal: &Journal, arm: usize) -> ArmSettings {
    let pinned_version = if arm == 0 {
        &journal.experiment.profile_a_version
    } else {
        &journal.experiment.profile_b_version
    };
    let mut settings = ArmSettings {
        arm: if arm == 0 { "a" } else { "b" },
        ..ArmSettings::default()
    };
    let mut requested = BTreeSet::new();
    let mut protocols = BTreeSet::new();
    for attempt in journal.dispatched_attempts(arm) {
        let Some(metadata) = &attempt.envelope.metadata else {
            settings.attempts_without_metadata += 1;
            continue;
        };
        settings.attempts_with_metadata += 1;
        protocols.insert(name_of(&metadata.provenance.protocol));
        *settings
            .reported_model
            .entry(name_of(&metadata.provenance.reported_model))
            .or_default() += 1;
        if &metadata.provenance.profile_version != pinned_version {
            settings.profile_version_mismatches += 1;
        }
        let mut used_default = false;
        for outcome in &metadata.settings {
            let Some(asked) = &outcome.requested else {
                used_default = true;
                continue;
            };
            requested.insert(setting_label(asked));
            match &outcome.effective {
                EffectiveSetting::Reported { value } if value == asked => {
                    settings.effective_confirmed += 1
                }
                EffectiveSetting::Reported { .. } => settings.effective_differs += 1,
                EffectiveSetting::Unknown => settings.effective_unknown += 1,
            }
        }
        settings.attempts_using_provider_defaults += u64::from(used_default);
    }
    settings.requested = requested.into_iter().collect();
    settings.protocol = match protocols.len() {
        0 => None,
        1 => protocols.into_iter().next(),
        _ => Some("mixed".to_string()),
    };
    settings
}

pub(super) fn assess(
    journal: &Journal,
    scored: &ComparisonReport,
    execution: ExecutionKind,
) -> Comparability {
    let arms = [arm_settings(journal, 0), arm_settings(journal, 1)];
    let mut flags = Vec::new();
    let mut flag = |code: &'static str, count: u64, detail: &'static str| {
        if count > 0 {
            flags.push(Flag {
                code,
                count,
                detail,
            });
        }
    };

    if execution == ExecutionKind::Fake {
        flag(
            "fake_execution",
            scored.pairs.eligible,
            "scripted synthetic replies: this run exercises the pipeline and says nothing about any model",
        );
    }
    if scored.pairs.matched < SMALL_SAMPLE_PAIRS {
        flag(
            "small_sample",
            scored.pairs.matched,
            "fewer than 30 matched pairs: read individual cases, not rates",
        );
    }
    flag(
        "incomplete_pairs",
        scored.pairs.eligible - scored.pairs.matched,
        "eligible cases where at least one arm has no response; they are counted, never dropped or treated as abstentions",
    );
    let different_protocols = matches!(
        (&arms[0].protocol, &arms[1].protocol),
        (Some(a), Some(b)) if a != b
    );
    flag(
        "different_protocols",
        u64::from(different_protocols),
        "the arms use different provider protocols, whose request wording and structured-output handling are adapter-specific",
    );
    flag(
        "protocol_unverified",
        arms.iter()
            .zip([0, 1])
            .filter(|(arm, index)| {
                arm.protocol.is_none() && journal.dispatched_attempts(*index).next().is_some()
            })
            .count() as u64,
        "arms that dispatched requests but reported no provider protocol, so protocol comparability is unverified",
    );
    flag(
        "settings_evidence_missing",
        arms.iter().map(|arm| arm.attempts_without_metadata).sum(),
        "dispatched attempts without diagnostic metadata: their requested and effective settings are unverified and the arms' settings cannot be compared",
    );
    flag(
        "requested_settings_differ",
        u64::from(
            arms.iter().all(|arm| arm.attempts_with_metadata > 0)
                && arms[0].requested != arms[1].requested,
        ),
        "the arms requested different settings",
    );
    flag(
        "effective_settings_unknown",
        arms.iter().map(|arm| arm.effective_unknown).sum(),
        "the provider did not confirm a requested setting, so it may not have been applied",
    );
    flag(
        "effective_settings_differ",
        arms.iter().map(|arm| arm.effective_differs).sum(),
        "the provider reported a different effective value than requested",
    );
    flag(
        "provider_defaults_in_use",
        arms.iter()
            .map(|arm| arm.attempts_using_provider_defaults)
            .sum(),
        "settings that were not requested ran on each provider's own default, which may differ between arms",
    );
    let reported = |key: &str| -> u64 {
        arms.iter()
            .map(|arm| arm.reported_model.get(key).copied().unwrap_or(0))
            .sum()
    };
    flag(
        "reported_model_differs",
        reported("different"),
        "the provider named a model other than the pinned one",
    );
    flag(
        "reported_model_not_reported",
        reported("not_reported"),
        "the provider named no model, so the served model is unverified",
    );
    flag(
        "profile_version_mismatch",
        arms.iter().map(|arm| arm.profile_version_mismatches).sum(),
        "an attempt's provenance names another profile version than the experiment pinned",
    );

    let mut not_derived = 0;
    let mut conflicting = 0;
    for case_index in 0..journal.cases.len() {
        let mut fingerprints = BTreeSet::new();
        for attempt in journal
            .attempts
            .iter()
            .filter(|attempt| attempt.case_index == case_index)
        {
            match &attempt.envelope.pair_fingerprint {
                Some(fingerprint) => {
                    fingerprints.insert(fingerprint.clone());
                }
                None if attempt.envelope.dispatched => not_derived += 1,
                None => {}
            }
        }
        conflicting += u64::from(fingerprints.len() > 1);
    }
    flag(
        "pairs_not_derived",
        not_derived,
        "dispatched attempts without a request fingerprint: equal inputs for the two arms are unverified",
    );
    flag(
        "request_fingerprint_conflict",
        conflicting,
        "attempts of one case carry different request fingerprints: the arms or runs did not see the same request",
    );
    let reruns: u64 = (0..2)
        .map(|arm| {
            journal
                .dispatched_attempts(arm)
                .filter(|attempt| attempt.envelope.rerun_of.is_some())
                .count() as u64
        })
        .sum();
    flag(
        "reruns_present",
        reruns,
        "a rerun replaced an earlier attempt in this report; the earlier attempt stays in the journal",
    );
    flag(
        "unknown_experiment_metadata",
        journal.experiment.unknown_metadata.unknown_fields.len() as u64,
        "the experiment record lists fields that are unknown or unsupported",
    );
    flag(
        "journal_tail_unreadable",
        u64::from(journal.torn_tail),
        "the journal ends in an incomplete record, which this report ignored and left in place",
    );
    flag(
        "outcome_unknown_attempts",
        journal.ambiguous_started_attempts as u64,
        "requests were reserved but never given an outcome; the provider may have received and billed them",
    );
    Comparability {
        model_identity_only_variable: "not_established",
        arms,
        flags,
    }
}
