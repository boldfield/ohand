use evaluation::corpus::Corpus;
use evaluation::responses::Responses;
use evaluation::run::evaluate;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: evaluate [--corpus PATH] [--responses PATH] [--format json|text] [--out PATH] [--no-gate]\n\
Runs the synthetic intent corpus through the fast path and recorded/fake provider replies,\n\
applies each candidate with the production guard to a scratch store, and prints a report.\n\
The exit status is 1 when the zero-forbidden-authoritative-mutations gate fails.";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("evaluate: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut corpus_path = repository_root.join(evaluation::report::CORPUS_PATH);
    let mut responses_path =
        Some(repository_root.join("tools/evaluation/fixtures/recorded-responses.json"));
    let mut format = "json".to_string();
    let mut out: Option<PathBuf> = None;
    let mut enforce_gate = true;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--corpus" => corpus_path = PathBuf::from(value("--corpus")?),
            "--responses" => responses_path = Some(PathBuf::from(value("--responses")?)),
            "--no-responses" => responses_path = None,
            "--format" => format = value("--format")?,
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--no-gate" => enforce_gate = false,
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if format != "json" && format != "text" {
        return Err(format!("--format must be json or text\n{USAGE}"));
    }

    let corpus = Corpus::load(&corpus_path).map_err(|error| error.to_string())?;
    let responses = match &responses_path {
        Some(path) => Some(Responses::load(path, &corpus).map_err(|error| error.to_string())?),
        None => None,
    };
    let report = evaluate(&corpus, responses.as_ref()).map_err(|error| error.to_string())?;
    let rendered = if format == "json" {
        report.to_json()
    } else {
        report.to_text()
    };
    match out {
        Some(path) => std::fs::write(&path, rendered)
            .map_err(|error| format!("{}: {error}", path.display()))?,
        None => print!("{rendered}"),
    }
    if enforce_gate && !report.gate.passed {
        eprintln!("evaluate: gate failed: {}", report.gate.failures.join("; "));
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}
