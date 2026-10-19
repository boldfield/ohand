//! Hand-rolled argument parsing, in the style of the evaluation tool. Parsing never reads the
//! environment, the file system or the network: it only turns words into validated options.

use std::path::PathBuf;

use ohand_core::providers::contracts::ProviderProtocol;
use ohand_core::review::compare::ArmLabel;

use crate::executor::{SideKey, MAX_ATTEMPTS_PER_SIDE, MAX_REQUEST_CAP};

pub const USAGE: &str = "\
bench: developer-only paired model comparison on a synthetic corpus

USAGE:
    bench help
    bench plan --corpus <file> --profile-a <spec> --profile-b <spec> [options]
    bench run  --corpus <file> --profile-a <spec> --profile-b <spec> --journal <file> [options]
    bench report --corpus <file> --journal <file> [--format text|json] [--rates <file>]

COMMANDS:
    help    Print this text. Needs no credentials and touches nothing.
    plan    Dry run: validate the corpus and both profiles, then list the planned requests.
            Never reads a credential, never opens the network, never writes the requested
            journal. With --journal it rehearses the run on a private scratch copy in the
            system temporary directory, deleted before it exits; without --journal it
            writes no file at all.
    run     Execute the plan and append every result to the journal. Resumable: sides that
            already have a dispatched attempt are kept, never silently sent again.
    report  Read a finished or partial journal and print a diagnostic report: pair counts,
            objective results per arm against the synthetic oracle, disagreement, requests,
            latency, provider-reported tokens, an optional cost estimate and what the two
            arms do not have in common. Read-only: it opens the journal for reading, never
            creates or repairs it, sends nothing and chooses no winner.

REPORT OPTIONS:
    --format <text|json>   Output format (default `text`).
    --rates <file>         Versioned price rates you supply (JSON: rates_version, currency,
                           profiles[{profile_version, input_micros_per_million_tokens,
                           output_micros_per_million_tokens}]). Without it every cost is
                           unknown; with it costs are labeled estimates, never billed amounts.

REQUIRED:
    --corpus <file>        Synthetic intent corpus (JSON). Nothing else is ever read as input.
    --profile-a <spec>     Exactly two profiles, a and b. A spec is
    --profile-b <spec>       <protocol>:<model>[:<credential-ref>]
                           with protocol `anthropic`, `openai` or `self-hosted`. The credential
                           reference is an opaque name (default `bench-<protocol>`), never the
                           secret itself. A self-hosted model tag is written with `@` where the
                           name has `:` (`name@tag` is sent as `name:tag`), and its endpoint
                           comes from --self-hosted-endpoint-env.
    --journal <file>       run only: the journal to create or resume. plan may name one to show
                           what a run would keep (it is read, never written; only a private
                           scratch copy is written and deleted).

OPTIONS:
    --mode <mode>          `fake` (default): scripted synthetic replies, no network, no
                           credentials. `live`: real providers, needs --execute-network.
                           `recorded` is a reserved label and is not available.
    --case <id>            Only this fixture; repeatable. Default: every fixture.
    --max-cases <n>        Keep at most the first n selected fixtures.
    --attempts-per-side <n>  Requests allowed per side of a case, first request included
                           (1..=5, default 1).
    --request-cap <n>      Hard cap on requests reserved by this run, retries and reruns
                           included. Required with --mode live; fake defaults to the
                           planned maximum.
    --rerun <id>:<a|b>     Send this side again although it was attempted; repeatable. Uses
                           new allowance from --request-cap and records what it follows.
    --self-hosted-endpoint-env <VAR>
                           Required with a self-hosted profile and refused without one: the
                           environment variable that holds the https base URL of the server.
                           The URL is read from it (plan too, which opens no socket), pinned
                           into the profile version, and never printed or journaled. A
                           self-hosted arm is paired only where its model has run evidence and
                           the production adapter declares diagnostic support; otherwise
                           `plan` says so and `run` records each case as not eligible and
                           sends nothing, for either arm. Nothing falls back to another
                           profile or destination.
    --temperature <0.0-1.0>
                           Request this temperature from both arms alike. Without it no
                           temperature is requested from either arm and each provider's own
                           default applies; the experiment record says which. Some current
                           models reject any temperature.
    --timeout-seconds <n>  Per-request timeout pinned into the profiles (1..=300, default 30).
    --build-revision <s>   Label recorded as the build revision (default: crate version).

