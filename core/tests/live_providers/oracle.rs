//! The contrastive fixtures and their oracle, scored against the authoritative state the
//! production apply step left behind.
//!
//! The scoring follows `tools/evaluation/src/score.rs` where durable state allows: a stored
//! reminder keeps no resolution quality and a stored item keeps no evidence spans, so those two
//! checks apply to candidates only and are not made here. Outcomes carry rule names and counts,
//! never captured text or model output.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

pub const DEFAULT_CAPTURE_INSTANT: &str = "2026-10-08T14:00:00Z";
pub const DEFAULT_TIMEZONE: &str = "America/New_York";

pub struct OracleFixture {
    pub id: String,
    pub category: String,
    pub raw_input: String,
    pub user_correction: Option<String>,
    pub capture_instant: DateTime<Utc>,
    pub timezone: String,
    pub expected: Value,
    pub forbidden: Value,
}

pub fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures/intent/contrastive-fixtures.json")
}

pub fn load_fixtures() -> Vec<OracleFixture> {
    let bytes = std::fs::read(fixtures_path()).expect("contrastive fixtures are present");
    let file: Value = serde_json::from_slice(&bytes).expect("fixtures are JSON");
    file["fixtures"]
        .as_array()
        .expect("fixtures array")
        .iter()
        .map(|fixture| {
            let context = &fixture["capture_context"];
            let corrected = fixture["text_basis"] == "corrected";
            OracleFixture {
                id: fixture["id"].as_str().unwrap().to_string(),
                category: fixture["category"].as_str().unwrap().to_string(),
                raw_input: fixture["input"].as_str().unwrap().to_string(),
                user_correction: corrected
                    .then(|| context["user_correction"].as_str().unwrap().to_string()),
                capture_instant: DateTime::parse_from_rfc3339(
                    context["capture_instant"]
                        .as_str()
                        .unwrap_or(DEFAULT_CAPTURE_INSTANT),
                )
                .expect("fixture capture instant")
                .with_timezone(&Utc),
                timezone: context["device_timezone"]
                    .as_str()
                    .unwrap_or(DEFAULT_TIMEZONE)
                    .to_string(),
                expected: fixture["expected"].clone(),
                forbidden: fixture["forbidden"].clone(),
            }
        })
        .collect()
}

/// The durable result of one fixture, read after the dispatcher finished.
#[derive(Debug, Clone, Default)]
pub struct ObservedState {
    pub item_type: Option<String>,
    pub session_topic: Option<String>,
    pub lifecycle: String,
    pub processing_state: String,
    pub reminder_instant: Option<DateTime<Utc>>,
    pub reminder_timezone: Option<String>,
    pub has_reminder: bool,
    pub applied_proposals: i64,
    pub abstained: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SemanticOutcome {
    pub item_type: &'static str,
    pub reminder: &'static str,
    pub session_topic: &'static str,
    pub abstention: &'static str,
    pub forbidden_rule_hits: Vec<String>,
}

impl SemanticOutcome {
    /// A fixture matches only when its run ended `applied` or `abstained`, every facet is
    /// `match` or `none`, an abstention was expected exactly when the run abstained, and no
    /// forbidden rule was hit; a `missing` facet or a failed run is a miss.
    pub fn matches_oracle(&self, run_result: &str) -> bool {
        let facets_hold = [
            self.item_type,
            self.reminder,
            self.session_topic,
            self.abstention,
        ]
        .iter()
        .all(|facet| matches!(*facet, "match" | "none"));
        let result_fits = match run_result {
            "abstained" => self.abstention == "match",
            "applied" => self.abstention == "none",
            _ => false,
        };
        facets_hold && result_fits && self.forbidden_rule_hits.is_empty()
    }
}

fn expected_instant(expected: &Value) -> Option<DateTime<Utc>> {
    expected["reminder_proposal"]["instant"]
        .as_str()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|instant| instant.with_timezone(&Utc))
}

