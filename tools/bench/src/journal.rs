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

        let text = std::str::from_utf8(&contents).unwrap_or("");
        let lines: Vec<&str> = text.lines().collect();

        if lines.is_empty() {
            return Ok(());
        }

        let last_line = lines.last().unwrap();
        if serde_json::from_str::<JournalRecord>(last_line).is_ok() {
            return Ok(());
        }

        if lines.len() > 1 {
            let truncate_pos = contents.len();
            let last_newline = contents[..truncate_pos].iter().rposition(|&b| b == b'\n');

            if let Some(pos) = last_newline {
                file.set_len((pos + 1) as u64)
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
    fn write_record(&mut self, record: &JournalRecord) -> Result<(), JournalError> {
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
    /// If truncation is detected at EOF, all complete records are preserved,
    /// and any Started attempts at the end are marked as Unknown.
    /// Mid-file corruption returns an error.
    /// Ambiguous started attempts (not preceded by truncation) are also marked Unknown.
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
                    records.push(record);
                    recovery.records_read += 1;
                }
                Err(e) => {
                    let is_last_line = line_idx == lines.len() - 1;

                    if is_last_line {
                        recovery.truncation_reason =
                            Some(format!("truncation at line {}: {}", line_idx + 1, e));

                        if line.contains("\"state\":\"started\"")
                            || line.contains("\"state\": \"started\"")
                        {
                            let unknown_attempt = Attempt {
                                id: "truncated".to_string(),
                                case_id: "unknown".to_string(),
                                arm: "unknown".to_string(),
                                state: crate::records::AttemptState::Unknown,
                                elapsed_ms: None,
                                usage_tokens: None,
                                failure_reason: Some("truncated during write".to_string()),
                                provider_output: None,
                                schema_version: 1,
                            };
                            records.push(JournalRecord::Attempt(unknown_attempt));
                            recovery.ambiguous_started_attempts += 1;
                        }
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

        Self::mark_ambiguous_started_attempts(&mut records, &mut recovery);

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

    /// Mark ambiguous started attempts as Unknown.
    /// This handles both truncation-induced and natural EOF cases.
    fn mark_ambiguous_started_attempts(
        records: &mut [JournalRecord],
        recovery: &mut TruncationRecovery,
    ) {
        let mut found_ambiguous = false;

        for record in records.iter().rev() {
            if let JournalRecord::Attempt(attempt) = record {
                if attempt.state == crate::records::AttemptState::Started {
                    found_ambiguous = true;
                    break;
                }
            }
        }

        if found_ambiguous {
            for record in records.iter_mut().rev() {
                match record {
                    JournalRecord::Attempt(attempt) => {
                        if attempt.state == crate::records::AttemptState::Started {
                            attempt.state = crate::records::AttemptState::Unknown;
                            attempt.failure_reason =
                                Some("marked unknown due to ambiguous journal state".to_string());
                            recovery.ambiguous_started_attempts += 1;
                        } else {
                            break;
                        }
                    }
                    _ => break,
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
        let attempt_b = Attempt::new(&case.id, "b")?;

        let mut writer = JournalWriter::open(file.path())?;
        writer.write_experiment(&exp)?;
        writer.write_case(&case)?;
        writer.write_attempt(&attempt_a)?;
        writer.write_attempt(&attempt_b)?;

        {
            let f = fs::OpenOptions::new().write(true).open(file.path())?;
            let file_size = f.metadata()?.len();
            f.set_len(file_size - 10)?;
        }

        let reader = JournalReader::open(file.path())?;
        assert_eq!(reader.recovery.ambiguous_started_attempts, 1);
        let attempts = reader.attempts();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].state, crate::records::AttemptState::Completed);
        assert_eq!(attempts[1].state, crate::records::AttemptState::Unknown);

        Ok(())
    }

    #[test]
    fn test_append_after_truncation_repair() -> Result<(), Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;

        let exp = Experiment::new("v1", "ctx", "instr", "pa", "1", "pb", "1", "build")?;
        let case = Case::new(&exp.id, "case-1", "content")?;
        let mut attempt_a = Attempt::new(&case.id, "a")?;
        attempt_a.complete(1000, None);

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
            attempt_b.complete(2000, None);
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
        attempt_a.complete(1000, None);
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
}