LIVE MODE (run only):
    --execute-network      The single switch that allows requests to leave this machine.
    --allow-destination <protocol>
                           Grants the hosted destination of that protocol; repeatable. Choosing
                           a profile never grants anything. Each selected protocol needs one.
    --credential-env <ref>=<VAR>
                           Reads the secret for credential reference <ref> from environment
                           variable <VAR> at dispatch time; repeatable. Secrets are never
                           printed, journaled or passed on the command line.

EXIT STATUS:
    0  every side completed      1  the run finished or stopped with sides not completed
    2  usage or refusal, nothing was sent
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fake,
    Recorded,
    Live,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Fake => "fake",
            Mode::Recorded => "recorded",
            Mode::Live => "live",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSpec {
    pub protocol: ProviderProtocol,
    pub model: String,
    pub credential_ref: String,
}

pub fn protocol_label(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenAi => "openai",
        ProviderProtocol::Anthropic => "anthropic",
        ProviderProtocol::SelfHosted => "self-hosted",
    }
}

fn parse_protocol(text: &str) -> Result<ProviderProtocol, String> {
    match text {
        "anthropic" => Ok(ProviderProtocol::Anthropic),
        "openai" => Ok(ProviderProtocol::OpenAi),
        "self-hosted" => Ok(ProviderProtocol::SelfHosted),
        _ => Err("protocol must be `anthropic`, `openai` or `self-hosted`".to_string()),
    }
}

