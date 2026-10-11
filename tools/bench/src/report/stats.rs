//! Per-arm request counts, latency, token usage and cost, from the journal's attempt records.
//!
//! Latency and the "matched" usage figures cover only the attempts that completed a matched
//! pair, so the two arms are summarized over the same cases. Usage and cost over every
//! dispatched attempt show what was spent. A missing figure is unknown and is never added as
//! zero.

use std::collections::HashSet;

use evaluation::comparison::Label;
use serde::Serialize;

use super::load::{AttemptRecord, Journal, Side};
use super::rates::{CostRates, ProfileRate};
use crate::records::AttemptState;

const MICROS_PER_MILLION: u128 = 1_000_000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct RequestCounts {
    /// Requests reserved for dispatch on this side, retries and reruns included.
    pub dispatched: u64,
    pub completed: u64,
    pub failed: u64,
    /// Reserved but never given an outcome; the provider may have received them.
    pub outcome_unknown: u64,
    pub retries: u64,
    pub reruns: u64,
}

/// Wall-clock time of completed attempts, measured by the client. `median_ms` is the lower
/// median. Attempts that did not complete have no recorded duration and are not counted.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LatencySummary {
    pub samples: u64,
    /// Completed attempts that recorded no duration.
    pub unknown: u64,
    pub min_ms: Option<u64>,
    pub median_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub total_ms: Option<u64>,
}

