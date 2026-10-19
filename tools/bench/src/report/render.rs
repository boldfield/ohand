//! Plain-text rendering. Every figure appears next to its denominator; nothing is ranked.
//! The text is built only from the report's own fields, which hold counts, fixed vocabulary and
//! journal strings already passed through `safe`.

use std::fmt::Write as _;

use evaluation::comparison::{Disagreement, PreferenceSection, SideStatus};
use evaluation::report::Totals;

use super::stats::{ArmOperations, CostSummary, LatencySummary, UsageSummary};
use super::{name_of, safe, BenchReport};

fn optional<T: std::fmt::Display>(value: Option<T>, unit: &str) -> String {
    match value {
        Some(value) => format!("{value}{unit}"),
        None => "unknown".to_string(),
    }
}

fn counts<K: serde::Serialize + Ord>(map: &std::collections::BTreeMap<K, u64>) -> String {
    let parts: Vec<String> = map
        .iter()
        .map(|(key, count)| format!("{}={count}", name_of(key)))
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(" ")
    }
}

fn latency_line(latency: &LatencySummary) -> String {
    if latency.samples == 0 {
        return format!(
            "none recorded ({} completed without a duration)",
            latency.unknown
        );
    }
    format!(
        "n={} min={} median={} max={} total={} (unknown duration: {})",
        latency.samples,
        optional(latency.min_ms, " ms"),
        optional(latency.median_ms, " ms"),
        optional(latency.max_ms, " ms"),
        optional(latency.total_ms, " ms"),
        latency.unknown
    )
}

fn usage_line(usage: &UsageSummary) -> String {
    format!(
        "{} attempts: input tokens {} (reported by {}), output tokens {} (reported by {}), no usage reported by {}",
        usage.attempts,
        optional(usage.input_tokens, ""),
        usage.input_tokens_attempts,
        optional(usage.output_tokens, ""),
        usage.output_tokens_attempts,
        usage.attempts_without_usage
    )
}

fn micros_text(micros: u64, currency: &str) -> String {
    format!(
        "{}.{:06} {currency}",
        micros / 1_000_000,
        micros % 1_000_000
    )
}

fn cost_line(cost: &CostSummary) -> String {
    match cost {
        CostSummary::Unknown { reason } => format!("unknown ({reason})"),
        CostSummary::Estimated {
            provenance,
            total_micros,
            attempts_priced,
            attempts_dispatched,
            complete,
            ..
        } => format!(
            "estimate {} from rates {} ({} of {} attempts reported tokens{}); not a billed cost",
            micros_text(*total_micros, provenance.currency.as_str()),
            provenance.rates_version.as_str(),
            attempts_priced,
            attempts_dispatched,
            if *complete { "" } else { "; a lower bound" }
        ),
    }
}

fn arm_block(out: &mut String, report: &BenchReport, arm: usize) {
    let summary = &report.arms[arm];
    let profile = &report.experiment.profiles[arm];
    let sides = &summary.sides;
    let totals: &Totals = &summary.totals;
    let _ = writeln!(
        out,
        "arm {}: profile {} (version {}, protocol {})",
        summary.arm,
        safe(&profile.profile_id),
        summary.profile.as_str(),
        profile.protocol.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        out,
        "  responses: completed {}/{} failed {} timed_out {} cancelled {} unknown {} skipped {} missing {}",
        sides.completed,
        sides.eligible,
        sides.failed,
        sides.timed_out,
        sides.cancelled,
        sides.unknown,
        sides.skipped,
        sides.missing
    );
    if !sides.reasons.is_empty() {
        let _ = writeln!(out, "  no-response reasons: {}", counts(&sides.reasons));
    }
    let _ = writeln!(
        out,
        "  invalid outputs among {} responses: mapping_rejected {} guard_rejected {}",
        sides.completed,
        summary.invalid_outputs.mapping_rejected,
        summary.invalid_outputs.guard_rejected
    );
    let _ = writeln!(
        out,
        "  defects against the oracle over {} responses: {}",
        totals.cases,
        counts(&totals.candidate_defects)
    );
    let _ = writeln!(
        out,
        "  abstention (oracle abstains in {}): {}",
        totals.oracle_abstains,
        counts(&totals.abstention)
    );
    let _ = writeln!(
        out,
        "  forbidden mutations, candidate: {}",
        counts(&totals.candidate_forbidden)
    );
    let _ = writeln!(
        out,
        "  forbidden mutations, authoritative: {}",
        counts(&totals.authoritative_forbidden)
    );
}

