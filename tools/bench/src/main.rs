//! `bench`: developer-only paired model comparison on the synthetic corpus. See `bench help`.

use std::io::Write;

use bench::cli::{cancel_on_interrupt, main_with, Services};
use ohand_core::providers::contracts::CancelToken;

fn main() {
    let words: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let cancel = CancelToken::new();
    if words.first().map(String::as_str) == Some("run") {
        cancel_on_interrupt(cancel.clone());
    }
    let services = Services::production(cancel);
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let status = main_with(&words, &services, &mut out, &mut err);
    let _ = out.flush();
    let _ = err.flush();
    std::process::exit(status);
}
