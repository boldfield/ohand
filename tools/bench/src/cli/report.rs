//! `bench report`: reads one journal and the corpus it ran on and prints the diagnostic report.
//! It opens the journal read-only, resolves no credential and opens no socket.

use std::io::Write;

use evaluation::corpus::Corpus;

use super::args::{ReportFormat, ReportOptions};
use super::{Refusal, EXIT_COMPLETED};
use crate::report::{build_report, CostRates};

pub fn report(options: &ReportOptions, out: &mut dyn Write) -> Result<i32, Refusal> {
    let corpus = Corpus::load(&options.corpus).map_err(|error| Refusal(error.to_string()))?;
    let rates = options
        .rates
        .as_deref()
        .map(CostRates::load)
        .transpose()
        .map_err(|error| Refusal(error.to_string()))?;
    let report = build_report(&corpus, &options.journal, rates.as_ref())
        .map_err(|error| Refusal(error.to_string()))?;
    let text = match options.format {
        ReportFormat::Text => report.to_text(),
        ReportFormat::Json => report.to_json(),
    };
    let _ = write!(out, "{text}");
    Ok(EXIT_COMPLETED)
}