fn operations_block(out: &mut String, operations: &ArmOperations) {
    let requests = &operations.requests;
    let _ = writeln!(out, "arm {}:", operations.arm);
    let _ = writeln!(
        out,
        "  requests: dispatched {} completed {} failed {} outcome_unknown {} retries {} reruns {}",
        requests.dispatched,
        requests.completed,
        requests.failed,
        requests.outcome_unknown,
        requests.retries,
        requests.reruns
    );
    if !operations.rejections.is_empty() {
        let _ = writeln!(
            out,
            "  provider rejections by error type: {}",
            counts(&operations.rejections)
        );
    }
    let _ = writeln!(
        out,
        "  latency, matched pairs: {}",
        latency_line(&operations.latency_matched)
    );
    let _ = writeln!(
        out,
        "  latency, every completed: {}",
        latency_line(&operations.latency_all_completed)
    );
    let _ = writeln!(
        out,
        "  usage, matched pairs: {}",
        usage_line(&operations.usage_matched)
    );
    let _ = writeln!(
        out,
        "  usage, all dispatched: {}",
        usage_line(&operations.usage_all_dispatched)
    );
    let _ = writeln!(out, "  cost: {}", cost_line(&operations.cost));
}

fn side_cell(side: &super::SideRow) -> String {
    let mut cell = name_of(&side.status);
    if let Some(producer) = side.producer {
        cell = producer.to_string();
    }
    if let Some(reason) = &side.reason {
        cell = format!("{cell}/{}", reason.as_str());
    }
    if !side.defects.is_empty() {
        cell = format!("{cell} defects:{}", side.defects.join(","));
    }
    if let Some(abstention) = &side.abstention {
        cell = format!("{cell} abstention:{abstention}");
    }
    if side.forbidden_hits > 0 {
        cell = format!("{cell} forbidden:{}", side.forbidden_hits);
    }
    cell
}

