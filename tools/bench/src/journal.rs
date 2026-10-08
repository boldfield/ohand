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
    #[error("truncated or corrupt record")]
    Truncated,
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
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| JournalError::Io(e.to_string()))?;
        Ok(JournalWriter { file })
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
    fn write_record(&mut self, record: &JournalRecord) -> Result<(), JournalError> {
        let json = serde_json::to_string(record)
            .map_err(|e| JournalError::Serialization(e.to_string()))?;
        writeln!(self.file, "{}", json).map_err(|e| JournalError::Io(e.to_string()))?;
        self.file
            .flush()
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
    /// If truncation is detected, all complete records are preserved,
    /// and any Started attempts at the end are marked as Unknown.
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

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| JournalError::Io(e.to_string()))?;
            let line = line.trim();

            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<JournalRecord>(line) {
                Ok(record) => {
                    records.push(record);
                    recovery.records_read += 1;
                }
                Err(e) => {
                    // Truncated line or corrupt JSON - mark and stop reading
                    recovery.truncation_reason =
                        Some(format!("truncation at line {}: {}", line_num + 1, e));

                    // Mark any trailing Started attempts as Unknown
                    for record in records.iter_mut().rev() {
                        match record {
                            JournalRecord::Attempt(attempt) => {
                                if attempt.state == crate::records::AttemptState::Started {
                                    attempt.state = crate::records::AttemptState::Unknown;
                                    attempt.failure_reason = Some(
                                        "marked unknown due to journal truncation".to_string(),
                                    );
                                    recovery.ambiguous_started_attempts += 1;
                                } else {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }

                    break;
                }
            }
        }

        Ok(JournalReader { records, recovery })
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
        attempt.complete(1000, None);

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
        attempt_a.complete(1000, None);
        let attempt_b = Attempt::new(&case.id, "b")?; // Still in Started state

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt_a)?;
        writer.write_attempt(&attempt_b)?;

        // Simulate truncation after attempt_b write started
        {
            let mut f = fs::OpenOptions::new().append(true).open(file.path())?;
            write!(f, r#"{{"record_type":"attempt","id":"incomplete"#)?;
            f.flush()?;
        }

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.recovery.ambiguous_started_attempts, 1);
        let attempts = reader.attempts();
        assert_eq!(attempts.len(), 2);
        // First attempt is unaffected
        assert_eq!(attempts[0].state, crate::records::AttemptState::Completed);
        // Second attempt is marked unknown due to truncation
        assert_eq!(attempts[1].state, crate::records::AttemptState::Unknown);

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
}
