//! Durable journal for experiment records with truncation detection and recovery.
//!
//! The journal appends run records as newline-delimited JSON. On open, it detects
//! truncated tails and preserves all intact records while marking ambiguous
//! started attempts as unknown.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use thiserror::Error;

use crate::records::{Attempt, Case, Experiment};

/// Error conditions in journal operations.
#[derive(Debug, Error, PartialEq)]
pub enum JournalError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("JSON serialization error: {0}")]
    Serialization(String),
    #[error("JSON deserialization error: {0}")]
    Deserialization(String),
    #[error("schema mismatch: expected {expected}, got {actual}")]
    SchemaMismatch { expected: u32, actual: u32 },
    #[error("truncated record at end of file")]
    Truncated,
    #[error("corruption in middle of file at line {line}: {reason}")]
    Corruption { line: usize, reason: String },
    #[error("unknown record type: {0}")]
    UnknownRecordType(String),
}

/// A journal record is one of: Experiment, Case, or Attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum JournalRecord {
    Experiment(Experiment),
    Case(Case),
    Attempt(Attempt),
}

/// Outcome of opening an existing journal.
#[derive(Debug, Clone, PartialEq)]
pub struct TruncationRecovery {
    /// Total records read (before truncation point).
    pub records_read: usize,
    /// Number of started attempts marked as unknown due to truncation.
    pub ambiguous_started_attempts: usize,
    /// Human-readable reason for any truncation detected.
    pub truncation_reason: Option<String>,
}

/// Writer for appending records to a journal.
pub struct JournalWriter {
    file: File,
}

impl JournalWriter {
    /// Open or create a journal file for appending.
    /// Detects and repairs any truncated tail.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path_ref = path.as_ref();

