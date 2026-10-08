//! The store read behind `ohand_core_start_store_check`.

use super::failure::AbiFailure;
use super::instance::lock;
use crate::store::schema::Database;
use serde::Serialize;
use std::sync::Mutex;

#[derive(Serialize)]
struct StoreCheck {
    operation_id: u64,
    schema_version: u32,
    capture_count: i64,
}

pub fn run(database: &Mutex<Database>, operation_id: u64) -> Result<Vec<u8>, AbiFailure> {
    let database = lock(database);
    let schema_version = database
        .schema_version()
        .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORAGE_ERROR))?;
    let capture_count = database
        .conn()
        .query_row("SELECT COUNT(*) FROM captures", [], |row| row.get(0))
        .map_err(|error| {
            AbiFailure::from_store(&anyhow::Error::from(error), AbiFailure::STORAGE_ERROR)
        })?;
    serde_json::to_vec(&StoreCheck {
        operation_id,
        schema_version,
        capture_count,
    })
    .map_err(|_| AbiFailure::INTERNAL)
}
