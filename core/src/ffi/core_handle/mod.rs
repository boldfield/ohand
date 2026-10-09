//! Shared handle lifetime, error conversion, cancellation and callback-threading boundary
//! used by production exports (B01b). See `exports` for the ABI contract.

pub mod exports;
pub mod failure;
pub mod instance;
pub mod store_check;

#[cfg(test)]
pub(crate) mod tests;