fn is_name(text: &str, extra: &[char], max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

impl ProfileSpec {
    fn parse(text: &str) -> Result<ProfileSpec, String> {
        let parts: Vec<&str> = text.split(':').collect();
        if !(2..=3).contains(&parts.len()) {
            return Err("a profile spec is <protocol>:<model>[:<credential-ref>]".to_string());
        }
        let protocol = parse_protocol(parts[0])?;
        let model = parts[1];
        if !is_name(model, &['.', '_', '-', '@'], 80)
            || (model.contains('@') && protocol != ProviderProtocol::SelfHosted)
        {
            return Err(
                "model must be 1-80 characters of letters, digits, `.`, `_` or `-` (a \
                 self-hosted model tag is written with `@` where its name has `:`)"
                    .to_string(),
            );
        }
        let model = model.replace('@', ":");
        let credential_ref = match parts.get(2) {
            Some(reference) => (*reference).to_string(),
            None => format!("bench-{}", protocol_label(protocol)),
        };
        if !is_name(&credential_ref, &['.', '_', '-'], 64)
            || crate::guard::is_secret_or_endpoint(&credential_ref)
        {
            return Err(
                "credential reference must be an opaque name of 1-64 letters, digits, `.`, `_` or `-`, never a secret"
                    .to_string(),
            );
        }
        Ok(ProfileSpec {
            protocol,
            model,
            credential_ref,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub corpus: PathBuf,
    pub profile_a: ProfileSpec,
    pub profile_b: ProfileSpec,
    pub mode: Mode,
    pub journal: Option<PathBuf>,
    pub case_ids: Vec<String>,
    pub max_cases: Option<usize>,
    pub attempts_per_side: u32,
    pub request_cap: Option<u32>,
    pub reruns: Vec<SideKey>,
    pub execute_network: bool,
    pub allow_destinations: Vec<ProviderProtocol>,
    pub credential_env: Vec<(String, String)>,
    pub self_hosted_endpoint_env: Option<String>,
    pub timeout_seconds: u32,
    /// Temperature requested from both arms, in thousandths; `None` leaves it to each provider.
    pub temperature_milli: Option<u32>,
    pub build_revision: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Help,
    Plan(Options),
    Run(Options),
    Report(ReportOptions),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportOptions {
    pub corpus: PathBuf,
    pub journal: PathBuf,
    pub format: ReportFormat,
    pub rates: Option<PathBuf>,
}

fn parse_report(words: &[String]) -> Result<Command, String> {
    let mut corpus = None;
    let mut journal = None;
    let mut format = None;
    let mut rates = None;
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        index += 1;
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (word.as_str(), None),
        };
        let slot = match flag {
            "--corpus" => &mut corpus,
            "--journal" => &mut journal,
            "--format" => &mut format,
            "--rates" => &mut rates,
            _ if flag.starts_with("--") => {
                return Err(format!(
                    "unknown option {flag} for `report`; see `bench help`"
                ))
            }
            _ => return Err("unexpected argument; see `bench help`".to_string()),
        };
        let value = match inline {
            Some(value) => value,
            None => {
                let value = words
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                value.clone()
            }
        };
        set_once(slot, flag, value)?;
    }
    let required = |slot: Option<String>, flag: &str| {
        slot.filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{flag} is required for `report`"))
    };
    let format = match format.as_deref() {
        None | Some("text") => ReportFormat::Text,
        Some("json") => ReportFormat::Json,
        Some(_) => return Err("--format must be `text` or `json`".to_string()),
    };
    Ok(Command::Report(ReportOptions {
        corpus: PathBuf::from(required(corpus, "--corpus")?),
        journal: PathBuf::from(required(journal, "--journal")?),
        format,
        rates: rates
            .map(|value| {
                if value.is_empty() {
                    Err("--rates needs a file".to_string())
                } else {
                    Ok(PathBuf::from(value))
                }
            })
            .transpose()?,
    }))
}

#[derive(Default)]
struct Collected {
    corpus: Option<String>,
    profile_a: Option<String>,
    profile_b: Option<String>,
    mode: Option<String>,
    journal: Option<String>,
    case_ids: Vec<String>,
    max_cases: Option<String>,
    attempts_per_side: Option<String>,
    request_cap: Option<String>,
    reruns: Vec<String>,
    execute_network: bool,
    allow_destinations: Vec<String>,
    credential_env: Vec<String>,
    self_hosted_endpoint_env: Option<String>,
    timeout_seconds: Option<String>,
    temperature: Option<String>,
    build_revision: Option<String>,
}

fn set_once(slot: &mut Option<String>, flag: &str, value: String) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{flag} was given more than once"));
    }
    *slot = Some(value);
    Ok(())
}

fn collect(words: &[String]) -> Result<Collected, String> {
    let mut collected = Collected::default();
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        index += 1;
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (word.as_str(), None),
        };
        if flag == "--execute-network" {
            if inline.is_some() {
                return Err("--execute-network takes no value".to_string());
            }
            collected.execute_network = true;
            continue;
        }
        if !flag.starts_with("--") {
            return Err("unexpected argument; see `bench help`".to_string());
        }
        let known = [
            "--corpus",
            "--profile-a",
            "--profile-b",
            "--mode",
            "--journal",
            "--case",
            "--max-cases",
            "--attempts-per-side",
            "--request-cap",
            "--rerun",
            "--allow-destination",
            "--credential-env",
            "--self-hosted-endpoint-env",
            "--timeout-seconds",
            "--temperature",
            "--build-revision",
        ];
        if !known.contains(&flag) {
            return Err(format!("unknown option {flag}; see `bench help`"));
        }
        let value = match inline {
            Some(value) => value,
            None => {
                let value = words
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                value.clone()
            }
        };
        match flag {
            "--corpus" => set_once(&mut collected.corpus, flag, value)?,
            "--profile-a" => set_once(&mut collected.profile_a, flag, value)?,
            "--profile-b" => set_once(&mut collected.profile_b, flag, value)?,
            "--mode" => set_once(&mut collected.mode, flag, value)?,
            "--journal" => set_once(&mut collected.journal, flag, value)?,
            "--max-cases" => set_once(&mut collected.max_cases, flag, value)?,
            "--attempts-per-side" => set_once(&mut collected.attempts_per_side, flag, value)?,
            "--request-cap" => set_once(&mut collected.request_cap, flag, value)?,
            "--self-hosted-endpoint-env" => {
                set_once(&mut collected.self_hosted_endpoint_env, flag, value)?
            }
            "--timeout-seconds" => set_once(&mut collected.timeout_seconds, flag, value)?,
            "--temperature" => set_once(&mut collected.temperature, flag, value)?,
            "--build-revision" => set_once(&mut collected.build_revision, flag, value)?,
            "--case" => collected.case_ids.push(value),
            "--rerun" => collected.reruns.push(value),
            "--allow-destination" => collected.allow_destinations.push(value),
            "--credential-env" => collected.credential_env.push(value),
            _ => unreachable!("every known flag is handled"),
        }
    }
    Ok(collected)
}

