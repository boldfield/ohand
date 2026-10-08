//! P07 handoff between the native capture entry and the management shell.
//!
//! The only thing that crosses is a capture identifier, in one exact URL shape:
//! `ohand-tauri://capture?captureId=<UPPERCASE-HYPHENATED-UUID>`. The grammar is matched as a string, with no URL
//! library, so the Swift sender and this receiver cannot disagree about normalisation. Anything else is rejected.
//!
//! Nothing here depends on Tauri or a webview: the shell calls [`receive_urls`] from its native URL event, so a
//! handoff is recorded whether or not the web UI is running.

mod inbox;
mod url_grammar;

pub use inbox::{
    receive_urls, HandoffInbox, HandoffRecord, HandoffSnapshot, ReceiveOutcome, RecordOutcome,
};
pub use url_grammar::{build_handoff_url, parse_handoff_url, CaptureId, HandoffError};
