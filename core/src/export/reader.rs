//! Test-only reader for the portable export format. It exists to prove the format is
//! self-describing and that damaged output is refused; it is not a restore path.

use super::*;
use crate::domain::items::{FieldProvenance, ItemState};
use crate::store::events::ItemType;
use std::collections::HashSet;

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    NotUtf8,
    /// Output does not end at a record boundary.
    Truncated,
    Empty,
    BlankLine {
        line: usize,
    },
    Malformed {
        line: usize,
        message: String,
    },
    HeaderNotFirst,
    FooterNotLast,
    UnexpectedRecord {
        line: usize,
    },
    UnsupportedFormat {
        format: String,
        version: u32,
    },
    ItemCountMismatch {
        footer: usize,
        actual: usize,
    },
    DigestMismatch,
    DuplicateItem {
        item_id: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum Record {
    Header(SnapshotHeader),
    Item(Box<ExportedItem>),
    Footer(SnapshotFooter),
}

pub fn read_export(bytes: &[u8]) -> Result<CoreSnapshot, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::NotUtf8)?;
    if text.is_empty() {
        return Err(ReadError::Empty);
    }
    if !text.ends_with('\n') {
        return Err(ReadError::Truncated);
    }

    let lines: Vec<&str> = text[..text.len() - 1].split('\n').collect();
    let mut records = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            return Err(ReadError::BlankLine { line: index + 1 });
        }
        let record: Record = serde_json::from_str(line).map_err(|error| ReadError::Malformed {
            line: index + 1,
            message: error.to_string(),
        })?;
        records.push(record);
    }

    let mut records = records.into_iter().enumerate();
    let header = match records.next() {
        Some((_, Record::Header(header))) => header,
        _ => return Err(ReadError::HeaderNotFirst),
    };
    if header.format != FORMAT_NAME || header.format_version != FORMAT_VERSION {
        return Err(ReadError::UnsupportedFormat {
            format: header.format,
            version: header.format_version,
        });
    }

    let mut items = Vec::new();
    let mut footer = None;
    let mut seen = HashSet::new();
    for (index, record) in records {
        match record {
            Record::Item(item) if footer.is_none() => {
                if !seen.insert(item.item_id.clone()) {
                    return Err(ReadError::DuplicateItem {
                        item_id: item.item_id,
                    });
                }
                items.push(*item);
            }
            Record::Footer(parsed) if footer.is_none() => footer = Some(parsed),
            _ => return Err(ReadError::UnexpectedRecord { line: index + 1 }),
        }
    }
    let footer = footer.ok_or(ReadError::FooterNotLast)?;

    let covered_length = text[..text.len() - 1]
        .rfind('\n')
        .map(|position| position + 1)
        .ok_or(ReadError::FooterNotLast)?;
    let digest = hex_lower(&Sha256::digest(&bytes[..covered_length]));
    if digest != footer.sha256 {
        return Err(ReadError::DigestMismatch);
    }
    if footer.item_count != items.len() {
        return Err(ReadError::ItemCountMismatch {
            footer: footer.item_count,
            actual: items.len(),
        });
    }

    Ok(CoreSnapshot { header, items })
}

/// Rebuild the item state an application would project from an exported item.
pub fn reconstruct_item_state(item: &ExportedItem) -> ItemState {
    let current_text = match (item.text.basis.as_str(), &item.text.at) {
        ("original", _) => TextState::Original {
            text: item.text.text.clone(),
        },
        ("transcribed", Some(at)) => TextState::Transcribed {
            text: item.text.text.clone().expect("transcribed text"),
            transcribed_at: at.clone(),
        },
        ("corrected", Some(at)) => TextState::Corrected {
            text: item.text.text.clone().expect("corrected text"),
            corrected_at: at.clone(),
        },
        other => panic!("unknown text basis {other:?}"),
    };
    ItemState {
        item_id: item.item_id.clone(),
        capture_id: item.capture_id.clone(),
        revision: item.revision,
        item_type: item
            .item_type
            .as_deref()
            .map(|value| value.parse::<ItemType>().expect("item type")),
        scope: item.scope.parse::<ItemScope>().expect("scope"),
        session_topic: item.session_topic.clone(),
        lifecycle_state: match item.lifecycle_state.as_str() {
            "active" => LifecycleState::Active,
            "completed" => LifecycleState::Completed,
            "cancelled" => LifecycleState::Cancelled,
            other => panic!("unexpected lifecycle state {other:?}"),
        },
        current_text,
        provenance: FieldProvenance {
            type_corrected: item.provenance.type_corrected,
            scope_corrected: item.provenance.scope_corrected,
            session_topic_corrected: item.provenance.session_topic_corrected,
            text_corrected: item.provenance.text_corrected,
        },
    }
}