fn parse_number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse::<T>()
        .map_err(|_| format!("{flag} must be a whole number"))
}

/// A temperature of 0.0 to 1.0 with at most three decimals, in thousandths.
fn parse_temperature(text: &str) -> Result<u32, String> {
    const MESSAGE: &str =
        "--temperature must be a number from 0.0 to 1.0 with at most three decimals";
    let well_formed = !text.is_empty()
        && text.chars().all(|c| c.is_ascii_digit() || c == '.')
        && text.matches('.').count() <= 1
        && !text.starts_with('.')
        && !text.ends_with('.');
    if !well_formed {
        return Err(MESSAGE.to_string());
    }
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > 3 || whole.len() > 1 {
        return Err(MESSAGE.to_string());
    }
    let milli = whole.parse::<u32>().map_err(|_| MESSAGE.to_string())? * 1000
        + format!("{fraction:0<3}")
            .parse::<u32>()
            .map_err(|_| MESSAGE.to_string())?;
    if milli > 1000 {
        return Err(MESSAGE.to_string());
    }
    Ok(milli)
}

fn parse_rerun(text: &str) -> Result<SideKey, String> {
    let (case_id, arm) = text
        .rsplit_once(':')
        .ok_or_else(|| "--rerun is <case-id>:<a|b>".to_string())?;
    let arm = match arm {
        "a" => ArmLabel::A,
        "b" => ArmLabel::B,
        _ => return Err("--rerun side must be `a` or `b`".to_string()),
    };
    if case_id.is_empty() {
        return Err("--rerun needs a case id".to_string());
    }
    Ok(SideKey {
        case_id: case_id.to_string(),
        arm,
    })
}

fn parse_credential_env(text: &str) -> Result<(String, String), String> {
    let (reference, variable) = text
        .split_once('=')
        .ok_or_else(|| "--credential-env is <ref>=<VAR>".to_string())?;
    if !is_name(reference, &['.', '_', '-'], 64) {
        return Err("--credential-env reference has an invalid shape".to_string());
    }
    if !is_name(variable, &['_'], 128) {
        return Err(
            "--credential-env variable must be letters, digits and `_` (a name, not a value)"
                .to_string(),
        );
    }
    Ok((reference.to_string(), variable.to_string()))
}

pub fn parse(words: &[String]) -> Result<Command, String> {
    let Some(first) = words.first() else {
        return Err("a command is required; see `bench help`".to_string());
    };
    if matches!(first.as_str(), "help" | "--help" | "-h") {
        return Ok(Command::Help);
    }
    let is_run = match first.as_str() {
        "plan" => false,
        "run" => true,
        "report" => {
            if words[1..]
                .iter()
                .any(|word| word == "--help" || word == "-h")
            {
                return Ok(Command::Help);
            }
            return parse_report(&words[1..]);
        }
        _ => return Err("unknown command; expected `plan`, `run`, `report` or `help`".to_string()),
    };
    if words[1..]
        .iter()
        .any(|word| word == "--help" || word == "-h")
    {
        return Ok(Command::Help);
    }
    let collected = collect(&words[1..])?;
    let options = build_options(collected, is_run)?;
    Ok(if is_run {
        Command::Run(options)
    } else {
        Command::Plan(options)
    })
}