/// Provider-reported tokens. A total is present only when at least one attempt reported that
/// count, and `*_attempts` says how many did: the rest are unknown, not zero.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageSummary {
    pub attempts: u64,
    pub attempts_with_usage: u64,
    pub attempts_without_usage: u64,
    pub input_tokens: Option<u64>,
    pub input_tokens_attempts: u64,
    pub output_tokens: Option<u64>,
    pub output_tokens_attempts: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CostProvenance {
    pub kind: &'static str,
    pub rates_version: Label,
    pub currency: Label,
    pub profile_version: String,
    pub basis: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CostSummary {
    /// No cost is claimed. Never zero.
    Unknown { reason: &'static str },
    /// An estimate from supplied rates and reported tokens. It is never a billed cost.
    Estimated {
        provenance: CostProvenance,
        /// Millionths of the currency unit.
        input_micros: Option<u64>,
        output_micros: Option<u64>,
        total_micros: u64,
        /// Dispatched attempts that reported at least one token count.
        attempts_priced: u64,
        attempts_dispatched: u64,
        /// True only when every dispatched attempt reported both counts; otherwise the figure
        /// is a lower bound.
        complete: bool,
        billed: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmOperations {
    pub arm: &'static str,
    pub requests: RequestCounts,
    /// Completed attempts of matched pairs only.
    pub latency_matched: LatencySummary,
    /// The side's final completed attempt in every case, matched or not.
    pub latency_all_completed: LatencySummary,
    pub usage_matched: UsageSummary,
    pub usage_all_dispatched: UsageSummary,
    pub cost: CostSummary,
}

pub(super) fn arm_operations(
    journal: &Journal,
    arm: usize,
    matched_cases: &HashSet<usize>,
    profile_version: &str,
    rates: Option<&CostRates>,
) -> ArmOperations {
    let dispatched: Vec<&AttemptRecord> = journal.dispatched_attempts(arm).collect();

    let mut requests = RequestCounts::default();
    for attempt in &dispatched {
        requests.dispatched += 1;
        match attempt.state {
            AttemptState::Completed => requests.completed += 1,
            AttemptState::Failed => requests.failed += 1,
            AttemptState::Started | AttemptState::Unknown => requests.outcome_unknown += 1,
        }
        requests.retries += u64::from(attempt.envelope.retry_of.is_some());
        requests.reruns += u64::from(attempt.envelope.rerun_of.is_some());
    }

    let final_completed = |only_matched: bool| -> Vec<&AttemptRecord> {
        (0..journal.cases.len())
            .filter(|case_index| !only_matched || matched_cases.contains(case_index))
            .filter_map(|case_index| match journal.side(case_index, arm) {
                Side::Dispatched(attempt) if attempt.state == AttemptState::Completed => {
                    Some(attempt)
                }
                _ => None,
            })
            .collect()
    };
    let matched = final_completed(true);
    let all_completed = final_completed(false);

    ArmOperations {
        arm: if arm == 0 { "a" } else { "b" },
        requests,
        latency_matched: latency(&matched),
        latency_all_completed: latency(&all_completed),
        usage_matched: usage(&matched),
        usage_all_dispatched: usage(&dispatched),
        cost: cost(&dispatched, profile_version, rates),
    }
}

fn duration_of(attempt: &AttemptRecord) -> Option<u64> {
    attempt
        .elapsed_ms
        .or_else(|| attempt.envelope.metadata.as_ref().map(|m| m.elapsed_ms))
}

fn latency(attempts: &[&AttemptRecord]) -> LatencySummary {
    let mut durations: Vec<u64> = attempts.iter().filter_map(|a| duration_of(a)).collect();
    durations.sort_unstable();
    let mut summary = LatencySummary {
        samples: durations.len() as u64,
        unknown: (attempts.len() - durations.len()) as u64,
        ..LatencySummary::default()
    };
    if !durations.is_empty() {
        summary.min_ms = durations.first().copied();
        summary.max_ms = durations.last().copied();
        summary.median_ms = Some(durations[(durations.len() - 1) / 2]);
        summary.total_ms = Some(durations.iter().fold(0u64, |a, b| a.saturating_add(*b)));
    }
    summary
}

fn usage(attempts: &[&AttemptRecord]) -> UsageSummary {
    let mut summary = UsageSummary {
        attempts: attempts.len() as u64,
        ..UsageSummary::default()
    };
    for attempt in attempts {
        let reported = attempt
            .usage
            .as_ref()
            .filter(|usage| usage.input_tokens.is_some() || usage.output_tokens.is_some());
        let Some(reported) = reported else {
            summary.attempts_without_usage += 1;
            continue;
        };
        summary.attempts_with_usage += 1;
        if let Some(input) = reported.input_tokens {
            summary.input_tokens_attempts += 1;
            summary.input_tokens = Some(summary.input_tokens.unwrap_or(0).saturating_add(input));
        }
        if let Some(output) = reported.output_tokens {
            summary.output_tokens_attempts += 1;
            summary.output_tokens = Some(summary.output_tokens.unwrap_or(0).saturating_add(output));
        }
    }
    summary
}

fn micros(tokens: u64, per_million: u64) -> u128 {
    u128::from(tokens) * u128::from(per_million) / MICROS_PER_MILLION
}

fn cost(
    dispatched: &[&AttemptRecord],
    profile_version: &str,
    rates: Option<&CostRates>,
) -> CostSummary {
    let Some(rates) = rates else {
        return CostSummary::Unknown {
            reason: "no_rates_supplied",
        };
    };
    let Some(rate) = rates.for_profile(profile_version) else {
        return CostSummary::Unknown {
            reason: "no_rate_for_profile_version",
        };
    };
    let spent = usage(dispatched);
    if spent.attempts_with_usage == 0 {
        return CostSummary::Unknown {
            reason: "no_usage_reported",
        };
    }
    estimate(&spent, rate, rates).unwrap_or(CostSummary::Unknown {
        reason: "estimate_out_of_range",
    })
}

fn estimate(spent: &UsageSummary, rate: &ProfileRate, rates: &CostRates) -> Option<CostSummary> {
    let input = spent
        .input_tokens
        .map(|tokens| micros(tokens, rate.input_micros_per_million_tokens));
    let output = spent
        .output_tokens
        .map(|tokens| micros(tokens, rate.output_micros_per_million_tokens));
    let total = input.unwrap_or(0) + output.unwrap_or(0);
    Some(CostSummary::Estimated {
        provenance: CostProvenance {
            kind: "supplied_rate_estimate",
            rates_version: rates.rates_version.clone(),
            currency: rates.currency.clone(),
            profile_version: rate.profile_version.as_str().to_string(),
            basis: "provider-reported tokens times supplied per-million-token rates; an estimate, not a billed cost",
        },
        input_micros: input.map(u64::try_from).transpose().ok()?,
        output_micros: output.map(u64::try_from).transpose().ok()?,
        total_micros: u64::try_from(total).ok()?,
        attempts_priced: spent.attempts_with_usage,
        attempts_dispatched: spent.attempts,
        complete: spent.input_tokens_attempts == spent.attempts
            && spent.output_tokens_attempts == spent.attempts,
        billed: false,
    })
}
