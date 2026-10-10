// Native capture handoff and import.
// Foreground import is owned by C02a; guarded transcript attachment is owned by C05a.

pub mod import;
pub mod transcription;

pub use import::{
    import_foreground_ingress, CommitStatus, ImportDisposition, IngressError, IngressErrorKind,
    IngressImportAcknowledgment, IngressRejection,
};
pub use transcription::{
    apply_transcription_outcome, DiscardReason, InterpretationQueued, InterpretationRequest,
    RecognizedTranscript, RecognizerResult, TranscriptionAcknowledgment, TranscriptionDisposition,
    TranscriptionError, TranscriptionErrorKind, TranscriptionOutcome, TranscriptionRejection,
};