pub(super) fn render_text(report: &BenchReport) -> String {
    let mut out = String::new();
    let experiment = &report.experiment;
    let _ = writeln!(out, "bench report (diagnostic only)");
    let _ = writeln!(out, "{}", report.selection.note);
    let _ = writeln!(out);
    let _ = writeln!(out, "EXPERIMENT");
    let _ = writeln!(
        out,
        "  id {}  execution {}  corpus {} ({} fixtures, synthetic)  instruction {}",
        experiment.id.as_str(),
        name_of(&experiment.execution),
        experiment.corpus_version,
        report.corpus.fixtures,
        experiment.instruction_version
    );
    let _ = writeln!(
        out,
        "  build {}  journal: {} records, {} runs, torn tail {}, outcome-unknown attempts {}",
        experiment.build_revision,
        report.journal.records,
        report.journal.runs,
        if report.journal.torn_tail {
            "yes"
        } else {
            "no"
        },
        report.journal.ambiguous_started_attempts
    );

    let pairs = &report.pairs;
    let _ = writeln!(
        out,
        "\nPAIRS (denominator: {} eligible cases)",
        pairs.eligible
    );
    let _ = writeln!(
        out,
        "  matched {}  failed {}  missing {}  (of which no record {})",
        pairs.matched, pairs.failed, pairs.missing, pairs.no_record
    );

    let _ = writeln!(
        out,
        "\nOBJECTIVE RESULTS PER ARM (against the synthetic oracle)"
    );
    for arm in 0..2 {
        arm_block(&mut out, report, arm);
    }

    let objective = &report.paired_objective;
    let _ = writeln!(
        out,
        "\nPAIRED OBJECTIVE (matched pairs: {})",
        objective.matched_pairs
    );
    let _ = writeln!(
        out,
        "  both clean {}  only a clean {}  only b clean {}  neither clean {}",
        objective.both_clean,
        objective.only_a_clean,
        objective.only_b_clean,
        objective.neither_clean
    );
    let disagreement = &report.disagreement;
    let _ = writeln!(
        out,
        "\nDISAGREEMENT (matched pairs: {}; agreement is not accuracy)",
        disagreement.matched_pairs
    );
    let _ = writeln!(
        out,
        "  comparable {}  agree {}  differ {}  not comparable {}  facets: {}",
        disagreement.comparable,
        disagreement.agree,
        disagreement.differ,
        disagreement.not_comparable,
        counts(&disagreement.by_facet)
    );
    let PreferenceSection::NotCollected = report.preference;
    let _ = writeln!(out, "\nPREFERENCE\n  not collected by this report");

    let _ = writeln!(
        out,
        "\nOPERATIONS (client-measured; token counts are provider-reported)"
    );
    for operations in &report.operations {
        operations_block(&mut out, operations);
    }

    let comparability = &report.comparability;
    let _ = writeln!(
        out,
        "\nCOMPARABILITY (model identity as the only variable: {})",
        comparability.model_identity_only_variable
    );
    let _ = writeln!(
        out,
        "  requested temperature: {}",
        comparability.requested_temperature
    );
    for arm in &comparability.arms {
        let requested = if arm.requested.is_empty() {
            "none".to_string()
        } else {
            arm.requested.join(" ")
        };
        let _ = writeln!(
            out,
            "  arm {}: protocol {}  requested [{}]  effective confirmed {} unknown {} differs {}  reported model {}",
            arm.arm,
            arm.protocol.as_deref().unwrap_or("unknown"),
            requested,
            arm.effective_confirmed,
            arm.effective_unknown,
            arm.effective_differs,
            counts(&arm.reported_model)
        );
    }
    if comparability.flags.is_empty() {
        let _ = writeln!(out, "  no flags raised");
    }
    for flag in &comparability.flags {
        let _ = writeln!(out, "  [{}] x{}: {}", flag.code, flag.count, flag.detail);
    }

    let _ = writeln!(out, "\nCASES");
    for row in &report.cases {
        let verdict = match &row.disagreement {
            Disagreement::Agree => "agree".to_string(),
            Disagreement::NotComparable => "not_comparable".to_string(),
            Disagreement::Differ { facets } => format!(
                "differ({})",
                facets.iter().map(name_of).collect::<Vec<_>>().join(",")
            ),
        };
        let objective = match row.objective {
            Some(objective) => format!("clean a={} b={}", objective.a_clean, objective.b_clean),
            None => "not scored as a pair".to_string(),
        };
        let _ = writeln!(
            out,
            "  {} [{}] {}: {}; {}",
            row.fixture_id,
            name_of(&row.category),
            name_of(&row.pairing),
            verdict,
            objective
        );
        for (arm, side) in ["a", "b"].iter().zip(&row.sides) {
            let marker = if side.status == SideStatus::Completed {
                ""
            } else {
                " (no response)"
            };
            let _ = writeln!(out, "      {arm}: {}{marker}", side_cell(side));
        }
    }
    if report.cases_omitted > 0 {
        let _ = writeln!(
            out,
            "  ... {} more cases omitted; the JSON format lists the same bounded rows",
            report.cases_omitted
        );
    }

    let _ = writeln!(out, "\nNOTICES");
    for notice in &report.notices {
        let _ = writeln!(out, "  - {notice}");
    }
    out
}
