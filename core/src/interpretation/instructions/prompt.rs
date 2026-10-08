//! Provider-neutral rendering of one interpretation request.

use super::text::{output_schema, M1_INSTRUCTION_TEXT};
use super::OUTPUT_CONTRACT_VERSION;
use crate::providers::contracts::InterpretationRequest;
use serde_json::{json, Value};

/// Everything an adapter sends to the model. The instructions are static and trusted; the user
/// document is trusted context around one escaped untrusted string.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedPrompt {
    /// Instruction version the prompt was rendered for.
    pub instruction_version: String,
    /// The system instructions, byte-for-byte [`M1_INSTRUCTION_TEXT`].
    pub system: String,
    /// The user message: a JSON document with the context and `source.text`.
    pub user: String,
    /// Schema of the expected reply, for adapters that support structured output.
    pub output_schema: Value,
}

/// Route, credentials and any other core-only data are deliberately not rendered.
pub(super) fn render(request: &InterpretationRequest) -> RenderedPrompt {
    let user = json!({
        "instruction_version": request.instruction_version(),
        "output_contract_version": OUTPUT_CONTRACT_VERSION,
        "request": {
            "request_version": request.request_version(),
            "capture_id": request.capture_id(),
            "source_revision": request.source_revision(),
            "text_basis": request.text_basis(),
        },
        "profile": {
            "profile_id": request.profile_id(),
            "profile_version": request.profile_version(),
        },
        "time_context": request.time_context(),
        "source": {
            "trust": "untrusted_data",
            "character_count": request.text().chars().count(),
            "text": request.text(),
        },
    });
    RenderedPrompt {
        instruction_version: request.instruction_version().to_string(),
        system: M1_INSTRUCTION_TEXT.to_string(),
        user: user.to_string(),
        output_schema: output_schema(),
    }
}