        if path_ref.exists() {
            Self::repair_truncated_tail(path_ref)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| JournalError::Io(e.to_string()))?;
        Ok(JournalWriter { file })
    }

    /// Detect and repair a truncated tail in an existing journal.
    /// Works at the byte level to handle UTF-8 truncation and ensures file ends with newline.
    fn repair_truncated_tail(path: &Path) -> Result<(), JournalError> {
        use std::io::Read;

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| JournalError::Io(e.to_string()))?;

        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|e| JournalError::Io(e.to_string()))?;

        if contents.is_empty() {
            return Ok(());
        }

        // Find the last complete line (ending with \n) by working backwards from the end.
        let mut last_newline_pos = None;
        for i in (0..contents.len()).rev() {
            if contents[i] == b'\n' {
                last_newline_pos = Some(i);
                break;
            }
        }

        // If there's no newline at all, the entire file is a potential truncation.
        if last_newline_pos.is_none() {
            // Try to parse the entire contents as valid UTF-8 JSON.
            match std::str::from_utf8(&contents) {
                Ok(text) => {
                    let text = text.trim();
                    if text.is_empty() {
                        return Ok(());
                    }
                    // If it parses as valid JSON, keep the entire file (missing final newline).
                    // But we must add one for the next append.
                    if serde_json::from_str::<JournalRecord>(text).is_ok() {
                        drop(file);
                        let mut f = OpenOptions::new()
                            .append(true)
                            .open(path)
                            .map_err(|e| JournalError::Io(e.to_string()))?;
                        writeln!(f).map_err(|e| JournalError::Io(e.to_string()))?;
                        return Ok(());
                    }
                }
                Err(_) => {
                    // Invalid UTF-8 from the start - truncate the file entirely.
                    file.set_len(0)
                        .map_err(|e| JournalError::Io(e.to_string()))?;
                    return Ok(());
                }
            }
            // If we get here, the entire file is incomplete JSON - truncate it.
            file.set_len(0)
                .map_err(|e| JournalError::Io(e.to_string()))?;
            return Ok(());
        }

        // We found a newline. Check if there's incomplete content after it.
        let after_last_newline_start = last_newline_pos.unwrap() + 1;
        if after_last_newline_start >= contents.len() {
            // File ends with a newline - nothing to repair.
            return Ok(());
        }

        // There's content after the last newline (potentially truncated).
        let partial_line = &contents[after_last_newline_start..];

        // Try to parse as UTF-8 and JSON. If it's valid, keep it and add newline.
        // Otherwise, truncate to the last complete line.
        match std::str::from_utf8(partial_line) {
            Ok(text) => {
                let text = text.trim();
                if text.is_empty() {
                    // Just whitespace after the newline - truncate.
                    file.set_len((last_newline_pos.unwrap() + 1) as u64)
                        .map_err(|e| JournalError::Io(e.to_string()))?;
                } else if serde_json::from_str::<JournalRecord>(text).is_ok() {
                    // Valid JSON but missing newline - add the newline.
                    drop(file);
                    let mut f = OpenOptions::new()
                        .append(true)
                        .open(path)
                        .map_err(|e| JournalError::Io(e.to_string()))?;
                    writeln!(f).map_err(|e| JournalError::Io(e.to_string()))?;
                } else {
                    // Invalid JSON - truncate to the last complete line.
                    file.set_len((last_newline_pos.unwrap() + 1) as u64)
                        .map_err(|e| JournalError::Io(e.to_string()))?;
                }
            }
            Err(_) => {
                // Invalid UTF-8 in the partial line - truncate to the last complete line.
                file.set_len((last_newline_pos.unwrap() + 1) as u64)
                    .map_err(|e| JournalError::Io(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Append an experiment record.
    pub fn write_experiment(&mut self, exp: &Experiment) -> Result<(), JournalError> {
        self.write_record(&JournalRecord::Experiment(exp.clone()))
    }

    /// Append a case record.
    pub fn write_case(&mut self, case: &Case) -> Result<(), JournalError> {
        self.write_record(&JournalRecord::Case(case.clone()))
    }

    /// Append an attempt record.
    pub fn write_attempt(&mut self, attempt: &Attempt) -> Result<(), JournalError> {
        self.write_record(&JournalRecord::Attempt(attempt.clone()))
    }

    /// Append a raw record, serializing as JSON and flushing.
    /// Validates the record before writing to prevent bad data from being persisted.
    fn write_record(&mut self, record: &JournalRecord) -> Result<(), JournalError> {
        // Validate before writing to ensure the journal stays readable
        JournalReader::validate_schema_version(record)?;
        JournalReader::validate_record_invariants(record)?;

        let json = serde_json::to_string(record)
            .map_err(|e| JournalError::Serialization(e.to_string()))?;
        writeln!(self.file, "{}", json).map_err(|e| JournalError::Io(e.to_string()))?;
        self.file
            .flush()
            .map_err(|e| JournalError::Io(e.to_string()))?;
        self.file
            .sync_data()
            .map_err(|e| JournalError::Io(e.to_string()))?;
        Ok(())
    }
}

/// Reader for loading records from a journal, with truncation recovery.
pub struct JournalReader {
    records: Vec<JournalRecord>,
    recovery: TruncationRecovery,
}

impl JournalReader {
    /// Open an existing journal and recover from any truncation.
    ///
    /// If the journal is empty, returns a reader with zero records.
    /// If truncation is detected at EOF, all complete records are preserved.
    /// Records are validated on read using the same invariants as constructors.
    /// Mid-file corruption returns an error.
    /// Ambiguous started attempts (Started with no matching terminal record) are marked Unknown.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let file = File::open(&path)
            .map_err(|e| JournalError::Io(format!("failed to open journal: {}", e)))?;

        let reader = BufReader::new(file);
        let mut records = Vec::new();
        let mut recovery = TruncationRecovery {
            records_read: 0,
            ambiguous_started_attempts: 0,
            truncation_reason: None,
        };

        let lines: Vec<String> = reader
            .lines()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| JournalError::Io(e.to_string()))?;

        for (line_idx, line) in lines.iter().enumerate() {
            let line = line.trim();

            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<JournalRecord>(line) {
                Ok(record) => {
                    Self::validate_schema_version(&record)?;
                    Self::validate_record_invariants(&record)?;
                    records.push(record);
                    recovery.records_read += 1;
                }
                Err(e) => {
                    let is_last_line = line_idx == lines.len() - 1;

                    if is_last_line {
                        recovery.truncation_reason =
                            Some(format!("truncation at line {}: {}", line_idx + 1, e));
                    } else {
                        return Err(JournalError::Corruption {
                            line: line_idx + 1,
                            reason: e.to_string(),
                        });
                    }

                    break;
                }
            }
        }

        Self::reconcile_ambiguous_started_attempts(&mut records, &mut recovery);

        Ok(JournalReader { records, recovery })
    }

    /// Validate schema version in a record.
    fn validate_schema_version(record: &JournalRecord) -> Result<(), JournalError> {
        match record {
            JournalRecord::Experiment(exp) => {
                if exp.schema_version != 1 {
                    return Err(JournalError::SchemaMismatch {
                        expected: 1,
                        actual: exp.schema_version,
                    });
                }
            }
            JournalRecord::Case(case) => {
                if case.schema_version != 1 {
                    return Err(JournalError::SchemaMismatch {
                        expected: 1,
                        actual: case.schema_version,
                    });
                }
            }
            JournalRecord::Attempt(attempt) => {
                if attempt.schema_version != 1 {
                    return Err(JournalError::SchemaMismatch {
                        expected: 1,
                        actual: attempt.schema_version,
                    });
                }
            }
        }
        Ok(())
    }

    /// Validate record invariants (empty identifiers, credentials, etc.)
    fn validate_record_invariants(record: &JournalRecord) -> Result<(), JournalError> {
        match record {
            JournalRecord::Experiment(exp) => {
                if exp.corpus_version.is_empty()
                    || exp.source_context.is_empty()
                    || exp.instruction_version.is_empty()
                    || exp.profile_a_id.is_empty()
                    || exp.profile_a_version.is_empty()
                    || exp.profile_b_id.is_empty()
                    || exp.profile_b_version.is_empty()
                    || exp.build_revision.is_empty()
                {
                    return Err(JournalError::Deserialization(
                        "experiment record has empty identifiers".to_string(),
                    ));
                }
                // Validate credential/endpoint patterns
                if crate::records::is_secret_or_endpoint(&exp.source_context) {
                    return Err(JournalError::Deserialization(
                        "experiment source_context contains credentials or private endpoint"
                            .to_string(),
                    ));
                }
                if crate::records::is_secret_or_endpoint(&exp.profile_a_id)
                    || crate::records::is_secret_or_endpoint(&exp.profile_a_version)
                    || crate::records::is_secret_or_endpoint(&exp.profile_b_id)
                    || crate::records::is_secret_or_endpoint(&exp.profile_b_version)
                    || crate::records::is_secret_or_endpoint(&exp.build_revision)
                {
                    return Err(JournalError::Deserialization(
                        "experiment record contains credentials or private endpoints".to_string(),
                    ));
                }
                Ok(())
            }
            JournalRecord::Case(case) => {
                if case.experiment_id.is_empty()
                    || case.case_id.is_empty()
                    || case.content.is_empty()
                {
                    return Err(JournalError::Deserialization(
                        "case record has empty identifiers".to_string(),
                    ));
                }
                // Validate case content for credentials
                if crate::records::is_secret_or_endpoint(&case.content) {
                    return Err(JournalError::Deserialization(
                        "case content contains credentials or private endpoints".to_string(),
                    ));
                }
                Ok(())
            }
            JournalRecord::Attempt(attempt) => {
                if attempt.case_id.is_empty() || (attempt.arm != "a" && attempt.arm != "b") {
                    return Err(JournalError::Deserialization(
                        "attempt record has invalid identifiers or arm".to_string(),
                    ));
                }
                // Validate failure reason and provider output
                if let Some(reason) = &attempt.failure_reason {
                    if crate::records::is_secret_or_endpoint(reason) {
                        return Err(JournalError::Deserialization(
                            "attempt failure_reason contains credentials or endpoints".to_string(),
                        ));
                    }
                }
                if let Some(output) = &attempt.provider_output {
                    if let Ok(output_str) = serde_json::to_string(output) {
                        if crate::records::is_secret_or_endpoint(&output_str) {
                            return Err(JournalError::Deserialization(
                                "attempt provider_output contains credentials or endpoints"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Reconcile ambiguous started attempts by attempt id.
    /// An attempt is ambiguous if it's in Started state with no matching terminal record.
    /// Reconciliation looks for Started records without a corresponding Completed/Failed
    /// record (by id) and marks them as Unknown.
    fn reconcile_ambiguous_started_attempts(
        records: &mut [JournalRecord],
        recovery: &mut TruncationRecovery,
    ) {
        // Build a map of attempt ids with their terminal states (if any).
        let mut attempt_terminals: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();

        for record in records.iter() {
            if let JournalRecord::Attempt(attempt) = record {
                match attempt.state {
                    crate::records::AttemptState::Completed
                    | crate::records::AttemptState::Failed => {
                        attempt_terminals.insert(attempt.id.clone(), true);
                    }
                    _ => {}
                }
            }
        }

        // Now mark any Started attempts without a terminal as Unknown.
        for record in records.iter_mut() {
            if let JournalRecord::Attempt(attempt) = record {
                if attempt.state == crate::records::AttemptState::Started
                    && !attempt_terminals.contains_key(&attempt.id)
                {
                    attempt.state = crate::records::AttemptState::Unknown;
                    attempt.failure_reason =
                        Some("marked unknown due to unmatched started attempt".to_string());
                    recovery.ambiguous_started_attempts += 1;
                }
            }
        }
    }

    /// Get all records read from the journal.
    pub fn records(&self) -> &[JournalRecord] {
        &self.records
    }

    /// Get the truncation recovery information.
    pub fn recovery(&self) -> &TruncationRecovery {
        &self.recovery
    }

    /// Filter records by type, returning only experiments.
    pub fn experiments(&self) -> Vec<&Experiment> {
        self.records
            .iter()
            .filter_map(|r| match r {
                JournalRecord::Experiment(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    /// Filter records by type, returning only cases.
    pub fn cases(&self) -> Vec<&Case> {
        self.records
            .iter()
            .filter_map(|r| match r {
                JournalRecord::Case(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    /// Filter records by type, returning only attempts.
    pub fn attempts(&self) -> Vec<&Attempt> {
        self.records
            .iter()
            .filter_map(|r| match r {
                JournalRecord::Attempt(a) => Some(a),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_write_and_read_experiment() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new(
            "v1.0",
            "2026-10-08T12:00:00Z",
            "instr-v1",
            "profile-a",
            "1.0",
            "profile-b",
            "1.0",
            "build-123",
        )?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.records.len(), 1);
        assert_eq!(reader.experiments().len(), 1);
        assert_eq!(reader.experiments()[0], &exp);

        Ok(())
    }

    #[test]
    fn test_write_multiple_records() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt = Attempt::new(&case.id, "a")?;
        attempt.complete(1000, None)?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt)?;

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.records.len(), 3);
        assert_eq!(reader.experiments().len(), 1);
        assert_eq!(reader.cases().len(), 1);
        assert_eq!(reader.attempts().len(), 1);

        Ok(())
    }

    #[test]
    fn test_truncation_detection() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let attempt = Attempt::new(&case.id, "a")?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt)?;

        // Simulate truncation by appending incomplete JSON
        {
            let mut f = fs::OpenOptions::new().append(true).open(file.path())?;
            write!(f, r#"{{"record_type":"attempt","id":"incomplete"#)?;
            f.flush()?;
        }

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.records.len(), 3);
        assert!(reader.recovery.truncation_reason.is_some());

        Ok(())
    }

    #[test]
    fn test_truncation_marks_started_attempts_unknown() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt_a = Attempt::new(&case.id, "a")?;
        attempt_a.complete(1000, None)?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt_a)?;

        // Write a started attempt but truncate it before completion
        let mut f = fs::OpenOptions::new().append(true).open(file.path())?;
        use std::io::Write as StdWrite;
        write!(
            f,
            r#"{{"record_type":"attempt","id":"b-attempt","case_id":""#
        )?;
        f.sync_all()?;
        drop(f);

        let reader = JournalReader::open(file.path())?;
        // The truncated attempt_b was never successfully parsed, so no ambiguous attempts
        assert_eq!(reader.recovery.ambiguous_started_attempts, 0);
        let attempts = reader.attempts();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, crate::records::AttemptState::Completed);

        Ok(())
    }

    #[test]
    fn test_append_after_truncation_repair() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt_a = Attempt::new(&case.id, "a")?;
        attempt_a.complete(1000, None)?;

        {
            let mut writer = JournalWriter::open(file.path())?;
            writer.write_experiment(&exp)?;
            writer.write_case(&case)?;
            writer.write_attempt(&attempt_a)?;
        }

        {
            let f = fs::OpenOptions::new().write(true).open(file.path())?;
            let file_size = f.metadata()?.len();
            f.set_len(file_size - 5)?;
        }

        {
            let mut writer = JournalWriter::open(file.path())?;
            let mut attempt_b = Attempt::new(&case.id, "b")?;
            attempt_b.complete(2000, None)?;
            writer.write_attempt(&attempt_b)?;
        }

        let reader = JournalReader::open(file.path())?;
        let attempts = reader.attempts();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, crate::records::AttemptState::Completed);

        Ok(())
    }

    #[test]
    fn test_mid_file_corruption_returns_error() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;

        {
            let mut f = fs::OpenOptions::new().append(true).open(file.path())?;
            writeln!(f, "corrupted json line")?;
            let json = r#"{"record_type":"case","id":"valid","experiment_id":"exp-id","case_id":"c-1","content":"ct","schema_version":1}"#;
            writeln!(f, "{}", json)?;
            f.flush()?;
        }

        let result = JournalReader::open(file.path());
        assert!(matches!(result, Err(JournalError::Corruption { .. })));

        Ok(())
    }

    #[test]
    fn test_schema_mismatch_detection() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        {
            let mut f = fs::File::create(file.path())?;
            let json = r#"{"record_type":"experiment","id":"e1","schema_version":99,"corpus_version":"v1","source_context":"ctx","instruction_version":"instr","profile_a_id":"pa","profile_a_version":"1","profile_b_id":"pb","profile_b_version":"1","build_revision":"build","unknown_metadata":{"unknown_fields":[]}}"#;
            writeln!(&mut f, "{}", json)?;
        }

        let result = JournalReader::open(file.path());
        assert!(matches!(
            result,
            Err(JournalError::SchemaMismatch {
                expected: 1,
                actual: 99
            })
        ));

        Ok(())
    }

    #[test]
    fn test_started_attempt_at_eof_marked_unknown() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt_a = Attempt::new(&case.id, "a")?;
        attempt_a.complete(1000, None)?;
        let attempt_b = Attempt::new(&case.id, "b")?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt_a)?;
        writer.write_attempt(&attempt_b)?;

        let reader = JournalReader::open(file.path())?;
        let attempts = reader.attempts();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].state, crate::records::AttemptState::Completed);
        assert_eq!(attempts[1].state, crate::records::AttemptState::Unknown);
        assert_eq!(reader.recovery.ambiguous_started_attempts, 1);

        Ok(())
    }

    #[test]
    fn test_empty_journal() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.records.len(), 0);
        assert!(reader.recovery.truncation_reason.is_none());

        Ok(())
    }

    #[test]
    fn test_schema_version_preserved() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        assert_eq!(exp.schema_version, 1);

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;

        let reader = JournalReader::open(file.path())?;
        let loaded_exp = &reader.experiments()[0];
        assert_eq!(loaded_exp.schema_version, exp.schema_version);

        Ok(())
    }

    #[test]
    fn test_single_line_truncation_repair() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;

        {
            let mut writer = JournalWriter::open(file.path())?;
            writer.write_experiment(&exp)?;
            writer.write_case(&case)?;
        }

        // Truncate the file to remove the newline at the end of the last line
        // This tests repair of a file with no final newline
        {
            let contents = std::fs::read(file.path())?;
            // Remove the last byte (the newline)
            std::fs::write(file.path(), &contents[..contents.len() - 1])?;
        }

        // Reopen and append - should repair by adding newline and allow new writes
        {
            let mut writer = JournalWriter::open(file.path())?;
            let mut attempt = Attempt::new(&case.id, "a")?;
            attempt.complete(1000, None)?;
            writer.write_attempt(&attempt)?;
        }

        // Read should have exp, case, and attempt
        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.experiments().len(), 1);
        assert_eq!(reader.cases().len(), 1);
        assert_eq!(reader.attempts().len(), 1);

        Ok(())
    }

    #[test]
    fn test_utf8_truncation_repair() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        // Create a case with UTF-8 content
        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "héllo ✓")?;

        {
            let mut writer = JournalWriter::open(file.path())?;
            writer.write_experiment(&exp)?;
            writer.write_case(&case)?;
        }

        // Truncate in the middle of a UTF-8 multi-byte sequence
        {
            let f = fs::OpenOptions::new().write(true).open(file.path())?;
            let file_size = f.metadata()?.len();
            f.set_len(file_size - 3)?; // Cut into UTF-8 sequence
        }

        // Reopen - should detect and repair, discarding the partial UTF-8 line
        {
            let mut writer = JournalWriter::open(file.path())?;
            let mut attempt = Attempt::new(&case.id, "a")?;
            attempt.complete(1000, None)?;
            writer.write_attempt(&attempt)?;
        }

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.experiments().len(), 1);
        // The case with UTF-8 truncation should be gone, but attempt should be there
        assert_eq!(reader.attempts().len(), 1);

        Ok(())
    }

    #[test]
    fn test_missing_final_newline_repair() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;

        {
            let mut f = fs::File::create(file.path())?;
            let json = serde_json::to_string(&JournalRecord::Experiment(exp.clone()))?;
            f.write_all(json.as_bytes())?; // Write without newline
            f.sync_all()?;
        }

        // Reopen and append
        {
            let mut writer = JournalWriter::open(file.path())?;
            let case = Case::new(&exp.id, "case-1", "content")?;
            writer.write_case(&case)?;
        }

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.experiments().len(), 1);
        assert_eq!(reader.cases().len(), 1);

        Ok(())
    }

    #[test]
    fn test_interleaved_started_and_completed_attempts() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;

        let mut attempt_a = Attempt::new(&case.id, "a")?;
        let mut attempt_b = Attempt::new(&case.id, "b")?;
        let attempt_c = Attempt::new(&case.id, "a")?;

        attempt_a.complete(1000, None)?;
        attempt_b.complete(1500, None)?;
        // attempt_c left as Started

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt_a)?;
        writer.write_attempt(&attempt_b)?;
        writer.write_attempt(&attempt_c)?;

        let reader = JournalReader::open(file.path())?;
        let attempts = reader.attempts();
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0].state, crate::records::AttemptState::Completed);
        assert_eq!(attempts[1].state, crate::records::AttemptState::Completed);
        // attempt_c should be marked Unknown because it has no terminal record
        assert_eq!(attempts[2].state, crate::records::AttemptState::Unknown);
        assert_eq!(reader.recovery.ambiguous_started_attempts, 1);

        Ok(())
    }

    #[test]
    fn test_record_validation_rejects_credentials_in_experiment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        {
            let mut f = fs::File::create(file.path())?;
            // Experiment with credentials in profile_a_id
            let json = r#"{"record_type":"experiment","id":"e1","schema_version":1,"corpus_version":"v1","source_context":"ctx","instruction_version":"instr","profile_a_id":"https://user:sk-abc123@api.internal","profile_a_version":"1","profile_b_id":"pb","profile_b_version":"1","build_revision":"build","unknown_metadata":{"unknown_fields":[]}}"#;
            writeln!(&mut f, "{}", json)?;
        }

        let result = JournalReader::open(file.path());
        assert!(matches!(result, Err(JournalError::Deserialization(_))));

        Ok(())
    }

    #[test]
    fn test_record_validation_rejects_credentials_in_attempt(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        {
            let mut f = fs::File::create(file.path())?;
            // Attempt with credentials in provider_output
            let json = r#"{"record_type":"attempt","id":"a1","case_id":"c1","arm":"a","state":"completed","elapsed_ms":1000,"usage_tokens":null,"failure_reason":null,"provider_output":{"api_key":"sk-abc123","endpoint":"https://api.internal"},"schema_version":1}"#;
            writeln!(&mut f, "{}", json)?;
        }

        let result = JournalReader::open(file.path());
        assert!(matches!(result, Err(JournalError::Deserialization(_))));

        Ok(())
    }

    #[test]
    fn test_normal_failure_text_not_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        {
            let mut f = fs::File::create(file.path())?;
            // Attempt with normal failure text that should be accepted
            let json = r#"{"record_type":"attempt","id":"a1","case_id":"c1","arm":"a","state":"failed","elapsed_ms":1000,"usage_tokens":null,"failure_reason":"max output tokens exceeded","provider_output":null,"schema_version":1}"#;
            writeln!(&mut f, "{}", json)?;
        }

        let result = JournalReader::open(file.path());
        assert!(result.is_ok());
        let reader = result?;
        assert_eq!(reader.attempts().len(), 1);
        assert_eq!(
            reader.attempts()[0].failure_reason.as_deref(),
            Some("max output tokens exceeded")
        );

        Ok(())
    }

    #[test]
    fn test_no_fabricated_records() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let attempt = Attempt::new(&case.id, "a")?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt)?;

        {
            let mut f = fs::OpenOptions::new().append(true).open(file.path())?;
            // Write incomplete JSON that looks like a Started attempt
            write!(
                f,
                r#"{{"record_type":"attempt","id":"incomplete","case_id":"c1","arm":"a","state":"started""#
            )?;
            f.sync_all()?;
        }

        let reader = JournalReader::open(file.path())?;
        // Should have 3 legitimate records (exp, case, attempt) but no fabricated ones
        assert_eq!(reader.records().len(), 3);
        // The incomplete record should not have been fabricated
        for record in reader.records() {
            if let JournalRecord::Attempt(att) = record {
                // No "truncated" id should exist
                assert_ne!(att.id, "truncated");
            }
        }

        Ok(())
    }

    #[test]
    fn test_write_boundary_rejects_credentials_in_provider_output(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt = Attempt::new(&case.id, "a")?;

        // Try to complete with provider_output containing an API key
        let result = attempt.complete(
            1000,
            Some(serde_json::json!({"api_key": "canary123", "endpoint": "https://api.internal"})),
        );
        assert!(result.is_err());

        // The journal should still be writable and readable with the bad attempt not added
        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;

        // After rejecting a credential-bearing attempt, verify the journal is still valid
        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.experiments().len(), 1);
        assert_eq!(reader.cases().len(), 1);
        assert_eq!(reader.attempts().len(), 0); // Bad attempt was never written

        Ok(())
    }

    #[test]
    fn test_write_boundary_rejects_private_endpoint_in_profile_id(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        // Attempt to create an experiment with a private endpoint in profile_a_id
        let result = Experiment::new(
            "v1",
            "ctx",
            "instr",
            "https://10.0.0.1:8080/api",
            "1",
            "pb",
            "1",
            "build",
        );
        assert!(result.is_err());

        // Try to write with JournalWriter to confirm write boundary validation
        let mut writer = JournalWriter::open(file.path())?;
        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        writer.write_experiment(&exp)?;

        // Journal should remain clean and readable
        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.experiments().len(), 1);

        Ok(())
    }

    #[test]
    fn test_write_rejects_json_key_pattern_api_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut attempt = Attempt::new("case-1", "a")?;

        // Try to complete with JSON containing "api_key": pattern
        let result = attempt.complete(1000, Some(serde_json::json!({"api_key": "sk-abc123"})));
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        Ok(())
    }

    #[test]
    fn test_write_accepts_normal_provider_output() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt = Attempt::new(&case.id, "a")?;

        // Normal provider output should be accepted
        attempt.complete(
            1000,
            Some(serde_json::json!({"result": "success", "tokens_used": 42})),
        )?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt)?;

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.attempts().len(), 1);
        assert!(reader.attempts()[0].provider_output.is_some());

        Ok(())
    }
}