pub fn score(fixture: &OracleFixture, observed: &ObservedState) -> SemanticOutcome {
    let expected = &fixture.expected;
    let forbidden = &fixture.forbidden;

    let item_type = match (
        expected["item_type"].as_str(),
        observed.item_type.as_deref(),
    ) {
        (None, None) => "none",
        (Some(wanted), Some(found)) if wanted == found => "match",
        (Some(_), None) => "missing",
        _ => "wrong",
    };
    let reminder = match (&expected["reminder_proposal"], observed.has_reminder) {
        (wanted, false) if wanted.is_null() => "none",
        (_, false) => "missing",
        (wanted, true) if wanted.is_null() => "wrong",
        (wanted, true) => {
            let instant_ok = match expected_instant(expected) {
                Some(instant) => observed.reminder_instant == Some(instant),
                None => observed.reminder_instant.is_none(),
            };
            let zone_ok = wanted["timezone_id"].is_null()
                || observed.reminder_instant.is_none()
                || observed.reminder_timezone.as_deref() == wanted["timezone_id"].as_str();
            if instant_ok && zone_ok {
                "match"
            } else {
                "wrong"
            }
        }
    };
    let session_topic = match (
        expected["session_topic_proposal"]["topic"].as_str(),
        observed.session_topic.as_deref(),
    ) {
        (None, None) => "none",
        (Some(wanted), Some(found)) if wanted.trim().eq_ignore_ascii_case(found.trim()) => "match",
        (Some(_), None) => "missing",
        _ => "wrong",
    };
    let abstention = match (!expected["abstention"].is_null(), observed.abstained) {
        (false, false) => "none",
        (true, true) => "match",
        (true, false) => "wrong",
        (false, true) => "wrong",
    };

    let mut hits = Vec::new();
    let mut hit = |rule: String| {
        if !hits.contains(&rule) {
            hits.push(rule)
        }
    };
    if let Some(found) = observed.item_type.as_deref() {
        if forbidden["item_types"]
            .as_array()
            .is_some_and(|banned| banned.iter().any(|item| item == found))
        {
            hit(format!("item_type:{found}"));
        }
        if forbidden["facets"] == "any" {
            hit("facet:any".to_string());
        }
    }
    if observed.has_reminder {
        match forbidden["reminder"].as_str() {
            Some("any") => hit("reminder:any".to_string()),
            Some("any_instant") if observed.reminder_instant.is_some() => {
                hit("reminder:any_instant".to_string())
            }
            _ => {}
        }
        if let (Some(zone), true) = (
            observed.reminder_timezone.as_deref(),
            observed.reminder_instant.is_some(),
        ) {
            if forbidden["reminder_timezones"]
                .as_array()
                .is_some_and(|banned| banned.iter().any(|item| item == zone))
            {
                hit(format!("reminder_timezone:{zone}"));
            }
        }
        if forbidden["facets"] == "any" {
            hit("facet:any".to_string());
        }
    }
    if observed.session_topic.is_some() {
        if forbidden["session_topic"] == true {
            hit("session_topic".to_string());
        }
        if forbidden["facets"] == "any" {
            hit("facet:any".to_string());
        }
    }
    if observed.lifecycle != "active" {
        hit("lifecycle:left_active".to_string());
    }
    SemanticOutcome {
        item_type,
        reminder,
        session_topic,
        abstention,
        forbidden_rule_hits: hits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixture_loads_with_a_context() {
        let fixtures = load_fixtures();
        assert!(fixtures.len() >= 30);
        assert!(fixtures
            .iter()
            .any(|fixture| fixture.user_correction.is_some()));
    }

    #[test]
    fn forbidden_item_type_is_a_hit_and_the_oracle_item_type_matches() {
        let fixture = load_fixtures()
            .into_iter()
            .find(|fixture| fixture.id == "design-broad-intention")
            .unwrap();
        let mut observed = ObservedState {
            lifecycle: "active".into(),
            item_type: Some("idea".into()),
            ..ObservedState::default()
        };
        assert!(score(&fixture, &observed).matches_oracle("applied"));
        observed.item_type = Some("action".into());
        let outcome = score(&fixture, &observed);
        assert_eq!(outcome.item_type, "wrong");
        assert_eq!(outcome.forbidden_rule_hits, vec!["item_type:action"]);
    }

    fn outcome(facet: &'static str, abstention: &'static str) -> SemanticOutcome {
        SemanticOutcome {
            item_type: facet,
            reminder: "none",
            session_topic: "none",
            abstention,
            forbidden_rule_hits: Vec::new(),
        }
    }

    #[test]
    fn a_failed_run_with_an_all_none_oracle_is_not_a_match() {
        let all_none = outcome("none", "none");
        for result in ["failed", "backed_off", "retired", "not_run", "other"] {
            assert!(!all_none.matches_oracle(result), "{result}");
        }
        assert!(all_none.matches_oracle("applied"));
    }

    #[test]
    fn a_missing_facet_is_not_a_match() {
        assert!(!outcome("missing", "none").matches_oracle("applied"));
    }

    #[test]
    fn an_applied_run_with_every_facet_matching_is_a_match() {
        assert!(outcome("match", "none").matches_oracle("applied"));
    }

    #[test]
    fn an_expected_abstention_matches_only_when_the_run_abstained() {
        assert!(!outcome("none", "match").matches_oracle("applied"));
        assert!(outcome("none", "match").matches_oracle("abstained"));
        assert!(!outcome("none", "none").matches_oracle("abstained"));
    }
}