fn build_options(collected: Collected, is_run: bool) -> Result<Options, String> {
    let corpus = collected
        .corpus
        .ok_or_else(|| "--corpus is required".to_string())?;
    let profile_a = ProfileSpec::parse(
        &collected
            .profile_a
            .ok_or_else(|| "--profile-a is required".to_string())?,
    )
    .map_err(|error| format!("--profile-a: {error}"))?;
    let profile_b = ProfileSpec::parse(
        &collected
            .profile_b
            .ok_or_else(|| "--profile-b is required".to_string())?,
    )
    .map_err(|error| format!("--profile-b: {error}"))?;
    if profile_a == profile_b {
        return Err("the two profiles must differ".to_string());
    }
    let mode = match collected.mode.as_deref() {
        None | Some("fake") => Mode::Fake,
        Some("live") => Mode::Live,
        Some("recorded") => Mode::Recorded,
        Some(_) => return Err("--mode must be `fake`, `live` or `recorded`".to_string()),
    };
    let attempts_per_side = match &collected.attempts_per_side {
        Some(text) => parse_number::<u32>("--attempts-per-side", text)?,
        None => 1,
    };
    if attempts_per_side == 0 || attempts_per_side > MAX_ATTEMPTS_PER_SIDE {
        return Err(format!(
            "--attempts-per-side must be between 1 and {MAX_ATTEMPTS_PER_SIDE}"
        ));
    }
    let request_cap = match &collected.request_cap {
        Some(text) => Some(parse_number::<u32>("--request-cap", text)?),
        None => None,
    };
    if request_cap.is_some_and(|cap| cap > MAX_REQUEST_CAP) {
        return Err(format!("--request-cap must not exceed {MAX_REQUEST_CAP}"));
    }
    let max_cases = match &collected.max_cases {
        Some(text) => {
            let count = parse_number::<usize>("--max-cases", text)?;
            if count == 0 {
                return Err("--max-cases must be at least 1".to_string());
            }
            Some(count)
        }
        None => None,
    };
    let timeout_seconds = match &collected.timeout_seconds {
        Some(text) => parse_number::<u32>("--timeout-seconds", text)?,
        None => 30,
    };
    if !(1..=300).contains(&timeout_seconds) {
        return Err("--timeout-seconds must be between 1 and 300".to_string());
    }
    let temperature_milli = collected
        .temperature
        .as_deref()
        .map(parse_temperature)
        .transpose()?;
    let reruns = collected
        .reruns
        .iter()
        .map(|text| parse_rerun(text))
        .collect::<Result<Vec<_>, _>>()?;
    let allow_destinations = collected
        .allow_destinations
        .iter()
        .map(|text| parse_protocol(text).map_err(|error| format!("--allow-destination: {error}")))
        .collect::<Result<Vec<_>, _>>()?;
    let credential_env = collected
        .credential_env
        .iter()
        .map(|text| parse_credential_env(text))
        .collect::<Result<Vec<_>, _>>()?;
    let self_hosted_endpoint_env = collected
        .self_hosted_endpoint_env
        .map(|variable| {
            if is_name(&variable, &['_'], 128) {
                Ok(variable)
            } else {
                Err(
                    "--self-hosted-endpoint-env must be a variable name, never the endpoint \
                     itself"
                        .to_string(),
                )
            }
        })
        .transpose()?;
    let self_hosted_arms = [&profile_a, &profile_b]
        .iter()
        .filter(|spec| spec.protocol == ProviderProtocol::SelfHosted)
        .count();
    if self_hosted_arms > 0 && self_hosted_endpoint_env.is_none() {
        return Err(
            "a self-hosted profile needs --self-hosted-endpoint-env <VAR>, the variable that \
             holds its https base URL"
                .to_string(),
        );
    }
    if self_hosted_arms == 0 && self_hosted_endpoint_env.is_some() {
        return Err("--self-hosted-endpoint-env names no self-hosted profile".to_string());
    }
    if credential_env.iter().enumerate().any(|(index, pair)| {
        credential_env[..index]
            .iter()
            .any(|prior| prior.0 == pair.0)
    }) {
        return Err("--credential-env names the same reference twice".to_string());
    }

    if !is_run {
        if collected.execute_network {
            return Err("--execute-network belongs to `run`; `plan` never sends anything".into());
        }
        if !allow_destinations.is_empty() || !credential_env.is_empty() {
            return Err(
                "--allow-destination and --credential-env belong to `run`; `plan` reads no \
                 credentials"
                    .into(),
            );
        }
    }
    if mode != Mode::Live
        && (collected.execute_network
            || !allow_destinations.is_empty()
            || !credential_env.is_empty())
    {
        return Err(
            "--execute-network, --allow-destination and --credential-env apply to --mode live \
             only"
                .into(),
        );
    }
    if is_run && collected.journal.is_none() {
        return Err("--journal is required for `run`".to_string());
    }

    Ok(Options {
        corpus: PathBuf::from(corpus),
        profile_a,
        profile_b,
        mode,
        journal: collected.journal.map(PathBuf::from),
        case_ids: collected.case_ids,
        max_cases,
        attempts_per_side,
        request_cap,
        reruns,
        execute_network: collected.execute_network,
        allow_destinations,
        credential_env,
        self_hosted_endpoint_env,
        timeout_seconds,
        temperature_milli,
        build_revision: collected.build_revision,
    })
}
